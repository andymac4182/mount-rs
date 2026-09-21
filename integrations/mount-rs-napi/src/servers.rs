//! N-API adapters for the transport servers.
//!
//! The transport crates own framing, sessions, request dispatch, and socket
//! teardown. This module only translates JavaScript option bags and keeps the
//! Filesystem driver alive while exposing the small lifecycle surface that
//! Node callers need.

use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mount_rs_9p::{
    P9LockTable, P9Server as TransportP9Server, P9ServerHooks as TransportP9ServerHooks,
    P9ServerOptions as TransportP9ServerOptions, P9TransportError as TransportP9Error,
    P9TransportErrorHook as TransportP9ErrorHook,
};
use mount_rs_core::{ErrorCode, FsDriver, FsError};
use mount_rs_nfs::{
    FileHandleTable as TransportNfsHandleTable, NFS_V4, NFS4_PROGRAM,
    Nfs3Session as TransportNfsSession, Nfs4Session as TransportNfs4Session,
    NfsRequestContext as TransportNfsRequestContext, NfsServer as TransportNfsServer,
    NfsServerHooks as TransportNfsServerHooks, NfsServerOptions as TransportNfsServerOptions,
    NfsTransportError as TransportNfsError, NfsTransportErrorHook as TransportNfsErrorHook,
};
use mount_rs_s3::{
    Credentials as TransportS3Credentials, S3Server as TransportS3Server,
    S3ServerOptions as TransportS3ServerOptions, S3Session, S3SessionOptions,
};
use mount_rs_webdav::{
    WebdavServer as TransportWebdavServer, WebdavServerOptions as TransportWebdavServerOptions,
};
use napi::bindgen_prelude::{
    BigInt, Buffer, Either, FnArgs, Function, JsObjectValue, Object, Reference, Unknown,
};
use napi::threadsafe_function::{ThreadsafeFunction, ThreadsafeFunctionCallMode};
use napi::{Error, Status, sys};
use napi_derive::napi;

use super::{Filesystem, MountDriver};

const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

type JsTransportErrorCallback = Function<'static, Unknown<'static>, Unknown<'static>>;
type TransportErrorCall = FnArgs<(Error, Option<String>)>;
type TransportErrorTsfn = ThreadsafeFunction<
    TransportErrorEvent,
    Unknown<'static>,
    TransportErrorCall,
    Status,
    false,
    false,
>;

#[derive(Clone)]
struct TransportErrorEvent {
    message: String,
    peer: Option<String>,
}

impl From<TransportNfsError> for TransportErrorEvent {
    fn from(error: TransportNfsError) -> Self {
        let TransportNfsError { message, peer, .. } = error;
        Self { message, peer }
    }
}

impl From<TransportP9Error> for TransportErrorEvent {
    fn from(error: TransportP9Error) -> Self {
        let TransportP9Error { message, peer, .. } = error;
        Self { message, peer }
    }
}

/// Owns the JavaScript callback independently from the transport hook.
///
/// The Rust transports retain their hook closure until their server is dropped,
/// so closing a Node server must explicitly abort and remove the TSFN rather
/// than merely dropping the N-API wrapper. The closed flag also makes a hook
/// callback racing with shutdown a no-op; the TSFN's aborted lock closes the
/// remaining call-versus-release race.
struct TransportErrorCallback {
    callback: Mutex<Option<Arc<TransportErrorTsfn>>>,
    closed: AtomicBool,
}

impl TransportErrorCallback {
    fn new(function: JsTransportErrorCallback) -> napi::Result<Arc<Self>> {
        let callback = function
            .build_threadsafe_function::<TransportErrorEvent>()
            .weak::<false>()
            .callee_handled::<false>()
            .build_callback(|context| {
                let event = context.value;
                Ok(FnArgs::from((
                    Error::new(Status::GenericFailure, event.message),
                    event.peer,
                )))
            })?;
        Ok(Arc::new(Self {
            callback: Mutex::new(Some(Arc::new(callback))),
            closed: AtomicBool::new(false),
        }))
    }

    fn report(&self, event: TransportErrorEvent) {
        if self.closed.load(Ordering::Acquire) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(callback) => callback.as_ref().cloned(),
            Err(poisoned) => poisoned.into_inner().as_ref().cloned(),
        };
        let Some(callback) = callback else {
            return;
        };

        // This is a notification, not a transport operation that can carry a
        // JavaScript rejection. `call_with_return_value` captures a synchronous
        // throw so it cannot become an uncaught exception or abort the process;
        // the transport event itself remains delivered exactly once.
        let _ = callback.call_with_return_value(
            event,
            ThreadsafeFunctionCallMode::NonBlocking,
            |result, _env| {
                let _ = result;
                Ok(())
            },
        );
    }

    fn release(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        let callback = match self.callback.lock() {
            Ok(mut callback) => callback.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        let Some(callback) = callback else {
            return;
        };
        callback.handle.with_write_aborted(|mut aborted| {
            if !*aborted {
                // SAFETY: the raw TSFN is owned by `callback.handle`; the
                // write guard serializes this abort with calls and Drop.
                let _ = unsafe {
                    sys::napi_release_threadsafe_function(
                        callback.handle.get_raw(),
                        sys::ThreadsafeFunctionReleaseMode::abort,
                    )
                };
                *aborted = true;
            }
        });
    }
}

