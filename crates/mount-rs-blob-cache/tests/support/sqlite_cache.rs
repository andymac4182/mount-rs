//! Persistent backing and cache savings share one actual SQLite authority.
//! This bounded component cell does not qualify ten CLI processes or capacity.
use super::*;
use mount_rs_core::{
    Loopback,
    diagnostics::storage,
    storage::{BlockReconcileReport, MetadataStore, NodeData},
};
use mount_rs_sdk::{Filesystem, SplitOptions, StoreConfig};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore, sqlite_io_diagnostics};
use serde_json::{Value, json};
use std::path::Path;

const BLOCK_BYTES: usize = 4096;
const FILES: usize = 3;
const DISK_ENTRY_BYTES: usize = BLOCK_BYTES + 32;

// Disk fills hold payload plus the production 128-byte reservation charge.
// A disk-only cache needs headroom even when this cell's payload is 4 KiB.
fn isolated_cache(path: &Path, memory: usize, disk: usize) -> Arc<LocalCache> {
    LocalCache::new(LocalCacheConfig {
        directory: path.to_owned(),
        memory_bytes: memory,
        disk_bytes: disk,
        max_entries: 128,
        max_blob_bytes: 8192,
    })
    .unwrap()
}

// Retain immediately, including unwinding/timeout before any cleanup can run.
// The owned parent removes unknown fixtures only after terminal containment.
struct PersistentDirectory {
    path: PathBuf,
    removed: bool,
}
impl PersistentDirectory {
    fn new() -> Self {
        Self {
            path: tempfile::tempdir().unwrap().keep(),
            removed: false,
        }
    }
    fn remove_after_oracles(&mut self) {
        std::fs::remove_dir_all(&self.path).expect("remove observed closed SQLite fixture");
        self.removed = true;
    }
}
impl Drop for PersistentDirectory {
    fn drop(&mut self) {
        if !self.removed {
            eprintln!(
                "sqlite_cache_cleanup=pending retained_directory={:?}",
                self.path
            );
        }
    }
}

fn payload(file: usize) -> Vec<u8> {
    (0..BLOCK_BYTES)
        .map(|i| ((i + file * 37) % 251) as u8)
        .collect()
}
fn options(root: &Path, owner: &str) -> SplitOptions {
    let mut options = SplitOptions::memory(owner, BLOCK_BYTES).with_compact_inode_updates(true);
    options.metadata = StoreConfig::Sqlite {
        path: root.join("metadata.sqlite"),
    };
    options.blocks = StoreConfig::Sqlite {
        path: root.join("blocks.sqlite"),
    };
    options
}

#[derive(Debug, PartialEq)]
struct PersistedState {
    backing: ConcurrentBackingId,
    ids: Vec<BlockId>,
    namespace: Value,
    anchor: Value,
}

