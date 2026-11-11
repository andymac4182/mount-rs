//! Mount-free Rust-backed FUSE session bindings.
//!
//! This exposes the protocol/session boundary for deterministic tests and
//! embedders. It intentionally does not open `/dev/fuse`, mount a filesystem,
//! or claim native kernel lifecycle parity.

use crate::{
    Filesystem,
    fuse_codec::{NativeFuseProtocolContext, invalid_argument, protocol_error, usize_value},
    serial,
};
use mount_rs_core::FsError;
use mount_rs_fuse::constants::{
    FUSE_ATOMIC_O_TRUNC, FUSE_CACHE_SYMLINKS, FUSE_DO_READDIRPLUS, FUSE_EXPORT_SUPPORT,
    FUSE_FLOCK_LOCKS, FUSE_PARALLEL_DIROPS, FUSE_POSIX_LOCKS, FUSE_SETXATTR_EXT,
    FUSE_WRITEBACK_CACHE,
};
use mount_rs_fuse::session::{
    FuseFlushMechanism, FuseSession as RustFuseSession, FuseSessionOptions,
};
use napi::bindgen_prelude::{BigInt, Buffer, Env, PromiseRaw, ToNapiValue};
use napi_derive::napi;
use tokio::sync::{mpsc, oneshot};

#[napi(object)]
pub struct NativeFuseInitPreferences {
    pub minor: Option<f64>,
    #[napi(js_name = "maxWrite")]
    pub max_write: Option<f64>,
    #[napi(js_name = "maxReadahead")]
    pub max_readahead: Option<f64>,
    #[napi(js_name = "maxBackground")]
    pub max_background: Option<f64>,
    #[napi(js_name = "congestionThreshold")]
    pub congestion_threshold: Option<f64>,
    #[napi(js_name = "timeGran")]
    pub time_gran: Option<f64>,
    #[napi(js_name = "maxStackDepth")]
    pub max_stack_depth: Option<f64>,
    pub flags: Option<BigInt>,
    #[napi(js_name = "extraFlags")]
    pub extra_flags: Option<BigInt>,
    #[napi(js_name = "withoutFlags")]
    pub without_flags: Option<BigInt>,
    #[napi(js_name = "readdirplus")]
    pub readdirplus: Option<bool>,
    #[napi(js_name = "writebackCache")]
    pub writeback_cache: Option<bool>,
    #[napi(js_name = "cacheSymlinks")]
    pub cache_symlinks: Option<bool>,
}

#[napi(object)]
pub struct NativeFuseSessionOptions {
    #[napi(js_name = "maxRequest")]
    pub max_request: Option<f64>,
    #[napi(js_name = "useDriverIno")]
    pub use_driver_ino: Option<bool>,
    #[napi(js_name = "attrTimeout")]
    pub attr_timeout: Option<f64>,
    #[napi(js_name = "entryTimeout")]
    pub entry_timeout: Option<f64>,
    #[napi(js_name = "negativeTimeout")]
    pub negative_timeout: Option<f64>,
    #[napi(js_name = "keepCache")]
    pub keep_cache: Option<bool>,
    #[napi(js_name = "flushMechanism")]
    pub flush_mechanism: Option<String>,
    pub init: Option<NativeFuseInitPreferences>,
}

#[napi(object)]
pub struct NativeFuseSessionError {
    pub code: String,
    pub errno: i32,
    pub syscall: Option<String>,
    pub path: Option<String>,
    pub dest: Option<String>,
    pub message: String,
}

impl From<FsError> for NativeFuseSessionError {
    fn from(error: FsError) -> Self {
        let message = error.to_string();
        Self {
            code: error.code.as_str().to_owned(),
            errno: -error.code.errno(),
            syscall: error.syscall,
            path: error.path,
            dest: error.dest,
            message,
        }
    }
}

#[napi(object)]
pub struct NativeFuseSessionObservation {
    pub reply: Option<Buffer>,
    pub error: Option<NativeFuseSessionError>,
}

