//! FsDriver adapter over the bounded remote request protocol.

use std::sync::Arc;
use tokio::sync::RwLock;

use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, FsError, GuardedMutation,
    GuardedMutationResult, GuardedRead, GuardedReadResult, MkdirOptions, PathIdentity, Stats,
    StatsFs,
};
use mount_rs_remote_protocol::{Operation, OperationName};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::connection::{ClientError, RemoteConnection};

const MAX_IO: usize = 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 4096;

pub struct RemoteFsDriver {
    connection: Arc<RemoteConnection>,
    drive_id: String,
    capabilities: Capabilities,
    guarded_reads: bool,
    guarded_mutations: bool,
    stable_inode_ids: bool,
    utimens: bool,
}

impl RemoteFsDriver {
    pub async fn new(
        connection: Arc<RemoteConnection>,
        drive_id: String,
    ) -> Result<Self, ClientError> {
        let value = connection
            .request(
                &drive_id,
                Operation {
                    name: OperationName::Capabilities,
                    body: Value::Null,
                },
            )
            .await?;
        let guarded_reads = value
            .get("guarded_reads")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let guarded_mutations = value
            .get("guarded_mutations")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let stable_inode_ids = value
            .get("stable_inode_ids")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let utimens = value
            .get("utimens")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let capabilities = serde_json::from_value(value).map_err(|_| ClientError::Protocol)?;
        Ok(Self {
            connection,
            drive_id,
            capabilities,
            guarded_reads,
            guarded_mutations,
            stable_inode_ids,
            utimens,
        })
    }

    async fn call<T: DeserializeOwned>(
        &self,
        name: OperationName,
        body: Value,
    ) -> mount_rs_core::Result<T> {
        call(&self.connection, &self.drive_id, name, body).await
    }
}

async fn call<T: DeserializeOwned>(
    connection: &RemoteConnection,
    drive_id: &str,
    name: OperationName,
    body: Value,
) -> mount_rs_core::Result<T> {
    let value = connection
        .request(drive_id, Operation { name, body })
        .await
        .map_err(fs_error)?;
    serde_json::from_value(value).map_err(|_| FsError::new(ErrorCode::Eproto))
}

fn fs_error(error: ClientError) -> FsError {
    let code = match error {
        ClientError::Remote(code) => match code.as_str() {
            "EPERM" => ErrorCode::Eperm,
            "ENOENT" => ErrorCode::Enoent,
            "EINTR" => ErrorCode::Eintr,
            "EIO" => ErrorCode::Eio,
            "ENXIO" => ErrorCode::Enxio,
            "EBADF" => ErrorCode::Ebadf,
            "EAGAIN" => ErrorCode::Eagain,
            "ENOMEM" => ErrorCode::Enomem,
            "EACCES" => ErrorCode::Eacces,
            "EBUSY" => ErrorCode::Ebusy,
            "EEXIST" => ErrorCode::Eexist,
            "EXDEV" => ErrorCode::Exdev,
            "ENODEV" => ErrorCode::Enodev,
            "ENOTDIR" => ErrorCode::Enotdir,
            "EISDIR" => ErrorCode::Eisdir,
            "EINVAL" => ErrorCode::Einval,
            "ENFILE" => ErrorCode::Enfile,
            "EMFILE" => ErrorCode::Emfile,
            "EFBIG" => ErrorCode::Efbig,
            "ENOSPC" => ErrorCode::Enospc,
            "ESPIPE" => ErrorCode::Espipe,
            "EROFS" => ErrorCode::Erofs,
            "EMLINK" => ErrorCode::Emlink,
            "ERANGE" => ErrorCode::Erange,
            "ENAMETOOLONG" => ErrorCode::Enametoolong,
            "ENOSYS" => ErrorCode::Enosys,
            "ENOTEMPTY" => ErrorCode::Enotempty,
            "ELOOP" => ErrorCode::Eloop,
            "ENODATA" => ErrorCode::Enodata,
            "EPROTO" => ErrorCode::Eproto,
            "EOVERFLOW" => ErrorCode::Eoverflow,
            "ENOTSUP" => ErrorCode::Enotsup,
            "ESTALE" => ErrorCode::Estale,
            "EDQUOT" => ErrorCode::Edquot,
            _ => ErrorCode::Eproto,
        },
        ClientError::Authentication | ClientError::Credential => ErrorCode::Eacces,
        ClientError::Transport | ClientError::Protocol => ErrorCode::Eio,
    };
    FsError::new(code)
}

