use super::*;
use futures_util::{
    FutureExt,
    stream::{self, BoxStream},
};
use mount_rs_core::diagnostics::storage as storage_metrics;
use object_store::memory::InMemory;
use object_store::{
    GetOptions, GetResult, GetResultPayload, ListResult, MultipartUpload, ObjectMeta,
    PutMultipartOptions, PutResult,
};
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::Semaphore;

/// Independent API oracle. Body faults happen in the returned stream after an
/// Ok get, never by turning a body fault into a get fault.
#[derive(Debug)]
struct ApiStore {
    inner: Arc<InMemory>,
    puts: AtomicUsize,
    gets: AtomicUsize,
    bodies: Arc<AtomicUsize>,
    heads: AtomicUsize,
    deletes: AtomicUsize,
    put_gate: Semaphore,
    body_gate: Arc<Semaphore>,
    hold_put: AtomicBool,
    hold_body: AtomicBool,
    put_fault: AtomicUsize,
    get_fault: AtomicBool,
    body_fault: AtomicBool,
    temporary_get_fault: AtomicBool,
    temporary_body_fault: AtomicBool,
    delete_not_found: AtomicBool,
}
impl ApiStore {
    fn new() -> Arc<Self> {
        Self::with_inner(Arc::new(InMemory::new()))
    }
    fn with_inner(inner: Arc<InMemory>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            puts: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            bodies: Arc::new(AtomicUsize::new(0)),
            heads: AtomicUsize::new(0),
            deletes: AtomicUsize::new(0),
            put_gate: Semaphore::new(0),
            body_gate: Arc::new(Semaphore::new(0)),
            hold_put: AtomicBool::new(false),
            hold_body: AtomicBool::new(false),
            put_fault: AtomicUsize::new(0),
            get_fault: AtomicBool::new(false),
            body_fault: AtomicBool::new(false),
            temporary_get_fault: AtomicBool::new(false),
            temporary_body_fault: AtomicBool::new(false),
            delete_not_found: AtomicBool::new(false),
        })
    }
    async fn preseed(&self, bytes: &[u8], actual: &[u8]) -> BlockId {
        let id = BlockId(block_id(bytes));
        self.inner
            .put(
                &ObjectPath::from(format!("private/{}", id.0)),
                PutPayload::from(actual.to_vec()),
            )
            .await
            .unwrap();
        id
    }
}
fn fault() -> object_store::Error {
    object_store::Error::Generic {
        store: "ApiStore",
        source: Box::new(std::io::Error::other("private-test-fault")),
    }
}
fn temporary_fault() -> object_store::Error {
    object_store::Error::Generic {
        store: "ApiStore",
        source: Box::new(std::io::Error::other(
            "connection reset: private-test-fault",
        )),
    }
}
impl std::fmt::Display for ApiStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiStore")
    }
}
#[async_trait]
impl ObjectStore for ApiStore {
    async fn put_opts(
        &self,
        p: &ObjectPath,
        v: PutPayload,
        o: PutOptions,
    ) -> object_store::Result<PutResult> {
        self.puts.fetch_add(1, Ordering::SeqCst);
        if self.hold_put.load(Ordering::SeqCst) {
            self.put_gate.acquire().await.unwrap().forget();
        }
        match self.put_fault.swap(0, Ordering::SeqCst) {
            1 => Err(fault()),
            2 => {
                self.inner.put_opts(p, v, o).await?;
                Err(fault())
            }
            _ => self.inner.put_opts(p, v, o).await,
        }
    }
    async fn put_multipart_opts(
        &self,
        p: &ObjectPath,
        o: PutMultipartOptions,
    ) -> object_store::Result<Box<dyn MultipartUpload>> {
        self.inner.put_multipart_opts(p, o).await
    }
    async fn get_opts(&self, p: &ObjectPath, o: GetOptions) -> object_store::Result<GetResult> {
        if o.head {
            self.heads.fetch_add(1, Ordering::SeqCst);
            return self.inner.get_opts(p, o).await;
        }
        self.gets.fetch_add(1, Ordering::SeqCst);
        if self.get_fault.swap(false, Ordering::SeqCst) {
            return Err(fault());
        }
        if self.temporary_get_fault.swap(false, Ordering::SeqCst) {
            return Err(temporary_fault());
        }
        let result = self.inner.get_opts(p, o).await?;
        let meta = result.meta.clone();
        let range = result.range.clone();
        let attributes = result.attributes.clone();
        let bodies = self.bodies.clone();
        let gate = self.body_gate.clone();
        let hold = self.hold_body.load(Ordering::SeqCst);
        let fail = self.body_fault.swap(false, Ordering::SeqCst);
        let temporary_fail = self.temporary_body_fault.swap(false, Ordering::SeqCst);
        let payload = GetResultPayload::Stream(
            stream::once(async move {
                bodies.fetch_add(1, Ordering::SeqCst);
                if hold {
                    gate.acquire().await.unwrap().forget();
                }
                if fail {
                    Err(fault())
                } else if temporary_fail {
                    Err(temporary_fault())
                } else {
                    result.bytes().await
                }
            })
            .boxed(),
        );
        Ok(GetResult {
            payload,
            meta,
            range,
            attributes,
        })
    }
    async fn delete(&self, p: &ObjectPath) -> object_store::Result<()> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        if self.delete_not_found.load(Ordering::SeqCst) {
            return Err(object_store::Error::NotFound {
                path: p.to_string(),
                source: Box::new(std::io::Error::other("gone")),
            });
        }
        self.inner.delete(p).await
    }
    fn list(&self, p: Option<&ObjectPath>) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
        self.inner
            .list(p)
            .map(|x| {
                x.map(|mut m| {
                    m.last_modified = (UNIX_EPOCH + Duration::from_secs(1)).into();
                    m
                })
            })
            .boxed()
    }
    async fn list_with_delimiter(
        &self,
        p: Option<&ObjectPath>,
    ) -> object_store::Result<ListResult> {
        self.inner.list_with_delimiter(p).await
    }
    async fn copy(&self, f: &ObjectPath, t: &ObjectPath) -> object_store::Result<()> {
        self.inner.copy(f, t).await
    }
    async fn copy_if_not_exists(&self, f: &ObjectPath, t: &ObjectPath) -> object_store::Result<()> {
        self.inner.copy_if_not_exists(f, t).await
    }
}
fn adapter(store: Arc<ApiStore>, enabled: bool) -> ObjectStoreBlockStore {
    let mut blocks = ObjectStoreBlockStore::new(store, "private", false).unwrap();
    blocks.stats = Arc::new(ObjectStoreBlockStoreStatsState {
        raw: enabled.then(|| Box::new(RawState::default())),
        ..Default::default()
    });
    blocks
}
fn snapshot(blocks: &ObjectStoreBlockStore) -> RawApiSnapshot {
    blocks.stats().raw_api.unwrap()
}
fn row<'a>(raw: &'a RawApiSnapshot, name: &str) -> &'a RawApiEntry {
    raw.entries.iter().find(|x| x.name == name).unwrap()
}
fn local_snapshot(blocks: &ObjectStoreBlockStore) -> LocalWorkSnapshot {
    blocks.stats().local_work.unwrap()
}
fn local_row<'a>(local: &'a LocalWorkSnapshot, name: &str) -> &'a LocalWorkEntry {
    local.entries.iter().find(|row| row.name == name).unwrap()
}
fn local_quiescent(local: &LocalWorkSnapshot) {
    assert!(!local.saturated);
    assert_eq!(local.in_flight, 0);
    assert_eq!(
        local.entries.each_ref().map(|row| row.name),
        LOCAL_WORK_NAMES
    );
    for row in &local.entries {
        assert_eq!(row.calls, row.success + row.error + row.cancelled);
        assert_eq!(row.calls, row.latency_log2_us.iter().sum::<u64>());
    }
}
fn quiescent(raw: &RawApiSnapshot) {
    assert_eq!(raw.in_flight, 0);
    assert_eq!(raw.pending_claims, 0);
    assert!(!raw.saturated);
    let c = &raw.claims;
    assert_eq!(
        c.leader_claims,
        c.leader_success + c.leader_error + c.leader_cancelled
    );
    assert_eq!(
        c.follower_claims,
        c.follower_success + c.follower_error + c.follower_cancelled
    );
    for r in &raw.entries {
        assert_eq!(r.calls, r.success + r.error + r.cancelled);
        assert_eq!(r.calls, r.latency_log2_us.iter().sum::<u64>());
    }
}
#[tokio::test]
async fn held_identical_puts_have_one_actual_upload_and_exact_follower_claims() {
    const N: usize = 6;
    let store = ApiStore::new();
    store.hold_put.store(true, Ordering::SeqCst);
    let blocks = adapter(store.clone(), true);
    let bytes = b"same payload";
    let mut leader = blocks.put(bytes);
    assert!(leader.as_mut().now_or_never().is_none());
    let clone = blocks.clone();
    let mut followers = (1..N).map(|_| clone.put(bytes)).collect::<Vec<_>>();
    // Each first poll deterministically takes its claim while the leader is held.
    for follower in &mut followers {
        assert!(follower.as_mut().now_or_never().is_none());
    }
    let held = snapshot(&blocks);
    assert_eq!(held.claims.leader_claims, 1);
    assert_eq!(held.claims.follower_claims, (N - 1) as u64);
    assert_eq!(held.pending_claims, N as u64);
    assert_eq!(held.in_flight, 1);
    assert_eq!(store.puts.load(Ordering::SeqCst), 1);
    let local = local_snapshot(&blocks);
    assert_eq!(local_row(&local, "sha256.digest").calls, N as u64);
    assert_eq!(
        local_row(&local, "block_id.encode").output_bytes,
        65 * N as u64
    );
    assert_eq!(local_row(&local, "copy.upload_payload").calls, 1);
    assert_eq!(
        local_row(&local, "copy.upload_payload").output_bytes,
        bytes.len() as u64
    );
    assert_eq!(local_row(&local, "put.follower_wait").calls, (N - 1) as u64);
    assert_eq!(local.in_flight, (N - 1) as u64);
    let put = row(&held, "put_opts.block_create");
    assert_eq!(
        (
            put.calls,
            put.success,
            put.error,
            put.cancelled,
            put.attempted_bytes,
            put.confirmed_bytes
        ),
        (1, 0, 0, 0, bytes.len() as u64, 0)
    );
    store.put_gate.add_permits(1);
    leader.await.unwrap();
    for follower in followers {
        follower.await.unwrap();
    }
    let raw = snapshot(&blocks);
    quiescent(&raw);
    assert_eq!(blocks.stats().puts, N as u64);
    assert_eq!(raw.claims.follower_success, (N - 1) as u64);
    assert_eq!(
        row(&raw, "put_opts.block_create").confirmed_bytes,
        bytes.len() as u64
    );
    let put = row(&raw, "put_opts.block_create");
    assert_eq!(
        (
            put.calls,
            put.success,
            put.error,
            put.cancelled,
            put.attempted_bytes
        ),
        (
            store.puts.load(Ordering::SeqCst) as u64,
            1,
            0,
            0,
            bytes.len() as u64
        )
    );
    assert_eq!(raw.claims.leader_success, 1);
    let local = local_snapshot(&blocks);
    local_quiescent(&local);
    assert_eq!(
        local_row(&local, "sha256.digest").input_bytes,
        N as u64 * bytes.len() as u64
    );
    assert_eq!(
        local_row(&local, "sha256.digest").output_bytes,
        N as u64 * 32
    );
    assert_eq!(
        local_row(&local, "block_id.encode").input_bytes,
        N as u64 * 32
    );
    assert_eq!(local_row(&local, "copy.cache_insert").calls, 1);
    assert_eq!(
        local_row(&local, "copy.cache_insert").output_bytes,
        bytes.len() as u64
    );
    assert_eq!(local_row(&local, "copy.return_vec").calls, 0);
    assert_eq!(local_row(&local, "cache.lock_acquire").calls, 1);
    assert_eq!(
        local_row(&local, "put.follower_wait").success,
        (N - 1) as u64
    );
    for name in [
        "get.conflict_verify",
        "body_read.conflict_verify",
        "get.block_read",
        "body_read.block_read",
    ] {
        assert_eq!(row(&raw, name).calls, 0);
    }
    assert_eq!(store.gets.load(Ordering::SeqCst), 0);
    eprintln!(
        "RAW_API_HELD_CONTROL logical_puts={} independent_put_calls={} raw_put_calls={} raw_put_success={} attempted_bytes={} confirmed_bytes={} leader_success={} follower_success={}",
        blocks.stats().puts,
        store.puts.load(Ordering::SeqCst),
        put.calls,
        put.success,
        put.attempted_bytes,
        put.confirmed_bytes,
        raw.claims.leader_success,
        raw.claims.follower_success
    );
}
#[tokio::test]
async fn conflict_verification_counts_body_before_integrity_and_separates_get_fault() {
    for case in 0..4 {
        let store = ApiStore::new();
        let bytes = b"expected";
        store
            .preseed(bytes, if case == 1 { b"wrong___" } else { bytes })
            .await;
        store.body_fault.store(case == 2, Ordering::SeqCst);
        store.get_fault.store(case == 3, Ordering::SeqCst);
        let blocks = adapter(store.clone(), true);
        let result = blocks.put(bytes).await;
        assert_eq!(result.is_ok(), case == 0);
        let raw = snapshot(&blocks);
        quiescent(&raw);
        let put = row(&raw, "put_opts.block_create");
        assert_eq!(
            (
                put.calls,
                put.error,
                put.attempted_bytes,
                put.confirmed_bytes
            ),
            (1, 1, 8, 0)
        );
        assert_eq!(blocks.stats().gets, 0);
        let get = row(&raw, "get.conflict_verify");
        assert_eq!(get.calls, store.gets.load(Ordering::SeqCst) as u64);
        assert_eq!(get.success, u64::from(case != 3));
        let body = row(&raw, "body_read.conflict_verify");
        assert_eq!(body.calls, store.bodies.load(Ordering::SeqCst) as u64);
        assert_eq!(body.returned_bytes, if case < 2 { 8 } else { 0 });
        assert_eq!(body.error, u64::from(case == 2));
        assert_eq!(raw.claims.leader_success, u64::from(case == 0));
        assert_eq!(raw.claims.leader_error, u64::from(case != 0));
        assert_eq!(blocks.cache.get(&block_id(bytes)).is_some(), case == 0);
    }
}
#[tokio::test]
async fn committed_lost_reply_is_not_replayed_or_confirmed_and_explicit_retry_verifies() {
    let store = ApiStore::new();
    store.put_fault.store(2, Ordering::SeqCst);
    let blocks = adapter(store.clone(), true);
    let bytes = b"uncertain";
    assert!(blocks.put(bytes).await.is_err());
    assert_eq!(store.puts.load(Ordering::SeqCst), 1);
    assert_eq!(store.gets.load(Ordering::SeqCst), 0);
    assert!(blocks.cache.get(&block_id(bytes)).is_none());
    let raw = snapshot(&blocks);
    assert_eq!(row(&raw, "put_opts.block_create").confirmed_bytes, 0);
    quiescent(&raw);
    let local = local_snapshot(&blocks);
    local_quiescent(&local);
    assert_eq!(
        local_row(&local, "copy.upload_payload").output_bytes,
        bytes.len() as u64
    );
    assert_eq!(local_row(&local, "copy.cache_insert").calls, 0);
    assert_eq!(local_row(&local, "copy.return_vec").calls, 0);
    blocks.put(bytes).await.unwrap();
    let raw = snapshot(&blocks);
    quiescent(&raw);
    assert_eq!(store.puts.load(Ordering::SeqCst), 2);
    assert_eq!(row(&raw, "get.conflict_verify").success, 1);
    assert_eq!(row(&raw, "put_opts.block_create").confirmed_bytes, 0);
}
#[tokio::test]
async fn cancelled_leader_errors_surviving_follower_but_dropped_follower_is_cancelled() {
    let store = ApiStore::new();
    store.hold_put.store(true, Ordering::SeqCst);
    let blocks = adapter(store.clone(), true);
    let mut leader = blocks.put(b"cancel");
    assert!(leader.as_mut().now_or_never().is_none());
    let mut survivor = blocks.put(b"cancel");
    let mut dropped = blocks.put(b"cancel");
    assert!(survivor.as_mut().now_or_never().is_none());
    assert!(dropped.as_mut().now_or_never().is_none());
    drop(dropped);
    drop(leader);
    assert!(survivor.await.is_err());
    let raw = snapshot(&blocks);
    quiescent(&raw);
    assert_eq!(raw.claims.leader_cancelled, 1);
    assert_eq!(raw.claims.follower_error, 1);
    assert_eq!(raw.claims.follower_cancelled, 1);
    assert_eq!(row(&raw, "put_opts.block_create").cancelled, 1);
    assert_eq!(store.puts.load(Ordering::SeqCst), 1);
    assert!(blocks.inflight_puts.lock().unwrap().is_empty());
    let local = local_snapshot(&blocks);
    local_quiescent(&local);
    let wait = local_row(&local, "put.follower_wait");
    assert_eq!(
        (wait.calls, wait.success, wait.error, wait.cancelled),
        (2, 0, 1, 1)
    );
    assert_eq!((wait.input_bytes, wait.output_bytes), (0, 0));
    assert_eq!(local_row(&local, "sha256.digest").success, 3);
    assert_eq!(local_row(&local, "copy.upload_payload").success, 1);
    assert_eq!(local_row(&local, "copy.cache_insert").calls, 0);
}
#[tokio::test]
async fn held_body_is_cancelled_after_successful_get() {
    let store = ApiStore::new();
    let id = store.preseed(b"body", b"body").await;
    store.hold_body.store(true, Ordering::SeqCst);
    let blocks = adapter(store.clone(), true);
    let mut read = blocks.get(&id);
    assert!(read.as_mut().now_or_never().is_none());
    let held = snapshot(&blocks);
    assert_eq!(row(&held, "get.block_read").success, 1);
    assert_eq!(row(&held, "body_read.block_read").calls, 1);
    assert_eq!(store.bodies.load(Ordering::SeqCst), 1);
    assert_eq!(held.in_flight, 1);
    drop(read);
    let raw = snapshot(&blocks);
    quiescent(&raw);
    assert_eq!(row(&raw, "body_read.block_read").cancelled, 1);
    assert_eq!(row(&raw, "body_read.block_read").returned_bytes, 0);
}
#[tokio::test]
async fn direct_read_migration_delete_and_reconcile_preserve_distinct_api_outcomes() {
    let store = ApiStore::new();
    let id = store.preseed(b"read", b"read").await;
    let blocks = adapter(store.clone(), true);
    blocks.get(&id).await.unwrap();
    let cold = local_snapshot(&blocks);
    assert_eq!(local_row(&cold, "copy.return_vec").calls, 1);
    assert_eq!(local_row(&cold, "copy.return_vec").output_bytes, 4);
    assert_eq!(local_row(&cold, "sha256.digest").calls, 1);
    assert_eq!(local_row(&cold, "copy.cache_insert").calls, 1);
    blocks.get(&id).await.unwrap();
    let hot = local_snapshot(&blocks);
    assert_eq!(local_row(&hot, "copy.return_vec").calls, 2);
    assert_eq!(local_row(&hot, "copy.return_vec").output_bytes, 8);
    assert_eq!(local_row(&hot, "sha256.digest").calls, 1);
    assert_eq!(local_row(&hot, "copy.cache_insert").calls, 1);
    blocks.get_for_migration(&id).await.unwrap();
    let migration = local_snapshot(&blocks);
    assert_eq!(local_row(&migration, "copy.return_vec").calls, 3);
    assert_eq!(local_row(&migration, "copy.return_vec").output_bytes, 12);
    assert_eq!(local_row(&migration, "sha256.digest").calls, 2);
    assert_eq!(local_row(&migration, "copy.cache_insert").calls, 1);
    assert_eq!(
        local_row(&migration, "cache.lock_acquire").calls,
        local_row(&hot, "cache.lock_acquire").calls
    );
    blocks.delete(&id).await.unwrap();
    let id = store.preseed(b"stale", b"stale").await;
    store.delete_not_found.store(true, Ordering::SeqCst);
    let report = blocks
        .reconcile(&BTreeSet::new(), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(report.deleted, 1);
    assert!(blocks.get_for_migration(&id).await.is_ok());
    let raw = snapshot(&blocks);
    quiescent(&raw);
    assert_eq!(row(&raw, "get.block_read").calls, 1);
    assert_eq!(row(&raw, "get.migration").calls, 2);
    assert_eq!(row(&raw, "body_read.migration").returned_bytes, 9);
    assert_eq!(row(&raw, "head.direct_delete").success, 1);
    assert_eq!(row(&raw, "delete.direct").success, 1);
    assert_eq!(row(&raw, "delete.reconcile").error, 1);
    assert_eq!(store.gets.load(Ordering::SeqCst), 3);
    assert_eq!(store.heads.load(Ordering::SeqCst), 1);
    assert_eq!(store.deletes.load(Ordering::SeqCst), 2);
    let local = local_snapshot(&blocks);
    local_quiescent(&local);
    assert_eq!(local_row(&local, "sha256.digest").calls, 3);
    assert_eq!(local_row(&local, "sha256.digest").input_bytes, 13);
    assert_eq!(local_row(&local, "block_id.encode").output_bytes, 3 * 65);
    assert_eq!(local_row(&local, "copy.cache_insert").calls, 1);
    assert_eq!(local_row(&local, "copy.cache_insert").output_bytes, 4);
    assert_eq!(local_row(&local, "copy.return_vec").calls, 4);
    assert_eq!(local_row(&local, "copy.return_vec").output_bytes, 17);
    assert_eq!(local_row(&local, "cache.lock_acquire").calls, 5);
    assert_eq!(local_row(&local, "copy.upload_payload").calls, 0);
}
#[tokio::test]
async fn backing_marker_calls_are_explicitly_outside_raw_bank() {
    let store = ApiStore::new();
    let blocks = adapter(store.clone(), true);
    let id = prepare_configured_backing_id(store.as_ref(), &blocks)
        .await
        .unwrap();
    verify_configured_backing_id(store.as_ref(), &blocks, id)
        .await
        .unwrap();
    let raw = snapshot(&blocks);
    quiescent(&raw);
    assert!(store.puts.load(Ordering::SeqCst) > 0);
    assert!(store.gets.load(Ordering::SeqCst) > 0);
    assert!(raw.entries.iter().all(|r| r.calls == 0));
    assert!(
        local_snapshot(&blocks)
            .entries
            .iter()
            .all(|row| row.calls == 0)
    );
}

const MARKER_METRIC_NAMES: [&str; 6] = [
    "object_store.backing_marker.probe.get",
    "object_store.backing_marker.probe.body_read",
    "object_store.backing_marker.data.get",
    "object_store.backing_marker.data.body_read",
    "object_store.backing_marker.probe.create",
    "object_store.backing_marker.retry_backoff",
];
type MarkerCounts = (u64, u64, u64, u64, u64); // calls, success, error, cancelled, bytes
const NO_MARKER_CALLS: MarkerCounts = (0, 0, 0, 0, 0);

fn isolated_marker_metrics(test: &str, expected_enabled: bool) -> bool {
    const CHILD: &str = "MOUNT_RS_MARKER_METRICS_TEST_CHILD";
    match std::env::var(CHILD) {
        Ok(child) => {
            assert_eq!(child, test, "the exact requested child must run");
            assert_eq!(storage_metrics::enabled(), expected_enabled);
            println!("MARKER_METRICS_CHILD_RUNNING test={test} enabled={expected_enabled}");
            true
        }
        Err(std::env::VarError::NotPresent) => {
            let filter = format!("raw_metrics_tests::{test}");
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", &filter, "--nocapture", "--test-threads=1"])
                .env(CHILD, test)
                .env(
                    "MOUNT_RS_PROFILE_IO",
                    if expected_enabled { "1" } else { "0" },
                )
                .env_remove("MOUNT_RS_TRACE_STORAGE")
                .output()
                .expect("run the owned marker metrics child");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stdout.contains("running 1 test"),
                "{filter}\n{stdout}\n{stderr}"
            );
            assert!(
                stdout.contains(&format!(
                    "MARKER_METRICS_CHILD_RUNNING test={test} enabled={expected_enabled}"
                )),
                "the named child did not execute: {filter}\n{stdout}\n{stderr}"
            );
            assert!(output.status.success(), "{filter}\n{stdout}\n{stderr}");
            assert!(
                stdout.contains("test result: ok. 1 passed"),
                "missing terminal child result: {filter}\n{stdout}\n{stderr}"
            );
            false
        }
        Err(error) => panic!("invalid marker metrics child selection: {error}"),
    }
}

