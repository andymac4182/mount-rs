//! TCP transport for the NFSv3 and NFSv4.1 sessions.
//!
//! RPC record marking is handled here; all protocol and filesystem behavior
//! remains in the byte-oriented session. The default bind address is loopback
//! with an ephemeral port, which keeps wire integration tests rootless on both
//! macOS and Linux. Kernel `mount(8)` use needs a caller-selected reachable
//! address and the host's own NFS client privileges; this server does not claim
//! to perform that native mount step.

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use arc_swap::{ArcSwap, ArcSwapOption};
use mount_rs_core::diagnostics::RequestTrace;
use mount_rs_core::turn_gate::TurnLock;
use mount_rs_core::{FsDriver, Loopback};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};

use crate::rpc::{DEFAULT_RECORD_LIMIT, RecordAssembler, decode_call, frame_record};
use crate::session::{
    Nfs3Session, NfsRequestContext, NfsSessionErrorHook, NfsSessionHooks, NfsSessionOptions,
    route_nfs_call,
};
use crate::v4::Nfs4Session;

pub const DEFAULT_NFS_PORT: u16 = 2049;

/// The transport phase that terminated an NFS connection or listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NfsTransportErrorKind {
    Accept,
    PeerRefused,
    Read,
    Record,
    Write,
}

/// A transport failure reported to an embedding server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NfsTransportError {
    pub kind: NfsTransportErrorKind,
    pub peer: Option<String>,
    pub message: String,
    pub raw_os_error: Option<i32>,
}

/// Synchronous callback used by the transport task to report one terminal
/// failure. The callback receives an owned value so it can cross an embedding
/// boundary without borrowing the Tokio task or its socket.
pub type NfsTransportErrorHook = Arc<dyn Fn(NfsTransportError) + Send + Sync + 'static>;

/// Optional hooks for an [`NfsServer`]. Kept separate from
/// [`NfsServerOptions`] so existing struct literals remain source and ABI
/// compatible.
#[derive(Clone, Default)]
pub struct NfsServerHooks {
    pub on_transport_error: Option<NfsTransportErrorHook>,
    pub on_error: Option<NfsSessionErrorHook>,
}

impl NfsTransportError {
    fn from_io(kind: NfsTransportErrorKind, peer: Option<String>, error: &io::Error) -> Self {
        Self {
            kind,
            peer,
            message: error.to_string(),
            raw_os_error: error.raw_os_error(),
        }
    }

    fn from_message(kind: NfsTransportErrorKind, peer: Option<String>, message: String) -> Self {
        Self {
            kind,
            peer,
            message,
            raw_os_error: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NfsServerOptions {
    pub bind: SocketAddr,
    pub session: NfsSessionOptions,
    pub record_limit: usize,
    /// Maximum number of RPC calls dispatched concurrently per connection.
    pub max_in_flight: usize,
    /// Accept peers other than loopback. AUTH_SYS is not a security boundary,
    /// so exporting a non-loopback bind is opt-in.
    pub allow_remote: bool,
}

impl Default for NfsServerOptions {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            session: NfsSessionOptions::default(),
            record_limit: DEFAULT_RECORD_LIMIT,
            max_in_flight: 64,
            allow_remote: false,
        }
    }
}

struct NfsConnectionControl {
    shutdown: Notify,
    closed: Notify,
    done: AtomicBool,
}

impl NfsConnectionControl {
    fn new() -> Self {
        Self {
            shutdown: Notify::new(),
            closed: Notify::new(),
            done: AtomicBool::new(false),
        }
    }

    fn stop(&self) {
        // Notify retains one permit when close wins the race with the serving
        // task registering its select branch.
        self.shutdown.notify_one();
    }

    fn finish(&self) {
        self.done.store(true, Ordering::Release);
        self.closed.notify_waiters();
    }

    async fn wait(&self) {
        loop {
            let notified = self.closed.notified();
            tokio::pin!(notified);
            // Register before observing done: notify_waiters does not retain
            // a permit for a waiter that has not been registered yet.
            notified.as_mut().enable();
            if self.done.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }
}

/// One accepted NFS client and its shared v3/v4 session lifecycle.
#[derive(Clone)]
pub struct NfsConnection {
    pub session: Nfs3Session,
    pub v4_session: Nfs4Session,
    /// The accepted TCP `address:port`.
    pub peer: Option<String>,
    id: u64,
    control: Arc<NfsConnectionControl>,
}

impl NfsConnection {
    /// Stable identity useful when inspecting or closing one accepted client.
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn is_closed(&self) -> bool {
        self.control.done.load(Ordering::Acquire)
    }

    /// Request connection/session teardown and wait until the serving task is
    /// gone. NFS uses one shared session per server, so closing this object
    /// closes only its transport and leaves the server session available to
    /// other clients.
    pub async fn close(&self) -> io::Result<()> {
        self.control.stop();
        self.control.wait().await;
        Ok(())
    }

    /// Wait for EOF, an explicit close, or server shutdown.
    pub async fn wait_closed(&self) {
        self.control.wait().await;
    }
}

/// TCP server with backend file handles that require asynchronous teardown.
/// Call [`Self::close`] before dropping a server while its driver is shared
/// with another live mount. Dropping only aborts transport tasks.
pub struct NfsServer {
    session: Nfs3Session,
    v4_session: Nfs4Session,
    options: NfsServerOptions,
    hooks: NfsServerHooks,
    address: Arc<OnceLock<SocketAddr>>,
    // Stored once by `listen` and taken once by `close` or `Drop`; nothing
    // reads either in place, so a swap is all the exclusion they need.
    shutdown: ArcSwapOption<oneshot::Sender<()>>,
    /// The accept loop owns the connection tasks and hands them back when
    /// it is told to stop. Aborting it instead drops the set, which aborts
    /// every connection.
    accept_task: ArcSwapOption<JoinHandle<JoinSet<()>>>,
    /// Live connections, replaced whole on each change: the accept loop
    /// inserts, a finishing connection removes, and readers load.
    clients: Arc<ArcSwap<HashMap<u64, NfsConnection>>>,
    next_connection_id: Arc<AtomicU64>,
    active_connections: Arc<AtomicUsize>,
    listen_lock: Arc<TurnLock<()>>,
    closed: AtomicBool,
}

/// Construct an NFSv3/MOUNTv3 and NFSv4.1 TCP server backed by an [`FsDriver`].
pub fn create_nfs_server<D>(driver: D, options: NfsServerOptions) -> NfsServer
where
    D: FsDriver + 'static,
{
    NfsServer::new(driver, options)
}

/// Construct an NFS server with transport failure reporting.
pub fn create_nfs_server_with_hooks<D>(
    driver: D,
    options: NfsServerOptions,
    hooks: NfsServerHooks,
) -> NfsServer
where
    D: FsDriver + 'static,
{
    NfsServer::new_with_hooks(driver, options, hooks)
}

impl NfsServer {
    pub fn new<D>(driver: D, options: NfsServerOptions) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::new_with_hooks(driver, options, NfsServerHooks::default())
    }

    pub fn new_with_hooks<D>(driver: D, options: NfsServerOptions, hooks: NfsServerHooks) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::from_loopback_with_hooks(Loopback::new(driver), options, hooks)
    }

    pub fn from_session(session: Nfs3Session, options: NfsServerOptions) -> Self {
        Self::from_session_with_hooks(session, options, NfsServerHooks::default())
    }

