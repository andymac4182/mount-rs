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
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::Poll,
    time::Duration,
};
use tokio::{
    sync::Semaphore,
    time::{Instant, timeout},
};
const BOUND: Duration = Duration::from_secs(15);
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
struct CountPeer {
    peer: Arc<QuicPeerTransport>,
    gets: AtomicU64,
    gate: Mutex<Option<Arc<ResponseGate>>>,
}
#[async_trait]
impl PeerTransport for CountPeer {
    async fn get(&self, p: &PeerId, s: &CacheScope, id: &BlockId) -> Result<Option<Vec<u8>>> {
        self.gets.fetch_add(1, Ordering::SeqCst);
        let result = self.peer.get(p, s, id).await;
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
        self.peer.put(p, s, id, b).await
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
        });
        Self {
            cleaned: false,
            dir: Some(dir),
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
        self.a_cache = Some(local);
        self.a = Some(peer);
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
// Async cleanup also runs on unwinding/outer timeout: close endpoints, abort runtime workers,
// and retain directories until endpoint/cache work has drained. A runtime owns no fixture refs.
impl Drop for Pair {
    fn drop(&mut self) {
        if self.cleaned {
            return;
        }
        let dir = self.dir.take();
        let a = self.a.take();
        let ac = self.a_cache.take();
        let b = self.b.clone();
        let bc = self.b_cache.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                if let Some(a) = a {
                    a.shutdown().await;
                }
                b.shutdown().await;
                if let Some(ac) = ac {
                    ac.shutdown().await;
                }
                bc.shutdown().await;
                drop(b);
                drop(bc);
                drop(dir);
            });
        }
    }
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
        first.shutdown().await.unwrap();
        drop(view);
        drop(first);
        drop(decorator);
        runtime.shutdown().await;
        drop(runtime);
        pair.shutdown().await;
        drop(pair);
        // Every decorator/cache/peer owner is gone. The fresh SDK uses only persisted SQLite.
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
            let pair = Pair::new(0, 32768);
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
            ))
            .unwrap();
            std::future::pending::<()>().await;
            drop(pair);
        });
        let (a, b, directory) = receive.await.unwrap();
        tasks.abort_all();
        assert!(tasks.join_next().await.unwrap().unwrap_err().is_cancelled());
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