fn marker_bytes(id: ConcurrentBackingId) -> Vec<u8> {
    let mut bytes = Vec::from(&b"MRC2"[..]);
    bytes.extend_from_slice(&id.as_bytes());
    bytes
}

async fn marker_clients(
    probe_bytes: Option<&[u8]>,
    data_bytes: Option<&[u8]>,
    enabled: bool,
) -> (Arc<ApiStore>, Arc<ApiStore>, ObjectStoreBlockStore) {
    let probe = ApiStore::new();
    let data = ApiStore::new();
    let blocks = if enabled {
        adapter(data.clone(), true)
    } else {
        ObjectStoreBlockStore::new(data.clone(), "private", false).unwrap()
    };
    // Fixture seeding bypasses the independent oracle, so its request/body
    // counts describe only actual marker methods invoked by the test.
    for (store, bytes) in [(&probe, probe_bytes), (&data, data_bytes)] {
        if let Some(bytes) = bytes {
            store
                .inner
                .put(&blocks.backing_id_path(), PutPayload::from(bytes.to_vec()))
                .await
                .unwrap();
        }
    }
    (probe, data, blocks)
}

fn marker_metric<'a>(
    snapshot: &'a storage_metrics::Snapshot,
    name: &str,
) -> &'a storage_metrics::Entry {
    snapshot
        .entries
        .iter()
        .find(|entry| entry.name == name)
        .unwrap_or_else(|| panic!("the fixed backing-marker metric must be exported: {name}"))
}

