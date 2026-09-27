//! Isolated component observations of discovery within public cache reads.
#![cfg(unix)]

use async_trait::async_trait;
use mount_rs_blob_cache::*;
use mount_rs_core::{
    Result,
    diagnostics::storage,
    error::{ErrorCode, FsError},
    storage::{BlockId, BlockStore, ConcurrentBackingId},
};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Semaphore, time::timeout};

#[path = "support/stage_metrics.rs"]
#[allow(dead_code)]
mod stage_metrics;
use stage_metrics::{Checks, Expected};

// Keep the literal independent of the enum so the old bank compiles and all
// behavior and ownership checks complete before a missing producer is reported.
const LOCATE: &str = "blob_cache.discovery.locate";
const BOUND: Duration = Duration::from_secs(15);
const CLEANUP_BOUND: Duration = Duration::from_secs(3);
const PEER_DEADLINE: Duration = Duration::from_millis(600);

fn io_error() -> FsError {
    FsError::new(ErrorCode::Eio)
}

fn scope() -> CacheScope {
    CacheScope {
        identity: ScopeIdentity {
            cluster: "discovery-metrics".into(),
            partition: "p".into(),
            drive: "d".into(),
        },
        backing: ConcurrentBackingId::from_bytes([9; 16]).unwrap(),
    }
}

fn payload() -> Vec<u8> {
    (0..4096).map(|index| (index % 256) as u8).collect()
}

fn block(bytes: &[u8]) -> BlockId {
    BlockId(format!("b{:x}", Sha256::digest(bytes)))
}

#[derive(Default)]
struct ReadCounts {
    calls: AtomicU64,
    bytes: AtomicU64,
}

impl ReadCounts {
    fn record(&self, bytes: usize) {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.bytes.fetch_add(bytes as u64, Ordering::SeqCst);
    }

    fn assert(&self, calls: u64, bytes: usize) {
        assert_eq!(self.calls.load(Ordering::SeqCst), calls);
        assert_eq!(self.bytes.load(Ordering::SeqCst), bytes as u64);
    }
}

struct Backing {
    id: BlockId,
    payload: Vec<u8>,
    reads: ReadCounts,
}

#[async_trait]
impl BlockStore for Backing {
    fn durable(&self) -> bool {
        false
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

    async fn put(&self, _bytes: &[u8]) -> Result<BlockId> {
        Err(io_error())
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        assert_eq!(id, &self.id, "exact backing block identity");
        self.reads.record(self.payload.len());
        Ok(self.payload.clone())
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, _id: &BlockId) -> Result<()> {
        Err(io_error())
    }
}

struct Peer {
    id: BlockId,
    payload: Vec<u8>,
    reads: ReadCounts,
}

#[async_trait]
impl PeerTransport for Peer {
    async fn get(
        &self,
        peer: &PeerId,
        found_scope: &CacheScope,
        id: &BlockId,
    ) -> Result<Option<Vec<u8>>> {
        assert_eq!(peer, &PeerId("holder".into()), "bounded nonlocal holder");
        assert_eq!(found_scope, &scope(), "exact peer cache scope");
        assert_eq!(id, &self.id, "exact peer block identity");
        self.reads.record(self.payload.len());
        Ok(Some(self.payload.clone()))
    }

    async fn put(
        &self,
        _peer: &PeerId,
        _scope: &CacheScope,
        _id: &BlockId,
        _bytes: &[u8],
    ) -> Result<()> {
        panic!("empty placement must never issue a peer put");
    }
}

#[derive(Clone, Copy)]
enum LookupOutcome {
    Peers,
    Empty,
    Error,
}

struct DiscoveryControl {
    id: BlockId,
    outcome: LookupOutcome,
    held: AtomicBool,
    calls: AtomicU64,
    active: AtomicU64,
    entered: Semaphore,
    release: Semaphore,
}

struct ActiveLookup<'a>(&'a AtomicU64);

