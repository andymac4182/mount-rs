use crate::*;
use async_trait::async_trait;
use bytes::Bytes;
use mount_rs_core::diagnostics::storage::{Operation, Span};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::{Future, poll_fn},
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    sync::atomic::{AtomicU8, AtomicU64, Ordering},
    task::{Context, Poll, Waker},
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore},
    time::timeout,
};
const MAX_HEADER_BYTES: usize = 25 + 4 * 1024;
// Cleanup is separate from the unchanged RPC/stream deadline. Quinn closes a
// connection after 3*PTO; its bootstrap RTT/variance give a ~2.997s close phase.
// Allow that protocol phase plus 1s for task wakeups. A slower/lost drain still
// fails with its owners retained; this is not a guarantee for arbitrary RTTs.
const PEER_DRAIN_ALLOWANCE: Duration = Duration::from_secs(4);
fn peer_transfer_charge(max: usize) -> Result<u32> {
    max.checked_add(MAX_HEADER_BYTES)
        .and_then(|n| n.checked_mul(2))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(error)
}
struct ChargedPayload {
    bytes: Bytes,
    _charge: Arc<tokio::sync::OwnedSemaphorePermit>,
}
impl AsRef<[u8]> for ChargedPayload {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
enum PeerPayload<'a> {
    Borrowed(&'a [u8]),
    Shared(Bytes),
}
impl PeerPayload<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Borrowed(b) => b.len(),
            Self::Shared(b) => b.len(),
        }
    }
    fn owned(self) -> Bytes {
        match self {
            Self::Borrowed(b) => Bytes::copy_from_slice(b),
            Self::Shared(b) => b,
        }
    }
}
#[derive(Clone, Debug)]
pub struct PeerEndpoint {
    pub address: SocketAddr,
    pub server_name: String,
    pub certificate_sha256: [u8; 32],
    pub partitions: BTreeSet<String>,
}
pub struct QuicPeerConfig {
    pub local: PeerId,
    pub bind: SocketAddr,
    pub certificates: Vec<CertificateDer<'static>>,
    pub private_key: PrivateKeyDer<'static>,
    pub roots: rustls::RootCertStore,
    pub trusted: BTreeMap<PeerId, PeerEndpoint>,
    pub max_blob_bytes: usize,
    pub max_inflight: usize,
    /// Total byte admission shared across peers, split between serving and requesting.
    pub transfer_bytes: usize,
    pub deadline: Duration,
}
/// Negotiation is isolated by role. Both slots may share one authenticated live
/// connection. Each peer has at most two guarded negotiations and cached slots;
/// Quinn can retain additional draining state after cancelled attempts.
#[derive(Default)]
struct PeerConnections {
    reads: Mutex<Option<quinn::Connection>>,
    placements: Mutex<Option<quinn::Connection>>,
}
#[cfg(all(test, unix))]
struct TestSessionExitGate {
    entered: tokio::sync::oneshot::Sender<()>,
    release: tokio::sync::oneshot::Receiver<()>,
}
#[derive(Default)]
struct PeerRequestState {
    stopped: bool,
    active: usize,
    failure: Option<FsError>,
    waker: Option<Waker>,
}
#[derive(Default)]
struct PeerRequests(std::sync::Mutex<PeerRequestState>);
impl PeerRequests {
    fn lock(&self) -> std::sync::MutexGuard<'_, PeerRequestState> {
        match self.0.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.stopped = true;
                state.failure.get_or_insert_with(error);
                state
            }
        }
    }
    fn enter(self: &Arc<Self>, permits: &Arc<Semaphore>) -> Result<PeerRequest> {
        let mut state = self.lock();
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.stopped {
            return Err(error());
        }
        let permit = permits.clone().try_acquire_owned().map_err(|_| error())?;
        state.active = state.active.checked_add(1).ok_or_else(error)?;
        Ok(PeerRequest {
            requests: self.clone(),
            permit: Some(permit),
        })
    }
    fn seal(&self) {
        self.lock().stopped = true;
    }
    fn poll_closed(&self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let mut state = self.lock();
        if let Some(error) = &state.failure {
            return Poll::Ready(Err(error.clone()));
        }
        if state.active == 0 {
            return Poll::Ready(Ok(()));
        }
        state.waker = Some(cx.waker().clone());
        Poll::Pending
    }
}
struct PeerRequest {
    requests: Arc<PeerRequests>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
}
impl Drop for PeerRequest {
    fn drop(&mut self) {
        drop(self.permit.take());
        let waker = {
            let mut state = self.requests.lock();
            match state.active.checked_sub(1) {
                Some(active) => state.active = active,
                None => {
                    state.stopped = true;
                    state.failure.get_or_insert_with(error);
                }
            }
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}
// Sessions own resource data, never the lifecycle that owns their joins.
struct PeerCore {
    endpoint: quinn::Endpoint,
    config: QuicPeerConfig,
    cache: Arc<LocalCache>,
    connections: BTreeMap<PeerId, Arc<PeerConnections>>,
    permits: Arc<Semaphore>,
    inbound: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    receive_bytes: Arc<Semaphore>,
    request_bytes: Arc<Semaphore>,
    transfer_charge: u32,
    stop: tokio::sync::watch::Sender<bool>,
    requests: Arc<PeerRequests>,
    #[cfg(all(test, unix))]
    test_session_exit_gate: std::sync::Mutex<Option<TestSessionExitGate>>,
}
type PeerCloseFuture = Pin<Box<dyn Future<Output = Result<()>> + Send>>;
#[derive(Default)]
struct PeerCloseState {
    started: bool,
    finished: bool,
    failure: Option<FsError>,
    current: Option<PeerCloseFuture>,
    // A completed error future may already have dropped its captures.
    resources: Option<Arc<PeerCore>>,
    // Unproven cleanup deliberately retains its actual resources and joins.
    retained: Option<Arc<PeerClose>>,
    #[cfg(all(test, unix))]
    worker: Option<tokio::task::AbortHandle>,
}
struct PeerClose {
    core: std::sync::Weak<PeerCore>,
    deadline: Duration,
    supervisor: Arc<crate::owned::OwnedTask>,
    state: std::sync::Mutex<PeerCloseState>,
    completed: tokio::sync::watch::Sender<Option<Result<()>>>,
    phase: AtomicU8,
    supervisor_join_micros: AtomicU64,
    requests_drained_micros: AtomicU64,
}
impl PeerClose {
    fn lock(&self) -> std::sync::MutexGuard<'_, PeerCloseState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.failure.get_or_insert_with(error);
                state
            }
        }
    }
    fn start(self: &Arc<Self>) {
        let mut state = self.lock();
        if state.started {
            return;
        }
        state.started = true;
        state.retained = Some(self.clone());
        let Some(core) = self.core.upgrade() else {
            drop(state);
            self.finish(Err(error()));
            return;
        };
        core.seal();
        state.resources = Some(core.clone());
        let supervisor = self.supervisor.clone();
        let progress = self.clone();
        let started = std::time::Instant::now();
        self.phase.store(1, Ordering::Release);
        state.current = Some(Box::pin(async move {
            supervisor.join().await?;
            progress.supervisor_join_micros.store(
                started.elapsed().as_micros().min(u64::MAX as u128) as u64,
                Ordering::Release,
            );
            progress.phase.store(2, Ordering::Release);
            poll_fn(|cx| core.requests.poll_closed(cx)).await?;
            progress.requests_drained_micros.store(
                started.elapsed().as_micros().min(u64::MAX as u128) as u64,
                Ordering::Release,
            );
            progress.phase.store(3, Ordering::Release);
            core.endpoint.wait_idle().await;
            progress.phase.store(4, Ordering::Release);
            Ok(())
        }));
        drop(state);
        let runtime = match tokio::runtime::Handle::try_current() {
            Ok(runtime) => runtime,
            Err(_) => {
                self.finish(Err(error()));
                return;
            }
        };
        let guard = PeerCloseGuard {
            close: self.clone(),
            acknowledged: false,
        };
        let _worker = runtime.spawn(guard.run());
        #[cfg(all(test, unix))]
        {
            self.lock().worker = Some(_worker.abort_handle());
        }
    }
    fn poll_close(&self, cx: &mut Context<'_>) -> Poll<Result<()>> {
        let mut state = self.lock();
        if let Some(error) = &state.failure {
            return Poll::Ready(Err(error.clone()));
        }
        match state.current.as_mut() {
            Some(current) => current.as_mut().poll(cx),
            None => Poll::Ready(Err(error())),
        }
    }
    fn finish(&self, result: Result<()>) {
        let result = {
            let mut state = self.lock();
            if state.finished {
                return;
            }
            state.finished = true;
            let result = state.failure.clone().map_or(result, Err);
            if result.is_ok() {
                state.current = None;
                state.resources = None;
                state.retained = None;
            }
            result
        };
        self.completed.send_replace(Some(result));
    }
    async fn join(self: &Arc<Self>) -> Result<()> {
        let mut completed = self.completed.subscribe();
        self.start();
        loop {
            if let Some(result) = completed.borrow().clone() {
                return result;
            }
            completed.changed().await.map_err(|_| error())?;
        }
    }
    fn timeout_error(&self) -> FsError {
        let phase = self.phase.load(Ordering::Acquire);
        let stage = match phase {
            1 => "supervisor",
            2 => "requests",
            3 => "endpoint_idle",
            4 => "complete",
            _ => "not_started",
        };
        let core = self.core.upgrade();
        let requests = core.as_ref().map(|core| core.requests.lock().active);
        let connections = core.as_ref().map(|core| core.endpoint.open_connections());
        FsError::new(ErrorCode::Ebusy).with_syscall("cache peer drain timeout")
            .with_message(format!("stage={stage} supervisor_joined={} request_owners={requests:?} endpoint_connections={connections:?} supervisor_join_us={} requests_drained_us={}", phase >= 2, self.supervisor_join_micros.load(Ordering::Acquire), self.requests_drained_micros.load(Ordering::Acquire)))
    }
}
struct PeerCloseGuard {
    close: Arc<PeerClose>,
    acknowledged: bool,
}
impl PeerCloseGuard {
    async fn run(mut self) {
        let result = timeout(self.close.deadline, poll_fn(|cx| self.close.poll_close(cx)))
            .await
            .unwrap_or_else(|_| Err(self.close.timeout_error()));
        self.close.finish(result);
        self.acknowledged = true;
    }
}
impl Drop for PeerCloseGuard {
    fn drop(&mut self) {
        if !self.acknowledged {
            self.close.finish(Err(error()));
        }
    }
}
pub struct QuicPeerTransport {
    core: Arc<PeerCore>,
    close: Arc<PeerClose>,
}
impl QuicPeerTransport {
    pub fn bind(config: QuicPeerConfig, cache: Arc<LocalCache>) -> Result<Arc<Self>> {
        if config.deadline.is_zero()
            || config.deadline > Duration::from_secs(60)
            || config.max_blob_bytes > 256 * 1024 * 1024
            || config.max_inflight == 0
            || config.max_inflight > 65535
            || config.max_blob_bytes == 0
            || config.max_blob_bytes > usize::MAX - 8192
            || config.trusted.len() > 1024
        {
            return Err(error());
        }
        let transfer_charge = peer_transfer_charge(config.max_blob_bytes)?;
        if config.transfer_bytes > 1024 * 1024 * 1024
            || config.transfer_bytes / 2 < transfer_charge as usize
        {
            return Err(error());
        }
        let mut pins = BTreeSet::new();
        if config.trusted.iter().any(|(id, p)| {
            id.0.is_empty()
                || id.0.len() > 1024
                || !pins.insert(p.certificate_sha256)
                || p.partitions.is_empty()
        }) {
            return Err(error());
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(config.roots.clone()),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .map_err(|_| error())?;
        let mut server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| error())?
        .with_client_cert_verifier(verifier)
        .with_single_cert(config.certificates.clone(), config.private_key.clone_key())
        .map_err(|_| error())?;
        server.alpn_protocols = vec![b"mount-rs-blob-cache/1".to_vec()];
        let mut server = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server).map_err(|_| error())?,
        ));
        let mut limits = quinn::TransportConfig::default();
        limits.max_concurrent_bidi_streams((config.max_inflight as u32).into());
        limits.max_concurrent_uni_streams(0u32.into());
        limits.max_idle_timeout(Some(
            Duration::from_secs(30).try_into().map_err(|_| error())?,
        ));
        limits.stream_receive_window(((config.max_blob_bytes + MAX_HEADER_BYTES) as u32).into());
        limits.receive_window(
            (u32::try_from(config.transfer_bytes / 2).map_err(|_| error())?).into(),
        );
        limits.send_window((config.transfer_bytes / 2).min(8 * 1024 * 1024) as u64);
        let limits = Arc::new(limits);
        let outbound_limits = limits.clone();
        server.transport_config(limits);
        let mut endpoint = quinn::Endpoint::server(server, config.bind).map_err(|_| error())?;
        let mut client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| error())?
        .with_root_certificates(config.roots.clone())
        .with_client_auth_cert(config.certificates.clone(), config.private_key.clone_key())
        .map_err(|_| error())?;
        client.alpn_protocols = vec![b"mount-rs-blob-cache/1".to_vec()];
        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client).map_err(|_| error())?,
        ));
        client_config.transport_config(outbound_limits);
        endpoint.set_default_client_config(client_config);
        let (stop, _) = tokio::sync::watch::channel(false);
        let connections = config
            .trusted
            .keys()
            .map(|peer| (peer.clone(), Arc::new(PeerConnections::default())))
            .collect();
        let core = Arc::new(PeerCore {
            endpoint,
            permits: Arc::new(Semaphore::new(config.max_inflight)),
            inbound: Arc::new(Semaphore::new(config.max_inflight)),
            streams: Arc::new(Semaphore::new(config.max_inflight)),
            receive_bytes: Arc::new(Semaphore::new(config.transfer_bytes / 2)),
            request_bytes: Arc::new(Semaphore::new(config.transfer_bytes / 2)),
            transfer_charge,
            config,
            cache,
            connections,
            stop,
            requests: Arc::new(PeerRequests::default()),
            #[cfg(all(test, unix))]
            test_session_exit_gate: std::sync::Mutex::new(None),
        });
        let supervisor_core = core.clone();
        let supervisor =
            crate::owned::OwnedTask::new(tokio::spawn(
                async move { supervisor_core.supervise().await },
            ));
        let (completed, _) = tokio::sync::watch::channel(None);
        let close = Arc::new(PeerClose {
            core: Arc::downgrade(&core),
            deadline: core.config.deadline + PEER_DRAIN_ALLOWANCE,
            supervisor,
            state: std::sync::Mutex::new(PeerCloseState::default()),
            completed,
            phase: AtomicU8::new(0),
            supervisor_join_micros: AtomicU64::new(0),
            requests_drained_micros: AtomicU64::new(0),
        });
        Ok(Arc::new(Self { core, close }))
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.core.endpoint.local_addr().map_err(|_| error())
    }
    /// Snapshot admitted inbound handshake/session owners, bounded by max_inflight.
    /// This includes unauthenticated handshakes and is not a TLS or drain ACK.
    /// Sealing admission does not erase permits still held by active sessions.
    pub fn active_inbound_sessions(&self) -> usize {
        self.core
            .config
            .max_inflight
            .saturating_sub(self.core.inbound.available_permits())
    }
    /// Join one bounded, independently owned drain of actual peer tasks.
    /// Cancellation only abandons this waiter. Unproven cleanup remains retained.
    pub async fn shutdown(&self) -> Result<()> {
        self.close.join().await
    }
    async fn request(
        &self,
        peer: &PeerId,
        scope: &CacheScope,
        id: &BlockId,
        bytes: Option<PeerPayload<'_>>,
    ) -> Result<Option<Bytes>> {
        self.core.request(peer, scope, id, bytes).await
    }
}
impl PeerCore {
    fn seal(&self) {
        self.requests.seal();
        self.permits.close();
        self.inbound.close();
        self.streams.close();
        self.receive_bytes.close();
        self.request_bytes.close();
        self.stop.send_replace(true);
        self.endpoint.close(0u32.into(), b"shutdown");
    }
    async fn supervise(self: Arc<Self>) -> Result<()> {
        let mut stopping = self.stop.subscribe();
        let mut sessions = tokio::task::JoinSet::new();
        let mut failure = None;
        while !*stopping.borrow() {
            tokio::select! {
                biased;
                _ = stopping.changed() => break,
                completed = sessions.join_next(), if !sessions.is_empty() => {
                    match completed {
                        Some(Ok(Ok(()))) => {},
                        _ => {
                            failure.get_or_insert_with(error);
                            self.seal();
                            break;
                        }
                    }
                }
                incoming = self.endpoint.accept() => {
                    let Some(incoming) = incoming else { break };
                    if *stopping.borrow() || sessions.len() >= self.config.max_inflight {
                        incoming.refuse();
                        continue;
                    }
                    let Ok(permit) = self.inbound.clone().try_acquire_owned() else {
                        incoming.refuse();
                        continue;
                    };
                    let service = self.clone();
                    sessions.spawn(async move {
                        let _permit = permit;
                        service.accept(incoming).await
                    });
                }
            }
        }
        // Sessions stop their streams and positively reap them. Aborting a
        // session here would merely drop its JoinSet without joining children.
        while let Some(completed) = sessions.join_next().await {
            if !matches!(completed, Ok(Ok(()))) {
                failure.get_or_insert_with(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn authenticated(&self, c: &quinn::Connection) -> Result<PeerId> {
        let certs = c
            .peer_identity()
            .ok_or_else(error)?
            .downcast::<Vec<CertificateDer<'static>>>()
            .map_err(|_| error())?;
        let leaf = certs.first().ok_or_else(error)?;
        let pin: [u8; 32] = Sha256::digest(leaf.as_ref()).into();
        self.config
            .trusted
            .iter()
            .find(|(_, p)| p.certificate_sha256 == pin)
            .map(|(id, _)| id.clone())
            .ok_or_else(error)
    }
    async fn connection(&self, id: &PeerId, placement: bool) -> Result<quinn::Connection> {
        if *self.stop.borrow() {
            return Err(error());
        }
        let peer = self.config.trusted.get(id).ok_or_else(error)?;
        let slots = self.connections.get(id).ok_or_else(error)?;
        let (slot, other) = if placement {
            (&slots.placements, &slots.reads)
        } else {
            (&slots.reads, &slots.placements)
        };
        let mut slot = {
            let mut span = Span::new(Operation::BlobCachePeerConnectionLockWait);
            let slot = slot.lock().await;
            span.finish_success(0);
            slot
        };
        if *self.stop.borrow() {
            return Err(error());
        }
        if let Some(connection) = slot.as_ref()
            && connection.close_reason().is_none()
        {
            return Ok(connection.clone());
        }
        // Slots are populated only after exact peer authentication below. Never
        // wait for the other role: it may own a stalled cold negotiation.
        if let Ok(other) = other.try_lock()
            && let Some(connection) = other.as_ref()
            && connection.close_reason().is_none()
        {
            *slot = Some(connection.clone());
            return Ok(connection.clone());
        }
        let mut span = Span::new(Operation::BlobCachePeerConnectionEstablish);
        let connecting = match self.endpoint.connect(peer.address, &peer.server_name) {
            Ok(connecting) => connecting,
            Err(_) => {
                span.finish_error();
                return Err(error());
            }
        };
        let connection = match connecting.await {
            Ok(connection) => connection,
            Err(_) => {
                span.finish_error();
                return Err(error());
            }
        };
        let authenticated = match self.authenticated(&connection) {
            Ok(authenticated) => authenticated,
            Err(error) => {
                span.finish_error();
                return Err(error);
            }
        };
        if &authenticated != id {
            connection.close(1u32.into(), b"untrusted peer");
            span.finish_error();
            return Err(error());
        }
        *slot = Some(connection.clone());
        span.finish_success(0);
        Ok(connection)
    }
    async fn accept(self: Arc<Self>, incoming: quinn::Incoming) -> Result<()> {
        let mut stopping = self.stop.subscribe();
        if *stopping.borrow() {
            return Ok(());
        }
        let connection = tokio::select! {
            biased;
            _ = stopping.changed() => return Ok(()),
            connection = timeout(self.config.deadline, incoming) => {
                match connection {
                    Ok(Ok(connection)) => connection,
                    // Handshake and trust failures are routine peer failures;
                    // they do not imply unobserved task cleanup.
                    _ => return Ok(()),
                }
            }
        };
        let id = match self.authenticated(&connection) {
            Ok(id) => id,
            Err(_) => {
                connection.close(1u32.into(), b"untrusted peer");
                return Ok(());
            }
        };
        let mut workers = tokio::task::JoinSet::new();
        let mut failure = None;
        while !*stopping.borrow() {
            tokio::select! {
                biased;
                _ = stopping.changed() => break,
                completed=workers.join_next(),if !workers.is_empty()=>{
                    if !matches!(completed, Some(Ok(()))) {
                        failure.get_or_insert_with(error);
                        break;
                    }
                },
                stream=timeout(Duration::from_secs(30),connection.accept_bi())=>{
                    let (mut send,mut recv)=match stream{Ok(Ok(stream))=>stream,_=>break};
                    let permit=match self.streams.clone().try_acquire_owned(){Ok(permit) if workers.len()<self.config.max_inflight=>permit,_=>{let _=send.reset(1u32.into());let _=recv.stop(1u32.into());continue;}};
                    let service=self.clone();let id=id.clone();workers.spawn(async move{let _stream=permit;let result=timeout(service.config.deadline,service.process_stream(&id,&mut send,&mut recv)).await;if !matches!(result,Ok(Ok(()))){let _=send.reset(1u32.into());let _=recv.stop(1u32.into());}});
                }
            }
        }
        workers.abort_all();
        while let Some(completed) = workers.join_next().await {
            if let Err(join) = completed
                && !join.is_cancelled()
            {
                failure.get_or_insert_with(error);
            }
        }
        #[cfg(all(test, unix))]
        {
            let gate = self
                .test_session_exit_gate
                .lock()
                .map_err(|_| error())?
                .take();
            if let Some(gate) = gate {
                let _ = gate.entered.send(());
                let _ = gate.release.await;
            }
        }
        failure.map_or(Ok(()), Err)
    }
    async fn process_stream(
        &self,
        id: &PeerId,
        send: &mut quinn::SendStream,
        recv: &mut quinn::RecvStream,
    ) -> Result<()> {
        let charge = Arc::new(
            self.receive_bytes
                .clone()
                .acquire_many_owned(self.transfer_charge)
                .await
                .map_err(|_| error())?,
        );
        let data = recv
            .read_to_end(self.config.max_blob_bytes + MAX_HEADER_BYTES)
            .await
            .map_err(|_| error())?;
        let (op, scope, block, bytes) = decode(&data, self.config.max_blob_bytes)?;
        let peer = self.config.trusted.get(id).ok_or_else(error)?;
        if !peer.partitions.contains(&scope.identity.partition) {
            return Err(error());
        }
        let policy = self.cache.scope_policy(&scope).ok_or_else(error)?;
        let response = if op == 0 {
            let key = LocalCache::key_hashed(&LocalCache::scope_hash(&scope), &block);
            if let Some(bytes) = self.cache.get_memory_shared_hashed(&key) {
                Some(Bytes::from_owner(bytes))
            } else {
                let disk_charge = charge.clone();
                bounded_cache_work(self.cache.clone(), move |cache| {
                    let _charge = disk_charge;
                    cache.get_disk(&scope, &block, policy).map(Bytes::from)
                })
                .await?
            }
        } else {
            let shared: Arc<[u8]> = Arc::from(bytes);
            let disk_charge = charge.clone();
            bounded_cache_work(self.cache.clone(), move |cache| {
                let _charge = disk_charge;
                cache.insert_shared(&scope, &block, shared, policy)
            })
            .await??;
            Some(Bytes::new())
        };
        let status = if response.is_some() {
            Bytes::from_static(&[1])
        } else {
            Bytes::from_static(&[0])
        };
        let status = Bytes::from_owner(ChargedPayload {
            bytes: status,
            _charge: charge.clone(),
        });
        let payload = Bytes::from_owner(ChargedPayload {
            bytes: response.unwrap_or_default(),
            _charge: charge,
        });
        let mut chunks = [status, payload];
        send.write_all_chunks(&mut chunks)
            .await
            .map_err(|_| error())?;
        send.finish().map_err(|_| error())?;
        Ok(())
    }
    async fn request(
        &self,
        peer: &PeerId,
        scope: &CacheScope,
        id: &BlockId,
        bytes: Option<PeerPayload<'_>>,
    ) -> Result<Option<Bytes>> {
        let p = self.config.trusted.get(peer).ok_or_else(error)?;
        if !p.partitions.contains(&scope.identity.partition) {
            return Err(error());
        }
        let _request = self.requests.enter(&self.permits)?;
        if bytes
            .as_ref()
            .is_some_and(|b| b.len() > self.config.max_blob_bytes)
        {
            return Err(error());
        }
        timeout(self.config.deadline, async {
            let charge = {
                let mut admission = Span::new(Operation::BlobCachePeerRequestByteAdmissionWait);
                let permit = self
                    .request_bytes
                    .clone()
                    .acquire_many_owned(self.transfer_charge)
                    .await
                    .map_err(|_| {
                        admission.finish_error();
                        error()
                    })?;
                admission.finish_success(0);
                Arc::new(permit)
            };
            let connection = self.connection(peer, bytes.is_some()).await?;
            let (mut send, mut recv) = {
                let mut opening = Span::new(Operation::BlobCachePeerOpenBi);
                let stream = connection.open_bi().await.map_err(|_| {
                    opening.finish_error();
                    error()
                })?;
                opening.finish_success(0);
                stream
            };
            let header = encode_header(
                scope,
                id,
                bytes.as_ref().map(PeerPayload::len),
                self.config.max_blob_bytes,
            )?;
            let put = bytes.is_some();
            let header = Bytes::from_owner(ChargedPayload {
                bytes: Bytes::from(header),
                _charge: charge.clone(),
            });
            let body = Bytes::from_owner(ChargedPayload {
                bytes: bytes.map(PeerPayload::owned).unwrap_or_default(),
                _charge: charge.clone(),
            });
            // Capture protocol submission bytes before Quinn mutates the chunks.
            // Success describes local submission, not acknowledgment or UDP bytes.
            let submitted_bytes = (header.len() + body.len()) as u64;
            let mut chunks = [header, body];
            {
                let mut sending = Span::new(Operation::BlobCachePeerRequestSend);
                send.write_all_chunks(&mut chunks).await.map_err(|_| {
                    sending.finish_error();
                    error()
                })?;
                send.finish().map_err(|_| {
                    sending.finish_error();
                    error()
                })?;
                sending.finish_success(submitted_bytes);
            }
            let mut receiving = Span::new(Operation::BlobCachePeerResponseReceive);
            let mut status = [0];
            recv.read_exact(&mut status).await.map_err(|_| {
                receiving.finish_error();
                error()
            })?;
            let body = recv
                .read_to_end(if put || status[0] == 0 {
                    0
                } else {
                    self.config.max_blob_bytes
                })
                .await
                .map_err(|_| {
                    receiving.finish_error();
                    error()
                })?;
            let received_bytes = 1 + body.len() as u64;
            let response = match status[0] {
                0 => None,
                1 => Some(Bytes::from_owner(ChargedPayload {
                    bytes: Bytes::from(body),
                    _charge: charge,
                })),
                _ => {
                    receiving.finish_error();
                    return Err(error());
                }
            };
            // A valid peer response is not yet an integrity/admission decision.
            receiving.finish_success(received_bytes);
            Ok(response)
        })
        .await
        .map_err(|_| error())?
    }
}
async fn bounded_cache_work<R: Send + 'static>(
    cache: Arc<LocalCache>,
    work: impl FnOnce(&crate::local::AdmittedCache) -> R + Send + 'static,
) -> Result<R> {
    cache.run_blocking(work).await
}
impl Drop for QuicPeerTransport {
    fn drop(&mut self) {
        self.close.start();
    }
}
// The GET span includes the caller's existing payload conversion. A miss is a
// separate classification event: zero-byte successful hits remain distinguishable.
fn finish_peer_get<T: AsRef<[u8]>>(getting: &mut Span<'_>, result: &Result<Option<T>>) {
    match result {
        Ok(Some(bytes)) => getting.finish_success(bytes.as_ref().len() as u64),
        Ok(None) => {
            getting.finish_success(0);
            let mut miss = Span::new(Operation::BlobCachePeerGetMiss);
            miss.finish_success(0);
        }
        Err(_) => getting.finish_error(),
    }
}

#[async_trait]
impl PeerTransport for QuicPeerTransport {
    async fn get(&self, p: &PeerId, s: &CacheScope, id: &BlockId) -> Result<Option<Vec<u8>>> {
        let mut getting = Span::new(Operation::BlobCachePeerGet);
        let result = self
            .request(p, s, id, None)
            .await
            .map(|bytes| bytes.map(|bytes| bytes.to_vec()));
        finish_peer_get(&mut getting, &result);
        result
    }
    async fn get_shared(&self, p: &PeerId, s: &CacheScope, id: &BlockId) -> Result<Option<Bytes>> {
        let mut getting = Span::new(Operation::BlobCachePeerGet);
        let result = self.request(p, s, id, None).await;
        finish_peer_get(&mut getting, &result);
        result
    }
    async fn put_shared(
        &self,
        p: &PeerId,
        s: &CacheScope,
        id: &BlockId,
        b: Arc<[u8]>,
    ) -> Result<()> {
        if self
            .request(p, s, id, Some(PeerPayload::Shared(Bytes::from_owner(b))))
            .await?
            .is_some()
        {
            Ok(())
        } else {
            Err(error())
        }
    }
    async fn put(&self, p: &PeerId, s: &CacheScope, id: &BlockId, b: &[u8]) -> Result<()> {
        if self
            .request(p, s, id, Some(PeerPayload::Borrowed(b)))
            .await?
            .is_some()
        {
            Ok(())
        } else {
            Err(error())
        }
    }
}
fn encode_header(
    s: &CacheScope,
    id: &BlockId,
    payload: Option<usize>,
    max: usize,
) -> Result<Vec<u8>> {
    if payload.is_some_and(|length| length > max) {
        return Err(error());
    }
    let texts = [
        &s.identity.cluster,
        &s.identity.partition,
        &s.identity.drive,
        &id.0,
    ];
    if texts
        .iter()
        .any(|text| text.is_empty() || text.len() > 1024)
    {
        return Err(error());
    }
    let length = 25 + texts.iter().map(|t| t.len()).sum::<usize>();
    let mut out = Vec::with_capacity(length);
    out.push(u8::from(payload.is_some()));
    for text in texts {
        out.extend_from_slice(&(text.len() as u16).to_be_bytes());
        out.extend_from_slice(text.as_bytes());
    }
    out.extend_from_slice(&s.backing.as_bytes());
    Ok(out)
}
#[cfg(all(test, unix))]
fn encode(s: &CacheScope, id: &BlockId, payload: Option<&[u8]>, max: usize) -> Result<Vec<u8>> {
    let mut out = encode_header(s, id, payload.map(<[u8]>::len), max)?;
    if let Some(bytes) = payload {
        out.extend_from_slice(bytes);
    }
    Ok(out)
}
fn decode(data: &[u8], max: usize) -> Result<(u8, CacheScope, BlockId, &[u8])> {
    let mut cursor = 0;
    fn take<'a>(d: &'a [u8], c: &mut usize, n: usize) -> Result<&'a [u8]> {
        let end = c.checked_add(n).ok_or_else(error)?;
        let v = d.get(*c..end).ok_or_else(error)?;
        *c = end;
        Ok(v)
    }
    let op = take(data, &mut cursor, 1)?[0];
    if op > 1 {
        return Err(error());
    }
    let mut texts = [""; 4];
    for text in &mut texts {
        let n = u16::from_be_bytes(
            take(data, &mut cursor, 2)?
                .try_into()
                .map_err(|_| error())?,
        ) as usize;
        if n == 0 || n > 1024 {
            return Err(error());
        }
        *text = std::str::from_utf8(take(data, &mut cursor, n)?).map_err(|_| error())?;
    }
    let backing = ConcurrentBackingId::from_bytes(
        take(data, &mut cursor, 16)?
            .try_into()
            .map_err(|_| error())?,
    )?;
    let bytes = &data[cursor..];
    if bytes.len() > max || (op == 0 && !bytes.is_empty()) {
        return Err(error());
    }
    let mut t = texts.into_iter().map(str::to_owned);
    Ok((
        op,
        CacheScope {
            identity: ScopeIdentity {
                cluster: t.next().unwrap(),
                partition: t.next().unwrap(),
                drive: t.next().unwrap(),
            },
            backing,
        },
        BlockId(t.next().unwrap()),
        bytes,
    ))
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_disk_work_holds_global_permit_until_the_worker_exits() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(LocalCacheConfig {
            directory: dir.path().into(),
            memory_bytes: 16,
            disk_bytes: 64,
            max_entries: 4,
            max_blob_bytes: 16,
        })
        .unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let c = cache.clone();
        let task = tokio::spawn(bounded_cache_work(c, move |_| {
            started.send(()).unwrap();
            blocked.recv().unwrap();
        }));
        ready.await.unwrap();
        task.abort();
        let _ = task.await;
        let available = cache.io_permits.available_permits();
        release.send(()).unwrap();
        timeout(Duration::from_secs(1), async {
            while cache.io_permits.available_permits() != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let drained = cache.shutdown().await;
        assert_eq!(
            available, 7,
            "noncancellable disk work must retain its global slot after async cancellation"
        );
        assert!(drained.is_ok(), "actual canceled cache worker did not join");
    }
    #[tokio::test]
    async fn queued_payload_ownership_retains_its_transfer_charge() {
        let budget = Arc::new(Semaphore::new(16));
        let charge = Arc::new(budget.clone().acquire_many_owned(8).await.unwrap());
        let bytes = Bytes::from_owner(ChargedPayload {
            bytes: Bytes::from_static(b"payload"),
            _charge: charge.clone(),
        });
        drop(charge);
        let retained = bytes.clone();
        drop(bytes);
        assert_eq!(
            budget.available_permits(),
            8,
            "queued QUIC ownership must retain byte charge"
        );
        drop(retained);
        assert_eq!(budget.available_permits(), 16);
        assert!(peer_transfer_charge(usize::MAX).is_err());
    }
    #[test]
    fn peer_headers_keep_v1_bytes_and_decode_borrows_payload() {
        let scope = CacheScope {
            identity: ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            backing: ConcurrentBackingId::from_bytes([2; 16]).unwrap(),
        };
        let id = BlockId("opaque".into());
        let frame = encode(&scope, &id, Some(b"payload"), 8).unwrap();
        let header = encode_header(&scope, &id, Some(7), 8).unwrap();
        assert_eq!(header.len(), 25 + 1 + 1 + 1 + 6);
        assert_eq!(header.capacity(), header.len());
        assert_eq!(&frame[..header.len()], header);
        let decoded = decode(&frame, 8).unwrap();
        assert_eq!(decoded.3.as_ptr(), frame[header.len()..].as_ptr());
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(LocalCacheConfig {
            directory: dir.path().into(),
            memory_bytes: 16,
            disk_bytes: 0,
            max_entries: 4,
            max_blob_bytes: 16,
        })
        .unwrap();
        let shared: Arc<[u8]> = Arc::from(b"payload".as_slice());
        cache
            .insert_memory_shared(&scope, &id, shared.clone(), IntegrityPolicy::Opaque)
            .unwrap();
        let first = cache
            .get_memory_shared_hashed(&LocalCache::key_hashed(
                &LocalCache::scope_hash(&scope),
                &id,
            ))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &shared));
    }
    #[test]
    fn binary_frames_reject_oversize_truncation_and_unknown_operation() {
        let s = CacheScope {
            identity: ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            backing: ConcurrentBackingId::from_bytes([2; 16]).unwrap(),
        };
        let id = BlockId("opaque".into());
        let f = encode(&s, &id, Some(b"bytes"), 8).unwrap();
        assert_eq!(decode(&f, 8).unwrap().3, b"bytes");
        assert!(decode(&f, 4).is_err());
        for n in 0..f.len() - 5 {
            assert!(decode(&f[..n], 8).is_err());
        }
        let mut bad = f;
        bad[0] = 2;
        assert!(decode(&bad, 8).is_err());
    }
    fn cert(
        ca: &rcgen::CertifiedIssuer<'static, rcgen::KeyPair>,
    ) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        params.extended_key_usages = vec![
            rcgen::ExtendedKeyUsagePurpose::ServerAuth,
            rcgen::ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let cert = params.signed_by(&key, ca).unwrap();
        (
            cert.der().clone(),
            rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
    }
    #[allow(dead_code)]
    mod stage_metrics {
        include!("../tests/support/stage_metrics.rs");
    }
    struct MetricPeers {
        a: Arc<QuicPeerTransport>,
        b: Arc<QuicPeerTransport>,
        b_cache: Arc<LocalCache>,
        blackhole: std::net::UdpSocket,
        scope: CacheScope,
    }
    impl MetricPeers {
        fn new(wrong_pin: bool, unknown_ca: bool) -> Self {
            let make_ca = || {
                let mut params = rcgen::CertificateParams::new(vec!["cache-ca".into()]).unwrap();
                params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
                rcgen::CertifiedIssuer::self_signed(params, rcgen::KeyPair::generate().unwrap())
                    .unwrap()
            };
            let ca = make_ca();
            let foreign = make_ca();
            let (ac, ak) = cert(if unknown_ca { &foreign } else { &ca });
            let (bc, bk) = cert(&ca);
            let mut roots = rustls::RootCertStore::empty();
            roots.add(ca.der().clone()).unwrap();
            // Retain before any service starts: startup panic, cancellation and
            // bounded shutdown all leave removal to the owned parent barrier.
            let directory = tempfile::tempdir().unwrap().keep();
            let cache = |name| {
                LocalCache::new(LocalCacheConfig {
                    directory: directory.join(name),
                    memory_bytes: 1024,
                    disk_bytes: 2048,
                    max_entries: 10,
                    max_blob_bytes: 1024,
                })
                .unwrap()
            };
            let a_cache = cache("a");
            let b_cache = cache("b");
            let scope = CacheScope {
                identity: ScopeIdentity {
                    cluster: "c".into(),
                    partition: "p".into(),
                    drive: "d".into(),
                },
                backing: ConcurrentBackingId::from_bytes([2; 16]).unwrap(),
            };
            a_cache
                .register_scope(scope.clone(), IntegrityPolicy::Opaque)
                .unwrap();
            b_cache
                .register_scope(scope.clone(), IntegrityPolicy::Opaque)
                .unwrap();
            let endpoint = |address, cert: &CertificateDer<'static>| PeerEndpoint {
                address,
                server_name: "localhost".into(),
                certificate_sha256: Sha256::digest(cert.as_ref()).into(),
                partitions: BTreeSet::from(["p".into()]),
            };
            let config = |local: &str, cert, key, trusted| QuicPeerConfig {
                local: PeerId(local.into()),
                bind: "127.0.0.1:0".parse().unwrap(),
                certificates: vec![cert],
                private_key: key,
                roots: roots.clone(),
                trusted,
                max_blob_bytes: 1024,
                max_inflight: 128,
                transfer_bytes: 128 * 1024 * 1024,
                deadline: if wrong_pin || unknown_ca {
                    Duration::from_millis(500)
                } else {
                    Duration::from_secs(2)
                },
            };
            // B's inbound authentication uses the pin, so no outbound address reservation is needed.
            let b = QuicPeerTransport::bind(
                config(
                    "b",
                    bc.clone(),
                    bk,
                    BTreeMap::from([(
                        PeerId("a".into()),
                        endpoint("127.0.0.1:0".parse().unwrap(), &ac),
                    )]),
                ),
                b_cache.clone(),
            )
            .unwrap();
            let blackhole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let mut target = endpoint(b.local_addr().unwrap(), &bc);
            if wrong_pin {
                target.certificate_sha256 = [99; 32];
            }
            let a = QuicPeerTransport::bind(
                config(
                    "a",
                    ac,
                    ak,
                    BTreeMap::from([
                        (PeerId("b".into()), target),
                        (
                            PeerId("blackhole".into()),
                            PeerEndpoint {
                                address: blackhole.local_addr().unwrap(),
                                server_name: "localhost".into(),
                                certificate_sha256: [77; 32],
                                partitions: BTreeSet::from(["p".into()]),
                            },
                        ),
                    ]),
                ),
                a_cache,
            )
            .unwrap();
            Self {
                a,
                b,
                b_cache,
                blackhole,
                scope,
            }
        }
        async fn shutdown(&self) -> Result<()> {
            let a = self.a.shutdown().await;
            let b = self.b.shutdown().await;
            let a_cache = self.a.core.cache.shutdown().await;
            let b_cache = self.b_cache.shutdown().await;
            a?;
            b?;
            a_cache?;
            b_cache?;
            Ok(())
        }
    }
    impl Drop for MetricPeers {
        fn drop(&mut self) {
            // Files were retained before startup. Shutdown's bounded worker
            // drain is not permission to delete them before the process barrier.
            self.a.core.endpoint.close(0u32.into(), b"fixture dropped");
            self.b.core.endpoint.close(0u32.into(), b"fixture dropped");
        }
    }
    #[tokio::test]
    async fn cancelled_close_waiter_does_not_acknowledge_a_live_peer_session_owner() {
        use std::{future::poll_fn, task::Poll};

        let peers = MetricPeers::new(false, false);
        let bytes = b"owned peer session shutdown\0\xff";
        let id = BlockId("owned session".into());
        peers
            .b_cache
            .insert(&peers.scope, &id, bytes, IntegrityPolicy::Opaque)
            .unwrap();
        let received = peers.a.get(&PeerId("b".into()), &peers.scope, &id).await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *peers.b.core.test_session_exit_gate.lock().unwrap() = Some(TestSessionExitGate {
            entered: entered_tx,
            release: release_rx,
        });
        let mut first_waiter = Box::pin(peers.b.shutdown());
        poll_fn(|cx| {
            let _ = first_waiter.as_mut().poll(cx);
            Poll::Ready(())
        })
        .await;
        drop(first_waiter);
        let entered = timeout(Duration::from_secs(1), entered_rx).await;
        let entered_exit = matches!(entered, Ok(Ok(())));
        let premature_acknowledgment = if entered_exit {
            matches!(
                timeout(Duration::from_millis(100), peers.b.shutdown()).await,
                Ok(Ok(()))
            )
        } else {
            false
        };
        let owners = [Arc::downgrade(&peers.a), Arc::downgrade(&peers.b)];
        let cache_owners = [
            Arc::downgrade(&peers.a.core.cache),
            Arc::downgrade(&peers.b_cache),
        ];
        // Release the actual session before any assertion or fixture removal.
        drop(release_tx);
        let drained = peers.shutdown().await;
        drop(peers);
        let released = timeout(Duration::from_secs(3), async {
            while owners.iter().any(|owner| owner.upgrade().is_some())
                || cache_owners.iter().any(|owner| owner.upgrade().is_some())
            {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(received.unwrap().as_deref(), Some(bytes.as_slice()));
        assert!(
            entered_exit,
            "actual authenticated session did not reach its exit gate"
        );
        assert!(
            released.is_ok(),
            "actual peer/cache owners did not release after the gate"
        );
        assert!(
            drained.is_ok(),
            "actual peer/cache drain did not acknowledge completion"
        );
        assert!(
            !premature_acknowledgment,
            "peer shutdown acknowledged while the actual authenticated session owner remained"
        );
    }
    #[tokio::test]
    async fn aborted_close_worker_retains_the_actual_peer_drain_and_cached_failure() {
        failed_close_retains_actual_owners(true).await;
    }
    #[tokio::test]
    async fn held_session_drain_timeout_retains_actual_owners_and_cached_failure() {
        failed_close_retains_actual_owners(false).await;
    }
    #[tokio::test]
    async fn cold_failed_connection_drain_joins_endpoint_before_ack() {
        let peers = MetricPeers::new(false, false);
        let peer = PeerId("blackhole".into());
        let id = BlockId("cancelled cold connection".into());
        let mut read = peers.a.get(&peer, &peers.scope, &id);
        poll_fn(|cx| {
            assert!(read.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        let cold_owner = peers.a.core.requests.lock().active == 1
            && peers.a.core.endpoint.open_connections() > 0
            && peers
                .a
                .core
                .connections
                .get(&peer)
                .unwrap()
                .reads
                .try_lock()
                .is_err();
        drop(read);
        let request_dropped = peers.a.core.requests.lock().active == 0;
        let started = std::time::Instant::now();
        let drained = peers.a.shutdown().await;
        let elapsed = started.elapsed();
        let phase = peers.a.close.phase.load(Ordering::Acquire);
        let supervisor_us = peers.a.close.supervisor_join_micros.load(Ordering::Acquire);
        let requests_us = peers
            .a
            .close
            .requests_drained_micros
            .load(Ordering::Acquire);
        let actual_join = matches!(peers.a.close.supervisor.poll_result(), Poll::Ready(Ok(())));
        let endpoint_connections = peers.a.core.endpoint.open_connections();
        let b_drained = peers.b.shutdown().await;
        let a_cache_drained = peers.a.core.cache.shutdown().await;
        let b_cache_drained = peers.b_cache.shutdown().await;
        let owners = [
            Arc::downgrade(&peers.a.core.cache),
            Arc::downgrade(&peers.b_cache),
        ];
        drop(peers);
        let released = timeout(Duration::from_secs(3), async {
            while owners.iter().any(|owner| owner.strong_count() != 0) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(
            cold_owner && request_dropped,
            "actual cold query admission and cancellation were not observed"
        );
        assert!(
            drained.is_ok()
                && b_drained.is_ok()
                && a_cache_drained.is_ok()
                && b_cache_drained.is_ok()
        );
        assert!(
            actual_join && phase == 4 && endpoint_connections == 0,
            "endpoint idle and actual session joins must precede ACK"
        );
        assert!(
            released.is_ok(),
            "acknowledged transport/cache owners remained"
        );
        println!(
            "MOUNT_RS_PEER_DRAIN cold_failed_connection=true actual_supervisor_join=true request_owners=0 endpoint_connections={endpoint_connections} elapsed_us={} supervisor_join_us={supervisor_us} requests_drained_us={requests_us}",
            elapsed.as_micros()
        );
    }
    async fn failed_close_retains_actual_owners(abort: bool) {
        let mut peers = MetricPeers::new(false, false);
        if !abort {
            // A shortened cleanup budget injects a real held-session timeout;
            // RPC/stream/transport timers retain their production values.
            Arc::get_mut(&mut Arc::get_mut(&mut peers.b).unwrap().close)
                .unwrap()
                .deadline = Duration::from_millis(75);
        }
        let id = BlockId("aborted close".into());
        let bytes = b"aborted peer closer\0\xff";
        peers
            .b_cache
            .insert(&peers.scope, &id, bytes, IntegrityPolicy::Opaque)
            .unwrap();
        let received = peers.a.get(&PeerId("b".into()), &peers.scope, &id).await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *peers.b.core.test_session_exit_gate.lock().unwrap() = Some(TestSessionExitGate {
            entered: entered_tx,
            release: release_rx,
        });
        let mut waiter = Box::pin(peers.b.shutdown());
        poll_fn(|cx| {
            let _ = waiter.as_mut().poll(cx);
            Poll::Ready(())
        })
        .await;
        drop(waiter);
        let entered = timeout(Duration::from_secs(1), entered_rx).await;
        let entered_exit = matches!(entered, Ok(Ok(())));
        let worker = peers.b.close.lock().worker.clone();
        let worker_owned = worker.is_some();
        if let Some(worker) = worker.filter(|_| abort) {
            worker.abort();
        }
        let failed = timeout(Duration::from_secs(1), peers.b.shutdown()).await;
        let repeated = timeout(Duration::from_secs(1), peers.b.shutdown()).await;
        let retained = {
            let state = peers.b.close.lock();
            state.retained.is_some() && state.current.is_some() && state.resources.is_some()
        };
        let sealed = peers.b.core.inbound.clone().try_acquire_owned().is_err()
            && peers.b.core.streams.clone().try_acquire_owned().is_err()
            && peers.b.core.requests.lock().stopped;
        // The public failure remains sticky. The fixture separately resumes the
        // retained actual future after releasing its known session gate, solely
        // to positively join all real owners before removing test observations.
        drop(release_tx);
        let actual_drain = timeout(
            Duration::from_secs(3),
            poll_fn(|cx| peers.b.close.poll_close(cx)),
        )
        .await;
        let a_drained = peers.a.shutdown().await;
        let a_cache_drained = peers.a.core.cache.shutdown().await;
        let b_cache_drained = peers.b_cache.shutdown().await;
        let owners = [
            Arc::downgrade(&peers.a.core.cache),
            Arc::downgrade(&peers.b_cache),
        ];
        if matches!(actual_drain, Ok(Ok(()))) {
            let mut state = peers.b.close.lock();
            state.current = None;
            state.resources = None;
            state.retained = None;
        }
        drop(peers);
        let released = timeout(Duration::from_secs(3), async {
            while owners.iter().any(|owner| owner.upgrade().is_some()) {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert_eq!(received.unwrap().as_deref(), Some(bytes.as_slice()));
        assert!(
            entered_exit && worker_owned,
            "actual session and close worker were not observed"
        );
        let expected = if abort {
            ErrorCode::Eio
        } else {
            ErrorCode::Ebusy
        };
        assert!(matches!(failed, Ok(Err(ref error)) if error.is(expected)));
        assert!(matches!(repeated, Ok(Err(ref error)) if error.is(expected)));
        assert!(
            retained && sealed,
            "unacknowledged cleanup lost ownership or admission seal"
        );
        assert!(
            matches!(actual_drain, Ok(Ok(()))),
            "actual retained peer drain did not join"
        );
        assert!(a_drained.is_ok() && a_cache_drained.is_ok() && b_cache_drained.is_ok());
        assert!(
            released.is_ok(),
            "fixture owners did not release after actual drain"
        );
    }
    // Literal labels allow this real-transport qualification to compile before producers exist.
    #[tokio::test]
    #[ignore = "isolated process: MOUNT_RS_PROFILE_IO=1, storage/request traces=0"]
    async fn peer_request_stage_metrics_preserve_bytes_outcomes_and_cancellation() {
        use mount_rs_core::diagnostics::storage;
        use stage_metrics::{Checks, Expected};
        use std::{future::poll_fn, task::Poll};

        const BYTE: &str = "blob_cache.peer.request_byte_admission_wait";
        const OPEN: &str = "blob_cache.peer.open_bi";
        const SEND: &str = "blob_cache.peer.request_send";
        const RECEIVE: &str = "blob_cache.peer.response_receive";
        const GET: &str = "blob_cache.peer.get";
        const MISS: &str = "blob_cache.peer.get_miss";

        async fn cleanup(peers: MetricPeers) {
            let addresses = [peers.a.local_addr().unwrap(), peers.b.local_addr().unwrap()];
            let lifetimes = [
                Arc::downgrade(&peers.a.core.cache),
                Arc::downgrade(&peers.b_cache),
            ];
            // The unchanged global15s qualification bounds these real drains.
            // The3s observer below measures owner/socket release after positive
            // protocol/task drain ACKs, not Quinn's distinct3*PTO close phase.
            peers.shutdown().await.expect("actual peer/cache shutdown");
            drop(peers);
            timeout(Duration::from_secs(3), async {
                loop {
                    if lifetimes.iter().all(|owner| owner.upgrade().is_none()) {
                        match (
                            std::net::UdpSocket::bind(addresses[0]),
                            std::net::UdpSocket::bind(addresses[1]),
                        ) {
                            (Ok(a), Ok(b)) => {
                                assert_eq!(a.local_addr().unwrap(), addresses[0]);
                                assert_eq!(b.local_addr().unwrap(), addresses[1]);
                                break;
                            }
                            (a, b) => {
                                for result in [a, b] {
                                    if let Err(error) = result {
                                        assert_eq!(error.kind(), std::io::ErrorKind::AddrInUse);
                                    }
                                }
                            }
                        }
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .expect("bounded real peer/cache owner and UDP release");
        }

        timeout(Duration::from_secs(20), async {
            assert!(storage::enabled(), "parent must isolate the enabled profiler");
            let mut checks = Checks::default();
            let peers = MetricPeers::new(false, false);
            let peer = PeerId("b".into());
            let id = BlockId("request stages".into());
            let empty = BlockId("empty hit".into());
            let absent = BlockId("remote miss".into());
            let bytes = b"peer request stages\0\xffcomplete bytes";
            peers.b_cache.insert(&peers.scope, &id, bytes, IntegrityPolicy::Opaque).unwrap();
            peers.b_cache.insert(&peers.scope, &empty, b"", IntegrityPolicy::Opaque).unwrap();

            for (phase, block, expected, miss) in [
                ("full GET", &id, Some(bytes.as_slice()), 0),
                ("empty GET hit", &empty, Some(b"".as_slice()), 0),
                ("remote GET miss", &absent, None, 1),
            ] {
                let before = storage::snapshot();
                assert_eq!(peers.a.get(&peer, &peers.scope, block).await.unwrap().as_deref(), expected);
                let length = expected.map_or(0, <[u8]>::len) as u64;
                let header = encode_header(&peers.scope, block, None, 1024).unwrap().len() as u64;
                checks.phase(phase, &before, vec![
                    Expected(BYTE, 1, 0, 0, 0), Expected(OPEN, 1, 0, 0, 0),
                    Expected(SEND, 1, 0, 0, header), Expected(RECEIVE, 1, 0, 0, 1 + length),
                    Expected(GET, 1, 0, 0, length), Expected(MISS, miss, 0, 0, 0),
                ]);
            }
            for (phase, block, expected, miss) in [
                ("full shared GET", &id, Some(bytes.as_slice()), 0),
                ("empty shared GET hit", &empty, Some(b"".as_slice()), 0),
                ("remote shared GET miss", &absent, None, 1),
            ] {
                let before = storage::snapshot();
                assert_eq!(peers.a.get_shared(&peer, &peers.scope, block).await.unwrap().as_deref(), expected);
                let length = expected.map_or(0, <[u8]>::len) as u64;
                let header = encode_header(&peers.scope, block, None, 1024).unwrap().len() as u64;
                checks.phase(phase, &before, vec![
                    Expected(BYTE, 1, 0, 0, 0), Expected(OPEN, 1, 0, 0, 0),
                    Expected(SEND, 1, 0, 0, header), Expected(RECEIVE, 1, 0, 0, 1 + length),
                    Expected(GET, 1, 0, 0, length), Expected(MISS, miss, 0, 0, 0),
                ]);
            }
            let connection = peers.a.core.connection(&peer, false).await.unwrap();
            let stable_id = connection.stable_id();

            let budget = peers.a.core.request_bytes.clone().acquire_many_owned(
                (peers.a.core.config.transfer_bytes / 2) as u32,
            ).await.unwrap();
            let before = storage::snapshot();
            let mut request = peers.a.get(&peer, &peers.scope, &id);
            poll_fn(|cx| { assert!(request.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            checks.pending("held request byte quota", 2, vec![(BYTE, 1), (GET, 1), (OPEN, 0)]);
            drop(request);
            checks.phase("byte quota caller cancellation", &before, vec![
                Expected(BYTE, 0, 0, 1, 0), Expected(GET, 0, 0, 1, 0), Expected(OPEN, 0, 0, 0, 0),
            ]);
            let before = storage::snapshot();
            assert!(peers.a.get(&peer, &peers.scope, &id).await.is_err());
            checks.phase("byte quota existing deadline", &before, vec![
                Expected(BYTE, 0, 0, 1, 0), Expected(GET, 0, 1, 0, 0),
            ]);
            drop(budget);

            let mut streams = Vec::new();
            loop {
                // Exhaust currently granted credit, including replenishment from
                // earlier completed streams; unopened streams send no request.
                let mut opening = std::pin::pin!(connection.open_bi());
                let stream = poll_fn(|cx| match opening.as_mut().poll(cx) {
                    Poll::Ready(result) => Poll::Ready(Some(result.unwrap())),
                    Poll::Pending => Poll::Ready(None),
                }).await;
                let Some(stream) = stream else { break };
                streams.push(stream);
                assert!(streams.len() < 512, "bounded real stream-credit exhaustion");
            }
            let before = storage::snapshot();
            let mut request = peers.a.get(&peer, &peers.scope, &id);
            poll_fn(|cx| { assert!(request.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            checks.pending("all real bidi credits owned", 2, vec![(OPEN, 1), (GET, 1), (SEND, 0)]);
            drop(request);
            checks.phase("stream credit caller cancellation", &before, vec![
                Expected(BYTE, 1, 0, 0, 0), Expected(OPEN, 0, 0, 1, 0), Expected(GET, 0, 0, 1, 0),
            ]);
            for (mut send, mut recv) in streams {
                let _ = send.reset(0u32.into());
                let _ = recv.stop(0u32.into());
            }
            assert_eq!(peers.a.get(&peer, &peers.scope, &id).await.unwrap(), Some(bytes.to_vec()));
            assert_eq!(peers.a.core.connection(&peer, false).await.unwrap().stable_id(), stable_id);

            let quota = peers.b.core.receive_bytes.clone().acquire_many_owned(
                (peers.b.core.config.transfer_bytes / 2) as u32,
            ).await.unwrap();
            let before = storage::snapshot();
            let mut request = peers.a.get(&peer, &peers.scope, &id);
            poll_fn(|cx| { assert!(request.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            checks.pending("real receiver byte quota held", 2, vec![(RECEIVE, 1), (GET, 1), (SEND, 0)]);
            drop(request);
            checks.phase("response caller cancellation", &before, vec![
                Expected(BYTE, 1, 0, 0, 0), Expected(OPEN, 1, 0, 0, 0),
                Expected(SEND, 1, 0, 0, encode_header(&peers.scope, &id, None, 1024).unwrap().len() as u64),
                Expected(RECEIVE, 0, 0, 1, 0), Expected(GET, 0, 0, 1, 0),
            ]);
            drop(quota);
            assert_eq!(peers.a.get(&peer, &peers.scope, &id).await.unwrap(), Some(bytes.to_vec()));
            assert_eq!(peers.a.core.connection(&peer, false).await.unwrap().stable_id(), stable_id);

            let mut unregistered = peers.scope.clone();
            unregistered.identity.drive = "unregistered sibling".into();
            let before = storage::snapshot();
            assert!(peers.a.get(&peer, &unregistered, &id).await.is_err());
            checks.phase("server rejects unregistered exact scope", &before, vec![
                Expected(GET, 0, 1, 0, 0), Expected(BYTE, 1, 0, 0, 0), Expected(OPEN, 1, 0, 0, 0),
                Expected(SEND, 1, 0, 0, encode_header(&unregistered, &id, None, 1024).unwrap().len() as u64),
                Expected(RECEIVE, 0, 1, 0, 0), Expected(MISS, 0, 0, 0, 0),
            ]);
            assert_eq!(peers.a.get(&peer, &peers.scope, &id).await.unwrap(), Some(bytes.to_vec()));
            assert_eq!(peers.a.core.connection(&peer, false).await.unwrap().stable_id(), stable_id);

            let mut denied = peers.scope.clone();
            denied.identity.partition = "forbidden".into();
            let before = storage::snapshot();
            assert!(peers.a.get(&peer, &denied, &id).await.is_err());
            checks.phase("partition denied before transport work", &before, vec![
                Expected(GET, 0, 1, 0, 0), Expected(BYTE, 0, 0, 0, 0), Expected(OPEN, 0, 0, 0, 0),
            ]);
            let permits = peers.a.core.permits.clone().acquire_many_owned(peers.a.core.config.max_inflight as u32).await.unwrap();
            let before = storage::snapshot();
            assert!(peers.a.get(&peer, &peers.scope, &id).await.is_err());
            checks.phase("fail fast operation admission", &before, vec![Expected(GET, 0, 1, 0, 0), Expected(BYTE, 0, 0, 0, 0)]);
            drop(permits);
            let placement = BlockId("placement preserved".into());
            peers.a.put(&peer, &peers.scope, &placement, bytes).await.unwrap();
            assert_eq!(peers.b_cache.get(&peers.scope, &placement, IntegrityPolicy::Opaque).unwrap(), Some(bytes.to_vec()));
            assert_eq!(peers.a.core.connection(&peer, true).await.unwrap().stable_id(), stable_id);
            drop(connection);
            cleanup(peers).await;

            for unknown_ca in [false, true] {
                let peers = MetricPeers::new(!unknown_ca, unknown_ca);
                let before = storage::snapshot();
                assert!(peers.a.get(&peer, &peers.scope, &id).await.is_err());
                checks.phase("rejected mTLS identity GET", &before, vec![Expected(GET, 0, 1, 0, 0), Expected(MISS, 0, 0, 0, 0)]);
                assert!(peers.b_cache.get(&peers.scope, &id, IntegrityPolicy::Opaque).unwrap().is_none());
                cleanup(peers).await;
            }
            println!("peer_request_stage_behavior_oracles=complete full_empty_miss_verified=true connection_reuse_verified=true security_verified=true cache_owners_and_udp_released=true send_backpressure_unqualified=true");
            checks.verify();
        }).await.expect("bounded isolated peer request stage qualification");
    }

    #[tokio::test]
    #[ignore = "isolated process: MOUNT_RS_PROFILE_IO=1, storage/request traces=0"]
    async fn peer_connection_stage_metrics_preserve_bytes_and_cancellation() {
        use mount_rs_core::diagnostics::{profile, storage};
        use stage_metrics::*;
        use std::{future::poll_fn, task::Poll};
        timeout(Duration::from_secs(15),async {
            assert!(storage::enabled() && profile::enabled(),"parent must isolate the enabled profiler");
            let mut checks = Checks::default();
            let peers = MetricPeers::new(false,false);
            let peer = PeerId("b".into()); let blackhole = PeerId("blackhole".into());
            let id = BlockId("opaque".into()); let bytes = b"peer stages\0\xffcomplete binary bytes";
            peers.b_cache.insert(&peers.scope,&id,bytes,IntegrityPolicy::Opaque).unwrap();
            let before = storage::snapshot();
            assert_eq!(peers.a.get(&peer,&peers.scope,&id).await.unwrap(),Some(bytes.to_vec()));
            checks.phase("cold authenticated connection",&before,vec![Expected(LOCK,1,0,0,0),Expected(ESTABLISH,1,0,0,0)]);
            let before = storage::snapshot();
            assert_eq!(peers.a.get(&peer,&peers.scope,&id).await.unwrap(),Some(bytes.to_vec()));
            checks.phase("reused authenticated connection",&before,vec![Expected(LOCK,1,0,0,0),Expected(ESTABLISH,0,0,0,0)]);
            let connection_id = peers.a.core.connection(&peer,false).await.unwrap().stable_id();
            let before = storage::snapshot();
            peers.a.put(&peer,&peers.scope,&BlockId("placement reuse".into()),bytes).await.unwrap();
            checks.phase("placement reuses authenticated read connection",&before,vec![Expected(LOCK,1,0,0,0),Expected(ESTABLISH,0,0,0,0)]);
            assert_eq!(peers.a.core.connection(&peer,true).await.unwrap().stable_id(),connection_id);
            assert_eq!(peers.b_cache.get(&peers.scope,&BlockId("placement reuse".into()),IntegrityPolicy::Opaque).unwrap(),Some(bytes.to_vec()));
            let mut denied = peers.scope.clone(); denied.identity.partition = "other".into();
            let before = storage::snapshot();
            assert!(peers.a.get(&peer,&denied,&id).await.is_err());
            checks.phase("partition rejected before connection",&before,vec![Expected(LOCK,0,0,0,0),Expected(ESTABLISH,0,0,0,0)]);
            let slot = peers.a.core.connections.get(&peer).unwrap().clone();
            let held = slot.reads.lock().await;
            let before = storage::snapshot();
            let mut read = peers.a.get(&peer,&peers.scope,&id);
            poll_fn(|cx| { assert!(read.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            checks.pending("held live connection mutex",2,vec![(LOCK,1),(ESTABLISH,0)]);
            drop(read); drop(held);
            checks.phase("connection mutex cancellation",&before,vec![Expected(LOCK,0,0,1,0),Expected(ESTABLISH,0,0,0,0)]);
            let before = storage::snapshot();
            assert_eq!(peers.a.get(&peer,&peers.scope,&id).await.unwrap(),Some(bytes.to_vec()));
            checks.phase("live connection survives cancelled waiter",&before,vec![Expected(LOCK,1,0,0,0),Expected(ESTABLISH,0,0,0,0)]);
            let before = storage::snapshot();
            let mut put = peers.a.put(&blackhole,&peers.scope,&id,bytes);
            poll_fn(|cx| { assert!(put.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            assert!(peers.a.core.connections.get(&blackhole).unwrap().placements.try_lock().is_err(),"cold PUT holds only its placement negotiation slot");
            checks.pending("blackhole PUT establishment",1,vec![(LOCK,0),(ESTABLISH,1)]);
            let mut get = peers.a.get(&blackhole,&peers.scope,&id);
            poll_fn(|cx| { assert!(get.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            assert!(peers.a.core.connections.get(&blackhole).unwrap().reads.try_lock().is_err(),"cold GET owns its independent read negotiation slot");
            checks.pending("blackhole PUT and same-peer GET",3,vec![(LOCK,0),(ESTABLISH,2)]);
            // A different peer's reused connection remains functional while the
            // owned blackhole holds two independent role negotiations.
            assert_eq!(peers.a.get(&peer,&peers.scope,&id).await.unwrap(),Some(bytes.to_vec()));
            drop(get);
            checks.pending("blackhole PUT after GET cancellation",1,vec![(LOCK,0),(ESTABLISH,1)]);
            drop(put);
            checks.phase("overlapping establishment cancellation",&before,vec![Expected(LOCK,3,0,0,0),Expected(ESTABLISH,0,0,2,0)]);
            let blackhole_slots = peers.a.core.connections.get(&blackhole).unwrap();
            assert!(blackhole_slots.reads.try_lock().is_ok());
            assert!(blackhole_slots.placements.try_lock().is_ok());
            let before = storage::snapshot();
            let mut read = peers.a.get(&blackhole,&peers.scope,&id);
            poll_fn(|cx| { assert!(read.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            let mut placement = peers.a.put(&blackhole,&peers.scope,&id,b"");
            poll_fn(|cx| { assert!(placement.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            checks.pending("cold GET and empty PUT negotiate independently",3,vec![(LOCK,0),(ESTABLISH,2)]);
            let mut same_lane = peers.a.get(&blackhole,&peers.scope,&id);
            poll_fn(|cx| { assert!(same_lane.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            checks.pending("same read role coalesces behind negotiation",5,vec![(LOCK,1),(ESTABLISH,2)]);
            drop(placement);
            assert!(blackhole_slots.placements.try_lock().is_ok());
            assert!(blackhole_slots.reads.try_lock().is_err());
            checks.pending("read negotiation survives empty PUT cancellation",4,vec![(LOCK,1),(ESTABLISH,1)]);
            drop(same_lane);
            checks.pending("read negotiation survives same-role waiter cancellation",2,vec![(LOCK,0),(ESTABLISH,1)]);
            drop(read);
            checks.phase("reverse role negotiation and waiter cancellation",&before,vec![Expected(LOCK,2,0,1,0),Expected(ESTABLISH,0,0,2,0)]);
            assert!(blackhole_slots.reads.try_lock().is_ok());
            assert!(blackhole_slots.placements.try_lock().is_ok());
            let before = storage::snapshot();
            let mut owner = peers.a.get(&blackhole,&peers.scope,&id);
            poll_fn(|cx| { assert!(owner.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            let mut waiter = peers.a.get(&blackhole,&peers.scope,&id);
            poll_fn(|cx| { assert!(waiter.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            checks.pending("same read role has one negotiation",4,vec![(LOCK,1),(ESTABLISH,1)]);
            drop(owner);
            poll_fn(|cx| { assert!(waiter.as_mut().poll(cx).is_pending()); Poll::Ready(()) }).await;
            checks.pending("waiter negotiates after owner cancellation",2,vec![(LOCK,0),(ESTABLISH,1)]);
            assert!(blackhole_slots.reads.try_lock().is_err());
            drop(waiter);
            checks.phase("same role recovers from owner cancellation",&before,vec![Expected(LOCK,2,0,0,0),Expected(ESTABLISH,0,0,2,0)]);
            assert!(blackhole_slots.reads.try_lock().is_ok());
            assert!(peers.blackhole.local_addr().unwrap().ip().is_loopback());
            peers.shutdown().await.expect("actual peer/cache shutdown"); drop(slot); drop(peers);
            let peers = MetricPeers::new(false,false);
            let before = storage::snapshot();
            peers.a.put(&peer,&peers.scope,&id,b"").await.unwrap();
            checks.phase("cold empty PUT authenticates placement",&before,vec![Expected(LOCK,1,0,0,0),Expected(ESTABLISH,1,0,0,0)]);
            let placement_id = peers.a.core.connection(&peer,true).await.unwrap().stable_id();
            let before = storage::snapshot();
            assert_eq!(peers.a.get(&peer,&peers.scope,&id).await.unwrap(),Some(Vec::new()));
            checks.phase("read reuses authenticated placement connection",&before,vec![Expected(LOCK,1,0,0,0),Expected(ESTABLISH,0,0,0,0)]);
            assert_eq!(peers.a.core.connection(&peer,false).await.unwrap().stable_id(),placement_id);
            let full_id = BlockId("placement full bytes".into());
            peers.a.put(&peer,&peers.scope,&full_id,bytes).await.unwrap();
            assert_eq!(peers.a.get(&peer,&peers.scope,&full_id).await.unwrap(),Some(bytes.to_vec()));
            peers.shutdown().await.expect("actual peer/cache shutdown"); drop(peers);
            for unknown_ca in [false,true] {
                let peers = MetricPeers::new(!unknown_ca,unknown_ca);
                let rejected = BlockId("rejected".into()); let before = storage::snapshot();
                assert!(peers.a.put(&peer,&peers.scope,&rejected,bytes).await.is_err());
                assert!(peers.b_cache.get(&peers.scope,&rejected,IntegrityPolicy::Opaque).unwrap().is_none(),"rejected identity cannot admit bytes");
                if !unknown_ca {
                    checks.phase("mismatched certificate pin",&before,vec![Expected(LOCK,1,0,0,0),Expected(ESTABLISH,0,1,0,0)]);
                }
                // Unknown CA can fail after TLS establishment in stream work;
                // only rejection/no-admission is required for this existing arm.
                peers.shutdown().await.expect("actual peer/cache shutdown"); drop(peers);
            }
            println!("peer_stage_behavior_oracles=complete bytes_verified=true denied_identity_admission=false");
            checks.verify();
        }).await.expect("bounded isolated peer connection stage qualification");
    }
    #[tokio::test]
    async fn actual_mtls_quic_cache_only_roundtrip_and_partition_denial() {
        let mut params = rcgen::CertificateParams::new(vec!["cache-ca".into()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = rcgen::CertifiedIssuer::self_signed(params, rcgen::KeyPair::generate().unwrap())
            .unwrap();
        let (ac, ak) = cert(&ca);
        let (bc, bk) = cert(&ca);
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let cache = |d: &std::path::Path| {
            LocalCache::new(LocalCacheConfig {
                directory: d.to_owned(),
                memory_bytes: 1024,
                disk_bytes: 2048,
                max_entries: 10,
                max_blob_bytes: 1024,
            })
            .unwrap()
        };
        let a_cache = cache(ad.path());
        let b_cache = cache(bd.path());
        let s = CacheScope {
            identity: ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            backing: ConcurrentBackingId::from_bytes([2; 16]).unwrap(),
        };
        a_cache
            .register_scope(s.clone(), IntegrityPolicy::Opaque)
            .unwrap();
        b_cache
            .register_scope(s.clone(), IntegrityPolicy::Opaque)
            .unwrap();
        let addr = || {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            socket.local_addr().unwrap()
        };
        let aa = addr();
        let ba = addr();
        let ep = |address, cert: &CertificateDer<'static>| PeerEndpoint {
            address,
            server_name: "localhost".into(),
            certificate_sha256: Sha256::digest(cert.as_ref()).into(),
            partitions: BTreeSet::from(["p".into()]),
        };
        let config = |local, bind, cert, key, trusted| QuicPeerConfig {
            local: PeerId(local),
            bind,
            certificates: vec![cert],
            private_key: key,
            roots: roots.clone(),
            trusted,
            max_blob_bytes: 1024,
            max_inflight: 128,
            transfer_bytes: 128 * 1024 * 1024,
            deadline: Duration::from_secs(2),
        };
        let blackhole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let failed_peer = PeerId("blackhole".into());
        let a = QuicPeerTransport::bind(
            config(
                "a".into(),
                aa,
                ac.clone(),
                ak,
                BTreeMap::from([
                    (PeerId("b".into()), ep(ba, &bc)),
                    (
                        failed_peer.clone(),
                        PeerEndpoint {
                            address: blackhole.local_addr().unwrap(),
                            server_name: "localhost".into(),
                            certificate_sha256: [77; 32],
                            partitions: BTreeSet::from(["p".into()]),
                        },
                    ),
                ]),
            ),
            a_cache,
        )
        .unwrap();
        let b = QuicPeerTransport::bind(
            config(
                "b".into(),
                ba,
                bc,
                bk,
                BTreeMap::from([(PeerId("a".into()), ep(aa, &ac))]),
            ),
            b_cache,
        )
        .unwrap();
        let peer = PeerId("b".into());
        let id = BlockId("opaque".into());
        assert!(a.get(&peer, &s, &id).await.unwrap().is_none());
        let read_connection = a.core.connection(&peer, false).await.unwrap();
        a.put(&peer, &s, &id, b"binary\0bytes").await.unwrap();
        let placement_connection = a.core.connection(&peer, true).await.unwrap();
        assert_eq!(
            read_connection.stable_id(),
            placement_connection.stable_id(),
            "healthy sequential reads and placements reuse one connection"
        );
        assert_eq!(
            a.get(&peer, &s, &id).await.unwrap(),
            Some(b"binary\0bytes".to_vec())
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            a.get(&peer, &s, &id).await.unwrap(),
            Some(b"binary\0bytes".to_vec())
        );
        let mut readers = tokio::task::JoinSet::new();
        for _ in 0..100 {
            let a = a.clone();
            let peer = peer.clone();
            let s = s.clone();
            let id = id.clone();
            readers.spawn(async move { a.get(&peer, &s, &id).await });
        }
        while let Some(result) = readers.join_next().await {
            assert_eq!(result.unwrap().unwrap(), Some(b"binary\0bytes".to_vec()));
        }
        let connection = a.core.connection(&peer, false).await.unwrap();
        let (mut stalled_send, mut stalled_recv) = connection.open_bi().await.unwrap();
        stalled_send
            .write_all(&encode(&s, &id, None, 1024).unwrap())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let start = std::time::Instant::now();
        let concurrent = a.get(&peer, &s, &id).await;
        let latency = start.elapsed();
        let _ = stalled_send.reset(0u32.into());
        let _ = stalled_recv.stop(0u32.into());
        assert_eq!(concurrent.unwrap(), Some(b"binary\0bytes".to_vec()));
        assert!(
            latency < Duration::from_millis(200),
            "one unfinished stream stalled unrelated reads: {latency:?}"
        );
        let transport = a.clone();
        let failed_scope = s.clone();
        let failed_id = id.clone();
        let failed =
            tokio::spawn(
                async move { transport.get(&failed_peer, &failed_scope, &failed_id).await },
            );
        tokio::time::sleep(Duration::from_millis(30)).await;
        let start = std::time::Instant::now();
        let healthy = a.get(&peer, &s, &id).await;
        let latency = start.elapsed();
        failed.abort();
        let _ = failed.await;
        assert_eq!(healthy.unwrap(), Some(b"binary\0bytes".to_vec()));
        assert!(
            latency < Duration::from_millis(200),
            "failed peer handshake blocked a healthy persistent connection: {latency:?}"
        );
        let mut denied = s;
        denied.identity.partition = "other".into();
        assert!(a.get(&peer, &denied, &id).await.is_err());
        a.shutdown().await.expect("actual peer shutdown");
        b.shutdown().await.expect("actual peer shutdown");
    }
    #[tokio::test]
    async fn actual_quic_rejects_mismatched_pin_and_unknown_client_ca() {
        for unknown_ca in [false, true] {
            let make_ca = || {
                let mut p = rcgen::CertificateParams::new(vec!["cache-ca".into()]).unwrap();
                p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
                rcgen::CertifiedIssuer::self_signed(p, rcgen::KeyPair::generate().unwrap()).unwrap()
            };
            let ca = make_ca();
            let foreign = make_ca();
            let (ac, ak) = cert(if unknown_ca { &foreign } else { &ca });
            let (bc, bk) = cert(&ca);
            let mut roots = rustls::RootCertStore::empty();
            roots.add(ca.der().clone()).unwrap();
            let ad = tempfile::tempdir().unwrap();
            let bd = tempfile::tempdir().unwrap();
            let cache = |d: &std::path::Path| {
                LocalCache::new(LocalCacheConfig {
                    directory: d.into(),
                    memory_bytes: 1024,
                    disk_bytes: 2048,
                    max_entries: 10,
                    max_blob_bytes: 1024,
                })
                .unwrap()
            };
            let a_cache = cache(ad.path());
            let b_cache = cache(bd.path());
            let scope = CacheScope {
                identity: ScopeIdentity {
                    cluster: "c".into(),
                    partition: "p".into(),
                    drive: "d".into(),
                },
                backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            };
            a_cache
                .register_scope(scope.clone(), IntegrityPolicy::Opaque)
                .unwrap();
            b_cache
                .register_scope(scope.clone(), IntegrityPolicy::Opaque)
                .unwrap();
            let addr = || {
                std::net::UdpSocket::bind("127.0.0.1:0")
                    .unwrap()
                    .local_addr()
                    .unwrap()
            };
            let aa = addr();
            let ba = addr();
            let endpoint = |address, cert: &CertificateDer<'static>, wrong| PeerEndpoint {
                address,
                server_name: "localhost".into(),
                certificate_sha256: if wrong {
                    [99; 32]
                } else {
                    Sha256::digest(cert.as_ref()).into()
                },
                partitions: BTreeSet::from(["p".into()]),
            };
            let cfg = |local, bind, cert, key, trusted| QuicPeerConfig {
                local: PeerId(local),
                bind,
                certificates: vec![cert],
                private_key: key,
                roots: roots.clone(),
                trusted,
                max_blob_bytes: 1024,
                max_inflight: 4,
                transfer_bytes: 128 * 1024 * 1024,
                deadline: Duration::from_millis(500),
            };
            let a = QuicPeerTransport::bind(
                cfg(
                    "a".into(),
                    aa,
                    ac.clone(),
                    ak,
                    BTreeMap::from([(PeerId("b".into()), endpoint(ba, &bc, !unknown_ca))]),
                ),
                a_cache,
            )
            .unwrap();
            let b = QuicPeerTransport::bind(
                cfg(
                    "b".into(),
                    ba,
                    bc,
                    bk,
                    BTreeMap::from([(PeerId("a".into()), endpoint(aa, &ac, false))]),
                ),
                b_cache.clone(),
            )
            .unwrap();
            let id = BlockId("opaque".into());
            assert!(
                a.put(&PeerId("b".into()), &scope, &id, b"untrusted")
                    .await
                    .is_err()
            );
            assert!(
                b_cache
                    .get(&scope, &id, IntegrityPolicy::Opaque)
                    .unwrap()
                    .is_none()
            );
            a.shutdown().await.expect("actual peer shutdown");
            b.shutdown().await.expect("actual peer shutdown");
        }
    }
}
