//! Portable 9P listeners and attached-stream serving.
//!
//! TCP, Unix-domain sockets, and an already-connected Tokio stream all enter
//! the same per-connection session. The transport owns framing and lifecycle;
//! [`P9Session`] remains independent of the socket type so its wire behavior is
//! testable on every host supported by this crate.

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use mount_rs_core::FsDriver;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;
#[cfg(unix)]
use tokio::net::UnixListener;
use tokio::sync::{Mutex, Notify, Semaphore, mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};

use crate::constants::{P9_DEFAULT_MAX_FRAME, P9_HDRSZ, P9_TVERSION};
use crate::locks::P9LockTable;
use crate::protocol::{P9FrameAssembler, P9VersionScan, decode_message_as, read_tversion};
use crate::session::{P9Session, P9SessionHooks, P9SessionOptions};
use crate::wire::P9Error;

pub const DEFAULT_P9_PORT: u16 = 564;
pub const DEFAULT_MAX_IN_FLIGHT: usize = 16;
pub const DEFAULT_SOCKET_MODE: u32 = 0o600;

/// The transport phase that terminated a 9P connection or listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum P9TransportErrorKind {
    Accept,
    PeerRefused,
    Read,
    Frame,
    Write,
    Task,
}

/// A transport failure reported to an embedding server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct P9TransportError {
    pub kind: P9TransportErrorKind,
    pub peer: Option<String>,
    pub message: String,
    pub raw_os_error: Option<i32>,
}

/// Synchronous callback used by the transport task to report one terminal
/// failure. The callback receives an owned value so it can cross an embedding
/// boundary without borrowing the Tokio task or its socket.
pub type P9TransportErrorHook = Arc<dyn Fn(P9TransportError) + Send + Sync + 'static>;

/// Optional hooks for a [`P9Server`]. Kept separate from
/// [`P9ServerOptions`] so existing struct literals remain source and ABI
/// compatible.
#[derive(Clone, Default)]
pub struct P9ServerHooks {
    pub on_transport_error: Option<P9TransportErrorHook>,
}

impl P9TransportError {
    fn from_io(kind: P9TransportErrorKind, peer: Option<String>, error: &io::Error) -> Self {
        Self {
            kind,
            peer,
            message: error.to_string(),
            raw_os_error: error.raw_os_error(),
        }
    }

    fn from_message(kind: P9TransportErrorKind, peer: Option<String>, message: String) -> Self {
        Self {
            kind,
            peer,
            message,
            raw_os_error: None,
        }
    }
}

#[derive(Clone)]
pub struct P9ServerOptions {
    pub host: String,
    pub port: u16,
    /// A Unix-domain socket path selects the Unix listener instead of TCP.
    pub path: Option<PathBuf>,
    pub allow_remote: bool,
    pub socket_mode: u32,
    pub allow_shared_directory: bool,
    pub max_frame: usize,
    pub max_in_flight: usize,
    pub msize: Option<u32>,
    pub use_driver_ino: bool,
    pub read_only: bool,
    pub claim_ownership: bool,
    pub debug: bool,
    pub locks: Option<crate::locks::P9LockTable>,
}

impl Default for P9ServerOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 0,
            path: None,
            allow_remote: false,
            socket_mode: DEFAULT_SOCKET_MODE,
            allow_shared_directory: false,
            max_frame: P9_DEFAULT_MAX_FRAME,
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
            msize: None,
            use_driver_ino: true,
            read_only: false,
            claim_ownership: true,
            debug: cfg!(debug_assertions),
            locks: None,
        }
    }
}

impl P9ServerOptions {
    pub fn session_options(&self) -> P9SessionOptions {
        P9SessionOptions {
            msize: self.msize,
            use_driver_ino: self.use_driver_ino,
            read_only: self.read_only,
            claim_ownership: self.claim_ownership,
            debug: self.debug,
            locks: self.locks.clone(),
        }
    }
}

/// How an already-connected stream is identified and who requested teardown.
///
/// A stream is passed by value to [`P9Server::attach`], so the server necessarily
/// owns its local half. `own` is still recorded because it distinguishes a
/// socket handed to the server from a composed test/embedding stream: the
/// former is explicitly shutdown on close, while the latter is only dropped.
#[derive(Clone, Debug)]
pub struct P9AttachOptions {
    pub peer: Option<String>,
    pub own: bool,
}

impl Default for P9AttachOptions {
    fn default() -> Self {
        Self {
            peer: None,
            own: true,
        }
    }
}

struct ConnectionControl {
    shutdown: Notify,
    closed: Notify,
    done: AtomicBool,
}

impl ConnectionControl {
    fn new() -> Self {
        Self {
            shutdown: Notify::new(),
            closed: Notify::new(),
            done: AtomicBool::new(false),
        }
    }

    fn stop(&self) {
        // There is one connection task. Keep a permit when close wins the
        // race with that task registering its select branch.
        self.shutdown.notify_one();
    }

    fn finish(&self) {
        self.done.store(true, Ordering::Release);
        self.closed.notify_waiters();
    }

    async fn wait(&self) {
        while !self.done.load(Ordering::Acquire) {
            self.closed.notified().await;
        }
    }
}

/// One client session and its transport lifecycle.
#[derive(Clone)]
pub struct P9Connection {
    pub session: P9Session,
    /// TCP `address:port`, the Unix socket path, or an attached-stream label.
    pub peer: Option<String>,
    id: u64,
    control: Arc<ConnectionControl>,
}