impl Drop for TransportErrorCallback {
    fn drop(&mut self) {
        self.release();
    }
}

fn nfs_hooks(callback: Option<&Arc<TransportErrorCallback>>) -> TransportNfsServerHooks {
    TransportNfsServerHooks {
        on_transport_error: callback.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(move |error: TransportNfsError| callback.report(error.into()))
                as TransportNfsErrorHook
        }),
    }
}

fn p9_hooks(callback: Option<&Arc<TransportErrorCallback>>) -> TransportP9ServerHooks {
    TransportP9ServerHooks {
        on_transport_error: callback.map(|callback| {
            let callback = Arc::clone(callback);
            Arc::new(move |error: TransportP9Error| callback.report(error.into()))
                as TransportP9ErrorHook
        }),
    }
}

fn config_error(message: impl Into<String>) -> Error {
    super::to_js_error(
        FsError::new(ErrorCode::Einval)
            .with_syscall("createServer")
            .with_message(message),
    )
}

fn transport_error(operation: &str, error: impl std::fmt::Display) -> Error {
    Error::new(
        Status::GenericFailure,
        format!("mount-rs {operation} failed: {error}"),
    )
}

fn number(name: &str, value: Option<f64>, default: usize) -> Result<usize, Error> {
    let Some(value) = value else {
        return Ok(default);
    };
    if !value.is_finite()
        || value.fract() != 0.0
        || !(0.0..=MAX_SAFE_INTEGER).contains(&value)
        || value > usize::MAX as f64
    {
        return Err(config_error(format!(
            "{name} must be an integer between 0 and {MAX_SAFE_INTEGER}"
        )));
    }
    Ok(value as usize)
}

fn positive_number(name: &str, value: Option<f64>, default: usize) -> Result<usize, Error> {
    let value = number(name, value, default)?;
    if value == 0 {
        return Err(config_error(format!("{name} must be greater than zero")));
    }
    Ok(value)
}

fn u16_number(name: &str, value: Option<f64>, default: u16) -> Result<u16, Error> {
    let value = number(name, value, default as usize)?;
    u16::try_from(value).map_err(|_| config_error(format!("{name} must be between 0 and 65535")))
}

fn u32_number(name: &str, value: Option<f64>, default: u32) -> Result<u32, Error> {
    let value = number(name, value, default as usize)?;
    u32::try_from(value).map_err(|_| config_error(format!("{name} must fit in a uint32")))
}

fn duration_ms(name: &str, value: Option<f64>, default: Duration) -> Result<Duration, Error> {
    let default_ms = default.as_millis();
    let value = number(
        name,
        value,
        usize::try_from(default_ms).unwrap_or(usize::MAX),
    )?;
    Ok(Duration::from_millis(u64::try_from(value).map_err(
        |_| config_error(format!("{name} is too large")),
    )?))
}

fn strip_brackets(host: &str) -> Result<&str, Error> {
    match (host.starts_with('['), host.ends_with(']')) {
        (true, true) if host.len() > 2 => Ok(&host[1..host.len() - 1]),
        (false, false) if !host.contains(['[', ']']) => Ok(host),
        _ => Err(config_error(format!("invalid host {host:?}"))),
    }
}

fn ip_host(host: Option<String>, default: &str) -> Result<(String, IpAddr), Error> {
    let host = host.unwrap_or_else(|| default.to_owned());
    let bare = strip_brackets(&host)?;
    let address = if bare.eq_ignore_ascii_case("localhost") {
        IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
    } else {
        bare.parse::<IpAddr>().map_err(|_| {
            config_error(format!("host {host:?} must be an IP address or localhost"))
        })?
    };
    Ok((host, address))
}

fn bracketed_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    }
}

fn valid_s3_bucket_name(bucket: &str) -> bool {
    !bucket.is_empty()
        && bucket != "."
        && bucket != ".."
        && bucket.len() <= 255
        && bucket
            .chars()
            .all(|character| !character.is_control() && character != '/' && character != '\\')
}

fn verifier(value: Option<Buffer>) -> Result<Option<[u8; 8]>, Error> {
    let Some(value) = value else {
        return Ok(None);
    };
    let bytes = value.as_ref();
    let array: [u8; 8] = bytes
        .try_into()
        .map_err(|_| config_error("verifier must contain exactly 8 bytes"))?;
    Ok(Some(array))
}

#[napi(object)]
pub struct NfsServerOptions {
    pub port: Option<f64>,
    pub host: Option<String>,
    pub allow_remote: Option<bool>,
    pub max_record: Option<f64>,
    pub max_in_flight: Option<f64>,
    pub use_driver_ino: Option<bool>,
    #[napi(ts_type = "Uint8Array")]
    pub verifier: Option<Buffer>,
    pub rtmax: Option<f64>,
    pub wtmax: Option<f64>,
    pub dtpref: Option<f64>,
    pub snapshot_cache: Option<f64>,
    pub claim_ownership: Option<bool>,
    #[napi(ts_type = "(error: unknown, peer: string | undefined) => void")]
    pub on_transport_error: Option<JsTransportErrorCallback>,
}