fn assert_marker_metrics(snapshot: &storage_metrics::Snapshot, expected: [MarkerCounts; 6]) {
    assert_eq!(snapshot.in_flight, 0);
    for (name, expected) in MARKER_METRIC_NAMES.into_iter().zip(expected) {
        let row = marker_metric(snapshot, name);
        assert_eq!(
            (row.calls, row.success, row.error, row.cancelled, row.bytes),
            expected,
            "{name}"
        );
        assert_eq!(row.in_flight, 0, "{name}");
        assert_eq!(row.calls, row.success + row.error + row.cancelled, "{name}");
        assert_eq!(row.calls, row.latency_log2_us.iter().sum::<u64>(), "{name}");
    }
}

fn assert_marker_oracle(
    snapshot: &storage_metrics::Snapshot,
    probe: &ApiStore,
    data: &ApiStore,
    blocks: &ObjectStoreBlockStore,
) {
    for (store, get, body) in [
        (probe, MARKER_METRIC_NAMES[0], MARKER_METRIC_NAMES[1]),
        (data, MARKER_METRIC_NAMES[2], MARKER_METRIC_NAMES[3]),
    ] {
        assert_eq!(
            marker_metric(snapshot, get).calls,
            store.gets.load(Ordering::SeqCst) as u64
        );
        assert_eq!(
            marker_metric(snapshot, body).calls,
            store.bodies.load(Ordering::SeqCst) as u64
        );
        assert_eq!(store.heads.load(Ordering::SeqCst), 0);
        assert_eq!(store.deletes.load(Ordering::SeqCst), 0);
    }
    assert_eq!(
        marker_metric(snapshot, MARKER_METRIC_NAMES[4]).calls,
        probe.puts.load(Ordering::SeqCst) as u64
    );
    assert_eq!(data.puts.load(Ordering::SeqCst), 0);
    assert_marker_payload_unobserved(blocks);
}

