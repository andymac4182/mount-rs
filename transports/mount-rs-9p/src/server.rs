//! Portable 9P listeners and attached-stream serving.
//!
//! TCP, Unix-domain sockets, and an already-connected Tokio stream all enter
//! the same per-connection session. The transport owns framing and lifecycle;
//! [`P9Session`] remains independent of the socket type so its wire behavior is
//! testable on every host supported by this crate.

use std::collections::HashMap;
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
use tokio::sync::{Mutex, Notify, Semaphore};
use tokio::task::JoinSet;

use crate::constants::{P9_DEFAULT_MAX_FRAME, P9_HDRSZ};
use crate::locks::P9LockTable;
use crate::protocol::P9FrameAssembler;
use crate::session::{P9Session, P9SessionOptions};

pub const DEFAULT_P9_PORT: u16 = 564;
pub const DEFAULT_MAX_IN_FLIGHT: usize = 16;
pub const DEFAULT_SOCKET_MODE: u32 = 0o600;

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
        self.shutdown.notify_waiters();
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
    locks: P9LockTable,
    shutdown: Arc<Notify>,
    closed: AtomicBool,
    next_id: AtomicU64,
    connections: Arc<StdMutex<HashMap<u64, P9Connection>>>,
    tcp_addr: StdMutex<Option<SocketAddr>>,
}