struct RemoteHandle {
    connection: Arc<RemoteConnection>,
    drive_id: String,
    id: u64,
    closed: RwLock<bool>,
}

impl RemoteHandle {
    async fn call<T: DeserializeOwned>(
        &self,
        name: OperationName,
        mut body: Value,
    ) -> mount_rs_core::Result<T> {
        let closed = self.closed.read().await;
        if *closed {
            return Err(FsError::new(ErrorCode::Ebadf));
        }
        body["handle"] = json!(self.id);
        let result = call(&self.connection, &self.drive_id, name, body).await;
        drop(closed);
        result
    }
}

#[async_trait]
impl FileHandle for RemoteHandle {
    fn fd(&self) -> Option<u64> {
        Some(self.id)
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> mount_rs_core::Result<usize> {
        let closed = self.closed.read().await;
        if *closed {
            return Err(FsError::new(ErrorCode::Ebadf));
        }
        let length = buffer.len().min(MAX_IO);
        self.connection
            .read(&self.drive_id, self.id, position, &mut buffer[..length])
            .await
            .map_err(fs_error)
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> mount_rs_core::Result<usize> {
        let closed = self.closed.read().await;
        if *closed {
            return Err(FsError::new(ErrorCode::Ebadf));
        }
        self.connection
            .write(
                &self.drive_id,
                self.id,
                position,
                &buffer[..buffer.len().min(MAX_IO)],
            )
            .await
            .map_err(fs_error)
    }

    async fn stat(&self) -> mount_rs_core::Result<Stats> {
        self.call(OperationName::HandleStat, json!({})).await
    }
    async fn truncate(&self, length: u64) -> mount_rs_core::Result<()> {
        self.call(OperationName::HandleTruncate, json!({"length":length}))
            .await
    }
    async fn sync(&self) -> mount_rs_core::Result<()> {
        self.call(OperationName::HandleSync, json!({})).await
    }
    async fn datasync(&self) -> mount_rs_core::Result<()> {
        self.call(OperationName::HandleDatasync, json!({})).await
    }
    async fn close(&self) -> mount_rs_core::Result<()> {
        let mut closed = self.closed.write().await;
        if *closed {
            return Err(FsError::new(ErrorCode::Ebadf));
        }
        *closed = true;
        call(
            &self.connection,
            &self.drive_id,
            OperationName::HandleClose,
            json!({"handle":self.id}),
        )
        .await
    }
}

#[async_trait]
impl FsDriver for RemoteFsDriver {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }
    fn supports_guarded_reads(&self) -> bool {
        self.guarded_reads
    }
    fn supports_guarded_mutations(&self) -> bool {
        self.guarded_mutations
    }
    fn stable_inode_ids(&self) -> bool {
        self.stable_inode_ids
    }

    async fn guarded_read(&self, request: GuardedRead) -> mount_rs_core::Result<GuardedReadResult> {
        if !self.guarded_reads {
            return Err(FsError::enotsup("guarded read"));
        }
        let body = serde_json::to_value(request).map_err(|_| FsError::new(ErrorCode::Einval))?;
        self.call(OperationName::GuardedRead, body).await
    }

    async fn guarded_mutation(
        &self,
        request: GuardedMutation,
    ) -> mount_rs_core::Result<GuardedMutationResult> {
        if !self.guarded_mutations {
            return Err(FsError::enotsup("guarded mutation"));
        }
        let body = serde_json::to_value(request).map_err(|_| FsError::new(ErrorCode::Einval))?;
        let response: Value = self.call(OperationName::GuardedMutation, body).await?;
        match response.get("kind").and_then(Value::as_str) {
            Some("applied") => Ok(GuardedMutationResult::Applied),
            Some("created") => {
                let identity: PathIdentity = serde_json::from_value(
                    response
                        .get("identity")
                        .cloned()
                        .ok_or_else(|| FsError::new(ErrorCode::Eproto))?,
                )
                .map_err(|_| FsError::new(ErrorCode::Eproto))?;
                Ok(GuardedMutationResult::Created(identity))
            }
            Some("opened") => {
                let identity: PathIdentity = serde_json::from_value(
                    response
                        .get("identity")
                        .cloned()
                        .ok_or_else(|| FsError::new(ErrorCode::Eproto))?,
                )
                .map_err(|_| FsError::new(ErrorCode::Eproto))?;
                let id = response
                    .get("handle")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| FsError::new(ErrorCode::Eproto))?;
                let handle: Arc<dyn FileHandle> = Arc::new(RemoteHandle {
                    connection: Arc::clone(&self.connection),
                    drive_id: self.drive_id.clone(),
                    id,
                    closed: RwLock::new(false),
                });
                Ok(GuardedMutationResult::Opened { handle, identity })
            }
            _ => Err(FsError::new(ErrorCode::Eproto)),
        }
    }