fn assert_marker_payload_unobserved(blocks: &ObjectStoreBlockStore) {
    let stats = blocks.stats();
    assert_eq!(
        (
            stats.puts,
            stats.gets,
            stats.bytes_written,
            stats.bytes_read
        ),
        (0, 0, 0, 0)
    );
    if let Some(raw) = stats.raw_api {
        quiescent(&raw);
        assert!(raw.entries.iter().all(|row| row.calls == 0));
        assert_eq!(
            (raw.claims.leader_claims, raw.claims.follower_claims),
            (0, 0)
        );
    }
    if let Some(local) = stats.local_work {
        local_quiescent(&local);
        assert!(local.entries.iter().all(|row| row.calls == 0));
    }
}

#[tokio::test]
async fn backing_marker_verification_observes_both_clients_without_payload_counts() {
    if !isolated_marker_metrics(
        "backing_marker_verification_observes_both_clients_without_payload_counts",
        true,
    ) {
        return;
    }
    let id = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
    let bytes = marker_bytes(id);
    let (probe, data, blocks) = marker_clients(Some(&bytes), Some(&bytes), true).await;
    let before = storage_metrics::snapshot();
    verify_configured_backing_id(probe.as_ref(), &blocks, id)
        .await
        .unwrap();
    let delta = storage_metrics::snapshot().delta(&before).unwrap();
    assert_marker_metrics(
        &delta,
        [
            (1, 1, 0, 0, 0),
            (1, 1, 0, 0, 20),
            (1, 1, 0, 0, 0),
            (1, 1, 0, 0, 20),
            NO_MARKER_CALLS,
            NO_MARKER_CALLS,
        ],
    );
    assert_marker_oracle(&delta, &probe, &data, &blocks);
}