fn nfs_options(
    options: Option<NfsServerOptions>,
) -> Result<
    (
        String,
        u16,
        TransportNfsServerOptions,
        Option<JsTransportErrorCallback>,
    ),
    Error,
> {
    let options = options.unwrap_or(NfsServerOptions {
        port: None,
        host: None,
        allow_remote: None,
        max_record: None,
        max_in_flight: None,
        use_driver_ino: None,
        verifier: None,
        rtmax: None,
        wtmax: None,
        dtpref: None,
        snapshot_cache: None,
        claim_ownership: None,
        on_transport_error: None,
    });
    let on_transport_error = options.on_transport_error;
    let (host, address) = ip_host(options.host, "127.0.0.1")?;
    let port = u16_number("port", options.port, 0)?;
    let mut output = TransportNfsServerOptions {
        bind: SocketAddr::new(address, port),
        ..TransportNfsServerOptions::default()
    };
    output.allow_remote = options.allow_remote.unwrap_or(false);
    output.record_limit = positive_number("maxRecord", options.max_record, output.record_limit)?;
    output.max_in_flight =
        positive_number("maxInFlight", options.max_in_flight, output.max_in_flight)?;
    output.session.use_driver_ino = options.use_driver_ino.unwrap_or(true);
    output.session.verifier = verifier(options.verifier)?;
    output.session.rtmax = positive_number("rtmax", options.rtmax, output.session.rtmax)?;
    output.session.wtmax = positive_number("wtmax", options.wtmax, output.session.wtmax)?;
    output.session.dtpref = positive_number("dtpref", options.dtpref, output.session.dtpref)?;
    output.session.snapshot_cache = number(
        "snapshotCache",
        options.snapshot_cache,
        output.session.snapshot_cache,
    )?;
    output.session.claim_ownership = options.claim_ownership.unwrap_or(true);
    Ok((host, port, output, on_transport_error))
}

#[napi]
pub struct NfsSession {
    inner: TransportNfsSession,
    v4_inner: TransportNfs4Session,
}

#[napi(object)]
pub struct NfsSessionStats {
    pub requests: f64,
    pub replies: f64,
    pub errors: f64,
    pub dropped: f64,
    pub procedures: HashMap<String, f64>,
}

#[napi(object)]
pub struct NfsHandleEntry {
    pub id: BigInt,
    pub fileid: BigInt,
    pub key: Option<String>,
    pub path: String,
}

fn nfs_handle_entries(table: &TransportNfsHandleTable) -> Vec<NfsHandleEntry> {
    table
        .entries()
        .into_iter()
        .map(|entry| NfsHandleEntry {
            id: BigInt::from(entry.id),
            fileid: BigInt::from(entry.fileid),
            key: entry.key,
            path: entry.path,
        })
        .collect()
}

/// Read-only N-API view of the versioned NFS sessions owned by a server.
#[napi]
impl NfsSession {
    /// Handle one unframed NFSv3 or NFSv4 RPC record. Malformed records return
    /// `null`; decoded calls return one encoded RPC reply.
    #[napi]
    pub async fn handle_call(&self, bytes: Buffer) -> Option<Buffer> {
        let context = TransportNfsRequestContext::default();
        let reply = if is_nfs_v4(bytes.as_ref()) {
            self.v4_inner.handle_call(bytes.as_ref(), context).await
        } else {
            self.inner.handle_call(bytes.as_ref(), context).await
        };
        reply.map(Buffer::from)
    }

    /// The NFSv4.1 session routed by this server. Its state is read-only at the
    /// N-API boundary and shares the server-owned driver lifetime.
    #[napi(getter)]
    pub fn v4(&self) -> Nfs4Session {
        Nfs4Session {
            inner: self.v4_inner.clone(),
        }
    }

    #[napi(getter)]
    pub fn stats(&self) -> NfsSessionStats {
        let stats = self.inner.stats();
        NfsSessionStats {
            requests: stats.requests as f64,
            replies: stats.replies as f64,
            errors: stats.errors as f64,
            dropped: stats.dropped as f64,
            procedures: stats
                .procedures
                .into_iter()
                .map(|(name, count)| (name, count as f64))
                .collect(),
        }
    }

    #[napi(getter)]
    pub fn mounts(&self) -> Vec<Vec<String>> {
        self.inner
            .mounts()
            .into_iter()
            .map(|(hostname, directory)| vec![hostname, directory])
            .collect()
    }

    /// Stable read-only snapshots of the shared v3/v4 file-handle table.
    /// Handles are BigInts because the transport identity is u64.
    #[napi(getter)]
    pub fn handles(&self) -> Vec<NfsHandleEntry> {
        nfs_handle_entries(&self.inner.handles)
    }

    #[napi(getter)]
    pub fn destroyed(&self) -> bool {
        self.inner.destroyed() && self.v4_inner.destroyed()
    }
}