    pub fn from_session_with_hooks(
        session: Nfs3Session,
        mut options: NfsServerOptions,
        hooks: NfsServerHooks,
    ) -> Self {
        let session = session.with_hooks(NfsSessionHooks {
            on_error: hooks.on_error.clone(),
        });
        // The supplied v3 session is already constructed. Its protocol
        // options are authoritative for the shared router and v4 session,
        // including the shared-view v4 refusal.
        options.session = session.options.clone();
        let shared = session.shared_state();
        let v4_session = Nfs4Session::from_loopback_shared_with_hooks(
            session.driver.clone(),
            options.session.clone(),
            &shared,
            NfsSessionHooks {
                on_error: hooks.on_error.clone(),
            },
        );
        Self {
            session,
            v4_session,
            options,
            hooks,
            address: Arc::new(OnceLock::new()),
            shutdown: ArcSwapOption::empty(),
            accept_task: ArcSwapOption::empty(),
            clients: Arc::new(ArcSwap::from_pointee(HashMap::new())),
            next_connection_id: Arc::new(AtomicU64::new(1)),
            active_connections: Arc::new(AtomicUsize::new(0)),
            listen_lock: Arc::new(TurnLock::new(())),
            closed: AtomicBool::new(false),
        }
    }

    pub fn from_loopback(driver: Loopback, options: NfsServerOptions) -> Self {
        Self::from_loopback_with_hooks(driver, options, NfsServerHooks::default())
    }

    pub fn from_loopback_with_hooks(
        driver: Loopback,
        options: NfsServerOptions,
        hooks: NfsServerHooks,
    ) -> Self {
        Self::from_session_with_hooks(
            Nfs3Session::from_loopback(driver, options.session.clone()),
            options,
            hooks,
        )
    }

    pub fn session(&self) -> &Nfs3Session {
        &self.session
    }

    pub fn v4_session(&self) -> &Nfs4Session {
        &self.v4_session
    }

    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.address.get().copied()
    }

    pub fn port(&self) -> Option<u16> {
        self.local_addr().map(|address| address.port())
    }

    /// Return live accepted clients in arrival order.
    pub fn clients(&self) -> io::Result<Vec<NfsConnection>> {
        let mut clients = self.clients.load().values().cloned().collect::<Vec<_>>();
        clients.sort_by_key(NfsConnection::id);
        Ok(clients)
    }

    /// Return the number of accepted TCP connections whose serving task has
    /// not finished yet. This is deliberately an active count rather than the
    /// number of retained join handles.
    pub fn connections(&self) -> usize {
        self.active_connections.load(Ordering::Acquire)
    }

    pub async fn listen(&self) -> io::Result<SocketAddr> {
        let _listen_guard = self.listen_lock.lock().await;
        if self.closed.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::NotConnected,
                "NFS server is closed",
            ));
        }
        if let Some(address) = self.local_addr() {
            return Ok(address);
        }
        let listener = TcpListener::bind(self.options.bind).await?;
        let address = listener.local_addr()?;
        let _ = self.address.set(address);
        let (shutdown_sender, mut shutdown_receiver) = oneshot::channel();
        self.shutdown.store(Some(Arc::new(shutdown_sender)));
        let session = self.session.clone();
        let v4_session = self.v4_session.clone();
        let record_limit = self.options.record_limit;
        let max_in_flight = self.options.max_in_flight.max(1);
        let allow_remote = self.options.allow_remote;
        let hooks = self.hooks.clone();
        let clients = self.clients.clone();
        let next_connection_id = self.next_connection_id.clone();
        let active_connections = self.active_connections.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut shutdown_receiver => break,
                    // Forget each connection as it ends, so the set holds
                    // only live ones however many have come and gone.
                    Some(_) = connections.join_next(), if !connections.is_empty() => {}
                    (stream, peer) = accept_next(|| listener.accept(), |error| {
                        report(&hooks, NfsTransportError::from_io(
                            NfsTransportErrorKind::Accept,
                            None,
                            error,
                        ));
                    }) => {
                        let session = session.clone();
                        let v4_session = v4_session.clone();
                        let hooks = hooks.clone();
                        let active_connections = active_connections.clone();
                        let clients = clients.clone();
                        let id = next_connection_id.fetch_add(1, Ordering::Relaxed);
                        let control = Arc::new(NfsConnectionControl::new());
                        let connection = NfsConnection {
                            session: session.clone(),
                            v4_session: v4_session.clone(),
                            peer: Some(peer.to_string()),
                            id,
                            control: Arc::clone(&control),
                        };
                        insert_client(&clients, connection);
                        active_connections.fetch_add(1, Ordering::AcqRel);
                        let guard = ConnectionGuard {
                            counter: active_connections,
                            clients,
                            control: Arc::clone(&control),
                            id,
                        };
                        connections.spawn(async move {
                            // The guard is constructed before spawning so an
                            // abort before the task is first polled still
                            // drops it and decrements the active count.
                            let _guard = guard;
                            serve_tcp_connection(NfsTcpConnectionRuntime {
                                stream,
                                peer,
                                session,
                                v4_session,
                                record_limit,
                                max_in_flight,
                                allow_remote,
                                hooks,
                                control,
                            })
                            .await;
                        });
                    }
                }
            }
            connections
        });
        self.accept_task.store(Some(Arc::new(task)));
        Ok(address)
    }

    pub async fn close(&self) -> io::Result<()> {
        let _listen_guard = self.listen_lock.lock().await;
        self.closed.store(true, Ordering::Release);
        if let Some(sender) = self.shutdown.swap(None).and_then(Arc::into_inner) {
            let _ = sender.send(());
        }
        let accept_task = self.accept_task.swap(None).and_then(Arc::into_inner);
        let mut connections = match accept_task {
            // A loop that failed dropped its set, which aborted its tasks.
            Some(task) => task.await.unwrap_or_default(),
            None => JoinSet::new(),
        };
        let clients = self.clients()?;
        for client in &clients {
            client.control.stop();
        }
        // Let serving tasks observe shutdown and drain their request workers
        // before close returns. Aborting a serving task immediately drops its
        // JoinSet without waiting for those workers to observe cancellation.
        let graceful = tokio::time::timeout(std::time::Duration::from_millis(200), async {
            while connections.join_next().await.is_some() {}
        })
        .await;
        if graceful.is_err() {
            connections.shutdown().await;
        }
        let v3_closed = self.session.destroy().await;
        debug_assert_eq!(self.connections(), 0);
        if !v3_closed {
            return Err(io::Error::other(
                "NFSv3 backend file handles remain open; retry server.close()",
            ));
        }
        // Both sessions share one FileHandleTable. V3 clears it only after
        // every retained descriptor has closed and released its pin; the
        // server-derived V4 session only tears down its own state.
        self.v4_session.destroy().await;
        Ok(())
    }
}

impl Drop for NfsServer {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.swap(None).and_then(Arc::into_inner) {
            let _ = sender.send(());
        }
        // Aborting the accept loop drops its connection set, which aborts
        // every connection. A loop that already stopped left the set in its
        // handle, and dropping the handle does the same.
        if let Some(task) = self.accept_task.swap(None).and_then(Arc::into_inner) {
            task.abort();
        }
    }
}

struct NfsConnectionRuntime<R, W> {
    reader: R,
    writer: W,
    peer: SocketAddr,
    session: Nfs3Session,
    v4_session: Nfs4Session,
    record_limit: usize,
    max_in_flight: usize,
    hooks: NfsServerHooks,
    reported: Arc<AtomicBool>,
    control: Arc<NfsConnectionControl>,
}

