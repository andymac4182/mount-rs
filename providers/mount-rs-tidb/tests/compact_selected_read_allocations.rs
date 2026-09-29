//! Ignored actual-TiDB Stage 0 controls. Root must bind the original owned
//! fixture before execution. These use TiDB for metadata AND blocks, not RustFS.
//! The current-thread Rust meter includes driver/runtime/background activity
//! during the window; it excludes foreign/server allocation and is not RSS.
//! The default compact layout is indexed MRC5. This cycle includes Stat's
//! complete-root validation as well as point-selected Open and Read operations.
//! Full snapshot setup and audits remain outside the measured window.

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::FsDriver;
use mount_rs_core::diagnostics::storage;
use mount_rs_core::storage::compact::{CompactSnapshot, CompactStructuralDelta, StructuralScope};
use mount_rs_core::storage::{DirectoryEntry, MetadataStore, NodeData};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore};
use mysql_async::{Pool, prelude::Queryable};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CONTENT: &[u8] = b"selected-read";
const TARGET: &str = "/file-0000";
const MAX_CALLS: u64 = 256;
const PERFORMANCE_WARMUPS: usize = 4;
const PERFORMANCE_SAMPLES: usize = 32;

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
struct Counts {
    calls: u64,
    requested_bytes: u64,
}
#[derive(Clone, Copy, Debug, serde::Serialize)]
struct PhaseCounts {
    counts: Counts,
    elapsed_ns: u128,
}
thread_local! {
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<Counts> = const { Cell::new(Counts { calls: 0, requested_bytes: 0 }) };
}
fn record(bytes: usize) {
    let _ = BUSY.try_with(|busy| {
        if busy.replace(true) {
            return;
        }
        let _ = RECORDING.try_with(|active| {
            if active.get() {
                let _ = COUNTS.try_with(|counts| {
                    let old = counts.get();
                    counts.set(Counts {
                        calls: old.calls.saturating_add(1),
                        requested_bytes: old.requested_bytes.saturating_add(bytes as u64),
                    });
                });
            }
        });
        busy.set(false);
    });
}
struct Meter;
unsafe impl GlobalAlloc for Meter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Meter = Meter;
struct Window;
impl Window {
    fn begin() -> Self {
        assert!(!RECORDING.with(Cell::get));
        COUNTS.with(|counts| counts.set(Counts::default()));
        RECORDING.with(|active| active.set(true));
        Self
    }
    fn finish(self) -> Counts {
        drop(self);
        COUNTS.with(Cell::get)
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        RECORDING.with(|active| active.set(false));
    }
}
// Counter reads, fixed-size values, and Instant snapshots allocate no Rust heap.
// The recording flag remains enabled for the same overall measurement window.
fn phase_counts(previous: &mut Counts, started: Instant) -> PhaseCounts {
    let current = COUNTS.with(Cell::get);
    let phase = PhaseCounts {
        counts: Counts {
            calls: current.calls.saturating_sub(previous.calls),
            requested_bytes: current
                .requested_bytes
                .saturating_sub(previous.requested_bytes),
        },
        elapsed_ns: started.elapsed().as_nanos(),
    };
    *previous = current;
    phase
}