#[tokio::test]
async fn backing_marker_authority_failures_preserve_estale_and_transport_outcomes() {
    if !isolated_marker_metrics(
        "backing_marker_authority_failures_preserve_estale_and_transport_outcomes",
        true,
    ) {
        return;
    }
    let id = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
    let other = ConcurrentBackingId::from_bytes([8; 16]).unwrap();
    let bytes = marker_bytes(id);
    let changed = marker_bytes(other);
    let complete = [
        (1, 1, 0, 0, 0),
        (1, 1, 0, 0, 20),
        (1, 1, 0, 0, 0),
        (1, 1, 0, 0, 20),
        NO_MARKER_CALLS,
        NO_MARKER_CALLS,
    ];
    for (name, first, second, expected_id, expected) in [
        (
            "expected mismatch",
            Some(bytes.as_slice()),
            Some(bytes.as_slice()),
            other,
            complete,
        ),
        (
            "client mismatch",
            Some(bytes.as_slice()),
            Some(changed.as_slice()),
            id,
            complete,
        ),
        (
            "absent",
            None,
            None,
            id,
            [
                (1, 0, 1, 0, 0),
                NO_MARKER_CALLS,
                (1, 0, 1, 0, 0),
                NO_MARKER_CALLS,
                NO_MARKER_CALLS,
                NO_MARKER_CALLS,
            ],
        ),
        (
            "malformed probe",
            Some(&b"bad!"[..]),
            Some(bytes.as_slice()),
            id,
            [
                (1, 1, 0, 0, 0),
                (1, 1, 0, 0, 4),
                NO_MARKER_CALLS,
                NO_MARKER_CALLS,
                NO_MARKER_CALLS,
                NO_MARKER_CALLS,
            ],
        ),
        (
            "malformed data",
            Some(bytes.as_slice()),
            Some(&b"bad!"[..]),
            id,
            [
                (1, 1, 0, 0, 0),
                (1, 1, 0, 0, 20),
                (1, 1, 0, 0, 0),
                (1, 1, 0, 0, 4),
                NO_MARKER_CALLS,
                NO_MARKER_CALLS,
            ],
        ),
    ] {
        let (probe, data, blocks) = marker_clients(first, second, true).await;
        let before = storage_metrics::snapshot();
        let error = verify_configured_backing_id(probe.as_ref(), &blocks, expected_id)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Estale, "{name}");
        assert_eq!(
            probe.puts.load(Ordering::SeqCst),
            0,
            "verification is read-only: {name}"
        );
        let delta = storage_metrics::snapshot().delta(&before).unwrap();
        assert_marker_metrics(&delta, expected);
        assert_marker_oracle(&delta, &probe, &data, &blocks);
    }
}