/// Read-only N-API view of the NFSv4.1 session routed by an [`NfsServer`].
#[napi]
pub struct Nfs4Session {
    inner: TransportNfs4Session,
}

#[napi]
impl Nfs4Session {
    /// Handle one unframed NFSv4 RPC record. Malformed records return `null`;
    /// decoded calls return one encoded RPC reply.
    #[napi]
    pub async fn handle_call(&self, bytes: Buffer) -> Option<Buffer> {
        self.inner
            .handle_call(bytes.as_ref(), TransportNfsRequestContext::default())
            .await
            .map(Buffer::from)
    }

    #[napi(getter)]
    pub fn stats(&self) -> NfsSessionStats {
        let stats = self.inner.stats();
        NfsSessionStats {
            requests: stats.requests as f64,
            replies: stats.replies as f64,
            errors: stats.errors as f64,
            dropped: stats.dropped as f64,
            procedures: stats
                .procedures
                .into_iter()
                .map(|(name, count)| (name, count as f64))
                .collect(),
        }
    }

    #[napi(getter)]
    pub fn handles(&self) -> Vec<NfsHandleEntry> {
        nfs_handle_entries(&self.inner.handles)
    }

    #[napi(getter)]
    pub fn destroyed(&self) -> bool {
        self.inner.destroyed()
    }
}

fn is_nfs_v4(bytes: &[u8]) -> bool {
    if bytes.len() < 20 {
        return false;
    }
    let program = u32::from_be_bytes(bytes[12..16].try_into().expect("NFS program bytes"));
    let version = u32::from_be_bytes(bytes[16..20].try_into().expect("NFS version bytes"));
    program == NFS4_PROGRAM && version == NFS_V4
}

#[napi]
pub struct NfsServer {
    inner: Arc<TransportNfsServer>,
    host: String,
    requested_port: u16,
    closed: AtomicBool,
    transport_error: Option<Arc<TransportErrorCallback>>,
}

#[napi]
impl NfsServer {
    #[napi(getter)]
    pub fn session(&self) -> NfsSession {
        NfsSession {
            inner: self.inner.session().clone(),
            v4_inner: self.inner.v4_session().clone(),
        }
    }

    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn port(&self) -> u32 {
        self.inner.port().unwrap_or(self.requested_port) as u32
    }

    #[napi(getter)]
    pub fn connections(&self) -> u32 {
        self.inner.connections() as u32
    }

    #[napi]
    pub async fn listen(&self) -> napi::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(transport_error("NFS listen", "server is closed"));
        }
        self.inner
            .listen()
            .await
            .map(|_| ())
            .map_err(|error| transport_error("NFS listen", error))
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.closed.store(true, Ordering::Release);
        if let Some(callback) = &self.transport_error {
            callback.release();
        }
        self.inner
            .close()
            .await
            .map_err(|error| transport_error("NFS close", error))
    }
}

#[napi]
pub fn create_nfs_server(
    driver: &Filesystem,
    options: Option<NfsServerOptions>,
) -> napi::Result<NfsServer> {
    let (host, requested_port, options, on_transport_error) = nfs_options(options)?;
    let transport_error = on_transport_error
        .map(TransportErrorCallback::new)
        .transpose()?;
    let inner = TransportNfsServer::new_with_hooks(
        MountDriver(Arc::clone(&driver.driver)),
        options,
        nfs_hooks(transport_error.as_ref()),
    );
    Ok(NfsServer {
        inner: Arc::new(inner),
        host,
        requested_port,
        closed: AtomicBool::new(false),
        transport_error,
    })
}

#[napi(object)]
pub struct P9ServerOptions {
    pub port: Option<f64>,
    pub host: Option<String>,
    pub path: Option<String>,
    pub allow_remote: Option<bool>,
    pub socket_mode: Option<f64>,
    pub allow_shared_directory: Option<bool>,
    pub max_frame: Option<f64>,
    pub max_in_flight: Option<f64>,
    pub msize: Option<f64>,
    pub use_driver_ino: Option<bool>,
    pub read_only: Option<bool>,
    pub claim_ownership: Option<bool>,
    #[napi(ts_type = "(error: unknown, peer: string | undefined) => void")]
    pub on_transport_error: Option<JsTransportErrorCallback>,
}

fn p9_options(
    options: Option<P9ServerOptions>,
) -> Result<
    (
        String,
        u16,
        TransportP9ServerOptions,
        Option<JsTransportErrorCallback>,
    ),
    Error,