    async fn stat(&self, path: &str) -> mount_rs_core::Result<Stats> {
        self.call(OperationName::Stat, json!({"path":path})).await
    }
    async fn lstat(&self, path: &str) -> mount_rs_core::Result<Stats> {
        self.call(OperationName::Lstat, json!({"path":path})).await
    }
    async fn statfs(&self, path: &str) -> mount_rs_core::Result<StatsFs> {
        self.call(OperationName::Statfs, json!({"path":path})).await
    }
    async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<DirEntry>> {
        self.readdir_bounded(path, MAX_DIRECTORY_ENTRIES).await
    }
    async fn readdir_bounded(
        &self,
        path: &str,
        max_entries: usize,
    ) -> mount_rs_core::Result<Vec<DirEntry>> {
        if max_entries == 0 || max_entries > MAX_DIRECTORY_ENTRIES {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let entries: Vec<DirEntry> = self
            .call(
                OperationName::ReaddirBounded,
                json!({"path":path,"max_entries":max_entries}),
            )
            .await?;
        if entries.len() > max_entries {
            return Err(FsError::new(ErrorCode::Eproto));
        }
        Ok(entries)
    }
    async fn open(
        &self,
        path: &str,
        flags: &str,
        mode: u32,
    ) -> mount_rs_core::Result<Arc<dyn FileHandle>> {
        let id: u64 = self
            .call(
                OperationName::Open,
                json!({"path":path,"flags":flags,"mode":mode}),
            )
            .await?;
        Ok(Arc::new(RemoteHandle {
            connection: Arc::clone(&self.connection),
            drive_id: self.drive_id.clone(),
            id,
            closed: RwLock::new(false),
        }))
    }
    async fn open_flags(
        &self,
        path: &str,
        flags: mount_rs_core::OpenFlags,
        mode: u32,
    ) -> mount_rs_core::Result<Arc<dyn FileHandle>> {
        let id: u64 = self
            .call(
                OperationName::Open,
                json!({"path":path,"flags":flags,"mode":mode}),
            )
            .await?;
        Ok(Arc::new(RemoteHandle {
            connection: Arc::clone(&self.connection),
            drive_id: self.drive_id.clone(),
            id,
            closed: RwLock::new(false),
        }))
    }
    async fn mkdir(
        &self,
        path: &str,
        options: MkdirOptions,
    ) -> mount_rs_core::Result<Option<String>> {
        self.call(
            OperationName::Mkdir,
            json!({"path":path,"recursive":options.recursive,"mode":options.mode.unwrap_or(0o777)}),
        )
        .await
    }
    async fn rmdir(&self, path: &str) -> mount_rs_core::Result<()> {
        self.call(OperationName::Rmdir, json!({"path":path})).await
    }
    async fn unlink(&self, path: &str) -> mount_rs_core::Result<()> {
        self.call(OperationName::Unlink, json!({"path":path})).await
    }
    async fn rename(&self, old_path: &str, new_path: &str) -> mount_rs_core::Result<()> {
        self.call(
            OperationName::Rename,
            json!({"path":old_path,"destination":new_path}),
        )
        .await
    }
    async fn link(&self, existing_path: &str, new_path: &str) -> mount_rs_core::Result<()> {
        self.call(
            OperationName::Link,
            json!({"path":existing_path,"destination":new_path}),
        )
        .await
    }
    async fn symlink(&self, target: &str, path: &str) -> mount_rs_core::Result<()> {
        self.call(OperationName::Symlink, json!({"target":target,"path":path}))
            .await
    }
    async fn readlink(&self, path: &str) -> mount_rs_core::Result<String> {
        self.call(OperationName::Readlink, json!({"path":path}))
            .await
    }
    async fn chmod(&self, path: &str, mode: u32) -> mount_rs_core::Result<()> {
        self.call(OperationName::Chmod, json!({"path":path,"mode":mode}))
            .await
    }
    async fn chown(&self, path: &str, uid: u32, gid: u32) -> mount_rs_core::Result<()> {
        self.call(
            OperationName::Chown,
            json!({"path":path,"uid":uid,"gid":gid}),
        )
        .await
    }
    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> mount_rs_core::Result<()> {
        self.call(
            OperationName::Lchown,
            json!({"path":path,"uid":uid,"gid":gid}),
        )
        .await
    }
    async fn truncate(&self, path: &str, length: u64) -> mount_rs_core::Result<()> {
        self.call(
            OperationName::Truncate,
            json!({"path":path,"length":length}),
        )
        .await
    }
    fn has_utimens(&self) -> bool {
        self.utimens
    }
    async fn utimens(
        &self,
        path: &str,
        atime_ns: i128,
        mtime_ns: i128,
        follow_symlinks: bool,
    ) -> mount_rs_core::Result<()> {
        self.call(OperationName::Utimens, json!({"path":path,"atime":atime_ns.to_string(),"mtime":mtime_ns.to_string(),"follow":follow_symlinks})).await
    }
    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> mount_rs_core::Result<()> {
        self.call(
            OperationName::Utimes,
            json!({"path":path,"atime":atime_ms,"mtime":mtime_ms}),
        )
        .await
    }
    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> mount_rs_core::Result<()> {
        self.call(
            OperationName::Lutimes,
            json!({"path":path,"atime":atime_ms,"mtime":mtime_ms}),
        )
        .await
    }
    async fn mknod(&self, path: &str, mode: u32, dev: u64) -> mount_rs_core::Result<()> {
        self.call(
            OperationName::Mknod,
            json!({"path":path,"mode":mode,"dev":dev}),
        )
        .await
    }
    async fn syncfs(&self) -> mount_rs_core::Result<()> {
        self.call(OperationName::Syncfs, Value::Null).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::{ClientError, Transport};
    use mount_rs_remote_protocol::Message;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeTransport {
        seen: Mutex<Vec<Operation>>,
        guards: bool,
    }

    #[async_trait]
    impl Transport for FakeTransport {
        async fn exchange(&self, message: Message) -> Result<Message, ClientError> {
            let Message::Request {
                request_id,
                operation,
                ..
            } = message
            else {
                panic!("request expected")
            };
            let response = match operation.name {
                OperationName::Capabilities => {
                    let mut value = serde_json::to_value(Capabilities::default()).unwrap();
                    if self.guards {
                        value["guarded_reads"] = json!(true);
                        value["guarded_mutations"] = json!(true);
                        value["stable_inode_ids"] = json!(true);
                        value["utimens"] = json!(true);
                    }
                    value
                }
                OperationName::Stat => {
                    serde_json::json!({"dev":1,"ino":2,"mode":33188,"nlink":1,"uid":0,"gid":0,"rdev":0,"size":0,"blksize":4096,"blocks":0,"atime_ms":0,"mtime_ms":0,"ctime_ms":0,"birthtime_ms":0})
                }
                OperationName::GuardedRead => {
                    json!({"Stat":{"dev":1,"ino":2,"mode":33188,"nlink":1,"uid":0,"gid":0,"rdev":0,"size":0,"blksize":4096,"blocks":0,"atime_ms":0,"mtime_ms":0,"ctime_ms":0,"birthtime_ms":0}})
                }
                OperationName::GuardedMutation => json!({"kind":"applied"}),
                OperationName::Open => json!(7),
                _ => Value::Null,
            };
            self.seen.lock().unwrap().push(operation);
            Ok(Message::Response {
                request_id,
                result: Ok(response),
            })
        }
    }

    #[tokio::test]
    async fn forwards_stat_and_keeps_guard_claims_false() {
        let transport = Arc::new(FakeTransport::default());
        let connection = RemoteConnection::from_transport(transport.clone());
        let driver = RemoteFsDriver::new(connection, "drive".into())
            .await
            .unwrap();
        assert!(!driver.supports_guarded_reads());
        assert!(!driver.supports_guarded_mutations());
        assert!(!driver.stable_inode_ids());
        assert_eq!(driver.stat("/hello").await.unwrap().ino, 2);
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[1].name, OperationName::Stat);
        assert_eq!(seen[1].body, serde_json::json!({"path":"/hello"}));
    }

    #[tokio::test]
    async fn decoded_exclusive_create_preserves_no_truncate() {
        let transport = Arc::new(FakeTransport::default());
        let connection = RemoteConnection::from_transport(transport.clone());
        let driver = RemoteFsDriver::new(connection, "drive".into())
            .await
            .unwrap();
        let flags = mount_rs_core::OpenFlags {
            read: false,
            write: true,
            create: true,
            truncate: false,
            append: false,
            exclusive: true,
        };
        let handle = driver.open_flags("/new", flags, 0o644).await.unwrap();
        assert_eq!(handle.fd(), Some(7));
        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen[1].name, OperationName::Open);
        assert_eq!(
            seen[1].body,
            json!({"path":"/new","flags":{"read":false,"write":true,"create":true,"truncate":false,"append":false,"exclusive":true},"mode":0o644})
        );
    }