#[tokio::test]
async fn backing_marker_claim_observes_create_and_fresh_dual_client_reads() {
    if !isolated_marker_metrics(
        "backing_marker_claim_observes_create_and_fresh_dual_client_reads",
        true,
    ) {
        return;
    }
    let inner = Arc::new(InMemory::new());
    let probe = ApiStore::with_inner(inner.clone());
    let data = ApiStore::with_inner(inner);
    let blocks = adapter(data.clone(), true);
    let candidate = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
    let before = storage_metrics::snapshot();
    let actual = prepare_configured_backing_id_with_candidate(probe.as_ref(), &blocks, candidate)
        .await
        .unwrap();
    assert_eq!(actual, candidate);
    let delta = storage_metrics::snapshot().delta(&before).unwrap();
    assert_marker_metrics(
        &delta,
        [
            (2, 1, 1, 0, 0),
            (1, 1, 0, 0, 20),
            (2, 1, 1, 0, 0),
            (1, 1, 0, 0, 20),
            (1, 1, 0, 0, 20),
            NO_MARKER_CALLS,
        ],
    );
    assert_marker_oracle(&delta, &probe, &data, &blocks);
}

#[tokio::test]
async fn backing_marker_temporary_get_and_body_failures_observe_retry_backoff() {
    if !isolated_marker_metrics(
        "backing_marker_temporary_get_and_body_failures_observe_retry_backoff",
        true,
    ) {
        return;
    }
    let id = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
    let bytes = marker_bytes(id);
    for body_fault in [false, true] {
        let (probe, data, blocks) = marker_clients(Some(&bytes), Some(&bytes), true).await;
        probe
            .temporary_get_fault
            .store(!body_fault, Ordering::SeqCst);
        probe
            .temporary_body_fault
            .store(body_fault, Ordering::SeqCst);
        let before = storage_metrics::snapshot();
        let mut verify = Box::pin(verify_configured_backing_id(probe.as_ref(), &blocks, id));
        assert!(verify.as_mut().now_or_never().is_none());
        // A real one-shot failure reached the existing positive backoff. No
        // detached task, manual sleeping, or private retry helper is involved.
        assert_eq!(probe.gets.load(Ordering::SeqCst), 1);
        assert_eq!(probe.bodies.load(Ordering::SeqCst), usize::from(body_fault));
        assert_eq!(data.gets.load(Ordering::SeqCst), 0);
        assert_eq!(
            marker_metric(&storage_metrics::snapshot(), MARKER_METRIC_NAMES[5]).in_flight,
            1
        );
        tokio::time::timeout(Duration::from_secs(1), verify)
            .await
            .unwrap()
            .unwrap();
        let delta = storage_metrics::snapshot().delta(&before).unwrap();
        assert_marker_metrics(
            &delta,
            [
                if body_fault {
                    (2, 2, 0, 0, 0)
                } else {
                    (2, 1, 1, 0, 0)
                },
                if body_fault {
                    (2, 1, 1, 0, 20)
                } else {
                    (1, 1, 0, 0, 20)
                },
                (1, 1, 0, 0, 0),
                (1, 1, 0, 0, 20),
                NO_MARKER_CALLS,
                (1, 1, 0, 0, 0),
            ],
        );
        assert!(marker_metric(&delta, MARKER_METRIC_NAMES[5]).elapsed_ns > 0);
        assert_marker_oracle(&delta, &probe, &data, &blocks);
    }
}

#[tokio::test]
async fn backing_marker_permanent_get_and_body_failures_preserve_first_error() {
    if !isolated_marker_metrics(
        "backing_marker_permanent_get_and_body_failures_preserve_first_error",
        true,
    ) {
        return;
    }
    let id = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
    let bytes = marker_bytes(id);
    for body_fault in [false, true] {
        let (probe, data, blocks) = marker_clients(Some(&bytes), Some(&bytes), true).await;
        probe.get_fault.store(!body_fault, Ordering::SeqCst);
        probe.body_fault.store(body_fault, Ordering::SeqCst);
        let before = storage_metrics::snapshot();
        let error = verify_configured_backing_id(probe.as_ref(), &blocks, id)
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        assert_eq!(
            error.to_string(),
            "object-store backing identity read failed (Server)"
        );
        let delta = storage_metrics::snapshot().delta(&before).unwrap();
        assert_marker_metrics(
            &delta,
            [
                if body_fault {
                    (1, 1, 0, 0, 0)
                } else {
                    (1, 0, 1, 0, 0)
                },
                if body_fault {
                    (1, 0, 1, 0, 0)
                } else {
                    NO_MARKER_CALLS
                },
                NO_MARKER_CALLS,
                NO_MARKER_CALLS,
                NO_MARKER_CALLS,
                NO_MARKER_CALLS,
            ],
        );
        assert_marker_oracle(&delta, &probe, &data, &blocks);
    }
}

#[tokio::test]
async fn backing_marker_held_body_cancellation_and_unpolled_futures_are_distinct() {
    if !isolated_marker_metrics(
        "backing_marker_held_body_cancellation_and_unpolled_futures_are_distinct",
        true,
    ) {
        return;
    }
    let id = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
    let bytes = marker_bytes(id);
    let (probe, data, blocks) = marker_clients(Some(&bytes), Some(&bytes), true).await;
    let before = storage_metrics::snapshot();
    drop(verify_configured_backing_id(probe.as_ref(), &blocks, id));
    drop(prepare_configured_backing_id_with_candidate(
        probe.as_ref(),
        &blocks,
        id,
    ));
    assert_eq!(probe.gets.load(Ordering::SeqCst), 0);
    assert_eq!(probe.puts.load(Ordering::SeqCst), 0);
    assert!(
        storage_metrics::snapshot()
            .delta(&before)
            .unwrap()
            .entries
            .iter()
            .all(|row| row.calls == 0 && row.in_flight == 0)
    );
    probe.hold_body.store(true, Ordering::SeqCst);
    let mut verify = Box::pin(verify_configured_backing_id(probe.as_ref(), &blocks, id));
    assert!(verify.as_mut().now_or_never().is_none());
    assert_eq!(probe.gets.load(Ordering::SeqCst), 1);
    assert_eq!(probe.bodies.load(Ordering::SeqCst), 1);
    assert_eq!(data.gets.load(Ordering::SeqCst), 0);
    let held = storage_metrics::snapshot();
    assert_eq!(marker_metric(&held, MARKER_METRIC_NAMES[0]).success, 1);
    let body = marker_metric(&held, MARKER_METRIC_NAMES[1]);
    assert_eq!((body.calls, body.in_flight), (0, 1));
    assert_eq!(held.in_flight, 1);
    drop(verify);
    let delta = storage_metrics::snapshot().delta(&before).unwrap();
    assert_marker_metrics(
        &delta,
        [
            (1, 1, 0, 0, 0),
            (1, 0, 0, 1, 0),
            NO_MARKER_CALLS,
            NO_MARKER_CALLS,
            NO_MARKER_CALLS,
            NO_MARKER_CALLS,
        ],
    );
    assert_marker_oracle(&delta, &probe, &data, &blocks);
}