#[napi(object)]
pub struct NativeFuseNegotiatedSession {
    pub major: u32,
    pub minor: u32,
    pub flags: BigInt,
    #[napi(js_name = "maxWrite")]
    pub max_write: u32,
    #[napi(js_name = "maxPages")]
    pub max_pages: u16,
    #[napi(js_name = "maxReadahead")]
    pub max_readahead: u32,
    #[napi(js_name = "maxBackground")]
    pub max_background: u16,
    #[napi(js_name = "congestionThreshold")]
    pub congestion_threshold: u16,
    #[napi(js_name = "timeGran")]
    pub time_gran: u32,
    #[napi(js_name = "readdirplus")]
    pub readdirplus: bool,
    #[napi(js_name = "writebackCache")]
    pub writeback_cache: bool,
    #[napi(js_name = "atomicOTrunc")]
    pub atomic_o_trunc: bool,
    #[napi(js_name = "parallelDirops")]
    pub parallel_dirops: bool,
    #[napi(js_name = "posixLocks")]
    pub posix_locks: bool,
    #[napi(js_name = "flockLocks")]
    pub flock_locks: bool,
    #[napi(js_name = "cacheSymlinks")]
    pub cache_symlinks: bool,
    #[napi(js_name = "exportSupport")]
    pub export_support: bool,
    #[napi(js_name = "setxattrExt")]
    pub setxattr_ext: bool,
    #[napi(js_name = "maxStackDepth")]
    pub max_stack_depth: u32,
    pub protocol: NativeFuseProtocolContext,
}

#[napi(object)]
pub struct NativeFuseSessionInode {
    pub nodeid: BigInt,
    pub key: Option<String>,
    pub nlookup: BigInt,
    pub paths: Vec<String>,
}

#[napi(object)]
pub struct NativeFuseSessionState {
    pub destroyed: bool,
    #[napi(js_name = "openHandles")]
    pub open_handles: u32,
    pub negotiated: Option<NativeFuseNegotiatedSession>,
    pub inodes: Vec<NativeFuseSessionInode>,
}

fn negotiated_state(reply: &mount_rs_fuse::init::InitReply) -> NativeFuseNegotiatedSession {
    let flags = reply.flags;
    let setxattr_ext = flags & FUSE_SETXATTR_EXT != 0;
    NativeFuseNegotiatedSession {
        major: reply.major,
        minor: reply.minor,
        flags: BigInt::from(flags),
        max_write: reply.max_write,
        max_pages: reply.max_pages,
        max_readahead: reply.max_readahead,
        max_background: reply.max_background,
        congestion_threshold: reply.congestion_threshold,
        time_gran: reply.time_gran,
        readdirplus: flags & FUSE_DO_READDIRPLUS != 0,
        writeback_cache: flags & FUSE_WRITEBACK_CACHE != 0,
        atomic_o_trunc: flags & FUSE_ATOMIC_O_TRUNC != 0,
        parallel_dirops: flags & FUSE_PARALLEL_DIROPS != 0,
        posix_locks: flags & FUSE_POSIX_LOCKS != 0,
        flock_locks: flags & FUSE_FLOCK_LOCKS != 0,
        cache_symlinks: flags & FUSE_CACHE_SYMLINKS != 0,
        export_support: flags & FUSE_EXPORT_SUPPORT != 0,
        setxattr_ext,
        max_stack_depth: reply.max_stack_depth,
        protocol: NativeFuseProtocolContext {
            minor: reply.minor,
            setxattr_ext,
        },
    }
}

fn session_state(session: &RustFuseSession) -> NativeFuseSessionState {
    let mut inodes: Vec<_> = session.inodes.entries().collect();
    inodes.sort_by_key(|inode| inode.nodeid);
    NativeFuseSessionState {
        destroyed: session.is_destroyed(),
        open_handles: u32::try_from(session.open_handles()).unwrap_or(u32::MAX),
        negotiated: session.negotiated.as_ref().map(negotiated_state),
        inodes: inodes
            .into_iter()
            .map(|inode| NativeFuseSessionInode {
                nodeid: BigInt::from(inode.nodeid),
                key: inode.key.map(|(dev, ino)| format!("{dev}:{ino}")),
                nlookup: BigInt::from(inode.nlookup),
                paths: inode.paths.clone(),
            })
            .collect(),
    }
}

