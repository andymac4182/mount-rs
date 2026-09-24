//! One-Partition session routing with a fresh catalog authorization check.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_core::{FileHandle, FsDriver, MkdirOptions};
use mount_rs_remote_protocol::{Operation, OperationName, WireError};
use serde_json::Value;
use tokio::sync::Mutex;

use crate::auth::authorize_drive;
use crate::catalog::{CatalogStore, DriveKey, IssuerPolicyDefinition, Permission};

#[derive(Debug, Clone)]
pub struct SessionIdentity {
    pub partition_id: String,
    pub policy_id: String,
    pub issuer: String,
    pub subject: String,
    pub signing_algorithm: String,
    pub claims: Value,
    pub expires_at: i64,
}

#[derive(Default)]
pub struct SessionHandles {
    state: Mutex<HandleState>,
}

#[derive(Default)]
struct HandleState {
    next: u64,
    closed: bool,
    revision: Option<u64>,
    entries: BTreeMap<u64, (String, u64, Arc<dyn FileHandle>)>,
}

fn handle_matches(
    stored_drive: &str,
    requested_drive: &str,
    stored_revision: u64,
    requested_revision: u64,
) -> bool {
    stored_drive == requested_drive && stored_revision == requested_revision
}

fn permission_allows(granted: Permission, required: mount_rs_remote_protocol::Permission) -> bool {
    granted == Permission::Write || required == mount_rs_remote_protocol::Permission::Read
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandleRejection {
    Closed,
    Stale,
    Full,
}
impl HandleRejection {
    fn code(self) -> &'static str {
        match self {
            Self::Closed => "EBADF",
            Self::Stale => "ESTALE",
            Self::Full => "EMFILE",
        }
    }
}

fn admit_handle(
    closed: bool,
    current: Option<u64>,
    revision: u64,
    count: usize,
    next: u64,
) -> Result<u64, HandleRejection> {
    if closed {
        return Err(HandleRejection::Closed);
    }
    if current != Some(revision) {
        return Err(HandleRejection::Stale);
    }
    if count >= 1024 {
        return Err(HandleRejection::Full);
    }
    next.checked_add(1).ok_or(HandleRejection::Full)
}

impl SessionHandles {
    async fn insert(
        &self,
        drive: &str,
        revision: u64,
        handle: Arc<dyn FileHandle>,
    ) -> Result<u64, WireError> {
        let mut state = self.state.lock().await;
        let next = match admit_handle(
            state.closed,
            state.revision,
            revision,
            state.entries.len(),
            state.next,
        ) {
            Ok(next) => next,
            Err(reason) => {
                drop(state);
                let _ = handle.close().await;
                return Err(error(reason.code()));
            }
        };
        state.next = next;
        let id = state.next;
        state
            .entries
            .insert(id, (drive.to_owned(), revision, handle));
        Ok(id)
    }

    async fn refresh_revision(&self, revision: u64) {
        let mut state = self.state.lock().await;
        if state.closed || state.revision.is_some_and(|current| current >= revision) {
            return;
        }
        state.revision = Some(revision);
        let entries = std::mem::take(&mut state.entries);
        drop(state);
        for (_, (_, _, handle)) in entries {
            let _ = handle.close().await;
        }
    }

    pub(crate) async fn is_closed(&self) -> bool {
        self.state.lock().await.closed
    }

    pub async fn close_all(&self) {
        let mut state = self.state.lock().await;
        state.closed = true;
        let entries = std::mem::take(&mut state.entries);
        drop(state);
        for (_, (_, _, handle)) in entries {
            let _ = handle.close().await;
        }
    }
}

pub struct DriveDispatcher {
    catalog: Arc<dyn CatalogStore>,
    drives: BTreeMap<DriveKey, Arc<dyn FsDriver>>,
    definitions: BTreeMap<DriveKey, Value>,
}

impl DriveDispatcher {
    #[must_use]
    pub fn new(catalog: Arc<dyn CatalogStore>) -> Self {
        Self {
            catalog,
            drives: BTreeMap::new(),
            definitions: BTreeMap::new(),
        }
    }