struct Opened {
    fs: ChunkedFs<TidbMetadataStore, TidbBlockStore>,
    metadata: TidbMetadataStore,
    blocks: TidbBlockStore,
}
async fn open(url: &str, key: &str, owner: &str) -> Opened {
    let metadata = TidbMetadataStore::connect_with_key(url, key).await.unwrap();
    let blocks = TidbBlockStore::connect_with_key(url, key).await.unwrap();
    let fs = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed(owner, 4096)
            .unwrap()
            .with_compact_inode_updates(true),
    )
    .await
    .unwrap();
    Opened {
        fs,
        metadata,
        blocks,
    }
}
async fn close(opened: Opened) {
    opened.fs.shutdown().await.unwrap();
    drop(opened.fs);
    opened.metadata.close().await.unwrap();
    opened.blocks.close().await.unwrap();
}
fn entries(snapshot: &CompactSnapshot) -> &[DirectoryEntry] {
    let NodeData::Directory { entries } = &snapshot.guards[&snapshot.anchor.root].node.data else {
        panic!("root directory required")
    };
    entries
}
async fn read_cycle(
    fs: &ChunkedFs<TidbMetadataStore, TidbBlockStore>,
    inode: u64,
) -> [PhaseCounts; 4] {
    let mut previous = COUNTS.with(Cell::get);
    let started = Instant::now();
    let handle = FsDriver::open(fs, TARGET, "r", 0).await.unwrap();
    let open = phase_counts(&mut previous, started);
    let started = Instant::now();
    assert_eq!(handle.stat().await.unwrap().ino, inode);
    let stat = phase_counts(&mut previous, started);
    let started = Instant::now();
    let mut bytes = [0; CONTENT.len()];
    assert_eq!(
        handle.read(&mut bytes, Some(0)).await.unwrap(),
        CONTENT.len()
    );
    assert_eq!(bytes.as_slice(), CONTENT);
    let read = phase_counts(&mut previous, started);
    let started = Instant::now();
    handle.close().await.unwrap();
    let close = phase_counts(&mut previous, started);
    [open, stat, read, close]
}