const MAX_SAFE_SECONDS: f64 = 9_007_199_254_740_991.0;

fn timeout_value(
    name: &str,
    value: Option<f64>,
    default: f64,
) -> napi::Result<std::time::Duration> {
    let value = value.unwrap_or(default);
    if !value.is_finite() || !(0.0..=MAX_SAFE_SECONDS).contains(&value) {
        return Err(invalid_argument(format!(
            "{name} must be a finite non-negative number up to {MAX_SAFE_SECONDS} seconds"
        )));
    }
    let whole = value.floor();
    let mut seconds = whole as u64;
    let mut nanos = ((value - whole) * 1_000_000_000.0).round() as u64;
    if nanos == 1_000_000_000 {
        seconds = seconds
            .checked_add(1)
            .ok_or_else(|| invalid_argument(format!("{name} is too large")))?;
        nanos = 0;
    }
    Ok(std::time::Duration::new(seconds, nanos as u32))
}

fn u32_value(name: &str, value: Option<f64>) -> napi::Result<Option<u32>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=u32::MAX as f64).contains(&value) {
        return Err(invalid_argument(format!(
            "{name} must be an integer between 0 and {}",
            u32::MAX
        )));
    }
    Ok(Some(value as u32))
}

fn u16_value(name: &str, value: Option<f64>) -> napi::Result<Option<u16>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !value.is_finite() || value.fract() != 0.0 || !(0.0..=u16::MAX as f64).contains(&value) {
        return Err(invalid_argument(format!(
            "{name} must be an integer between 0 and {}",
            u16::MAX
        )));
    }
    Ok(Some(value as u16))
}

fn u64_from_bigint(value: &BigInt) -> u64 {
    let (negative, magnitude, _) = value.get_u128();
    let low = magnitude as u64;
    if negative {
        0u64.wrapping_sub(low)
    } else {
        low
    }
}

fn init_preferences(
    options: Option<NativeFuseInitPreferences>,
) -> napi::Result<mount_rs_fuse::init::Preferences> {
    let Some(options) = options else {
        return Ok(FuseSessionOptions::default().init);
    };
    // The generic INIT codec defaults intentionally expose the broad protocol
    // negotiation surface. A serialized mount-free session must start from the
    // same conservative policy as the native dispatcher, however: it does not
    // provide asynchronous direct I/O, parallel directory dispatch, or the
    // SETXATTR extension. Callers may still opt into explicit flags through
    // `flags`/`extraFlags` when they own the corresponding implementation.
    let mut preferences = FuseSessionOptions::default().init;
    if let Some(value) = u32_value("init.minor", options.minor)? {
        preferences.minor = value;
    }
    if let Some(value) = u32_value("init.maxWrite", options.max_write)? {
        preferences.max_write = value;
    }
    preferences.max_readahead = u32_value("init.maxReadahead", options.max_readahead)?;
    if let Some(value) = u16_value("init.maxBackground", options.max_background)? {
        preferences.max_background = value;
    }
    preferences.congestion_threshold =
        u16_value("init.congestionThreshold", options.congestion_threshold)?;
    if let Some(value) = u32_value("init.timeGran", options.time_gran)? {
        preferences.time_gran = value;
    }
    if let Some(value) = u32_value("init.maxStackDepth", options.max_stack_depth)? {
        preferences.max_stack_depth = value;
    }

    let mut flags = options
        .flags
        .as_ref()
        .map(u64_from_bigint)
        .unwrap_or(preferences.flags);
    let readdir_flags = mount_rs_fuse::constants::FUSE_DO_READDIRPLUS
        | mount_rs_fuse::constants::FUSE_READDIRPLUS_AUTO;
    if let Some(enabled) = options.readdirplus {
        if enabled {
            flags |= readdir_flags;
        } else {
            flags &= !readdir_flags;
        }
    }
    if let Some(enabled) = options.writeback_cache {
        if enabled {
            flags |= mount_rs_fuse::constants::FUSE_WRITEBACK_CACHE;
        } else {
            flags &= !mount_rs_fuse::constants::FUSE_WRITEBACK_CACHE;
        }
    }
    if let Some(enabled) = options.cache_symlinks {
        if enabled {
            flags |= mount_rs_fuse::constants::FUSE_CACHE_SYMLINKS;
        } else {
            flags &= !mount_rs_fuse::constants::FUSE_CACHE_SYMLINKS;
        }
    }
    if let Some(value) = options.extra_flags {
        flags |= u64_from_bigint(&value);
    }
    if let Some(value) = options.without_flags {
        flags &= !u64_from_bigint(&value);
    }
    preferences.flags = flags;
    Ok(preferences)
}