async fn persisted_oracle(root: &Path) -> PersistedState {
    let metadata = SqliteMetadataStore::open(root.join("metadata.sqlite")).unwrap();
    let mode = metadata
        .compact_inode_mode_state()
        .await
        .unwrap()
        .expect("persisted compact mode");
    let snapshot = metadata.load_compact_snapshot(mode.backing).await.unwrap();
    let namespace = snapshot.namespace().unwrap();
    assert_eq!(namespace.nodes.len(), FILES + 1);
    assert_eq!(snapshot.anchor.members.len(), FILES + 1);
    assert_eq!(snapshot.anchor.generation, mode.structural_generation);
    let root_node = &namespace.nodes[&namespace.root];
    assert!(root_node.stats.is_directory());
    let NodeData::Directory { entries } = &root_node.data else {
        panic!("persisted root type")
    };
    assert_eq!(entries.len(), FILES);
    let raw_metadata = rusqlite::Connection::open(root.join("metadata.sqlite")).unwrap();
    let write_mode: String = raw_metadata
        .query_row(
            "SELECT write_mode FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(write_mode, "MRC5");
    let blocks = SqliteBlockStore::open(root.join("blocks.sqlite")).unwrap();
    blocks
        .verify_concurrent_backing(mode.backing)
        .await
        .unwrap();
    let raw_blocks = rusqlite::Connection::open(root.join("blocks.sqlite")).unwrap();
    let mut ids = Vec::new();
    for file in 0..FILES {
        let name = format!("file-{file}");
        let entry = entries
            .iter()
            .find(|entry| entry.name == name)
            .expect("persisted named file");
        let node = &namespace.nodes[&entry.inode];
        assert!(node.stats.is_file());
        assert_eq!(node.stats.size, BLOCK_BYTES as u64);
        let NodeData::File(layout) = &node.data else {
            panic!("persisted file type")
        };
        assert_eq!(layout.extents.len(), 1);
        let extent = &layout.extents[0];
        assert_eq!(
            (extent.file_offset, extent.block_offset, extent.length),
            (0, 0, BLOCK_BYTES as u64)
        );
        let raw: Vec<u8> = raw_blocks
            .query_row(
                "SELECT bytes FROM mount_rs_blocks WHERE id=?1",
                [&extent.block.0],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(raw, payload(file), "independent raw stored block bytes");
        ids.push(extent.block.clone());
    }
    assert_eq!(ids.iter().collect::<BTreeSet<_>>().len(), FILES);
    let state = PersistedState {
        backing: mode.backing,
        ids,
        namespace: serde_json::to_value(&namespace).unwrap(),
        anchor: serde_json::to_value(&snapshot.anchor).unwrap(),
    };
    drop(raw_blocks);
    drop(blocks);
    drop(raw_metadata);
    drop(metadata);
    state
}

async fn sdk_oracle(root: &Path) {
    let fresh = Filesystem::split(options(root, "fresh-undecorated-reader"))
        .await
        .unwrap();
    let driver = fresh.driver();
    let view = Loopback::from_arc(driver.clone());
    for file in 0..FILES {
        let name = format!("/file-{file}");
        assert_eq!(view.read_file(&name).await.unwrap(), payload(file));
        let handle = driver.open(&name, "r", 0).await.unwrap();
        let mut received = vec![0; BLOCK_BYTES + 17];
        assert_eq!(
            handle.read(&mut received, Some(0)).await.unwrap(),
            BLOCK_BYTES
        );
        assert_eq!(&received[..BLOCK_BYTES], payload(file));
        assert_eq!(
            handle
                .read(&mut received, Some(BLOCK_BYTES as u64))
                .await
                .unwrap(),
            0
        );
        handle.close().await.unwrap();
        drop(handle);
    }
    fresh.shutdown().await.unwrap();
    drop(view);
    drop(driver);
    drop(fresh);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Gets {
    started: u64,
    success: u64,
    errors: u64,
    cancelled: u64,
    bytes: u64,
    in_flight: u64,
}
impl Gets {
    fn delta(self, before: Self) -> Self {
        Self {
            started: self.started.checked_sub(before.started).unwrap(),
            success: self.success.checked_sub(before.success).unwrap(),
            errors: self.errors.checked_sub(before.errors).unwrap(),
            cancelled: self.cancelled.checked_sub(before.cancelled).unwrap(),
            bytes: self.bytes.checked_sub(before.bytes).unwrap(),
            in_flight: self.in_flight,
        }
    }
}
struct CountingBacking {
    inner: SqliteBlockStore,
    gets: Mutex<Gets>,
}
struct GetAttempt<'a> {
    backing: &'a CountingBacking,
    finished: bool,
}
impl GetAttempt<'_> {
    fn finish(&mut self, result: &Result<Vec<u8>>) {
        let mut counts = self.backing.gets.lock().unwrap();
        counts.in_flight -= 1;
        match result {
            Ok(bytes) => {
                counts.success += 1;
                counts.bytes += bytes.len() as u64;
            }
            Err(_) => counts.errors += 1,
        }
        self.finished = true;
    }
}
impl Drop for GetAttempt<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let mut counts = self.backing.gets.lock().unwrap();
            counts.cancelled += 1;
            counts.in_flight -= 1;
        }
    }
}
#[async_trait]
impl BlockStore for CountingBacking {
    fn durable(&self) -> bool {
        self.inner.durable()
    }
    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        self.inner.prepare_concurrent_backing().await
    }
    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        self.inner.verify_concurrent_backing(expected).await
    }
    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.inner.get_for_migration(id).await
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.inner.put(bytes).await
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        {
            let mut counts = self.gets.lock().unwrap();
            counts.started += 1;
            counts.in_flight += 1;
        }
        let mut attempt = GetAttempt {
            backing: self,
            finished: false,
        };
        let result = self.inner.get(id).await;
        attempt.finish(&result);
        result
    }
    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.inner.delete(id).await
    }
    async fn reconcile(
        &self,
        live: &BTreeSet<BlockId>,
        grace: Duration,
    ) -> Result<BlockReconcileReport> {
        self.inner.reconcile(live, grace).await
    }
}

