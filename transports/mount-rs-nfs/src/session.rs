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
    ErrorCode, FileHandle, FsDriver, FsError, GuardedDirectoryEntry, GuardedMutation,
    GuardedMutationResult, GuardedRead, GuardedReadResult, GuardedSetattr, Loopback, MkdirOptions,
    ObservedEntry, OpenFlags, PathGuard, PathIdentity, Result as FsResult, S_IFBLK, S_IFCHR,
    S_IFDIR, S_IFIFO, S_IFMT, S_IFSOCK,
};

use crate::constants::*;
use crate::handles::{
    DirectorySnapshot, DirectorySnapshots, FH_SIZE, FileHandleTable, FileHandleTableOptions,
    HandleEntry, cookie_verifier, guarded_cookie_verifier, same_backend_inode, same_verifier,
};
use crate::protocol::*;
use crate::rpc::{
    AUTH_NONE, AUTH_SYS, RPC_GARBAGE_ARGS, RPC_PROC_UNAVAIL, RPC_PROG_MISMATCH, RPC_PROG_UNAVAIL,
    RPC_SYSTEM_ERR, RPC_VERSION, RpcCall, RpcCredentials, checked_credentials_of, decode_call,
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
    /// Validate opaque handles against the live backend inode and require
    /// atomic identity guards for NFSv3 mutations. NFSv4 is unavailable in
    /// this mode until it can apply the same guards.
    pub shared_concurrent_view: bool,
    /// Omit NFSv3 weak cache consistency attributes for a nonshared view.
    /// Shared views always omit them because another server can change
    /// post-operation attributes before this server writes its reply.
    pub omit_wcc_attributes: bool,
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
            shared_concurrent_view: false,
            omit_wcc_attributes: false,
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
/// NFSv4.1 sessions. Shared multiwriter sessions advertise only NFSv3, since
/// NFSv4 handle mutations do not use the v3 identity guards.
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
            } else if let Err(auth_status) = checked_credentials_of(&call.cred) {
                encode_auth_error(call.xid, auth_status)
            } else if call.program != NFS_PROGRAM {
                encode_accept_error(call.xid, RPC_PROG_UNAVAIL, None)
            } else {
                let highest = if v3.options.shared_concurrent_view {
                    NFS_V3
                } else {
                    NFS_V4
                };
                encode_accept_error(call.xid, RPC_PROG_MISMATCH, Some((NFS_V3, highest)))
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
    identity: Option<PathIdentity>,
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
    fn set(&mut self, path: String, verifier: Vec<u8>, identity: Option<PathIdentity>) {
        self.forget(&path);
        self.entries.insert(
            path.clone(),
            ExclusiveCreate {
                verifier,
                identity,
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

    fn matches(&mut self, path: &str, verifier: &[u8], identity: Option<PathIdentity>) -> bool {
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
            .is_some_and(|entry| entry.verifier == verifier && entry.identity == identity)
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
/// Byte-oriented NFSv3 session. Call [`Self::destroy`] before dropping a
/// session that shares its driver with another live mount. Retained backend
/// file handles require asynchronous close.
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
    retained_handles: Arc<RetainedHandles>,
    stats: SharedStats,
    destroyed: Arc<Mutex<bool>>,
    path_lock: Arc<tokio::sync::RwLock<()>>,
    hooks: NfsSessionHooks,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseOutcome {
    Pending,
    Succeeded,
    Failed,
}

struct CloseEntry {
    handle: Arc<dyn FileHandle>,
    closing: Option<tokio::sync::watch::Receiver<CloseOutcome>>,
}

impl CloseEntry {
    fn needs_attempt(&self) -> bool {
        match self.closing.as_ref() {
            None => true,
            Some(wait) => match *wait.borrow() {
                CloseOutcome::Pending => wait.has_changed().is_err(),
                CloseOutcome::Succeeded => false,
                CloseOutcome::Failed => true,
            },
        }
    }
}

struct ProvisionalCloses {
    next_id: u64,
    by_id: HashMap<u64, CloseEntry>,
}

struct RetainedHandles {
    table: FileHandleTable,
    by_id: Mutex<HashMap<u64, CloseEntry>>,
    provisional: Mutex<ProvisionalCloses>,
    active: tokio::sync::watch::Sender<usize>,
}

impl RetainedHandles {
    fn begin(self: &Arc<Self>) -> ActiveRetention {
        self.active
            .send_modify(|count| *count = count.saturating_add(1));
        ActiveRetention {
            retained: Arc::clone(self),
        }
    }

    async fn await_active_zero(&self) {
        let mut active = self.active.subscribe();
        while *active.borrow_and_update() > 0 {
            if active.changed().await.is_err() {
                break;
            }
        }
    }

    fn register_provisional(&self, handle: Arc<dyn FileHandle>) -> u64 {
        let mut state = self.provisional.lock().expect("NFS provisional close lock");
        let id = state.next_id;
        state.next_id = state
            .next_id
            .checked_add(1)
            .expect("provisional close id overflow");
        state.by_id.insert(
            id,
            CloseEntry {
                handle,
                closing: None,
            },
        );
        id
    }

    fn schedule_retained(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
    ) -> Vec<tokio::sync::watch::Receiver<CloseOutcome>> {
        let mut retained = self.by_id.lock().expect("NFS retained handle lock");
        let mut waits = Vec::with_capacity(retained.len());
        for (&id, entry) in retained.iter_mut() {
            if entry.needs_attempt() {
                let handle = Arc::clone(&entry.handle);
                let owner = Arc::clone(self);
                let (completed, wait) = tokio::sync::watch::channel(CloseOutcome::Pending);
                runtime.spawn(async move {
                    let outcome = if handle.close().await.is_ok() {
                        let removed = owner
                            .by_id
                            .lock()
                            .expect("NFS retained handle lock")
                            .remove(&id);
                        if removed.is_some() {
                            owner.table.unpin(id);
                        }
                        CloseOutcome::Succeeded
                    } else {
                        CloseOutcome::Failed
                    };
                    let _ = completed.send(outcome);
                });
                entry.closing = Some(wait);
            }
            waits.push(
                entry
                    .closing
                    .as_ref()
                    .expect("retained close waiter")
                    .clone(),
            );
        }
        waits
    }

    fn schedule_provisional(
        self: &Arc<Self>,
        id: u64,
        runtime: &tokio::runtime::Handle,
    ) -> Option<tokio::sync::watch::Receiver<CloseOutcome>> {
        let mut state = self.provisional.lock().expect("NFS provisional close lock");
        let entry = state.by_id.get_mut(&id)?;
        if entry.needs_attempt() {
            let handle = Arc::clone(&entry.handle);
            let owner = Arc::clone(self);
            let (completed, wait) = tokio::sync::watch::channel(CloseOutcome::Pending);
            runtime.spawn(async move {
                let outcome = if handle.close().await.is_ok() {
                    owner
                        .provisional
                        .lock()
                        .expect("NFS provisional close lock")
                        .by_id
                        .remove(&id);
                    CloseOutcome::Succeeded
                } else {
                    CloseOutcome::Failed
                };
                let _ = completed.send(outcome);
            });
            entry.closing = Some(wait);
        }
        entry.closing.clone()
    }

    fn schedule_all_provisional(
        self: &Arc<Self>,
        runtime: &tokio::runtime::Handle,
    ) -> Vec<tokio::sync::watch::Receiver<CloseOutcome>> {
        let ids = self
            .provisional
            .lock()
            .expect("NFS provisional close lock")
            .by_id
            .keys()
            .copied()
            .collect::<Vec<_>>();
        ids.into_iter()
            .filter_map(|id| self.schedule_provisional(id, runtime))
            .collect()
    }

    async fn await_attempts(waits: Vec<tokio::sync::watch::Receiver<CloseOutcome>>) {
        for mut wait in waits {
            while *wait.borrow_and_update() == CloseOutcome::Pending {
                if wait.changed().await.is_err() {
                    break;
                }
            }
        }
    }

    fn all_closed(&self) -> bool {
        self.by_id
            .lock()
            .expect("NFS retained handle lock")
            .is_empty()
            && self
                .provisional
                .lock()
                .expect("NFS provisional close lock")
                .by_id
                .is_empty()
    }
}

struct ActiveRetention {
    retained: Arc<RetainedHandles>,
}

impl Drop for ActiveRetention {
    fn drop(&mut self) {
        self.retained
            .active
            .send_modify(|count| *count = count.saturating_sub(1));
    }
}

impl Drop for RetainedHandles {
    fn drop(&mut self) {
        // Balance table pins even if a caller drops the last session clone
        // without async destroy. Backend handles still require async close;
        // dropping their Arc alone does not release every backend open ref.
        let retained = std::mem::take(
            self.by_id
                .get_mut()
                .unwrap_or_else(|error| error.into_inner()),
        );
        for (id, entry) in retained {
            drop(entry);
            self.table.unpin(id);
        }
    }
}

struct ProvisionalRetainedPin {
    table: FileHandleTable,
    id: u64,
    transferred: bool,
}

impl ProvisionalRetainedPin {
    fn new(table: &FileHandleTable, id: u64) -> Self {
        Self {
            table: table.clone(),
            id,
            transferred: false,
        }
    }

    fn transfer(&mut self) {
        self.transferred = true;
    }
}

impl Drop for ProvisionalRetainedPin {
    fn drop(&mut self) {
        if !self.transferred {
            self.table.unpin(self.id);
        }
    }
}

/// Own a newly opened descriptor until it is registered as a retained handle.
/// Aborting an RPC while validating its identity must still close the backend
/// handle, even though the provisional table pin already drops with the RPC.
struct ProvisionalOpenedHandle {
    handle: Option<Arc<dyn FileHandle>>,
    runtime: Option<tokio::runtime::Handle>,
    retained: Arc<RetainedHandles>,
}

impl ProvisionalOpenedHandle {
    fn new(handle: Arc<dyn FileHandle>, retained: Arc<RetainedHandles>) -> Self {
        Self {
            handle: Some(handle),
            runtime: tokio::runtime::Handle::try_current().ok(),
            retained,
        }
    }

    fn as_ref(&self) -> &dyn FileHandle {
        self.handle
            .as_ref()
            .expect("provisional open handle")
            .as_ref()
    }

    fn clone_handle(&self) -> Arc<dyn FileHandle> {
        self.handle
            .as_ref()
            .expect("provisional open handle")
            .clone()
    }

    fn transfer(mut self) -> Arc<dyn FileHandle> {
        self.handle.take().expect("provisional open handle")
    }

    fn schedule_close(&mut self) -> Option<tokio::sync::watch::Receiver<CloseOutcome>> {
        let handle = self.handle.take()?;
        let id = self.retained.register_provisional(handle);
        if let Some(runtime) = self
            .runtime
            .clone()
            .or_else(|| tokio::runtime::Handle::try_current().ok())
        {
            self.retained.schedule_provisional(id, &runtime)
        } else if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            // Direct session users may poll on a non-Tokio executor. There
            // is no current runtime to schedule onto, so close synchronously.
            let wait = self.retained.schedule_provisional(id, runtime.handle())?;
            runtime.block_on(RetainedHandles::await_attempts(vec![wait.clone()]));
            Some(wait)
        } else {
            // Keep the descriptor registered for a later destroy retry.
            None
        }
    }

    async fn close(&mut self) {
        if let Some(mut completed) = self.schedule_close() {
            // Cancellation only drops this waiter; the single close task
            // continues through backend close_inode and is drained by destroy.
            while *completed.borrow_and_update() == CloseOutcome::Pending {
                if completed.changed().await.is_err() {
                    break;
                }
            }
        }
    }
}

impl Drop for ProvisionalOpenedHandle {
    fn drop(&mut self) {
        let _ = self.schedule_close();
    }
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
        if options.shared_concurrent_view && driver.driver.stable_inode_ids() {
            handles.trust_stable_inode_ids();
        }
        let write_verifier = handles.verifier();
        let (active, _) = tokio::sync::watch::channel(0usize);
        let retained_handles = Arc::new(RetainedHandles {
            table: handles.clone(),
            by_id: Mutex::new(HashMap::new()),
            provisional: Mutex::new(ProvisionalCloses {
                next_id: 1,
                by_id: HashMap::new(),
            }),
            active,
        });
        Self {
            driver,
            options: options.clone(),
            handles,
            write_verifier,
            snapshots: DirectorySnapshots::new(options.snapshot_cache),
            mounts: Arc::new(Mutex::new(Vec::new())),
            exclusive_creates: Arc::new(Mutex::new(ExclusiveCreates::default())),
            retained_handles,
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

    /// Returns `true` after all retained and provisional descriptors close
    /// and the session's handle table is cleared. `false` means at least one
    /// close is incomplete; the caller must retry while the session and a
    /// Tokio runtime remain live. New RPCs are rejected after the first call.
    pub async fn destroy(&self) -> bool {
        let _path_guard = self.path_lock.write().await;
        *self.destroyed.lock().expect("NFS destroyed lock") = true;
        // Retention begins under the same destroyed lock. Wait for every
        // in-flight opener to register its descriptor or provisional close
        // before taking a snapshot of work to close.
        self.retained_handles.await_active_zero().await;
        let current_runtime = tokio::runtime::Handle::try_current().ok();
        let fallback_runtime = if current_runtime.is_none() {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .ok()
        } else {
            None
        };
        let close_runtime = current_runtime.or_else(|| {
            fallback_runtime
                .as_ref()
                .map(|runtime| runtime.handle().clone())
        });
        let Some(close_runtime) = close_runtime else {
            // Keep the map intact so a caller can retry destroy if even a
            // local Tokio runtime cannot be created to finish async closes.
            return false;
        };
        // Keep each Arc in the registry until close succeeds. If a runtime
        // stops midway through teardown, a later destroy can schedule every
        // unfinished descriptor on the new runtime without losing its pin.
        let mut waits = self.retained_handles.schedule_retained(&close_runtime);
        waits.extend(
            self.retained_handles
                .schedule_all_provisional(&close_runtime),
        );
        if let Some(runtime) = fallback_runtime {
            runtime.block_on(RetainedHandles::await_attempts(waits));
        } else {
            RetainedHandles::await_attempts(waits).await;
        }
        if !self.retained_handles.all_closed() {
            // A backend close failed or its runtime stopped. Preserve the
            // registry and table for a later destroy attempt.
            return false;
        }
        self.handles.clear();
        self.snapshots.clear();
        self.mounts.lock().expect("NFS mount lock").clear();
        self.exclusive_creates
            .lock()
            .expect("NFS exclusive-create lock")
            .clear();
        true
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
        if self.destroyed() {
            self.record_error();
            let mut stats = self.stats.0.lock().expect("NFS stats lock");
            stats.replies = stats.replies.saturating_add(1);
            return Some(encode_accept_error(call.xid, RPC_SYSTEM_ERR, None));
        }
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
        let credentials = match checked_credentials_of(&call.cred) {
            Ok(credentials) => credentials,
            Err(auth_status) => return Ok(encode_auth_error(call.xid, auth_status)),
        };
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
        let mut writer = XdrWriter::with_capacity(256);
        write_accepted_reply_header(&mut writer, call.xid);
        match (call.program, call.procedure) {
            (NFS_PROGRAM, NFSPROC3_READ | NFSPROC3_WRITE) => {
                // Reads and writes can block in a backend; do not hold the
                // path-map lock across either operation.
                self.nfs(call.procedure, args, &credentials, &mut writer)
                    .await?;
            }
            (NFS_PROGRAM, NFSPROC3_REMOVE | NFSPROC3_RMDIR | NFSPROC3_RENAME) => {
                // Destructive namespace changes must wait for a stat/bind in
                // either version, or an unlinked path can be rebound later.
                let _guard = self.path_lock.write().await;
                if self.destroyed() {
                    return Ok(encode_accept_error(call.xid, RPC_SYSTEM_ERR, None));
                }
                self.nfs(call.procedure, args, &credentials, &mut writer)
                    .await?;
            }
            _ => {
                let _guard = self.path_lock.read().await;
                if self.destroyed() {
                    return Ok(encode_accept_error(call.xid, RPC_SYSTEM_ERR, None));
                }
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
        self.describe_stats(path, stats).await
    }

    async fn describe_stats(
        &self,
        path: &str,
        stats: mount_rs_core::Stats,
    ) -> FsResult<(HandleEntry, Fattr3, mount_rs_core::Stats)> {
        self.require_driver_inode_keys()?;
        if self.options.shared_concurrent_view && PathIdentity::from_stats(&stats).is_none() {
            return Err(FsError::new(ErrorCode::Estale)
                .with_message("shared view requires stable backend inode identity"));
        }
        let active = if stats.is_file() {
            Some(self.begin_retention()?)
        } else {
            None
        };
        let entry = if stats.is_file() {
            self.handles.bind_pinned(path, &stats)
        } else {
            self.handles.bind(path, &stats)
        };
        if stats.is_file() {
            // Best effort: a lookup must still be able to report attributes for
            // a read-only/special backend that cannot create a second handle.
            // The normal regular-file path can retain a descriptor, which is
            // what makes NFSv3's synthetic client-side OPEN survive unlink.
            let pin = ProvisionalRetainedPin::new(&self.handles, entry.id);
            if let Err(error) = self
                .ensure_retained_handle_with_pin(
                    &entry,
                    path,
                    pin,
                    active.expect("regular file has active retention"),
                )
                .await
                && self.options.shared_concurrent_view
                && error.code == ErrorCode::Estale
            {
                return Err(error);
            }
        }
        let attr = fattr_of(&stats, entry.fileid);
        Ok((entry, attr, stats))
    }

    fn retained_handle(&self, id: u64) -> Option<Arc<dyn FileHandle>> {
        self.retained_handles
            .by_id
            .lock()
            .expect("NFS retained handle lock")
            .get(&id)
            .map(|entry| Arc::clone(&entry.handle))
    }

    fn begin_retention(&self) -> FsResult<ActiveRetention> {
        let destroyed = self.destroyed.lock().expect("NFS destroyed lock");
        if *destroyed {
            return Err(FsError::new(ErrorCode::Estale).with_message("NFS session was destroyed"));
        }
        Ok(self.retained_handles.begin())
    }

    async fn retained_attributes(&self, entry: Option<&HandleEntry>) -> Option<FsResult<Fattr3>> {
        let entry = entry?;
        let retained = self.retained_handle(entry.id)?;
        Some(retained.stat().await.and_then(|stats| {
            self.check_handle_identity(entry, &stats)?;
            Ok(fattr_of(&stats, entry.fileid))
        }))
    }

    async fn ensure_retained_handle(
        &self,
        entry: &HandleEntry,
        path: &str,
    ) -> FsResult<Arc<dyn FileHandle>> {
        let active = self.begin_retention()?;
        if let Some(handle) = self.retained_handle(entry.id) {
            return Ok(handle);
        }
        if !self.handles.try_pin(entry.id) {
            return Err(FsError::new(ErrorCode::Estale)
                .with_message("file handle was retired before opening a descriptor"));
        }
        let pin = ProvisionalRetainedPin::new(&self.handles, entry.id);
        self.ensure_retained_handle_with_pin(entry, path, pin, active)
            .await
    }

    async fn ensure_retained_handle_with_pin(
        &self,
        entry: &HandleEntry,
        path: &str,
        mut pin: ProvisionalRetainedPin,
        _active: ActiveRetention,
    ) -> FsResult<Arc<dyn FileHandle>> {
        if let Some(handle) = self.retained_handle(entry.id) {
            return Ok(handle);
        }
        let handle = self
            .driver
            .open_flags(path, OpenFlags::READ_ONLY, 0)
            .await?;
        let mut opened = ProvisionalOpenedHandle::new(handle, Arc::clone(&self.retained_handles));
        if let Err(error) = self.check_opened_handle(entry, opened.as_ref()).await {
            opened.close().await;
            return Err(error);
        }
        let existing = {
            let mut retained = self
                .retained_handles
                .by_id
                .lock()
                .expect("NFS retained handle lock");
            if let Some(existing) = retained
                .get(&entry.id)
                .map(|entry| Arc::clone(&entry.handle))
            {
                Ok(Some(existing))
            } else if *self.destroyed.lock().expect("NFS destroyed lock") {
                Err(FsError::new(ErrorCode::Estale)
                    .with_message("NFS session was destroyed while retaining a file handle"))
            } else {
                retained.insert(
                    entry.id,
                    CloseEntry {
                        handle: opened.clone_handle(),
                        closing: None,
                    },
                );
                pin.transfer();
                Ok(None)
            }
        };
        match existing {
            Ok(Some(existing)) => {
                opened.close().await;
                Ok(existing)
            }
            Ok(None) => Ok(opened.transfer()),
            Err(error) => {
                opened.close().await;
                Err(error)
            }
        }
    }

    async fn post_op(&self, path: Option<&str>) -> Option<Fattr3> {
        let path = path?;
        self.attr_of(path).await.ok().map(|(_, attr, _)| attr)
    }

    async fn pre_op(&self, path: &str) -> Option<WccAttr> {
        if self.options.shared_concurrent_view || self.options.omit_wcc_attributes {
            return None;
        }
        self.stat_of(path)
            .await
            .ok()
            .map(|stats| wcc_attr_of(&stats))
    }

    async fn wcc(&self, before: Option<WccAttr>, path: &str) -> WccData {
        self.wcc_opt(before, Some(path)).await
    }

    async fn wcc_opt(&self, before: Option<WccAttr>, path: Option<&str>) -> WccData {
        if self.options.shared_concurrent_view || self.options.omit_wcc_attributes {
            return WccData {
                before: None,
                after: None,
            };
        }
        WccData {
            before,
            after: self.post_op(path).await,
        }
    }

    async fn path_of(&self, handle: &[u8]) -> FsResult<String> {
        let entry = self.handles.decode(handle)?;
        self.path_of_entry(&entry).await
    }

    async fn path_of_entry(&self, entry: &HandleEntry) -> FsResult<String> {
        if self.options.shared_concurrent_view {
            let preserve_orphan_key =
                self.retained_handle(entry.id).is_some() || self.driver.driver.stable_inode_ids();
            self.handles
                .live_path_of(entry, self.driver.driver.as_ref(), preserve_orphan_key)
                .await
        } else {
            self.handles.path_of(entry)
        }
    }

    fn check_handle_identity(
        &self,
        entry: &HandleEntry,
        stats: &mount_rs_core::Stats,
    ) -> FsResult<()> {
        if !self.options.shared_concurrent_view || entry.id == crate::handles::ROOT_HANDLE_ID {
            return Ok(());
        }
        let expected = entry
            .key
            .as_deref()
            .and_then(PathIdentity::parse_backend_key)
            .ok_or_else(|| {
                FsError::new(ErrorCode::Estale).with_message("handle has no inode key")
            })?;
        let actual = PathIdentity::from_stats(stats)
            .ok_or_else(|| FsError::new(ErrorCode::Estale).with_message("inode is unknown"))?;
        if actual != expected {
            return Err(FsError::new(ErrorCode::Estale)
                .with_message("path no longer names the handle's inode"));
        }
        Ok(())
    }

    async fn check_opened_handle(
        &self,
        entry: &HandleEntry,
        handle: &dyn FileHandle,
    ) -> FsResult<()> {
        if self.options.shared_concurrent_view {
            self.check_handle_identity(entry, &handle.stat().await?)?;
        }
        Ok(())
    }

    fn require_driver_inode_keys(&self) -> FsResult<()> {
        if self.options.shared_concurrent_view && !self.options.use_driver_ino {
            Err(FsError::new(ErrorCode::Enotsup)
                .with_message("shared NFS requires backend inode handle keys"))
        } else {
            Ok(())
        }
    }

    fn require_guarded_mutations(&self) -> FsResult<()> {
        self.require_driver_inode_keys()?;
        if self.options.shared_concurrent_view && !self.driver.driver.supports_guarded_mutations() {
            Err(FsError::new(ErrorCode::Enotsup)
                .with_message("shared NFS mutations require backend identity guards"))
        } else {
            Ok(())
        }
    }

    async fn path_guard(&self, handle: &[u8]) -> FsResult<PathGuard> {
        let entry = self.handles.decode(handle)?;
        let path = self.path_of_entry(&entry).await?;
        let identity = if entry.id == crate::handles::ROOT_HANDLE_ID {
            PathIdentity::from_stats(&self.stat_of(&path).await?)
        } else {
            entry
                .key
                .as_deref()
                .and_then(PathIdentity::parse_backend_key)
        }
        .ok_or_else(|| FsError::new(ErrorCode::Estale).with_message("handle has no inode key"))?;
        Ok(PathGuard { path, identity })
    }

    async fn observed_entry(&self, path: &str) -> FsResult<ObservedEntry> {
        match self.stat_of(path).await {
            Ok(stats) => PathIdentity::from_stats(&stats)
                .map(ObservedEntry::Identity)
                .ok_or_else(|| FsError::new(ErrorCode::Estale)),
            Err(error) if error.code == ErrorCode::Enoent => Ok(ObservedEntry::Absent),
            Err(error) => Err(error),
        }
    }

    async fn guarded(&self, request: GuardedMutation) -> FsResult<GuardedMutationResult> {
        self.require_guarded_mutations()?;
        self.driver.driver.guarded_mutation(request).await
    }

    fn require_guarded_reads(&self) -> FsResult<()> {
        self.require_driver_inode_keys()?;
        if self.options.shared_concurrent_view && !self.driver.driver.supports_guarded_reads() {
            Err(FsError::new(ErrorCode::Enotsup)
                .with_message("shared NFS reads require backend identity guards"))
        } else {
            Ok(())
        }
    }

    async fn guarded_read(&self, request: GuardedRead) -> FsResult<GuardedReadResult> {
        self.require_guarded_reads()?;
        self.driver.driver.guarded_read(request).await
    }

    fn guarded_sattr(
        attr: &Sattr3,
        current: &mount_rs_core::Stats,
        expected_ctime_ms: Option<i64>,
    ) -> FsResult<GuardedSetattr> {
        let now = now_ms();
        let time = |value: &SetTime3, old| match value.how {
            DONT_CHANGE => None,
            SET_TO_SERVER_TIME => Some(now),
            SET_TO_CLIENT_TIME => Some(from_time(value.time.unwrap_or(NfsTime3 {
                seconds: 0,
                nseconds: 0,
            }))),
            _ => Some(old),
        };
        Ok(GuardedSetattr {
            mode: if current.mode & S_IFMT == mount_rs_core::types::S_IFLNK {
                None
            } else {
                attr.mode.map(|mode| mode & 0o7777)
            },
            uid: attr.uid,
            gid: attr.gid,
            size: attr
                .size
                .map(|size| Self::offset(size, "truncate"))
                .transpose()?,
            atime_ms: time(&attr.atime, current.atime_ms),
            mtime_ms: time(&attr.mtime, current.mtime_ms),
            expected_ctime_ms,
        })
    }

    async fn created_stats(&self, target: &PathGuard) -> FsResult<mount_rs_core::Stats> {
        let stats = self.stat_of(&target.path).await?;
        if PathIdentity::from_stats(&stats) != Some(target.identity) {
            return Err(FsError::new(ErrorCode::Estale)
                .with_message("created path no longer names the committed inode"));
        }
        Ok(stats)
    }

    async fn apply_created_sattr(&self, target: &PathGuard, attr: &Sattr3) -> FsResult<()> {
        let current = self.created_stats(target).await?;
        let change = Self::guarded_sattr(attr, &current, None)?;
        self.guarded(GuardedMutation::Setattr {
            target: target.clone(),
            change,
        })
        .await?;
        Ok(())
    }

    async fn claim_created_owner(
        &self,
        target: &PathGuard,
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
        let current = self.created_stats(target).await?;
        let current_mode = current.mode & 0o7777;
        let wanted_mode = if !parent_setgid {
            current_mode
        } else if directory {
            current_mode | S_ISGID
        } else {
            let setgid_executable = mode & (S_ISGID | S_IXGRP) == (S_ISGID | S_IXGRP);
            let member = credentials.gid == Some(gid) || credentials.gids.contains(&gid);
            if setgid_executable && !member && uid != 0 {
                current_mode & !S_ISGID
            } else {
                current_mode
            }
        };
        let change = GuardedSetattr {
            mode: (wanted_mode != current_mode).then_some(wanted_mode),
            uid: (uid != u32::MAX).then_some(uid),
            gid: (gid != u32::MAX).then_some(gid),
            ..GuardedSetattr::default()
        };
        if change.mode.is_some() || change.uid.is_some() || change.gid.is_some() {
            self.guarded(GuardedMutation::Setattr {
                target: target.clone(),
                change,
            })
            .await?;
        }
        Ok(())
    }

    async fn open_child(
        &self,
        parent: Option<&PathGuard>,
        name: &str,
        path: &str,
        observed: ObservedEntry,
        flags: OpenFlags,
        mode: u32,
    ) -> FsResult<(Arc<dyn FileHandle>, Option<PathIdentity>)> {
        if let Some(parent) = parent {
            let result = self
                .guarded(GuardedMutation::Open {
                    parent: parent.clone(),
                    name: name.to_owned(),
                    observed,
                    flags,
                    mode,
                })
                .await?;
            match result {
                GuardedMutationResult::Opened { handle, identity } => {
                    let stats = match handle.stat().await {
                        Ok(stats) => stats,
                        Err(error) => {
                            let _ = handle.close().await;
                            return Err(error);
                        }
                    };
                    if PathIdentity::from_stats(&stats) != Some(identity) {
                        let _ = handle.close().await;
                        return Err(FsError::new(ErrorCode::Estale));
                    }
                    Ok((handle, Some(identity)))
                }
                _ => Err(FsError::new(ErrorCode::Eio)
                    .with_message("guarded open returned no file handle")),
            }
        } else {
            self.driver
                .open_flags(path, flags, mode)
                .await
                .map(|handle| (handle, None))
        }
    }

    fn join_path(directory: &str, name: &str) -> String {
        mount_rs_core::path::normalize_path(&format!("{directory}/{name}"))
    }

    fn check_name(name: &str) -> FsResult<()> {
        if name.is_empty() || name.contains(['/', '\0']) || name == "." || name == ".." {
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
        let entry = self.handles.decode(&handle).ok();
        let attributes = if self.options.shared_concurrent_view {
            async {
                let target = self.path_guard(&handle).await?;
                let GuardedReadResult::Stat(stats) =
                    self.guarded_read(GuardedRead::Stat { target }).await?
                else {
                    return Err(FsError::new(ErrorCode::Eio));
                };
                let entry = entry
                    .as_ref()
                    .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
                self.check_handle_identity(entry, &stats)?;
                Ok(fattr_of(&stats, entry.fileid))
            }
            .await
        } else {
            match self.path_of(&handle).await {
                Ok(path) => self.attr_of(&path).await.map(|(_, attr, _)| attr),
                Err(error) => Err(error),
            }
        };
        // An NFSv3 client can hold an inode after its last name disappears.
        // If namespace validation loses that name, describe the original
        // retained descriptor only after checking its dev:ino against the FH.
        let attributes = match attributes {
            Ok(attr) => Ok(attr),
            Err(error) => self
                .retained_attributes(entry.as_ref())
                .await
                .unwrap_or(Err(error)),
        };
        match attributes {
            Ok(attr) => write_getattr_res(
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
            let target = if self.options.shared_concurrent_view {
                self.require_guarded_mutations()?;
                Some(self.path_guard(&request.object).await?)
            } else {
                None
            };
            let resolved = match target.as_ref() {
                Some(target) => target.path.clone(),
                None => self.path_of(&request.object).await?,
            };
            let current = self.stat_of(&resolved).await?;
            if let Some(target) = target.as_ref()
                && PathIdentity::from_stats(&current) != Some(target.identity)
            {
                return Err(FsError::new(ErrorCode::Estale));
            }
            path = Some(resolved.clone());
            before = Some(wcc_attr_of(&current));
            if let Some(guard) = request.guard
                && guard != to_time(current.ctime_ms)
            {
                forced_status = Some(NFS3ERR_NOT_SYNC);
                return Err(FsError::new(ErrorCode::Eio));
            }
            if let Some(target) = target {
                let change = Self::guarded_sattr(
                    &request.attributes,
                    &current,
                    request.guard.map(from_time),
                )?;
                self.guarded(GuardedMutation::Setattr { target, change })
                    .await?;
                Ok(())
            } else {
                self.apply_sattr(&resolved, &request.attributes, &current)
                    .await
            }
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
                        status: forced_status.unwrap_or_else(|| {
                            if request.guard.is_some() && error.code == ErrorCode::Eagain {
                                NFS3ERR_NOT_SYNC
                            } else {
                                Self::status(&error)
                            }
                        }),
                        wcc: self.wcc_opt(before, path.as_deref()).await,
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
        let mut directory = None;
        let result = async {
            let parent_guard = if self.options.shared_concurrent_view {
                self.require_guarded_reads()?;
                Some(self.path_guard(&request.dir).await?)
            } else {
                None
            };
            let dir = match parent_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.dir).await?,
            };
            directory = Some(dir.clone());
            let path = if request.name == "." {
                dir.clone()
            } else if request.name == ".." {
                mount_rs_core::path::dirname(&dir)
            } else {
                Self::check_name(&request.name)?;
                Self::join_path(&dir, &request.name)
            };
            if let Some(parent_guard) = parent_guard {
                let parent_entry = self.handles.decode(&request.dir)?;
                let original_identity = parent_guard.identity;
                let GuardedReadResult::Lookup { parent, child } = self
                    .guarded_read(GuardedRead::Lookup {
                        parent: parent_guard,
                        name: request.name.clone(),
                    })
                    .await?
                else {
                    return Err(FsError::new(ErrorCode::Eio));
                };
                if PathIdentity::from_stats(&parent) != Some(original_identity) {
                    return Err(FsError::new(ErrorCode::Estale));
                }
                let (entry, attr, _) = self.describe_stats(&path, child).await?;
                Ok((entry, attr, Some(fattr_of(&parent, parent_entry.fileid))))
            } else {
                let (entry, attr, _) = self.attr_of(&path).await?;
                Ok((entry, attr, self.post_op(Some(&dir)).await))
            }
        }
        .await;
        match result {
            Ok((entry, attr, dir_attributes)) => write_lookup_res(
                writer,
                &Lookup3res {
                    status: NFS3_OK,
                    object: Some(self.handles.encode(&entry)),
                    obj_attributes: Some(attr),
                    dir_attributes,
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
                        dir_attributes: if self.options.shared_concurrent_view {
                            None
                        } else {
                            self.post_op(directory.as_deref()).await
                        },
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
        let result = async {
            if self.options.shared_concurrent_view {
                let entry = self.handles.decode(&request.object)?;
                let target = self.path_guard(&request.object).await?;
                let GuardedReadResult::Stat(stats) =
                    self.guarded_read(GuardedRead::Stat { target }).await?
                else {
                    return Err(FsError::new(ErrorCode::Eio));
                };
                self.check_handle_identity(&entry, &stats)?;
                Ok((fattr_of(&stats, entry.fileid), stats))
            } else {
                let path = self.path_of(&request.object).await?;
                self.attr_of(&path)
                    .await
                    .map(|(_, attr, stats)| (attr, stats))
            }
        }
        .await;
        match result {
            Ok((attr, stats)) => {
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
        let mut default_path = None;
        let result = async {
            if self.options.shared_concurrent_view {
                let entry = self.handles.decode(&handle)?;
                let target = self.path_guard(&handle).await?;
                let GuardedReadResult::Readlink { stats, target } =
                    self.guarded_read(GuardedRead::Readlink { target }).await?
                else {
                    return Err(FsError::new(ErrorCode::Eio));
                };
                self.check_handle_identity(&entry, &stats)?;
                Ok((target, Some(fattr_of(&stats, entry.fileid))))
            } else {
                let path = self.path_of(&handle).await?;
                default_path = Some(path.clone());
                let target = self.driver.readlink(&path).await?;
                Ok((target, self.post_op(Some(&path)).await))
            }
        }
        .await;
        match result {
            Ok((target, attributes)) => write_readlink_res(
                writer,
                &Readlink3res {
                    status: NFS3_OK,
                    attributes,
                    target: Some(target),
                },
            ),
            Err(error) => {
                self.record_error();
                write_readlink_res(
                    writer,
                    &Readlink3res {
                        status: Self::status(&error),
                        attributes: if self.options.shared_concurrent_view {
                            None
                        } else {
                            self.post_op(default_path.as_deref()).await
                        },
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
        let path = self.path_of_entry(&entry).await.ok();
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
                let attributes = if self.options.shared_concurrent_view {
                    None
                } else {
                    self.post_op(path.as_deref()).await
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
            self.check_opened_handle(&entry, handle.as_ref()).await?;
            let mut data = vec![0_u8; count];
            let read = handle.read(&mut data, Some(offset)).await;
            let read = read?;
            data.truncate(read);
            let retained_stats = if self.options.shared_concurrent_view {
                let stats = handle.stat().await?;
                self.check_handle_identity(&entry, &stats)?;
                Some(stats)
            } else {
                handle.stat().await.ok()
            };
            Ok::<(Vec<u8>, Option<mount_rs_core::Stats>), FsError>((data, retained_stats))
        }
        .await;
        match result {
            Ok((data, retained_stats)) => {
                let attributes = if self.options.shared_concurrent_view {
                    retained_stats
                        .as_ref()
                        .map(|stats| fattr_of(stats, entry.fileid))
                } else {
                    match path.as_deref() {
                        Some(path) => self.post_op(Some(path)).await,
                        None => retained_stats
                            .as_ref()
                            .map(|stats| fattr_of(stats, entry.fileid)),
                    }
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
                let attributes = if self.options.shared_concurrent_view {
                    None
                } else {
                    match path.as_deref() {
                        Some(path) => self.post_op(Some(path)).await,
                        None => None,
                    }
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
        let entry = self.handles.decode(&request.file).ok();
        let path = match entry.as_ref() {
            Some(entry) => self.path_of_entry(entry).await.ok(),
            None => None,
        };
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
                        wcc: self.wcc_opt(None, Some(&path)).await,
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
            if let Some(entry) = entry.as_ref()
                && let Err(error) = self.check_opened_handle(entry, handle.as_ref()).await
            {
                let _ = handle.close().await;
                return Err(error);
            }
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
                        wcc: self.wcc(before, &path).await,
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
        let path = self.path_of(&request.file).await.ok();
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
        expected: Option<PathIdentity>,
    ) {
        match self.attr_of(path).await {
            Ok((entry, attr, stats))
                if !self.options.shared_concurrent_view
                    || expected.is_some_and(|identity| {
                        PathIdentity::from_stats(&stats) == Some(identity)
                    }) =>
            {
                write_create_res(
                    writer,
                    &CreateRes {
                        status: NFS3_OK,
                        obj: Some(self.handles.encode(&entry)),
                        obj_attributes: Some(attr),
                        dir_wcc: self.wcc(before, directory).await,
                    },
                )
            }
            Ok(_) => {
                self.record_error();
                write_create_res(
                    writer,
                    &CreateRes {
                        status: NFS3ERR_STALE,
                        obj: None,
                        obj_attributes: None,
                        dir_wcc: self.wcc(before, directory).await,
                    },
                );
            }
            Err(error) => {
                self.record_error();
                write_create_res(
                    writer,
                    &CreateRes {
                        status: Self::status(&error),
                        obj: None,
                        obj_attributes: None,
                        dir_wcc: self.wcc(before, directory).await,
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
        let mut created_identity = None;
        let result = async {
            let parent_guard = if self.options.shared_concurrent_view {
                self.require_guarded_mutations()?;
                Some(self.path_guard(&request.where_.dir).await?)
            } else {
                None
            };
            let dir = match parent_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.where_.dir).await?,
            };
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
            let newly_created = match self
                .open_child(
                    parent_guard.as_ref(),
                    &request.where_.name,
                    &path,
                    ObservedEntry::Any,
                    create_flags,
                    mode,
                )
                .await
            {
                Ok((handle, identity)) => {
                    created_identity = identity;
                    if request.mode == CREATE_EXCLUSIVE {
                        let verifier = request
                            .verf
                            .clone()
                            .unwrap_or_else(|| vec![0; NFS3_CREATEVERFSIZE]);
                        self.exclusive_creates
                            .lock()
                            .expect("NFS exclusive-create lock")
                            .set(path.clone(), verifier, created_identity);
                    }
                    handle.close().await?;
                    true
                }
                Err(error) if error.code == ErrorCode::Eexist => false,
                Err(error) => return Err(error),
            };
            if newly_created {
                self.invalidate(&dir);
                if let Some(identity) = created_identity {
                    let target = PathGuard {
                        path: path.clone(),
                        identity,
                    };
                    self.claim_created_owner(&target, credentials, parent.as_ref(), false, mode)
                        .await?;
                    if request.mode != CREATE_EXCLUSIVE
                        && let Some(attributes) = request.attributes.as_ref()
                    {
                        self.apply_created_sattr(&target, attributes).await?;
                    }
                } else {
                    self.claim_owner(&path, credentials, parent.as_ref(), false, mode)
                        .await?;
                    if request.mode != CREATE_EXCLUSIVE
                        && let Some(attributes) = request.attributes.as_ref()
                    {
                        let current = self.stat_of(&path).await?;
                        self.apply_sattr(&path, attributes, &current).await?;
                    }
                }
                return Ok(path);
            }
            match request.mode {
                CREATE_GUARDED => Err(FsError::new(ErrorCode::Eexist)),
                CREATE_EXCLUSIVE => {
                    let verifier = request.verf.as_deref().unwrap_or(&[0; NFS3_CREATEVERFSIZE]);
                    let existing_identity = if self.options.shared_concurrent_view {
                        Some(
                            PathIdentity::from_stats(&self.stat_of(&path).await?)
                                .ok_or_else(|| FsError::new(ErrorCode::Estale))?,
                        )
                    } else {
                        None
                    };
                    let matches = self
                        .exclusive_creates
                        .lock()
                        .expect("NFS exclusive-create lock")
                        .matches(&path, verifier, existing_identity);
                    if matches {
                        created_identity = existing_identity;
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
                    let (handle, identity) = self
                        .open_child(
                            parent_guard.as_ref(),
                            &request.where_.name,
                            &path,
                            if parent_guard.is_some() {
                                self.observed_entry(&path).await?
                            } else {
                                ObservedEntry::Any
                            },
                            flags,
                            mode,
                        )
                        .await?;
                    created_identity = identity;
                    handle.close().await?;
                    if let Some(attributes) = request.attributes.as_ref() {
                        let mut attributes = attributes.clone();
                        attributes.mode = None;
                        if let Some(identity) = created_identity {
                            let target = PathGuard {
                                path: path.clone(),
                                identity,
                            };
                            self.apply_created_sattr(&target, &attributes).await?;
                        } else {
                            let current = self.stat_of(&path).await?;
                            self.apply_sattr(&path, &attributes, &current).await?;
                        }
                    }
                    Ok(path)
                }
            }
        }
        .await;
        match result {
            Ok(path) => {
                self.created_response(
                    writer,
                    directory.as_deref().unwrap_or("/"),
                    before,
                    &path,
                    created_identity,
                )
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
                        dir_wcc: self.wcc_opt(before, directory.as_deref()).await,
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
        let mut created_identity = None;
        let result = async {
            let parent_guard = if self.options.shared_concurrent_view {
                self.require_guarded_mutations()?;
                Some(self.path_guard(&request.where_.dir).await?)
            } else {
                None
            };
            let dir = match parent_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.where_.dir).await?,
            };
            Self::check_name(&request.where_.name)?;
            let path = Self::join_path(&dir, &request.where_.name);
            directory = Some(dir.clone());
            let parent = self.stat_of(&dir).await.ok();
            before = parent.as_ref().map(wcc_attr_of);
            let mode = request.attributes.mode.unwrap_or(0o777) & 0o7777;
            if let Some(parent_guard) = parent_guard {
                created_identity = match self
                    .guarded(GuardedMutation::Mkdir {
                        parent: parent_guard,
                        name: request.where_.name.clone(),
                        mode,
                    })
                    .await?
                {
                    GuardedMutationResult::Created(identity) => Some(identity),
                    _ => return Err(FsError::new(ErrorCode::Eio)),
                };
            } else {
                self.driver
                    .mkdir(
                        &path,
                        MkdirOptions {
                            recursive: false,
                            mode: Some(mode),
                        },
                    )
                    .await?;
            }
            self.invalidate(&dir);
            let mut attributes = request.attributes.clone();
            attributes.mode = None;
            if let Some(identity) = created_identity {
                let target = PathGuard {
                    path: path.clone(),
                    identity,
                };
                self.claim_created_owner(&target, credentials, parent.as_ref(), true, mode)
                    .await?;
                self.apply_created_sattr(&target, &attributes).await?;
            } else {
                self.claim_owner(&path, credentials, parent.as_ref(), true, mode)
                    .await?;
                let current = self.stat_of(&path).await?;
                self.apply_sattr(&path, &attributes, &current).await?;
            }
            Ok::<String, FsError>(path)
        }
        .await;
        match result {
            Ok(path) => {
                self.created_response(
                    writer,
                    directory.as_deref().unwrap_or("/"),
                    before,
                    &path,
                    created_identity,
                )
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
                        dir_wcc: self.wcc_opt(before, directory.as_deref()).await,
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
        let mut created_identity = None;
        let result = async {
            let parent_guard = if self.options.shared_concurrent_view {
                self.require_guarded_mutations()?;
                Some(self.path_guard(&request.where_.dir).await?)
            } else {
                None
            };
            let dir = match parent_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.where_.dir).await?,
            };
            Self::check_name(&request.where_.name)?;
            let path = Self::join_path(&dir, &request.where_.name);
            directory = Some(dir.clone());
            let parent = self.stat_of(&dir).await.ok();
            before = parent.as_ref().map(wcc_attr_of);
            if let Some(parent_guard) = parent_guard {
                created_identity = match self
                    .guarded(GuardedMutation::Symlink {
                        parent: parent_guard,
                        name: request.where_.name.clone(),
                        target: request.target.clone(),
                    })
                    .await?
                {
                    GuardedMutationResult::Created(identity) => Some(identity),
                    _ => return Err(FsError::new(ErrorCode::Eio)),
                };
            } else {
                self.driver.symlink(&request.target, &path).await?;
            }
            self.invalidate(&dir);
            // Symlink mode is fixed by POSIX; apply only timestamps/ownership.
            let mut attributes = request.attributes.clone();
            attributes.mode = None;
            attributes.size = None;
            if let Some(identity) = created_identity {
                let target = PathGuard {
                    path: path.clone(),
                    identity,
                };
                self.claim_created_owner(&target, credentials, parent.as_ref(), false, 0o777)
                    .await?;
                self.apply_created_sattr(&target, &attributes).await?;
            } else {
                self.claim_owner(&path, credentials, parent.as_ref(), false, 0o777)
                    .await?;
                let current = self.stat_of(&path).await?;
                self.apply_sattr(&path, &attributes, &current).await?;
            }
            Ok::<String, FsError>(path)
        }
        .await;
        match result {
            Ok(path) => {
                self.created_response(
                    writer,
                    directory.as_deref().unwrap_or("/"),
                    before,
                    &path,
                    created_identity,
                )
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
                        dir_wcc: self.wcc_opt(before, directory.as_deref()).await,
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
        let mut created_identity = None;
        let result = async {
            let parent_guard = if self.options.shared_concurrent_view {
                self.require_guarded_mutations()?;
                Some(self.path_guard(&request.where_.dir).await?)
            } else {
                None
            };
            let dir = match parent_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.where_.dir).await?,
            };
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
            if let Some(parent_guard) = parent_guard {
                created_identity = match self
                    .guarded(GuardedMutation::Mknod {
                        parent: parent_guard,
                        name: request.where_.name.clone(),
                        mode: kind | mode,
                        dev,
                    })
                    .await?
                {
                    GuardedMutationResult::Created(identity) => Some(identity),
                    _ => return Err(FsError::new(ErrorCode::Eio)),
                };
            } else {
                self.driver.mknod(&path, kind | mode, dev).await?;
            }
            self.invalidate(&dir);
            if let Some(identity) = created_identity {
                let target = PathGuard {
                    path: path.clone(),
                    identity,
                };
                self.claim_created_owner(&target, credentials, parent.as_ref(), false, mode)
                    .await?;
            } else {
                self.claim_owner(&path, credentials, parent.as_ref(), false, mode)
                    .await?;
            }
            Ok::<String, FsError>(path)
        }
        .await;
        match result {
            Ok(path) => {
                self.created_response(
                    writer,
                    directory.as_deref().unwrap_or("/"),
                    before,
                    &path,
                    created_identity,
                )
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
                        dir_wcc: self.wcc_opt(before, directory.as_deref()).await,
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
            let parent_guard = if self.options.shared_concurrent_view {
                self.require_guarded_mutations()?;
                Some(self.path_guard(&request.dir).await?)
            } else {
                None
            };
            let parent = match parent_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.dir).await?,
            };
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
            if let Some(parent_guard) = parent_guard {
                let entry = self.observed_entry(&path).await?;
                let mutation = if directory {
                    GuardedMutation::Rmdir {
                        parent: parent_guard,
                        name: request.name.clone(),
                        entry,
                    }
                } else {
                    GuardedMutation::Unlink {
                        parent: parent_guard,
                        name: request.name.clone(),
                        entry,
                    }
                };
                self.guarded(mutation).await?;
            } else if directory {
                self.driver.rmdir(&path).await?;
            } else {
                self.driver.unlink(&path).await?;
            }
            if !self.options.shared_concurrent_view {
                if directory {
                    self.handles.forget(&path);
                } else {
                    self.handles.orphan(&path);
                }
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
                        wcc: self.wcc_opt(before, dir.as_deref()).await,
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
            let from_guard = if self.options.shared_concurrent_view {
                self.require_guarded_mutations()?;
                Some(self.path_guard(&request.from.dir).await?)
            } else {
                None
            };
            let to_guard = if self.options.shared_concurrent_view {
                Some(self.path_guard(&request.to.dir).await?)
            } else {
                None
            };
            let from_parent = match from_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.from.dir).await?,
            };
            let to_parent = match to_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.to.dir).await?,
            };
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
            let same_inode = if self.options.shared_concurrent_view {
                true
            } else if from == to {
                self.stat_of(&from).await.is_ok()
            } else {
                match (self.stat_of(&from).await, self.stat_of(&to).await) {
                    (Ok(source), Ok(destination)) => same_backend_inode(&source, &destination),
                    _ => false,
                }
            };
            if let (Some(from_parent), Some(to_parent)) = (from_guard, to_guard) {
                let source = self.observed_entry(&from).await?;
                let destination = self.observed_entry(&to).await?;
                self.guarded(GuardedMutation::Rename {
                    from_parent,
                    from_name: request.from.name.clone(),
                    source,
                    to_parent,
                    to_name: request.to.name.clone(),
                    destination,
                })
                .await?;
            } else {
                // NFSv3 RENAME of two hard links to one inode is a no-op. In
                // particular, a Windows host rename can remove the source
                // alias even though the backend inode is unchanged.
                if !same_inode {
                    self.driver.rename(&from, &to).await?;
                }
            }
            if !same_inode && !self.options.shared_concurrent_view {
                self.handles.remap(&from, &to);
                self.exclusive_creates
                    .lock()
                    .expect("NFS exclusive-create lock")
                    .forget(&from);
                self.exclusive_creates
                    .lock()
                    .expect("NFS exclusive-create lock")
                    .forget(&to);
            }
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
                        from_wcc: self.wcc_opt(from_before, from_dir.as_deref()).await,
                        to_wcc: self.wcc_opt(to_before, to_dir.as_deref()).await,
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
            let source_guard = if self.options.shared_concurrent_view {
                self.require_guarded_mutations()?;
                Some(self.path_guard(&request.file).await?)
            } else {
                None
            };
            let parent_guard = if self.options.shared_concurrent_view {
                Some(self.path_guard(&request.link.dir).await?)
            } else {
                None
            };
            let source = match source_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.file).await?,
            };
            let parent = match parent_guard.as_ref() {
                Some(guard) => guard.path.clone(),
                None => self.path_of(&request.link.dir).await?,
            };
            Self::check_name(&request.link.name)?;
            let path = Self::join_path(&parent, &request.link.name);
            file = Some(source.clone());
            directory = Some(parent.clone());
            before = self.pre_op(&parent).await;
            if let (Some(source_guard), Some(parent_guard)) = (source_guard, parent_guard) {
                let destination = self.observed_entry(&path).await?;
                self.guarded(GuardedMutation::Link {
                    source: source_guard,
                    to_parent: parent_guard,
                    to_name: request.link.name.clone(),
                    destination,
                })
                .await?;
            } else {
                self.driver.link(&source, &path).await?;
            }
            self.invalidate(&parent);
            if !self.options.shared_concurrent_view {
                self.attr_of(&path).await?;
            }
            Ok::<(), FsError>(())
        }
        .await;
        match result {
            Ok(()) => write_link_res(
                writer,
                &Link3res {
                    status: NFS3_OK,
                    attributes: if self.options.shared_concurrent_view {
                        None
                    } else {
                        self.post_op(file.as_deref()).await
                    },
                    linkdir_wcc: self.wcc(before, directory.as_deref().unwrap_or("/")).await,
                },
            ),
            Err(error) => {
                self.record_error();
                write_link_res(
                    writer,
                    &Link3res {
                        status: Self::status(&error),
                        attributes: if self.options.shared_concurrent_view {
                            None
                        } else {
                            self.post_op(file.as_deref()).await
                        },
                        linkdir_wcc: self.wcc_opt(before, directory.as_deref()).await,
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
    ) -> FsResult<(
        DirectorySnapshot,
        usize,
        Option<(mount_rs_core::Stats, Vec<GuardedDirectoryEntry>)>,
    )> {
        if self.options.shared_concurrent_view {
            let identity = if entry.id == crate::handles::ROOT_HANDLE_ID {
                PathIdentity::from_stats(&self.stat_of(path).await?)
            } else {
                entry
                    .key
                    .as_deref()
                    .and_then(PathIdentity::parse_backend_key)
            }
            .ok_or_else(|| FsError::new(ErrorCode::Estale))?;
            let guarded = self
                .guarded_read(GuardedRead::Readdir {
                    directory: PathGuard {
                        path: path.to_owned(),
                        identity,
                    },
                    max_entries: usize::MAX,
                })
                .await?;
            let GuardedReadResult::Directory { stats, entries } = guarded else {
                return Err(FsError::new(ErrorCode::Eproto)
                    .with_message("guarded readdir returned the wrong result"));
            };
            if PathIdentity::from_stats(&stats) != Some(identity)
                || entries
                    .iter()
                    .any(|child| PathIdentity::from_stats(&child.stats).is_none())
            {
                return Err(FsError::new(ErrorCode::Estale)
                    .with_message("shared directory snapshot has an unknown inode"));
            }
            for child in &entries {
                Self::check_name(&child.name).map_err(|_| {
                    FsError::new(ErrorCode::Eproto)
                        .with_message("guarded readdir returned an invalid child name")
                })?;
            }
            let verifier = guarded_cookie_verifier(&stats, &entries);
            if cookie != 0 && !same_verifier(&verifier, cookieverf) {
                return Err(FsError::new(ErrorCode::Einval)
                    .with_message("NFS3ERR_BAD_COOKIE: directory verifier mismatch"));
            }
            let names = entries.iter().map(|child| child.name.clone()).collect();
            let snapshot = DirectorySnapshot { names, verifier };
            let from = usize::try_from(cookie).map_err(|_| FsError::new(ErrorCode::Einval))?;
            if from > snapshot.names.len() {
                return Err(FsError::new(ErrorCode::Einval)
                    .with_message("NFS3ERR_BAD_COOKIE: cookie is past the end"));
            }
            return Ok((snapshot, from, Some((stats, entries))));
        }
        if cookie == 0 {
            let entries = self.driver.readdir(path).await?;
            let names = entries.into_iter().map(|entry| entry.name).collect();
            return Ok((self.snapshots.set(entry.id, names), 0, None));
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
        Ok((snapshot, from, None))
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
            let path_result = self.path_of(&request.dir).await;
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
                Ok((snapshot, from, shared)) => {
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
                                dir_attributes: if let Some((stats, _)) = &shared {
                                    Some(fattr_of(stats, entry.fileid))
                                } else {
                                    self.post_op(Some(&path)).await
                                },
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
                        let described = if let Some((_, children)) = &shared {
                            self.describe_stats(&child, children[index].stats.clone())
                                .await
                                .ok()
                                .map(|(child_entry, attr, _)| (child_entry, attr))
                        } else {
                            self.attr_of(&child)
                                .await
                                .ok()
                                .map(|(child_entry, attr, _)| (child_entry, attr))
                        };
                        let snapshot_attr = shared.as_ref().map(|(_, children)| {
                            fattr_of(&children[index].stats, children[index].stats.ino)
                        });
                        let fileid = snapshot_attr
                            .as_ref()
                            .map(|attr| attr.fileid)
                            .or_else(|| described.as_ref().map(|(_, attr)| attr.fileid))
                            .unwrap_or(0);
                        entries.push(EntryPlus3 {
                            fileid,
                            name,
                            cookie: (index + 1) as u64,
                            attributes: snapshot_attr
                                .or_else(|| described.as_ref().map(|(_, attr)| attr.clone())),
                            handle: described
                                .as_ref()
                                .map(|(entry, _)| self.handles.encode(entry)),
                        });
                    }
                    write_readdirplus_res(
                        writer,
                        &Readdirplus3res {
                            status: NFS3_OK,
                            dir_attributes: if let Some((stats, _)) = &shared {
                                Some(fattr_of(stats, entry.fileid))
                            } else {
                                self.post_op(Some(&path)).await
                            },
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
                            dir_attributes: if self.options.shared_concurrent_view {
                                None
                            } else {
                                self.post_op(Some(&path)).await
                            },
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
            let path_result = self.path_of(&request.dir).await;
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
                Ok((snapshot, from, shared)) => {
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
                                dir_attributes: if let Some((stats, _)) = &shared {
                                    Some(fattr_of(stats, entry.fileid))
                                } else {
                                    self.post_op(Some(&path)).await
                                },
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
                        let fileid = if let Some((_, children)) = &shared {
                            self.handles.bind(&child, &children[index].stats).fileid
                        } else {
                            match self.handles.at(&child) {
                                Some(entry) => entry.fileid,
                                None => self
                                    .stat_of(&child)
                                    .await
                                    .map(|stats| self.handles.bind(&child, &stats).fileid)
                                    .unwrap_or(0),
                            }
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
                            dir_attributes: if let Some((stats, _)) = &shared {
                                Some(fattr_of(stats, entry.fileid))
                            } else {
                                self.post_op(Some(&path)).await
                            },
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
                            dir_attributes: if self.options.shared_concurrent_view {
                                None
                            } else {
                                self.post_op(Some(&path)).await
                            },
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
        match self.path_of(&handle).await {
            Ok(path) => match self.driver.statfs(&path).await {
                Ok(stats) => {
                    let block_size = stats.block_size.max(4096);
                    write_fsstat_res(
                        writer,
                        &Fsstat3res {
                            status: NFS3_OK,
                            attributes: if self.options.shared_concurrent_view {
                                None
                            } else {
                                self.post_op(Some(&path)).await
                            },
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
                            attributes: if self.options.shared_concurrent_view {
                                None
                            } else {
                                self.post_op(Some(&path)).await
                            },
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
        match self.path_of(&handle).await {
            Ok(path) => {
                let capabilities = self.driver.capabilities;
                write_fsinfo_res(
                    writer,
                    &Fsinfo3res {
                        status: NFS3_OK,
                        attributes: if self.options.shared_concurrent_view {
                            None
                        } else {
                            self.post_op(Some(&path)).await
                        },
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
        match self.path_of(&handle).await {
            Ok(path) => {
                let capabilities = self.driver.capabilities;
                write_pathconf_res(
                    writer,
                    &Pathconf3res {
                        status: NFS3_OK,
                        attributes: if self.options.shared_concurrent_view {
                            None
                        } else {
                            self.post_op(Some(&path)).await
                        },
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
                            ErrorCode::Enotsup => MNT3ERR_NOTSUPP,
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
    use mount_rs_memfs::{MemoryFs, MemoryOptions};

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
        assert_eq!(reply.auth_stat, Some(crate::rpc::AUTH_TOOWEAK));
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