impl P9Connection {
    /// Stable identity useful when adopting the connection created by a mount.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn is_closed(&self) -> bool {
        self.control.done.load(Ordering::Acquire)
    }

    /// Request transport/session teardown and wait until the stream task is gone.
    pub async fn close(&self) -> io::Result<()> {
        self.control.stop();
        self.control.wait().await;
        Ok(())
    }

    /// Wait for EOF, an explicit close, or a server shutdown.
    pub async fn wait_closed(&self) {
        self.control.wait().await;
    }
}

trait P9Duplex: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T> P9Duplex for T where T: AsyncRead + AsyncWrite + Send + Unpin {}

type BoxedP9Stream = Box<dyn P9Duplex + 'static>;

enum P9Listener {
    Tcp(TcpListener),
    #[cfg(unix)]
    Unix(UnixListener),
}

/// A running or attach-only 9P server.
pub struct P9Server {
    listener: Arc<Mutex<Option<P9Listener>>>,
    driver: Arc<dyn FsDriver>,
    options: P9ServerOptions,
    hooks: P9ServerHooks,
    session_hooks: P9SessionHooks,
    locks: P9LockTable,
    shutdown: Arc<Notify>,
    shutdown_requested: Arc<AtomicBool>,
    closed: AtomicBool,
    serving: AtomicBool,
    next_id: AtomicU64,
    connections: Arc<StdMutex<HashMap<u64, P9Connection>>>,
    tcp_addr: StdMutex<Option<SocketAddr>>,
    #[cfg(target_os = "linux")]
    adoption: Mutex<()>,
}

