//! Byte-oriented NFSv3 and MOUNTv3 session.
//!
//! `Nfs3Session` performs no socket I/O. It decodes one complete RPC record,
//! executes the corresponding [`FsDriver`] operation, and returns one complete
//! RPC reply. This keeps protocol behavior independently testable and is also
//! the boundary used by the TCP server.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mount_rs_core::{
    ErrorCode, FileHandle, FsDriver, FsError, Loopback, MkdirOptions, OpenFlags,
    Result as FsResult, S_IFBLK, S_IFCHR, S_IFDIR, S_IFIFO, S_IFMT, S_IFSOCK,
};

use crate::constants::*;
use crate::handles::{
    DirectorySnapshot, DirectorySnapshots, FH_SIZE, FileHandleTable, FileHandleTableOptions,
    HandleEntry, cookie_verifier, same_verifier,
};
use crate::protocol::*;
use crate::rpc::{
    AUTH_NONE, AUTH_SYS, AUTH_TOOWEAK, RPC_GARBAGE_ARGS, RPC_PROC_UNAVAIL, RPC_PROG_MISMATCH,
    RPC_PROG_UNAVAIL, RPC_VERSION, RpcCall, RpcCredentials, credentials_of, decode_call,
    encode_accept_error, encode_auth_error, encode_rpc_mismatch, write_accepted_reply_header,
};
use crate::v4::{NFS_V4, Nfs4Session};
use crate::xdr::{XdrError, XdrReader, XdrWriter};

pub const DEFAULT_RTMAX: usize = 1024 * 1024;
pub const DEFAULT_WTMAX: usize = 1024 * 1024;
pub const DEFAULT_DTPREF: usize = 32 * 1024;
pub const DEFAULT_NFS4_LEASE_SECONDS: u32 = 90;
pub const DEFAULT_NFS4_MAX_SESSIONS: usize = 4;
pub const DEFAULT_NFS4_MAX_FORE_SLOTS: usize = 64;
pub const DEFAULT_NFS4_MAX_OPERATIONS: usize = 64;
pub const DEFAULT_NFS4_MAX_REQUEST_SIZE: usize = 1024 * 1024;
pub const DEFAULT_NFS4_MAX_CACHED_RESPONSE_SIZE: usize = 64 * 1024;
pub const DEFAULT_NFS4_MAX_OPENS_PER_FILE: usize = 256;
pub const DEFAULT_NFS4_MAX_LOCKS_PER_FILE: usize = 1024;
pub const MAX_OFFSET: u64 = 9_007_199_254_740_991;
pub const NAME_MAX: usize = 255;
pub const DEFAULT_EXCLUSIVE_CREATES: usize = 256;
pub const EXCLUSIVE_CREATE_WINDOW: Duration = Duration::from_secs(120);
const S_ISGID: u32 = 0o2000;
const S_IXGRP: u32 = 0o0010;

/// A synchronous NFSv4 owner-name callback.
pub type Nfs4NameCallback = Arc<dyn Fn(u32, bool) -> Option<String> + Send + Sync + 'static>;

/// A synchronous NFSv4 owner-id callback.
pub type Nfs4IdCallback = Arc<dyn Fn(&str, bool) -> Option<u32> + Send + Sync + 'static>;

/// An NFSv4 owner-name translation table.
///
/// RFC 8881 carries owners as strings rather than numeric uids/gids.  The
/// default wire representation remains the numeric form; this map lets an
/// embedding server opt into stable local names. The callback forms mirror
/// the upstream synchronous `nameOf`/`idOf` contract and are invoked only
/// while translating one request's attributes.
#[derive(Clone, Default)]
pub struct Nfs4IdMap {
    domain: Option<String>,
    users: HashMap<String, u32>,
    groups: HashMap<String, u32>,
    users_by_id: HashMap<u32, String>,
    groups_by_id: HashMap<u32, String>,
    name_callback: Option<Nfs4NameCallback>,
    id_callback: Option<Nfs4IdCallback>,
}

impl fmt::Debug for Nfs4IdMap {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Nfs4IdMap")
            .field("domain", &self.domain)
            .field("users", &self.users)
            .field("groups", &self.groups)
            .field("name_callback", &self.name_callback.is_some())
            .field("id_callback", &self.id_callback.is_some())
            .finish()
    }
}

impl Nfs4IdMap {
    /// Create an empty map. `domain` qualifies names returned by the reverse
    /// lookup unless the configured name already contains `@`.
    pub fn new(domain: Option<String>) -> Self {
        Self {
            domain,
            ..Self::default()
        }
    }

    /// Add one user name to uid mapping.
    pub fn with_user(mut self, name: impl Into<String>, id: u32) -> Self {
        let name = name.into();
        if let Some(previous_id) = self.users.insert(name.clone(), id)
            && previous_id != id
        {
            self.users_by_id.remove(&previous_id);
        }
        if let Some(previous_name) = self.users_by_id.insert(id, name.clone())
            && previous_name != name
        {
            self.users.remove(&previous_name);
        }
        self
    }

    /// Add one group name to gid mapping.
    pub fn with_group(mut self, name: impl Into<String>, id: u32) -> Self {
        let name = name.into();
        if let Some(previous_id) = self.groups.insert(name.clone(), id)
            && previous_id != id
        {
            self.groups_by_id.remove(&previous_id);
        }
        if let Some(previous_name) = self.groups_by_id.insert(id, name.clone())
            && previous_name != name
        {
            self.groups.remove(&previous_name);
        }
        self
    }

    /// Install the synchronous uid/gid-to-owner callback.
    ///
    /// The `group` flag distinguishes `owner_group` from `owner`. Returning
    /// `None` uses the numeric wire form. A callback takes precedence over the
    /// static reverse table when both are configured.
    pub fn with_name_of<F>(mut self, callback: F) -> Self
    where
        F: Fn(u32, bool) -> Option<String> + Send + Sync + 'static,
    {
        self.name_callback = Some(Arc::new(callback));
        self
    }

    /// Install the synchronous owner-to-uid/gid callback.
    ///
    /// The `group` flag distinguishes an `owner_group` from an `owner`.
    /// Returning `None` produces `NFS4ERR_BADOWNER` for a non-numeric owner.
    /// A callback takes precedence over the static forward table when both are
    /// configured.
    pub fn with_id_of<F>(mut self, callback: F) -> Self
    where
        F: Fn(&str, bool) -> Option<u32> + Send + Sync + 'static,
    {
        self.id_callback = Some(Arc::new(callback));
        self
    }

    /// The configured DNS domain, if any.
    pub fn domain(&self) -> Option<&str> {
        self.domain.as_deref()
    }

    /// Resolve a uid/gid to its local name, if the map has one.
    pub fn name_of(&self, id: u32, group: bool) -> Option<&str> {
        if group {
            self.groups_by_id.get(&id).map(String::as_str)
        } else {
            self.users_by_id.get(&id).map(String::as_str)
        }
    }

    /// Resolve a local owner name to a uid/gid, if the map has one.
    pub fn id_of(&self, name: &str, group: bool) -> Option<u32> {
        if group {
            self.groups.get(name).copied()
        } else {
            self.users.get(name).copied()
        }
    }

    /// Resolve a uid/gid using the configured callback or static table.
    /// Callback panics are treated as an unavailable translation so a mapping
    /// failure cannot take down the NFS request task.
    pub(crate) fn resolve_name(&self, id: u32, group: bool) -> Option<String> {
        if let Some(callback) = &self.name_callback {
            return catch_unwind(AssertUnwindSafe(|| callback(id, group)))
                .ok()
                .flatten();
        }
        self.name_of(id, group).map(str::to_owned)
    }

    /// Resolve an owner name using the configured callback or static table.
    /// Callback panics are treated as an unavailable translation.
    pub(crate) fn resolve_id(&self, name: &str, group: bool) -> Option<u32> {
        if let Some(callback) = &self.id_callback {
            return catch_unwind(AssertUnwindSafe(|| callback(name, group)))
                .ok()
                .flatten();
        }
        self.id_of(name, group)
    }
}

/// Clock used by the NFSv4 lease table.
///
/// The default uses the process monotonic clock. Rust callers can inject a
/// deterministic clock for lease and expiry tests without making the wire
/// session depend on wall-clock time. The N-API adapter retains this default
/// unless its synchronous JavaScript `now` callback is configured.
#[derive(Clone)]
pub struct Nfs4Clock(Arc<dyn Fn() -> Instant + Send + Sync + 'static>);

impl Nfs4Clock {
    /// Use the process monotonic clock.
    pub fn system() -> Self {
        Self::from_fn(Instant::now)
    }

    /// Construct a clock from a thread-safe monotonic time source.
    pub fn from_fn<F>(now: F) -> Self
    where
        F: Fn() -> Instant + Send + Sync + 'static,
    {
        Self(Arc::new(now))
    }

    pub(crate) fn now(&self) -> Instant {
        (self.0)()
    }
}

impl Default for Nfs4Clock {
    fn default() -> Self {
        Self::system()
    }
}

impl fmt::Debug for Nfs4Clock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Nfs4Clock(<injected monotonic source>)")
    }
}

#[derive(Debug, Clone)]
pub struct Nfs4StateOptions {
    /// Lease length reported by FATTR4_LEASE_TIME.
    pub lease_seconds: u32,
    /// Monotonic source used to enforce the NFSv4 client lease.
    pub clock: Nfs4Clock,
    /// Upper 32 bits folded into NFSv4 client and session identities.
    ///
    /// A stable non-zero value keeps identities from separate process
    /// instances distinguishable. The default is zero for deterministic
    /// compatibility with the original process-local sequence.
    pub seed: u32,
    /// Maximum number of sessions one client may create.
    pub max_sessions: usize,
    /// Fore-channel slot ceiling advertised by CREATE_SESSION.
    pub max_fore_slots: usize,
    /// COMPOUND operation ceiling advertised and enforced per session.
    pub max_operations: usize,
    /// Fore-channel request/response size ceiling advertised by CREATE_SESSION.
    pub max_request_size: usize,
    /// Maximum cached replay response size.
    pub max_cached_response_size: usize,
    /// Maximum number of open states a file may carry across clients.
    pub max_opens_per_file: usize,
    /// Maximum number of granted lock ranges a file may carry.
    pub max_locks_per_file: usize,
    /// Require RECLAIM_COMPLETE before granting a new byte-range lock.
    pub require_reclaim_complete: bool,
    /// Optional static or callback-backed uid/gid to NFSv4 owner translation.
    pub idmap: Option<Nfs4IdMap>,
}