fn session_options(options: Option<NativeFuseSessionOptions>) -> napi::Result<FuseSessionOptions> {
    let options = options.unwrap_or(NativeFuseSessionOptions {
        max_request: None,
        use_driver_ino: None,
        attr_timeout: None,
        entry_timeout: None,
        negative_timeout: None,
        keep_cache: None,
        flush_mechanism: None,
        init: None,
    });
    let max_request = usize_value("maxRequest", options.max_request, 1024 * 1024)?;
    if max_request == 0 {
        return Err(invalid_argument("maxRequest must be greater than 0"));
    }
    let attr_timeout = timeout_value("attrTimeout", options.attr_timeout, 10.0)?;
    let entry_timeout = timeout_value("entryTimeout", options.entry_timeout, 10.0)?;
    let negative_timeout = timeout_value("negativeTimeout", options.negative_timeout, 0.0)?;
    let init = init_preferences(options.init)?;
    let flush_mechanism = match options.flush_mechanism.as_deref().unwrap_or("sync") {
        "sync" => FuseFlushMechanism::Sync,
        "enosys" => FuseFlushMechanism::Enosys,
        "noflush" => FuseFlushMechanism::Noflush,
        value => {
            return Err(invalid_argument(format!(
                "flushMechanism must be \"sync\", \"enosys\", or \"noflush\", got \"{value}\""
            )));
        }
    };
    Ok(FuseSessionOptions {
        max_request,
        use_driver_ino: options.use_driver_ino.unwrap_or(true),
        attr_timeout,
        entry_timeout,
        negative_timeout,
        keep_cache: options.keep_cache.unwrap_or(true),
        direct_io: false,
        flush_mechanism,
        init,
    })
}

/// The reply to one frame and the structured error behind a negative reply.
type HandleReply = (napi::Result<Option<Vec<u8>>>, Option<FsError>);

/// One call on the session, with the channel its answer goes back through.
enum SessionRequest {
    Handle(Buffer, oneshot::Sender<HandleReply>),
    State(oneshot::Sender<NativeFuseSessionState>),
    Destroy(oneshot::Sender<()>),
}

impl SessionRequest {
    /// The caller is gone (its promise could not be made, or the runtime is
    /// being torn down), so the request is not run.
    fn abandoned(&self) -> bool {
        match self {
            Self::Handle(_, reply) => reply.is_closed(),
            Self::State(reply) => reply.is_closed(),
            Self::Destroy(reply) => reply.is_closed(),
        }
    }
}

/// The task that owns the session. Requests are sent to it synchronously
/// from the JavaScript thread, so it receives them in the order JavaScript
/// made the calls and runs them one at a time in that order. The structured
/// error of a frame is read before the next request is received, so it is
/// always the error of that frame. The task ends, dropping the session, when
/// the JavaScript object is collected.
async fn own_session(
    mut session: RustFuseSession,
    mut requests: mpsc::UnboundedReceiver<SessionRequest>,
) {
    while let Some(request) = requests.recv().await {
        if request.abandoned() {
            continue;
        }
        // A request that panics answers nobody: its reply channel is dropped
        // and its caller sees the request as cancelled. The session stays
        // with this task for the requests behind it.
        let _ = serial::caught(async {
            match request {
                SessionRequest::Handle(bytes, reply) => {
                    let result = session.handle(bytes.as_ref()).await;
                    let error = session.take_last_error();
                    let _ = reply.send((result.map_err(protocol_error), error));
                }
                SessionRequest::State(reply) => {
                    let _ = reply.send(session_state(&session));
                }
                SessionRequest::Destroy(reply) => {
                    session.destroy().await;
                    let _ = reply.send(());
                }
            }
        })
        .await;
    }
}