impl P9Server {
    /// Construct an attach-only server. Call [`P9Server::bind`] or
    /// [`P9Server::bind_unix`] when a listener is desired.
    pub fn new<D>(driver: D, options: P9ServerOptions) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::new_with_hooks(driver, options, P9ServerHooks::default())
    }

    pub fn new_arc(driver: Arc<dyn FsDriver>, options: P9ServerOptions) -> Self {
        Self::new_arc_with_hooks(driver, options, P9ServerHooks::default())
    }

    /// Construct an attach-only server with transport failure reporting.
    pub fn new_with_hooks<D>(driver: D, options: P9ServerOptions, hooks: P9ServerHooks) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::new_arc_with_hooks(Arc::new(driver), options, hooks)
    }

    pub fn new_arc_with_hooks(
        driver: Arc<dyn FsDriver>,
        options: P9ServerOptions,
        hooks: P9ServerHooks,
    ) -> Self {
        Self::new_arc_with_hooks_and_session_hooks(
            driver,
            options,
            hooks,
            P9SessionHooks::default(),
        )
    }

    /// Construct an attach-only server with transport and session hooks.
    pub fn new_arc_with_hooks_and_session_hooks(
        driver: Arc<dyn FsDriver>,
        options: P9ServerOptions,
        hooks: P9ServerHooks,
        session_hooks: P9SessionHooks,
    ) -> Self {
        let locks = options
            .locks
            .clone()
            .unwrap_or_else(|| P9LockTable::new(Default::default()));
        Self {
            listener: Arc::new(Mutex::new(None)),
            driver,
            options,
            hooks,
            session_hooks,
            locks,
            shutdown: Arc::new(Notify::new()),
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            closed: AtomicBool::new(false),
            serving: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            connections: Arc::new(StdMutex::new(HashMap::new())),
            tcp_addr: StdMutex::new(None),
            #[cfg(target_os = "linux")]
            adoption: Mutex::new(()),
        }
    }

    pub async fn bind<D>(driver: D, options: P9ServerOptions) -> io::Result<Self>
    where
        D: FsDriver + 'static,
    {
        Self::bind_with_hooks(driver, options, P9ServerHooks::default()).await
    }

    pub async fn bind_arc(driver: Arc<dyn FsDriver>, options: P9ServerOptions) -> io::Result<Self> {
        Self::bind_arc_with_hooks(driver, options, P9ServerHooks::default()).await
    }

    pub async fn bind_with_hooks<D>(
        driver: D,
        options: P9ServerOptions,
        hooks: P9ServerHooks,
    ) -> io::Result<Self>
    where
        D: FsDriver + 'static,
    {
        Self::bind_arc_with_hooks(Arc::new(driver), options, hooks).await
    }

    pub async fn bind_arc_with_hooks(
        driver: Arc<dyn FsDriver>,
        options: P9ServerOptions,
        hooks: P9ServerHooks,
    ) -> io::Result<Self> {
        Self::bind_arc_with_hooks_and_session_hooks(
            driver,
            options,
            hooks,
            P9SessionHooks::default(),
        )
        .await
    }

    /// Bind a listener with transport and session hooks.
    pub async fn bind_arc_with_hooks_and_session_hooks(
        driver: Arc<dyn FsDriver>,
        options: P9ServerOptions,
        hooks: P9ServerHooks,
        session_hooks: P9SessionHooks,
    ) -> io::Result<Self> {
        let server = Self::new_arc_with_hooks_and_session_hooks(
            driver,
            options.clone(),
            hooks,
            session_hooks,
        );
        if let Some(path) = options.path {
            #[cfg(unix)]
            {
                server.bind_unix_listener(path).await?;
                return Ok(server);
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Unix-domain 9P listeners are unavailable on this platform",
                ));
            }
        }
        server.bind_tcp_listener().await?;
        Ok(server)
    }

    /// Bind a Unix-domain listener. The parent directory must be private
    /// (0700 and owned by the current uid) unless `allow_shared_directory` is
    /// explicitly set, because 9P2000.L has no authentication exchange.
    #[cfg(unix)]
    pub async fn bind_unix<D, P>(driver: D, path: P, options: P9ServerOptions) -> io::Result<Self>
    where
        D: FsDriver + 'static,
        P: AsRef<Path>,
    {
        Self::bind_unix_with_hooks(driver, path, options, P9ServerHooks::default()).await
    }

    #[cfg(unix)]
    pub async fn bind_unix_with_hooks<D, P>(
        driver: D,
        path: P,
        mut options: P9ServerOptions,
        hooks: P9ServerHooks,
    ) -> io::Result<Self>
    where
        D: FsDriver + 'static,
        P: AsRef<Path>,
    {
        options.path = Some(path.as_ref().to_path_buf());
        Self::bind_unix_arc_with_hooks(Arc::new(driver), path, options, hooks).await
    }

    #[cfg(unix)]
    pub async fn bind_unix_arc<P>(
        driver: Arc<dyn FsDriver>,
        path: P,
        options: P9ServerOptions,
    ) -> io::Result<Self>
    where
        P: AsRef<Path>,
    {
        Self::bind_unix_arc_with_hooks(driver, path, options, P9ServerHooks::default()).await
    }

    #[cfg(unix)]
    pub async fn bind_unix_arc_with_hooks<P>(
        driver: Arc<dyn FsDriver>,
        path: P,
        mut options: P9ServerOptions,
        hooks: P9ServerHooks,
    ) -> io::Result<Self>
    where
        P: AsRef<Path>,
    {
        options.path = Some(path.as_ref().to_path_buf());
        let server = Self::new_arc_with_hooks(driver, options, hooks);
        server
            .bind_unix_listener(path.as_ref().to_path_buf())
            .await?;
        Ok(server)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.tcp_addr
            .lock()
            .map_err(|_| io::Error::other("TCP address lock poisoned"))?
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "server is not TCP"))
    }

    pub fn unix_path(&self) -> Option<&Path> {
        self.options.path.as_deref()
    }

    pub fn options(&self) -> &P9ServerOptions {
        &self.options
    }

    /// Return the server-owned driver for a higher-level lifecycle wrapper.
    /// The returned trait object is the same shared driver used by every
    /// session; no new ownership or filesystem instance is created.
    pub fn driver(&self) -> Arc<dyn FsDriver> {
        Arc::clone(&self.driver)
    }

    pub fn shutdown_handle(&self) -> Arc<Notify> {
        Arc::clone(&self.shutdown)
    }

    pub fn shutdown(&self) {
        // Store the state before broadcasting. The accept loop and every
        // connection task can therefore observe shutdown even when the
        // notification happens before one of them reaches its select.
        self.shutdown_requested.store(true, Ordering::Release);
        self.shutdown.notify_waiters();
    }

    /// Return live connections in arrival order.
    pub fn clients(&self) -> io::Result<Vec<P9Connection>> {
        let mut clients = self
            .connections
            .lock()
            .map_err(|_| io::Error::other("connection lock poisoned"))?
            .values()
            .cloned()
            .collect::<Vec<_>>();
        clients.sort_by_key(P9Connection::id);
        Ok(clients)
    }

    pub fn connection_count(&self) -> io::Result<usize> {
        Ok(self
            .connections
            .lock()
            .map_err(|_| io::Error::other("connection lock poisoned"))?
            .len())
    }

    /// Close every client and unlink a Unix listener path. The serve task, if
    /// one is running, observes the same shutdown notification and returns.
    pub async fn close(&self) -> io::Result<()> {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.shutdown();
            let clients = self.clients()?;
            for client in clients {
                let _ = client.close().await;
            }
            // If serve() has not taken the listener yet, drop it here. If it
            // has, its select loop drops it when the notification is seen.
            self.listener.lock().await.take();
            #[cfg(unix)]
            if let Some(path) = &self.options.path {
                match std::fs::remove_file(path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
        }
        Ok(())
    }

    /// Accept connections until [`P9Server::shutdown`] or [`P9Server::close`].
    pub async fn serve(&self) -> io::Result<()> {
        if self.serving.swap(true, Ordering::AcqRel) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "server is already serving",
            ));
        }
        let result = self.serve_inner().await;
        self.serving.store(false, Ordering::Release);
        result
    }

    /// Start accepting in a spawned Tokio task. A shared server may already be
    /// serving; callers can treat `AlreadyExists` as an existing accept loop.
    pub fn start(self: &Arc<Self>) -> io::Result<JoinHandle<io::Result<()>>> {
        if self.closed.load(Ordering::Acquire) || self.shutdown_requested.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "9P server is closed",
            ));
        }
        if self.serving.swap(true, Ordering::AcqRel) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "server is already serving",
            ));
        }
        let server = Arc::clone(self);
        Ok(tokio::spawn(async move {
            let result = server.serve_inner().await;
            server.serving.store(false, Ordering::Release);
            result
        }))
    }

    async fn serve_inner(&self) -> io::Result<()> {
        let listener = self.listener.lock().await.take();
        if self.closed.load(Ordering::Acquire) || self.shutdown_requested.load(Ordering::Acquire) {
            return Ok(());
        }
        let listener = listener.ok_or_else(|| {
            io::Error::new(io::ErrorKind::AlreadyExists, "server has no listener")
        })?;
        match listener {
            P9Listener::Tcp(listener) => self.serve_tcp(listener).await,
            #[cfg(unix)]
            P9Listener::Unix(listener) => self.serve_unix(listener).await,
        }
    }

    pub async fn run(&self) -> io::Result<()> {
        self.serve().await
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn adoption_lock(&self) -> &Mutex<()> {
        &self.adoption
    }

    /// Serve an already-connected stream, such as a socketpair end or a test
    /// duplex. The returned connection is live before this method returns.
    pub fn attach<S>(&self, stream: S, options: P9AttachOptions) -> io::Result<P9Connection>
    where
        S: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        self.attach_boxed(Box::new(stream), options)
    }

    async fn bind_tcp_listener(&self) -> io::Result<()> {
        if self.options.path.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TCP and Unix listener options cannot be combined",
            ));
        }
        let address = bind_address(&self.options.host, self.options.port);
        let listener = TcpListener::bind(&address).await?;
        let local = listener.local_addr()?;
        *self
            .tcp_addr
            .lock()
            .map_err(|_| io::Error::other("TCP address lock poisoned"))? = Some(local);
        *self.listener.lock().await = Some(P9Listener::Tcp(listener));
        Ok(())
    }

    #[cfg(unix)]
    async fn bind_unix_listener(&self, path: PathBuf) -> io::Result<()> {
        if !self.options.allow_shared_directory {
            check_socket_directory(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        let mode = std::os::unix::fs::PermissionsExt::from_mode(self.options.socket_mode);
        std::fs::set_permissions(&path, mode)?;
        *self.listener.lock().await = Some(P9Listener::Unix(listener));
        Ok(())
    }

    async fn serve_tcp(&self, listener: TcpListener) -> io::Result<()> {
        loop {
            if self.shutdown_requested.load(Ordering::Acquire) {
                return Ok(());
            }
            let shutdown_notified = self.shutdown.notified();
            tokio::pin!(shutdown_notified);
            shutdown_notified.as_mut().enable();
            if self.shutdown_requested.load(Ordering::Acquire) {
                return Ok(());
            }
            tokio::select! {
                _ = shutdown_notified => return Ok(()),
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            self.report(P9TransportError::from_io(
                                P9TransportErrorKind::Accept,
                                None,
                                &error,
                            ));
                            return Err(error);
                        }
                    };
                    if self.shutdown_requested.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    if !self.options.allow_remote && !is_loopback(peer.ip()) {
                        self.report(P9TransportError::from_message(
                            P9TransportErrorKind::PeerRefused,
                            Some(peer.to_string()),
                            "remote 9P peer is not allowed".to_owned(),
                        ));
                        continue;
                    }
                    if let Err(error) = self.attach_boxed(
                        Box::new(stream),
                        P9AttachOptions { peer: Some(peer.to_string()), own: true },
                    ) {
                        self.report(P9TransportError::from_io(
                            P9TransportErrorKind::Accept,
                            Some(peer.to_string()),
                            &error,
                        ));
                    }
                }
            }
        }
    }

    #[cfg(unix)]
    async fn serve_unix(&self, listener: UnixListener) -> io::Result<()> {
        loop {
            if self.shutdown_requested.load(Ordering::Acquire) {
                return Ok(());
            }
            let shutdown_notified = self.shutdown.notified();
            tokio::pin!(shutdown_notified);
            shutdown_notified.as_mut().enable();
            if self.shutdown_requested.load(Ordering::Acquire) {
                return Ok(());
            }
            tokio::select! {
                _ = shutdown_notified => return Ok(()),
                accepted = listener.accept() => {
                    let (stream, _) = match accepted {
                        Ok(accepted) => accepted,
                        Err(error) => {
                            self.report(P9TransportError::from_io(
                                P9TransportErrorKind::Accept,
                                self.unix_path().map(|path| path.display().to_string()),
                                &error,
                            ));
                            return Err(error);
                        }
                    };
                    if self.shutdown_requested.load(Ordering::Acquire) {
                        return Ok(());
                    }
                    if let Err(error) = self.attach_boxed(
                        Box::new(stream),
                        P9AttachOptions {
                            peer: self.unix_path().map(|path| path.display().to_string()),
                            own: true,
                        },
                    ) {
                        self.report(P9TransportError::from_io(
                            P9TransportErrorKind::Accept,
                            self.unix_path().map(|path| path.display().to_string()),
                            &error,
                        ));
                    }
                }
            }
        }
    }

    fn session_options(&self) -> P9SessionOptions {
        let mut options = self.options.session_options();
        // Every connection gets its own client identity but all clients on a
        // server must observe the same byte-range lock table.
        options.locks = Some(self.locks.clone());
        options
    }

    fn report(&self, error: P9TransportError) {
        if let Some(hook) = &self.hooks.on_transport_error {
            hook(error);
        }
    }

    fn attach_boxed(
        &self,
        stream: BoxedP9Stream,
        attach: P9AttachOptions,
    ) -> io::Result<P9Connection> {
        if self.closed.load(Ordering::Acquire) || self.shutdown_requested.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "9P server is closed",
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let control = Arc::new(ConnectionControl::new());
        let connection = P9Connection {
            session: P9Session::with_options_and_hooks(
                Arc::clone(&self.driver),
                self.session_options(),
                self.session_hooks.clone(),
            ),
            peer: attach.peer,
            id,
            control: Arc::clone(&control),
        };
        let mut connections = self
            .connections
            .lock()
            .map_err(|_| io::Error::other("connection lock poisoned"))?;
        // Recheck under the same lock used by close(). This closes the race
        // where close observes an empty client map just before attach inserts
        // the new connection.
        if self.closed.load(Ordering::Acquire) || self.shutdown_requested.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "9P server is closed",
            ));
        }
        connections.insert(id, connection.clone());
        drop(connections);
        let server_shutdown = Arc::clone(&self.shutdown);
        let server_shutdown_requested = Arc::clone(&self.shutdown_requested);
        let connections = Arc::clone(&self.connections);
        let hooks = self.hooks.clone();
        let reported = Arc::new(AtomicBool::new(false));
        let max_frame = self.options.max_frame;
        let max_in_flight = self.options.max_in_flight.max(1);
        let runtime_connection = connection.clone();
        tokio::spawn(async move {
            run_connection(ConnectionRuntime {
                stream,
                connection: runtime_connection,
                control,
                server_shutdown,
                server_shutdown_requested,
                connections,
                hooks,
                reported,
                own: attach.own,
                max_frame,
                max_in_flight,
            })
            .await;
        });
        Ok(connection)
    }
}