struct NfsTcpConnectionRuntime {
    stream: TcpStream,
    peer: SocketAddr,
    session: Nfs3Session,
    v4_session: Nfs4Session,
    record_limit: usize,
    max_in_flight: usize,
    allow_remote: bool,
    hooks: NfsServerHooks,
    control: Arc<NfsConnectionControl>,
}

struct ConnectionGuard {
    counter: Arc<AtomicUsize>,
    clients: Arc<ArcSwap<HashMap<u64, NfsConnection>>>,
    control: Arc<NfsConnectionControl>,
    id: u64,
}

fn insert_client(clients: &ArcSwap<HashMap<u64, NfsConnection>>, connection: NfsConnection) {
    clients.rcu(|current| {
        let mut next = HashMap::clone(current);
        next.insert(connection.id, connection.clone());
        next
    });
}

fn remove_client(clients: &ArcSwap<HashMap<u64, NfsConnection>>, id: u64) {
    clients.rcu(|current| {
        if !current.contains_key(&id) {
            return Arc::clone(current);
        }
        let mut next = HashMap::clone(current);
        next.remove(&id);
        Arc::new(next)
    });
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
        remove_client(&self.clients, self.id);
        self.control.finish();
    }
}

async fn serve_tcp_connection(runtime: NfsTcpConnectionRuntime) {
    let NfsTcpConnectionRuntime {
        mut stream,
        peer,
        session,
        v4_session,
        record_limit,
        max_in_flight,
        allow_remote,
        hooks,
        control,
    } = runtime;
    let peer_name = peer.ip().to_string();
    let reported = Arc::new(AtomicBool::new(false));
    if !allow_remote && !is_loopback(peer.ip()) {
        report_once(
            &hooks,
            &reported,
            NfsTransportError::from_message(
                NfsTransportErrorKind::PeerRefused,
                Some(peer_name),
                "remote NFS peer is not allowed".to_owned(),
            ),
        );
        let _ = stream.shutdown().await;
        return;
    }
    if let Err(error) = stream.set_nodelay(true) {
        report_once(
            &hooks,
            &reported,
            NfsTransportError::from_io(NfsTransportErrorKind::Accept, Some(peer_name), &error),
        );
        let _ = stream.shutdown().await;
        return;
    }
    let (reader, writer) = stream.into_split();
    serve_connection(NfsConnectionRuntime {
        reader,
        writer,
        peer,
        session,
        v4_session,
        record_limit,
        max_in_flight,
        hooks,
        reported,
        control,
    })
    .await;
}

async fn serve_connection<R, W>(runtime: NfsConnectionRuntime<R, W>)
where
    R: AsyncRead + Send + Unpin + 'static,
    // `Sync` because the write half travels between calls inside an `Arc`.
    W: AsyncWrite + Send + Sync + Unpin + 'static,
{
    let NfsConnectionRuntime {
        reader,
        writer,
        peer,
        session,
        v4_session,
        record_limit,
        max_in_flight,
        hooks,
        reported,
        control,
    } = runtime;
    let peer_name = peer.ip().to_string();
    let mut reader = reader;
    let permits = Arc::new(Semaphore::new(max_in_flight.max(1)));
    let stop = Arc::new(Notify::new());
    let mut workers = JoinSet::new();
    let replies = Arc::new(ReplyWriter::new(
        writer,
        max_in_flight.max(1),
        hooks.clone(),
        reported.clone(),
        peer_name.clone(),
        stop.clone(),
    ));
    let mut assembler = RecordAssembler::new(record_limit);
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        while let Some(result) = workers.try_join_next() {
            if let Err(error) = result {
                report_once(
                    &hooks,
                    &reported,
                    NfsTransportError::from_message(
                        NfsTransportErrorKind::Write,
                        Some(peer_name.clone()),
                        format!("NFS connection worker failed: {error}"),
                    ),
                );
                stop.notify_one();
                workers.shutdown().await;
                return;
            }
        }
        let count = tokio::select! {
            _ = control.shutdown.notified() => {
                workers.shutdown().await;
                return;
            }
            _ = stop.notified() => {
                workers.shutdown().await;
                return;
            }
            read = reader.read(&mut buffer) => match read {
                Ok(count) => count,
                Err(error) => {
                    if !is_expected_disconnect(&error) {
                        report_once(
                            &hooks,
                            &reported,
                            NfsTransportError::from_io(
                                NfsTransportErrorKind::Read,
                                Some(peer_name.clone()),
                                &error,
                            ),
                        );
                    }
                    workers.shutdown().await;
                    return;
                }
            }
        };
        if count == 0 {
            workers.shutdown().await;
            return;
        }
        let records = match assembler.push(&buffer[..count]) {
            Ok(records) => records,
            Err(error) => {
                report_once(
                    &hooks,
                    &reported,
                    NfsTransportError::from_message(
                        NfsTransportErrorKind::Record,
                        Some(peer_name.clone()),
                        error.to_string(),
                    ),
                );
                workers.shutdown().await;
                return;
            }
        };
        for record in records {
            let mut trace = RequestTrace::new("nfs", "rpc");
            let rpc = if trace.is_enabled() {
                match decode_call(&record) {
                    Ok((call, _)) => format!(
                        "xid={} program={} version={} procedure={}",
                        call.xid, call.program, call.version, call.procedure
                    ),
                    Err(_) => "undecodable=true".to_owned(),
                }
            } else {
                String::new()
            };
            trace.stage(
                "received",
                format_args!("{rpc} peer={peer} bytes={}", record.len()),
            );
            trace.stage("permit_wait", format_args!("{rpc}"));
            let permit = tokio::select! {
                _ = control.shutdown.notified() => {
                    workers.shutdown().await;
                    return;
                }
                _ = stop.notified() => {
                    workers.shutdown().await;
                    return;
                }
                permit = permits.clone().acquire_owned() => match permit {
                    Ok(permit) => permit,
                    Err(error) => {
                        report_once(
                            &hooks,
                            &reported,
                            NfsTransportError::from_message(
                                NfsTransportErrorKind::Read,
                                Some(peer_name.clone()),
                                format!("NFS connection semaphore closed: {error}"),
                            ),
                        );
                        workers.shutdown().await;
                        return;
                    }
                }
            };
            let session = session.clone();
            let v4_session = v4_session.clone();
            let replies = replies.clone();
            let peer = peer_name.clone();
            let error_peer = peer_name.clone();
            let hooks = hooks.clone();
            let reported = reported.clone();
            let stop = stop.clone();
            workers.spawn(async move {
                let context = NfsRequestContext { peer: Some(peer) };
                trace.stage("dispatch", format_args!("{rpc}"));
                let reply = route_nfs_call(&session, &v4_session, &record, context).await;
                let Some(reply) = reply else {
                    trace.finish(format_args!("{rpc} outcome=no_reply"));
                    return;
                };
                trace.stage("reply", format_args!("{rpc} bytes={}", reply.len()));
                let framed = match frame_record(&reply) {
                    Ok(framed) => framed,
                    Err(error) => {
                        report_once(
                            &hooks,
                            &reported,
                            NfsTransportError::from_message(
                                NfsTransportErrorKind::Record,
                                Some(error_peer.clone()),
                                error.to_string(),
                            ),
                        );
                        stop.notify_one();
                        trace.finish(format_args!("{rpc} outcome=frame_error"));
                        return;
                    }
                };
                trace.stage("socket_queue", format_args!("{rpc}"));
                replies
                    .send(QueuedReply {
                        framed,
                        trace,
                        rpc,
                        _permit: permit,
                    })
                    .await;
            });
        }
    }
}