> {
    let options = options.unwrap_or(P9ServerOptions {
        port: None,
        host: None,
        path: None,
        allow_remote: None,
        socket_mode: None,
        allow_shared_directory: None,
        max_frame: None,
        max_in_flight: None,
        msize: None,
        use_driver_ino: None,
        read_only: None,
        claim_ownership: None,
        on_transport_error: None,
    });
    let on_transport_error = options.on_transport_error;
    if options.path.is_some() && (options.host.is_some() || options.port.is_some()) {
        return Err(config_error(
            "a 9P server uses either path or host/port, not both",
        ));
    }
    let host = options.host.unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = u16_number("port", options.port, 0)?;
    let mut output = TransportP9ServerOptions {
        host: host.clone(),
        port,
        path: options.path.map(Into::into),
        ..TransportP9ServerOptions::default()
    };
    output.allow_remote = options.allow_remote.unwrap_or(false);
    output.socket_mode = u32_number("socketMode", options.socket_mode, output.socket_mode)?;
    output.allow_shared_directory = options.allow_shared_directory.unwrap_or(false);
    output.max_frame = positive_number("maxFrame", options.max_frame, output.max_frame)?;
    output.max_in_flight =
        positive_number("maxInFlight", options.max_in_flight, output.max_in_flight)?;
    output.msize = options
        .msize
        .map(|value| u32_number("msize", Some(value), 0))
        .transpose()?;
    output.use_driver_ino = options.use_driver_ino.unwrap_or(true);
    output.read_only = options.read_only.unwrap_or(false);
    output.claim_ownership = options.claim_ownership.unwrap_or(true);
    Ok((host, port, output, on_transport_error))
}

type P9ServeFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

struct P9State {
    server: Option<Arc<TransportP9Server>>,
    serve_task: Option<P9ServeFuture>,
}

#[napi]
pub struct P9Session {
    inner: mount_rs_9p::P9Session,
}

#[napi(object)]
pub struct P9SessionStats {
    pub requests: f64,
    pub replies: f64,
    pub errors: f64,
    pub dropped: f64,
    pub flushed: f64,
    pub messages: HashMap<String, f64>,
}

impl From<mount_rs_9p::P9SessionStats> for P9SessionStats {
    fn from(stats: mount_rs_9p::P9SessionStats) -> Self {
        Self {
            requests: stats.requests as f64,
            replies: stats.replies as f64,
            errors: stats.errors as f64,
            dropped: stats.dropped as f64,
            flushed: stats.flushed as f64,
            messages: stats
                .messages
                .into_iter()
                .map(|(name, count)| (name, count as f64))
                .collect(),
        }
    }
}

#[napi]
impl P9Session {
    /// Handle one complete 9P frame without a socket. Malformed framing
    /// returns `null`; protocol and driver failures remain encoded as an
    /// `Rlerror`, matching the transport session contract.
    #[napi]
    pub async fn handle_call(&self, bytes: Buffer) -> Option<Buffer> {
        self.inner
            .handle_call(bytes.as_ref())
            .await
            .map(Buffer::from)
    }

    /// Tear down the session and release every driver handle it owns.
    #[napi]
    pub async fn destroy(&self) {
        self.inner.destroy().await;
    }

    #[napi(getter)]
    pub fn msize(&self) -> Option<u32> {
        self.inner.msize()
    }

    #[napi(getter)]
    pub fn version(&self) -> Option<String> {
        self.inner.version().map(str::to_owned)
    }

    #[napi(getter)]
    pub fn generation(&self) -> f64 {
        self.inner.generation() as f64
    }

    #[napi(getter)]
    pub fn destroyed(&self) -> bool {
        self.inner.destroyed()
    }

    #[napi(getter)]
    pub fn inflight(&self) -> f64 {
        self.inner.inflight() as f64
    }

    #[napi(getter)]
    pub fn stats(&self) -> P9SessionStats {
        self.inner.stats().into()
    }
}

#[napi]
pub struct P9Connection {
    inner: mount_rs_9p::P9Connection,
}

#[napi]
impl P9Connection {
    #[napi(getter)]
    pub fn session(&self) -> P9Session {
        P9Session {
            inner: self.inner.session.clone(),
        }
    }

    #[napi(getter)]
    pub fn id(&self) -> f64 {
        self.inner.id() as f64
    }

    #[napi(getter)]
    pub fn peer(&self) -> Option<String> {
        self.inner.peer.clone()
    }

    #[napi(getter)]
    pub fn is_closed(&self) -> bool {
        self.inner.is_closed()
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.inner
            .close()
            .await
            .map_err(|error| transport_error("9P connection close", error))
    }

    #[napi]
    pub async fn wait_closed(&self) -> napi::Result<()> {
        self.inner.wait_closed().await;
        Ok(())
    }
}

#[napi]
pub struct P9Server {
    driver: Arc<dyn FsDriver>,
    options: TransportP9ServerOptions,
    host: String,
    requested_port: u16,
    state: Mutex<P9State>,
    binding: AtomicBool,
    closed: AtomicBool,
    transport_error: Option<Arc<TransportErrorCallback>>,
}

#[napi]
impl P9Server {
    /// Create a session for the JavaScript duplex-stream adapter. The method
    /// is intentionally internal: the public Node contract is `attach`, while
    /// this native seam supplies the same shared driver/lock state without
    /// making N-API own a JavaScript stream from a Tokio task.
    #[napi(js_name = "_createAttachedSession")]
    pub fn create_attached_session(&self) -> P9Session {
        P9Session {
            inner: mount_rs_9p::P9Session::with_options(
                Arc::clone(&self.driver),
                self.options.session_options(),
            ),
        }
    }

    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn path(&self) -> Option<String> {
        self.options
            .path
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
    }