/// A serialized FUSE request/reply session backed by a mount-rs filesystem
/// driver. One task owns the Rust session; each call is sent to it at the
/// moment JavaScript makes the call, and the calls run one at a time in that
/// order, preserving the Rust transport's inode and open-handle state across
/// calls.
#[napi]
pub struct FuseSession {
    requests: mpsc::UnboundedSender<SessionRequest>,
}

impl FuseSession {
    /// Send a request to the owner and return the promise for its answer.
    fn call<R, V>(
        &self,
        env: Env,
        request: impl FnOnce(oneshot::Sender<R>) -> SessionRequest,
        settle: impl FnOnce(R) -> napi::Result<V> + Send + 'static,
    ) -> napi::Result<PromiseRaw<'static, V>>
    where
        R: Send + 'static,
        V: ToNapiValue + Send + 'static,
    {
        let (reply, answer) = oneshot::channel();
        // A send only fails once the owner is gone, which drops the reply
        // channel and so reports the request as cancelled.
        let _ = self.requests.send(request(reply));
        serial::promise(env, async move { answer.await.ok() }, settle)
    }
}

#[napi]
impl FuseSession {
    #[napi(constructor)]
    pub fn new(
        filesystem: &Filesystem,
        options: Option<NativeFuseSessionOptions>,
    ) -> napi::Result<Self> {
        let session =
            RustFuseSession::with_options(filesystem.driver()?, session_options(options)?);
        let (requests, inbox) = mpsc::unbounded_channel();
        napi::bindgen_prelude::spawn(own_session(session, inbox));
        Ok(Self { requests })
    }

    /// Handle one complete FUSE request frame. `null` is returned for
    /// protocol no-reply operations such as FORGET and BATCH_FORGET.
    #[napi(ts_return_type = "Promise<Buffer | null>")]
    pub fn handle(
        &self,
        env: Env,
        bytes: Buffer,
    ) -> napi::Result<PromiseRaw<'static, Option<Buffer>>> {
        self.call(
            env,
            |reply| SessionRequest::Handle(bytes, reply),
            |(result, _error): HandleReply| result.map(|reply| reply.map(Buffer::from)),
        )
    }

    /// Internal observed form used by the JavaScript facade to preserve the
    /// structured error that produced a negative FUSE reply.
    #[napi(
        js_name = "__handleObserved",
        ts_return_type = "Promise<NativeFuseSessionObservation>"
    )]
    pub fn handle_observed(
        &self,
        env: Env,
        bytes: Buffer,
    ) -> napi::Result<PromiseRaw<'static, NativeFuseSessionObservation>> {
        self.call(
            env,
            |reply| SessionRequest::Handle(bytes, reply),
            |(result, error): HandleReply| {
                result.map(|reply| NativeFuseSessionObservation {
                    reply: reply.map(Buffer::from),
                    error: error.map(Into::into),
                })
            },
        )
    }

    /// Internal lifecycle readback used by the JavaScript facade's public
    /// session getters. It takes its turn with `handle`, so it observes the
    /// session between requests.
    #[napi(
        js_name = "__state",
        ts_return_type = "Promise<NativeFuseSessionState>"
    )]
    pub fn state(&self, env: Env) -> napi::Result<PromiseRaw<'static, NativeFuseSessionState>> {
        self.call(env, SessionRequest::State, Ok)
    }

    /// Release all session-owned handles and inode state. This remains
    /// separate from `Filesystem.shutdown()`, which owns the provider.
    #[napi(ts_return_type = "Promise<void>")]
    pub fn destroy(&self, env: Env) -> napi::Result<PromiseRaw<'static, ()>> {
        self.call(env, SessionRequest::Destroy, Ok)
    }
}