#[tokio::test]
async fn backing_marker_metrics_exclude_configured_prefix_preflight() {
    if !isolated_marker_metrics(
        "backing_marker_metrics_exclude_configured_prefix_preflight",
        true,
    ) {
        return;
    }
    let store = ApiStore::new();
    let blocks = adapter(store.clone(), true);
    let before = storage_metrics::snapshot();
    probe_configured_concurrent_prefix(store.as_ref(), "private")
        .await
        .unwrap();
    assert_eq!(store.puts.load(Ordering::SeqCst), 1);
    assert_eq!(store.gets.load(Ordering::SeqCst), 2);
    assert_eq!(store.bodies.load(Ordering::SeqCst), 1);
    assert_marker_metrics(
        &storage_metrics::snapshot().delta(&before).unwrap(),
        [NO_MARKER_CALLS; 6],
    );
    assert_marker_payload_unobserved(&blocks);
}

#[tokio::test]
async fn backing_marker_disabled_profile_preserves_authority_and_zero_observations() {
    if !isolated_marker_metrics(
        "backing_marker_disabled_profile_preserves_authority_and_zero_observations",
        false,
    ) {
        return;
    }
    let id = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
    let other = ConcurrentBackingId::from_bytes([8; 16]).unwrap();
    let bytes = marker_bytes(id);
    for (first, second, expected, succeeds) in [
        (Some(bytes.as_slice()), Some(bytes.as_slice()), id, true),
        (Some(bytes.as_slice()), Some(bytes.as_slice()), other, false),
        (None, None, id, false),
        (Some(&b"bad!"[..]), Some(bytes.as_slice()), id, false),
    ] {
        let (probe, data, blocks) = marker_clients(first, second, false).await;
        let before = storage_metrics::snapshot();
        let result = verify_configured_backing_id(probe.as_ref(), &blocks, expected).await;
        if succeeds {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().code, ErrorCode::Estale);
        }
        assert_eq!(probe.gets.load(Ordering::SeqCst), 1);
        assert_eq!(
            data.gets.load(Ordering::SeqCst),
            usize::from(first != Some(&b"bad!"[..]))
        );
        assert_eq!(probe.puts.load(Ordering::SeqCst), 0);
        let delta = storage_metrics::snapshot().delta(&before).unwrap();
        assert_marker_metrics(&delta, [NO_MARKER_CALLS; 6]);
        for row in &delta.entries {
            assert_eq!(
                (row.calls, row.bytes, row.elapsed_ns, row.in_flight),
                (0, 0, 0, 0)
            );
        }
        assert!(blocks.stats().raw_api.is_none());
        assert!(blocks.stats().local_work.is_none());
        assert_marker_payload_unobserved(&blocks);
    }
}

#[tokio::test]
async fn disabled_injection_keeps_behavior_and_raw_unavailable() {
    let store = ApiStore::new();
    let blocks = adapter(store.clone(), false);
    let id = blocks.put(b"disabled").await.unwrap();
    assert_eq!(blocks.get(&id).await.unwrap(), b"disabled");
    assert!(blocks.stats().raw_api.is_none());
    assert!(blocks.stats().local_work.is_none());
    assert_eq!(store.puts.load(Ordering::SeqCst), 1);
}
#[test]
#[ignore = "isolated process; MOUNT_RS_PROFILE_IO=1 or unset"]
fn public_constructor_caches_actual_profile_enablement() {
    let blocks = ObjectStoreBlockStore::new(Arc::new(InMemory::new()), "public", false).unwrap();
    assert_eq!(
        blocks.stats().raw_api.is_some(),
        std::env::var_os("MOUNT_RS_PROFILE_IO").is_some_and(|v| v == "1")
    );
    assert_eq!(
        blocks.stats().local_work.is_some(),
        blocks.stats().raw_api.is_some()
    );
}