async fn check(siblings: usize) {
    let url =
        std::env::var("MOUNT_RS_TIDB_URL").expect("explicit root-owned TiDB fixture required");
    let key = format!(
        "compact-selected-read-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let setup = open(&url, &key, "selected-read-seed").await;
    setup.fs.write_file(TARGET, CONTENT).await.unwrap();
    let backing = setup.fs.concurrent_backing_id().unwrap();
    let base = setup.metadata.load_compact_snapshot(backing).await.unwrap();
    let mut namespace = base.namespace().unwrap();
    let inode = entries(&base)[0].inode;
    let template = namespace.nodes[&inode].clone();
    for index in 1..siblings {
        let created = namespace.next_inode;
        namespace.next_inode += 1;
        let mut node = template.clone();
        node.stats.ino = created;
        namespace.nodes.insert(created, node);
        let NodeData::Directory { entries } =
            &mut namespace.nodes.get_mut(&namespace.root).unwrap().data
        else {
            unreachable!()
        };
        entries.push(DirectoryEntry {
            name: format!("file-{index:04}"),
            inode: created,
        });
    }
    let NodeData::Directory {
        entries: root_entries,
    } = &mut namespace.nodes.get_mut(&namespace.root).unwrap().data
    else {
        unreachable!()
    };
    root_entries.reverse();
    let delta = CompactStructuralDelta::capture(&base, &namespace, StructuralScope::Full).unwrap();
    setup
        .metadata
        .publish_compact_structure(&delta)
        .await
        .unwrap();
    let seeded = setup.metadata.load_compact_snapshot(backing).await.unwrap();
    assert_eq!(seeded.namespace().unwrap().nodes.len(), siblings + 1);
    close(setup).await;

    let opened = open(&url, &key, "selected-read-measured").await;
    read_cycle(&opened.fs, inode).await;
    let root = &seeded.guards[&seeded.anchor.root].node;
    let window = Window::begin();
    let cloned = std::hint::black_box(root.clone());
    let control = window.finish();
    assert_eq!(cloned, *root);
    assert!(control.calls > siblings as u64, "{control:?}");
    drop(cloned);
    let window = Window::begin();
    let phases = read_cycle(&opened.fs, inode).await;
    let measured = window.finish();
    assert!(!opened.fs.failed());
    close(opened).await;

    // Fresh independent providers/Full audit/order/inode/byte oracle run before
    // the intended performance assertion. No failing read is counted as fast.
    let oracle = open(&url, &key, "selected-read-oracle").await;
    let fresh = oracle
        .metadata
        .load_compact_snapshot(backing)
        .await
        .unwrap();
    assert_eq!(fresh.anchor, seeded.anchor);
    assert_eq!(
        fresh.guards[&seeded.anchor.root],
        seeded.guards[&seeded.anchor.root]
    );
    assert_eq!(entries(&fresh), entries(&seeded));
    read_cycle(&oracle.fs, inode).await;
    close(oracle).await;

    // Exact owned logical key cleanup, after every provider has closed. Do not
    // clear tables or a caller-selected key; a failed earlier oracle retains its
    // fresh diagnostic key for root inspection instead of pretending success.
    let pool = Pool::from_url(&url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    for table in [
        "mount_rs_tidb_compact_dentries",
        "mount_rs_tidb_compact_guards",
        "mount_rs_tidb_compact_members",
        "mount_rs_tidb_inodes",
        "mount_rs_tidb_metadata",
        "mount_rs_tidb_blocks",
        "mount_rs_tidb_block_authority",
    ] {
        conn.exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (&key,))
            .await
            .unwrap();
        let remaining: Option<u64> = conn
            .exec_first(
                format!("SELECT COUNT(*) FROM {table} WHERE volume_key=?"),
                (&key,),
            )
            .await
            .unwrap();
        assert_eq!(remaining, Some(0), "owned key rows remain in {table}");
    }
    drop(conn);
    pool.disconnect().await.unwrap();
    assert!(
        measured.calls > 0 && measured.requested_bytes > 0,
        "missing measurement cannot count as zero"
    );
    println!(
        "compact_selected_read_allocations {{\"backend\":\"tidb-metadata-and-blocks\",\"siblings\":{siblings},\"rust_calls\":{},\"rust_requested_bytes\":{},\"driver_and_runtime_included\":true,\"foreign_allocations_included\":false,\"fresh_oracles_passed\":true}}",
        measured.calls, measured.requested_bytes
    );
    for (operation, phase) in ["Open", "Stat", "Read", "Close"].into_iter().zip(phases) {
        println!(
            "compact_selected_read_allocations_phase {{\"backend\":\"tidb-metadata-and-blocks\",\"siblings\":{siblings},\"operation\":\"{operation}\",\"rust_calls\":{},\"rust_requested_bytes\":{},\"elapsed_ns\":{}}}",
            phase.counts.calls, phase.counts.requested_bytes, phase.elapsed_ns
        );
    }
    assert!(
        measured.calls <= MAX_CALLS,
        "actual TiDB selected open/stat/read/close at {siblings} siblings exceeds the Rust allocation bound: {measured:?}; call ceiling={MAX_CALLS}; cycle includes complete-root Stat validation"
    );
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires original root-owned TiDB fixture and explicit MOUNT_RS_TIDB_URL"]
async fn actual_tidb_fresh_selected_128_siblings_has_bounded_rust_allocations() {
    tokio::time::timeout(Duration::from_secs(180), check(128))
        .await
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires original root-owned TiDB fixture and explicit MOUNT_RS_TIDB_URL"]
async fn actual_tidb_fresh_selected_1000_siblings_has_bounded_rust_allocations() {
    tokio::time::timeout(Duration::from_secs(180), check(1000))
        .await
        .unwrap();
}

#[derive(serde::Serialize)]
struct StorageCounts {
    calls: u64,
    success: u64,
    error: u64,
    cancelled: u64,
    in_flight: u64,
    returned_rows: u64,
    returned_row_observations: u64,
    elapsed_ns: u64,
}

fn storage_counts(snapshot: &storage::Snapshot, name: &str) -> StorageCounts {
    let entry = snapshot
        .entries
        .iter()
        .find(|entry| entry.name == name)
        .expect("storage operation must be present in the diagnostic snapshot");
    StorageCounts {
        calls: entry.calls,
        success: entry.success,
        error: entry.error,
        cancelled: entry.cancelled,
        in_flight: entry.in_flight,
        returned_rows: entry.returned_rows,
        returned_row_observations: entry.returned_row_observations,
        elapsed_ns: entry.elapsed_ns,
    }
}

#[derive(serde::Serialize)]
struct PerformanceSample {
    index: usize,
    elapsed_ns: u128,
    rust_calls: u64,
    rust_requested_bytes: u64,
    phases: [PhaseCounts; 4],
    tidb_sql_inode_read: StorageCounts,
    tidb_sql_metadata_read: StorageCounts,
    tidb_sql_block_read: StorageCounts,
    tidb_tx_rollback: StorageCounts,
    tidb_pool_checkout: StorageCounts,
    tidb_session_configure: StorageCounts,
}

// Repeated performance samples are a separate benchmark. The original controls
// above retain their one warmup, one measured cycle, and unchanged call ceiling.
async fn performance_case(siblings: usize) -> serde_json::Value {
    let url =
        std::env::var("MOUNT_RS_TIDB_URL").expect("explicit root-owned TiDB fixture required");
    let key = format!(
        "compact-selected-performance-{siblings}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let setup = open(&url, &key, "selected-read-seed").await;
    setup.fs.write_file(TARGET, CONTENT).await.unwrap();
    let backing = setup.fs.concurrent_backing_id().unwrap();
    let base = setup.metadata.load_compact_snapshot(backing).await.unwrap();
    let mut namespace = base.namespace().unwrap();
    let inode = entries(&base)[0].inode;
    let template = namespace.nodes[&inode].clone();
    for index in 1..siblings {
        let created = namespace.next_inode;
        namespace.next_inode += 1;
        let mut node = template.clone();
        node.stats.ino = created;
        namespace.nodes.insert(created, node);
        let NodeData::Directory { entries } =
            &mut namespace.nodes.get_mut(&namespace.root).unwrap().data
        else {
            unreachable!()
        };
        entries.push(DirectoryEntry {
            name: format!("file-{index:04}"),
            inode: created,
        });
    }
    let NodeData::Directory {
        entries: root_entries,
    } = &mut namespace.nodes.get_mut(&namespace.root).unwrap().data
    else {
        unreachable!()
    };
    root_entries.reverse();
    let delta = CompactStructuralDelta::capture(&base, &namespace, StructuralScope::Full).unwrap();
    setup
        .metadata
        .publish_compact_structure(&delta)
        .await
        .unwrap();
    let seeded = setup.metadata.load_compact_snapshot(backing).await.unwrap();
    assert_eq!(seeded.namespace().unwrap().nodes.len(), siblings + 1);
    close(setup).await;

    let opened = open(&url, &key, "selected-read-performance-measured").await;
    for _ in 0..PERFORMANCE_WARMUPS {
        read_cycle(&opened.fs, inode).await;
    }
    let mut samples = Vec::with_capacity(PERFORMANCE_SAMPLES);
    for index in 0..PERFORMANCE_SAMPLES {
        // Diagnostic snapshots, deltas, sample storage, and JSON construction
        // allocate only outside the same single allocation window for this cycle.
        let before = storage::snapshot();
        let window = Window::begin();
        let started = Instant::now();
        let phases = read_cycle(&opened.fs, inode).await;
        let elapsed_ns = started.elapsed().as_nanos();
        let measured = window.finish();
        let after = storage::snapshot();
        assert!(!opened.fs.failed());
        assert!(
            measured.calls > 0 && measured.requested_bytes > 0,
            "missing measurement cannot count as zero"
        );
        let observed = after
            .delta(&before)
            .expect("storage counters must not reset");
        let inode_read = storage_counts(&observed, "tidb.sql.inode_read");
        assert!(
            inode_read.calls > 0
                && inode_read.success == inode_read.calls
                && inode_read.returned_row_observations > 0
                && inode_read.in_flight == 0
                && inode_read.error == 0
                && inode_read.cancelled == 0,
            "inode-read diagnostic coverage must be complete"
        );
        samples.push(PerformanceSample {
            index,
            elapsed_ns,
            rust_calls: measured.calls,
            rust_requested_bytes: measured.requested_bytes,
            phases,
            tidb_sql_inode_read: inode_read,
            tidb_sql_metadata_read: storage_counts(&observed, "tidb.sql.metadata_read"),
            tidb_sql_block_read: storage_counts(&observed, "tidb.sql.block_read"),
            tidb_tx_rollback: storage_counts(&observed, "tidb.tx.rollback"),
            tidb_pool_checkout: storage_counts(&observed, "tidb.pool.checkout"),
            tidb_session_configure: storage_counts(&observed, "tidb.session.configure"),
        });
    }
    close(opened).await;

    // A fresh complete audit and byte/inode oracle precede successful cleanup.
    // Any earlier failure retains this locally generated diagnostic dataset.
    let oracle = open(&url, &key, "selected-read-performance-oracle").await;
    let fresh = oracle
        .metadata
        .load_compact_snapshot(backing)
        .await
        .unwrap();
    assert_eq!(fresh.anchor, seeded.anchor);
    assert_eq!(
        fresh.guards[&seeded.anchor.root],
        seeded.guards[&seeded.anchor.root]
    );
    assert_eq!(entries(&fresh), entries(&seeded));
    read_cycle(&oracle.fs, inode).await;
    close(oracle).await;

    let pool = Pool::from_url(&url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    for table in [
        "mount_rs_tidb_compact_dentries",
        "mount_rs_tidb_compact_guards",
        "mount_rs_tidb_compact_members",
        "mount_rs_tidb_inodes",
        "mount_rs_tidb_metadata",
        "mount_rs_tidb_blocks",
        "mount_rs_tidb_block_authority",
    ] {
        conn.exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (&key,))
            .await
            .unwrap();
        let remaining: Option<u64> = conn
            .exec_first(
                format!("SELECT COUNT(*) FROM {table} WHERE volume_key=?"),
                (&key,),
            )
            .await
            .unwrap();
        assert_eq!(remaining, Some(0), "owned key rows remain in {table}");
    }
    drop(conn);
    pool.disconnect().await.unwrap();

    serde_json::json!({
        "siblings": siblings,
        "samples": samples,
        "fresh_full_anchor_root_order_and_byte_oracles_passed": true,
        "actors_closed": true,
        "owned_key_cleanup_verified": true,
        "owned_row_families": 7
    })
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "requires original root-owned TiDB fixture, MOUNT_RS_TIDB_URL, and MOUNT_RS_PROFILE_IO=1"]
async fn actual_tidb_fresh_selected_cycle_performance_samples() {
    assert!(
        storage::enabled(),
        "benchmark requires MOUNT_RS_PROFILE_IO=1"
    );
    let small = tokio::time::timeout(Duration::from_secs(180), performance_case(128))
        .await
        .unwrap();
    let large = tokio::time::timeout(Duration::from_secs(180), performance_case(1000))
        .await
        .unwrap();
    // The one report is emitted after both independent cases have closed every
    // actor and verified zero rows in all seven exact owned-key row families.
    println!(
        "compact_selected_read_cycle_performance {}",
        serde_json::json!({
            "schema": "compact-selected-read-cycle-performance-v1",
            "backend": "tidb-metadata-and-blocks",
            "warmups_per_case": PERFORMANCE_WARMUPS,
            "timed_samples_per_case": PERFORMANCE_SAMPLES,
            "phase_order": ["Open", "Stat", "Read", "Close"],
            "allocation_scope": "current-thread Rust allocation requests; driver/runtime included; foreign threads and server excluded; not RSS",
            "latency_scope": "exact Open/Stat/Read/Close read_cycle; setup, warmup, diagnostic snapshots/deltas, sample storage, oracle, cleanup and reporting excluded",
            "original_control_call_ceiling": MAX_CALLS,
            "enforces_original_control_call_ceiling": false,
            "cases": [small, large]
        })
    );
}