    pub fn register(
        &mut self,
        partition_id: &str,
        drive_id: &str,
        driver: Arc<dyn FsDriver>,
    ) -> Result<(), &'static str> {
        let key = DriveKey {
            partition_id: partition_id.to_owned(),
            drive_id: drive_id.to_owned(),
        };
        if self.drives.contains_key(&key) {
            return Err("duplicate Drive registration");
        }
        self.drives.insert(key, driver);
        Ok(())
    }

    pub fn register_definition(
        &mut self,
        partition_id: &str,
        drive_id: &str,
        definition: Value,
        driver: Arc<dyn FsDriver>,
    ) -> Result<(), &'static str> {
        self.register(partition_id, drive_id, driver)?;
        self.definitions.insert(
            DriveKey {
                partition_id: partition_id.into(),
                drive_id: drive_id.into(),
            },
            definition,
        );
        Ok(())
    }

    pub async fn renewal_matches(&self, current: &SessionIdentity, next: &SessionIdentity) -> bool {
        let Ok(catalog) = self.catalog.load_current().await else {
            return false;
        };
        catalog
            .grants
            .values()
            .filter(|g| g.partition_id == current.partition_id && g.policy_id == current.policy_id)
            .flat_map(|g| g.claim_conditions.keys())
            .all(|pointer| current.claims.pointer(pointer) == next.claims.pointer(pointer))
    }

    pub async fn dispatch(
        &self,
        identity: &SessionIdentity,
        drive_id: &str,
        operation: &Operation,
    ) -> Result<Value, WireError> {
        let handles = SessionHandles::default();
        let result = self
            .dispatch_with_handles(identity, drive_id, operation, &handles)
            .await;
        handles.close_all().await;
        result
    }

    pub async fn dispatch_with_handles(
        &self,
        identity: &SessionIdentity,
        drive_id: &str,
        operation: &Operation,
        handles: &SessionHandles,
    ) -> Result<Value, WireError> {
        self.dispatch_request(identity, drive_id, operation, handles, 0)
            .await
    }

    pub async fn dispatch_request(
        &self,
        identity: &SessionIdentity,
        drive_id: &str,
        operation: &Operation,
        handles: &SessionHandles,
        request_id: u64,
    ) -> Result<Value, WireError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| error("EIO"))?
            .as_secs() as i64;
        if now >= identity.expires_at {
            handles.close_all().await;
            return Err(error("EACCES"));
        }
        let catalog = match self.catalog.load_current().await {
            Ok(catalog) => catalog,
            Err(_) => {
                handles.close_all().await;
                return Err(error("EACCES"));
            }
        };
        handles.refresh_revision(catalog.revision).await;
        let policy: IssuerPolicyDefinition = catalog
            .issuer_policies
            .get(&identity.policy_id)
            .cloned()
            .ok_or_else(|| error("EACCES"))
            .and_then(|value| serde_json::from_value(value).map_err(|_| error("EACCES")))?;
        let audience_ok = match identity.claims.get("aud") {
            Some(Value::String(value)) => policy.audiences.contains(value),
            Some(Value::Array(values)) => values.iter().any(|v| {
                v.as_str()
                    .is_some_and(|v| policy.audiences.iter().any(|a| a == v))
            }),
            _ => false,
        };
        if policy.issuer != identity.issuer
            || !audience_ok
            || !policy.algorithms.contains(&identity.signing_algorithm)
        {
            return Err(error("EACCES"));
        }
        let permission = authorize_drive(
            &catalog,
            &identity.policy_id,
            &identity.claims,
            &identity.partition_id,
            drive_id,
        )
        .ok_or_else(|| error("EACCES"))?;
        if !permission_allows(permission, operation.required_permission()) {
            return Err(error("EACCES"));
        }
        let key = DriveKey {
            partition_id: identity.partition_id.clone(),
            drive_id: drive_id.to_owned(),
        };
        if let Some(definition) = self.definitions.get(&key)
            && catalog
                .partitions
                .get(&identity.partition_id)
                .and_then(|p| p.drives.get(drive_id))
                .is_none_or(|d| d.driver != *definition)
        {
            return Err(error("ESTALE"));
        }
        let driver = self.drives.get(&key).ok_or_else(|| error("EACCES"))?;
        let body = &operation.body;
        let result = async {
            match operation.name {
                OperationName::Capabilities => {
                    let mut capabilities = driver.capabilities();
                    capabilities.read_only |= permission == Permission::Read;
                    let mut value = encode(capabilities)?;
                    value["guarded_reads"] = Value::Bool(driver.supports_guarded_reads());
                    value["guarded_mutations"] = Value::Bool(driver.supports_guarded_mutations());
                    value["stable_inode_ids"] = Value::Bool(driver.stable_inode_ids());
                    value["utimens"] = Value::Bool(driver.has_utimens());
                    Ok(value)
                }
                OperationName::GuardedRead => {
                    let request: mount_rs_core::GuardedRead =
                        serde_json::from_value(body.clone()).map_err(|_| error("EINVAL"))?;
                    validate_guarded_read(&request)?;
                    encode(driver.guarded_read(request).await.map_err(fs_error)?)
                }
                OperationName::GuardedMutation => {
                    let request: mount_rs_core::GuardedMutation =
                        serde_json::from_value(body.clone()).map_err(|_| error("EINVAL"))?;
                    validate_guarded_mutation(&request)?;
                    match driver.guarded_mutation(request).await.map_err(fs_error)? {
                        mount_rs_core::GuardedMutationResult::Applied => {
                            Ok(serde_json::json!({"kind":"applied"}))
                        }
                        mount_rs_core::GuardedMutationResult::Created(identity) => {
                            Ok(serde_json::json!({"kind":"created","identity":identity}))
                        }
                        mount_rs_core::GuardedMutationResult::Opened { handle, identity } => {
                            let id = handles.insert(drive_id, catalog.revision, handle).await?;
                            Ok(serde_json::json!({"kind":"opened","identity":identity,"handle":id}))
                        }
                    }
                }
                OperationName::Stat => encode(driver.stat(path(body)?).await.map_err(fs_error)?),
                OperationName::Lstat => encode(driver.lstat(path(body)?).await.map_err(fs_error)?),
                OperationName::Statfs => {
                    encode(driver.statfs(path(body)?).await.map_err(fs_error)?)
                }
                OperationName::ReaddirBounded => {
                    let limit = number(body, "max_entries")?;
                    if limit == 0 || limit > 4096 {
                        return Err(error("EINVAL"));
                    }
                    encode(
                        driver
                            .readdir_bounded(path(body)?, limit as usize)
                            .await
                            .map_err(fs_error)?,
                    )
                }
                OperationName::Readlink => {
                    encode(driver.readlink(path(body)?).await.map_err(fs_error)?)
                }
                OperationName::Open => {
                    let flags = body.get("flags").ok_or_else(|| error("EINVAL"))?;
                    let handle = if let Some(flags) = flags.as_str() {
                        driver
                            .open(path(body)?, flags, integer(body, "mode")?)
                            .await
                    } else {
                        let flags: mount_rs_core::OpenFlags =
                            serde_json::from_value(flags.clone()).map_err(|_| error("EINVAL"))?;
                        if !flags.has_valid_truncate_access() || (!flags.read && !flags.write) {
                            return Err(error("EINVAL"));
                        }
                        driver
                            .open_flags(path(body)?, flags, integer(body, "mode")?)
                            .await
                    }
                    .map_err(fs_error)?;
                    encode(handles.insert(drive_id, catalog.revision, handle).await?)
                }
                OperationName::HandleRead
                | OperationName::HandleStat
                | OperationName::HandleWrite
                | OperationName::HandleTruncate
                | OperationName::HandleSync
                | OperationName::HandleDatasync
                | OperationName::HandleClose => {
                    let id = number(body, "handle")?;
                    let mut state = handles.state.lock().await;
                    let (drive, revision, handle) =
                        state.entries.get(&id).ok_or_else(|| error("EBADF"))?;
                    if !handle_matches(drive, drive_id, *revision, catalog.revision) {
                        return Err(error("EBADF"));
                    }
                    let handle = Arc::clone(handle);
                    if operation.name == OperationName::HandleClose {
                        state.entries.remove(&id);
                    }
                    drop(state);
                    match operation.name {
                        OperationName::HandleRead => {
                            let length = number(body, "length")?;
                            if length > 1024 * 1024 {
                                return Err(error("EINVAL"));
                            }
                            let mut data = vec![0; length as usize];
                            let count = handle
                                .read(&mut data, position(body)?)
                                .await
                                .map_err(fs_error)?;
                            if count > data.len() {
                                return Err(error("EIO"));
                            }
                            data.truncate(count);
                            encode(data)
                        }
                        OperationName::HandleWrite => encode(
                            handle
                                .write(&bytes(body)?, position(body)?)
                                .await
                                .map_err(fs_error)?,
                        ),
                        OperationName::HandleStat => encode(handle.stat().await.map_err(fs_error)?),
                        OperationName::HandleTruncate => {
                            handle
                                .truncate(number(body, "length")?)
                                .await
                                .map_err(fs_error)?;
                            Ok(Value::Null)
                        }
                        OperationName::HandleSync => {
                            handle.sync().await.map_err(fs_error)?;
                            Ok(Value::Null)
                        }
                        OperationName::HandleDatasync => {
                            handle.datasync().await.map_err(fs_error)?;
                            Ok(Value::Null)
                        }
                        OperationName::HandleClose => {
                            handle.close().await.map_err(fs_error)?;
                            Ok(Value::Null)
                        }
                        _ => unreachable!(),
                    }
                }
                OperationName::Write => {
                    driver
                        .write_file(path(body)?, &bytes(body)?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Mkdir => encode(
                    driver
                        .mkdir(
                            path(body)?,
                            MkdirOptions {
                                recursive: body
                                    .get("recursive")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(false),
                                mode: Some(integer(body, "mode")?),
                            },
                        )
                        .await
                        .map_err(fs_error)?,
                ),
                OperationName::Rmdir => {
                    driver.rmdir(path(body)?).await.map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Unlink => {
                    driver.unlink(path(body)?).await.map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Rename => {
                    driver
                        .rename(path(body)?, named_path(body, "destination")?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Link => {
                    driver
                        .link(path(body)?, named_path(body, "destination")?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Symlink => {
                    driver
                        .symlink(string(body, "target")?, path(body)?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Chmod => {
                    driver
                        .chmod(path(body)?, integer(body, "mode")?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Chown => {
                    driver
                        .chown(path(body)?, integer(body, "uid")?, integer(body, "gid")?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Lchown => {
                    driver
                        .lchown(path(body)?, integer(body, "uid")?, integer(body, "gid")?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Truncate => {
                    driver
                        .truncate(path(body)?, number(body, "length")?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Utimes => {
                    driver
                        .utimes(path(body)?, signed(body, "atime")?, signed(body, "mtime")?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Lutimes => {
                    driver
                        .lutimes(path(body)?, signed(body, "atime")?, signed(body, "mtime")?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Utimens => {
                    driver
                        .utimens(
                            path(body)?,
                            string(body, "atime")?
                                .parse()
                                .map_err(|_| error("EINVAL"))?,
                            string(body, "mtime")?
                                .parse()
                                .map_err(|_| error("EINVAL"))?,
                            body.get("follow")
                                .and_then(Value::as_bool)
                                .ok_or_else(|| error("EINVAL"))?,
                        )
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Mknod => {
                    driver
                        .mknod(path(body)?, integer(body, "mode")?, number(body, "dev")?)
                        .await
                        .map_err(fs_error)?;
                    Ok(Value::Null)
                }
                OperationName::Syncfs => {
                    driver.syncfs().await.map_err(fs_error)?;
                    Ok(Value::Null)
                }
            }
        }
        .await;
        if request_id != 0 {
            let grants: Vec<_> = catalog
                .grants
                .iter()
                .filter(|(_, grant)| {
                    grant.partition_id == identity.partition_id
                        && grant.policy_id == identity.policy_id
                        && grant.drives.contains_key(drive_id)
                        && grant.claim_conditions.iter().all(|(pointer, expected)| {
                            identity.claims.pointer(pointer).and_then(Value::as_str)
                                == Some(expected.as_str())
                        })
                })
                .map(|(id, _)| id)
                .collect();
            eprintln!(
                "{}",
                serde_json::json!({"event":"remote_access","partition_id":identity.partition_id,"drive_id":drive_id,"grant_ids":grants,"operation":operation.name,"request_id":request_id,"outcome":result.as_ref().map(|_|"ok").unwrap_or_else(|error|&error.code)})
            );
        }
        result
    }
}

fn path(body: &Value) -> Result<&str, WireError> {
    named_path(body, "path")
}

fn named_path<'a>(body: &'a Value, name: &str) -> Result<&'a str, WireError> {
    let path = string(body, name)?;
    if path.len() > 4096
        || !path.starts_with('/')
        || path.contains('\0')
        || mount_rs_core::path::normalize_path(path) != path
    {
        return Err(error("EINVAL"));
    }
    Ok(path)
}

fn error(code: &str) -> WireError {
    WireError {
        code: code.to_owned(),
    }
}

fn fs_error(error_value: mount_rs_core::FsError) -> WireError {
    error(error_value.code.as_str())
}

fn encode(value: impl serde::Serialize) -> Result<Value, WireError> {
    serde_json::to_value(value).map_err(|_| error("EIO"))
}
fn string<'a>(body: &'a Value, name: &str) -> Result<&'a str, WireError> {
    body.get(name)
        .and_then(Value::as_str)
        .ok_or_else(|| error("EINVAL"))
}
fn number(body: &Value, name: &str) -> Result<u64, WireError> {
    body.get(name)
        .and_then(Value::as_u64)
        .ok_or_else(|| error("EINVAL"))
}
fn integer(body: &Value, name: &str) -> Result<u32, WireError> {
    u32::try_from(number(body, name)?).map_err(|_| error("EINVAL"))
}
fn signed(body: &Value, name: &str) -> Result<i64, WireError> {
    body.get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| error("EINVAL"))
}
fn position(body: &Value) -> Result<Option<u64>, WireError> {
    match body.get("position") {
        Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().map(Some).ok_or_else(|| error("EINVAL")),
        None => Err(error("EINVAL")),
    }
}
fn bytes(body: &Value) -> Result<Vec<u8>, WireError> {
    let values = body
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| error("EINVAL"))?;
    if values.len() > 1024 * 1024 {
        return Err(error("EINVAL"));
    }
    values
        .iter()
        .map(|v| {
            v.as_u64()
                .and_then(|v| u8::try_from(v).ok())
                .ok_or_else(|| error("EINVAL"))
        })
        .collect()
}

fn validate_guard(path: &mount_rs_core::PathGuard) -> Result<(), WireError> {
    if path.identity.ino == 0 {
        return Err(error("EINVAL"));
    }
    named_path(&serde_json::json!({"path":path.path}), "path")?;
    Ok(())
}
fn child(name: &str) -> Result<(), WireError> {
    if name.is_empty()
        || name.len() > 255
        || name.contains('/')
        || name.contains('\0')
        || name == "."
        || name == ".."
    {
        Err(error("EINVAL"))
    } else {
        Ok(())
    }
}
fn validate_guarded_read(request: &mount_rs_core::GuardedRead) -> Result<(), WireError> {
    use mount_rs_core::GuardedRead::*;
    match request {
        Stat { target } | Readlink { target } => validate_guard(target),
        Lookup { parent, name } => {
            validate_guard(parent)?;
            if name == "." || name == ".." {
                Ok(())
            } else {
                child(name)
            }
        }
        Readdir {
            directory,
            max_entries,
        } => {
            validate_guard(directory)?;
            if *max_entries == 0 || *max_entries > 4096 {
                Err(error("EINVAL"))
            } else {
                Ok(())
            }
        }
    }
}
fn validate_guarded_mutation(request: &mount_rs_core::GuardedMutation) -> Result<(), WireError> {
    use mount_rs_core::GuardedMutation::*;
    match request {
        Setattr { target, .. } => validate_guard(target),
        Open { parent, name, .. }
        | Mkdir { parent, name, .. }
        | Symlink { parent, name, .. }
        | Mknod { parent, name, .. }
        | Unlink { parent, name, .. }
        | Rmdir { parent, name, .. } => {
            validate_guard(parent)?;
            child(name)
        }
        Rename {
            from_parent,
            from_name,
            to_parent,
            to_name,
            ..
        } => {
            validate_guard(from_parent)?;
            validate_guard(to_parent)?;
            child(from_name)?;
            child(to_name)
        }
        Link {
            source,
            to_parent,
            to_name,
            ..
        } => {
            validate_guard(source)?;
            validate_guard(to_parent)?;
            child(to_name)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct CountedHandle(Arc<AtomicUsize>);
    #[async_trait::async_trait]
    impl FileHandle for CountedHandle {
        async fn read(&self, _: &mut [u8], _: Option<u64>) -> mount_rs_core::Result<usize> {
            Err(mount_rs_core::FsError::enosys("read"))
        }
        async fn write(&self, _: &[u8], _: Option<u64>) -> mount_rs_core::Result<usize> {
            Err(mount_rs_core::FsError::enosys("write"))
        }
        async fn stat(&self) -> mount_rs_core::Result<mount_rs_core::Stats> {
            Err(mount_rs_core::FsError::enosys("stat"))
        }
        async fn truncate(&self, _: u64) -> mount_rs_core::Result<()> {
            Err(mount_rs_core::FsError::enosys("truncate"))
        }
        async fn close(&self) -> mount_rs_core::Result<()> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }
    #[tokio::test]
    async fn in_flight_open_cannot_reinsert_after_revision_change_or_shutdown() {
        let handles = SessionHandles::default();
        let closed = Arc::new(AtomicUsize::new(0));
        handles.refresh_revision(1).await;
        handles.refresh_revision(2).await;
        let result = handles
            .insert("data", 1, Arc::new(CountedHandle(closed.clone())))
            .await;
        assert_eq!(result.unwrap_err().code, "ESTALE");
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        handles.refresh_revision(1).await;
        assert_eq!(handles.state.lock().await.revision, Some(2));
        handles.close_all().await;
        let result = handles
            .insert("data", 2, Arc::new(CountedHandle(closed.clone())))
            .await;
        assert_eq!(result.unwrap_err().code, "EBADF");
        assert_eq!(closed.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn handle_capacity_and_counter_exhaustion_close_rejected_handles() {
        let handles = SessionHandles::default();
        let closed = Arc::new(AtomicUsize::new(0));
        handles.refresh_revision(1).await;
        for _ in 0..1024 {
            handles
                .insert("data", 1, Arc::new(CountedHandle(closed.clone())))
                .await
                .unwrap();
        }
        assert_eq!(
            handles
                .insert("data", 1, Arc::new(CountedHandle(closed.clone())))
                .await
                .unwrap_err()
                .code,
            "EMFILE"
        );
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        handles.refresh_revision(2).await;
        assert_eq!(closed.load(Ordering::SeqCst), 1025);
        handles.state.lock().await.next = u64::MAX;
        assert_eq!(
            handles
                .insert("data", 2, Arc::new(CountedHandle(closed.clone())))
                .await
                .unwrap_err()
                .code,
            "EMFILE"
        );
        assert_eq!(closed.load(Ordering::SeqCst), 1026);
    }

    #[tokio::test]
    async fn catalog_revision_closes_old_handles_and_releases_slots() {
        let handles = SessionHandles::default();
        let closed = Arc::new(AtomicUsize::new(0));
        handles.refresh_revision(1).await;
        let old = handles
            .insert("data", 1, Arc::new(CountedHandle(closed.clone())))
            .await
            .unwrap();
        handles.refresh_revision(2).await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        assert!(!handles.state.lock().await.entries.contains_key(&old));
        let new = handles
            .insert("data", 2, Arc::new(CountedHandle(closed.clone())))
            .await
            .unwrap();
        assert_ne!(old, new);
        handles.refresh_revision(2).await;
        assert_eq!(closed.load(Ordering::SeqCst), 1);
        handles.close_all().await;
        assert_eq!(closed.load(Ordering::SeqCst), 2);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;
    #[kani::proof]
    #[kani::unwind(3)]
    fn remote_handles_require_exact_drive_and_revision() {
        let same_drive: bool = kani::any();
        let stored_revision: u64 = kani::any();
        let requested_revision: u64 = kani::any();
        let accepted = handle_matches(
            "a",
            if same_drive { "a" } else { "b" },
            stored_revision,
            requested_revision,
        );
        assert_eq!(
            accepted,
            same_drive && stored_revision == requested_revision
        );
        kani::cover!(accepted);
        kani::cover!(!accepted && !same_drive && stored_revision == requested_revision);
        kani::cover!(!accepted && same_drive && stored_revision != requested_revision);
    }

    #[kani::proof]
    fn remote_read_grants_cannot_authorize_mutations() {
        let write_grant: bool = kani::any();
        let mutation: bool = kani::any();
        let granted = if write_grant {
            Permission::Write
        } else {
            Permission::Read
        };
        let required = if mutation {
            mount_rs_remote_protocol::Permission::Write
        } else {
            mount_rs_remote_protocol::Permission::Read
        };
        let allowed = permission_allows(granted, required);
        assert_eq!(allowed, write_grant || !mutation);
        assert!(!(allowed && mutation && !write_grant));
        kani::cover!(allowed && mutation);
        kani::cover!(allowed && !write_grant);
        kani::cover!(!allowed);
    }

    #[kani::proof]
    fn remote_handle_admission_is_fenced_and_bounded() {
        let closed: bool = kani::any();
        let present: bool = kani::any();
        let current: u64 = kani::any();
        let revision: u64 = kani::any();
        let count: usize = kani::any();
        let next: u64 = kani::any();
        let result = admit_handle(
            closed,
            if present { Some(current) } else { None },
            revision,
            count,
            next,
        );
        let allowed = !closed && present && current == revision && count < 1024 && next < u64::MAX;
        assert_eq!(result.is_ok(), allowed);
        if let Ok(id) = result {
            assert!(id > next);
            assert_eq!(id, next + 1);
        }
        kani::cover!(result.is_ok());
        kani::cover!(result == Err(HandleRejection::Closed));
        kani::cover!(result == Err(HandleRejection::Stale));
        kani::cover!(result == Err(HandleRejection::Full));
    }
}
