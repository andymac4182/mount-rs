//! Bounded loopback qualification of the real authenticated peer/cache/backing seam.
#![cfg(unix)]
use async_trait::async_trait;
use mount_rs_blob_cache::*;
use mount_rs_core::{
    Result,
    error::{ErrorCode, FsError},
    storage::{BlockId, BlockStore, ConcurrentBackingId},
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::{Future, poll_fn},
    net::SocketAddr,
    path::PathBuf,
    process::{ExitStatus, Stdio},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::Poll,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, Command},
    sync::Semaphore,
    time::{Instant, timeout},
};
const BOUND: Duration = Duration::from_secs(15);
const REDIS_BOUND: Duration = Duration::from_secs(3);
const CLEANUP_BOUND: Duration = Duration::from_secs(3);
#[path = "support/stage_metrics.rs"]
#[allow(dead_code)]
mod stage_metrics;
fn io_error() -> FsError {
    FsError::new(ErrorCode::Eio)
}
fn scope() -> CacheScope {
    CacheScope {
        identity: ScopeIdentity {
            cluster: "qualification".into(),
            partition: "p".into(),
            drive: "d".into(),
        },
        backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
    }
}
fn block(bytes: &[u8]) -> BlockId {
    BlockId(format!("b{:x}", Sha256::digest(bytes)))
}
// This memory fixture counts calls and returned bytes at the BlockStore boundary.
// Its synthetic durability flag does not measure SSD IOPS or provider durability.
struct Backing {
    committed: Mutex<BTreeMap<BlockId, Vec<u8>>>,
    staged: Mutex<BTreeMap<BlockId, Vec<u8>>>,
    gets: AtomicU64,
    bytes: AtomicU64,
    gate: AtomicBool,
    fail: AtomicBool,
    entered: Semaphore,
    release: Semaphore,
}
impl Backing {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            committed: Mutex::new(BTreeMap::new()),
            staged: Mutex::new(BTreeMap::new()),
            gets: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            gate: AtomicBool::new(false),
            fail: AtomicBool::new(false),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        })
    }
    fn seed(&self, bytes: &[u8]) -> BlockId {
        let id = block(bytes);
        self.committed
            .lock()
            .unwrap()
            .insert(id.clone(), bytes.to_vec());
        id
    }
    fn reads(&self) -> u64 {
        self.gets.load(Ordering::SeqCst)
    }
}
#[async_trait]
impl BlockStore for Backing {
    fn durable(&self) -> bool {
        true
    }
    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        Ok(scope().backing)
    }
    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        if expected == scope().backing {
            Ok(())
        } else {
            Err(io_error())
        }
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let id = block(bytes);
        self.staged
            .lock()
            .unwrap()
            .insert(id.clone(), bytes.to_vec());
        Ok(id)
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let value = self
            .committed
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .or_else(|| self.staged.lock().unwrap().get(id).cloned())
            .ok_or_else(io_error)?;
        self.bytes.fetch_add(value.len() as u64, Ordering::SeqCst);
        Ok(value)
    }
    async fn flush(&self) -> Result<()> {
        if self.gate.load(Ordering::SeqCst) {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        if self.fail.load(Ordering::SeqCst) {
            return Err(io_error());
        }
        self.committed
            .lock()
            .unwrap()
            .append(&mut self.staged.lock().unwrap());
        Ok(())
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.committed.lock().unwrap().remove(id);
        Ok(())
    }
}
struct ResponseGate {
    arrived: Semaphore,
    release: tokio::sync::watch::Sender<bool>,
}
#[derive(Clone, Copy, Default)]
struct PeerPutObservation {
    started: u64,
    completed: u64,
    errors: u64,
    cancelled: u64,
    inflight: u64,
}
struct CountPeer {
    peer: Arc<QuicPeerTransport>,
    gets: AtomicU64,
    gate: Mutex<Option<Arc<ResponseGate>>>,
    puts: Mutex<BTreeMap<BlockId, PeerPutObservation>>,
    put_completions: Semaphore,
    diagnostics: AtomicBool,
    trace_epoch: Instant,
}
impl CountPeer {
    fn put_snapshot(&self) -> PeerPutObservation {
        self.puts
            .lock()
            .unwrap()
            .values()
            .fold(PeerPutObservation::default(), |mut total, next| {
                total.started += next.started;
                total.completed += next.completed;
                total.errors += next.errors;
                total.cancelled += next.cancelled;
                total.inflight += next.inflight;
                total
            })
    }
    async fn observe_put_completion(&self, phase: &str, id: &BlockId, expected_errors: u64) {
        let observation = timeout(Duration::from_secs(2), async {
            loop {
                let observation = self
                    .puts
                    .lock()
                    .unwrap()
                    .get(id)
                    .copied()
                    .unwrap_or_default();
                if observation.completed != 0 || observation.cancelled != 0 {
                    break observation;
                }
                // Retained permits avoid a lost completion between the snapshot
                // and this await; the per-block record identifies the actual PUT.
                self.put_completions.acquire().await.unwrap().forget();
            }
        })
        .await
        .expect("bounded actual placement PUT completion");
        assert_eq!(
            observation.started, 1,
            "one placement PUT started for {phase}"
        );
        assert_eq!(
            observation.completed, 1,
            "normal placement PUT completion for {phase}"
        );
        assert_eq!(
            observation.errors, expected_errors,
            "placement PUT outcome for {phase}"
        );
        assert_eq!(
            observation.cancelled, 0,
            "cancelled placement is not complete for {phase}"
        );
        assert_eq!(observation.inflight, 0, "placement PUT settled for {phase}");
        println!(
            "peer_put_barrier=observed phase={phase} block={} at_us={} \
             started=1 completed=1 errors={} cancelled=0 inflight=0",
            id.0,
            self.trace_epoch.elapsed().as_micros(),
            observation.errors
        );
    }
}
// A dropped placement future is observable cancellation, not normal completion.
struct PeerPutAttempt<'a> {
    counted: &'a CountPeer,
    peer: &'a PeerId,
    id: &'a BlockId,
    started: Instant,
    completed: bool,
}
impl<'a> PeerPutAttempt<'a> {
    fn start(counted: &'a CountPeer, peer: &'a PeerId, id: &'a BlockId) -> Self {
        let attempt = Self {
            counted,
            peer,
            id,
            started: Instant::now(),
            completed: false,
        };
        let observation = {
            let mut puts = counted.puts.lock().unwrap();
            let observation = puts.entry(id.clone()).or_default();
            observation.started += 1;
            observation.inflight += 1;
            *observation
        };
        attempt.trace("start", "pending", observation);
        attempt
    }
    fn finish(&mut self, error: bool) {
        let observation = {
            let mut puts = self.counted.puts.lock().unwrap();
            let observation = puts.get_mut(self.id).unwrap();
            observation.completed += 1;
            observation.errors += u64::from(error);
            observation.inflight -= 1;
            *observation
        };
        self.completed = true;
        self.trace("complete", if error { "error" } else { "ok" }, observation);
        self.counted.put_completions.add_permits(1);
    }
    fn trace(&self, event: &str, outcome: &str, observation: PeerPutObservation) {
        if self.counted.diagnostics.load(Ordering::SeqCst) {
            println!(
                "peer_put_event={event} peer={} block={} outcome={outcome} at_us={} \
                 elapsed_us={} started={} completed={} errors={} cancelled={} inflight={}",
                self.peer.0,
                self.id.0,
                self.counted.trace_epoch.elapsed().as_micros(),
                self.started.elapsed().as_micros(),
                observation.started,
                observation.completed,
                observation.errors,
                observation.cancelled,
                observation.inflight
            );
        }
    }
}
impl Drop for PeerPutAttempt<'_> {
    fn drop(&mut self) {
        if !self.completed {
            let observation = {
                let mut puts = self.counted.puts.lock().unwrap();
                let observation = puts.get_mut(self.id).unwrap();
                observation.cancelled += 1;
                observation.inflight -= 1;
                *observation
            };
            self.trace("cancel", "cancelled", observation);
            self.counted.put_completions.add_permits(1);
        }
    }
}
#[async_trait]
impl PeerTransport for CountPeer {
    async fn get(&self, p: &PeerId, s: &CacheScope, id: &BlockId) -> Result<Option<Vec<u8>>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let started = Instant::now();
        if self.diagnostics.load(Ordering::SeqCst) {
            println!(
                "peer_get_event=start peer={} block={} outcome=pending at_us={}",
                p.0,
                id.0,
                self.trace_epoch.elapsed().as_micros()
            );
        }
        let result = self.peer.get(p, s, id).await;
        if self.diagnostics.load(Ordering::SeqCst) {
            let (outcome, bytes) = match &result {
                Ok(Some(bytes)) => ("hit", bytes.len()),
                Ok(None) => ("miss", 0),
                Err(_) => ("error", 0),
            };
            println!(
                "peer_get_event=complete peer={} block={} outcome={outcome} at_us={} \
                 elapsed_us={} response_bytes={bytes}",
                p.0,
                id.0,
                self.trace_epoch.elapsed().as_micros(),
                started.elapsed().as_micros()
            );
        }
        let gate = self.gate.lock().unwrap().clone();
        if let Some(gate) = gate {
            let mut release = gate.release.subscribe();
            gate.arrived.add_permits(1);
            while !*release.borrow_and_update() {
                release.changed().await.unwrap();
            }
        }
        result
    }
    async fn put(&self, p: &PeerId, s: &CacheScope, id: &BlockId, b: &[u8]) -> Result<()> {
        let mut attempt = PeerPutAttempt::start(self, p, id);
        let result = self.peer.put(p, s, id, b).await;
        attempt.finish(result.is_err());
        result
    }
}
// Discovery deliberately retains A as a stale holder; reads do not repopulate A via placement.
struct Hint {
    placement: bool,
}
#[async_trait]
impl Discovery for Hint {
    async fn locate(&self, _: &CacheScope, _: &BlockId) -> Result<Vec<PeerId>> {
        Ok(vec![PeerId("a".into())])
    }
    fn placement(&self, _: &CacheScope, _: &BlockId) -> Vec<PeerId> {
        if self.placement {
            vec![PeerId("a".into())]
        } else {
            vec![]
        }
    }
    async fn advertise(&self, _: &CacheScope, _: &BlockId, _: &PeerId) -> Result<()> {
        Ok(())
    }
    async fn withdraw(&self, _: &CacheScope, _: &BlockId, _: &PeerId) -> Result<()> {
        Ok(())
    }
    async fn heartbeat(&self, _: &PeerId) -> Result<()> {
        Ok(())
    }
}
struct Pair {
    cleaned: bool,
    dir: Option<tempfile::TempDir>,
    cache_lifetimes: Vec<Weak<LocalCache>>,
    cleanup_complete: Option<tokio::sync::oneshot::Sender<PairCleanupResult>>,
    a_cache: Option<Arc<LocalCache>>,
    b_cache: Arc<LocalCache>,
    a: Option<Arc<QuicPeerTransport>>,
    b: Arc<QuicPeerTransport>,
    counted: Arc<CountPeer>,
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    roots: rustls::RootCertStore,
    trusted: BTreeMap<PeerId, PeerEndpoint>,
    address: SocketAddr,
}
fn cache(path: &std::path::Path, memory: usize, disk: usize) -> Arc<LocalCache> {
    LocalCache::new(LocalCacheConfig {
        directory: path.to_owned(),
        memory_bytes: memory,
        disk_bytes: disk,
        max_entries: 128,
        max_blob_bytes: 4096,
    })
    .unwrap()
}
fn leaf(
    ca: &rcgen::CertifiedIssuer<'static, rcgen::KeyPair>,
) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let key = rcgen::KeyPair::generate().unwrap();
    let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
    params.extended_key_usages = vec![
        rcgen::ExtendedKeyUsagePurpose::ServerAuth,
        rcgen::ExtendedKeyUsagePurpose::ClientAuth,
    ];
    (
        params.signed_by(&key, ca).unwrap().der().clone(),
        rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
    )
}
fn endpoint(address: SocketAddr, cert: &CertificateDer<'static>) -> PeerEndpoint {
    PeerEndpoint {
        address,
        server_name: "localhost".into(),
        certificate_sha256: Sha256::digest(cert.as_ref()).into(),
        partitions: BTreeSet::from(["p".into()]),
    }
}
fn config(
    local: &str,
    bind: SocketAddr,
    cert: CertificateDer<'static>,
    key: PrivateKeyDer<'static>,
    roots: rustls::RootCertStore,
    trusted: BTreeMap<PeerId, PeerEndpoint>,
) -> QuicPeerConfig {
    QuicPeerConfig {
        local: PeerId(local.into()),
        bind,
        certificates: vec![cert],
        private_key: key,
        roots,
        trusted,
        max_blob_bytes: 4096,
        max_inflight: 128,
        transfer_bytes: 4 * 1024 * 1024,
        deadline: Duration::from_millis(400),
    }
}
impl Pair {
    fn new(memory: usize, disk: usize) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let a_cache = cache(&dir.path().join("a"), memory, disk);
        let b_cache = cache(&dir.path().join("b"), 8192, 32768);
        a_cache
            .register_scope(scope(), IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        b_cache
            .register_scope(scope(), IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["cache-ca".into()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = rcgen::CertifiedIssuer::self_signed(params, rcgen::KeyPair::generate().unwrap())
            .unwrap();
        let (cert, key) = leaf(&ca);
        let (bc, bk) = leaf(&ca);
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        // Inbound identity uses the leaf pin, so A can bind :0 before B's address is known.
        let trusted = BTreeMap::from([(
            PeerId("b".into()),
            endpoint("127.0.0.1:0".parse().unwrap(), &bc),
        )]);
        let a = QuicPeerTransport::bind(
            config(
                "a",
                "127.0.0.1:0".parse().unwrap(),
                cert.clone(),
                key.clone_key(),
                roots.clone(),
                trusted.clone(),
            ),
            a_cache.clone(),
        )
        .unwrap();
        let address = a.local_addr().unwrap();
        // B permits outgoing forbidden queries so A must enforce its inbound allowance.
        let mut a_endpoint = endpoint(address, &cert);
        a_endpoint.partitions.insert("forbidden".into());
        let b = QuicPeerTransport::bind(
            config(
                "b",
                "127.0.0.1:0".parse().unwrap(),
                bc,
                bk,
                roots.clone(),
                BTreeMap::from([(PeerId("a".into()), a_endpoint)]),
            ),
            b_cache.clone(),
        )
        .unwrap();
        let counted = Arc::new(CountPeer {
            peer: b.clone(),
            gets: AtomicU64::new(0),
            gate: Mutex::new(None),
            puts: Mutex::new(BTreeMap::new()),
            put_completions: Semaphore::new(0),
            diagnostics: AtomicBool::new(false),
            trace_epoch: Instant::now(),
        });
        Self {
            cleaned: false,
            dir: Some(dir),
            cache_lifetimes: vec![Arc::downgrade(&a_cache), Arc::downgrade(&b_cache)],
            cleanup_complete: None,
            a_cache: Some(a_cache),
            b_cache,
            a: Some(a),
            b,
            counted,
            cert,
            key,
            roots,
            trusted,
            address,
        }
    }
    fn runtime(&self, placement: bool) -> Arc<DistributedRuntime> {
        DistributedRuntime::new(
            Arc::new(Hint { placement }),
            self.counted.clone(),
            PeerId("b".into()),
            DistributedConfig {
                max_inflight_misses: 128,
                deadline: Duration::from_millis(600),
                hedge_delay: Duration::from_millis(300),
                ..Default::default()
            },
        )
        .unwrap()
    }
    async fn store(
        &self,
        backing: Arc<Backing>,
        runtime: Arc<DistributedRuntime>,
    ) -> Arc<CachedBlockStore> {
        let store = Arc::new(
            CachedBlockStore::new(
                backing,
                self.b_cache.clone(),
                scope().identity,
                IntegrityPolicy::Sha256Prefixed,
            )
            .with_runtime(runtime),
        );
        store.prepare_concurrent_backing().await.unwrap();
        store
    }
    async fn stop_a(&mut self) {
        self.a.take().unwrap().shutdown().await;
        let local = self.a_cache.take().unwrap();
        local.shutdown().await;
        drop(local);
    }
    fn restart_a(&mut self) {
        let local = cache(&self.dir.as_ref().unwrap().path().join("a"), 0, 32768);
        local
            .register_scope(scope(), IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        let peer = QuicPeerTransport::bind(
            config(
                "a",
                self.address,
                self.cert.clone(),
                self.key.clone_key(),
                self.roots.clone(),
                self.trusted.clone(),
            ),
            local.clone(),
        )
        .unwrap();
        self.track_cache(&local);
        self.a_cache = Some(local);
        self.a = Some(peer);
    }
    fn track_cache(&mut self, cache: &Arc<LocalCache>) {
        self.cache_lifetimes.push(Arc::downgrade(cache));
    }
    fn cleanup_completion(&mut self) -> tokio::sync::oneshot::Receiver<PairCleanupResult> {
        let (complete, completed) = tokio::sync::oneshot::channel();
        assert!(self.cleanup_complete.replace(complete).is_none());
        completed
    }
    async fn shutdown(&mut self) {
        if self.a.is_some() {
            self.stop_a().await;
        }
        self.b.shutdown().await;
        self.b_cache.shutdown().await;
        self.cleaned = true;
    }
}
type PairCleanupResult = std::result::Result<(), String>;
async fn wait_for_cache_owners(cache_lifetimes: Vec<Weak<LocalCache>>) -> PairCleanupResult {
    // LocalCache::shutdown discards its timeout result. Zero remaining Arc owners
    // proves its non-cancellable blocking closures and other cache users are gone.
    while cache_lifetimes
        .iter()
        .any(|cache| cache.strong_count() != 0)
    {
        tokio::task::yield_now().await;
    }
    Ok(())
}
fn pair_cleanup(
    directory: Option<tempfile::TempDir>,
    work: impl Future<Output = PairCleanupResult>,
    complete: Option<tokio::sync::oneshot::Sender<PairCleanupResult>>,
) -> impl Future<Output = ()> {
    // This executes before spawn, so dropping an unpolled/cancelled worker cannot
    // remove files while endpoint or blocking cache work is still unobserved.
    let retained = directory.map(tempfile::TempDir::keep);
    eprintln!(
        "peer_cache_cleanup=pending retained_directory={:?}",
        retained.as_deref()
    );
    async move {
        let result = match timeout(CLEANUP_BOUND, work).await {
            Ok(result) => result,
            Err(_) => {
                Err("peer/cache cleanup deadline; remaining owners are unobserved".to_owned())
            }
        };
        let result = result.and_then(|()| {
            if let Some(path) = &retained {
                std::fs::remove_dir_all(path)
                    .map_err(|error| format!("peer/cache directory removal failed: {error}"))?;
            }
            Ok(())
        });
        if let Err(error) = &result {
            eprintln!(
                "{error}; retained peer/cache directory {:?}",
                retained.as_deref()
            );
        }
        if let Some(complete) = complete {
            let _ = complete.send(result);
        }
    }
}
// Drop cleanup covers unwinding and outer timeout. Directory removal requires
// observed endpoint shutdown and loss of every tracked cache owner, not a drain timeout.
impl Drop for Pair {
    fn drop(&mut self) {
        let dir = self.dir.take();
        let directory_path = dir.as_ref().map(|directory| directory.path().to_owned());
        let a = self.a.take();
        let ac = self.a_cache.take();
        let b = self.b.clone();
        let bc = self.b_cache.clone();
        let cleaned = self.cleaned;
        let caches = std::mem::take(&mut self.cache_lifetimes);
        let complete = self.cleanup_complete.take();
        let work = async move {
            if !cleaned {
                if let Some(a) = &a {
                    a.shutdown().await;
                }
                b.shutdown().await;
                if let Some(ac) = &ac {
                    ac.shutdown().await;
                }
                bc.shutdown().await;
            }
            drop(a);
            drop(ac);
            drop(b);
            drop(bc);
            wait_for_cache_owners(caches).await
        };
        let cleanup = pair_cleanup(dir, work, complete);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(cleanup);
        } else {
            eprintln!(
                "peer/cache cleanup has no runtime; retained {:?}; remaining owners are unobserved",
                directory_path.as_deref()
            );
            drop(cleanup);
        }
    }
}
#[test]
fn cancelled_pair_cleanup_future_retains_owned_directory() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_owned();
    let (complete, mut completed) = tokio::sync::oneshot::channel();
    let cleanup = pair_cleanup(
        Some(directory),
        std::future::pending::<PairCleanupResult>(),
        Some(complete),
    );
    drop(cleanup);
    assert!(
        path.is_dir(),
        "unobserved cache drain must retain the directory"
    );
    assert!(matches!(
        completed.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    std::fs::remove_dir_all(path).unwrap();
}
#[tokio::test]
async fn pair_cleanup_waits_for_all_cache_owners_before_removing_directory() {
    timeout(BOUND, async {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().to_owned();
        let local = cache(&path.join("cache"), 0, 32768);
        let caches = vec![Arc::downgrade(&local)];
        let (entered, ready) = tokio::sync::oneshot::channel();
        let work = async move {
            entered.send(()).unwrap();
            wait_for_cache_owners(caches).await
        };
        let (complete, mut completed) = tokio::sync::oneshot::channel();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(pair_cleanup(Some(directory), work, Some(complete)));
        ready.await.unwrap();
        assert!(path.is_dir(), "a live cache owner keeps its directory");
        assert!(matches!(
            completed.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(local);
        timeout(CLEANUP_BOUND, completed)
            .await
            .expect("bounded observed cache-owner release")
            .expect("cache cleanup worker remained available")
            .expect("observed cache cleanup succeeded");
        tasks.join_next().await.unwrap().unwrap();
        assert!(
            !path.exists(),
            "observed cache-owner release permits removal"
        );
    })
    .await
    .expect("bounded cache cleanup ownership control");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn explicit_pair_shutdown_retains_directory_until_all_cache_owners_release() {
    timeout(BOUND, async {
        let mut pair = Pair::new(0, 32768);
        let directory = pair.dir.as_ref().unwrap().path().to_owned();
        let cache_owner = pair.b_cache.clone();
        let mut completed = pair.cleanup_completion();
        pair.shutdown().await;
        drop(pair);
        assert!(
            directory.is_dir(),
            "shutdown is not proof that all cache owners are gone"
        );
        assert!(matches!(
            completed.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        drop(cache_owner);
        timeout(CLEANUP_BOUND, completed)
            .await
            .expect("bounded cleanup after explicit shutdown")
            .expect("explicit-shutdown cleanup worker remained available")
            .expect("all cache owners were observed released");
        assert!(
            !directory.exists(),
            "directory removal follows observed owner release"
        );
    })
    .await
    .expect("bounded explicit-shutdown directory ownership control");
}
async fn eventually(mut check: impl AsyncFnMut() -> bool) {
    timeout(Duration::from_secs(3), async {
        loop {
            if check().await {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("bounded eventual cache observation");
}
struct RedisFixture {
    child: Option<Child>,
    reaped: Option<ExitStatus>,
    directory: Option<tempfile::TempDir>,
    cleanup_complete: Option<tokio::sync::oneshot::Sender<RedisCleanupResult>>,
    address: SocketAddr,
    password: String,
}
type RedisCleanupResult = std::result::Result<ExitStatus, String>;
impl RedisFixture {
    async fn start() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("mount-rs-redis-peer-")
            .tempdir()
            .unwrap();
        let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = reservation.local_addr().unwrap();
        drop(reservation);
        // A unique credential prevents readiness from accepting another listener
        // if the reserved port is taken before our child binds it.
        let password = format!(
            "fixture-{:x}",
            Sha256::digest(rcgen::KeyPair::generate().unwrap().serialize_der())
        );
        let binary = std::env::var_os("MOUNT_RS_CACHE_REDIS_SERVER")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let homebrew = PathBuf::from("/opt/homebrew/bin/redis-server");
                if homebrew.is_file() {
                    homebrew
                } else {
                    PathBuf::from("redis-server")
                }
            });
        let log = std::fs::File::create(directory.path().join("redis.log")).unwrap();
        let child = Command::new(&binary)
            .args(["--bind", "127.0.0.1", "--port"])
            .arg(address.port().to_string())
            .args(["--save", "", "--appendonly", "no", "--dir"])
            .arg(directory.path())
            .args([
                "--protected-mode",
                "yes",
                "--daemonize",
                "no",
                "--maxclients",
                "16",
                "--requirepass",
            ])
            .arg(&password)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .kill_on_drop(true)
            .spawn()
            .unwrap_or_else(|error| panic!("start owned Redis {}: {error}", binary.display()));
        // Own both child and directory before the first cancellable readiness wait.
        let mut fixture = Self {
            child: Some(child),
            reaped: None,
            directory: Some(directory),
            cleanup_complete: None,
            address,
            password,
        };
        timeout(REDIS_BOUND, async {
            loop {
                assert!(
                    fixture
                        .child
                        .as_mut()
                        .unwrap()
                        .try_wait()
                        .expect("inspect owned Redis child")
                        .is_none(),
                    "owned Redis exited before authenticated PING readiness"
                );
                if fixture.ping().await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("owned Redis authenticated PING readiness deadline");
        fixture
    }
    async fn ping(&self) -> std::io::Result<()> {
        timeout(Duration::from_millis(200), async {
            let mut stream = tokio::net::TcpStream::connect(self.address).await?;
            let request = format!(
                "*2\r\n$4\r\nAUTH\r\n${}\r\n{}\r\n*1\r\n$4\r\nPING\r\n",
                self.password.len(),
                self.password
            );
            stream.write_all(request.as_bytes()).await?;
            let mut response = [0; 12];
            stream.read_exact(&mut response).await?;
            if &response != b"+OK\r\n+PONG\r\n" {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "owned Redis did not authenticate and reply PONG",
                ));
            }
            Ok(())
        })
        .await
        .map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::TimedOut, "owned Redis PING deadline")
        })?
    }
    fn config(&self) -> RedisConfig {
        RedisConfig {
            address: self.address.to_string(),
            username: None,
            password: Some(self.password.clone()),
            namespace: self
                .directory
                .as_ref()
                .unwrap()
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            ttl: Duration::from_secs(60),
            deadline: Duration::from_millis(100),
            max_reply_bytes: 8192,
            tls: None,
        }
    }
    async fn stop(&mut self) {
        if let Some(child) = &mut self.child {
            self.reaped = Some(
                kill_owned_redis(child)
                    .await
                    .expect("kill and reap owned Redis child"),
            );
            drop(self.child.take());
        }
    }
    fn cleanup_completion(&mut self) -> tokio::sync::oneshot::Receiver<RedisCleanupResult> {
        let (complete, completed) = tokio::sync::oneshot::channel();
        assert!(self.cleanup_complete.replace(complete).is_none());
        completed
    }
}
async fn kill_owned_redis(child: &mut Child) -> std::io::Result<ExitStatus> {
    if let Some(status) = child.try_wait()? {
        return Ok(status);
    }
    child.start_kill()?;
    timeout(REDIS_BOUND, child.wait()).await.map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::TimedOut, "owned Redis reap deadline")
    })?
}
fn redis_cleanup(
    mut child: Option<Child>,
    reaped: Option<ExitStatus>,
    directory: Option<tempfile::TempDir>,
    complete: Option<tokio::sync::oneshot::Sender<RedisCleanupResult>>,
) -> impl Future<Output = ()> {
    // Retain synchronously, before constructing or spawning the cancellable future.
    // Cancelling that future drops only a PathBuf, never an auto-removing TempDir.
    let retained = directory.map(tempfile::TempDir::keep);
    eprintln!(
        "redis_cleanup=pending retained_directory={:?}",
        retained.as_deref()
    );
    async move {
        let result = if let Some(child) = &mut child {
            kill_owned_redis(child)
                .await
                .map_err(|error| format!("owned Redis reap failed: {error}"))
        } else {
            reaped.ok_or_else(|| "owned Redis has no observed child reap".to_owned())
        };
        finish_redis_cleanup(retained, result, complete);
    }
}
fn finish_redis_cleanup(
    retained: Option<PathBuf>,
    result: RedisCleanupResult,
    complete: Option<tokio::sync::oneshot::Sender<RedisCleanupResult>>,
) {
    let result = result.and_then(|status| {
        if let Some(path) = &retained {
            std::fs::remove_dir_all(path).map_err(|error| {
                format!("owned Redis was reaped but directory removal failed: {error}")
            })?;
        }
        Ok(status)
    });
    if let Err(error) = &result {
        eprintln!(
            "{error}; retained Redis directory {:?}",
            retained.as_deref()
        );
    }
    if let Some(complete) = complete {
        let _ = complete.send(result);
    }
}
impl Drop for RedisFixture {
    fn drop(&mut self) {
        let directory = self.directory.take();
        let complete = self.cleanup_complete.take();
        if let Some(mut child) = self.child.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                // redis_cleanup retains the directory before spawn can own or drop it.
                let cleanup = redis_cleanup(Some(child), None, directory, complete);
                handle.spawn(cleanup);
            } else {
                let retained = directory.map(tempfile::TempDir::keep);
                let _ = child.start_kill();
                finish_redis_cleanup(
                    retained,
                    Err("owned Redis cleanup has no runtime; child reap is unobserved".to_owned()),
                    complete,
                );
            }
        } else {
            // Explicit stop already observed wait/try_wait before releasing the child.
            let retained = directory.map(tempfile::TempDir::keep);
            finish_redis_cleanup(
                retained,
                self.reaped
                    .take()
                    .ok_or_else(|| "owned Redis has no observed child reap".to_owned()),
                complete,
            );
        }
    }
}
#[test]
fn cancelled_redis_cleanup_future_retains_directory_without_reap_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_owned();
    let (complete, mut completed) = tokio::sync::oneshot::channel();
    let cleanup = redis_cleanup(None, None, Some(directory), Some(complete));
    // An unpolled cleanup future models cancellation at spawn/runtime teardown.
    // No child is launched and no reap is claimed by this retention control.
    drop(cleanup);
    assert!(
        path.is_dir(),
        "unobserved reap must leave the directory retained"
    );
    assert!(matches!(
        completed.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    std::fs::remove_dir_all(path).unwrap();
}
#[test]
fn failed_redis_cleanup_retains_directory_and_reports_reap_error() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.keep();
    let (complete, mut completed) = tokio::sync::oneshot::channel();
    finish_redis_cleanup(
        Some(path.clone()),
        Err("injected owned-child reap failure".to_owned()),
        Some(complete),
    );
    assert_eq!(
        completed.try_recv().unwrap(),
        Err("injected owned-child reap failure".to_owned())
    );
    assert!(
        path.is_dir(),
        "failed reap must leave the directory retained"
    );
    std::fs::remove_dir_all(path).unwrap();
}
async fn redis_payload(phase: &str, fallback: &FixedDiscovery, selected: &PeerId) -> Vec<u8> {
    for nonce in 0_u16..256 {
        let mut bytes = (0..768).map(|i| (i % 256) as u8).collect::<Vec<_>>();
        bytes.extend_from_slice(phase.as_bytes());
        bytes.extend_from_slice(&nonce.to_be_bytes());
        if fallback.locate(&scope(), &block(&bytes)).await.unwrap() == vec![selected.clone()] {
            return bytes;
        }
    }
    panic!(
        "bounded payload selection did not find fallback {} for {phase}",
        selected.0
    );
}
async fn assert_redis_phase(
    phase: &str,
    pair: &Pair,
    store: &CachedBlockStore,
    backing: &Backing,
    id: &BlockId,
    bytes: &[u8],
    expected_backing_gets: u64,
) {
    let before_gets = backing.reads();
    let before_bytes = backing.bytes.load(Ordering::SeqCst);
    let before_peer = pair.counted.gets.load(Ordering::SeqCst);
    let before_puts = pair.counted.put_snapshot();
    let before_metrics = store.metrics().snapshot();
    let started = Instant::now();
    let actual = timeout(Duration::from_secs(2), store.get(id))
        .await
        .expect("bounded Redis/peer read")
        .unwrap();
    assert_eq!(actual, bytes, "full binary bytes for {phase}");
    let backing_gets = backing.reads() - before_gets;
    let backing_bytes = backing.bytes.load(Ordering::SeqCst) - before_bytes;
    let peer_gets = pair.counted.gets.load(Ordering::SeqCst) - before_peer;
    let puts = pair.counted.put_snapshot();
    let metrics = store.metrics().snapshot();
    println!(
        "phase={phase} payload_bytes={} memory_backing_get_calls={backing_gets} \
         memory_backing_bytes={backing_bytes} peer_get_attempts={peer_gets} peer_hits={} \
         cache_errors={} elapsed_us={} peer_put_started={} peer_put_completed={} \
         peer_put_errors={} peer_put_cancelled={} peer_put_inflight={}",
        bytes.len(),
        metrics.peer_hits - before_metrics.peer_hits,
        metrics.cache_errors - before_metrics.cache_errors,
        started.elapsed().as_micros(),
        puts.started - before_puts.started,
        puts.completed - before_puts.completed,
        puts.errors - before_puts.errors,
        puts.cancelled - before_puts.cancelled,
        puts.inflight
    );
    assert_eq!(
        backing_gets, expected_backing_gets,
        "backing GET calls for {phase}"
    );
    assert_eq!(backing_bytes, expected_backing_gets * bytes.len() as u64);
    assert_eq!(peer_gets, 1, "one actual QUIC peer attempt for {phase}");
    assert_eq!(
        metrics.backing_fetches - before_metrics.backing_fetches,
        backing_gets
    );
    assert_eq!(
        metrics.peer_hits - before_metrics.peer_hits,
        u64::from(backing_gets == 0)
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an owned redis-server binary; see MOUNT_RS_CACHE_REDIS_SERVER"]
async fn redis_directory_real_peer_failures_preserve_exact_backing() {
    timeout(BOUND, async {
        let a = PeerId("a".into());
        let b = PeerId("b".into());
        let peers = vec![a.clone(), b.clone()];
        let fallback =
            FixedDiscovery::new_with_mode(peers.clone(), 1, DiscoveryMode::PeerQuery).unwrap();
        let healthy = redis_payload("healthy-directory", &fallback, &b).await;
        let stale = redis_payload("stale-holder", &fallback, &b).await;
        let peer_down = redis_payload("peer-outage", &fallback, &b).await;
        let directory_down = redis_payload("directory-outage", &fallback, &a).await;
        let both_down = redis_payload("combined-outage", &fallback, &a).await;
        let mut redis = RedisFixture::start().await;
        let redis_cleanup = redis.cleanup_completion();
        let redis_address = redis.address;
        let redis_directory = redis.directory.as_ref().unwrap().path().to_owned();
        let discovery =
            RedisDiscovery::new(redis.config(), peers, 1, DiscoveryMode::Directory).unwrap();
        let mut pair = Pair::new(0, 32768);
        pair.counted.diagnostics.store(true, Ordering::SeqCst);
        let backing = Backing::new();
        let healthy_id = backing.seed(&healthy);
        let stale_id = backing.seed(&stale);
        let peer_down_id = backing.seed(&peer_down);
        let directory_down_id = backing.seed(&directory_down);
        let both_down_id = backing.seed(&both_down);
        let runtime = DistributedRuntime::new(
            discovery.clone(),
            pair.counted.clone(),
            b.clone(),
            DistributedConfig {
                max_peer_queries: 1,
                deadline: Duration::from_millis(600),
                ..Default::default()
            },
        )
        .unwrap();
        let store = pair.store(backing.clone(), runtime.clone()).await;
        let local = pair.a_cache.as_ref().unwrap();
        for (id, bytes) in [
            (&healthy_id, &healthy),
            (&peer_down_id, &peer_down),
            (&directory_down_id, &directory_down),
        ] {
            local
                .insert(&scope(), id, bytes, IntegrityPolicy::Sha256Prefixed)
                .unwrap();
        }
        assert!(
            local
                .get(&scope(), &stale_id, IntegrityPolicy::Sha256Prefixed)
                .is_none()
        );
        assert_eq!(pair.b_cache.usage().2, 0, "all requester blocks start cold");
        discovery.heartbeat(&a).await.unwrap();
        for id in [&healthy_id, &stale_id, &peer_down_id] {
            // The one-query fallback selects local B, which the runtime excludes.
            // Only a valid Redis holder+lease can steer these reads to remote A.
            assert_eq!(
                fallback.locate(&scope(), id).await.unwrap(),
                vec![b.clone()]
            );
            discovery.advertise(&scope(), id, &a).await.unwrap();
            assert_eq!(
                discovery.locate(&scope(), id).await.unwrap(),
                vec![a.clone()]
            );
        }
        assert_redis_phase(
            "healthy-directory",
            &pair,
            &store,
            &backing,
            &healthy_id,
            &healthy,
            0,
        )
        .await;
        assert_redis_phase(
            "stale-holder",
            &pair,
            &store,
            &backing,
            &stale_id,
            &stale,
            1,
        )
        .await;
        // Keep the existing replica placement, and observe its result before
        // changing the endpoint state for the next independent fault phase.
        pair.counted
            .observe_put_completion("stale-holder", &stale_id, 0)
            .await;
        // A actually held the outage block before its authenticated endpoint closed.
        assert_eq!(
            pair.a_cache
                .as_ref()
                .unwrap()
                .get(&scope(), &peer_down_id, IntegrityPolicy::Sha256Prefixed)
                .unwrap(),
            peer_down
        );
        pair.stop_a().await;
        redis.ping().await.unwrap();
        assert_eq!(
            discovery.locate(&scope(), &peer_down_id).await.unwrap(),
            vec![a.clone()],
            "owned Redis retains the now-stale holder and unexpired node lease"
        );
        assert_redis_phase(
            "peer-outage",
            &pair,
            &store,
            &backing,
            &peer_down_id,
            &peer_down,
            1,
        )
        .await;
        // A remains stopped until its failed placement handshake releases B's
        // per-peer connection slot, which the next phase's GET also needs.
        pair.counted
            .observe_put_completion("peer-outage", &peer_down_id, 1)
            .await;
        let puts = pair.counted.put_snapshot();
        assert_eq!(
            puts.started, 2,
            "exactly two known backing fills started placement"
        );
        assert_eq!(
            puts.completed, 2,
            "both known placements returned before A restart"
        );
        assert_eq!(puts.errors, 1, "only the stopped-peer placement failed");
        assert_eq!(
            puts.cancelled, 0,
            "no placement was cancelled before A restart"
        );
        assert_eq!(
            puts.inflight, 0,
            "no placement holds a connection slot at A restart"
        );
        redis.stop().await;
        assert!(
            discovery.heartbeat(&a).await.is_err(),
            "owned directory is unavailable"
        );
        pair.restart_a();
        let local = pair.a_cache.as_ref().unwrap();
        assert_eq!(
            local.usage().0,
            0,
            "restarted peer begins with no RAM entries"
        );
        assert!(local.path_for(&scope(), &directory_down_id).exists());
        assert_eq!(
            local
                .get(
                    &scope(),
                    &directory_down_id,
                    IntegrityPolicy::Sha256Prefixed
                )
                .unwrap(),
            directory_down,
            "restarted peer independently verifies full persisted binary bytes"
        );
        assert_eq!(
            local.usage().0,
            0,
            "zero-capacity peer disk verification does not admit RAM bytes"
        );
        let puts = pair.counted.put_snapshot();
        println!(
            "peer_fixture_event=restart_a_disk_verified at_us={} payload_bytes={} ram_bytes=0 \
             peer_put_started={} peer_put_completed={} peer_put_errors={} \
             peer_put_cancelled={} peer_put_inflight={}",
            pair.counted.trace_epoch.elapsed().as_micros(),
            directory_down.len(),
            puts.started,
            puts.completed,
            puts.errors,
            puts.cancelled,
            puts.inflight
        );
        assert_eq!(
            discovery
                .locate(&scope(), &directory_down_id)
                .await
                .unwrap(),
            fallback.locate(&scope(), &directory_down_id).await.unwrap()
        );
        assert_redis_phase(
            "directory-outage-persisted-peer",
            &pair,
            &store,
            &backing,
            &directory_down_id,
            &directory_down,
            0,
        )
        .await;
        pair.stop_a().await;
        assert_eq!(
            discovery.locate(&scope(), &both_down_id).await.unwrap(),
            vec![a]
        );
        assert_redis_phase(
            "combined-outage",
            &pair,
            &store,
            &backing,
            &both_down_id,
            &both_down,
            1,
        )
        .await;
        assert_eq!(backing.reads(), 3);
        assert_eq!(
            backing.bytes.load(Ordering::SeqCst),
            (stale.len() + peer_down.len() + both_down.len()) as u64
        );
        assert_eq!(pair.counted.gets.load(Ordering::SeqCst), 5);
        assert_eq!(store.metrics().snapshot().peer_hits, 2);
        assert_eq!(store.metrics().snapshot().backing_fetches, 3);
        println!(
            "redis-real-peer qualification: logical_reads=5 peer_get_attempts=5 peer_hits=2 \
             memory_backing_get_calls=3 memory_backing_bytes={}; \
             BlockStore call counts are not physical SSD IOPS or provider durability",
            backing.bytes.load(Ordering::SeqCst)
        );
        drop(store);
        runtime.shutdown().await;
        drop(runtime);
        drop(discovery);
        pair.shutdown().await;
        drop(pair);
        drop(redis);
        let reaped = timeout(REDIS_BOUND, redis_cleanup)
            .await
            .expect("bounded explicit Redis cleanup receipt")
            .expect("Redis cleanup was not cancelled before observed reap")
            .expect("owned Redis reap and directory removal succeeded");
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&reaped),
            Some(libc::SIGKILL)
        );
        assert!(
            !redis_directory.exists(),
            "owned Redis directory removed after reap"
        );
        drop(
            std::net::TcpListener::bind(redis_address)
                .expect("owned Redis TCP socket was released"),
        );
    })
    .await
    .expect("bounded owned Redis and authenticated peer qualification");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an owned redis-server binary; see MOUNT_RS_CACHE_REDIS_SERVER"]
async fn cancelled_redis_fixture_reaps_owned_child_before_removing_directory() {
    timeout(BOUND, async {
        let (send, receive) = tokio::sync::oneshot::channel();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            let mut redis = RedisFixture::start().await;
            let completed = redis.cleanup_completion();
            send.send((
                redis.address,
                redis.directory.as_ref().unwrap().path().to_owned(),
                completed,
            ))
            .unwrap();
            std::future::pending::<()>().await;
            drop(redis);
        });
        let (address, directory, completed) = receive.await.unwrap();
        tasks.abort_all();
        assert!(tasks.join_next().await.unwrap().unwrap_err().is_cancelled());
        let reaped = timeout(REDIS_BOUND, completed)
            .await
            .expect("bounded cancellation Redis cleanup receipt")
            .expect("cleanup cancellation cannot be reported as an observed child reap")
            .expect("owned Redis child was reaped and its directory removed");
        assert_eq!(
            std::os::unix::process::ExitStatusExt::signal(&reaped),
            Some(libc::SIGKILL)
        );
        println!("redis_cleanup=observed child_exit_status={reaped}");
        eventually(async || !directory.exists() && std::net::TcpListener::bind(address).is_ok())
            .await;
    })
    .await
    .expect("bounded cancellation cleanup of owned Redis child");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn authenticated_hierarchy_saves_reads_and_reconnects_to_persisted_peer() {
    timeout(BOUND, async {
        let mut pair = Pair::new(8192, 32768);
        let backing = Backing::new();
        let runtime = pair.runtime(false);
        let store = pair.store(backing.clone(), runtime.clone()).await;
        let bytes = b"exact binary\0immutable bytes";
        let id = backing.seed(bytes);
        assert_eq!(store.get(&id).await.unwrap(), bytes);
        assert_eq!(backing.reads(), 1);
        assert_eq!(store.metrics().snapshot().backing_fetches, 1);
        for _ in 0..20 {
            assert_eq!(store.get(&id).await.unwrap(), bytes);
        }
        assert_eq!(backing.reads(), 1);
        assert_eq!(store.metrics().snapshot().local_hits, 20);
        // The requester starts cold for the peer phase.
        pair.b_cache.shutdown().await;
        pair.b_cache.invalidate(&scope(), &id);
        assert_eq!(
            pair.b_cache.usage().2,
            0,
            "requester must be cold after drain"
        );
        pair.a_cache
            .as_ref()
            .unwrap()
            .insert(&scope(), &id, bytes, IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        assert_eq!(store.get(&id).await.unwrap(), bytes);
        assert_eq!(store.metrics().snapshot().peer_hits, 1);
        assert_eq!(backing.reads(), 1);
        pair.b_cache.shutdown().await;
        pair.b_cache.invalidate(&scope(), &id);
        assert_eq!(
            pair.b_cache.usage().2,
            0,
            "requester must be cold after drain"
        );
        let attempts = pair.counted.gets.load(Ordering::SeqCst);
        let peers = store.metrics().snapshot().peer_hits;
        let (release, _) = tokio::sync::watch::channel(false);
        let gate = Arc::new(ResponseGate { arrived: Semaphore::new(0), release });
        *pair.counted.gate.lock().unwrap() = Some(gate.clone());
        let entered = Arc::new(AtomicU64::new(0));
        let mut readers = tokio::task::JoinSet::new();
        for _ in 0..100 {
            let store = store.clone();
            let id = id.clone();
            let entered = entered.clone();
            readers.spawn(async move {
                let mut read = std::pin::pin!(store.get(&id));
                let mut first = true;
                poll_fn(|cx| {
                    let state = read.as_mut().poll(cx);
                    if first {
                        assert!(matches!(state,Poll::Pending),"reader must enter a cold miss while peer response is held");
                        first = false;
                        entered.fetch_add(1,Ordering::SeqCst);
                    }
                    state
                }).await.unwrap()
            });
        }
        // All100 have actually polled the cold path, with128 miss permits, before
        // any peer response can admit bytes. Holding a real response avoids warm-hit
        // launch races; the no-flight-lock mutation must fail the exact-one oracle.
        eventually(async || entered.load(Ordering::SeqCst)==100).await;
        gate.arrived.acquire().await.unwrap().forget();
        assert!(readers.try_join_next().is_none(),"no reader may complete before response release");
        assert_eq!(pair.b_cache.usage().2,0);
        assert_eq!(store.metrics().snapshot().peer_hits,peers);
        assert_eq!(backing.reads(),1);
        assert_eq!(pair.counted.gets.load(Ordering::SeqCst)-attempts,1,"only the miss owner may query the real peer while all100 are cold");
        gate.release.send(true).unwrap();
        while let Some(read) = readers.join_next().await {assert_eq!(read.unwrap(),bytes);}
        *pair.counted.gate.lock().unwrap()=None;
        assert_eq!(pair.counted.gets.load(Ordering::SeqCst) - attempts, 1);
        assert_eq!(store.metrics().snapshot().peer_hits - peers, 1);
        assert_eq!(backing.reads(), 1);
        pair.stop_a().await;
        pair.b_cache.shutdown().await;
        pair.b_cache.invalidate(&scope(), &id);
        assert_eq!(
            pair.b_cache.usage().2,
            0,
            "requester must be cold after drain"
        );
        let attempts = pair.counted.gets.load(Ordering::SeqCst);
        let start = Instant::now();
        assert_eq!(store.get(&id).await.unwrap(), bytes);
        assert_eq!(backing.reads(), 2);
        assert!(pair.counted.gets.load(Ordering::SeqCst) > attempts);
        println!("stopped peer fallback {:?}", start.elapsed());
        pair.restart_a();
        assert_eq!(pair.a_cache.as_ref().unwrap().usage().0, 0);
        assert!(
            pair.a_cache
                .as_ref()
                .unwrap()
                .path_for(&scope(), &id)
                .exists()
        );
        // Direct local disk read and then real peer read both use the persisted bytes.
        assert_eq!(
            pair.a_cache
                .as_ref()
                .unwrap()
                .get(&scope(), &id, IntegrityPolicy::Sha256Prefixed)
                .unwrap(),
            bytes
        );
        pair.b_cache.shutdown().await;
        pair.b_cache.invalidate(&scope(), &id);
        assert_eq!(
            pair.b_cache.usage().2,
            0,
            "requester must be cold after drain"
        );
        let peers = store.metrics().snapshot().peer_hits;
        assert_eq!(store.get(&id).await.unwrap(), bytes);
        assert_eq!(store.metrics().snapshot().peer_hits, peers + 1);
        assert_eq!(backing.reads(), 2);
        let disk_cache = cache(&pair.dir.as_ref().unwrap().path().join("disk-requester"),0,32768);
        pair.track_cache(&disk_cache);
        disk_cache.insert(&scope(),&id,bytes,IntegrityPolicy::Sha256Prefixed).unwrap();
        let disk_store = CachedBlockStore::new(backing.clone(),disk_cache.clone(),scope().identity,IntegrityPolicy::Sha256Prefixed).with_runtime(runtime.clone());
        disk_store.prepare_concurrent_backing().await.unwrap();
        let attempts = pair.counted.gets.load(Ordering::SeqCst);
        assert_eq!(disk_cache.usage().0,0);
        assert_eq!(disk_store.get(&id).await.unwrap(),bytes);
        assert_eq!(disk_store.metrics().snapshot().local_hits,1);
        assert_eq!(pair.counted.gets.load(Ordering::SeqCst),attempts);
        assert_eq!(backing.reads(),2);
        drop(disk_store);disk_cache.shutdown().await;drop(disk_cache);
        println!(
            "hierarchy: 125 logical reads, 2 backing GETs / {} bytes; 3 peer fills; 1 disk-only local hit",
            backing.bytes.load(Ordering::SeqCst)
        );
        drop(store);
        runtime.shutdown().await;
        drop(runtime);
        pair.shutdown().await;
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "isolated process: MOUNT_RS_PROFILE_IO=1, storage/request traces=0"]
async fn cache_lookup_stage_metrics_preserve_bytes_and_cancellation() {
    use mount_rs_core::diagnostics::{profile, storage};
    use stage_metrics::*;
    fn runtime(pair: &Pair, permits: usize) -> Arc<DistributedRuntime> {
        DistributedRuntime::new(
            Arc::new(Hint { placement: false }),
            pair.counted.clone(),
            PeerId("b".into()),
            DistributedConfig {
                max_inflight_misses: permits,
                deadline: Duration::from_millis(600),
                hedge_delay: Duration::from_millis(300),
                ..Default::default()
            },
        )
        .unwrap()
    }
    timeout(BOUND, async {
        assert!(
            storage::enabled() && profile::enabled(),
            "parent must isolate the enabled profiler"
        );
        let mut checks = Checks::default();
        let mut pair = Pair::new(8192, 32768);
        let cleanup = pair.cleanup_completion();
        let backing = Backing::new();
        let bytes = b"cache stages\0\xffcomplete binary bytes";
        let id = backing.seed(bytes);
        pair.a_cache
            .as_ref()
            .unwrap()
            .insert(&scope(), &id, bytes, IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        let rt = runtime(&pair, 128);
        let store = pair.store(backing.clone(), rt.clone()).await;
        let (release, _) = tokio::sync::watch::channel(false);
        let gate = Arc::new(ResponseGate {
            arrived: Semaphore::new(0),
            release,
        });
        *pair.counted.gate.lock().unwrap() = Some(gate.clone());
        let entered = Arc::new(AtomicU64::new(0));
        let before = storage::snapshot();
        let hits = profile::snapshot();
        let mut readers = tokio::task::JoinSet::new();
        for _ in 0..100 {
            let store = store.clone();
            let id = id.clone();
            let entered = entered.clone();
            readers.spawn(async move {
                let mut read = std::pin::pin!(store.get(&id));
                let mut first = true;
                poll_fn(|cx| {
                    let state = read.as_mut().poll(cx);
                    if first {
                        assert!(state.is_pending(), "every cold reader first polls Pending");
                        first = false;
                        entered.fetch_add(1, Ordering::SeqCst);
                    }
                    state
                })
                .await
                .unwrap()
            });
        }
        eventually(async || entered.load(Ordering::SeqCst) == 100).await;
        gate.arrived.acquire().await.unwrap().forget();
        assert!(readers.try_join_next().is_none());
        assert_eq!(pair.b_cache.usage().2, 0);
        assert_eq!(pair.counted.gets.load(Ordering::SeqCst), 1);
        assert_eq!(backing.reads(), 0);
        checks.pending(
            "cold100 held response",
            99,
            vec![(FLIGHT, 99), (ADMISSION, 0)],
        );
        gate.release.send(true).unwrap();
        while let Some(read) = readers.join_next().await {
            assert_eq!(read.unwrap(), bytes);
        }
        *pair.counted.gate.lock().unwrap() = None;
        assert_eq!(pair.counted.gets.load(Ordering::SeqCst), 1);
        assert_eq!(backing.reads(), 0);
        assert_eq!(
            pair.counted.put_snapshot().started,
            0,
            "read fixture has no replica placement"
        );
        checks.phase(
            "cold100",
            &before,
            vec![
                Expected(ADMISSION, 100, 0, 0, 0),
                Expected(FLIGHT, 100, 0, 0, 0),
                Expected(RAM, 200, 0, 0, 99 * bytes.len() as u64),
                Expected(DISK, 1, 0, 0, 0),
                Expected(LOCK, 1, 0, 0, 0),
                Expected(ESTABLISH, 1, 0, 0, 0),
            ],
        );
        checks.hit("cold100", &hits, RAM_HIT, 99, 99 * bytes.len() as u64);
        checks.hit("cold100", &hits, DISK_HIT, 0, 0);
        let before = storage::snapshot();
        let hits = profile::snapshot();
        for _ in 0..20 {
            assert_eq!(store.get(&id).await.unwrap(), bytes);
        }
        assert_eq!(
            (backing.reads(), pair.counted.gets.load(Ordering::SeqCst)),
            (0, 1)
        );
        checks.phase(
            "warm RAM20",
            &before,
            vec![
                Expected(RAM, 20, 0, 0, 20 * bytes.len() as u64),
                Expected(DISK, 0, 0, 0, 0),
                Expected(ADMISSION, 0, 0, 0, 0),
                Expected(FLIGHT, 0, 0, 0, 0),
                Expected(LOCK, 0, 0, 0, 0),
                Expected(ESTABLISH, 0, 0, 0, 0),
            ],
        );
        checks.hit("warm RAM20", &hits, RAM_HIT, 20, 20 * bytes.len() as u64);
        let empty = backing.seed(b"");
        pair.b_cache
            .insert(&scope(), &empty, b"", IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        let before = storage::snapshot();
        let hits = profile::snapshot();
        assert_eq!(store.get(&empty).await.unwrap(), b"");
        assert_eq!(
            (backing.reads(), pair.counted.gets.load(Ordering::SeqCst)),
            (0, 1)
        );
        checks.phase(
            "empty RAM hit",
            &before,
            vec![Expected(RAM, 1, 0, 0, 0), Expected(DISK, 0, 0, 0, 0)],
        );
        checks.hit("empty RAM hit", &hits, RAM_HIT, 1, 0);
        drop(store);
        rt.shutdown().await;
        drop(rt);

        for (phase, permits, same_key) in [
            ("admission cancellation", 1, false),
            ("singleflight cancellation", 128, true),
        ] {
            let leader_bytes = if same_key {
                b"flight\0leader".as_slice()
            } else {
                b"admission\0leader".as_slice()
            };
            let leader_id = backing.seed(leader_bytes);
            let follower_id = if same_key {
                leader_id.clone()
            } else {
                backing.seed(b"different\0follower")
            };
            pair.a_cache
                .as_ref()
                .unwrap()
                .insert(
                    &scope(),
                    &leader_id,
                    leader_bytes,
                    IntegrityPolicy::Sha256Prefixed,
                )
                .unwrap();
            let rt = runtime(&pair, permits);
            let store = pair.store(backing.clone(), rt.clone()).await;
            let (release, _) = tokio::sync::watch::channel(false);
            let gate = Arc::new(ResponseGate {
                arrived: Semaphore::new(0),
                release,
            });
            *pair.counted.gate.lock().unwrap() = Some(gate.clone());
            let attempts = pair.counted.gets.load(Ordering::SeqCst);
            let before = storage::snapshot();
            let leader_store = store.clone();
            let read_id = leader_id.clone();
            let leader = tokio::spawn(async move { leader_store.get(&read_id).await.unwrap() });
            gate.arrived.acquire().await.unwrap().forget();
            let mut follower = store.get(&follower_id);
            poll_fn(|cx| {
                assert!(follower.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
            checks.pending(
                phase,
                1,
                vec![
                    (if same_key { FLIGHT } else { ADMISSION }, 1),
                    (if same_key { ADMISSION } else { FLIGHT }, 0),
                ],
            );
            drop(follower);
            assert_eq!(pair.counted.gets.load(Ordering::SeqCst) - attempts, 1);
            assert_eq!(backing.reads(), 0);
            gate.release.send(true).unwrap();
            assert_eq!(leader.await.unwrap(), leader_bytes);
            *pair.counted.gate.lock().unwrap() = None;
            assert_eq!(pair.counted.gets.load(Ordering::SeqCst) - attempts, 1);
            assert_eq!(backing.reads(), 0);
            checks.phase(
                phase,
                &before,
                vec![
                    Expected(
                        ADMISSION,
                        if same_key { 2 } else { 1 },
                        0,
                        u64::from(!same_key),
                        0,
                    ),
                    Expected(FLIGHT, 1, 0, u64::from(same_key), 0),
                    Expected(RAM, 3, 0, 0, 0),
                    Expected(DISK, 1, 0, 0, 0),
                    Expected(LOCK, 1, 0, 0, 0),
                    Expected(ESTABLISH, 0, 0, 0, 0),
                ],
            );
            drop(store);
            rt.shutdown().await;
            drop(rt);
        }

        let disk = cache(
            &pair.dir.as_ref().unwrap().path().join("metrics-disk"),
            0,
            32768,
        );
        pair.track_cache(&disk);
        disk.insert(&scope(), &id, bytes, IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        let rt = runtime(&pair, 128);
        let store = Arc::new(
            CachedBlockStore::new(
                backing.clone(),
                disk.clone(),
                scope().identity,
                IntegrityPolicy::Sha256Prefixed,
            )
            .with_runtime(rt.clone()),
        );
        store.prepare_concurrent_backing().await.unwrap();
        let attempts = pair.counted.gets.load(Ordering::SeqCst);
        let before = storage::snapshot();
        let hits = profile::snapshot();
        assert_eq!(disk.usage().0, 0);
        assert_eq!(store.get(&id).await.unwrap(), bytes);
        assert_eq!(disk.usage().0, 0);
        assert_eq!(
            (backing.reads(), pair.counted.gets.load(Ordering::SeqCst)),
            (0, attempts)
        );
        checks.phase(
            "disk-only hit",
            &before,
            vec![
                Expected(RAM, 2, 0, 0, 0),
                Expected(DISK, 1, 0, 0, bytes.len() as u64),
                Expected(ADMISSION, 1, 0, 0, 0),
                Expected(FLIGHT, 1, 0, 0, 0),
                Expected(LOCK, 0, 0, 0, 0),
            ],
        );
        checks.hit("disk-only hit", &hits, DISK_HIT, 1, bytes.len() as u64);
        checks.hit("disk-only hit", &hits, RAM_HIT, 0, 0);
        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(disk.io_permit().await.unwrap());
        }
        let before = storage::snapshot();
        let mut read = store.get(&id);
        poll_fn(|cx| {
            assert!(read.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        checks.pending("disk permit cancellation", 1, vec![(DISK, 1)]);
        drop(read);
        drop(held);
        assert_eq!(
            (backing.reads(), pair.counted.gets.load(Ordering::SeqCst)),
            (0, attempts)
        );
        checks.phase(
            "disk permit cancellation",
            &before,
            vec![
                Expected(DISK, 0, 0, 1, 0),
                Expected(RAM, 2, 0, 0, 0),
                Expected(ADMISSION, 1, 0, 0, 0),
                Expected(FLIGHT, 1, 0, 0, 0),
            ],
        );
        assert_eq!(
            store.get(&id).await.unwrap(),
            bytes,
            "disk still readable after cancellation"
        );
        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(disk.io_permit().await.unwrap());
        }
        let before = storage::snapshot();
        let hits = profile::snapshot();
        assert_eq!(
            store.get(&id).await.unwrap(),
            bytes,
            "existing disk deadline falls through to full healthy peer bytes"
        );
        assert_eq!(backing.reads(), 0);
        assert_eq!(pair.counted.gets.load(Ordering::SeqCst) - attempts, 1);
        drop(held);
        checks.phase(
            "disk deadline peer fallback",
            &before,
            vec![
                Expected(DISK, 0, 1, 0, 0),
                Expected(RAM, 2, 0, 0, 0),
                Expected(ADMISSION, 1, 0, 0, 0),
                Expected(FLIGHT, 1, 0, 0, 0),
                Expected(LOCK, 1, 0, 0, 0),
                Expected(ESTABLISH, 0, 0, 0, 0),
            ],
        );
        checks.hit("disk deadline peer fallback", &hits, DISK_HIT, 0, 0);
        checks.hit("disk deadline peer fallback", &hits, RAM_HIT, 0, 0);
        let attempts = pair.counted.gets.load(Ordering::SeqCst);
        drop(store);
        rt.shutdown().await;
        drop(rt);
        disk.shutdown().await;
        drop(disk);

        let path = pair.dir.as_ref().unwrap().path().join("metrics-empty-disk");
        let seed = cache(&path, 0, 32768);
        pair.track_cache(&seed);
        seed.insert(&scope(), &empty, b"", IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        seed.shutdown().await;
        drop(seed);
        let disk = cache(&path, 0, 32768);
        pair.track_cache(&disk);
        let store = CachedBlockStore::new(
            backing.clone(),
            disk.clone(),
            scope().identity,
            IntegrityPolicy::Sha256Prefixed,
        );
        store.prepare_concurrent_backing().await.unwrap();
        assert!(
            disk.get_memory(&scope(), &empty, IntegrityPolicy::Sha256Prefixed)
                .is_none(),
            "reopened empty block starts disk-only"
        );
        let before = storage::snapshot();
        let hits = profile::snapshot();
        assert_eq!(store.get(&empty).await.unwrap(), b"");
        assert_eq!(
            (backing.reads(), pair.counted.gets.load(Ordering::SeqCst)),
            (0, attempts)
        );
        checks.phase(
            "empty disk hit",
            &before,
            vec![Expected(RAM, 2, 0, 0, 0), Expected(DISK, 1, 0, 0, 0)],
        );
        checks.hit("empty disk hit", &hits, DISK_HIT, 1, 0);
        checks.hit("empty disk hit", &hits, RAM_HIT, 0, 0);
        drop(store);
        disk.shutdown().await;
        drop(disk);
        pair.shutdown().await;
        drop(pair);
        timeout(CLEANUP_BOUND, cleanup)
            .await
            .expect("bounded cache owner cleanup")
            .expect("cleanup observed")
            .expect("all cache owners released");
        println!("cache_stage_behavior_oracles=complete coalesced_readers=100 backing_get_calls=0");
        checks.verify();
    })
    .await
    .expect("bounded isolated cache stage qualification");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupt_disk_capacity_eviction_and_stale_hint_fall_back_to_exact_backing() {
    timeout(BOUND, async {
        let mut pair = Pair::new(0, 700);
        let backing = Backing::new();
        let runtime = pair.runtime(false);
        let store = pair.store(backing.clone(), runtime.clone()).await;
        let bytes = vec![7; 256];
        let id = backing.seed(&bytes);
        let local = pair.a_cache.as_ref().unwrap();
        local
            .insert(&scope(), &id, &bytes, IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        let path = local.path_for(&scope(), &id);
        let mut corrupted = std::fs::read(&path).unwrap();
        *corrupted.last_mut().unwrap() ^= 1;
        std::fs::write(&path, corrupted).unwrap();
        assert_eq!(local.usage().0, 0);
        assert_eq!(store.get(&id).await.unwrap(), bytes);
        assert_eq!(backing.reads(), 1);
        assert_eq!(store.metrics().snapshot().peer_hits, 0);
        assert_eq!(pair.counted.gets.load(Ordering::SeqCst), 1);
        local
            .insert(&scope(), &id, &bytes, IntegrityPolicy::Sha256Prefixed)
            .unwrap();
        for byte in 8..16 {
            let payload = vec![byte; 256];
            let new = backing.seed(&payload);
            local
                .insert(&scope(), &new, &payload, IntegrityPolicy::Sha256Prefixed)
                .unwrap();
            let usage = local.usage();
            assert_eq!(usage.0, 0);
            assert!(usage.1 <= 700);
            assert!(usage.2 <= 128);
        }
        assert!(!path.exists(), "old entry must actually be evicted");
        pair.b_cache.shutdown().await;
        pair.b_cache.invalidate(&scope(), &id);
        assert_eq!(
            pair.b_cache.usage().2,
            0,
            "requester must be cold after drain"
        );
        assert_eq!(store.get(&id).await.unwrap(), bytes);
        assert_eq!(backing.reads(), 2);
        let missing = backing.seed(b"stale hint never held this block");
        let attempts = pair.counted.gets.load(Ordering::SeqCst);
        assert_eq!(
            store.get(&missing).await.unwrap(),
            b"stale hint never held this block"
        );
        assert_eq!(backing.reads(), 3);
        assert_eq!(pair.counted.gets.load(Ordering::SeqCst), attempts + 1);
        drop(store);
        runtime.shutdown().await;
        drop(runtime);
        pair.shutdown().await;
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn exact_registered_scope_isolation_precedes_peer_lookup_and_put() {
    timeout(BOUND, async {
        let mut pair = Pair::new(0, 32768);
        let id = BlockId("opaque-identical-id".into());
        let mut base = scope();
        base.identity.drive = "one".into();
        let mut sibling = base.clone();
        sibling.identity.drive = "two".into();
        let mut other_backing = base.clone();
        other_backing.backing = ConcurrentBackingId::from_bytes([2; 16]).unwrap();
        let mut other_cluster = base.clone();
        other_cluster.identity.cluster = "other".into();
        let mut forbidden = base.clone();
        forbidden.identity.partition = "forbidden".into();
        let local = pair.a_cache.as_ref().unwrap();
        for (s, bytes) in [
            (&base, b"one".as_slice()),
            (&sibling, b"two"),
            (&other_backing, b"backing"),
            (&other_cluster, b"cluster"),
            (&forbidden, b"partition"),
        ] {
            local
                .register_scope(s.clone(), IntegrityPolicy::Opaque)
                .unwrap();
            local
                .insert(s, &id, bytes, IntegrityPolicy::Opaque)
                .unwrap();
        }
        let peer = PeerId("a".into());
        for (s, bytes) in [
            (&base, b"one".as_slice()),
            (&sibling, b"two"),
            (&other_backing, b"backing"),
            (&other_cluster, b"cluster"),
        ] {
            assert_eq!(
                pair.b.get(&peer, s, &id).await.unwrap(),
                Some(bytes.to_vec())
            );
        }
        let mut unregistered = base.clone();
        unregistered.identity.drive = "unregistered-sibling".into();
        let before = local.usage();
        for denied in [&unregistered, &forbidden] {
            assert!(pair.b.get(&peer, denied, &id).await.is_err());
            assert!(
                pair.b
                    .put(
                        &peer,
                        denied,
                        &BlockId("denied-put".into()),
                        b"unauthorized"
                    )
                    .await
                    .is_err()
            );
            assert!(
                !local
                    .path_for(denied, &BlockId("denied-put".into()))
                    .exists()
            );
        }
        assert_eq!(local.usage(), before);
        pair.shutdown().await;
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn real_peer_placement_waits_for_successful_backing_flush() {
    timeout(BOUND, async {
        for fail in [false, true] {
            let mut pair = Pair::new(0, 32768);
            let backing = Backing::new();
            backing.gate.store(true, Ordering::SeqCst);
            backing.fail.store(fail, Ordering::SeqCst);
            let runtime = pair.runtime(true);
            let store = pair.store(backing.clone(), runtime.clone()).await;
            let bytes = b"acknowledge only committed bytes";
            let id = store.put(bytes).await.unwrap();
            assert_eq!(store.get(&id).await.unwrap(), bytes);
            assert_eq!(pair.b_cache.usage().2, 0);
            assert!(
                pair.b
                    .get(&PeerId("a".into()), &scope(), &id)
                    .await
                    .unwrap()
                    .is_none()
            );
            let flushing = store.clone();
            let mut flushes = tokio::task::JoinSet::new();
            flushes.spawn(async move { flushing.flush().await });
            backing.entered.acquire().await.unwrap().forget();
            assert!(flushes.try_join_next().is_none());
            assert!(!backing.committed.lock().unwrap().contains_key(&id));
            assert!(
                pair.b
                    .get(&PeerId("a".into()), &scope(), &id)
                    .await
                    .unwrap()
                    .is_none()
            );
            backing.release.add_permits(1);
            let result = flushes.join_next().await.unwrap().unwrap();
            assert_eq!(result.is_err(), fail);
            if fail {
                assert_eq!(pair.b_cache.usage().2, 0);
                assert!(
                    pair.b
                        .get(&PeerId("a".into()), &scope(), &id)
                        .await
                        .unwrap()
                        .is_none()
                );
                assert!(!backing.committed.lock().unwrap().contains_key(&id));
            } else {
                eventually(async || {
                    pair.b
                        .get(&PeerId("a".into()), &scope(), &id)
                        .await
                        .unwrap()
                        == Some(bytes.to_vec())
                })
                .await;
                assert_eq!(backing.committed.lock().unwrap().get(&id).unwrap(), bytes);
            }
            drop(store);
            runtime.shutdown().await;
            drop(runtime);
            pair.shutdown().await;
        }
    })
    .await
    .unwrap();
}
struct SqliteDecorator {
    cache: Arc<LocalCache>,
    runtime: Arc<DistributedRuntime>,
    backing: Mutex<Option<Arc<dyn BlockStore>>>,
}
impl mount_rs_sdk::BlockStoreDecorator for SqliteDecorator {
    fn decorate(
        &self,
        _: &mount_rs_sdk::StoreConfig,
        backing: Arc<dyn BlockStore>,
    ) -> Result<Arc<dyn BlockStore>> {
        *self.backing.lock().unwrap() = Some(backing.clone());
        Ok(Arc::new(
            CachedBlockStore::new(
                backing,
                self.cache.clone(),
                scope().identity,
                IntegrityPolicy::Opaque,
            )
            .with_runtime(self.runtime.clone()),
        ))
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_sdk_acknowledgment_survives_fresh_undecorated_reopen() {
    timeout(BOUND, async {
        let mut pair = Pair::new(0, 32768);
        let runtime = pair.runtime(true);
        let decorator = SqliteDecorator {
            cache: pair.b_cache.clone(),
            runtime: runtime.clone(),
            backing: Mutex::new(None),
        };
        let dir = tempfile::tempdir().unwrap();
        let mut options =
            mount_rs_sdk::SplitOptions::memory("writer", 4096).with_concurrent_writes(true);
        options.metadata = mount_rs_sdk::StoreConfig::Sqlite {
            path: dir.path().join("metadata.sqlite"),
        };
        options.blocks = mount_rs_sdk::StoreConfig::Sqlite {
            path: dir.path().join("blocks.sqlite"),
        };
        let first =
            mount_rs_sdk::Filesystem::split_with_block_decorator(options.clone(), &decorator)
                .await
                .unwrap();
        let underlying = decorator.backing.lock().unwrap().as_ref().unwrap().clone();
        let mut actual_scope = scope();
        actual_scope.backing = underlying.prepare_concurrent_backing().await.unwrap();
        pair.a_cache
            .as_ref()
            .unwrap()
            .register_scope(actual_scope, IntegrityPolicy::Opaque)
            .unwrap();
        drop(underlying);
        let bytes: Vec<u8> = (0..3072).map(|i| (i % 251) as u8).collect();
        let view = mount_rs_core::Loopback::from_arc(first.driver());
        view.write_file("/acknowledged", &bytes).await.unwrap();
        assert_eq!(view.read_file("/acknowledged").await.unwrap(), bytes);
        eventually(async || pair.a_cache.as_ref().unwrap().usage().2 > 0).await;
        let completed = pair.cleanup_completion();
        first.shutdown().await.unwrap();
        drop(view);
        drop(first);
        drop(decorator);
        runtime.shutdown().await;
        drop(runtime);
        pair.shutdown().await;
        drop(pair);
        timeout(CLEANUP_BOUND, completed)
            .await
            .expect("bounded peer/cache cleanup before fresh SQLite reopen")
            .expect("cleanup was not cancelled before observing all cache owners gone")
            .expect("endpoint shutdown and every tracked cache owner were observed released");
        // Observed cleanup released every cache/peer owner; reopen only persisted SQLite.
        options.owner = "fresh-undecorated-reader".into();
        let fresh = mount_rs_sdk::Filesystem::split(options).await.unwrap();
        let view = mount_rs_core::Loopback::from_arc(fresh.driver());
        assert_eq!(view.read_file("/acknowledged").await.unwrap(), bytes);
        fresh.shutdown().await.unwrap();
        drop(view);
        drop(fresh);
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_fixture_releases_authenticated_sockets_and_owned_directory() {
    timeout(BOUND, async {
        let (send, receive) = tokio::sync::oneshot::channel();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn(async move {
            let mut pair = Pair::new(0, 32768);
            let completed = pair.cleanup_completion();
            assert!(
                pair.b
                    .get(&PeerId("a".into()), &scope(), &BlockId("missing".into()))
                    .await
                    .unwrap()
                    .is_none()
            );
            send.send((
                pair.address,
                pair.b.local_addr().unwrap(),
                pair.dir.as_ref().unwrap().path().to_owned(),
                completed,
            ))
            .unwrap();
            std::future::pending::<()>().await;
            drop(pair);
        });
        let (a, b, directory, completed) = receive.await.unwrap();
        tasks.abort_all();
        assert!(tasks.join_next().await.unwrap().unwrap_err().is_cancelled());
        timeout(CLEANUP_BOUND, completed)
            .await
            .expect("bounded cancellation peer/cache cleanup receipt")
            .expect("cleanup cancellation cannot be reported as an observed cache drain")
            .expect("endpoint shutdown and all tracked cache owners were observed");
        // Quinn closes its driver asynchronously after the last endpoint owner drops.
        // Observe directory removal and actual socket reuse together.
        eventually(async || {
            !directory.exists()
                && std::net::UdpSocket::bind(a).is_ok()
                && std::net::UdpSocket::bind(b).is_ok()
        })
        .await;
    })
    .await
    .unwrap();
}