impl Default for Nfs4StateOptions {
    fn default() -> Self {
        Self {
            lease_seconds: DEFAULT_NFS4_LEASE_SECONDS,
            clock: Nfs4Clock::default(),
            seed: 0,
            max_sessions: DEFAULT_NFS4_MAX_SESSIONS,
            max_fore_slots: DEFAULT_NFS4_MAX_FORE_SLOTS,
            max_operations: DEFAULT_NFS4_MAX_OPERATIONS,
            max_request_size: DEFAULT_NFS4_MAX_REQUEST_SIZE,
            max_cached_response_size: DEFAULT_NFS4_MAX_CACHED_RESPONSE_SIZE,
            max_opens_per_file: DEFAULT_NFS4_MAX_OPENS_PER_FILE,
            max_locks_per_file: DEFAULT_NFS4_MAX_LOCKS_PER_FILE,
            require_reclaim_complete: true,
            idmap: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NfsSessionOptions {
    pub use_driver_ino: bool,
    pub verifier: Option<[u8; 8]>,
    /// Positive values bound the shared handle table with a soft LRU cap.
    pub max_handles: Option<usize>,
    pub rtmax: usize,
    pub wtmax: usize,
    pub dtpref: usize,
    pub snapshot_cache: usize,
    pub claim_ownership: bool,
    /// NFSv4.1 state and channel limits. These are ignored by the v3 session.
    pub nfs4: Nfs4StateOptions,
}

impl Default for NfsSessionOptions {
    fn default() -> Self {
        Self {
            use_driver_ino: true,
            verifier: None,
            max_handles: None,
            rtmax: DEFAULT_RTMAX,
            wtmax: DEFAULT_WTMAX,
            dtpref: DEFAULT_DTPREF,
            snapshot_cache: 64,
            claim_ownership: true,
            nfs4: Nfs4StateOptions::default(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct NfsRequestContext {
    pub peer: Option<String>,
}

/// One request-level failure reported by an NFS session.
///
/// Driver and protocol-status failures do not have a single Rust error value
/// at the session boundary, so they carry a stable message and no XDR offset.
/// Decode failures retain their byte offset for diagnostics. The optional RPC
/// call is present when the call header decoded successfully, matching the
/// upstream `onError(error, call | undefined)` contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NfsSessionError {
    pub message: String,
    pub offset: Option<usize>,
}

impl NfsSessionError {
    pub(crate) fn status(status: u32) -> Self {
        Self {
            message: format!("NFS request returned status {status}"),
            offset: None,
        }
    }

    pub(crate) fn generic(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            offset: None,
        }
    }
}

impl From<XdrError> for NfsSessionError {
    fn from(error: XdrError) -> Self {
        Self {
            message: error.message,
            offset: Some(error.offset),
        }
    }
}

/// Synchronous callback used by a session to report one request-level error.
///
/// The hook owns both values and may therefore cross an embedding boundary
/// without borrowing the dispatch future. Implementations must remain quick;
/// adapters that need another runtime should enqueue the owned event.
pub type NfsSessionErrorHook =
    Arc<dyn Fn(NfsSessionError, Option<RpcCall>) + Send + Sync + 'static>;

/// Optional request-level hooks for an [`Nfs3Session`] or [`Nfs4Session`].
/// Kept separate from [`NfsSessionOptions`] so existing option literals remain
/// source-compatible.
#[derive(Clone, Default)]
pub struct NfsSessionHooks {
    pub on_error: Option<NfsSessionErrorHook>,
}

impl NfsSessionHooks {
    pub(crate) fn report(&self, error: NfsSessionError, call: Option<RpcCall>) {
        let Some(hook) = self.on_error.as_ref().cloned() else {
            return;
        };
        // A diagnostic callback must not turn one failed request into a
        // failed transport task or take down the process.
        let _ = catch_unwind(AssertUnwindSafe(|| hook(error, call)));
    }
}

#[derive(Debug, Clone, Default)]
pub struct NfsSessionStats {
    pub requests: u64,
    pub replies: u64,
    pub errors: u64,
    pub dropped: u64,
    pub procedures: HashMap<String, u64>,
}

#[derive(Debug, Clone)]
pub(crate) struct SharedStats(pub(crate) Arc<Mutex<NfsSessionStats>>);

impl Default for SharedStats {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(NfsSessionStats::default())))
    }
}

/// State that an NFS server shares between its v3 and v4 protocol sessions.
///
/// Standalone versioned sessions remain independently constructible for codec
/// and protocol tests, while [`NfsServer`](crate::NfsServer) uses one table,
/// path lock, and counter set for both versions.
#[derive(Clone)]
pub(crate) struct SharedNfsState {
    pub(crate) handles: FileHandleTable,
    pub(crate) stats: SharedStats,
    pub(crate) path_lock: Arc<tokio::sync::RwLock<()>>,
}

impl SharedNfsState {
    pub(crate) fn new(options: &NfsSessionOptions) -> Self {
        Self {
            handles: FileHandleTable::new(FileHandleTableOptions {
                use_driver_ino: options.use_driver_ino,
                verifier: options.verifier,
                max_handles: options.max_handles,
            }),
            stats: SharedStats::default(),
            path_lock: Arc::new(tokio::sync::RwLock::new(())),
        }
    }
}

/// Route one unframed RPC call across the server's shared NFSv3/MOUNTv3 and
/// NFSv4.1 sessions. An unsupported NFS program version advertises the full
/// 3..4 range; standalone versioned sessions retain their own narrower range.
pub async fn route_nfs_call(
    v3: &Nfs3Session,
    v4: &Nfs4Session,
    message: &[u8],
    context: NfsRequestContext,
) -> Option<Vec<u8>> {
    let peek = message.get(12..20).map(|bytes| {
        let program = u32::from_be_bytes(bytes[..4].try_into().expect("RPC program bytes"));
        let version = u32::from_be_bytes(bytes[4..].try_into().expect("RPC version bytes"));
        (program, version)
    });
    match peek {
        Some((MOUNT_PROGRAM, _)) | Some((NFS_PROGRAM, NFS_V3)) | None => {
            v3.handle_call(message, context).await
        }
        Some((NFS_PROGRAM, NFS_V4)) => v4.handle_call(message, context).await,
        _ => {
            {
                let mut stats = v3.stats.0.lock().expect("NFS stats lock");
                stats.requests = stats.requests.saturating_add(1);
            }
            let call = match decode_call(message) {
                Ok((call, _)) => call,
                Err(_) => {
                    let mut stats = v3.stats.0.lock().expect("NFS stats lock");
                    stats.dropped = stats.dropped.saturating_add(1);
                    return None;
                }
            };
            let reply = if call.rpc_version != RPC_VERSION {
                encode_rpc_mismatch(call.xid, RPC_VERSION, RPC_VERSION)
            } else if call.cred.flavor != AUTH_NONE && call.cred.flavor != AUTH_SYS {
                encode_auth_error(call.xid, AUTH_TOOWEAK)
            } else if call.program != NFS_PROGRAM {
                encode_accept_error(call.xid, RPC_PROG_UNAVAIL, None)
            } else {
                encode_accept_error(call.xid, RPC_PROG_MISMATCH, Some((NFS_V3, NFS_V4)))
            };
            let mut stats = v3.stats.0.lock().expect("NFS stats lock");
            stats.replies = stats.replies.saturating_add(1);
            Some(reply)
        }
    }
}

#[derive(Debug, Clone)]
struct MountRecord {
    hostname: String,
    directory: String,
}

#[derive(Debug)]
struct ExclusiveCreate {
    verifier: Vec<u8>,
    recorded: Instant,
}

/// Retransmission verifiers are deliberately bounded and short-lived.
///
/// NFSv3's exclusive-create verifier is an idempotence aid for a lost reply,
/// not durable filesystem metadata. Keeping it process-local matches the
/// upstream transport; the limit and expiry prevent a client creating files
/// in a loop from turning it into an unbounded cache.
#[derive(Debug, Default)]
struct ExclusiveCreates {
    entries: HashMap<String, ExclusiveCreate>,
    order: VecDeque<String>,
}

impl ExclusiveCreates {
    fn set(&mut self, path: String, verifier: Vec<u8>) {
        self.forget(&path);
        self.entries.insert(
            path.clone(),
            ExclusiveCreate {
                verifier,
                recorded: Instant::now(),
            },
        );
        self.order.push_back(path);
        while self.order.len() > DEFAULT_EXCLUSIVE_CREATES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn matches(&mut self, path: &str, verifier: &[u8]) -> bool {
        let expired = self
            .entries
            .get(path)
            .is_some_and(|entry| entry.recorded.elapsed() >= EXCLUSIVE_CREATE_WINDOW);
        if expired {
            self.forget(path);
            return false;
        }
        self.entries
            .get(path)
            .is_some_and(|entry| entry.verifier == verifier)
    }

    fn forget(&mut self, path: &str) {
        let prefix = format!("{path}/");
        self.entries
            .retain(|candidate, _| candidate != path && !candidate.starts_with(&prefix));
        self.order
            .retain(|candidate| self.entries.contains_key(candidate));
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }
}

#[derive(Debug)]
enum DispatchError {
    Xdr(XdrError),
    ProcedureUnavailable,
}

impl From<XdrError> for DispatchError {
    fn from(error: XdrError) -> Self {
        Self::Xdr(error)
    }
}

#[derive(Clone)]
pub struct Nfs3Session {
    pub driver: Loopback,
    pub options: NfsSessionOptions,
    pub handles: FileHandleTable,
    pub write_verifier: [u8; 8],
    snapshots: DirectorySnapshots,
    mounts: Arc<Mutex<Vec<MountRecord>>>,
    exclusive_creates: Arc<Mutex<ExclusiveCreates>>,
    /// NFSv3 has no OPEN on the wire. Keep one backend read handle for every
    /// regular filehandle we expose so an unlink can detach the namespace name
    /// without destroying the object a client is still holding.
    retained_handles: Arc<Mutex<HashMap<u64, Arc<dyn FileHandle>>>>,
    stats: SharedStats,
    destroyed: Arc<Mutex<bool>>,
    path_lock: Arc<tokio::sync::RwLock<()>>,
    hooks: NfsSessionHooks,
}

impl Nfs3Session {
    pub fn new<D>(driver: D, options: NfsSessionOptions) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::from_loopback(Loopback::new(driver), options)
    }

    pub fn from_arc(driver: Arc<dyn FsDriver>, options: NfsSessionOptions) -> Self {
        Self::from_loopback(Loopback::from_arc(driver), options)
    }

    pub fn from_loopback(driver: Loopback, options: NfsSessionOptions) -> Self {
        let shared = SharedNfsState::new(&options);
        Self::from_loopback_shared(driver, options, &shared)
    }

    /// Construct a session with a request-level error hook.
    pub fn new_with_hooks<D>(driver: D, options: NfsSessionOptions, hooks: NfsSessionHooks) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::from_loopback_with_hooks(Loopback::new(driver), options, hooks)
    }

    /// Construct a loopback session with a request-level error hook.
    pub fn from_loopback_with_hooks(
        driver: Loopback,
        options: NfsSessionOptions,
        hooks: NfsSessionHooks,
    ) -> Self {
        let shared = SharedNfsState::new(&options);
        Self::from_loopback_shared_with_hooks(driver, options, &shared, hooks)
    }

    pub(crate) fn from_loopback_shared(
        driver: Loopback,
        options: NfsSessionOptions,
        shared: &SharedNfsState,
    ) -> Self {
        Self::from_loopback_shared_with_hooks(driver, options, shared, NfsSessionHooks::default())
    }

    pub(crate) fn from_loopback_shared_with_hooks(
        driver: Loopback,
        options: NfsSessionOptions,
        shared: &SharedNfsState,
        hooks: NfsSessionHooks,
    ) -> Self {
        let handles = shared.handles.clone();
        let write_verifier = handles.verifier();
        Self {
            driver,
            options: options.clone(),
            handles,
            write_verifier,
            snapshots: DirectorySnapshots::new(options.snapshot_cache),
            mounts: Arc::new(Mutex::new(Vec::new())),
            exclusive_creates: Arc::new(Mutex::new(ExclusiveCreates::default())),
            retained_handles: Arc::new(Mutex::new(HashMap::new())),
            stats: shared.stats.clone(),
            destroyed: Arc::new(Mutex::new(false)),
            path_lock: Arc::clone(&shared.path_lock),
            hooks,
        }
    }

    pub(crate) fn with_hooks(mut self, hooks: NfsSessionHooks) -> Self {
        self.hooks = hooks;
        self
    }

    pub(crate) fn shared_state(&self) -> SharedNfsState {
        SharedNfsState {
            handles: self.handles.clone(),
            stats: self.stats.clone(),
            path_lock: Arc::clone(&self.path_lock),
        }
    }

    pub fn stats(&self) -> NfsSessionStats {
        self.stats.0.lock().expect("NFS stats lock").clone()
    }

    pub fn mounts(&self) -> Vec<(String, String)> {
        self.mounts
            .lock()
            .expect("NFS mount lock")
            .iter()
            .map(|record| (record.hostname.clone(), record.directory.clone()))
            .collect()
    }

    pub fn destroyed(&self) -> bool {
        *self.destroyed.lock().expect("NFS destroyed lock")
    }

    pub async fn destroy(&self) {
        *self.destroyed.lock().expect("NFS destroyed lock") = true;
        let retained: Vec<_> = self
            .retained_handles
            .lock()
            .expect("NFS retained handle lock")
            .drain()
            .map(|(_, handle)| handle)
            .collect();
        for handle in retained {
            let _ = handle.close().await;
        }
        self.handles.clear();
        self.snapshots.clear();
        self.mounts.lock().expect("NFS mount lock").clear();
        self.exclusive_creates
            .lock()
            .expect("NFS exclusive-create lock")
            .clear();
    }

    /// Handle one complete, unframed RPC record. Malformed records that still
    /// carry no trustworthy XID return `None`; all decoded calls receive one
    /// legal RPC reply and the method does not propagate driver or codec errors.
    pub async fn handle_call(&self, message: &[u8], context: NfsRequestContext) -> Option<Vec<u8>> {
        {
            let mut stats = self.stats.0.lock().expect("NFS stats lock");
            stats.requests = stats.requests.saturating_add(1);
        }
        let (call, mut args) = match decode_call(message) {
            Ok(value) => value,
            Err(_) => {
                let mut stats = self.stats.0.lock().expect("NFS stats lock");
                stats.dropped = stats.dropped.saturating_add(1);
                return None;
            }
        };
        let reply = match self.dispatch(&call, &mut args, &context).await {
            Ok(reply) => reply,
            Err(DispatchError::Xdr(error)) => {
                self.record_stat_error();
                self.hooks.report(error.into(), Some(call.clone()));
                encode_accept_error(call.xid, RPC_GARBAGE_ARGS, None)
            }
            Err(DispatchError::ProcedureUnavailable) => {
                self.record_error();
                encode_accept_error(call.xid, RPC_PROC_UNAVAIL, None)
            }
        };
        let mut stats = self.stats.0.lock().expect("NFS stats lock");
        stats.replies = stats.replies.saturating_add(1);
        Some(reply)
    }

    async fn dispatch(
        &self,
        call: &RpcCall,
        args: &mut XdrReader<'_>,
        context: &NfsRequestContext,
    ) -> Result<Vec<u8>, DispatchError> {
        if call.rpc_version != RPC_VERSION {
            return Ok(encode_rpc_mismatch(call.xid, RPC_VERSION, RPC_VERSION));
        }
        if call.cred.flavor != AUTH_NONE && call.cred.flavor != AUTH_SYS {
            return Ok(encode_auth_error(call.xid, AUTH_TOOWEAK));
        }
        if call.program != NFS_PROGRAM && call.program != MOUNT_PROGRAM {
            return Ok(encode_accept_error(call.xid, RPC_PROG_UNAVAIL, None));
        }
        let expected_version = if call.program == NFS_PROGRAM {
            NFS_V3
        } else {
            MOUNT_V3
        };
        if call.version != expected_version {
            return Ok(encode_accept_error(
                call.xid,
                RPC_PROG_MISMATCH,
                Some((expected_version, expected_version)),
            ));
        }
        let name = format!(
            "{}:{}",
            if call.program == NFS_PROGRAM {
                "NFS"
            } else {
                "MOUNT"
            },
            procedure_name(call.program, call.procedure)
        );
        self.stats
            .0
            .lock()
            .expect("NFS stats lock")
            .procedures
            .entry(name)
            .and_modify(|count| *count = count.saturating_add(1))
            .or_insert(1);
        let credentials = credentials_of(&call.cred);
        let mut writer = XdrWriter::with_capacity(256);
        write_accepted_reply_header(&mut writer, call.xid);
        match (call.program, call.procedure) {
            (NFS_PROGRAM, NFSPROC3_READ | NFSPROC3_WRITE) => {
                // Reads and writes can block in a backend; do not hold the
                // path-map lock across either operation.
                self.nfs(call.procedure, args, &credentials, &mut writer)
                    .await?;
            }
            (NFS_PROGRAM, NFSPROC3_RENAME) => {
                let _guard = self.path_lock.write().await;
                self.nfs(call.procedure, args, &credentials, &mut writer)
                    .await?;
            }
            _ => {
                let _guard = self.path_lock.read().await;
                if call.program == NFS_PROGRAM {
                    self.nfs(call.procedure, args, &credentials, &mut writer)
                        .await?;
                } else {
                    self.mount(call.procedure, args, context, &mut writer)
                        .await?;
                }
            }
        }
        Ok(writer.into_bytes())
    }

    async fn nfs(
        &self,
        procedure: u32,
        args: &mut XdrReader<'_>,
        credentials: &RpcCredentials,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        match procedure {
            NFSPROC3_NULL => {
                args.end("NULL arguments")?;
            }
            NFSPROC3_GETATTR => self.getattr(args, writer).await?,
            NFSPROC3_SETATTR => self.setattr(args, writer).await?,
            NFSPROC3_LOOKUP => self.lookup(args, writer).await?,
            NFSPROC3_ACCESS => self.access(args, credentials, writer).await?,
            NFSPROC3_READLINK => self.readlink(args, writer).await?,
            NFSPROC3_READ => self.read_file(args, writer).await?,
            NFSPROC3_WRITE => self.write_file(args, writer).await?,
            NFSPROC3_CREATE => self.create(args, credentials, writer).await?,
            NFSPROC3_MKDIR => self.mkdir(args, credentials, writer).await?,
            NFSPROC3_SYMLINK => self.symlink(args, credentials, writer).await?,
            NFSPROC3_MKNOD => self.mknod(args, credentials, writer).await?,
            NFSPROC3_REMOVE => self.remove(args, false, writer).await?,
            NFSPROC3_RMDIR => self.remove(args, true, writer).await?,
            NFSPROC3_RENAME => self.rename(args, writer).await?,
            NFSPROC3_LINK => self.link(args, writer).await?,
            NFSPROC3_READDIR => self.readdir(args, writer, false).await?,
            NFSPROC3_READDIRPLUS => self.readdir(args, writer, true).await?,
            NFSPROC3_FSSTAT => self.fsstat(args, writer).await?,
            NFSPROC3_FSINFO => self.fsinfo(args, writer).await?,
            NFSPROC3_PATHCONF => self.pathconf(args, writer).await?,
            NFSPROC3_COMMIT => self.commit(args, writer).await?,
            _ => return Err(DispatchError::ProcedureUnavailable),
        }
        Ok(())
    }

    async fn stat_of(&self, path: &str) -> FsResult<mount_rs_core::Stats> {
        match self.driver.lstat(path).await {
            Ok(stats) => Ok(stats),
            Err(error) if error.code == ErrorCode::Enosys => self.driver.stat(path).await,
            Err(error) => Err(error),
        }
    }

    async fn attr_of(&self, path: &str) -> FsResult<(HandleEntry, Fattr3, mount_rs_core::Stats)> {
        let stats = self.stat_of(path).await?;
        let entry = self.handles.bind(path, &stats);
        if stats.is_file() {
            // Best effort: a lookup must still be able to report attributes for
            // a read-only/special backend that cannot create a second handle.
            // The normal regular-file path can retain a descriptor, which is
            // what makes NFSv3's synthetic client-side OPEN survive unlink.
            let _ = self.ensure_retained_handle(&entry, path).await;
        }
        let attr = fattr_of(&stats, entry.fileid);
        Ok((entry, attr, stats))
    }

    fn retained_handle(&self, id: u64) -> Option<Arc<dyn FileHandle>> {
        self.retained_handles
            .lock()
            .expect("NFS retained handle lock")
            .get(&id)
            .cloned()
    }

    async fn ensure_retained_handle(
        &self,
        entry: &HandleEntry,
        path: &str,
    ) -> FsResult<Arc<dyn FileHandle>> {
        if let Some(handle) = self.retained_handle(entry.id) {
            return Ok(handle);
        }
        let handle = self
            .driver
            .open_flags(path, OpenFlags::READ_ONLY, 0)
            .await?;
        let existing = {
            let mut retained = self
                .retained_handles
                .lock()
                .expect("NFS retained handle lock");
            if let Some(existing) = retained.get(&entry.id).cloned() {
                Some(existing)
            } else {
                retained.insert(entry.id, handle.clone());
                None
            }
        };
        if let Some(existing) = existing {
            let _ = handle.close().await;
            return Ok(existing);
        }
        Ok(handle)
    }

    async fn post_op(&self, path: Option<&str>) -> Option<Fattr3> {
        let path = path?;
        self.attr_of(path).await.ok().map(|(_, attr, _)| attr)
    }

    async fn pre_op(&self, path: &str) -> Option<WccAttr> {
        self.stat_of(path)
            .await
            .ok()
            .map(|stats| wcc_attr_of(&stats))
    }

    async fn wcc(&self, before: Option<WccAttr>, path: &str) -> WccData {
        WccData {
            before,
            after: self.post_op(Some(path)).await,
        }
    }

    fn path_of(&self, handle: &[u8]) -> FsResult<String> {
        self.handles.resolve(handle)
    }

    fn join_path(directory: &str, name: &str) -> String {
        mount_rs_core::path::normalize_path(&format!("{directory}/{name}"))
    }

    fn check_name(name: &str) -> FsResult<()> {
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message(format!("EINVAL: invalid NFS directory entry name {name:?}")));
        }
        if name.len() > NAME_MAX {
            return Err(FsError::new(ErrorCode::Enametoolong));
        }
        Ok(())
    }

    fn offset(value: u64, syscall: &str) -> FsResult<u64> {
        if value > MAX_OFFSET {
            Err(FsError::new(ErrorCode::Einval).with_syscall(syscall))
        } else {
            Ok(value)
        }
    }

    fn status(error: &FsError) -> u32 {
        nfs_status_of(error)
    }

    fn record_stat_error(&self) {
        let mut stats = self.stats.0.lock().expect("NFS stats lock");
        stats.errors = stats.errors.saturating_add(1);
    }

    fn record_error(&self) {
        self.record_stat_error();
        self.hooks.report(
            NfsSessionError::generic("NFS request returned an error"),
            None,
        );
    }

    fn invalidate(&self, path: &str) {
        if let Some(entry) = self.handles.at(path) {
            self.snapshots.invalidate(entry.id);
        }
    }

    async fn getattr(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let handle = args.var_opaque(NFS3_FHSIZE, "nfs_fh3")?;
        args.end("GETATTR arguments")?;
        match self.path_of(&handle) {
            Ok(path) => match self.attr_of(&path).await {
                Ok((_, attr, _)) => write_getattr_res(
                    writer,
                    &Getattr3res {
                        status: NFS3_OK,
                        attributes: Some(attr),
                    },
                ),
                Err(error) => {
                    self.record_error();
                    write_getattr_res(
                        writer,
                        &Getattr3res {
                            status: Self::status(&error),
                            attributes: None,
                        },
                    );
                }
            },
            Err(error) => {
                // A regular file can be unlinked while a client still holds
                // its NFSv3 filehandle.  There is no OPEN state on the wire,
                // so use the retained backend descriptor rather than turning
                // the otherwise valid handle into ESTALE.
                let orphan = self
                    .handles
                    .decode(&handle)
                    .ok()
                    .and_then(|entry| self.retained_handle(entry.id));
                if let Some(orphan) = orphan {
                    match orphan.stat().await {
                        Ok(stats) => {
                            let entry = self
                                .handles
                                .decode(&handle)
                                .expect("retained handle was decoded above");
                            write_getattr_res(
                                writer,
                                &Getattr3res {
                                    status: NFS3_OK,
                                    attributes: Some(fattr_of(&stats, entry.fileid)),
                                },
                            );
                        }
                        Err(error) => {
                            self.record_error();
                            write_getattr_res(
                                writer,
                                &Getattr3res {
                                    status: Self::status(&error),
                                    attributes: None,
                                },
                            );
                        }
                    }
                } else {
                    self.record_error();
                    write_getattr_res(
                        writer,
                        &Getattr3res {
                            status: Self::status(&error),
                            attributes: None,
                        },
                    );
                }
            }
        }
        Ok(())
    }

    async fn setattr(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_setattr_args(args)?;
        args.end("SETATTR arguments")?;
        let mut path = None;
        let mut before = None;
        let mut forced_status = None;
        let result = async {
            let resolved = self.path_of(&request.object)?;
            let current = self.stat_of(&resolved).await?;
            path = Some(resolved.clone());
            before = Some(wcc_attr_of(&current));
            if let Some(guard) = request.guard
                && guard.seconds != (current.ctime_ms.max(0) / 1000) as u32
            {
                forced_status = Some(NFS3ERR_NOT_SYNC);
                return Err(FsError::new(ErrorCode::Eio));
            }
            self.apply_sattr(&resolved, &request.attributes, &current)
                .await
        }
        .await;
        match result {
            Ok(()) => {
                if let Some(path) = path.as_deref() {
                    self.exclusive_creates
                        .lock()
                        .expect("NFS exclusive-create lock")
                        .forget(path);
                    write_wcc_res(
                        writer,
                        &WccRes {
                            status: NFS3_OK,
                            wcc: self.wcc(before, path).await,
                        },
                    );
                }
            }
            Err(error) => {
                self.record_error();
                write_wcc_res(
                    writer,
                    &WccRes {
                        status: forced_status.unwrap_or_else(|| Self::status(&error)),
                        wcc: WccData {
                            before,
                            after: self.post_op(path.as_deref()).await,
                        },
                    },
                );
            }
        }
        Ok(())
    }

    async fn apply_sattr(
        &self,
        path: &str,
        attr: &Sattr3,
        current: &mount_rs_core::Stats,
    ) -> FsResult<()> {
        if let Some(mode) = attr.mode
            && current.mode & S_IFMT != mount_rs_core::types::S_IFLNK
        {
            self.driver.chmod(path, mode & 0o7777).await?;
        }
        if attr.uid.is_some() || attr.gid.is_some() {
            let uid = attr.uid.unwrap_or(u32::MAX);
            let gid = attr.gid.unwrap_or(u32::MAX);
            match self.driver.lchown(path, uid, gid).await {
                Ok(()) => {}
                Err(error) if error.code == ErrorCode::Enosys => {
                    self.driver.chown(path, uid, gid).await?;
                }
                Err(error) => return Err(error),
            }
        }
        if let Some(size) = attr.size {
            self.driver
                .truncate(path, Self::offset(size, "truncate")?)
                .await?;
        }
        if attr.atime.how == DONT_CHANGE && attr.mtime.how == DONT_CHANGE {
            return Ok(());
        }
        let now = now_ms();
        let current = if attr.size.is_some()
            && (attr.atime.how == DONT_CHANGE || attr.mtime.how == DONT_CHANGE)
        {
            self.stat_of(path).await?
        } else {
            current.clone()
        };
        let atime = match attr.atime.how {
            DONT_CHANGE => current.atime_ms,
            SET_TO_SERVER_TIME => now,
            SET_TO_CLIENT_TIME => from_time(attr.atime.time.unwrap_or(NfsTime3 {
                seconds: 0,
                nseconds: 0,
            })),
            _ => current.atime_ms,
        };
        let mtime = match attr.mtime.how {
            DONT_CHANGE => current.mtime_ms,
            SET_TO_SERVER_TIME => now,
            SET_TO_CLIENT_TIME => from_time(attr.mtime.time.unwrap_or(NfsTime3 {
                seconds: 0,
                nseconds: 0,
            })),
            _ => current.mtime_ms,
        };
        match self.driver.lutimes(path, atime, mtime).await {
            Ok(()) => {}
            Err(error) if error.code == ErrorCode::Enosys => {
                self.driver.utimes(path, atime, mtime).await?;
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    async fn lookup(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_dir_op(args)?;
        args.end("LOOKUP arguments")?;
        let directory = match self.path_of(&request.dir) {
            Ok(path) => path,
            Err(error) => {
                self.record_error();
                write_lookup_res(
                    writer,
                    &Lookup3res {
                        status: Self::status(&error),
                        object: None,
                        obj_attributes: None,
                        dir_attributes: None,
                    },
                );
                return Ok(());
            }
        };
        let path = if request.name == "." {
            directory.clone()
        } else if request.name == ".." {
            mount_rs_core::path::dirname(&directory)
        } else {
            if let Err(error) = Self::check_name(&request.name) {
                self.record_error();
                write_lookup_res(
                    writer,
                    &Lookup3res {
                        status: Self::status(&error),
                        object: None,
                        obj_attributes: None,
                        dir_attributes: self.post_op(Some(&directory)).await,
                    },
                );
                return Ok(());
            }
            Self::join_path(&directory, &request.name)
        };
        match self.attr_of(&path).await {
            Ok((entry, attr, _)) => write_lookup_res(
                writer,
                &Lookup3res {
                    status: NFS3_OK,
                    object: Some(self.handles.encode(&entry)),
                    obj_attributes: Some(attr),
                    dir_attributes: self.post_op(Some(&directory)).await,
                },
            ),
            Err(error) => {
                self.record_error();
                write_lookup_res(
                    writer,
                    &Lookup3res {
                        status: Self::status(&error),
                        object: None,
                        obj_attributes: None,
                        dir_attributes: self.post_op(Some(&directory)).await,
                    },
                );
            }
        }
        Ok(())
    }

    async fn access(
        &self,
        args: &mut XdrReader<'_>,
        credentials: &RpcCredentials,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_access_args(args)?;
        args.end("ACCESS arguments")?;
        match self.path_of(&request.object) {
            Ok(path) => match self.attr_of(&path).await {
                Ok((_, attr, stats)) => {
                    let rights = allowed_access(&stats, credentials);
                    write_access_res(
                        writer,
                        &Access3res {
                            status: NFS3_OK,
                            attributes: Some(attr),
                            access: request.access & access_bits3(rights),
                        },
                    );
                }
                Err(error) => {
                    self.record_error();
                    write_access_res(
                        writer,
                        &Access3res {
                            status: Self::status(&error),
                            attributes: None,
                            access: 0,
                        },
                    );
                }
            },
            Err(error) => {
                self.record_error();
                write_access_res(
                    writer,
                    &Access3res {
                        status: Self::status(&error),
                        attributes: None,
                        access: 0,
                    },
                );
            }
        }
        Ok(())
    }

    async fn readlink(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let handle = args.var_opaque(NFS3_FHSIZE, "nfs_fh3")?;
        args.end("READLINK arguments")?;
        let path = self.path_of(&handle).ok();
        match path.as_deref() {
            Some(path) => match self.driver.readlink(path).await {
                Ok(target) => write_readlink_res(
                    writer,
                    &Readlink3res {
                        status: NFS3_OK,
                        attributes: self.post_op(Some(path)).await,
                        target: Some(target),
                    },
                ),
                Err(error) => {
                    self.record_error();
                    write_readlink_res(
                        writer,
                        &Readlink3res {
                            status: Self::status(&error),
                            attributes: self.post_op(Some(path)).await,
                            target: None,
                        },
                    );
                }
            },
            None => {
                let error = FsError::new(ErrorCode::Estale);
                self.record_error();
                write_readlink_res(
                    writer,
                    &Readlink3res {
                        status: Self::status(&error),
                        attributes: None,
                        target: None,
                    },
                );
            }
        }
        Ok(())
    }

    async fn read_file(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_read_args(args)?;
        args.end("READ arguments")?;
        let entry = match self.handles.decode(&request.file) {
            Ok(entry) => entry,
            Err(error) => {
                self.record_error();
                write_read_res(
                    writer,
                    &Read3res {
                        status: Self::status(&error),
                        attributes: None,
                        count: 0,
                        eof: false,
                        data: Vec::new(),
                    },
                );
                return Ok(());
            }
        };
        let path = self.handles.path_of(&entry).ok();
        let handle = match path.as_deref() {
            Some(path) => self.ensure_retained_handle(&entry, path).await,
            None => self.retained_handle(entry.id).ok_or_else(|| {
                FsError::new(ErrorCode::Estale)
                    .with_syscall("read")
                    .with_message("ESTALE: unlinked file has no retained backend handle")
            }),
        };
        let handle = match handle {
            Ok(handle) => handle,
            Err(error) => {
                self.record_error();
                write_read_res(
                    writer,
                    &Read3res {
                        status: Self::status(&error),
                        attributes: None,
                        count: 0,
                        eof: false,
                        data: Vec::new(),
                    },
                );
                return Ok(());
            }
        };
        let count = (request.count as usize).min(self.options.rtmax);
        let offset = match Self::offset(request.offset, "read") {
            Ok(offset) => offset,
            Err(error) => {
                self.record_error();
                let attributes = match path.as_deref() {
                    Some(path) => self.post_op(Some(path)).await,
                    None => None,
                };
                write_read_res(
                    writer,
                    &Read3res {
                        status: Self::status(&error),
                        attributes,
                        count: 0,
                        eof: false,
                        data: Vec::new(),
                    },
                );
                return Ok(());
            }
        };
        let result = async {
            let mut data = vec![0_u8; count];
            let read = handle.read(&mut data, Some(offset)).await;
            let read = read?;
            data.truncate(read);
            Ok::<(Vec<u8>, Option<mount_rs_core::Stats>), FsError>((data, handle.stat().await.ok()))
        }
        .await;
        match result {
            Ok((data, retained_stats)) => {
                let attributes = match path.as_deref() {
                    Some(path) => self.post_op(Some(path)).await,
                    None => retained_stats
                        .as_ref()
                        .map(|stats| fattr_of(stats, entry.fileid)),
                };
                let eof = attributes.as_ref().map_or(data.len() < count, |attr| {
                    request.offset.saturating_add(data.len() as u64) >= attr.size
                });
                write_read_res(
                    writer,
                    &Read3res {
                        status: NFS3_OK,
                        attributes,
                        count: data.len() as u32,
                        eof,
                        data,
                    },
                );
            }
            Err(error) => {
                self.record_error();
                let attributes = match path.as_deref() {
                    Some(path) => self.post_op(Some(path)).await,
                    None => None,
                };
                write_read_res(
                    writer,
                    &Read3res {
                        status: Self::status(&error),
                        attributes,
                        count: 0,
                        eof: false,
                        data: Vec::new(),
                    },
                );
            }
        }
        Ok(())
    }

    async fn write_file(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_write_args(args, self.options.wtmax)?;
        args.end("WRITE arguments")?;
        let path = self.path_of(&request.file).ok();
        let Some(path) = path else {
            let error = FsError::new(ErrorCode::Estale);
            self.record_error();
            write_write_res(
                writer,
                &Write3res {
                    status: Self::status(&error),
                    wcc: WccData {
                        before: None,
                        after: None,
                    },
                    count: 0,
                    committed: FILE_SYNC,
                    verf: self.write_verifier.to_vec(),
                },
            );
            return Ok(());
        };
        let offset = match Self::offset(request.offset, "write") {
            Ok(offset) => offset,
            Err(error) => {
                self.record_error();
                write_write_res(
                    writer,
                    &Write3res {
                        status: Self::status(&error),
                        wcc: WccData {
                            before: None,
                            after: self.post_op(Some(&path)).await,
                        },
                        count: 0,
                        committed: FILE_SYNC,
                        verf: self.write_verifier.to_vec(),
                    },
                );
                return Ok(());
            }
        };
        let before = self.pre_op(&path).await;
        let result = async {
            let flags = OpenFlags {
                read: false,
                write: true,
                create: false,
                truncate: false,
                append: false,
                exclusive: false,
            };
            let handle = self.driver.open_flags(&path, flags, 0).await?;
            let written = handle.write(&request.data, Some(offset)).await;
            let result = match written {
                Ok(written) => handle.sync().await.map(|()| written),
                Err(error) => Err(error),
            };
            let _ = handle.close().await;
            let written = result?;
            Ok::<usize, FsError>(written)
        }
        .await;
        match result {
            Ok(written) => write_write_res(
                writer,
                &Write3res {
                    status: NFS3_OK,
                    wcc: self.wcc(before, &path).await,
                    count: written as u32,
                    committed: FILE_SYNC,
                    verf: self.write_verifier.to_vec(),
                },
            ),
            Err(error) => {
                self.record_error();
                write_write_res(
                    writer,
                    &Write3res {
                        status: Self::status(&error),
                        wcc: WccData {
                            before,
                            after: self.post_op(Some(&path)).await,
                        },
                        count: 0,
                        committed: FILE_SYNC,
                        verf: self.write_verifier.to_vec(),
                    },
                );
            }
        }
        Ok(())
    }

    async fn commit(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_commit_args(args)?;
        args.end("COMMIT arguments")?;
        let path = self.path_of(&request.file).ok();
        let Some(path) = path else {
            let error = FsError::new(ErrorCode::Estale);
            self.record_error();
            write_commit_res(
                writer,
                &Commit3res {
                    status: Self::status(&error),
                    wcc: WccData {
                        before: None,
                        after: None,
                    },
                    verf: self.write_verifier.to_vec(),
                },
            );
            return Ok(());
        };
        let before = self.pre_op(&path).await;
        // Every WRITE is acknowledged FILE_SYNC, so there is no deferred
        // writeback for COMMIT to flush. The handle/path check above still
        // gives a stale handle the correct NFS error and WCC shape.
        write_commit_res(
            writer,
            &Commit3res {
                status: NFS3_OK,
                wcc: self.wcc(before, &path).await,
                verf: self.write_verifier.to_vec(),
            },
        );
        Ok(())
    }

    async fn created_response(
        &self,
        writer: &mut XdrWriter,
        directory: &str,
        before: Option<WccAttr>,
        path: &str,
    ) {
        match self.attr_of(path).await {
            Ok((entry, attr, _)) => write_create_res(
                writer,
                &CreateRes {
                    status: NFS3_OK,
                    obj: Some(self.handles.encode(&entry)),
                    obj_attributes: Some(attr),
                    dir_wcc: self.wcc(before, directory).await,
                },
            ),
            Err(error) => {
                self.record_error();
                write_create_res(
                    writer,
                    &CreateRes {
                        status: Self::status(&error),
                        obj: None,
                        obj_attributes: None,
                        dir_wcc: WccData {
                            before,
                            after: self.post_op(Some(directory)).await,
                        },
                    },
                );
            }
        }
    }

    async fn claim_owner(
        &self,
        path: &str,
        credentials: &RpcCredentials,
        parent: Option<&mount_rs_core::Stats>,
        directory: bool,
        mode: u32,
    ) -> FsResult<()> {
        if !self.options.claim_ownership {
            return Ok(());
        }

        let uid = credentials.uid.unwrap_or(u32::MAX);
        let parent_setgid = parent.is_some_and(|stats| stats.mode & S_ISGID != 0);
        let gid = if parent_setgid {
            parent.map_or(u32::MAX, |stats| stats.gid)
        } else {
            credentials.gid.unwrap_or(u32::MAX)
        };

        // AUTH_NONE uses the chown convention of u32::MAX for "leave alone".
        // A set-group-ID parent can still contribute its group in that case.
        if uid != u32::MAX || gid != u32::MAX {
            match self.driver.lchown(path, uid, gid).await {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.code,
                        ErrorCode::Enosys | ErrorCode::Eperm | ErrorCode::Enotsup
                    ) => {}
                Err(error) => return Err(error),
            }
        }

        let setgid = if !parent_setgid {
            None
        } else if directory {
            Some(true)
        } else {
            let setgid_executable = mode & (S_ISGID | S_IXGRP) == (S_ISGID | S_IXGRP);
            let member = credentials.gid == Some(gid) || credentials.gids.contains(&gid);
            if setgid_executable && !member && uid != 0 {
                Some(false)
            } else {
                None
            }
        };

        if let Some(setgid) = setgid {
            // Read back the mode made by the create. Reasserting the requested
            // mode here would bypass the driver's umask; only change the bit
            // that the set-group-ID parent contributes.
            let current = self.stat_of(path).await?;
            let current_mode = current.mode & 0o7777;
            let wanted = if setgid {
                current_mode | S_ISGID
            } else {
                current_mode & !S_ISGID
            };
            if wanted != current_mode {
                match self.driver.chmod(path, wanted).await {
                    Ok(()) => {}
                    Err(error)
                        if matches!(
                            error.code,
                            ErrorCode::Enosys | ErrorCode::Eperm | ErrorCode::Enotsup
                        ) => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    async fn create(
        &self,
        args: &mut XdrReader<'_>,
        credentials: &RpcCredentials,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_create_args(args)?;
        args.end("CREATE arguments")?;
        let mut directory = None;
        let mut before = None;
        let result = async {
            let dir = self.path_of(&request.where_.dir)?;
            Self::check_name(&request.where_.name)?;
            let path = Self::join_path(&dir, &request.where_.name);
            directory = Some(dir.clone());
            let parent = self.stat_of(&dir).await.ok();
            before = parent.as_ref().map(wcc_attr_of);
            let mode = request
                .attributes
                .as_ref()
                .and_then(|attributes| attributes.mode)
                .unwrap_or(0o666)
                & 0o7777;
            let create_flags = OpenFlags {
                read: false,
                write: true,
                create: true,
                truncate: false,
                append: false,
                exclusive: true,
            };
            let newly_created = match self.driver.open_flags(&path, create_flags, mode).await {
                Ok(handle) => {
                    if request.mode == CREATE_EXCLUSIVE {
                        let verifier = request
                            .verf
                            .clone()
                            .unwrap_or_else(|| vec![0; NFS3_CREATEVERFSIZE]);
                        self.exclusive_creates
                            .lock()
                            .expect("NFS exclusive-create lock")
                            .set(path.clone(), verifier);
                    }
                    handle.close().await?;
                    true
                }
                Err(error) if error.code == ErrorCode::Eexist => false,
                Err(error) => return Err(error),
            };
            if newly_created {
                self.invalidate(&dir);
                self.claim_owner(&path, credentials, parent.as_ref(), false, mode)
                    .await?;
                if request.mode != CREATE_EXCLUSIVE
                    && let Some(attributes) = request.attributes.as_ref()
                {
                    let current = self.stat_of(&path).await?;
                    self.apply_sattr(&path, attributes, &current).await?;
                }
                return Ok(path);
            }
            match request.mode {
                CREATE_GUARDED => Err(FsError::new(ErrorCode::Eexist)),
                CREATE_EXCLUSIVE => {
                    let verifier = request.verf.as_deref().unwrap_or(&[0; NFS3_CREATEVERFSIZE]);
                    let matches = self
                        .exclusive_creates
                        .lock()
                        .expect("NFS exclusive-create lock")
                        .matches(&path, verifier);
                    if matches {
                        Ok(path)
                    } else {
                        Err(FsError::new(ErrorCode::Eexist))
                    }
                }
                _ => {
                    let flags = OpenFlags {
                        read: false,
                        write: true,
                        create: true,
                        truncate: false,
                        append: false,
                        exclusive: false,
                    };
                    let handle = self.driver.open_flags(&path, flags, mode).await?;
                    handle.close().await?;
                    if let Some(attributes) = request.attributes.as_ref() {
                        let current = self.stat_of(&path).await?;
                        let mut attributes = attributes.clone();
                        attributes.mode = None;
                        self.apply_sattr(&path, &attributes, &current).await?;
                    }
                    Ok(path)
                }
            }
        }
        .await;
        match result {
            Ok(path) => {
                self.created_response(writer, directory.as_deref().unwrap_or("/"), before, &path)
                    .await;
            }
            Err(error) => {
                self.record_error();
                write_create_res(
                    writer,
                    &CreateRes {
                        status: Self::status(&error),
                        obj: None,
                        obj_attributes: None,
                        dir_wcc: WccData {
                            before,
                            after: self.post_op(directory.as_deref()).await,
                        },
                    },
                );
            }
        }
        Ok(())
    }

    async fn mkdir(
        &self,
        args: &mut XdrReader<'_>,
        credentials: &RpcCredentials,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_mkdir_args(args)?;
        args.end("MKDIR arguments")?;
        let mut directory = None;
        let mut before = None;
        let result = async {
            let dir = self.path_of(&request.where_.dir)?;
            Self::check_name(&request.where_.name)?;
            let path = Self::join_path(&dir, &request.where_.name);
            directory = Some(dir.clone());
            let parent = self.stat_of(&dir).await.ok();
            before = parent.as_ref().map(wcc_attr_of);
            let mode = request.attributes.mode.unwrap_or(0o777) & 0o7777;
            self.driver
                .mkdir(
                    &path,
                    MkdirOptions {
                        recursive: false,
                        mode: Some(mode),
                    },
                )
                .await?;
            self.invalidate(&dir);
            self.claim_owner(&path, credentials, parent.as_ref(), true, mode)
                .await?;
            let current = self.stat_of(&path).await?;
            let mut attributes = request.attributes.clone();
            attributes.mode = None;
            self.apply_sattr(&path, &attributes, &current).await?;
            Ok::<String, FsError>(path)
        }
        .await;
        match result {
            Ok(path) => {
                self.created_response(writer, directory.as_deref().unwrap_or("/"), before, &path)
                    .await
            }
            Err(error) => {
                self.record_error();
                write_create_res(
                    writer,
                    &CreateRes {
                        status: Self::status(&error),
                        obj: None,
                        obj_attributes: None,
                        dir_wcc: WccData {
                            before,
                            after: self.post_op(directory.as_deref()).await,
                        },
                    },
                );
            }
        }
        Ok(())
    }

    async fn symlink(
        &self,
        args: &mut XdrReader<'_>,
        credentials: &RpcCredentials,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_symlink_args(args)?;
        args.end("SYMLINK arguments")?;
        let mut directory = None;
        let mut before = None;
        let result = async {
            let dir = self.path_of(&request.where_.dir)?;
            Self::check_name(&request.where_.name)?;
            let path = Self::join_path(&dir, &request.where_.name);
            directory = Some(dir.clone());
            let parent = self.stat_of(&dir).await.ok();
            before = parent.as_ref().map(wcc_attr_of);
            self.driver.symlink(&request.target, &path).await?;
            self.invalidate(&dir);
            self.claim_owner(&path, credentials, parent.as_ref(), false, 0o777)
                .await?;
            // Symlink mode is fixed by POSIX; apply only timestamps/ownership.
            let current = self.stat_of(&path).await?;
            let mut attributes = request.attributes.clone();
            attributes.mode = None;
            attributes.size = None;
            self.apply_sattr(&path, &attributes, &current).await?;
            Ok::<String, FsError>(path)
        }
        .await;
        match result {
            Ok(path) => {
                self.created_response(writer, directory.as_deref().unwrap_or("/"), before, &path)
                    .await
            }
            Err(error) => {
                self.record_error();
                write_create_res(
                    writer,
                    &CreateRes {
                        status: Self::status(&error),
                        obj: None,
                        obj_attributes: None,
                        dir_wcc: WccData {
                            before,
                            after: self.post_op(directory.as_deref()).await,
                        },
                    },
                );
            }
        }
        Ok(())
    }

    async fn mknod(
        &self,
        args: &mut XdrReader<'_>,
        credentials: &RpcCredentials,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_mknod_args(args)?;
        args.end("MKNOD arguments")?;
        let mut directory = None;
        let mut before = None;
        let result = async {
            let dir = self.path_of(&request.where_.dir)?;
            Self::check_name(&request.where_.name)?;
            let path = Self::join_path(&dir, &request.where_.name);
            directory = Some(dir.clone());
            let parent = self.stat_of(&dir).await.ok();
            before = parent.as_ref().map(wcc_attr_of);
            let kind = match request.r#type {
                NF3BLK => S_IFBLK,
                NF3CHR => S_IFCHR,
                NF3SOCK => S_IFSOCK,
                NF3FIFO => S_IFIFO,
                _ => return Err(FsError::new(ErrorCode::Einval)),
            };
            let mode = request
                .attributes
                .as_ref()
                .and_then(|attributes| attributes.mode)
                .unwrap_or(0o666)
                & 0o7777;
            let dev = request
                .spec
                .map(|spec| (u64::from(spec.major) << 8) | u64::from(spec.minor))
                .unwrap_or(0);
            self.driver.mknod(&path, kind | mode, dev).await?;
            self.invalidate(&dir);
            self.claim_owner(&path, credentials, parent.as_ref(), false, mode)
                .await?;
            Ok::<String, FsError>(path)
        }
        .await;
        match result {
            Ok(path) => {
                self.created_response(writer, directory.as_deref().unwrap_or("/"), before, &path)
                    .await
            }
            Err(error) => {
                self.record_error();
                write_create_res(
                    writer,
                    &CreateRes {
                        status: Self::status(&error),
                        obj: None,
                        obj_attributes: None,
                        dir_wcc: WccData {
                            before,
                            after: self.post_op(directory.as_deref()).await,
                        },
                    },
                );
            }
        }
        Ok(())
    }

    async fn remove(
        &self,
        args: &mut XdrReader<'_>,
        directory: bool,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_dir_op(args)?;
        args.end(if directory {
            "RMDIR arguments"
        } else {
            "REMOVE arguments"
        })?;
        let mut dir = None;
        let mut before = None;
        let result = async {
            let parent = self.path_of(&request.dir)?;
            Self::check_name(&request.name)?;
            let path = Self::join_path(&parent, &request.name);
            dir = Some(parent.clone());
            before = self.pre_op(&parent).await;
            let target = if directory {
                None
            } else {
                self.stat_of(&path).await.ok()
            };
            if let Some(stats) = target.as_ref()
                && stats.is_file()
            {
                let entry = self.handles.bind(&path, stats);
                let _ = self.ensure_retained_handle(&entry, &path).await;
            }
            if directory {
                self.driver.rmdir(&path).await?;
            } else {
                self.driver.unlink(&path).await?;
            }
            if directory {
                self.handles.forget(&path);
            } else {
                self.handles.orphan(&path);
            }
            self.exclusive_creates
                .lock()
                .expect("NFS exclusive-create lock")
                .forget(&path);
            self.invalidate(&parent);
            Ok::<String, FsError>(parent)
        }
        .await;
        match result {
            Ok(parent) => write_wcc_res(
                writer,
                &WccRes {
                    status: NFS3_OK,
                    wcc: self.wcc(before, &parent).await,
                },
            ),
            Err(error) => {
                self.record_error();
                write_wcc_res(
                    writer,
                    &WccRes {
                        status: Self::status(&error),
                        wcc: WccData {
                            before,
                            after: self.post_op(dir.as_deref()).await,
                        },
                    },
                );
            }
        }
        Ok(())
    }

    async fn rename(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_rename_args(args)?;
        args.end("RENAME arguments")?;
        let mut from_dir = None;
        let mut to_dir = None;
        let mut from_before = None;
        let mut to_before = None;
        let result = async {
            let from_parent = self.path_of(&request.from.dir)?;
            let to_parent = self.path_of(&request.to.dir)?;
            Self::check_name(&request.from.name)?;
            Self::check_name(&request.to.name)?;
            let from = Self::join_path(&from_parent, &request.from.name);
            let to = Self::join_path(&to_parent, &request.to.name);
            from_dir = Some(from_parent.clone());
            to_dir = Some(to_parent.clone());
            from_before = self.pre_op(&from_parent).await;
            to_before = if from_parent == to_parent {
                from_before.clone()
            } else {
                self.pre_op(&to_parent).await
            };
            self.driver.rename(&from, &to).await?;
            self.handles.remap(&from, &to);
            self.exclusive_creates
                .lock()
                .expect("NFS exclusive-create lock")
                .forget(&from);
            self.exclusive_creates
                .lock()
                .expect("NFS exclusive-create lock")
                .forget(&to);
            self.invalidate(&from_parent);
            self.invalidate(&to_parent);
            Ok::<(), FsError>(())
        }
        .await;
        match result {
            Ok(()) => {
                let from = from_dir.as_deref().unwrap_or("/");
                let to = to_dir.as_deref().unwrap_or(from);
                write_rename_res(
                    writer,
                    &Rename3res {
                        status: NFS3_OK,
                        from_wcc: self.wcc(from_before, from).await,
                        to_wcc: self.wcc(to_before, to).await,
                    },
                );
            }
            Err(error) => {
                self.record_error();
                write_rename_res(
                    writer,
                    &Rename3res {
                        status: Self::status(&error),
                        from_wcc: WccData {
                            before: from_before,
                            after: self.post_op(from_dir.as_deref()).await,
                        },
                        to_wcc: WccData {
                            before: to_before,
                            after: self.post_op(to_dir.as_deref()).await,
                        },
                    },
                );
            }
        }
        Ok(())
    }

    async fn link(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let request = read_link_args(args)?;
        args.end("LINK arguments")?;
        let mut file = None;
        let mut directory = None;
        let mut before = None;
        let result = async {
            let source = self.path_of(&request.file)?;
            let parent = self.path_of(&request.link.dir)?;
            Self::check_name(&request.link.name)?;
            let path = Self::join_path(&parent, &request.link.name);
            file = Some(source.clone());
            directory = Some(parent.clone());
            before = self.pre_op(&parent).await;
            self.driver.link(&source, &path).await?;
            self.invalidate(&parent);
            self.attr_of(&path).await?;
            Ok::<(), FsError>(())
        }
        .await;
        match result {
            Ok(()) => write_link_res(
                writer,
                &Link3res {
                    status: NFS3_OK,
                    attributes: self.post_op(file.as_deref()).await,
                    linkdir_wcc: self.wcc(before, directory.as_deref().unwrap_or("/")).await,
                },
            ),
            Err(error) => {
                self.record_error();
                write_link_res(
                    writer,
                    &Link3res {
                        status: Self::status(&error),
                        attributes: self.post_op(file.as_deref()).await,
                        linkdir_wcc: WccData {
                            before,
                            after: self.post_op(directory.as_deref()).await,
                        },
                    },
                );
            }
        }
        Ok(())
    }

    async fn directory_page(
        &self,
        entry: &HandleEntry,
        path: &str,
        cookie: u64,
        cookieverf: &[u8],
    ) -> FsResult<(DirectorySnapshot, usize)> {
        if cookie == 0 {
            let entries = self.driver.readdir(path).await?;
            let names = entries.into_iter().map(|entry| entry.name).collect();
            return Ok((self.snapshots.set(entry.id, names), 0));
        }
        let snapshot = match self.snapshots.get(entry.id) {
            Some(snapshot) if same_verifier(&snapshot.verifier, cookieverf) => snapshot,
            _ => {
                let entries = self.driver.readdir(path).await?;
                let names: Vec<String> = entries.into_iter().map(|entry| entry.name).collect();
                if !same_verifier(&cookie_verifier(&names), cookieverf) {
                    return Err(FsError::new(ErrorCode::Einval)
                        .with_message("NFS3ERR_BAD_COOKIE: directory verifier mismatch"));
                }
                self.snapshots.set(entry.id, names)
            }
        };
        let from = usize::try_from(cookie).map_err(|_| FsError::new(ErrorCode::Einval))?;
        if from > snapshot.names.len() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("NFS3ERR_BAD_COOKIE: cookie is past the end"));
        }
        Ok((snapshot, from))
    }

    async fn readdir(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
        plus: bool,
    ) -> Result<(), DispatchError> {
        if plus {
            let request = read_readdirplus_args(args)?;
            args.end("READDIRPLUS arguments")?;
            let path_result = self.path_of(&request.dir);
            let path = match path_result {
                Ok(path) => path,
                Err(error) => {
                    self.record_error();
                    write_readdirplus_res(
                        writer,
                        &Readdirplus3res {
                            status: Self::status(&error),
                            dir_attributes: None,
                            cookieverf: vec![0; NFS3_COOKIEVERFSIZE],
                            entries: Vec::new(),
                            eof: false,
                        },
                    );
                    return Ok(());
                }
            };
            let entry = match self.handles.decode(&request.dir) {
                Ok(entry) => entry,
                Err(error) => {
                    self.record_error();
                    write_readdirplus_res(
                        writer,
                        &Readdirplus3res {
                            status: Self::status(&error),
                            dir_attributes: None,
                            cookieverf: vec![0; NFS3_COOKIEVERFSIZE],
                            entries: Vec::new(),
                            eof: false,
                        },
                    );
                    return Ok(());
                }
            };
            let result = self
                .directory_page(&entry, &path, request.cookie, &request.cookieverf)
                .await;
            match result {
                Ok((snapshot, from)) => {
                    let dir_budget = request.dircount.saturating_sub(128) as usize;
                    let max_budget = (request.maxcount as usize)
                        .min(self.options.rtmax)
                        .saturating_sub(128);
                    let mut chosen = Vec::new();
                    let mut dir_used = 0;
                    let mut max_used = 0;
                    let mut index = from;
                    while index < snapshot.names.len() {
                        let name = &snapshot.names[index];
                        let dir_size = entry_size(name.len());
                        let plus_size = entry_plus_size(name.len(), FH_SIZE, true);
                        if dir_used + dir_size > dir_budget || max_used + plus_size > max_budget {
                            break;
                        }
                        dir_used += dir_size;
                        max_used += plus_size;
                        chosen.push((index, name.clone()));
                        index += 1;
                    }
                    if chosen.is_empty() && index < snapshot.names.len() {
                        let error = FsError::new(ErrorCode::Einval);
                        self.record_error();
                        write_readdirplus_res(
                            writer,
                            &Readdirplus3res {
                                status: NFS3ERR_TOOSMALL,
                                dir_attributes: self.post_op(Some(&path)).await,
                                cookieverf: snapshot.verifier,
                                entries: Vec::new(),
                                eof: false,
                            },
                        );
                        let _ = error;
                        return Ok(());
                    }
                    let mut entries = Vec::with_capacity(chosen.len());
                    for (index, name) in chosen {
                        let child = Self::join_path(&path, &name);
                        let described = self.attr_of(&child).await.ok();
                        entries.push(EntryPlus3 {
                            fileid: described.as_ref().map_or(0, |(_, attr, _)| attr.fileid),
                            name,
                            cookie: (index + 1) as u64,
                            attributes: described.as_ref().map(|(_, attr, _)| attr.clone()),
                            handle: described
                                .as_ref()
                                .map(|(entry, _, _)| self.handles.encode(entry)),
                        });
                    }
                    write_readdirplus_res(
                        writer,
                        &Readdirplus3res {
                            status: NFS3_OK,
                            dir_attributes: self.post_op(Some(&path)).await,
                            cookieverf: snapshot.verifier,
                            entries,
                            eof: index >= snapshot.names.len(),
                        },
                    );
                }
                Err(error) => {
                    let status = if error.code == ErrorCode::Einval {
                        NFS3ERR_BAD_COOKIE
                    } else {
                        Self::status(&error)
                    };
                    self.record_error();
                    write_readdirplus_res(
                        writer,
                        &Readdirplus3res {
                            status,
                            dir_attributes: self.post_op(Some(&path)).await,
                            cookieverf: vec![0; NFS3_COOKIEVERFSIZE],
                            entries: Vec::new(),
                            eof: false,
                        },
                    );
                }
            }
        } else {
            let request = read_readdir_args(args)?;
            args.end("READDIR arguments")?;
            let path_result = self.path_of(&request.dir);
            let path = match path_result {
                Ok(path) => path,
                Err(error) => {
                    self.record_error();
                    write_readdir_res(
                        writer,
                        &Readdir3res {
                            status: Self::status(&error),
                            dir_attributes: None,
                            cookieverf: vec![0; NFS3_COOKIEVERFSIZE],
                            entries: Vec::new(),
                            eof: false,
                        },
                    );
                    return Ok(());
                }
            };
            let entry = match self.handles.decode(&request.dir) {
                Ok(entry) => entry,
                Err(error) => {
                    self.record_error();
                    write_readdir_res(
                        writer,
                        &Readdir3res {
                            status: Self::status(&error),
                            dir_attributes: None,
                            cookieverf: vec![0; NFS3_COOKIEVERFSIZE],
                            entries: Vec::new(),
                            eof: false,
                        },
                    );
                    return Ok(());
                }
            };
            match self
                .directory_page(&entry, &path, request.cookie, &request.cookieverf)
                .await
            {
                Ok((snapshot, from)) => {
                    let budget = (request.count as usize)
                        .min(self.options.rtmax)
                        .saturating_sub(128);
                    let mut chosen = Vec::new();
                    let mut used = 0;
                    let mut index = from;
                    while index < snapshot.names.len() {
                        let name = &snapshot.names[index];
                        let size = entry_size(name.len());
                        if used + size > budget {
                            break;
                        }
                        used += size;
                        chosen.push((index, name.clone()));
                        index += 1;
                    }
                    if chosen.is_empty() && index < snapshot.names.len() {
                        self.record_error();
                        write_readdir_res(
                            writer,
                            &Readdir3res {
                                status: NFS3ERR_TOOSMALL,
                                dir_attributes: self.post_op(Some(&path)).await,
                                cookieverf: snapshot.verifier,
                                entries: Vec::new(),
                                eof: false,
                            },
                        );
                        return Ok(());
                    }
                    let mut entries = Vec::with_capacity(chosen.len());
                    for (index, name) in chosen {
                        let child = Self::join_path(&path, &name);
                        let fileid = match self.handles.at(&child) {
                            Some(entry) => entry.fileid,
                            None => self
                                .stat_of(&child)
                                .await
                                .map(|stats| self.handles.bind(&child, &stats).fileid)
                                .unwrap_or(0),
                        };
                        entries.push(Entry3 {
                            fileid,
                            name,
                            cookie: (index + 1) as u64,
                        });
                    }
                    write_readdir_res(
                        writer,
                        &Readdir3res {
                            status: NFS3_OK,
                            dir_attributes: self.post_op(Some(&path)).await,
                            cookieverf: snapshot.verifier,
                            entries,
                            eof: index >= snapshot.names.len(),
                        },
                    );
                }
                Err(error) => {
                    let status = if error.code == ErrorCode::Einval {
                        NFS3ERR_BAD_COOKIE
                    } else {
                        Self::status(&error)
                    };
                    self.record_error();
                    write_readdir_res(
                        writer,
                        &Readdir3res {
                            status,
                            dir_attributes: self.post_op(Some(&path)).await,
                            cookieverf: vec![0; NFS3_COOKIEVERFSIZE],
                            entries: Vec::new(),
                            eof: false,
                        },
                    );
                }
            }
        }
        Ok(())
    }

    async fn fsstat(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let handle = args.var_opaque(NFS3_FHSIZE, "nfs_fh3")?;
        args.end("FSSTAT arguments")?;
        match self.path_of(&handle) {
            Ok(path) => match self.driver.statfs(&path).await {
                Ok(stats) => {
                    let block_size = stats.block_size.max(4096);
                    write_fsstat_res(
                        writer,
                        &Fsstat3res {
                            status: NFS3_OK,
                            attributes: self.post_op(Some(&path)).await,
                            tbytes: stats.blocks.saturating_mul(block_size),
                            fbytes: stats.blocks_free.saturating_mul(block_size),
                            abytes: stats.blocks_available.saturating_mul(block_size),
                            tfiles: stats.files,
                            ffiles: stats.files_free,
                            afiles: stats.files_free,
                            invarsec: 0,
                        },
                    );
                }
                Err(error) => {
                    self.record_error();
                    write_fsstat_res(
                        writer,
                        &Fsstat3res {
                            status: Self::status(&error),
                            attributes: self.post_op(Some(&path)).await,
                            tbytes: 0,
                            fbytes: 0,
                            abytes: 0,
                            tfiles: 0,
                            ffiles: 0,
                            afiles: 0,
                            invarsec: 0,
                        },
                    );
                }
            },
            Err(error) => {
                self.record_error();
                write_fsstat_res(
                    writer,
                    &Fsstat3res {
                        status: Self::status(&error),
                        attributes: None,
                        tbytes: 0,
                        fbytes: 0,
                        abytes: 0,
                        tfiles: 0,
                        ffiles: 0,
                        afiles: 0,
                        invarsec: 0,
                    },
                );
            }
        }
        Ok(())
    }

    async fn fsinfo(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let handle = args.var_opaque(NFS3_FHSIZE, "nfs_fh3")?;
        args.end("FSINFO arguments")?;
        match self.path_of(&handle) {
            Ok(path) => {
                let capabilities = self.driver.capabilities;
                write_fsinfo_res(
                    writer,
                    &Fsinfo3res {
                        status: NFS3_OK,
                        attributes: self.post_op(Some(&path)).await,
                        rtmax: self.options.rtmax as u32,
                        rtpref: self.options.rtmax as u32,
                        rtmult: 4096,
                        wtmax: self.options.wtmax as u32,
                        wtpref: self.options.wtmax as u32,
                        wtmult: 4096,
                        dtpref: self.options.dtpref as u32,
                        maxfilesize: MAX_OFFSET,
                        time_delta: NfsTime3 {
                            seconds: 0,
                            nseconds: 1_000_000,
                        },
                        properties: FSF3_HOMOGENEOUS
                            | if capabilities.hardlinks { FSF3_LINK } else { 0 }
                            | if capabilities.symlinks {
                                FSF3_SYMLINK
                            } else {
                                0
                            }
                            | if capabilities.times {
                                FSF3_CANSETTIME
                            } else {
                                0
                            },
                    },
                );
            }
            Err(error) => {
                self.record_error();
                write_fsinfo_res(
                    writer,
                    &Fsinfo3res {
                        status: Self::status(&error),
                        attributes: None,
                        rtmax: 0,
                        rtpref: 0,
                        rtmult: 0,
                        wtmax: 0,
                        wtpref: 0,
                        wtmult: 0,
                        dtpref: 0,
                        maxfilesize: 0,
                        time_delta: NfsTime3 {
                            seconds: 0,
                            nseconds: 0,
                        },
                        properties: 0,
                    },
                );
            }
        }
        Ok(())
    }

    async fn pathconf(
        &self,
        args: &mut XdrReader<'_>,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        let handle = args.var_opaque(NFS3_FHSIZE, "nfs_fh3")?;
        args.end("PATHCONF arguments")?;
        match self.path_of(&handle) {
            Ok(path) => {
                let capabilities = self.driver.capabilities;
                write_pathconf_res(
                    writer,
                    &Pathconf3res {
                        status: NFS3_OK,
                        attributes: self.post_op(Some(&path)).await,
                        linkmax: if capabilities.hardlinks { 32_000 } else { 1 },
                        name_max: NAME_MAX as u32,
                        no_trunc: true,
                        chown_restricted: true,
                        case_insensitive: !capabilities.case_sensitive,
                        case_preserving: true,
                    },
                );
            }
            Err(error) => {
                self.record_error();
                write_pathconf_res(
                    writer,
                    &Pathconf3res {
                        status: Self::status(&error),
                        attributes: None,
                        linkmax: 0,
                        name_max: 0,
                        no_trunc: false,
                        chown_restricted: false,
                        case_insensitive: false,
                        case_preserving: false,
                    },
                );
            }
        }
        Ok(())
    }

    async fn mount(
        &self,
        procedure: u32,
        args: &mut XdrReader<'_>,
        context: &NfsRequestContext,
        writer: &mut XdrWriter,
    ) -> Result<(), DispatchError> {
        match procedure {
            MOUNTPROC3_NULL => args.end("MOUNT NULL arguments")?,
            MOUNTPROC3_MNT => {
                let requested = args.string(MNT3_PATHLEN, "dirpath")?;
                args.end("MNT arguments")?;
                let path = mount_rs_core::path::normalize_path(&requested);
                match self.attr_of(&path).await {
                    Ok((entry, attr, _)) if attr.r#type == NF3DIR => {
                        self.mounts
                            .lock()
                            .expect("NFS mount lock")
                            .push(MountRecord {
                                hostname: context
                                    .peer
                                    .clone()
                                    .unwrap_or_else(|| "localhost".to_owned()),
                                directory: path,
                            });
                        write_mount_res(
                            writer,
                            &Mountres3 {
                                status: MNT3_OK,
                                fh: Some(self.handles.encode(&entry)),
                                auth_flavors: vec![AUTH_NONE, AUTH_SYS],
                            },
                        );
                    }
                    Ok(_) => {
                        self.record_error();
                        write_mount_res(
                            writer,
                            &Mountres3 {
                                status: MNT3ERR_NOTDIR,
                                fh: None,
                                auth_flavors: Vec::new(),
                            },
                        );
                    }
                    Err(error) => {
                        self.record_error();
                        let status = match error.code {
                            ErrorCode::Enoent | ErrorCode::Estale => MNT3ERR_NOENT,
                            ErrorCode::Enotdir => MNT3ERR_NOTDIR,
                            ErrorCode::Eacces => MNT3ERR_ACCES,
                            ErrorCode::Eperm => MNT3ERR_PERM,
                            ErrorCode::Enametoolong => MNT3ERR_NAMETOOLONG,
                            _ => MNT3ERR_IO,
                        };
                        write_mount_res(
                            writer,
                            &Mountres3 {
                                status,
                                fh: None,
                                auth_flavors: Vec::new(),
                            },
                        );
                    }
                }
            }
            MOUNTPROC3_DUMP => {
                args.end("DUMP arguments")?;
                let entries: Vec<MountEntry3> = self
                    .mounts
                    .lock()
                    .expect("NFS mount lock")
                    .iter()
                    .map(|record| MountEntry3 {
                        hostname: record.hostname.clone(),
                        directory: record.directory.clone(),
                    })
                    .collect();
                write_mount_list(writer, &entries);
            }
            MOUNTPROC3_UMNT => {
                let requested =
                    mount_rs_core::path::normalize_path(&args.string(MNT3_PATHLEN, "dirpath")?);
                args.end("UMNT arguments")?;
                let host = context.peer.as_deref().unwrap_or("localhost");
                self.mounts
                    .lock()
                    .expect("NFS mount lock")
                    .retain(|record| !(record.hostname == host && record.directory == requested));
            }
            MOUNTPROC3_UMNTALL => {
                args.end("UMNTALL arguments")?;
                let host = context.peer.as_deref().unwrap_or("localhost");
                self.mounts
                    .lock()
                    .expect("NFS mount lock")
                    .retain(|record| record.hostname != host);
            }
            MOUNTPROC3_EXPORT => {
                args.end("EXPORT arguments")?;
                write_export_list(
                    writer,
                    &[ExportEntry3 {
                        directory: "/".to_owned(),
                        groups: Vec::new(),
                    }],
                );
            }
            _ => return Err(DispatchError::ProcedureUnavailable),
        }
        Ok(())
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[derive(Debug, Clone, Copy)]
struct AccessRights {
    read: bool,
    lookup: bool,
    modify: bool,
    extend: bool,
    delete: bool,
    execute: bool,
}

fn allowed_access(stats: &mount_rs_core::Stats, credentials: &RpcCredentials) -> AccessRights {
    let is_dir = stats.mode & S_IFMT == S_IFDIR;
    let mode = stats.mode & 0o777;
    let uid = credentials.uid.unwrap_or(0);
    let gid = credentials.gid.unwrap_or(0);
    let root = uid == 0;
    let bits = if root {
        0b111
    } else if uid == stats.uid {
        (mode >> 6) & 0b111
    } else if gid == stats.gid || credentials.gids.contains(&stats.gid) {
        (mode >> 3) & 0b111
    } else {
        mode & 0b111
    };
    let readable = bits & 0b100 != 0;
    let writable = bits & 0b010 != 0;
    let executable = if root {
        mode & 0o111 != 0 || is_dir
    } else {
        bits & 0b001 != 0
    };
    AccessRights {
        read: readable,
        lookup: executable && is_dir,
        modify: writable,
        extend: writable,
        delete: writable && is_dir,
        execute: executable && !is_dir,
    }
}

fn access_bits3(rights: AccessRights) -> u32 {
    (if rights.read { ACCESS3_READ } else { 0 })
        | (if rights.lookup { ACCESS3_LOOKUP } else { 0 })
        | (if rights.modify { ACCESS3_MODIFY } else { 0 })
        | (if rights.extend { ACCESS3_EXTEND } else { 0 })
        | (if rights.delete { ACCESS3_DELETE } else { 0 })
        | (if rights.execute { ACCESS3_EXECUTE } else { 0 })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use mount_rs_core::{MemoryFs, MemoryOptions};

    async fn session() -> Nfs3Session {
        Nfs3Session::new(
            MemoryFs::new(MemoryOptions::default()),
            NfsSessionOptions::default(),
        )
    }

    #[tokio::test]
    async fn mount_lookup_create_write_read_and_remove_are_filesystem_backed() {
        let session = session().await;
        let mount_call = crate::rpc::encode_call(
            1,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_MNT,
            None,
            None,
            &crate::xdr::encode_xdr(|writer| writer.string("/")),
        );
        let mount_reply = session
            .handle_call(&mount_call, NfsRequestContext::default())
            .await
            .unwrap();
        let mut reader = XdrReader::new(&mount_reply[24..]);
        let mount = read_mount_res(&mut reader).unwrap();
        assert_eq!(mount.status, MNT3_OK);
        let root = mount.fh.unwrap();

        let create_args = crate::xdr::encode_xdr(|writer| {
            write_create_args(
                writer,
                &Create3args {
                    where_: DirOpArgs {
                        dir: root.clone(),
                        name: "hello".to_owned(),
                    },
                    mode: CREATE_UNCHECKED,
                    attributes: Some(Sattr3 {
                        mode: Some(0o644),
                        ..Sattr3::default()
                    }),
                    verf: None,
                },
            )
        });
        let create_call = crate::rpc::encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_CREATE,
            None,
            None,
            &create_args,
        );
        let create_reply = session
            .handle_call(&create_call, NfsRequestContext::default())
            .await
            .unwrap();
        let mut reader = XdrReader::new(&create_reply[24..]);
        let created = read_create_res(&mut reader).unwrap();
        assert_eq!(created.status, NFS3_OK);
        let file = created.obj.unwrap();

        let write_args = crate::xdr::encode_xdr(|writer| {
            write_write_args(
                writer,
                &Write3args {
                    file: file.clone(),
                    offset: 0,
                    count: 5,
                    stable: FILE_SYNC,
                    data: b"hello".to_vec(),
                },
            )
        });
        let write_call = crate::rpc::encode_call(
            3,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_WRITE,
            None,
            None,
            &write_args,
        );
        let write_reply = session
            .handle_call(&write_call, NfsRequestContext::default())
            .await
            .unwrap();
        let mut reader = XdrReader::new(&write_reply[24..]);
        assert_eq!(read_write_res(&mut reader).unwrap().status, NFS3_OK);

        let read_args = crate::xdr::encode_xdr(|writer| {
            write_read_args(
                writer,
                &Read3args {
                    file,
                    offset: 0,
                    count: 16,
                },
            )
        });
        let read_call = crate::rpc::encode_call(
            4,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_READ,
            None,
            None,
            &read_args,
        );
        let read_reply = session
            .handle_call(&read_call, NfsRequestContext::default())
            .await
            .unwrap();
        let mut reader = XdrReader::new(&read_reply[24..]);
        let read = read_read_res(&mut reader, 1024).unwrap();
        assert_eq!(read.status, NFS3_OK);
        assert_eq!(read.data, b"hello");
    }

    #[tokio::test]
    async fn unsupported_auth_flavor_is_rejected_before_dispatch() {
        let session = session().await;
        let unsupported = crate::rpc::OpaqueAuth {
            flavor: 99,
            body: Vec::new(),
        };
        let call = crate::rpc::encode_call(
            5,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_NULL,
            Some(&unsupported),
            None,
            &[],
        );
        let reply = session
            .handle_call(&call, NfsRequestContext::default())
            .await
            .unwrap();
        let (reply, results) = crate::rpc::decode_reply(&reply).unwrap();
        assert_eq!(reply.reply_stat, crate::rpc::MSG_DENIED);
        assert_eq!(reply.reject_stat, Some(crate::rpc::RPC_AUTH_ERROR));
        assert_eq!(reply.auth_stat, Some(AUTH_TOOWEAK));
        results.end("AUTH_TOOWEAK reply").unwrap();
    }

    #[tokio::test]
    async fn status_errors_report_without_an_rpc_call() {
        let observed = Arc::new(Mutex::new(Vec::<(String, Option<u32>)>::new()));
        let callback_observed = Arc::clone(&observed);
        let session = Nfs3Session::new_with_hooks(
            MemoryFs::new(MemoryOptions::default()),
            NfsSessionOptions::default(),
            NfsSessionHooks {
                on_error: Some(Arc::new(move |error, call| {
                    callback_observed
                        .lock()
                        .expect("NFS session error lock")
                        .push((error.message, call.map(|call| call.xid)));
                })),
            },
        );
        let call = crate::rpc::encode_call(
            6,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_GETATTR,
            None,
            None,
            &crate::xdr::encode_xdr(|writer| writer.var_opaque(&[0_u8; FH_SIZE])),
        );
        let reply = session
            .handle_call(&call, NfsRequestContext::default())
            .await
            .expect("status failure still receives a reply");
        assert_eq!(session.stats().errors, 1);
        let observed = observed.lock().expect("NFS session error lock");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].1, None);
        assert_eq!(observed[0].0, "NFS request returned an error");
        assert!(!reply.is_empty());
    }
}