struct ConnectionRuntime {
    stream: BoxedP9Stream,
    connection: P9Connection,
    control: Arc<ConnectionControl>,
    server_shutdown: Arc<Notify>,
    server_shutdown_requested: Arc<AtomicBool>,
    connections: Arc<StdMutex<HashMap<u64, P9Connection>>>,
    hooks: P9ServerHooks,
    reported: Arc<AtomicBool>,
    own: bool,
    max_frame: usize,
    max_in_flight: usize,
}

enum PendingFrame {
    Request(Vec<u8>),
    Version(Vec<u8>),
}

enum FrameTaskOutcome {
    Request,
    Version,
    WriteFailed,
}

struct ReplyWrite {
    bytes: Vec<u8>,
    epoch: u64,
    version: bool,
    done: oneshot::Sender<bool>,
}

const MAX_FRAMES_PER_PARSE: usize = 4_096;

impl PendingFrame {
    fn len(&self) -> usize {
        match self {
            Self::Request(bytes) | Self::Version(bytes) => bytes.len(),
        }
    }
}

fn enqueue_frames(
    assembler: &mut P9FrameAssembler,
    chunk: &[u8],
    pending_frames: &mut VecDeque<PendingFrame>,
    pending_bytes: &mut usize,
    pending_limit: usize,
    pending_frame_limit: usize,
) -> Result<bool, P9Error> {
    let mut chunk = chunk;
    let mut version_queued = false;
    let mut parsed_frames = 0;
    loop {
        if parsed_frames >= MAX_FRAMES_PER_PARSE || pending_frames.len() >= pending_frame_limit {
            assembler.hold_unparsed(chunk);
            break;
        }
        let frame = assembler.push_one(chunk)?;
        chunk = &[];
        let Some(frame) = frame else {
            break;
        };
        parsed_frames += 1;
        // A malformed Tversion is an ordinary request error. It must not
        // discard established I/O or change the framing limit.
        let version = frame[4] == P9_TVERSION && decode_message_as(&frame, read_tversion).is_ok();
        let pending = if version {
            PendingFrame::Version(frame)
        } else {
            PendingFrame::Request(frame)
        };
        if pending.len() > pending_limit.saturating_sub(*pending_bytes) {
            return Err(P9Error::new(format!(
                "9P pending request queue exceeds {pending_limit} bytes"
            )));
        }
        *pending_bytes += pending.len();
        pending_frames.push_back(pending);
        if version {
            version_queued = true;
            break;
        }
    }
    if assembler.pending() > pending_limit.saturating_sub(*pending_bytes) {
        return Err(P9Error::new(format!(
            "9P pending request queue exceeds {pending_limit} bytes"
        )));
    }
    Ok(version_queued)
}