/// A framed reply waiting for the connection's socket.
struct QueuedReply {
    framed: Vec<u8>,
    trace: RequestTrace,
    rpc: String,
    /// The call's in-flight slot, released only once the frame is written
    /// (or the connection ends). A peer that stops reading therefore stops
    /// new calls from being dispatched instead of growing the queue.
    _permit: OwnedSemaphorePermit,
}

/// How many queued replies one socket write may carry.
const REPLY_BATCH: usize = 16;

/// The write half of a connection, shared by its calls without a lock.
///
/// Record frames from concurrent calls must not interleave. Each call puts
/// its framed reply on a queue and then tries to take the write half, which
/// sits in an atomic slot. The call that gets it writes everything queued,
/// its own reply and those of calls that found the slot empty, then puts the
/// write half back. A call that finds the slot empty returns at once: the
/// holder writes its reply for it. Nobody waits for the write half, and a
/// lone call writes its own reply without handing it to another task.
///
/// The queue is bounded by the in-flight limit. A queued reply holds its
/// call's permit, so there are never more queued replies than permits and
/// the queue never makes a sender wait.
struct ReplyWriter<W> {
    queue: mpsc::Sender<QueuedReply>,
    /// `Some` while no call is writing.
    idle: ArcSwapOption<ReplySocket<W>>,
    hooks: NfsServerHooks,
    reported: Arc<AtomicBool>,
    peer: String,
    stop: Arc<Notify>,
}

/// What the writing call owns: the write half and the queue's receiving end.
struct ReplySocket<W> {
    writer: W,
    queued: mpsc::Receiver<QueuedReply>,
    batch: Vec<QueuedReply>,
}

impl<W> ReplyWriter<W>
where
    W: AsyncWrite + Send + Sync + Unpin + 'static,
{
    fn new(
        writer: W,
        max_in_flight: usize,
        hooks: NfsServerHooks,
        reported: Arc<AtomicBool>,
        peer: String,
        stop: Arc<Notify>,
    ) -> Self {
        let (queue, queued) = mpsc::channel(max_in_flight);
        Self {
            queue,
            idle: ArcSwapOption::from_pointee(ReplySocket {
                writer,
                queued,
                batch: Vec::with_capacity(REPLY_BATCH),
            }),
            hooks,
            reported,
            peer,
            stop,
        }
    }

    /// Queue `reply` and write the queue unless another call is doing so.
    async fn send(&self, reply: QueuedReply) {
        if let Err(mpsc::error::SendError(mut unsent)) = self.queue.send(reply).await {
            // A write failed and its call dropped the socket, having
            // reported the failure; the connection is closing.
            unsent
                .trace
                .finish(format_args!("{} outcome=writer_stopped", unsent.rpc));
            return;
        }
        loop {
            let Some(mut socket) = self.idle.swap(None) else {
                // Another call holds the socket. It looks at the queue again
                // after putting the socket back, so it will see this reply.
                return;
            };
            let owned = Arc::get_mut(&mut socket).expect("a taken socket has one owner");
            let mut wrote = false;
            loop {
                while owned.batch.len() < REPLY_BATCH
                    && let Ok(reply) = owned.queued.try_recv()
                {
                    owned.batch.push(reply);
                }
                if owned.batch.is_empty() {
                    break;
                }
                wrote = true;
                if !self.write_batch(owned).await {
                    // Dropping the socket closes the queue: later senders
                    // fail, and the replies still queued are discarded.
                    return;
                }
            }
            self.idle.store(Some(socket));
            // A reply queued while this call held the socket was left by a
            // sender that found the slot empty and returned. Its frame holds
            // queue capacity until it is received, so look once more now
            // that the socket is back; a sender that queues after this check
            // finds the socket itself.
            if self.queue.capacity() == self.queue.max_capacity() {
                return;
            }
            if !wrote {
                // A sender has reserved its place but not yet stored its
                // reply. It takes the socket itself when it has; give it
                // the chance instead of spinning.
                tokio::task::yield_now().await;
            }
        }
    }

    /// Write and flush the batch as whole frames. Returns `false` after a
    /// failed write, which it reports and which stops the connection.
    async fn write_batch(&self, socket: &mut ReplySocket<W>) -> bool {
        for reply in &mut socket.batch {
            reply.trace.stage(
                "socket_write_start",
                format_args!("{} bytes={}", reply.rpc, reply.framed.len()),
            );
        }
        let result = match write_frames(&mut socket.writer, &socket.batch).await {
            Ok(()) => socket.writer.flush().await,
            Err(error) => Err(error),
        };
        // Draining the batch releases its in-flight permits: only now, after
        // the frames have left, may the calls behind them be dispatched.
        for mut reply in socket.batch.drain(..) {
            reply.trace.finish(format_args!(
                "{} outcome={} os_code={:?}",
                reply.rpc,
                if result.is_ok() {
                    "written"
                } else {
                    "write_error"
                },
                result.as_ref().err().and_then(io::Error::raw_os_error),
            ));
        }
        let Err(error) = result else {
            return true;
        };
        if !is_expected_disconnect(&error) {
            report_once(
                &self.hooks,
                &self.reported,
                NfsTransportError::from_io(
                    NfsTransportErrorKind::Write,
                    Some(self.peer.clone()),
                    &error,
                ),
            );
        }
        self.stop.notify_one();
        false
    }
}

/// Write every frame of `batch` completely, in order.
async fn write_frames<W>(writer: &mut W, batch: &[QueuedReply]) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    if let [reply] = batch {
        return writer.write_all(&reply.framed).await;
    }
    let mut slices = [io::IoSlice::new(&[]); REPLY_BATCH];
    for (slice, reply) in slices.iter_mut().zip(batch) {
        *slice = io::IoSlice::new(&reply.framed);
    }
    let mut remaining = &mut slices[..batch.len()];
    while !remaining.is_empty() {
        let written = writer.write_vectored(remaining).await?;
        if written == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        io::IoSlice::advance_slices(&mut remaining, written);
    }
    Ok(())
}

/// Wait for the next connection, outliving failed accepts.
///
/// An accept fails for reasons that pass: the process is briefly out of file
/// descriptors, or a client gave up before it was accepted. Ending the
/// listener on one of those leaves a mount whose server looks alive but
/// never answers the client's next reconnect. Each failure is reported, and
/// the pause keeps a persistent one from spinning.
async fn accept_next<T, F, Fut>(mut accept: F, mut failed: impl FnMut(&std::io::Error)) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::io::Result<T>>,
{
    let mut pause = std::time::Duration::from_millis(10);
    loop {
        match accept().await {
            Ok(accepted) => return accepted,
            Err(error) => {
                failed(&error);
                tokio::time::sleep(pause).await;
                pause = (pause * 2).min(std::time::Duration::from_secs(1));
            }
        }
    }
}

fn report(hooks: &NfsServerHooks, error: NfsTransportError) {
    if let Some(hook) = &hooks.on_transport_error {
        hook(error);
    }
}

fn report_once(hooks: &NfsServerHooks, reported: &AtomicBool, error: NfsTransportError) {
    if reported
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        report(hooks, error);
    }
}