impl P9Server {
    /// Construct an attach-only server. Call [`P9Server::bind`] or
    /// [`P9Server::bind_unix`] when a listener is desired.
    pub fn new<D>(driver: D, options: P9ServerOptions) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::new_arc(Arc::new(driver), options)
    }

    pub fn new_arc(driver: Arc<dyn FsDriver>, options: P9ServerOptions) -> Self {
        let locks = options
            .locks
            .clone()
            .unwrap_or_else(|| P9LockTable::new(Default::default()));
        Self {
            listener: Arc::new(Mutex::new(None)),
            driver,
            options,
            locks,
            shutdown: Arc::new(Notify::new()),
            closed: AtomicBool::new(false),
            next_id: AtomicU64::new(1),
            connections: Arc::new(StdMutex::new(HashMap::new())),
            tcp_addr: StdMutex::new(None),
        }
    }

    pub async fn bind<D>(driver: D, options: P9ServerOptions) -> io::Result<Self>
    where
        D: FsDriver + 'static,
    {
        Self::bind_arc(Arc::new(driver), options).await
    }

    pub async fn bind_arc(driver: Arc<dyn FsDriver>, options: P9ServerOptions) -> io::Result<Self> {
        let server = Self::new_arc(driver, options.clone());
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
    pub async fn bind_unix<D, P>(
        driver: D,
        path: P,
        mut options: P9ServerOptions,
    ) -> io::Result<Self>
    where
        D: FsDriver + 'static,
        P: AsRef<Path>,
    {
        options.path = Some(path.as_ref().to_path_buf());
        Self::bind_unix_arc(Arc::new(driver), path, options).await
    }

    #[cfg(unix)]
    pub async fn bind_unix_arc<P>(
        driver: Arc<dyn FsDriver>,
        path: P,
        mut options: P9ServerOptions,
    ) -> io::Result<Self>
    where
        P: AsRef<Path>,
    {
        options.path = Some(path.as_ref().to_path_buf());
        let server = Self::new_arc(driver, options);
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

    pub fn shutdown_handle(&self) -> Arc<Notify> {
        Arc::clone(&self.shutdown)
    }

    pub fn shutdown(&self) {
        self.shutdown.notify_waiters();
    }

    /// Return live connections in arrival order.
    pub fn clients(&self) -> io::Result<Vec<P9Connection>> {
        Ok(self
            .connections
            .lock()
            .map_err(|_| io::Error::other("connection lock poisoned"))?
            .values()
            .cloned()
            .collect())
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
        let listener = self.listener.lock().await.take().ok_or_else(|| {
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
            tokio::select! {
                _ = self.shutdown.notified() => return Ok(()),
                accepted = listener.accept() => {
                    let (stream, peer) = accepted?;
                    if !self.options.allow_remote && !is_loopback(peer.ip()) {
                        continue;
                    }
                    self.attach_boxed(
                        Box::new(stream),
                        P9AttachOptions { peer: Some(peer.to_string()), own: true },
                    )?;
                }
            }
        }
    }

    #[cfg(unix)]
    async fn serve_unix(&self, listener: UnixListener) -> io::Result<()> {
        loop {
            tokio::select! {
                _ = self.shutdown.notified() => return Ok(()),
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    self.attach_boxed(
                        Box::new(stream),
                        P9AttachOptions {
                            peer: self.unix_path().map(|path| path.display().to_string()),
                            own: true,
                        },
                    )?;
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

    fn attach_boxed(
        &self,
        stream: BoxedP9Stream,
        attach: P9AttachOptions,
    ) -> io::Result<P9Connection> {
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "9P server is closed",
            ));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let control = Arc::new(ConnectionControl::new());
        let connection = P9Connection {
            session: P9Session::with_options(Arc::clone(&self.driver), self.session_options()),
            peer: attach.peer,
            id,
            control: Arc::clone(&control),
        };
        self.connections
            .lock()
            .map_err(|_| io::Error::other("connection lock poisoned"))?
            .insert(id, connection.clone());
        let server_shutdown = Arc::clone(&self.shutdown);
        let connections = Arc::clone(&self.connections);
        let max_frame = self.options.max_frame;
        let max_in_flight = self.options.max_in_flight.max(1);
        tokio::spawn(async move {
            run_connection(ConnectionRuntime {
                stream,
                connection,
                control,
                server_shutdown,
                connections,
                own: attach.own,
                max_frame,
                max_in_flight,
            })
            .await;
        });
        Ok(self
            .connections
            .lock()
            .map_err(|_| io::Error::other("connection lock poisoned"))?
            .get(&id)
            .cloned()
            .expect("connection inserted immediately before task spawn"))
    }
}

struct ConnectionRuntime {
    stream: BoxedP9Stream,
    connection: P9Connection,
    control: Arc<ConnectionControl>,
    server_shutdown: Arc<Notify>,
    connections: Arc<StdMutex<HashMap<u64, P9Connection>>>,
    own: bool,
    max_frame: usize,
    max_in_flight: usize,
}

async fn run_connection(runtime: ConnectionRuntime) {
    let ConnectionRuntime {
        stream,
        connection,
        control,
        server_shutdown,
        connections,
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
        Err(_) => {
            connections
                .lock()
                .ok()
                .map(|mut map| map.remove(&connection.id));
            control.finish();
            return;
        }
    };
    let mut tasks = JoinSet::new();
    let mut buffer = vec![0_u8; 64 * 1024];

    'read: loop {
        tokio::select! {
            _ = control.shutdown.notified() => break,
            _ = server_shutdown.notified() => break,
            read = reader.read(&mut buffer) => {
                let count = match read {
                    Ok(count) => count,
                    Err(_) => break,
                };
                if count == 0 {
                    break;
                }
                if session
                    .msize()
                    .is_some_and(|msize| assembler.set_limit(msize as usize).is_err())
                {
                    break;
                }
                let frames = match assembler.push(&buffer[..count]) {
                    Ok(frames) => frames,
                    Err(_) => break,
                };
                for frame in frames {
                    let permit = match permits.clone().acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => break 'read,
                    };
                    let session = session.clone();
                    let writer = Arc::clone(&writer);
                    let control = Arc::clone(&control);
                    tasks.spawn(async move {
                        let reply = session.handle_call(&frame).await;
                        if let Some(reply) = reply {
                            let mut writer = writer.lock().await;
                            if writer.write_all(&reply).await.is_err()
                                || writer.flush().await.is_err()
                            {
                                control.stop();
                            }
                        }
                        drop(permit);
                    });
                }
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
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