fn discard_prior_calls_for_version(
    pending_frames: &mut VecDeque<PendingFrame>,
    pending_bytes: &mut usize,
    tasks: &mut JoinSet<FrameTaskOutcome>,
    reply_epoch: &AtomicU64,
) {
    // Tversion aborts outstanding I/O and releases fids. Earlier requests
    // still in the queue must not start in the new session generation.
    while matches!(pending_frames.front(), Some(PendingFrame::Request(_))) {
        if let Some(frame) = pending_frames.pop_front() {
            *pending_bytes = pending_bytes.saturating_sub(frame.len());
        }
    }
    reply_epoch.fetch_add(1, Ordering::AcqRel);
    tasks.abort_all();
}

async fn run_connection(runtime: ConnectionRuntime) {
    let ConnectionRuntime {
        stream,
        connection,
        control,
        server_shutdown,
        server_shutdown_requested,
        connections,
        hooks,
        reported,
        own,
        max_frame,
        max_in_flight,
    } = runtime;
    let session = connection.session.clone();
    let (mut reader, writer) = tokio::io::split(stream);
    let writer = Arc::new(Mutex::new(writer));
    let permits = Arc::new(Semaphore::new(max_in_flight));
    let mut assembler = match P9FrameAssembler::new(max_frame.max(P9_HDRSZ)) {
        Ok(assembler) => assembler,
        Err(error) => {
            report_once(
                &hooks,
                &reported,
                P9TransportError::from_message(
                    P9TransportErrorKind::Frame,
                    connection.peer.clone(),
                    error.to_string(),
                ),
            );
            connections
                .lock()
                .ok()
                .map(|mut map| map.remove(&connection.id));
            control.finish();
            return;
        }
    };
    let mut tasks = JoinSet::<FrameTaskOutcome>::new();
    let reply_epoch = Arc::new(AtomicU64::new(0));
    let (reply_sender, mut reply_receiver) = mpsc::channel::<ReplyWrite>(max_in_flight.max(1));
    let write_control = Arc::clone(&control);
    let write_hooks = hooks.clone();
    let write_reported = Arc::clone(&reported);
    let write_peer = connection.peer.clone();
    let write_epoch = Arc::clone(&reply_epoch);
    let write_writer = Arc::clone(&writer);
    let writer_task = tokio::spawn(async move {
        while let Some(reply) = reply_receiver.recv().await {
            // Requests completed before renegotiation may be queued here.
            // A write already in progress finishes before the new Rversion.
            if !reply.version && reply.epoch != write_epoch.load(Ordering::Acquire) {
                let _ = reply.done.send(true);
                continue;
            }
            let mut writer = write_writer.lock().await;
            if !reply.version && reply.epoch != write_epoch.load(Ordering::Acquire) {
                let _ = reply.done.send(true);
                continue;
            }
            let result = match writer.write_all(&reply.bytes).await {
                Ok(()) => writer.flush().await,
                Err(error) => Err(error),
            };
            let failed = if let Err(error) = result {
                if !is_expected_disconnect(&error) {
                    report_once(
                        &write_hooks,
                        &write_reported,
                        P9TransportError::from_io(
                            P9TransportErrorKind::Write,
                            write_peer.clone(),
                            &error,
                        ),
                    );
                }
                write_control.stop();
                true
            } else {
                false
            };
            let _ = reply.done.send(!failed);
            if failed {
                break;
            }
        }
    });
    let mut buffer = vec![0_u8; 64 * 1024];
    // Keep reading while request work is at the in-flight bound so a peer's
    // EOF can still terminate a connection whose replies are backpressured.
    // The previous per-delivery permit wait could stop before the next read:
    // a paused peer then left the connection task waiting forever for a permit
    // that only a readable peer could release.
    let pending_limit = max_frame.saturating_mul(max_in_flight).max(buffer.len());
    // A byte cap alone permits millions of tiny frames and much larger queue
    // metadata. Keep one full read's minimal frames, then bound queued items.
    let pending_frame_limit = max_in_flight
        .saturating_mul(1_024)
        .clamp(buffer.len() / P9_HDRSZ, 65_536);
    let mut pending_frames: VecDeque<PendingFrame> = VecDeque::new();
    let mut pending_bytes = 0_usize;
    let mut version_fenced = false;
    let mut version_running = false;
    let mut version_aborting = false;
    let mut resume_frames = false;

    loop {
        if server_shutdown_requested.load(Ordering::Acquire) {
            break;
        }
        if resume_frames
            || (!version_fenced
                && assembler.pending() > 0
                && pending_frames.len() < pending_frame_limit)
        {
            resume_frames = false;
            match enqueue_frames(
                &mut assembler,
                &[],
                &mut pending_frames,
                &mut pending_bytes,
                pending_limit,
                pending_frame_limit,
            ) {
                Ok(fenced) => {
                    version_fenced = fenced;
                    if fenced && session.msize().is_some() {
                        discard_prior_calls_for_version(
                            &mut pending_frames,
                            &mut pending_bytes,
                            &mut tasks,
                            &reply_epoch,
                        );
                        version_aborting = true;
                    }
                }
                Err(error) => {
                    report_once(
                        &hooks,
                        &reported,
                        P9TransportError::from_message(
                            P9TransportErrorKind::Frame,
                            connection.peer.clone(),
                            error.to_string(),
                        ),
                    );
                    break;
                }
            }
        }
        let mut scan_again = false;
        if !version_fenced
            && session.msize().is_some()
            && pending_frames.len() >= pending_frame_limit
            && assembler.pending() > 0
        {
            match assembler.scan_for_version(MAX_FRAMES_PER_PARSE) {
                Ok(P9VersionScan::Found) => {
                    discard_prior_calls_for_version(
                        &mut pending_frames,
                        &mut pending_bytes,
                        &mut tasks,
                        &reply_epoch,
                    );
                    version_aborting = true;
                    match enqueue_frames(
                        &mut assembler,
                        &[],
                        &mut pending_frames,
                        &mut pending_bytes,
                        pending_limit,
                        pending_frame_limit,
                    ) {
                        Ok(true) => version_fenced = true,
                        Ok(false) => {
                            report_once(
                                &hooks,
                                &reported,
                                P9TransportError::from_message(
                                    P9TransportErrorKind::Frame,
                                    connection.peer.clone(),
                                    "9P version scan lost its complete frame".to_owned(),
                                ),
                            );
                            break;
                        }
                        Err(error) => {
                            report_once(
                                &hooks,
                                &reported,
                                P9TransportError::from_message(
                                    P9TransportErrorKind::Frame,
                                    connection.peer.clone(),
                                    error.to_string(),
                                ),
                            );
                            break;
                        }
                    }
                }
                Ok(P9VersionScan::More) => scan_again = true,
                Ok(P9VersionScan::Incomplete) => {}
                Err(error) => {
                    report_once(
                        &hooks,
                        &reported,
                        P9TransportError::from_message(
                            P9TransportErrorKind::Frame,
                            connection.peer.clone(),
                            error.to_string(),
                        ),
                    );
                    break;
                }
            }
        }
        let server_shutdown_notified = server_shutdown.notified();
        tokio::pin!(server_shutdown_notified);
        server_shutdown_notified.as_mut().enable();
        if server_shutdown_requested.load(Ordering::Acquire) {
            break;
        }
        tokio::select! {
            _ = control.shutdown.notified() => break,
            _ = server_shutdown_notified => break,
            _ = tokio::task::yield_now(), if scan_again => {},
            completed = tasks.join_next(), if !tasks.is_empty() => {
                match completed {
                    Some(Ok(FrameTaskOutcome::Version)) => {
                        version_running = false;
                        version_fenced = false;
                        version_aborting = false;
                        let limit = session.msize().map_or(max_frame.max(P9_HDRSZ), |msize| msize as usize);
                        if let Err(error) = assembler.set_limit(limit) {
                            report_once(
                                &hooks,
                                &reported,
                                P9TransportError::from_message(
                                    P9TransportErrorKind::Frame,
                                    connection.peer.clone(),
                                    error.to_string(),
                                ),
                            );
                            break;
                        }
                        resume_frames = true;
                    }
                    Some(Ok(FrameTaskOutcome::Request)) | None => {}
                    Some(Ok(FrameTaskOutcome::WriteFailed)) => break,
                    Some(Err(error)) if error.is_cancelled() && version_aborting && !version_running => {}
                    Some(Err(error)) => {
                        report_once(
                            &hooks,
                            &reported,
                            P9TransportError::from_message(
                                P9TransportErrorKind::Task,
                                connection.peer.clone(),
                                format!("9P connection task failed: {error}"),
                            ),
                        );
                        break;
                    }
                }
            }
            permit = permits.clone().acquire_owned(), if !scan_again && !pending_frames.is_empty()
                && !version_running
                && (!matches!(pending_frames.front(), Some(PendingFrame::Version(_))) || tasks.is_empty()) => {
                let permit = match permit {
                    Ok(permit) => permit,
                    Err(_) => break,
                };
                let Some(frame) = pending_frames.pop_front() else {
                    drop(permit);
                    continue;
                };
                pending_bytes = pending_bytes.saturating_sub(frame.len());
                let is_version = matches!(&frame, PendingFrame::Version(_));
                if is_version {
                    version_running = true;
                }
                let session = session.clone();
                let reply_sender = reply_sender.clone();
                let request_epoch = reply_epoch.load(Ordering::Acquire);
                let control = Arc::clone(&control);
                tasks.spawn(async move {
                    let frame = match frame {
                        PendingFrame::Request(frame) | PendingFrame::Version(frame) => frame,
                    };
                    let reply = session.handle_call(&frame).await;
                    if let Some(reply) = reply {
                        let (done, written) = oneshot::channel();
                        let queued = reply_sender
                            .send(ReplyWrite {
                                bytes: reply,
                                epoch: request_epoch,
                                version: is_version,
                                done,
                            })
                            .await;
                        if queued.is_err() || !written.await.unwrap_or(false) {
                            control.stop();
                            return FrameTaskOutcome::WriteFailed;
                        }
                    }
                    drop(permit);
                    if is_version {
                        FrameTaskOutcome::Version
                    } else {
                        FrameTaskOutcome::Request
                    }
                });
            }
            read = reader.read(&mut buffer) => {
                let count = match read {
                    Ok(count) => count,
                    Err(error) => {
                        if !is_expected_disconnect(&error) {
                            report_once(
                                &hooks,
                                &reported,
                                P9TransportError::from_io(
                                    P9TransportErrorKind::Read,
                                    connection.peer.clone(),
                                    &error,
                                ),
                            );
                        }
                        break;
                    }
                };
                if count == 0 {
                    break;
                }
                if pending_bytes.saturating_add(assembler.pending()) >= pending_limit {
                    report_once(
                        &hooks,
                        &reported,
                        P9TransportError::from_message(
                            P9TransportErrorKind::Frame,
                            connection.peer.clone(),
                            format!(
                                "9P pending request queue exceeds {} bytes",
                                pending_limit
                            ),
                        ),
                    );
                    break;
                }
                if !version_fenced && let Some(msize) = session.msize()
                    && let Err(error) = assembler.set_limit(msize as usize)
                {
                    report_once(
                        &hooks,
                        &reported,
                        P9TransportError::from_message(
                            P9TransportErrorKind::Frame,
                            connection.peer.clone(),
                            error.to_string(),
                        ),
                    );
                    break;
                }
                if version_fenced {
                    if count > pending_limit.saturating_sub(pending_bytes.saturating_add(assembler.pending())) {
                        report_once(
                            &hooks,
                            &reported,
                            P9TransportError::from_message(
                                P9TransportErrorKind::Frame,
                                connection.peer.clone(),
                                format!("9P pending request queue exceeds {pending_limit} bytes"),
                            ),
                        );
                        break;
                    }
                    assembler.hold_unparsed(&buffer[..count]);
                    continue;
                }
                match enqueue_frames(
                    &mut assembler,
                    &buffer[..count],
                    &mut pending_frames,
                    &mut pending_bytes,
                    pending_limit,
                    pending_frame_limit,
                ) {
                    Ok(fenced) => {
                        version_fenced = fenced;
                        if fenced && session.msize().is_some() {
                            discard_prior_calls_for_version(
                                &mut pending_frames,
                                &mut pending_bytes,
                                &mut tasks,
                                &reply_epoch,
                            );
                            version_aborting = true;
                        }
                    }
                    Err(error) => {
                        report_once(
                            &hooks,
                            &reported,
                            P9TransportError::from_message(
                                P9TransportErrorKind::Frame,
                                connection.peer.clone(),
                                error.to_string(),
                            ),
                        );
                        break;
                    }
                }
            }
        }
    }
    tasks.abort_all();
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result
            && !error.is_cancelled()
        {
            report_once(
                &hooks,
                &reported,
                P9TransportError::from_message(
                    P9TransportErrorKind::Task,
                    connection.peer.clone(),
                    format!("9P connection task failed: {error}"),
                ),
            );
        }
    }
    writer_task.abort();
    let _ = writer_task.await;
    if own {
        let _ = writer.lock().await.shutdown().await;
    }
    let _ = session.destroy().await;
    connections
        .lock()
        .ok()
        .map(|mut map| map.remove(&connection.id));
    control.finish();
}

