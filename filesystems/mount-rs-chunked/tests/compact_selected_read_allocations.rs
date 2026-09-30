//! Complete warmed root-child open/read/close through the actual SQLite
//! provider. The Rust allocator meter excludes SQLite's foreign allocations,
//! setup, Full audits and shutdown; it is not a process RSS or zero-I/O claim.
//! No production hooks or changed write paths are required for this RED.

#![cfg(unix)]

use futures_lite::future::block_on;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::compact::{CompactSnapshot, CompactStructuralDelta, StructuralScope};
use mount_rs_core::storage::{DirectoryEntry, MetadataStore, NodeData, NodeMetadata};
use mount_rs_core::{ErrorCode, FsDriver};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

const CONTENT: &[u8] = b"selected-read";
const TARGET: &str = "/file-0000";
const MAX_CALLS: u64 = 128;
const MAX_BYTES: u64 = 16 * 1024;

#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    calls: u64,
    requested_bytes: u64,
}

thread_local! {
    static BUSY: Cell<bool> = const { Cell::new(false) };
    static RECORDING: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<Counts> = const {
        Cell::new(Counts { calls: 0, requested_bytes: 0 })
    };
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

struct Volume(PathBuf);
impl Volume {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mount-rs-compact-selected-read-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn metadata(&self) -> SqliteMetadataStore {
        SqliteMetadataStore::open(self.0.join("metadata.db")).unwrap()
    }
    async fn open(&self, owner: &str) -> ChunkedFs<SqliteMetadataStore, SqliteBlockStore> {
        ChunkedFs::open(
            self.metadata(),
            SqliteBlockStore::open(self.0.join("blocks.db")).unwrap(),
            ChunkedOptions::fixed(owner, 4096)
                .unwrap()
                .with_compact_inode_updates(true),
        )
        .await
        .unwrap()
    }
    fn connection(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.0.join("metadata.db")).unwrap()
    }
    async fn seed(&self, siblings: usize) -> CompactSnapshot {
        assert!(siblings >= 2);
        let setup = self.open("selected-read-seed").await;
        setup.write_file(TARGET, CONTENT).await.unwrap();
        let backing = setup.concurrent_backing_id().unwrap();
        setup.shutdown().await.unwrap();
        drop(setup);
        let metadata = self.metadata();
        let base = metadata.load_compact_snapshot(backing).await.unwrap();
        let mut namespace = base.namespace().unwrap();
        let NodeData::Directory { entries } = &namespace.nodes[&namespace.root].data else {
            panic!("seed root must be a directory")
        };
        let template = namespace.nodes[&entries[0].inode].clone();
        for index in 1..siblings {
            let inode = namespace.next_inode;
            namespace.next_inode += 1;
            let mut node = template.clone();
            node.stats.ino = inode;
            namespace.nodes.insert(inode, node);
            let NodeData::Directory { entries } =
                &mut namespace.nodes.get_mut(&namespace.root).unwrap().data
            else {
                unreachable!()
            };
            let name = if index == 1 {
                format!("quoted\"back\\slash-{}-😀", "x".repeat(256))
            } else {
                format!("file-{index:04}")
            };
            entries.push(DirectoryEntry { name, inode });
        }
        // Preserve an order which differs from inode/name sorting. An unchanged
        // root comparator must compare insertion positions, not just a map.
        let NodeData::Directory { entries } =
            &mut namespace.nodes.get_mut(&namespace.root).unwrap().data
        else {
            unreachable!()
        };
        entries.reverse();
        let delta =
            CompactStructuralDelta::capture(&base, &namespace, StructuralScope::Full).unwrap();
        metadata.publish_compact_structure(&delta).await.unwrap();
        let seeded = metadata.load_compact_snapshot(backing).await.unwrap();
        assert_eq!(seeded.namespace().unwrap().nodes.len(), siblings + 1);
        seeded
    }
}
impl Drop for Volume {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn entries(root: &NodeMetadata) -> &[DirectoryEntry] {
    let NodeData::Directory { entries } = &root.data else {
        panic!("root directory required")
    };
    entries
}

async fn read_cycle(fs: &ChunkedFs<SqliteMetadataStore, SqliteBlockStore>, expected_inode: u64) {
    let handle = FsDriver::open(fs, TARGET, "r", 0).await.unwrap();
    assert_eq!(handle.stat().await.unwrap().ino, expected_inode);
    let mut bytes = [0; CONTENT.len()];
    assert_eq!(
        handle.read(&mut bytes, Some(0)).await.unwrap(),
        CONTENT.len()
    );
    assert_eq!(bytes.as_slice(), CONTENT);
    handle.close().await.unwrap();
}

async fn oracle(volume: &Volume, seeded: &CompactSnapshot, inode: u64) {
    let fresh_metadata = volume.metadata();
    let fresh = fresh_metadata
        .load_compact_snapshot(seeded.anchor.backing)
        .await
        .unwrap();
    assert_eq!(fresh.anchor, seeded.anchor);
    assert_eq!(
        fresh.guards[&seeded.anchor.root],
        seeded.guards[&seeded.anchor.root]
    );
    assert_eq!(
        entries(&fresh.guards[&seeded.anchor.root].node),
        entries(&seeded.guards[&seeded.anchor.root].node)
    );
    let fresh_fs = volume.open("selected-read-oracle").await;
    read_cycle(&fresh_fs, inode).await;
    fresh_fs.shutdown().await.unwrap();
}

fn check_allocations(siblings: usize) {
    block_on(async {
        let volume = Volume::new();
        let seeded = volume.seed(siblings).await;
        let root = &seeded.guards[&seeded.anchor.root].node;
        let inode = entries(root)
            .iter()
            .find(|entry| entry.name == "file-0000")
            .unwrap()
            .inode;
        let fs = volume.open("selected-read-measured").await;
        read_cycle(&fs, inode).await;

        // Positive baseline from the same actual persisted directory: meter
        // must observe its Vec and individual name allocations before the RED.
        let control_window = Window::begin();
        let cloned = std::hint::black_box(root.clone());
        let control = control_window.finish();
        assert_eq!(cloned, *root);
        assert!(control.calls > siblings as u64, "{control:?}");
        assert!(
            control.requested_bytes > MAX_BYTES || control.calls > MAX_CALLS,
            "{control:?}"
        );
        drop(cloned);

        let window = Window::begin();
        read_cycle(&fs, inode).await;
        let measured = window.finish();
        assert!(!fs.failed());
        fs.shutdown().await.unwrap();
        drop(fs);
        // All behavioral, freshness and independent durable oracles precede
        // the intended failure; failed correctness never counts as fast.
        oracle(&volume, &seeded, inode).await;
        assert!(
            measured.calls > 0,
            "missing measurement cannot count as zero"
        );
        println!(
            "compact_selected_read_allocations {{\"backend\":\"sqlite\",\"siblings\":{siblings},\"rust_calls\":{},\"rust_requested_bytes\":{},\"foreign_allocations_included\":false,\"fresh_oracles_passed\":true}}",
            measured.calls, measured.requested_bytes
        );
        assert!(
            measured.calls <= MAX_CALLS && measured.requested_bytes <= MAX_BYTES,
            "fresh selected open/read/close at {siblings} siblings materializes the directory: {measured:?}; limits calls={MAX_CALLS}, bytes={MAX_BYTES}"
        );
    });
}

#[test]
fn fresh_selected_open_read_close_128_siblings_has_bounded_rust_allocations() {
    check_allocations(128);
}

#[test]
fn fresh_selected_open_read_close_1000_siblings_has_bounded_rust_allocations() {
    check_allocations(1000);
}

#[test]
fn fresh_selected_root_rejects_unrequested_same_identity_entry_and_order_changes() {
    for change_order in [false, true] {
        block_on(async {
            let volume = Volume::new();
            let seeded = volume.seed(8).await;
            let fs = volume.open("selected-root-corruption").await;
            let inode = entries(&seeded.guards[&seeded.anchor.root].node)
                .iter()
                .find(|entry| entry.name == "file-0000")
                .unwrap()
                .inode;
            read_cycle(&fs, inode).await;
            let mut changed = seeded.guards[&seeded.anchor.root].node.clone();
            let NodeData::Directory { entries } = &mut changed.data else {
                unreachable!()
            };
            if change_order {
                entries.swap(0, 1);
            } else {
                entries[0].name = "changed-unrequested".into();
            }
            let connection = volume.connection();
            connection
                .execute(
                    "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
                    rusqlite::params![
                        serde_json::to_string(&changed).unwrap(),
                        seeded.anchor.root.to_string()
                    ],
                )
                .unwrap();
            let rejected = FsDriver::open(&fs, TARGET, "r", 0)
                .await
                .err()
                .expect("same-identity parent change must fail closed");
            assert_eq!(rejected.code, ErrorCode::Estale);
            assert!(fs.failed());
            connection
                .execute(
                    "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
                    rusqlite::params![
                        serde_json::to_string(&seeded.guards[&seeded.anchor.root].node).unwrap(),
                        seeded.anchor.root.to_string()
                    ],
                )
                .unwrap();
            drop(connection);
            let _ = fs.shutdown().await;
            drop(fs);
            oracle(&volume, &seeded, inode).await;
        });
    }
}

#[test]
fn fresh_selected_root_checks_unrequested_child_membership_and_anchor_order() {
    for malformed in 0..3 {
        block_on(async {
            let volume = Volume::new();
            let seeded = volume.seed(8).await;
            let fs = volume.open("selected-membership-corruption").await;
            let inode = entries(&seeded.guards[&seeded.anchor.root].node)
                .iter()
                .find(|entry| entry.name == "file-0000")
                .unwrap()
                .inode;
            read_cycle(&fs, inode).await;
            let mut anchor = serde_json::to_value(&seeded.anchor).unwrap();
            let members = anchor["members"].as_array_mut().unwrap();
            if malformed == 1 {
                members.swap(0, 1);
            } else if malformed == 2 {
                members.insert(1, members[0].clone());
            } else {
                let unrequested = entries(&seeded.guards[&seeded.anchor.root].node)[0].inode;
                members.retain(|member| member.as_u64() != Some(unrequested));
            }
            let bad =
                serde_json::json!({"layout":"mount-rs-compact-inodes","version":1,"anchor":anchor});
            let connection = volume.connection();
            connection
                .execute(
                    "UPDATE mount_rs_metadata SET namespace=?1 WHERE id=1",
                    [serde_json::to_string(&bad).unwrap()],
                )
                .unwrap();
            assert!(FsDriver::open(&fs, TARGET, "r", 0).await.is_err());
            assert!(fs.failed());
            connection
                .execute(
                    "UPDATE mount_rs_metadata SET namespace=?1 WHERE id=1",
                    [String::from_utf8(
                        mount_rs_core::storage::compact::encode_compact_anchor(&seeded.anchor)
                            .unwrap(),
                    )
                    .unwrap()],
                )
                .unwrap();
            drop(connection);
            let _ = fs.shutdown().await;
            drop(fs);
            oracle(&volume, &seeded, inode).await;
        });
    }
}

#[test]
fn fresh_selected_reads_preserve_derived_serde_semantics_and_valid_unused_anchor_changes() {
    block_on(async {
        let volume = Volume::new();
        let seeded = volume.seed(8).await;
        let root = &seeded.guards[&seeded.anchor.root].node;
        let inode = entries(root)
            .iter()
            .find(|entry| entry.name == "file-0000")
            .unwrap()
            .inode;
        let fs = volume.open("selected-semantic-variants").await;
        read_cycle(&fs, inode).await;
        let connection = volume.connection();
        let root_json = serde_json::to_string(root).unwrap();
        let mut stats = serde_json::to_value(&root.stats).unwrap();
        stats["ignored_nested"] = serde_json::json!({"array":[{"escaped":"quotes\"and\\slashes"}]});
        let variants = [
            serde_json::to_string_pretty(root).unwrap(),
            // Reordered known fields plus ignored nested fields retain ordinary
            // derived NodeMetadata/Stats semantics, rather than raw-byte equality.
            format!(
                "{{\"ignored\":{{\"nested\":[1,{{\"key\":true}}]}},\"data\":{},\"stats\":{}}}",
                serde_json::to_string(&root.data).unwrap(),
                serde_json::to_string(&stats).unwrap()
            ),
            root_json
                .replace("\"name\":", "\"\\u006eame\":")
                .replace("file-0000", "\\u0066ile-0000")
                .replace('😀', "\\ud83d\\ude00"),
        ];
        for body in variants {
            let reference: NodeMetadata = serde_json::from_str(&body).unwrap();
            assert_eq!(&reference, root);
            connection
                .execute(
                    "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
                    rusqlite::params![body, seeded.anchor.root.to_string()],
                )
                .unwrap();
            let actual = volume
                .metadata()
                .load_compact_inode(seeded.anchor.backing, seeded.anchor.root)
                .await
                .unwrap();
            assert_eq!(&actual.guard.node, root);
            read_cycle(&fs, inode).await;
            assert!(!fs.failed());
        }

        // A duplicate BTreeMap parameter follows the reference's last-value
        // semantics. Known struct-field duplicates remain rejected by Serde.
        let file = &seeded.guards[&inode].node;
        let file_json = serde_json::to_string(file).unwrap();
        let duplicate_parameter = file_json.replace(
            "\"chunk_size\":4096",
            "\"chunk_size\":1,\"chunk_size\":4096",
        );
        assert_ne!(duplicate_parameter, file_json);
        assert_eq!(
            serde_json::from_str::<NodeMetadata>(&duplicate_parameter).unwrap(),
            *file
        );
        connection
            .execute(
                "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
                rusqlite::params![duplicate_parameter, inode.to_string()],
            )
            .unwrap();
        read_cycle(&fs, inode).await;
        let duplicate_uid = file_json.replace("\"uid\":0", "\"uid\":0,\"uid\":0");
        assert_ne!(duplicate_uid, file_json);
        assert!(serde_json::from_str::<NodeMetadata>(&duplicate_uid).is_err());
        connection
            .execute(
                "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
                rusqlite::params![file_json, inode.to_string()],
            )
            .unwrap();

        // Ordinary selected reads validate the entire current anchor but do not
        // compare all old members/defaults. A valid unused member/default change
        // is currently accepted; do not silently impose Full-audit equality here.
        let mut anchor = seeded.anchor.clone();
        anchor.default_uid = 42;
        anchor.default_gid = 43;
        anchor.umask = 0o077;
        anchor
            .default_chunker
            .parameters
            .insert("chunk_size".into(), 2048);
        anchor.members.push(anchor.next_inode);
        anchor.next_inode += 1;
        anchor.validate().unwrap();
        connection
            .execute(
                "UPDATE mount_rs_metadata SET namespace=?1 WHERE id=1",
                [String::from_utf8(
                    mount_rs_core::storage::compact::encode_compact_anchor(&anchor).unwrap(),
                )
                .unwrap()],
            )
            .unwrap();
        let actual = volume
            .metadata()
            .load_compact_inode(seeded.anchor.backing, inode)
            .await
            .unwrap();
        assert_eq!(actual.generation, seeded.anchor.generation);
        assert_eq!(actual.guard.node, *file);
        read_cycle(&fs, inode).await;
        assert!(!fs.failed());
        connection
            .execute(
                "UPDATE mount_rs_metadata SET namespace=?1 WHERE id=1",
                [String::from_utf8(
                    mount_rs_core::storage::compact::encode_compact_anchor(&seeded.anchor).unwrap(),
                )
                .unwrap()],
            )
            .unwrap();
        drop(connection);
        fs.shutdown().await.unwrap();
        drop(fs);
        oracle(&volume, &seeded, inode).await;
    });
}

#[test]
fn fresh_selected_root_rechecks_authority_defaults_and_known_field_duplicates() {
    for invalid in 0..3 {
        block_on(async {
            let volume = Volume::new();
            let seeded = volume.seed(8).await;
            let root = &seeded.guards[&seeded.anchor.root].node;
            let inode = entries(root)
                .iter()
                .find(|entry| entry.name == "file-0000")
                .unwrap()
                .inode;
            let fs = volume.open("selected-authority-corruption").await;
            read_cycle(&fs, inode).await;
            let connection = volume.connection();
            if invalid == 1 {
                connection
                    .execute(
                        "UPDATE mount_rs_metadata SET write_mode='MRC4' WHERE id=1",
                        [],
                    )
                    .unwrap();
            } else if invalid == 0 {
                let mut anchor = serde_json::to_value(&seeded.anchor).unwrap();
                anchor["default_chunker"]["algorithm"] =
                    serde_json::json!("unsupported-test-chunker");
                let bad = serde_json::json!({"layout":"mount-rs-compact-inodes","version":1,"anchor":anchor});
                connection
                    .execute(
                        "UPDATE mount_rs_metadata SET namespace=?1 WHERE id=1",
                        [serde_json::to_string(&bad).unwrap()],
                    )
                    .unwrap();
            } else {
                let file_json = serde_json::to_string(&seeded.guards[&inode].node).unwrap();
                let duplicated = file_json.replace("\"uid\":0", "\"uid\":0,\"uid\":0");
                assert_ne!(duplicated, file_json);
                assert!(serde_json::from_str::<NodeMetadata>(&duplicated).is_err());
                connection
                    .execute(
                        "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
                        rusqlite::params![duplicated, inode.to_string()],
                    )
                    .unwrap();
            }
            assert!(FsDriver::open(&fs, TARGET, "r", 0).await.is_err());
            assert!(fs.failed());
            connection
                .execute(
                    "UPDATE mount_rs_metadata SET write_mode='MRC5',namespace=?1 WHERE id=1",
                    [String::from_utf8(
                        mount_rs_core::storage::compact::encode_compact_anchor(&seeded.anchor)
                            .unwrap(),
                    )
                    .unwrap()],
                )
                .unwrap();
            connection
                .execute(
                    "UPDATE mount_rs_compact_guards SET node=?1 WHERE inode=?2",
                    rusqlite::params![
                        serde_json::to_string(&seeded.guards[&inode].node).unwrap(),
                        inode.to_string()
                    ],
                )
                .unwrap();
            drop(connection);
            let _ = fs.shutdown().await;
            drop(fs);
            oracle(&volume, &seeded, inode).await;
        });
    }
}
