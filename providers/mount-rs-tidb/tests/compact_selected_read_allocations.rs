//! Ignored actual-TiDB Stage 0 controls. Root must bind the original owned
//! fixture before execution. These use TiDB for metadata AND blocks, not RustFS.
//! The current-thread Rust meter includes driver/runtime/background activity
//! during the window; it excludes foreign/server allocation and is not RSS.
//! Whole-parent bytes are still returned, even if JSON tree work is removed.

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::FsDriver;
use mount_rs_core::storage::compact::{CompactSnapshot, CompactStructuralDelta, StructuralScope};
use mount_rs_core::storage::{DirectoryEntry, MetadataStore, NodeData};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore};
use mysql_async::{Pool, prelude::Queryable};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const CONTENT: &[u8] = b"selected-read";
const TARGET: &str = "/file-0000";
const MAX_CALLS: u64 = 256;

#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    calls: u64,
    requested_bytes: u64,
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
async fn read_cycle(fs: &ChunkedFs<TidbMetadataStore, TidbBlockStore>, inode: u64) {
    let handle = FsDriver::open(fs, TARGET, "r", 0).await.unwrap();
    assert_eq!(handle.stat().await.unwrap().ino, inode);
    let mut bytes = [0; CONTENT.len()];
    assert_eq!(
        handle.read(&mut bytes, Some(0)).await.unwrap(),
        CONTENT.len()
    );
    assert_eq!(bytes.as_slice(), CONTENT);
    handle.close().await.unwrap();
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
    read_cycle(&opened.fs, inode).await;
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
        "mount_rs_tidb_compact_guards",
        "mount_rs_tidb_metadata",
        "mount_rs_tidb_blocks",
        "mount_rs_tidb_block_authority",
    ] {
        conn.exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (&key,))
            .await
            .unwrap();
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
    assert!(
        measured.calls <= MAX_CALLS,
        "actual TiDB selected open/read/close at {siblings} siblings still materializes JSON trees: {measured:?}; call ceiling={MAX_CALLS}; receive/SQL bytes remain O(parent)"
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