fn report_once(hooks: &P9ServerHooks, reported: &AtomicBool, error: P9TransportError) {
    if reported
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
        && let Some(hook) = &hooks.on_transport_error
    {
        hook(error);
    }
}

fn is_expected_disconnect(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
    )
}

#[cfg(unix)]
fn check_socket_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let directory = path.parent().unwrap_or_else(|| Path::new("."));
    let metadata = std::fs::metadata(directory)?;
    let mode = metadata.permissions().mode() & 0o777;
    let uid = current_uid();
    if mode & 0o077 != 0 || uid != Some(metadata.uid()) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "refusing 9P socket in {}: directory must be uid {} mode 0700",
                directory.display(),
                uid.map_or_else(|| "the current user".to_owned(), |uid| uid.to_string()),
            ),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    unsafe extern "C" {
        fn geteuid() -> u32;
    }
    // `geteuid` has no failure mode and is present on both macOS and Linux.
    Some(unsafe { geteuid() })
}

fn is_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_loopback(),
        IpAddr::V6(address) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
    }
}

fn bind_address(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::P9_TCLUNK;

    #[test]
    fn one_extraction_pass_keeps_a_large_coalesced_tail_for_the_next_turn() {
        let frame = [11, 0, 0, 0, P9_TCLUNK, 7, 0, 1, 0, 0, 0];
        let burst = frame.repeat(4_097);
        let mut assembler = P9FrameAssembler::new(8_192).unwrap();
        let mut queue = VecDeque::new();
        let mut queued_bytes = 0;

        assert!(
            !enqueue_frames(
                &mut assembler,
                &burst,
                &mut queue,
                &mut queued_bytes,
                64 * 1024,
                9_362,
            )
            .unwrap()
        );
        assert!(
            queue.len() <= 4_096,
            "one turn queued {} frames",
            queue.len()
        );
        assert!(assembler.pending() > 0, "unparsed tail must remain bounded");

        assert!(
            !enqueue_frames(
                &mut assembler,
                &[],
                &mut queue,
                &mut queued_bytes,
                64 * 1024,
                9_362,
            )
            .unwrap()
        );
        assert_eq!(queue.len(), 4_097);
        assert_eq!(assembler.pending(), 0);
        assert_eq!(queued_bytes, burst.len());
    }

    #[test]
    fn deferred_tiny_frames_stay_in_raw_bytes_when_the_queue_is_full() {
        let frame = [11, 0, 0, 0, P9_TCLUNK, 7, 0, 1, 0, 0, 0];
        let burst = frame.repeat(12_000);
        let mut assembler = P9FrameAssembler::new(8_192).unwrap();
        let mut queue = VecDeque::new();
        let mut queued_bytes = 0;

        for chunk in [&burst[..], &[][..], &[][..]] {
            assert!(
                !enqueue_frames(
                    &mut assembler,
                    chunk,
                    &mut queue,
                    &mut queued_bytes,
                    256 * 1024,
                    8_192,
                )
                .unwrap()
            );
        }
        assert_eq!(queue.len(), 8_192);
        assert_eq!(assembler.pending(), (12_000 - 8_192) * frame.len());
        assert_eq!(queued_bytes + assembler.pending(), burst.len());

        for _ in 0..4_096 {
            queued_bytes -= queue.pop_front().unwrap().len();
        }
        assert!(
            !enqueue_frames(
                &mut assembler,
                &[],
                &mut queue,
                &mut queued_bytes,
                256 * 1024,
                8_192,
            )
            .unwrap()
        );
        assert_eq!(queue.len(), 12_000 - 4_096);
        assert_eq!(assembler.pending(), 0);
        assert_eq!(queued_bytes, queue.len() * frame.len());
    }
}
