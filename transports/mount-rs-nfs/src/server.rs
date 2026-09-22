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
use std::sync::{Arc, Mutex};

use mount_rs_core::{FsDriver, Loopback};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore, oneshot};
use tokio::task::{JoinHandle, JoinSet};

use crate::rpc::{DEFAULT_RECORD_LIMIT, RecordAssembler, frame_record};
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

pub struct NfsServer {
    session: Nfs3Session,
    v4_session: Nfs4Session,
    options: NfsServerOptions,
    hooks: NfsServerHooks,
    address: Arc<Mutex<Option<SocketAddr>>>,
    shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    accept_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    connection_tasks: Arc<Mutex<Vec<JoinHandle<()>>>>,
    clients: Arc<Mutex<HashMap<u64, NfsConnection>>>,
    next_connection_id: Arc<AtomicU64>,
    active_connections: Arc<AtomicUsize>,
    listen_lock: Arc<AsyncMutex<()>>,
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
        options: NfsServerOptions,
        hooks: NfsServerHooks,
    ) -> Self {
        let session = session.with_hooks(NfsSessionHooks {
            on_error: hooks.on_error.clone(),
        });
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
            address: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(Mutex::new(None)),
            accept_task: Arc::new(Mutex::new(None)),
            connection_tasks: Arc::new(Mutex::new(Vec::new())),
            clients: Arc::new(Mutex::new(HashMap::new())),
            next_connection_id: Arc::new(AtomicU64::new(1)),
            active_connections: Arc::new(AtomicUsize::new(0)),
            listen_lock: Arc::new(AsyncMutex::new(())),
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
        *self.address.lock().expect("NFS server address lock")
    }

    pub fn port(&self) -> Option<u16> {
        self.local_addr().map(|address| address.port())
    }

    /// Return live accepted clients in arrival order.
    pub fn clients(&self) -> io::Result<Vec<NfsConnection>> {
        let mut clients = self
            .clients
            .lock()
            .map_err(|_| io::Error::other("NFS connection lock poisoned"))?
            .values()
            .cloned()
            .collect::<Vec<_>>();
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
        *self.address.lock().expect("NFS server address lock") = Some(address);
        let (shutdown_sender, mut shutdown_receiver) = oneshot::channel();
        *self.shutdown.lock().expect("NFS shutdown lock") = Some(shutdown_sender);
        let session = self.session.clone();
        let v4_session = self.v4_session.clone();
        let record_limit = self.options.record_limit;
        let max_in_flight = self.options.max_in_flight.max(1);
        let allow_remote = self.options.allow_remote;
        let hooks = self.hooks.clone();
        let connection_tasks = self.connection_tasks.clone();
        let clients = self.clients.clone();
        let next_connection_id = self.next_connection_id.clone();
        let active_connections = self.active_connections.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_receiver => break,
                    result = listener.accept() => {
                        let (stream, peer) = match result {
                            Ok(accepted) => accepted,
                            Err(error) => {
                                report(&hooks, NfsTransportError::from_io(
                                    NfsTransportErrorKind::Accept,
                                    None,
                                    &error,
                                ));
                                break;
                            }
                        };
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
                        clients
                            .lock()
                            .expect("NFS connection lock")
                            .insert(id, connection);
                        active_connections.fetch_add(1, Ordering::AcqRel);
                        let guard = ConnectionGuard {
                            counter: active_connections,
                            clients,
                            control: Arc::clone(&control),
                            id,
                        };
                        let task = tokio::spawn(async move {
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
                        connection_tasks
                            .lock()
                            .expect("NFS connection task lock")
                            .push(task);
                    }
                }
            }
        });
        *self.accept_task.lock().expect("NFS accept lock") = Some(task);
        Ok(address)
    }

    pub async fn close(&self) -> io::Result<()> {
        let _listen_guard = self.listen_lock.lock().await;
        self.closed.store(true, Ordering::Release);
        if let Some(sender) = self.shutdown.lock().expect("NFS shutdown lock").take() {
            let _ = sender.send(());
        }
        let accept_task = self.accept_task.lock().expect("NFS accept lock").take();
        if let Some(task) = accept_task {
            let _ = task.await;
        }
        let clients = self.clients()?;
        for client in &clients {
            client.control.stop();
        }
        let tasks = self
            .connection_tasks
            .lock()
            .expect("NFS connection lock")
            .drain(..)
            .collect::<Vec<_>>();
        // Let serving tasks observe shutdown and drain their request workers
        // before close returns. Aborting a serving task immediately drops its
        // JoinSet without waiting for those workers to observe cancellation.
        let mut tasks = tasks;
        let mut drained = 0;
        let graceful = tokio::time::timeout(std::time::Duration::from_millis(200), async {
            for task in &mut tasks {
                let _ = task.await;
                drained += 1;
            }
        })
        .await;
        if graceful.is_err() {
            for task in &tasks[drained..] {
                task.abort();
            }
            for task in &mut tasks[drained..] {
                let _ = task.await;
            }
        }
        self.session.destroy().await;
        self.v4_session.destroy().await;
        debug_assert_eq!(self.connections(), 0);
        Ok(())
    }
}

impl Drop for NfsServer {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.lock().expect("NFS shutdown lock").take() {
            let _ = sender.send(());
        }
        if let Some(task) = self.accept_task.lock().expect("NFS accept lock").take() {
            task.abort();
        }
        for task in self
            .connection_tasks
            .lock()
            .expect("NFS connection lock")
            .drain(..)
        {
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
    clients: Arc<Mutex<HashMap<u64, NfsConnection>>>,
    control: Arc<NfsConnectionControl>,
    id: u64,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
        self.clients
            .lock()
            .expect("NFS connection lock")
            .remove(&self.id);
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
    W: AsyncWrite + Send + Unpin + 'static,
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
    let writer = Arc::new(AsyncMutex::new(writer));
    let permits = Arc::new(Semaphore::new(max_in_flight.max(1)));
    let stop = Arc::new(Notify::new());
    let mut workers = JoinSet::new();
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
            let writer = writer.clone();
            let peer = peer_name.clone();
            let error_peer = peer_name.clone();
            let hooks = hooks.clone();
            let reported = reported.clone();
            let stop = stop.clone();
            workers.spawn(async move {
                let _permit = permit;
                let context = NfsRequestContext { peer: Some(peer) };
                let reply = route_nfs_call(&session, &v4_session, &record, context).await;
                let Some(reply) = reply else {
                    return;
                };
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
                        return;
                    }
                };
                let mut writer = writer.lock().await;
                let result = match writer.write_all(&framed).await {
                    Ok(()) => writer.flush().await,
                    Err(error) => Err(error),
                };
                if let Err(error) = result {
                    if !is_expected_disconnect(&error) {
                        report_once(
                            &hooks,
                            &reported,
                            NfsTransportError::from_io(
                                NfsTransportErrorKind::Write,
                                Some(error_peer),
                                &error,
                            ),
                        );
                    }
                    stop.notify_one();
                }
            });
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
}