impl Drop for ActiveLookup<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl Discovery for DiscoveryControl {
    async fn locate(&self, found_scope: &CacheScope, id: &BlockId) -> Result<Vec<PeerId>> {
        assert_eq!(found_scope, &scope(), "exact discovery cache scope");
        assert_eq!(id, &self.id, "exact discovery block identity");
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.active.fetch_add(1, Ordering::SeqCst);
        let _active = ActiveLookup(&self.active);
        self.entered.add_permits(1);
        if self.held.load(Ordering::SeqCst) {
            self.release.acquire().await.unwrap().forget();
        }
        match self.outcome {
            LookupOutcome::Peers => Ok(["local", "holder", "holder", "unused"]
                .into_iter()
                .map(|name| PeerId(name.into()))
                .collect()),
            LookupOutcome::Empty => Ok(Vec::new()),
            LookupOutcome::Error => Err(io_error()),
        }
    }

    fn placement(&self, _scope: &CacheScope, _id: &BlockId) -> Vec<PeerId> {
        Vec::new()
    }

    async fn advertise(&self, _scope: &CacheScope, _id: &BlockId, _peer: &PeerId) -> Result<()> {
        Ok(())
    }

    async fn withdraw(&self, _scope: &CacheScope, _id: &BlockId, _peer: &PeerId) -> Result<()> {
        Ok(())
    }

    async fn heartbeat(&self, _peer: &PeerId) -> Result<()> {
        Ok(())
    }
}

struct Fixture {
    store: CachedBlockStore,
    cache: Arc<LocalCache>,
    runtime: Arc<DistributedRuntime>,
    discovery: Arc<DiscoveryControl>,
    peer: Arc<Peer>,
    backing: Arc<Backing>,
    directory: PathBuf,
    id: BlockId,
    payload: Vec<u8>,
}

impl Fixture {
    async fn new(outcome: LookupOutcome) -> Self {
        let payload = payload();
        let id = block(&payload);
        // Retain before constructing any asynchronous owner. A panic or outer
        // timeout must not remove a directory still used by a blocking worker.
        let directory = tempfile::Builder::new()
            .prefix("mount-rs-discovery-metrics-")
            .tempdir()
            .unwrap()
            .keep();
        let cache = LocalCache::new(LocalCacheConfig {
            directory: directory.clone(),
            memory_bytes: 8192,
            disk_bytes: 0,
            max_entries: 16,
            max_blob_bytes: payload.len(),
        })
        .unwrap();
        let discovery = Arc::new(DiscoveryControl {
            id: id.clone(),
            outcome,
            held: AtomicBool::new(true),
            calls: AtomicU64::new(0),
            active: AtomicU64::new(0),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        });
        let peer = Arc::new(Peer {
            id: id.clone(),
            payload: payload.clone(),
            reads: ReadCounts::default(),
        });
        let backing = Arc::new(Backing {
            id: id.clone(),
            payload: payload.clone(),
            reads: ReadCounts::default(),
        });
        let runtime = DistributedRuntime::new(
            discovery.clone(),
            peer.clone(),
            PeerId("local".into()),
            DistributedConfig {
                max_peer_queries: 1,
                max_inflight_misses: 1,
                deadline: PEER_DEADLINE,
                hedge_delay: Duration::from_millis(300),
                ..Default::default()
            },
        )
        .unwrap();
        let store = CachedBlockStore::new(
            backing.clone(),
            cache.clone(),
            scope().identity,
            IntegrityPolicy::Sha256Prefixed,
        )
        .with_runtime(runtime.clone());
        assert_eq!(
            store.prepare_concurrent_backing().await.unwrap(),
            scope().backing
        );
        Self {
            store,
            cache,
            runtime,
            discovery,
            peer,
            backing,
            directory,
            id,
            payload,
        }
    }

    fn assert_reads(&self, discovery: u64, peer: u64, backing: u64) {
        assert_eq!(self.discovery.calls.load(Ordering::SeqCst), discovery);
        assert_eq!(self.discovery.active.load(Ordering::SeqCst), 0);
        self.peer
            .reads
            .assert(peer, peer as usize * self.payload.len());
        self.backing
            .reads
            .assert(backing, backing as usize * self.payload.len());
    }

    async fn cleanup(self) {
        let Self {
            store,
            cache,
            runtime,
            directory,
            ..
        } = self;
        drop(store);
        runtime.shutdown().await;
        drop(runtime);
        // No producer remains. Holding every I/O permit proves the blocking
        // workers drained; LocalCache::shutdown alone ignores its timeout.
        let permits = timeout(CLEANUP_BOUND, async {
            let mut permits = Vec::new();
            for _ in 0..8 {
                permits.push(cache.io_permit().await.unwrap());
            }
            permits
        })
        .await
        .expect("cache workers drain before directory removal; retain on doubt");
        let weak = Arc::downgrade(&cache);
        drop(cache);
        assert!(
            weak.upgrade().is_none(),
            "retain cache directory if an owner remains"
        );
        drop(permits);
        std::fs::remove_dir_all(&directory).unwrap();
        assert!(
            !directory.exists(),
            "owned component cache directory removed"
        );
    }
}