    #[napi(getter)]
    pub fn port(&self) -> u32 {
        let state = self.state.lock().expect("9P state lock");
        state
            .server
            .as_ref()
            .and_then(|server| server.local_addr().ok())
            .map_or(self.requested_port, |address| address.port()) as u32
    }

    #[napi(getter)]
    pub fn connections(&self) -> u32 {
        let state = self.state.lock().expect("9P state lock");
        state
            .server
            .as_ref()
            .and_then(|server| server.connection_count().ok())
            .unwrap_or(0) as u32
    }

    #[napi]
    pub fn address(&self) -> Option<String> {
        let state = self.state.lock().expect("9P state lock");
        let Some(server) = state.server.as_ref() else {
            return self.path();
        };
        if let Some(path) = server.unix_path() {
            return Some(path.to_string_lossy().into_owned());
        }
        server.local_addr().ok().map(|address| address.to_string())
    }

    #[napi]
    pub fn clients(&self) -> napi::Result<Vec<P9Connection>> {
        let state = self.state.lock().expect("9P state lock");
        state.server.as_ref().map_or(Ok(Vec::new()), |server| {
            server
                .clients()
                .map(|clients| {
                    clients
                        .into_iter()
                        .map(|inner| P9Connection { inner })
                        .collect()
                })
                .map_err(|error| transport_error("9P clients", error))
        })
    }

    #[napi]
    pub async fn listen(&self) -> napi::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(transport_error("9P listen", "server is closed"));
        }
        {
            let state = self.state.lock().expect("9P state lock");
            if state.server.is_some() {
                return Ok(());
            }
        }
        if self
            .binding
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(transport_error("9P listen", "server is already starting"));
        }
        let result = TransportP9Server::bind_arc_with_hooks(
            Arc::clone(&self.driver),
            self.options.clone(),
            p9_hooks(self.transport_error.as_ref()),
        )
        .await
        .map(Arc::new)
        .map_err(|error| transport_error("9P listen", error));
        let server = match result {
            Ok(server) => server,
            Err(error) => {
                self.binding.store(false, Ordering::Release);
                return Err(error);
            }
        };
        let task = match server.start() {
            Ok(task) => task,
            Err(error) => {
                let close_result = server.close().await;
                self.binding.store(false, Ordering::Release);
                if let Err(close_error) = close_result {
                    return Err(transport_error("9P close", close_error));
                }
                return Err(transport_error("9P listen", error));
            }
        };
        let serve_task: P9ServeFuture = Box::pin(async move {
            let _ = task.await;
        });
        if self.closed.load(Ordering::Acquire) {
            server
                .close()
                .await
                .map_err(|error| transport_error("9P close", error))?;
            serve_task.await;
            self.binding.store(false, Ordering::Release);
            return Err(transport_error(
                "9P listen",
                "server was closed while binding",
            ));
        }
        {
            let mut state = self.state.lock().expect("9P state lock");
            state.server = Some(server);
            state.serve_task = Some(serve_task);
        }
        self.binding.store(false, Ordering::Release);
        Ok(())
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.closed.store(true, Ordering::Release);
        if let Some(callback) = &self.transport_error {
            callback.release();
        }
        let (server, task) = {
            let mut state = self.state.lock().expect("9P state lock");
            (state.server.take(), state.serve_task.take())
        };
        if let Some(server) = server {
            server
                .close()
                .await
                .map_err(|error| transport_error("9P close", error))?;
        }
        if let Some(task) = task {
            task.await;
        }
        Ok(())
    }
}

#[napi]
pub fn create_p9_server(
    driver: &Filesystem,
    options: Option<P9ServerOptions>,
) -> napi::Result<P9Server> {
    let (host, requested_port, mut options, on_transport_error) = p9_options(options)?;
    // The native listener and the JavaScript attach seam must share byte-range
    // lock ownership when they are used on the same public server object.
    if options.locks.is_none() {
        options.locks = Some(P9LockTable::new(Default::default()));
    }
    let transport_error = on_transport_error
        .map(TransportErrorCallback::new)
        .transpose()?;
    Ok(P9Server {
        driver: Arc::clone(&driver.driver),
        options,
        host,
        requested_port,
        state: Mutex::new(P9State {
            server: None,
            serve_task: None,
        }),
        binding: AtomicBool::new(false),
        closed: AtomicBool::new(false),
        transport_error,
    })
}

#[napi(object)]
pub struct S3Credentials {
    pub access_key_id: String,
    pub secret_access_key: String,
}

#[napi(object)]
pub struct S3ServerOptions {
    pub bucket: Option<String>,
    pub host: Option<String>,
    pub port: Option<f64>,
    pub credentials: Option<S3Credentials>,
    pub region: Option<String>,
    pub max_body_bytes: Option<f64>,
    pub max_xml_bytes: Option<f64>,
    pub read_chunk_bytes: Option<f64>,
    pub drain_timeout: Option<f64>,
}