fn is_expected_disconnect(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::ConnectionReset
}

fn is_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_loopback(),
        IpAddr::V6(address) => {
            address.is_loopback() || address.to_ipv4().is_some_and(|mapped| mapped.is_loopback())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};
    use std::time::Duration;

    use super::*;
    use crate::constants::{MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_NULL};
    use crate::rpc::{RecordAssembler, decode_reply, encode_call};
    use mount_rs_memfs::MemoryFs;
    use tokio::io::{
        AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf, ReadHalf, WriteHalf,
    };
    use tokio::sync::Notify;
    use tokio::time::timeout;

    #[tokio::test]
    async fn connection_close_wakes_all_waiters_and_late_waiters() {
        let control = Arc::new(NfsConnectionControl::new());
        let waiters = (0..32)
            .map(|_| {
                let control = Arc::clone(&control);
                tokio::spawn(async move { control.wait().await })
            })
            .collect::<Vec<_>>();
        tokio::task::yield_now().await;
        control.finish();

        for waiter in waiters {
            timeout(Duration::from_secs(1), waiter)
                .await
                .expect("registered NFS close waiter wakes")
                .expect("NFS close waiter task succeeds");
        }
        timeout(Duration::from_secs(1), control.wait())
            .await
            .expect("late NFS close waiter sees completion");
    }

    #[tokio::test]
    async fn a_failed_accept_is_reported_and_the_listener_keeps_accepting() {
        let attempts = std::cell::Cell::new(0);
        let reported = std::cell::RefCell::new(Vec::new());
        let accepted = super::accept_next(
            || {
                attempts.set(attempts.get() + 1);
                let attempt = attempts.get();
                async move {
                    if attempt < 4 {
                        // EMFILE: the process is out of file descriptors.
                        Err(std::io::Error::from_raw_os_error(24))
                    } else {
                        Ok(attempt)
                    }
                }
            },
            |error| reported.borrow_mut().push(error.raw_os_error()),
        )
        .await;
        assert_eq!(accepted, 4);
        assert_eq!(*reported.borrow(), vec![Some(24); 3]);
    }

    #[tokio::test]
    async fn loopback_tcp_server_handles_rpc_without_native_mount() {
        let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
        let address = server.listen().await.unwrap();
        let mut stream = TcpStream::connect(address).await.unwrap();
        let call = encode_call(
            42,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_NULL,
            None,
            None,
            &[],
        );
        stream
            .write_all(&frame_record(&call).unwrap())
            .await
            .unwrap();
        let mut assembler = RecordAssembler::default();
        let mut buffer = [0_u8; 256];
        let count = stream.read(&mut buffer).await.unwrap();
        let records = assembler.push(&buffer[..count]).unwrap();
        assert_eq!(records.len(), 1);
        let (reply, results) = decode_reply(&records[0]).unwrap();
        assert_eq!(reply.xid, 42);
        assert_eq!(reply.accept_stat, Some(0));
        results.end("NULL reply").unwrap();
        assert_eq!(server.connections(), 1);
        let client = server.clients().unwrap().pop().expect("live NFS client");
        assert_eq!(client.id(), 1);
        assert_eq!(
            client.peer.as_deref().unwrap().split(':').next(),
            Some("127.0.0.1")
        );
        assert!(!client.is_closed());
        client.close().await.unwrap();
        assert!(client.is_closed());
        drop(stream);
        timeout(Duration::from_secs(1), async {
            while server.connections() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("connection task closes");
        assert!(server.clients().unwrap().is_empty());
        server.close().await.unwrap();
        assert_eq!(server.connections(), 0);
    }

    /// Connections coming and going from many threads at once never lose
    /// an insert or resurrect a removed entry in the copy-on-write registry.
    #[test]
    fn client_registry_keeps_every_concurrent_insert_and_remove() {
        const THREADS: u64 = 8;
        const PER_THREAD: u64 = 200;
        let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
        let clients = Arc::clone(&server.clients);
        let connection = |id: u64| NfsConnection {
            session: server.session.clone(),
            v4_session: server.v4_session.clone(),
            peer: None,
            id,
            control: Arc::new(NfsConnectionControl::new()),
        };
        let barrier = std::sync::Barrier::new(THREADS as usize);
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let clients = &clients;
                let barrier = &barrier;
                let connection = &connection;
                scope.spawn(move || {
                    barrier.wait();
                    for index in 0..PER_THREAD {
                        let id = thread * PER_THREAD + index;
                        insert_client(clients, connection(id));
                        assert!(clients.load().contains_key(&id));
                        // Odd ids end at once, as a refused peer does.
                        if id % 2 == 1 {
                            remove_client(clients, id);
                            assert!(!clients.load().contains_key(&id));
                        }
                    }
                });
            }
        });
        let mut ids = server
            .clients()
            .unwrap()
            .iter()
            .map(NfsConnection::id)
            .collect::<Vec<_>>();
        let expected = (0..THREADS * PER_THREAD)
            .filter(|id| id % 2 == 0)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, expected);
        // Removing an absent id leaves the same map in place.
        let before = clients.load_full();
        remove_client(&clients, u64::MAX);
        assert!(Arc::ptr_eq(&before, &clients.load_full()));
    }

    /// Real connections opened and dropped concurrently are each listed
    /// while live and gone once their serving task ends.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_tcp_clients_are_all_listed_and_all_forgotten() {
        let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
        let address = server.listen().await.unwrap();
        let connects = (0..32)
            .map(|_| tokio::spawn(async move { TcpStream::connect(address).await.unwrap() }))
            .collect::<Vec<_>>();
        let mut streams = Vec::new();
        for connect in connects {
            streams.push(connect.await.unwrap());
        }
        timeout(Duration::from_secs(5), async {
            while server.clients().unwrap().len() != 32 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("every accepted client is listed");
        let ids = server
            .clients()
            .unwrap()
            .iter()
            .map(NfsConnection::id)
            .collect::<Vec<_>>();
        assert_eq!(ids, (1..=32).collect::<Vec<_>>());
        let kept = streams.split_off(16);
        // Drop half while as many again connect.
        let drops = streams
            .into_iter()
            .map(|stream| tokio::spawn(async move { drop(stream) }))
            .collect::<Vec<_>>();
        let connects = (0..16)
            .map(|_| tokio::spawn(async move { TcpStream::connect(address).await.unwrap() }))
            .collect::<Vec<_>>();
        let mut later = Vec::new();
        for connect in connects {
            later.push(connect.await.unwrap());
        }
        for dropped in drops {
            dropped.await.unwrap();
        }
        timeout(Duration::from_secs(5), async {
            loop {
                let clients = server.clients().unwrap();
                if clients.len() == 32 && clients.iter().filter(|c| c.id() > 32).count() == 16 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("dropped clients leave and new ones are listed");
        assert_eq!(server.connections(), 32);
        server.close().await.unwrap();
        assert!(server.clients().unwrap().is_empty());
        assert_eq!(server.connections(), 0);
        drop((kept, later));
    }

    #[derive(Clone)]
    struct FaultEvents {
        values: Arc<Mutex<Vec<NfsTransportError>>>,
        notify: Arc<Notify>,
    }

    impl FaultEvents {
        fn new() -> (Self, NfsServerHooks) {
            let values = Arc::new(Mutex::new(Vec::new()));
            let notify = Arc::new(Notify::new());
            let callback_values = Arc::clone(&values);
            let callback_notify = Arc::clone(&notify);
            let hooks = NfsServerHooks {
                on_transport_error: Some(Arc::new(move |error| {
                    callback_values
                        .lock()
                        .expect("NFS injected event lock")
                        .push(error);
                    callback_notify.notify_waiters();
                })),
                on_error: None,
            };
            (Self { values, notify }, hooks)
        }

        fn snapshot(&self) -> Vec<NfsTransportError> {
            self.values.lock().expect("NFS injected event lock").clone()
        }

        async fn wait_for(&self, count: usize) {
            timeout(Duration::from_secs(2), async {
                loop {
                    if self.snapshot().len() >= count {
                        return;
                    }
                    self.notify.notified().await;
                }
            })
            .await
            .expect("NFS injected transport hook callback");
        }
    }

    struct FaultStream {
        input: Vec<u8>,
        offset: usize,
        read_error: Option<io::ErrorKind>,
        write_error: Option<io::ErrorKind>,
        flush_error: Option<io::ErrorKind>,
    }

    impl FaultStream {
        fn new(
            input: Vec<u8>,
            read_error: Option<io::ErrorKind>,
            write_error: Option<io::ErrorKind>,
            flush_error: Option<io::ErrorKind>,
        ) -> Self {
            Self {
                input,
                offset: 0,
                read_error,
                write_error,
                flush_error,
            }
        }
    }

    impl AsyncRead for FaultStream {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let stream = self.get_mut();
            if let Some(kind) = stream.read_error {
                return Poll::Ready(Err(io::Error::new(kind, "injected read failure")));
            }
            if stream.offset == stream.input.len() {
                return Poll::Pending;
            }
            let count = (stream.input.len() - stream.offset).min(buffer.remaining());
            buffer.put_slice(&stream.input[stream.offset..stream.offset + count]);
            stream.offset += count;
            Poll::Ready(Ok(()))
        }
    }

    impl AsyncWrite for FaultStream {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            let stream = self.get_mut();
            if let Some(kind) = stream.write_error {
                return Poll::Ready(Err(io::Error::new(kind, "injected write failure")));
            }
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            let stream = self.get_mut();
            if let Some(kind) = stream.flush_error {
                return Poll::Ready(Err(io::Error::new(kind, "injected flush failure")));
            }
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn null_call(xid: u32) -> Vec<u8> {
        frame_record(&encode_call(
            xid,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_NULL,
            None,
            None,
            &[],
        ))
        .expect("frame injected NULL call")
    }

    fn runtime(
        stream: FaultStream,
        hooks: NfsServerHooks,
        max_in_flight: usize,
    ) -> NfsConnectionRuntime<ReadHalf<FaultStream>, WriteHalf<FaultStream>> {
        let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
        let (reader, writer) = tokio::io::split(stream);
        NfsConnectionRuntime {
            reader,
            writer,
            peer: SocketAddr::from(([127, 0, 0, 1], 12345)),
            session: server.session.clone(),
            v4_session: server.v4_session.clone(),
            record_limit: DEFAULT_RECORD_LIMIT,
            max_in_flight,
            hooks,
            reported: Arc::new(AtomicBool::new(false)),
            control: Arc::new(NfsConnectionControl::new()),
        }
    }

    #[tokio::test]
    async fn injected_read_error_reports_once() {
        let (events, hooks) = FaultEvents::new();
        timeout(
            Duration::from_secs(2),
            serve_connection(runtime(
                FaultStream::new(Vec::new(), Some(io::ErrorKind::Other), None, None),
                hooks,
                1,
            )),
        )
        .await
        .expect("injected NFS read failure returns");
        events.wait_for(1).await;
        let observed = events.snapshot();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, NfsTransportErrorKind::Read);
    }

    #[tokio::test]
    async fn injected_write_and_flush_errors_report_once() {
        for (write_error, flush_error) in [
            (Some(io::ErrorKind::Other), None),
            (None, Some(io::ErrorKind::Other)),
        ] {
            let (events, hooks) = FaultEvents::new();
            timeout(
                Duration::from_secs(2),
                serve_connection(runtime(
                    FaultStream::new(null_call(1), None, write_error, flush_error),
                    hooks,
                    1,
                )),
            )
            .await
            .expect("injected NFS write failure returns");
            events.wait_for(1).await;
            let observed = events.snapshot();
            assert_eq!(observed.len(), 1);
            assert_eq!(observed[0].kind, NfsTransportErrorKind::Write);
        }
    }

    #[tokio::test]
    async fn expected_connection_reset_is_silent_on_read_write_and_flush() {
        for (input, read_error, write_error, flush_error) in [
            (Vec::new(), Some(io::ErrorKind::ConnectionReset), None, None),
            (
                null_call(1),
                None,
                Some(io::ErrorKind::ConnectionReset),
                None,
            ),
            (
                null_call(1),
                None,
                None,
                Some(io::ErrorKind::ConnectionReset),
            ),
        ] {
            let (events, hooks) = FaultEvents::new();
            timeout(
                Duration::from_secs(2),
                serve_connection(runtime(
                    FaultStream::new(input, read_error, write_error, flush_error),
                    hooks,
                    1,
                )),
            )
            .await
            .expect("expected NFS reset returns");
            assert!(events.snapshot().is_empty());
        }
    }

    #[tokio::test]
    async fn broken_pipe_write_is_reported_per_upstream_semantics() {
        let (events, hooks) = FaultEvents::new();
        timeout(
            Duration::from_secs(2),
            serve_connection(runtime(
                FaultStream::new(null_call(1), None, Some(io::ErrorKind::BrokenPipe), None),
                hooks,
                1,
            )),
        )
        .await
        .expect("NFS broken-pipe write returns");
        events.wait_for(1).await;
        let observed = events.snapshot();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, NfsTransportErrorKind::Write);
    }

    #[tokio::test]
    async fn concurrent_response_failures_report_once() {
        let (events, hooks) = FaultEvents::new();
        let mut input = null_call(1);
        input.extend_from_slice(&null_call(2));
        timeout(
            Duration::from_secs(2),
            serve_connection(runtime(
                FaultStream::new(input, None, Some(io::ErrorKind::Other), None),
                hooks,
                2,
            )),
        )
        .await
        .expect("concurrent NFS write failures return");
        events.wait_for(1).await;
        assert_eq!(events.snapshot().len(), 1);
    }

    /// A writer that accepts a few bytes at a time, across buffer
    /// boundaries, and is sometimes not ready: any two frames written
    /// without exclusion, or a frame resumed at the wrong offset, would show
    /// in the bytes it collects.
    struct ChoppyWriter {
        written: Arc<Mutex<Vec<u8>>>,
        calls: usize,
    }

    impl ChoppyWriter {
        fn accept(&mut self, cx: &mut Context<'_>, buffers: &[&[u8]]) -> Poll<io::Result<usize>> {
            self.calls += 1;
            if self.calls.is_multiple_of(5) {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
            let mut budget = 1 + (self.calls * 37) % 301;
            let mut total = 0;
            let mut written = self.written.lock().expect("choppy writer lock");
            for buffer in buffers {
                let count = buffer.len().min(budget);
                written.extend_from_slice(&buffer[..count]);
                total += count;
                budget -= count;
                if budget == 0 {
                    break;
                }
            }
            Poll::Ready(Ok(total))
        }
    }

    impl AsyncWrite for ChoppyWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.get_mut().accept(cx, &[buffer])
        }

        fn poll_write_vectored(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffers: &[io::IoSlice<'_>],
        ) -> Poll<io::Result<usize>> {
            let buffers = buffers.iter().map(|buffer| &**buffer).collect::<Vec<_>>();
            self.get_mut().accept(cx, &buffers)
        }

        fn is_write_vectored(&self) -> bool {
            true
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Many calls hand replies to one connection's writer at once. Every
    /// frame reaches the socket whole and exactly once, and none is left
    /// queued when the last call returns: a call that finds another one
    /// writing leaves its reply behind, and the writing call must pick it
    /// up however the two interleave.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_replies_are_written_whole_and_none_is_stranded() {
        const TASKS: u32 = 8;
        const PER_TASK: u32 = 1_500;
        const IN_FLIGHT: usize = 24;
        for _ in 0..4 {
            let written = Arc::new(Mutex::new(Vec::new()));
            let (events, hooks) = FaultEvents::new();
            let replies = Arc::new(ReplyWriter::new(
                ChoppyWriter {
                    written: Arc::clone(&written),
                    calls: 0,
                },
                IN_FLIGHT,
                hooks,
                Arc::new(AtomicBool::new(false)),
                "127.0.0.1".to_owned(),
                Arc::new(Notify::new()),
            ));
            let permits = Arc::new(Semaphore::new(IN_FLIGHT));
            let tasks = (0..TASKS)
                .map(|task| {
                    let replies = Arc::clone(&replies);
                    let permits = Arc::clone(&permits);
                    tokio::spawn(async move {
                        for sequence in 0..PER_TASK {
                            let id = task * PER_TASK + sequence;
                            // The body repeats the id, so a frame cut by
                            // another is not mistaken for a whole one.
                            let words = 1 + (id as usize * 7) % 190;
                            let body = id.to_be_bytes().repeat(words);
                            let permit = permits.clone().acquire_owned().await.unwrap();
                            replies
                                .send(QueuedReply {
                                    framed: frame_record(&body).unwrap(),
                                    trace: RequestTrace::new("nfs", "rpc"),
                                    rpc: String::new(),
                                    _permit: permit,
                                })
                                .await;
                            if sequence % 64 == 0 {
                                tokio::task::yield_now().await;
                            }
                        }
                    })
                })
                .collect::<Vec<_>>();
            for task in tasks {
                timeout(Duration::from_secs(30), task)
                    .await
                    .expect("reply senders finish")
                    .unwrap();
            }
            // No call is running any more, so nothing may still be queued
            // and every in-flight slot must be free again.
            assert_eq!(permits.available_permits(), IN_FLIGHT);
            assert!(replies.idle.load().is_some(), "the socket was put back");
            let written = written.lock().unwrap();
            let mut seen = vec![false; (TASKS * PER_TASK) as usize];
            for record in RecordAssembler::new(DEFAULT_RECORD_LIMIT)
                .push(&written)
                .expect("written bytes are whole record frames")
            {
                let id = u32::from_be_bytes(record[..4].try_into().unwrap());
                assert_eq!(record.len(), 4 * (1 + (id as usize * 7) % 190));
                assert!(record.chunks(4).all(|word| word == id.to_be_bytes()));
                assert!(
                    !std::mem::replace(&mut seen[id as usize], true),
                    "duplicate"
                );
            }
            assert!(seen.iter().all(|seen| *seen), "a reply was never written");
            assert!(events.snapshot().is_empty());
        }
    }

    /// The same through the connection loop: pipelined calls with replies
    /// of two sizes, answered concurrently over a writer that takes a few
    /// bytes at a time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pipelined_calls_get_whole_replies_through_a_choppy_socket() {
        const CALLS: u32 = 600;
        let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
        let mut input = Vec::new();
        for xid in 1..=CALLS {
            if xid % 3 == 0 {
                input.extend(
                    frame_record(&encode_call(
                        xid,
                        MOUNT_PROGRAM,
                        MOUNT_V3,
                        crate::constants::MOUNTPROC3_MNT,
                        None,
                        None,
                        &crate::xdr::encode_xdr(|writer| writer.string("/")),
                    ))
                    .unwrap(),
                );
            } else {
                input.extend(null_call(xid));
            }
        }
        let written = Arc::new(Mutex::new(Vec::new()));
        let control = Arc::new(NfsConnectionControl::new());
        let (events, hooks) = FaultEvents::new();
        let task = tokio::spawn(serve_connection(NfsConnectionRuntime {
            reader: FaultStream::new(input, None, None, None),
            writer: ChoppyWriter {
                written: Arc::clone(&written),
                calls: 0,
            },
            peer: SocketAddr::from(([127, 0, 0, 1], 12345)),
            session: server.session.clone(),
            v4_session: server.v4_session.clone(),
            record_limit: DEFAULT_RECORD_LIMIT,
            max_in_flight: 16,
            hooks,
            reported: Arc::new(AtomicBool::new(false)),
            control: Arc::clone(&control),
        }));
        let replies = timeout(Duration::from_secs(30), async {
            loop {
                let records = RecordAssembler::new(DEFAULT_RECORD_LIMIT)
                    .push(&written.lock().unwrap())
                    .expect("replies are whole record frames");
                if records.len() == CALLS as usize {
                    return records;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("every pipelined call is answered");
        let mut sizes = std::collections::HashMap::new();
        let mut xids = std::collections::HashSet::new();
        for record in &replies {
            let (reply, _) = decode_reply(record).expect("decode reply");
            assert_eq!(reply.accept_stat, Some(0));
            assert!(xids.insert(reply.xid), "duplicate reply");
            let size = sizes.entry(reply.xid % 3 == 0).or_insert(record.len());
            assert_eq!(*size, record.len(), "replies of one kind have one size");
        }
        assert_eq!(xids.len(), CALLS as usize);
        assert_ne!(sizes[&true], sizes[&false]);
        control.stop();
        timeout(Duration::from_secs(5), task)
            .await
            .expect("connection stops")
            .unwrap();
        assert!(events.snapshot().is_empty());
    }

    #[tokio::test]
    async fn close_cancels_a_backpressured_reply_and_queued_request() {
        let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
        let (mut request_writer, request_reader) = tokio::io::duplex(1024);
        let (reply_writer, mut reply_reader) = tokio::io::duplex(16);
        let control = Arc::new(NfsConnectionControl::new());
        let (events, hooks) = FaultEvents::new();
        let task = tokio::spawn(serve_connection(NfsConnectionRuntime {
            reader: request_reader,
            writer: reply_writer,
            peer: SocketAddr::from(([127, 0, 0, 1], 12345)),
            session: server.session.clone(),
            v4_session: server.v4_session.clone(),
            record_limit: DEFAULT_RECORD_LIMIT,
            max_in_flight: 1,
            hooks,
            reported: Arc::new(AtomicBool::new(false)),
            control: Arc::clone(&control),
        }));

        let mut requests = null_call(1);
        requests.extend_from_slice(&null_call(2));
        request_writer
            .write_all(&requests)
            .await
            .expect("write both pipelined calls");
        timeout(Duration::from_secs(2), async {
            while server.session().stats().replies < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("first RPC reaches reply writer");
        assert_eq!(server.session().stats().requests, 1);

        // The peer deliberately does not read its 16-byte reply buffer. The
        // first worker holds the only permit while write_all is pending, so
        // the second framed RPC must remain undispatched.
        assert!(
            timeout(Duration::from_millis(100), async {
                while server.session().stats().requests < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .is_err(),
            "backpressured reply must retain the in-flight slot"
        );

        control.stop();
        timeout(Duration::from_secs(2), task)
            .await
            .expect("NFS connection closes despite stalled write")
            .expect("NFS connection task succeeds");
        let mut partial_reply = Vec::new();
        timeout(
            Duration::from_secs(2),
            reply_reader.read_to_end(&mut partial_reply),
        )
        .await
        .expect("canceled writer closes reply stream")
        .expect("read partial NFS reply");
        assert_eq!(partial_reply.len(), 16, "reply writer filled its buffer");
        assert_eq!(server.session().stats().requests, 1);
        assert!(events.snapshot().is_empty(), "close is not a write error");
    }

    // ---- listen_lock ------------------------------------------------------

    /// `listen` has no await between finding no address and storing the one
    /// it bound, so two calls only meet inside it on real threads.
    #[test]
    fn concurrent_listens_from_real_threads_bind_one_listener() {
        const ROUNDS: usize = if cfg!(windows) { 20 } else { 200 };
        const CALLERS: usize = 4;
        for round in 0..ROUNDS {
            let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
            let barrier = std::sync::Barrier::new(CALLERS);
            let addresses = std::thread::scope(|scope| {
                let callers = (0..CALLERS)
                    .map(|_| {
                        scope.spawn(|| {
                            let runtime = tokio::runtime::Builder::new_current_thread()
                                .enable_all()
                                .build()
                                .expect("test runtime");
                            barrier.wait();
                            runtime.block_on(server.listen()).expect("listen")
                        })
                    })
                    .collect::<Vec<_>>();
                callers
                    .into_iter()
                    .map(|caller| caller.join().expect("listen caller"))
                    .collect::<Vec<_>>()
            });
            let address = server.local_addr().expect("listening address");
            assert!(
                addresses.iter().all(|returned| *returned == address),
                "round {round}: a caller was given a listener the server does not own: \
                 {addresses:?}, server at {address}"
            );
        }
    }

    /// A file whose READ never answers and, once cancelled, does not let go
    /// until the test says so, as a backend stuck in a blocking call does
    /// not. Its connection lasts until a close stops waiting for it.
    struct HeldReadDriver {
        backing: MemoryFs,
        entered: Arc<Notify>,
        release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
    }

    struct HeldReadHandle {
        inner: Arc<dyn mount_rs_core::FileHandle>,
        entered: Arc<Notify>,
        release: Arc<Mutex<std::sync::mpsc::Receiver<()>>>,
    }

    struct HeldUntilReleased(Arc<Mutex<std::sync::mpsc::Receiver<()>>>);

    impl Drop for HeldUntilReleased {
        fn drop(&mut self) {
            // Returns when the test sends, or drops its sender.
            let _ = self.0.lock().unwrap().recv();
        }
    }

    #[async_trait::async_trait]
    impl mount_rs_core::FileHandle for HeldReadHandle {
        async fn read(
            &self,
            _buffer: &mut [u8],
            _position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            let _held = HeldUntilReleased(Arc::clone(&self.release));
            self.entered.notify_one();
            std::future::pending().await
        }

        async fn write(
            &self,
            buffer: &[u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            self.inner.write(buffer, position).await
        }

        async fn stat(&self) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.inner.stat().await
        }

        async fn truncate(&self, length: u64) -> mount_rs_core::Result<()> {
            self.inner.truncate(length).await
        }

        async fn close(&self) -> mount_rs_core::Result<()> {
            self.inner.close().await
        }
    }

    #[async_trait::async_trait]
    impl mount_rs_core::FsDriver for HeldReadDriver {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            self.backing.capabilities()
        }

        async fn stat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.backing.stat(path).await
        }

        async fn lstat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
            self.backing.lstat(path).await
        }

        async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
            self.backing.readdir(path).await
        }

        async fn open(
            &self,
            path: &str,
            flags: &str,
            mode: u32,
        ) -> mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>> {
            Ok(Arc::new(HeldReadHandle {
                inner: self.backing.open(path, flags, mode).await?,
                entered: Arc::clone(&self.entered),
                release: Arc::clone(&self.release),
            }))
        }

        async fn open_flags(
            &self,
            path: &str,
            flags: mount_rs_core::OpenFlags,
            mode: u32,
        ) -> mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>> {
            Ok(Arc::new(HeldReadHandle {
                inner: self.backing.open_flags(path, flags, mode).await?,
                entered: Arc::clone(&self.entered),
                release: Arc::clone(&self.release),
            }))
        }
    }

    /// The first close is draining a connection whose READ is still in the
    /// backend. READ holds no path lock, so nothing but `listen_lock` keeps
    /// a second close, which finds the accept task already taken and has
    /// nothing of its own to drain, from destroying the session under it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_second_close_waits_for_the_first_to_drain_its_connections() {
        let backing = MemoryFs::empty();
        let file = backing.open("/file", "w+", 0o666).await.unwrap();
        file.close().await.unwrap();
        let stats = mount_rs_core::FsDriver::stat(&backing, "/file")
            .await
            .unwrap();
        let entered = Arc::new(Notify::new());
        let (release, held) = std::sync::mpsc::channel();
        let server = Arc::new(NfsServer::new(
            HeldReadDriver {
                backing,
                entered: Arc::clone(&entered),
                release: Arc::new(Mutex::new(held)),
            },
            NfsServerOptions::default(),
        ));
        let handles = &server.session().handles;
        let file = handles.encode(&handles.bind("/file", &stats));
        let address = server.listen().await.unwrap();
        let mut stream = TcpStream::connect(address).await.unwrap();
        let read = encode_call(
            7,
            crate::constants::NFS_PROGRAM,
            crate::constants::NFS_V3,
            crate::constants::NFSPROC3_READ,
            None,
            None,
            &crate::xdr::encode_xdr(|writer| {
                crate::protocol::write_read_args(
                    writer,
                    &crate::protocol::Read3args {
                        file,
                        offset: 0,
                        count: 16,
                    },
                );
            }),
        );
        stream
            .write_all(&frame_record(&read).unwrap())
            .await
            .unwrap();
        timeout(Duration::from_secs(5), entered.notified())
            .await
            .expect("READ reaches the backend");

        let first = tokio::spawn({
            let server = Arc::clone(&server);
            async move { server.close().await }
        });
        // `closed` is set by a close that has its turn. Looking at the
        // accept task itself would hold a reference to it, and close takes
        // the task only when it holds the last one.
        timeout(Duration::from_secs(5), async {
            while !server.closed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the first close starts");
        let second = tokio::spawn({
            let server = Arc::clone(&server);
            async move { server.close().await }
        });
        // The held READ keeps the connection alive until the first close
        // gives up on draining it. A second close that did not wait
        // reaches the session at once, well inside that time.
        let mut live_when_destroyed = None;
        while !first.is_finished() && live_when_destroyed.is_none() {
            if server.session().destroyed() {
                live_when_destroyed = Some(server.connections());
            }
            tokio::task::yield_now().await;
        }
        release.send(()).unwrap();
        assert!(
            live_when_destroyed.is_none_or(|live| live == 0),
            "a second close destroyed the session while the first was still draining \
             {live_when_destroyed:?} connection(s)"
        );
        first.await.expect("first close task").expect("first close");
        second
            .await
            .expect("second close task")
            .expect("second close");
        assert_eq!(server.connections(), 0);
    }
}