async fn wait_until_locate<F>(discovery: &DiscoveryControl, read: &mut F)
where
    F: Future<Output = Result<Vec<u8>>> + Unpin,
{
    tokio::select! {
        biased;
        entered = discovery.entered.acquire() => entered.unwrap().forget(),
        result = read => panic!("held discovery must remain pending: {result:?}"),
    }
    assert_eq!(discovery.active.load(Ordering::SeqCst), 1);
}

#[tokio::test]
#[ignore = "isolated process: MOUNT_RS_PROFILE_IO=1, storage/request traces=0"]
async fn discovery_locate_metrics_preserve_bytes_outcomes_and_cancellation() {
    timeout(BOUND, async {
        assert!(
            storage::enabled(),
            "parent must isolate the enabled profiler"
        );
        let mut checks = Checks::default();
        for (name, outcome, peer, backing, success, error) in [
            ("nonempty", LookupOutcome::Peers, 1, 0, 1, 0),
            ("empty", LookupOutcome::Empty, 0, 1, 1, 0),
            ("error", LookupOutcome::Error, 0, 1, 0, 1),
        ] {
            let fixture = Fixture::new(outcome).await;
            let before = storage::snapshot();
            let mut read = fixture.store.get(&fixture.id);
            wait_until_locate(&fixture.discovery, &mut read).await;
            checks.pending(name, 1, vec![(LOCATE, 1)]);
            fixture.discovery.release.add_permits(1);
            assert_eq!(
                read.await.unwrap(),
                fixture.payload,
                "full binary read for {name}"
            );
            fixture.assert_reads(1, peer, backing);
            checks.phase(name, &before, vec![Expected(LOCATE, success, error, 0, 0)]);

            let warm = storage::snapshot();
            assert_eq!(
                fixture.store.get(&fixture.id).await.unwrap(),
                fixture.payload
            );
            fixture.assert_reads(1, peer, backing);
            checks.phase("warm_ram", &warm, vec![Expected(LOCATE, 0, 0, 0, 0)]);
            fixture.cleanup().await;
        }

        let fixture = Fixture::new(LookupOutcome::Peers).await;
        let before = storage::snapshot();
        let mut read = fixture.store.get(&fixture.id);
        wait_until_locate(&fixture.discovery, &mut read).await;
        checks.pending("caller_cancel", 1, vec![(LOCATE, 1)]);
        drop(read);
        fixture.assert_reads(1, 0, 0);
        checks.phase("caller_cancel", &before, vec![Expected(LOCATE, 0, 0, 1, 0)]);

        fixture.discovery.held.store(false, Ordering::SeqCst);
        let retry = storage::snapshot();
        // One miss permit and the identical flight key make this a public proof
        // that dropping the held lookup releases both admission and flight ownership.
        assert_eq!(
            fixture.store.get(&fixture.id).await.unwrap(),
            fixture.payload
        );
        fixture.assert_reads(2, 1, 0);
        checks.phase("same_id_retry", &retry, vec![Expected(LOCATE, 1, 0, 0, 0)]);
        fixture.cleanup().await;

        let fixture = Fixture::new(LookupOutcome::Peers).await;
        let before = storage::snapshot();
        let mut read = fixture.store.get(&fixture.id);
        wait_until_locate(&fixture.discovery, &mut read).await;
        checks.pending("existing_peer_deadline", 1, vec![(LOCATE, 1)]);
        // Never release discovery. The existing whole-peer timeout drops the
        // lookup future and the public read still falls back to exact backing bytes.
        assert_eq!(read.await.unwrap(), fixture.payload);
        fixture.assert_reads(1, 0, 1);
        checks.phase(
            "existing_peer_deadline",
            &before,
            vec![Expected(LOCATE, 0, 0, 1, 0)],
        );
        fixture.cleanup().await;

        checks.verify();
    })
    .await
    .expect("bounded isolated cache discovery metrics component fixture");
}