fn s3_options(
    options: Option<S3ServerOptions>,
) -> Result<
    (
        String,
        u16,
        Option<String>,
        TransportS3ServerOptions,
        S3SessionOptions,
    ),
    Error,
> {
    let options = options.unwrap_or(S3ServerOptions {
        bucket: None,
        host: None,
        port: None,
        credentials: None,
        region: None,
        max_body_bytes: None,
        max_xml_bytes: None,
        read_chunk_bytes: None,
        drain_timeout: None,
    });
    let (host, address) = ip_host(options.host, "127.0.0.1")?;
    let port = u16_number("port", options.port, 0)?;
    let credentials = options.credentials.map(|credentials| {
        TransportS3Credentials::new(credentials.access_key_id, credentials.secret_access_key)
    });
    if credentials.is_none() && !address.is_loopback() {
        return Err(config_error(
            "unauthenticated S3 servers must bind a loopback address",
        ));
    }
    let mut session = S3SessionOptions::default();
    session.credentials = credentials;
    session.region = options.region;
    session.max_body_bytes = positive_number(
        "maxBodyBytes",
        options.max_body_bytes,
        session.max_body_bytes,
    )?;
    session.max_xml_bytes =
        positive_number("maxXmlBytes", options.max_xml_bytes, session.max_xml_bytes)?;
    session.read_chunk_bytes = positive_number(
        "readChunkBytes",
        options.read_chunk_bytes,
        session.read_chunk_bytes,
    )?;
    let mut server_options = TransportS3ServerOptions {
        host: address,
        port,
        ..TransportS3ServerOptions::default()
    };
    server_options.drain_timeout = duration_ms(
        "drainTimeout",
        options.drain_timeout,
        server_options.drain_timeout,
    )?;
    Ok((host, port, options.bucket, server_options, session))
}

type S3BucketEntries = Vec<(String, Arc<dyn FsDriver>)>;

fn s3_buckets(
    source: Either<&Filesystem, Object<'_>>,
    bucket: Option<String>,
) -> Result<S3BucketEntries, Error> {
    match source {
        Either::A(driver) => {
            let bucket = bucket.unwrap_or_else(|| "mountx".to_owned());
            if !valid_s3_bucket_name(&bucket) {
                return Err(config_error(format!("invalid S3 bucket name {bucket:?}")));
            }
            Ok(vec![(bucket, Arc::clone(&driver.driver))])
        }
        Either::B(source) => {
            let buckets = source
                .get_named_property::<Object<'_>>("buckets")
                .map_err(|_| config_error("S3 source must contain a buckets object"))?;
            let mut entries = Vec::new();
            for name in Object::keys(&buckets)
                .map_err(|error| config_error(format!("could not enumerate S3 buckets: {error}")))?
            {
                if !valid_s3_bucket_name(&name) {
                    return Err(config_error(format!("invalid S3 bucket name {name:?}")));
                }
                let driver = buckets
                    .get_named_property_unchecked::<Reference<Filesystem>>(&name)
                    .map_err(|_| {
                        config_error(format!("S3 bucket {name:?} must contain a Filesystem"))
                    })?;
                entries.push((name, Arc::clone(&driver.driver)));
            }
            Ok(entries)
        }
    }
}

#[napi]
pub struct S3Server {
    session: Arc<S3Session>,
    options: TransportS3ServerOptions,
    host: String,
    requested_port: u16,
    buckets: Vec<String>,
    running: Mutex<Option<Arc<TransportS3Server>>>,
    binding: AtomicBool,
    closed: AtomicBool,
}

#[napi]
impl S3Server {
    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn port(&self) -> u32 {
        let running = self.running.lock().expect("S3 server lock");
        running
            .as_ref()
            .map_or(self.requested_port, |server| server.address().port()) as u32
    }

    #[napi(getter)]
    pub fn url(&self) -> String {
        format!("http://{}:{}", bracketed_host(&self.host), self.port())
    }

    #[napi(getter)]
    pub fn buckets(&self) -> Vec<String> {
        self.buckets.clone()
    }

    #[napi]
    pub async fn listen(&self) -> napi::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(transport_error("S3 listen", "server is closed"));
        }
        {
            let running = self.running.lock().expect("S3 server lock");
            if running.is_some() {
                return Ok(());
            }
        }
        if self
            .binding
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(transport_error("S3 listen", "server is already starting"));
        }
        let result = TransportS3Server::start(Arc::clone(&self.session), self.options.clone())
            .await
            .map(Arc::new)
            .map_err(|error| transport_error("S3 listen", error));
        match result {
            Ok(server) => {
                if self.closed.load(Ordering::Acquire) {
                    server
                        .close()
                        .await
                        .map_err(|error| transport_error("S3 close", error))?;
                    self.binding.store(false, Ordering::Release);
                    return Err(transport_error(
                        "S3 listen",
                        "server was closed while binding",
                    ));
                }
                *self.running.lock().expect("S3 server lock") = Some(server);
                self.binding.store(false, Ordering::Release);
                Ok(())
            }
            Err(error) => {
                self.binding.store(false, Ordering::Release);
                Err(error)
            }
        }
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.closed.store(true, Ordering::Release);
        let running = self.running.lock().expect("S3 server lock").take();
        if let Some(server) = running {
            server
                .close()
                .await
                .map_err(|error| transport_error("S3 close", error))?;
        } else {
            self.session
                .close()
                .await
                .map_err(|error| transport_error("S3 session close", error))?;
        }
        Ok(())
    }
}

