//! Measures only the synchronous next-node construction in two actual write
//! paths. Provider/rewrite/async allocations are deliberately outside this
//! window; this is not a zero-allocation claim for all metadata or a write.

use super::*;
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PreparationPath {
    HandleWrite,
    WholeFileReplace,
    MeterControl,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct AllocationCounts {
    calls: u64,
    requested_bytes: u64,
}

thread_local! {
    static BOOKKEEPING: Cell<bool> = const { Cell::new(false) };
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static REQUESTED_PATH: Cell<Option<PreparationPath>> = const { Cell::new(None) };
    static COUNTS: Cell<AllocationCounts> = const {
        Cell::new(AllocationCounts { calls: 0, requested_bytes: 0 })
    };
    static OBSERVED: Cell<Option<AllocationCounts>> = const { Cell::new(None) };
}

fn allocation_call(bytes: usize) {
    // Constant TLS bookkeeping needs no heap work. The guard additionally
    // prevents reentrant allocator/TLS activity from recursively recording.
    let _ = BOOKKEEPING.try_with(|busy| {
        if busy.replace(true) {
            return;
        }
        let _ = RECORDING.try_with(|recording| {
            if recording.get() {
                let _ = COUNTS.try_with(|counts| {
                    let previous = counts.get();
                    counts.set(AllocationCounts {
                        calls: previous.calls.saturating_add(1),
                        requested_bytes: previous.requested_bytes.saturating_add(bytes as u64),
                    });
                });
            }
        });
        busy.set(false);
    });
}

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocation_call(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocation_call(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocation_call(size);
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

pub(super) struct PreparationAllocationScope {
    active: bool,
}

/// Called only around synchronous construction, never across an await.
pub(super) fn begin(path: PreparationPath) -> PreparationAllocationScope {
    let active = REQUESTED_PATH.with(|requested| {
        if requested.get() == Some(path) {
            requested.set(None);
            true
        } else {
            false
        }
    });
    if active {
        COUNTS.with(|counts| counts.set(AllocationCounts::default()));
        RECORDING.with(|recording| recording.set(true));
    }
    PreparationAllocationScope { active }
}

impl Drop for PreparationAllocationScope {
    fn drop(&mut self) {
        if self.active {
            RECORDING.with(|recording| recording.set(false));
            let counts = COUNTS.with(Cell::get);
            OBSERVED.with(|observed| observed.set(Some(counts)));
        }
    }
}

fn arm(path: PreparationPath) {
    assert!(!RECORDING.with(Cell::get));
    assert_eq!(REQUESTED_PATH.with(Cell::get), None);
    REQUESTED_PATH.with(|requested| requested.set(Some(path)));
    OBSERVED.with(|observed| observed.set(None));
}

fn observation() -> AllocationCounts {
    assert_eq!(
        REQUESTED_PATH.with(Cell::get),
        None,
        "actual synchronous next-node construction hook was not reached"
    );
    assert!(!RECORDING.with(Cell::get));
    OBSERVED
        .with(|observed| observed.replace(None))
        .expect("missing measurement must not count as zero allocations")
}

struct Volume(PathBuf);

impl Volume {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mount-rs-selected-preparation-allocation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    async fn open(&self, owner: &str) -> ChunkedFs<SqliteMetadataStore, SqliteBlockStore> {
        ChunkedFs::open(
            SqliteMetadataStore::open(self.0.join("metadata.db")).unwrap(),
            SqliteBlockStore::open(self.0.join("blocks.db")).unwrap(),
            ChunkedOptions::fixed(owner, BLOCK_SIZE as usize)
                .unwrap()
                .with_compact_inode_updates(true),
        )
        .await
        .unwrap()
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn check_actual_path(path: PreparationPath) -> AllocationCounts {
    const EXTENTS: usize = 256;
    let volume = Volume::new();
    let original_bytes: Vec<u8> = (0..EXTENTS * BLOCK_SIZE as usize)
        .map(|offset| ((offset / BLOCK_SIZE as usize) % 251 + 1) as u8)
        .collect();
    let setup = volume.open("allocation-seed").await;
    setup.write_file("/file", &original_bytes).await.unwrap();
    setup.shutdown().await.unwrap();
    drop(setup);

    let fs = volume.open("allocation-write").await;
    let handle = FsDriver::open(&fs, "/file", "r+", 0).await.unwrap();
    let inode = handle.stat().await.unwrap().ino;
    let pinned = fs.lock_state().unwrap().selected_inodes[&inode].clone();
    let original = pinned.as_ref().clone();
    let NodeData::File(layout) = &original.data else {
        panic!("seed must be a regular file")
    };
    assert_eq!(layout.extents.len(), EXTENTS);
    assert_eq!(original.stats.size, original_bytes.len() as u64);
    let retained_reader = ReadLayoutSnapshot::Selected(Arc::clone(&pinned));

    // Positive control measures a real owned clone of this same large body.
    // It proves both Vec and per-BlockId String allocations reach the meter.
    arm(PreparationPath::MeterControl);
    let control_scope = begin(PreparationPath::MeterControl);
    let duplicate = std::hint::black_box(original.clone());
    drop(control_scope);
    let control = observation();
    assert_eq!(duplicate, original);
    assert!(control.calls > EXTENTS as u64, "{control:?}");
    assert!(
        control.requested_bytes >= (EXTENTS * std::mem::size_of::<BlockExtent>()) as u64,
        "{control:?}"
    );
    drop(duplicate);

    let replacement = [0xd3; BLOCK_SIZE as usize];
    arm(path);
    let mut expected = original_bytes.clone();
    match path {
        PreparationPath::HandleWrite => {
            assert_eq!(
                handle.write(&replacement, Some(0)).await.unwrap(),
                replacement.len()
            );
            expected[..replacement.len()].copy_from_slice(&replacement);
        }
        PreparationPath::WholeFileReplace => {
            fs.write_file("/file", &replacement).await.unwrap();
            expected = replacement.to_vec();
        }
        PreparationPath::MeterControl => unreachable!(),
    }
    let measured = observation();
    assert!(!fs.failed());
    {
        let state = fs.lock_state().unwrap();
        let next = &state.selected_inodes[&inode];
        assert!(!Arc::ptr_eq(next, &pinned));
        assert_eq!(next.stats.ino, original.stats.ino);
        assert_eq!(next.stats.mode, original.stats.mode);
        assert_eq!(next.stats.nlink, original.stats.nlink);
        assert_eq!(next.stats.uid, original.stats.uid);
        assert_eq!(next.stats.gid, original.stats.gid);
        assert_eq!(next.stats.atime_ms, original.stats.atime_ms);
        assert_eq!(next.stats.birthtime_ms, original.stats.birthtime_ms);
        assert_eq!(next.stats.size, expected.len() as u64);
        assert!(next.stats.mtime_ms > original.stats.mtime_ms);
        assert!(next.stats.ctime_ms > original.stats.ctime_ms);
    }
    assert_eq!(pinned.as_ref(), &original);
    let mut retained_bytes = vec![0; original_bytes.len()];
    read_layout_into(
        &fs.inner.blocks,
        retained_reader.layout().unwrap(),
        original.stats.size,
        0,
        &mut retained_bytes,
        "/file",
        "retained-allocation-reader",
    )
    .await
    .unwrap();
    assert_eq!(retained_bytes, original_bytes);
    handle.close().await.unwrap();
    fs.shutdown().await.unwrap();
    drop(fs);

    // Complete durable and retained-reader oracles before the allocation RED.
    let fresh = volume.open("allocation-oracle").await;
    let file = fresh.open("/file", "r", 0).await.unwrap();
    assert_eq!(file.stat().await.unwrap().ino, inode);
    assert_eq!(file.stat().await.unwrap().size, expected.len() as u64);
    let mut bytes = vec![0; expected.len()];
    assert_eq!(
        file.read(&mut bytes, Some(0)).await.unwrap(),
        expected.len()
    );
    assert_eq!(bytes, expected);
    assert_eq!(
        file.read(&mut [0; 1], Some(expected.len() as u64))
            .await
            .unwrap(),
        0
    );
    file.close().await.unwrap();
    fresh.shutdown().await.unwrap();
    eprintln!(
        "selected_next_node_construction path={path:?} old_extents={EXTENTS} calls={} requested_bytes={} deliberate_clone_calls={} deliberate_clone_bytes={} pinned_bytes=verified fresh_bytes=verified inode=verified eof=verified",
        measured.calls, measured.requested_bytes, control.calls, control.requested_bytes,
    );
    measured
}

#[test]
fn selected_handle_write_does_not_copy_discarded_old_layout() {
    let measured = futures_lite::future::block_on(check_actual_path(PreparationPath::HandleWrite));
    assert_eq!(
        measured,
        AllocationCounts::default(),
        "only synchronous next-node construction must allocate nothing; old layout is discarded"
    );
}

#[test]
fn selected_whole_file_replace_does_not_copy_discarded_old_layout() {
    let measured =
        futures_lite::future::block_on(check_actual_path(PreparationPath::WholeFileReplace));
    assert_eq!(
        measured,
        AllocationCounts::default(),
        "only synchronous next-node construction must allocate nothing; old layout is discarded"
    );
}