    #[tokio::test]
    async fn forwards_negotiated_guard_ops_and_closed_handle_is_badf() {
        let transport = Arc::new(FakeTransport {
            guards: true,
            ..FakeTransport::default()
        });
        let connection = RemoteConnection::from_transport(transport.clone());
        let driver = RemoteFsDriver::new(connection, "drive".into())
            .await
            .unwrap();
        assert!(driver.supports_guarded_reads());
        assert!(driver.supports_guarded_mutations());
        assert!(driver.stable_inode_ids());
        assert!(driver.has_utimens());
        let target = mount_rs_core::PathGuard {
            path: "/hello".into(),
            identity: PathIdentity { dev: 1, ino: 2 },
        };
        let read = driver
            .guarded_read(GuardedRead::Stat {
                target: target.clone(),
            })
            .await
            .unwrap();
        assert!(matches!(read, GuardedReadResult::Stat(stats) if stats.ino == 2));
        let mutation = driver
            .guarded_mutation(GuardedMutation::Setattr {
                target,
                change: mount_rs_core::GuardedSetattr::default(),
            })
            .await
            .unwrap();
        assert!(matches!(mutation, GuardedMutationResult::Applied));
        let handle = driver.open("/hello", "r", 0).await.unwrap();
        handle.close().await.unwrap();
        assert_eq!(handle.stat().await.unwrap_err().code, ErrorCode::Ebadf);
    }
}