#[napi]
pub fn create_s3_server(
    #[napi(ts_arg_type = "Filesystem | { buckets: Record<string, Filesystem> }")] source: Either<
        &Filesystem,
        Object<'_>,
    >,
    options: Option<S3ServerOptions>,
) -> napi::Result<S3Server> {
    let (host, requested_port, bucket, server_options, session_options) = s3_options(options)?;
    let buckets = s3_buckets(source, bucket)?;
    let session = Arc::new(S3Session::from_buckets_with_options(
        buckets,
        session_options,
    ));
    let buckets = session.bucket_names();
    Ok(S3Server {
        session,
        options: server_options,
        host,
        requested_port,
        buckets,
        running: Mutex::new(None),
        binding: AtomicBool::new(false),
        closed: AtomicBool::new(false),
    })
}

#[napi(object)]
pub struct WebdavCredentials {
    pub username: String,
    pub password: String,
}

#[napi(object)]
pub struct WebdavServerOptions {
    pub host: Option<String>,
    pub port: Option<f64>,
    pub credentials: Option<WebdavCredentials>,
    pub realm: Option<String>,
    pub read_chunk_bytes: Option<f64>,
    pub max_xml_bytes: Option<f64>,
    pub max_body_bytes: Option<f64>,
    pub drain_timeout: Option<f64>,
    pub debug: Option<bool>,
}

fn webdav_options(
    options: Option<WebdavServerOptions>,
) -> Result<(String, u16, TransportWebdavServerOptions), Error> {
    let options = options.unwrap_or(WebdavServerOptions {
        host: None,
        port: None,
        credentials: None,
        realm: None,
        read_chunk_bytes: None,
        max_xml_bytes: None,
        max_body_bytes: None,
        drain_timeout: None,
        debug: None,
    });
    let host = options.host.unwrap_or_else(|| "127.0.0.1".to_owned());
    let port = u16_number("port", options.port, 0)?;
    let mut output = TransportWebdavServerOptions {
        host: host.clone(),
        port,
        ..TransportWebdavServerOptions::default()
    };
    if let Some(credentials) = options.credentials {
        output = output.with_credentials(credentials.username, credentials.password);
    }
    if let Some(realm) = options.realm {
        output.session.realm = realm;
    }
    output.session.read_chunk_bytes = positive_number(
        "readChunkBytes",
        options.read_chunk_bytes,
        output.session.read_chunk_bytes,
    )?;
    output.session.max_xml_bytes = positive_number(
        "maxXmlBytes",
        options.max_xml_bytes,
        output.session.max_xml_bytes,
    )?;
    if let Some(max_body_bytes) = options.max_body_bytes {
        let max_body_bytes = positive_number(
            "maxBodyBytes",
            Some(max_body_bytes),
            output.max_request_bytes,
        )?;
        output.session.max_body_bytes = Some(max_body_bytes);
        output.max_request_bytes = max_body_bytes;
    }
    output.drain_timeout =
        duration_ms("drainTimeout", options.drain_timeout, output.drain_timeout)?;
    output.session.debug = options.debug.unwrap_or(output.session.debug);
    Ok((host, port, output))
}

#[napi]
pub struct WebdavServer {
    inner: Arc<TransportWebdavServer>,
    host: String,
    closed: AtomicBool,
}

#[napi]
impl WebdavServer {
    #[napi(getter)]
    pub fn host(&self) -> String {
        self.host.clone()
    }

    #[napi(getter)]
    pub fn port(&self) -> u32 {
        self.inner.port() as u32
    }

    #[napi(getter)]
    pub fn url(&self) -> String {
        format!("http://{}:{}", bracketed_host(&self.host), self.port())
    }

    #[napi(getter)]
    pub fn connections(&self) -> u32 {
        self.inner.connections() as u32
    }

    #[napi]
    pub async fn listen(&self) -> napi::Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(transport_error("WebDAV listen", "server is closed"));
        }
        self.inner
            .listen()
            .await
            .map_err(|error| transport_error("WebDAV listen", error))
    }

    #[napi]
    pub async fn close(&self) -> napi::Result<()> {
        self.closed.store(true, Ordering::Release);
        self.inner
            .close()
            .await
            .map_err(|error| transport_error("WebDAV close", error))
    }
}

#[napi]
pub fn create_webdav_server(
    driver: &Filesystem,
    options: Option<WebdavServerOptions>,
) -> napi::Result<WebdavServer> {
    let (host, _requested_port, options) = webdav_options(options)?;
    let inner = mount_rs_webdav::create_webdav_server(Arc::clone(&driver.driver), options)
        .map_err(|error| transport_error("WebDAV create", error))?;
    Ok(WebdavServer {
        inner: Arc::new(inner),
        host,
        closed: AtomicBool::new(false),
    })
}