// Count only this test thread. The positive control establishes that the
// allocator actually observes heap work before checking recorder updates.
mod allocation_control {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    std::thread_local! {static TRACK:Cell<bool>=const{Cell::new(false)};static COUNT:Cell<usize>=const{Cell::new(0)};}
    pub struct Allocator;
    unsafe impl GlobalAlloc for Allocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let _ = TRACK.try_with(|flag| {
                if flag.get() {
                    let _ = COUNT.try_with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.alloc(layout) }
        }
        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            unsafe { System.dealloc(pointer, layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            let _ = TRACK.try_with(|flag| {
                if flag.get() {
                    let _ = COUNT.try_with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            let _ = TRACK.try_with(|flag| {
                if flag.get() {
                    let _ = COUNT.try_with(|count| count.set(count.get() + 1));
                }
            });
            unsafe { System.realloc(pointer, layout, size) }
        }
    }
    pub fn count<T>(work: impl FnOnce() -> T) -> (T, usize) {
        COUNT.with(|x| x.set(0));
        TRACK.with(|x| x.set(true));
        let result = work();
        TRACK.with(|x| x.set(false));
        (result, COUNT.with(Cell::get))
    }
}
#[global_allocator]
static ALLOCATOR: allocation_control::Allocator = allocation_control::Allocator;
#[test]
fn recorder_updates_allocate_nothing_enabled_or_disabled_with_positive_control() {
    let (bank, startup) = allocation_control::count(|| Box::new(RawState::default()));
    assert!(startup > 0);
    let (_, positive) = allocation_control::count(|| std::hint::black_box(vec![1_u8; 128]));
    assert!(positive > 0);
    for state in [None, Some(bank.as_ref())] {
        let (_, allocations) = allocation_control::count(|| {
            for _ in 0..20 {
                let mut raw = RawSpan::new(state, Api::Put, 12);
                raw.result(&Ok::<_, object_store::Error>(()), 12, 0);
                let mut claim = ClaimSpan::new(state, Claim::Leader);
                claim.result(&Ok::<_, FsError>(()));
                let mut local =
                    LocalSpan::new(state.map(|state| &state.local), Local::UploadCopy, 12);
                local.success(12);
                let mut wait =
                    LocalSpan::new(state.map(|state| &state.local), Local::FollowerWait, 0);
                wait.result(&Err::<(), ()>(()), 0);
                drop(LocalSpan::new(
                    state.map(|state| &state.local),
                    Local::FollowerWait,
                    0,
                ));
            }
        });
        assert_eq!(allocations, 0);
    }
    let (_, snapshots) = allocation_control::count(|| bank.snapshot());
    assert_eq!(snapshots, 0);
    let (_, local_snapshots) = allocation_control::count(|| bank.local.snapshot());
    assert_eq!(local_snapshots, 0);
    eprintln!(
        "RAW_METRICS_ALLOCATION_CONTROL enabled_startup_allocations={startup} positive_control_allocations={positive} enabled_update_allocations=0 disabled_update_allocations=0 fixed_raw_snapshot_allocations=0 fixed_local_snapshot_allocations=0 raw_span_size={} claim_span_size={} raw_bank_size={} local_span_size={} local_bank_size={}",
        std::mem::size_of::<RawSpan<'_>>(),
        std::mem::size_of::<ClaimSpan<'_>>(),
        std::mem::size_of::<RawState>(),
        std::mem::size_of::<LocalSpan<'_>>(),
        std::mem::size_of::<LocalState>()
    );
}
#[tokio::test]
async fn integrity_failure_keeps_raw_success_bytes_and_unpolled_drop_has_no_calls() {
    let store = ApiStore::new();
    let id = store.preseed(b"expected", b"corrupt!").await;
    let blocks = adapter(store.clone(), true);
    drop(blocks.put(b"unpolled"));
    assert!(snapshot(&blocks).entries.iter().all(|r| r.calls == 0));
    assert!(local_snapshot(&blocks).entries.iter().all(|r| r.calls == 0));
    assert!(blocks.get(&id).await.is_err());
    assert!(blocks.get_for_migration(&id).await.is_err());
    let raw = snapshot(&blocks);
    quiescent(&raw);
    assert_eq!(row(&raw, "get.block_read").success, 1);
    assert_eq!(row(&raw, "body_read.block_read").returned_bytes, 8);
    assert_eq!(row(&raw, "body_read.migration").returned_bytes, 8);
    assert!(blocks.cache.get(&id.0).is_none());
    let local = local_snapshot(&blocks);
    local_quiescent(&local);
    assert_eq!(local_row(&local, "sha256.digest").success, 2);
    assert_eq!(local_row(&local, "sha256.digest").output_bytes, 64);
    assert_eq!(local_row(&local, "copy.cache_insert").calls, 0);
    assert_eq!(local_row(&local, "copy.return_vec").calls, 0);
    assert!(blocks.delete(&BlockId(block_id(b"missing"))).await.is_err());
    let raw = snapshot(&blocks);
    quiescent(&raw);
    assert_eq!(row(&raw, "head.direct_delete").error, 1);
    assert_eq!(row(&raw, "delete.direct").calls, 0);
}

#[tokio::test]
async fn poisoned_cache_acquisition_counts_error_and_preserves_remote_fallback() {
    let store = ApiStore::new();
    let id = store.preseed(b"fallback", b"fallback").await;
    let blocks = adapter(store.clone(), true);
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = blocks.cache.state.lock().unwrap();
        panic!("owned cache poison control");
    }));
    assert!(poisoned.is_err());
    assert_eq!(blocks.get(&id).await.unwrap(), b"fallback");
    let local = local_snapshot(&blocks);
    local_quiescent(&local);
    let lock = local_row(&local, "cache.lock_acquire");
    assert_eq!((lock.calls, lock.error, lock.success), (2, 2, 0));
    assert_eq!(local_row(&local, "copy.cache_insert").calls, 0);
    assert_eq!(local_row(&local, "copy.return_vec").output_bytes, 8);
    assert_eq!(row(&snapshot(&blocks), "get.block_read").success, 1);
    assert_eq!(store.gets.load(Ordering::SeqCst), 1);
}

#[test]
fn held_real_cache_mutex_keeps_local_span_active_until_acquisition() {
    let blocks = adapter(ApiStore::new(), true);
    let guard = blocks.cache.state.lock().unwrap();
    let reader = blocks.clone();
    let worker =
        std::thread::spawn(move || reader.cache.get_with_metrics("missing", reader.local()));
    let started = std::time::Instant::now();
    let held = loop {
        let local = local_snapshot(&blocks);
        if local.in_flight == 1 || started.elapsed() > Duration::from_secs(5) {
            break local;
        }
        std::thread::yield_now();
    };
    drop(guard);
    assert!(worker.join().unwrap().is_none());
    assert_eq!(held.in_flight, 1);
    assert_eq!(local_row(&held, "cache.lock_acquire").success, 0);
    let local = local_snapshot(&blocks);
    local_quiescent(&local);
    let lock = local_row(&local, "cache.lock_acquire");
    assert_eq!((lock.calls, lock.success), (1, 1));
    assert!(lock.elapsed_ns > 0);
    assert_eq!(lock.latency_max_ns, lock.elapsed_ns);
    assert_eq!((lock.input_bytes, lock.output_bytes), (0, 0));
    assert_eq!(local_row(&local, "copy.return_vec").calls, 0);
}

#[tokio::test]
async fn legacy_migration_copy_does_not_invent_digest_or_cache_work() {
    let store = ApiStore::new();
    let id = BlockId("b0123456789abcdef0123456789abcdef".to_owned());
    store
        .inner
        .put(
            &ObjectPath::from(format!("private/{}", id.0)),
            PutPayload::from(b"legacy".to_vec()),
        )
        .await
        .unwrap();
    let blocks = adapter(store, true);
    assert_eq!(blocks.get_for_migration(&id).await.unwrap(), b"legacy");
    let local = local_snapshot(&blocks);
    local_quiescent(&local);
    assert_eq!(local_row(&local, "copy.return_vec").calls, 1);
    assert_eq!(local_row(&local, "copy.return_vec").output_bytes, 6);
    for name in [
        "sha256.digest",
        "block_id.encode",
        "cache.lock_acquire",
        "copy.cache_insert",
        "copy.upload_payload",
    ] {
        assert_eq!(local_row(&local, name).calls, 0);
    }
}