fn observed_connection(sample: &Value) -> std::result::Result<&Value, &'static str> {
    if sample.get("error").is_some() {
        return Err("SQLite registry error");
    }
    let connections = sample["connections"]
        .as_array()
        .ok_or("missing SQLite connections")?;
    if connections.len() != 1 {
        return Err("exactly one block provider connection required");
    }
    let connection = &connections[0];
    if connection.get("error").is_some()
        || connection["connection_id"].as_u64().is_none()
        || connection["vfs_observed"] != true
        || connection["counter_overflow"] != false
        || connection["sql_statements"].as_u64().is_none()
        || connection["configuration"]["synchronous"] != 2
        || connection["configuration"]["is_autocommit"] != true
        || connection["configuration"]["journal_mode"]
            .as_str()
            .is_none()
        || connection["sql_categories"].as_object().is_none()
        || connection["sql_profile"]["SELECT"]["completed"]
            .as_u64()
            .is_none()
        || connection["pager"]["cache_hits"].as_u64().is_none()
        || connection["pager"]["cache_misses"].as_u64().is_none()
        || connection["pager"]["page_writes"].as_u64().is_none()
        || connection["pager"]["cache_spills"].as_u64().is_none()
    {
        return Err("incomplete SQLite connection observation");
    }
    let vfs = &sample["vfs"];
    if vfs.get("error").is_some()
        || vfs["schema"] != "mount-rs.sqlite-vfs.v1"
        || vfs["overflow"] != false
        || vfs["in_flight"] != 0
        || vfs["checkpoint"]["active_windows"] != 0
    {
        return Err("incomplete or pending VFS observation");
    }
    let entries = vfs["entries"].as_array().ok_or("missing VFS rows")?;
    let mut names = BTreeSet::new();
    for entry in entries {
        let name = entry["name"].as_str().ok_or("missing VFS row identity")?;
        if !names.insert(name)
            || entry["overflow"] != false
            || [
                "completed",
                "errors",
                "requested_bytes",
                "confirmed_bytes",
                "short_reads",
            ]
            .iter()
            .any(|field| entry[*field].as_u64().is_none())
        {
            return Err("incomplete VFS row");
        }
    }
    let expected_names = ["main_database", "main_journal", "wal", "temporary", "other"]
        .into_iter()
        .flat_map(|role| ["read", "write", "sync"].map(|op| format!("{role}.{op}")))
        .collect::<BTreeSet<_>>();
    if names
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>()
        != expected_names
    {
        return Err("incomplete VFS role bank");
    }
    Ok(connection)
}

// Eight permits are the existing LocalCache worker bank. Hold all of them to
// observe actual drain and to prevent late fills from racing invalidation.
async fn cache_barrier(cache: &Arc<LocalCache>) -> Vec<mount_rs_blob_cache::CacheIoPermit> {
    timeout(CLEANUP_BOUND, async {
        let mut permits = Vec::new();
        for _ in 0..8 {
            permits.push(cache.io_permit().await.unwrap());
        }
        permits
    })
    .await
    .expect("observed complete cache-worker permit bank")
}
async fn cold(cache: &Arc<LocalCache>, scope: &CacheScope, id: &BlockId) {
    let mut permits = cache_barrier(cache).await;
    cache
        .invalidate_with_permit(&mut permits[0], scope, id)
        .unwrap();
    assert!(
        cache
            .get_memory(scope, id, IntegrityPolicy::Opaque)
            .is_none()
    );
    assert!(!cache.path_for(scope, id).exists());
    drop(permits);
}
fn complete_entry(cache: &Arc<LocalCache>, scope: &CacheScope, id: &BlockId, bytes: &[u8]) {
    let stored = std::fs::read(cache.path_for(scope, id)).expect("complete owned disk admission");
    assert_eq!(stored.len(), bytes.len() + 32);
    assert_eq!(&stored[32..], bytes);
    let key = LocalCache::key_hashed(&LocalCache::scope_hash(scope), id);
    let mut checksum = Sha256::new();
    checksum.update(key);
    checksum.update(bytes);
    assert_eq!(&stored[..32], &checksum.finalize()[..]);
}
async fn retire(store: Arc<CachedBlockStore>, cache: Arc<LocalCache>) {
    drop(store);
    drop(cache_barrier(&cache).await);
    let owner = Arc::downgrade(&cache);
    drop(cache);
    timeout(CLEANUP_BOUND, async {
        while owner.strong_count() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("actual isolated cache owner release");
}

struct ScopedPair {
    pair: Pair,
    scope: CacheScope,
}
impl ScopedPair {
    fn new(scope: CacheScope) -> Self {
        let pair = Pair::new(0, 32768);
        for cache in [pair.a_cache.as_ref().unwrap(), &pair.b_cache] {
            cache.unregister_scope(&super::scope());
            cache
                .register_scope(scope.clone(), IntegrityPolicy::Opaque)
                .unwrap();
        }
        Self { pair, scope }
    }
    async fn stop_holder(&mut self) {
        let owner = Arc::downgrade(self.pair.a_cache.as_ref().unwrap());
        self.pair.stop_a().await;
        timeout(CLEANUP_BOUND, async {
            while owner.strong_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("actual holder cache owner release");
    }
    async fn restart_holder(&mut self) {
        // Observe real driver release; retain the first bound socket until the
        // one intentional handoff to the new transport. A collision still fails.
        let released = bind_stopped_fixture_udp(self.pair.address).await.unwrap();
        drop(released);
        let local = cache(&self.pair.dir.as_ref().unwrap().path().join("a"), 0, 32768);
        local
            .register_scope(self.scope.clone(), IntegrityPolicy::Opaque)
            .unwrap();
        let peer = QuicPeerTransport::bind(
            config(
                "a",
                self.pair.address,
                self.pair.cert.clone(),
                self.pair.key.clone_key(),
                self.pair.roots.clone(),
                self.pair.trusted.clone(),
            ),
            local.clone(),
        )
        .unwrap();
        self.pair.track_cache(&local);
        self.pair.a_cache = Some(local);
        self.pair.a = Some(peer);
    }
}

async fn cached(
    backing: Arc<CountingBacking>,
    local: Arc<LocalCache>,
    scope: &CacheScope,
    runtime: Option<Arc<DistributedRuntime>>,
) -> Arc<CachedBlockStore> {
    let mut store = CachedBlockStore::new(
        backing,
        local,
        scope.identity.clone(),
        IntegrityPolicy::Opaque,
    );
    if let Some(runtime) = runtime {
        store = store.with_runtime(runtime);
    }
    assert_eq!(
        store.prepare_concurrent_backing().await.unwrap(),
        scope.backing
    );
    Arc::new(store)
}

struct Expected {
    local: u64,
    peer: u64,
    backing: u64,
    hit_bytes: u64,
    peer_attempts: u64,
}
struct Phase {
    gets: Gets,
    cache: CacheMetricsSnapshot,
    peer_attempts: u64,
    connection_id: u64,
    configuration: Value,
    storage: storage::Snapshot,
}
impl Phase {
    fn begin(store: &CachedBlockStore, backing: &CountingBacking, peer: &CountPeer) -> Self {
        let sample = sqlite_io_diagnostics(true);
        let connection =
            observed_connection(&sample).expect("drained phase-entry SQLite observation");
        assert_eq!(backing.gets.lock().unwrap().in_flight, 0);
        let snapshot = storage::snapshot();
        assert_eq!(snapshot.in_flight, 0, "drained storage phase entry");
        Self {
            gets: *backing.gets.lock().unwrap(),
            cache: store.metrics().snapshot(),
            peer_attempts: peer.gets.load(Ordering::SeqCst),
            connection_id: connection["connection_id"].as_u64().unwrap(),
            configuration: connection["configuration"].clone(),
            storage: snapshot,
        }
    }
    async fn finish(
        self,
        name: &str,
        store: &CachedBlockStore,
        backing: &CountingBacking,
        peer: &CountPeer,
        local: &Arc<LocalCache>,
        expected: Expected,
    ) {
        let permits = cache_barrier(local).await;
        timeout(CLEANUP_BOUND, async {
            while storage::snapshot().in_flight != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("observed terminal storage spans");
        let sqlite = sqlite_io_diagnostics(false);
        let connection =
            observed_connection(&sqlite).expect("drained phase-completion SQLite observation");
        assert_eq!(
            connection["connection_id"], self.connection_id,
            "stable provider connection"
        );
        assert_eq!(connection["configuration"], self.configuration);
        let gets = backing.gets.lock().unwrap().delta(self.gets);
        assert_eq!(
            gets,
            Gets {
                started: expected.backing,
                success: expected.backing,
                bytes: expected.backing * BLOCK_BYTES as u64,
                ..Gets::default()
            },
            "delegated SQLite GETs in {name}"
        );
        // There is one SELECT per actual SqliteBlockStore::get; observer queries
        // are suppressed and all setup/authority operations precede the reset.
        assert_eq!(
            connection["sql_statements"], expected.backing,
            "actual SQL count in {name}"
        );
        assert_eq!(
            connection["sql_categories"]["SELECT"].as_u64().unwrap_or(0),
            expected.backing
        );
        assert_eq!(
            connection["sql_profile"]["SELECT"]["completed"],
            expected.backing
        );
        let now = store.metrics().snapshot();
        assert_eq!(
            now.local_hits - self.cache.local_hits,
            expected.local,
            "local hits in {name}"
        );
        assert_eq!(
            now.peer_hits - self.cache.peer_hits,
            expected.peer,
            "peer hits in {name}"
        );
        assert_eq!(
            now.backing_fetches - self.cache.backing_fetches,
            expected.backing,
            "backing fetches in {name}"
        );
        assert_eq!(
            now.hit_bytes - self.cache.hit_bytes,
            expected.hit_bytes,
            "hit bytes in {name}"
        );
        let attempts = peer
            .gets
            .load(Ordering::SeqCst)
            .checked_sub(self.peer_attempts)
            .unwrap();
        assert_eq!(
            attempts, expected.peer_attempts,
            "real peer query attempts in {name}"
        );
        let spans = storage::snapshot()
            .delta(&self.storage)
            .expect("checked storage delta");
        let cache_spans = spans
            .entries
            .iter()
            .filter(|row| row.name.starts_with("blob_cache."))
            .collect::<Vec<_>>();
        println!(
            "MOUNT_RS_SQLITE_CACHE {}",
            json!({"kind":"phase","phase":name,
            "complete_bytes":true,"delegated_gets":gets.started,"delegated_success":gets.success,
            "delegated_errors":gets.errors,"delegated_cancelled":gets.cancelled,"backing_bytes":gets.bytes,
            "local_hits":expected.local,"peer_hits":expected.peer,"peer_get_attempts":attempts,
            "backing_fetches":expected.backing,"hit_bytes":expected.hit_bytes,
            "cache_errors":now.cache_errors - self.cache.cache_errors,
            "maintenance_dropped":now.maintenance_dropped - self.cache.maintenance_dropped,
            "sqlite":sqlite,"storage":cache_spans,
            "io_scope":"delegated SQLite calls, statements, pager and VFS callbacks; not physical IOPS"})
        );
        drop(permits);
    }
}

async fn read_phase(
    name: &str,
    store: &Arc<CachedBlockStore>,
    backing: &Arc<CountingBacking>,
    pair: &Pair,
    local: &Arc<LocalCache>,
    file: (&BlockId, usize),
    expected: Expected,
) {
    let phase = Phase::begin(store, backing, &pair.counted);
    assert_eq!(
        store.get(file.0).await.unwrap(),
        payload(file.1),
        "complete bytes in {name}"
    );
    phase
        .finish(name, store, backing, &pair.counted, local, expected)
        .await;
}
fn fallback(peer_attempts: u64) -> Expected {
    Expected {
        local: 0,
        peer: 0,
        backing: 1,
        hit_bytes: 0,
        peer_attempts,
    }
}
fn peer_hit() -> Expected {
    Expected {
        local: 0,
        peer: 1,
        backing: 0,
        hit_bytes: BLOCK_BYTES as u64,
        peer_attempts: 1,
    }
}

pub(super) async fn run() {
    timeout(BOUND, async {
        assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
        assert!(
            storage::enabled(),
            "owned parent must enable profiling before initialization"
        );
        let mut directory = PersistentDirectory::new();
        let first = Filesystem::split(options(&directory.path, "persistent-seed"))
            .await
            .unwrap();
        let view = Loopback::from_arc(first.driver());
        for file in 0..FILES {
            view.write_file(&format!("/file-{file}"), &payload(file))
                .await
                .unwrap();
        }
        first.driver().syncfs().await.unwrap();
        first.shutdown().await.unwrap();
        drop(view);
        drop(first);
        sdk_oracle(&directory.path).await;
        let initial = persisted_oracle(&directory.path).await;
        assert!(
            sqlite_io_diagnostics(false)["connections"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let backing = Arc::new(CountingBacking {
            inner: SqliteBlockStore::open(directory.path.join("blocks.sqlite")).unwrap(),
            gets: Mutex::new(Gets::default()),
        });
        assert!(backing.durable());
        assert_eq!(
            backing.prepare_concurrent_backing().await.unwrap(),
            initial.backing
        );
        let actual = CacheScope {
            identity: super::scope().identity,
            backing: initial.backing,
        };
        let mut scoped = ScopedPair::new(actual.clone());
        let completed = scoped.pair.cleanup_completion();
        let runtime = scoped.pair.runtime(false);
        let store = cached(
            backing.clone(),
            scoped.pair.b_cache.clone(),
            &actual,
            Some(runtime.clone()),
        )
        .await;
        let id = &initial.ids[0];

        // A is an authenticated nonholder; cold absence is an actual peer miss.
        cold(&scoped.pair.b_cache, &actual, id).await;
        assert!(
            !scoped
                .pair
                .a_cache
                .as_ref()
                .unwrap()
                .path_for(&actual, id)
                .exists()
        );
        read_phase(
            "cold-nonholder",
            &store,
            &backing,
            &scoped.pair,
            &scoped.pair.b_cache,
            (id, 0),
            fallback(1),
        )
        .await;
        cold(&scoped.pair.b_cache, &actual, id).await;
        let holder = scoped.pair.a_cache.as_ref().unwrap();
        holder
            .insert(&actual, id, &payload(0), IntegrityPolicy::Opaque)
            .unwrap();
        assert_eq!(holder.usage().0, 0);
        complete_entry(holder, &actual, id, &payload(0));
        read_phase(
            "disk-only-peer",
            &store,
            &backing,
            &scoped.pair,
            &scoped.pair.b_cache,
            (id, 0),
            peer_hit(),
        )
        .await;
        cold(&scoped.pair.b_cache, &actual, id).await;
        // Distinct unheld bytes cannot be masked by the preceding warm peer.
        read_phase(
            "stale-holder",
            &store,
            &backing,
            &scoped.pair,
            &scoped.pair.b_cache,
            (&initial.ids[2], 2),
            fallback(1),
        )
        .await;
        cold(&scoped.pair.b_cache, &actual, id).await;
        scoped.stop_holder().await;
        let stopped_owner = bind_stopped_fixture_udp(scoped.pair.address).await.unwrap();
        read_phase(
            "holder-outage",
            &store,
            &backing,
            &scoped.pair,
            &scoped.pair.b_cache,
            (id, 0),
            fallback(1),
        )
        .await;
        drop(stopped_owner);
        scoped.restart_holder().await;
        assert_eq!(scoped.pair.a_cache.as_ref().unwrap().usage().0, 0);
        complete_entry(
            scoped.pair.a_cache.as_ref().unwrap(),
            &actual,
            id,
            &payload(0),
        );
        cold(&scoped.pair.b_cache, &actual, id).await;
        read_phase(
            "same-holder-restart",
            &store,
            &backing,
            &scoped.pair,
            &scoped.pair.b_cache,
            (id, 0),
            peer_hit(),
        )
        .await;

        // Independent requesters have no distributed runtime or eligible peers.
        let cache_root = scoped.pair.dir.as_ref().unwrap().path().to_owned();
        let ram = isolated_cache(&cache_root.join("ram-only"), 8192, 0);
        scoped.pair.track_cache(&ram);
        let ram_store = cached(backing.clone(), ram.clone(), &actual, None).await;
        read_phase(
            "ram-cold",
            &ram_store,
            &backing,
            &scoped.pair,
            &ram,
            (id, 0),
            fallback(0),
        )
        .await;
        let phase = Phase::begin(&ram_store, &backing, &scoped.pair.counted);
        for _ in 0..20 {
            assert_eq!(ram_store.get(id).await.unwrap(), payload(0));
        }
        phase
            .finish(
                "ram-twenty-repeats",
                &ram_store,
                &backing,
                &scoped.pair.counted,
                &ram,
                Expected {
                    local: 20,
                    peer: 0,
                    backing: 0,
                    hit_bytes: 20 * BLOCK_BYTES as u64,
                    peer_attempts: 0,
                },
            )
            .await;
        retire(ram_store, ram).await;

        // Full payloads still return from backing when optional disk admission
        // is refused; observe that refusal independently from disk latency.
        let rejected = cache(&cache_root.join("disk-budget-reject"), 0, 32768);
        scoped.pair.track_cache(&rejected);
        assert!(rejected.reserve_pending(BLOCK_BYTES).is_none());
        let rejected_store = cached(backing.clone(), rejected.clone(), &actual, None).await;
        read_phase(
            "disk-budget-reject",
            &rejected_store,
            &backing,
            &scoped.pair,
            &rejected,
            (id, 0),
            fallback(0),
        )
        .await;
        assert_eq!(rejected_store.metrics().snapshot().maintenance_dropped, 1);
        assert!(!rejected.path_for(&actual, id).exists());
        assert_eq!(rejected.usage(), (0, 0, 0));
        retire(rejected_store, rejected).await;

        let disk_path = cache_root.join("disk-only");
        let disk = isolated_cache(&disk_path, 0, 32768);
        scoped.pair.track_cache(&disk);
        let disk_store = cached(backing.clone(), disk.clone(), &actual, None).await;
        read_phase(
            "disk-cold",
            &disk_store,
            &backing,
            &scoped.pair,
            &disk,
            (id, 0),
            fallback(0),
        )
        .await;
        complete_entry(&disk, &actual, id, &payload(0));
        let disk_file = disk.path_for(&actual, id);
        retire(disk_store, disk).await;
        let disk = isolated_cache(&disk_path, 0, 32768);
        scoped.pair.track_cache(&disk);
        let disk_store = cached(backing.clone(), disk.clone(), &actual, None).await;
        assert_eq!(disk.usage().0, 0);
        read_phase(
            "disk-restart",
            &disk_store,
            &backing,
            &scoped.pair,
            &disk,
            (id, 0),
            Expected {
                local: 1,
                peer: 0,
                backing: 0,
                hit_bytes: BLOCK_BYTES as u64,
                peer_attempts: 0,
            },
        )
        .await;
        retire(disk_store, disk).await;
        let mut corrupt = std::fs::read(&disk_file).unwrap();
        let checksum = corrupt[..32].to_vec();
        corrupt[32] ^= 0xff;
        std::fs::write(&disk_file, &corrupt).unwrap();
        assert_eq!(&corrupt[..32], checksum);
        let disk = isolated_cache(&disk_path, 0, 32768);
        scoped.pair.track_cache(&disk);
        let disk_store = cached(backing.clone(), disk.clone(), &actual, None).await;
        read_phase(
            "corrupt-isolated-disk",
            &disk_store,
            &backing,
            &scoped.pair,
            &disk,
            (id, 0),
            fallback(0),
        )
        .await;
        retire(disk_store, disk).await;

        let eviction_path = cache_root.join("eviction-only");
        let eviction = isolated_cache(&eviction_path, 0, DISK_ENTRY_BYTES);
        scoped.pair.track_cache(&eviction);
        let eviction_store = cached(backing.clone(), eviction.clone(), &actual, None).await;
        read_phase(
            "eviction-first-admitted",
            &eviction_store,
            &backing,
            &scoped.pair,
            &eviction,
            (id, 0),
            fallback(0),
        )
        .await;
        complete_entry(&eviction, &actual, id, &payload(0));
        let first_entry = eviction.path_for(&actual, id);
        read_phase(
            "eviction-replacement",
            &eviction_store,
            &backing,
            &scoped.pair,
            &eviction,
            (&initial.ids[1], 1),
            fallback(0),
        )
        .await;
        complete_entry(&eviction, &actual, &initial.ids[1], &payload(1));
        assert!(
            !first_entry.exists(),
            "observed eviction, not failed first admission"
        );
        retire(eviction_store, eviction).await;
        let eviction = isolated_cache(&eviction_path, 0, DISK_ENTRY_BYTES);
        scoped.pair.track_cache(&eviction);
        let eviction_store = cached(backing.clone(), eviction.clone(), &actual, None).await;
        read_phase(
            "evicted-block-after-restart",
            &eviction_store,
            &backing,
            &scoped.pair,
            &eviction,
            (id, 0),
            fallback(0),
        )
        .await;
        retire(eviction_store, eviction).await;

        drop(store);
        runtime.shutdown().await.unwrap();
        drop(runtime);
        scoped.pair.shutdown().await;
        drop(scoped);
        timeout(CLEANUP_BOUND, completed)
            .await
            .expect("bounded actual pair cleanup")
            .expect("pair cleanup receipt was not cancelled")
            .expect("peer idle and all cache owners released");
        let counts = *backing.gets.lock().unwrap();
        assert_eq!(
            counts,
            Gets {
                started: 10,
                success: 10,
                bytes: 10 * BLOCK_BYTES as u64,
                ..Gets::default()
            }
        );
        let backing_owner = Arc::downgrade(&backing);
        drop(backing);
        assert_eq!(
            backing_owner.strong_count(),
            0,
            "every counted backing owner released"
        );
        assert!(
            sqlite_io_diagnostics(false)["connections"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let final_state = persisted_oracle(&directory.path).await;
        assert_eq!(
            final_state, initial,
            "fresh metadata, authority and extent identities unchanged by cache faults"
        );
        sdk_oracle(&directory.path).await;
        assert!(
            sqlite_io_diagnostics(false)["connections"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        directory.remove_after_oracles();
        assert!(!directory.path.exists());
        println!(
            "MOUNT_RS_SQLITE_CACHE {}",
            json!({"kind":"complete","files":FILES,
            "bytes_each":BLOCK_BYTES,"backing_gets":counts.success,"backing_bytes":counts.bytes,
            "logical_cache_reads":33,"observed_cache_hits":23,"hit_bytes":23 * BLOCK_BYTES,
            "persistent_metadata_and_block_ids_unchanged":true,"fresh_sdk_payload_and_eof":true,
            "all_cache_peer_provider_owners_released":true,"sqlite_fixture_removed":true,
            "capacity_claim":false,"power_loss":false,"physical_iops":false})
        );
    })
    .await
    .expect("bounded persistent SQLite cache component qualification");
}

#[test]
fn missing_or_pending_sqlite_observations_cannot_qualify() {
    assert!(observed_connection(&json!({})).is_err());
    assert!(observed_connection(&json!({"connections":[]})).is_err());
    assert!(observed_connection(&json!({"connections":[{}]})).is_err());
    // Missing counters/identity never become fake zero activity.
    assert!(
        observed_connection(&json!({"connections":[{"vfs_observed":true,
        "counter_overflow":false,"sql_statements":0,"configuration":{"synchronous":2,
        "is_autocommit":true}}],"vfs":{"schema":"mount-rs.sqlite-vfs.v1",
        "overflow":false,"in_flight":0,"checkpoint":{"active_windows":0}}}))
        .is_err()
    );
}

#[test]
fn sqlite_observation_contract_rejects_missing_counters_and_pending_work() {
    let rows = ["main_database", "main_journal", "wal", "temporary", "other"]
        .into_iter()
        .flat_map(|role| {
            ["read", "write", "sync"].map(|op| {
                json!({
                    "name":format!("{role}.{op}"),"overflow":false,"completed":0,
                    "errors":0,"requested_bytes":0,"confirmed_bytes":0,"short_reads":0,
                })
            })
        })
        .collect::<Vec<_>>();
    let complete = json!({"connections":[{"connection_id":1,"vfs_observed":true,
        "counter_overflow":false,"sql_statements":0,"sql_categories":{},
        "sql_profile":{"SELECT":{"completed":0}},"pager":{"cache_hits":0,
        "cache_misses":0,"page_writes":0,"cache_spills":0},
        "configuration":{"journal_mode":"delete","synchronous":2,"is_autocommit":true}}],
        "vfs":{"schema":"mount-rs.sqlite-vfs.v1","overflow":false,"in_flight":0,
        "checkpoint":{"active_windows":0},"entries":rows}});
    assert!(observed_connection(&complete).is_ok());
    let mut missing_counter = complete.clone();
    missing_counter["connections"][0]["sql_statements"] = Value::Null;
    assert!(observed_connection(&missing_counter).is_err());
    let mut missing_identity = complete.clone();
    missing_identity["connections"][0]["connection_id"] = Value::Null;
    assert!(observed_connection(&missing_identity).is_err());
    let mut pending = complete.clone();
    pending["vfs"]["in_flight"] = json!(1);
    assert!(observed_connection(&pending).is_err());
    let mut copying = complete.clone();
    copying["vfs"]["checkpoint"]["active_windows"] = json!(1);
    assert!(observed_connection(&copying).is_err());
    let mut raced = complete.clone();
    raced["vfs"]["error"] = json!("SQLite VFS counters raced with reset");
    assert!(observed_connection(&raced).is_err());
    let mut missing_row = complete;
    missing_row["vfs"]["entries"].as_array_mut().unwrap().pop();
    assert!(observed_connection(&missing_row).is_err());
}
