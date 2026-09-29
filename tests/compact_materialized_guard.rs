//! Typed selected-read contracts and allocation controls. These consume already
//! materialized provider rows; row decoding and fixture setup are outside the
//! calling-thread allocation window.

use mount_rs_core::storage::compact::{
    CompactAnchor, CompactGuard, CompactInodeExpectation, CompactInodeRead, CompactSnapshot,
    LoadedCompactInode, PhysicalInodeIdentity, check_compact_inode_unchanged,
    encode_compact_anchor,
};
use mount_rs_core::storage::{
    BlockExtent, BlockId, ConcurrentBackingId, DirectoryEntry, FileLayout, NodeData, NodeMetadata,
};
use mount_rs_core::{Result, S_IFDIR, S_IFIFO, S_IFLNK, S_IFREG, Stats};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

type NamedMutation<T> = (&'static str, fn(&mut T));

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<usize> = const { Cell::new(0) };
    static REQUESTED_BYTES: Cell<usize> = const { Cell::new(0) };
}

struct CountingAllocator;

fn count(size: usize) {
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            CALLS.with(|calls| calls.set(calls.get() + 1));
            REQUESTED_BYTES.with(|bytes| bytes.set(bytes.get() + size));
        }
    });
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count(size);
        unsafe { System.realloc(pointer, layout, size) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn measured<T>(operation: impl FnOnce() -> T) -> (T, usize, usize) {
    struct Window;
    impl Drop for Window {
        fn drop(&mut self) {
            COUNTING.with(|counting| counting.set(false));
        }
    }
    // Initialize all thread-local counters before enabling the meter.
    CALLS.with(|calls| calls.set(0));
    REQUESTED_BYTES.with(|bytes| bytes.set(0));
    COUNTING.with(|counting| counting.set(true));
    let window = Window;
    let value = std::hint::black_box(operation());
    drop(window);
    (
        value,
        CALLS.with(Cell::get),
        REQUESTED_BYTES.with(Cell::get),
    )
}

fn fixture(siblings: usize) -> CompactSnapshot {
    let chunker = mount_rs_core::chunking::ChunkerConfig {
        algorithm: "fixed-size".into(),
        version: 1,
        parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
    };
    let identity = PhysicalInodeIdentity {
        incarnation: 1,
        epoch: 3,
        revision: 4,
    };
    let node = |inode, mode, nlink, size, data| CompactGuard {
        identity,
        node: NodeMetadata {
            stats: Stats {
                dev: 0,
                ino: inode,
                mode,
                nlink,
                uid: 1000,
                gid: 1000,
                rdev: 0,
                size,
                blksize: 4096,
                blocks: u64::from(size != 0),
                atime_ms: 0,
                mtime_ms: 0,
                ctime_ms: 0,
                birthtime_ms: 0,
            },
            data,
        },
    };
    let mut entries = Vec::new();
    let mut guards = BTreeMap::new();
    for inode in (2..=siblings as u64 + 1).rev() {
        entries.push(DirectoryEntry {
            name: format!("child-{inode}"),
            inode,
        });
        guards.insert(
            inode,
            node(
                inode,
                S_IFREG | 0o644,
                1,
                4,
                NodeData::File(FileLayout {
                    chunker: chunker.clone(),
                    extents: vec![BlockExtent {
                        file_offset: 0,
                        block: BlockId("block".into()),
                        block_offset: 0,
                        length: 4,
                    }],
                }),
            ),
        );
    }
    guards.insert(
        1,
        node(1, S_IFDIR | 0o755, 2, 0, NodeData::Directory { entries }),
    );
    CompactSnapshot {
        anchor: CompactAnchor {
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            generation: 3,
            root: 1,
            next_inode: siblings as u64 + 2,
            default_uid: 1000,
            default_gid: 1000,
            umask: 0o022,
            default_chunker: chunker,
            members: guards.keys().copied().collect(),
        },
        guards,
    }
}

/// Actual pre-factory primitives: validate the owned guard, serialize its typed
/// view, then run the existing complete streamed certifier. This reference
/// deliberately preserves its conservative match/fallback semantics.
fn encoded_reference(
    anchor: &CompactAnchor,
    backing: ConcurrentBackingId,
    inode: u64,
    guard: CompactGuard,
    expected: CompactInodeExpectation<'_>,
) -> Result<CompactInodeRead> {
    let loaded = LoadedCompactInode::from_guard(anchor, inode, guard)?;
    let anchor_bytes = encode_compact_anchor(anchor)?;
    let node_bytes =
        serde_json::to_vec(&loaded.guard.node).map_err(mount_rs_core::backend_error)?;
    Ok(
        match check_compact_inode_unchanged(
            &anchor_bytes,
            backing,
            loaded.generation,
            inode,
            loaded.guard.identity,
            &node_bytes,
            expected,
        ) {
            Some(checked) => CompactInodeRead::Unchanged(checked),
            None => CompactInodeRead::Loaded(loaded),
        },
    )
}

fn assert_matches_reference(
    label: &str,
    anchor: &CompactAnchor,
    backing: ConcurrentBackingId,
    inode: u64,
    guard: CompactGuard,
    expected: CompactInodeExpectation<'_>,
) {
    let reference = encoded_reference(anchor, backing, inode, guard.clone(), expected);
    let actual = CompactInodeRead::from_materialized_guard(anchor, backing, inode, guard, expected);
    match (reference, actual) {
        (Err(reference), Err(actual)) => {
            assert_eq!(actual.code, reference.code, "{label}");
            assert_eq!(actual.to_string(), reference.to_string(), "{label}");
        }
        (Ok(CompactInodeRead::Loaded(reference)), Ok(CompactInodeRead::Loaded(actual))) => {
            assert_eq!(actual, reference, "{label}");
        }
        (Ok(CompactInodeRead::Unchanged(reference)), Ok(CompactInodeRead::Unchanged(actual))) => {
            assert_eq!(actual.generation(), reference.generation(), "{label}");
            assert_eq!(actual.inode(), reference.inode(), "{label}");
            assert_eq!(actual.identity(), reference.identity(), "{label}");
            match (reference.into_verified_root(), actual.into_verified_root()) {
                (None, None) => {}
                (Some(reference), Some(actual)) => {
                    assert_eq!(actual.generation(), reference.generation(), "{label}");
                    assert_eq!(actual.guard(), reference.guard(), "{label}");
                }
                (reference, actual) => panic!("{label}: root receipt {reference:?} != {actual:?}"),
            }
        }
        (reference, actual) => panic!("{label}: {reference:?} != {actual:?}"),
    }
}

#[test]
fn allocation_meter_observes_the_actual_encode_and_stream_reference() {
    let snapshot = fixture(128);
    let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
    let guard = snapshot.guards[&1].clone();
    let (read, calls, bytes) = measured(|| {
        encoded_reference(
            &snapshot.anchor,
            snapshot.anchor.backing,
            1,
            guard,
            structure.expect_root(),
        )
        .unwrap()
    });
    assert!(matches!(read, CompactInodeRead::Unchanged(_)));
    assert!(
        calls > 0 && bytes > 0,
        "old typed-to-JSON path must be measured"
    );
}

#[test]
fn materialized_unchanged_regular_file_allocates_nothing() {
    let snapshot = fixture(1000);
    let guard = snapshot.guards[&2].clone();
    let expected_guard = &snapshot.guards[&2];
    let expected = CompactInodeExpectation::selected(
        snapshot.anchor.generation,
        expected_guard.identity,
        &expected_guard.node,
    );
    let (read, calls, bytes) = measured(|| {
        CompactInodeRead::from_materialized_guard(
            &snapshot.anchor,
            snapshot.anchor.backing,
            2,
            guard,
            expected,
        )
        .unwrap()
    });
    let CompactInodeRead::Unchanged(checked) = read else {
        panic!("identical valid regular file must certify");
    };
    assert_eq!(checked.inode(), 2);
    assert!(checked.into_verified_root().is_none());
    assert_eq!(
        (calls, bytes),
        (0, 0),
        "already owned file must not encode or reparse"
    );
}

#[test]
fn materialized_unchanged_audited_root_allocates_nothing_at_128_and_1000_siblings() {
    for siblings in [128, 1000] {
        let snapshot = fixture(siblings);
        let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
        let guard = snapshot.guards[&1].clone();
        let (read, calls, bytes) = measured(|| {
            CompactInodeRead::from_materialized_guard(
                &snapshot.anchor,
                snapshot.anchor.backing,
                1,
                guard,
                structure.expect_root(),
            )
            .unwrap()
        });
        let CompactInodeRead::Unchanged(checked) = read else {
            panic!("identical audited root must certify");
        };
        assert_eq!(checked.inode(), 1);
        let verified = checked.into_verified_root().expect("sealed root receipt");
        assert_eq!(verified.guard(), &snapshot.guards[&1]);
        assert_eq!(
            (calls, bytes),
            (0, 0),
            "siblings {siblings}: no name copy or JSON staging"
        );
    }
}

#[test]
fn materialized_changed_file_retains_owned_buffers_without_allocating() {
    let snapshot = fixture(2);
    let mut guard = snapshot.guards[&2].clone();
    let NodeData::File(layout) = &mut guard.node.data else {
        panic!();
    };
    layout.extents[0].block.0 = "changed-block".into();
    let algorithm_pointer = layout.chunker.algorithm.as_ptr();
    let extents_pointer = layout.extents.as_ptr();
    let block_pointer = layout.extents[0].block.0.as_ptr();
    let expected_guard = &snapshot.guards[&2];
    let expected =
        CompactInodeExpectation::selected(3, expected_guard.identity, &expected_guard.node);
    let (read, calls, bytes) = measured(|| {
        CompactInodeRead::from_materialized_guard(
            &snapshot.anchor,
            snapshot.anchor.backing,
            2,
            guard,
            expected,
        )
        .unwrap()
    });
    let CompactInodeRead::Loaded(loaded) = read else {
        panic!("changed valid file must return its owned guard");
    };
    let NodeData::File(layout) = &loaded.guard.node.data else {
        panic!();
    };
    assert_eq!(layout.chunker.algorithm.as_ptr(), algorithm_pointer);
    assert_eq!(layout.extents.as_ptr(), extents_pointer);
    assert_eq!(layout.extents[0].block.0.as_ptr(), block_pointer);
    assert_eq!(
        (calls, bytes),
        (0, 0),
        "fallback must reuse the supplied file buffers"
    );
}

#[test]
fn materialized_valid_fresh_anchor_changes_match_streamed_semantics() {
    let snapshot = fixture(3);
    let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
    let guard = &snapshot.guards[&1];
    let mutants: &[NamedMutation<CompactAnchor>] = &[
        ("generation", |anchor| anchor.generation += 1),
        ("next inode", |anchor| anchor.next_inode += 100),
        ("uid", |anchor| anchor.default_uid += 1),
        ("gid", |anchor| anchor.default_gid += 1),
        ("umask", |anchor| anchor.umask = 0o077),
        ("default chunker", |anchor| {
            anchor
                .default_chunker
                .parameters
                .insert("chunk_size".into(), 8192);
        }),
        ("unused member", |anchor| {
            anchor.members.push(99);
            anchor.next_inode = 100;
        }),
        (
            "current root differs from audited selected root",
            |anchor| {
                anchor.members.push(99);
                anchor.next_inode = 100;
                anchor.root = 99;
            },
        ),
    ];
    for (label, mutate) in mutants {
        let mut anchor = snapshot.anchor.clone();
        mutate(&mut anchor);
        assert_matches_reference(
            label,
            &anchor,
            anchor.backing,
            1,
            guard.clone(),
            structure.expect_root(),
        );
    }
    let mut changed = snapshot.anchor.clone();
    changed.members.push(99);
    changed.next_inode = 100;
    changed.root = 99;
    changed.default_uid = 55;
    changed.default_gid = 66;
    changed.umask = 0o077;
    let read = CompactInodeRead::from_materialized_guard(
        &changed,
        changed.backing,
        1,
        guard.clone(),
        structure.expect_root(),
    )
    .unwrap();
    assert!(
        matches!(read, CompactInodeRead::Unchanged(_)),
        "fresh valid fields are not full-anchor equality guards"
    );
}

#[test]
fn materialized_backing_mismatch_is_a_hint_miss_with_valid_owned_fallback() {
    let snapshot = fixture(3);
    let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
    let other = ConcurrentBackingId::from_bytes([2; 16]).unwrap();
    assert_matches_reference(
        "backing mismatch",
        &snapshot.anchor,
        other,
        1,
        snapshot.guards[&1].clone(),
        structure.expect_root(),
    );
    let read = CompactInodeRead::from_materialized_guard(
        &snapshot.anchor,
        other,
        1,
        snapshot.guards[&1].clone(),
        structure.expect_root(),
    )
    .unwrap();
    assert!(matches!(read, CompactInodeRead::Loaded(_)));
}

#[test]
fn materialized_invalid_fresh_anchor_and_missing_children_preserve_validation_errors() {
    let snapshot = fixture(3);
    let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
    let mutants: &[NamedMutation<CompactAnchor>] = &[
        ("zero generation", |anchor| anchor.generation = 0),
        ("zero root", |anchor| anchor.root = 0),
        ("missing current root", |anchor| anchor.root = 99),
        ("empty membership", |anchor| anchor.members.clear()),
        ("zero member", |anchor| anchor.members.insert(0, 0)),
        ("duplicate member", |anchor| anchor.members.insert(1, 1)),
        ("unsorted membership", |anchor| anchor.members.swap(1, 2)),
        ("exhausted membership", |anchor| {
            anchor.members.push(u64::MAX)
        }),
        ("invalid next inode", |anchor| anchor.next_inode = 4),
        ("missing selected inode", |anchor| {
            anchor.members.remove(0);
            anchor.root = 2;
        }),
        ("missing audited child", |anchor| {
            anchor.members.pop();
        }),
        ("invalid default chunker", |anchor| {
            anchor
                .default_chunker
                .parameters
                .insert("chunk_size".into(), 0);
        }),
    ];
    for (label, mutate) in mutants {
        let mut anchor = snapshot.anchor.clone();
        mutate(&mut anchor);
        assert_matches_reference(
            label,
            &anchor,
            anchor.backing,
            1,
            snapshot.guards[&1].clone(),
            structure.expect_root(),
        );
    }
}

#[test]
fn materialized_every_stat_field_and_complete_directory_body_match_reference() {
    let snapshot = fixture(3);
    let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
    let mutants: &[NamedMutation<Stats>] = &[
        ("dev", |stats| stats.dev += 1),
        ("ino", |stats| stats.ino += 1),
        ("mode", |stats| stats.mode ^= 1),
        ("nlink", |stats| stats.nlink += 1),
        ("uid", |stats| stats.uid += 1),
        ("gid", |stats| stats.gid += 1),
        ("rdev", |stats| stats.rdev += 1),
        ("size", |stats| stats.size += 1),
        ("blksize", |stats| stats.blksize += 1),
        ("blocks", |stats| stats.blocks += 1),
        ("atime", |stats| stats.atime_ms += 1),
        ("mtime", |stats| stats.mtime_ms += 1),
        ("ctime", |stats| stats.ctime_ms += 1),
        ("birthtime", |stats| stats.birthtime_ms += 1),
    ];
    for (label, mutate) in mutants {
        let mut guard = snapshot.guards[&1].clone();
        mutate(&mut guard.node.stats);
        assert_matches_reference(
            label,
            &snapshot.anchor,
            snapshot.anchor.backing,
            1,
            guard,
            structure.expect_root(),
        );
    }
    let mutants: &[NamedMutation<Vec<DirectoryEntry>>] = &[
        ("unrequested sibling rename", |entries| {
            entries[2].name = "renamed".into()
        }),
        ("order", |entries| entries.swap(0, 2)),
        ("child identity", |entries| entries[2].inode = 3),
        ("duplicate name", |entries| {
            entries[2].name = entries[0].name.clone()
        }),
        ("invalid name", |entries| {
            entries[2].name = "../escape".into()
        }),
        ("missing membership child", |entries| entries[2].inode = 99),
    ];
    for (label, mutate) in mutants {
        let mut guard = snapshot.guards[&1].clone();
        let NodeData::Directory { entries } = &mut guard.node.data else {
            panic!();
        };
        mutate(entries);
        assert_matches_reference(
            label,
            &snapshot.anchor,
            snapshot.anchor.backing,
            1,
            guard,
            structure.expect_root(),
        );
    }
}

#[test]
fn materialized_identity_and_file_layout_mutants_match_reference() {
    let snapshot = fixture(3);
    let expected_guard = &snapshot.guards[&2];
    let expected =
        CompactInodeExpectation::selected(3, expected_guard.identity, &expected_guard.node);
    let mutants: &[NamedMutation<CompactGuard>] = &[
        ("different revision", |guard| guard.identity.revision += 1),
        ("different incarnation", |guard| {
            guard.identity.incarnation = 2
        }),
        ("different valid epoch", |guard| guard.identity.epoch = 2),
        ("zero incarnation", |guard| guard.identity.incarnation = 0),
        ("incarnation past epoch", |guard| {
            guard.identity.incarnation = 4
        }),
        ("epoch past generation", |guard| guard.identity.epoch = 4),
        ("wrong inode", |guard| guard.node.stats.ino = 3),
        ("mode/data mismatch", |guard| {
            guard.node.stats.mode = S_IFDIR | 0o755
        }),
        ("valid larger size", |guard| guard.node.stats.size = 5),
        ("invalid smaller size", |guard| guard.node.stats.size = 3),
    ];
    for (label, mutate) in mutants {
        let mut guard = expected_guard.clone();
        mutate(&mut guard);
        assert_matches_reference(
            label,
            &snapshot.anchor,
            snapshot.anchor.backing,
            2,
            guard,
            expected,
        );
    }
    let mutants: &[NamedMutation<FileLayout>] = &[
        ("different block", |layout| {
            layout.extents[0].block.0 = "other".into()
        }),
        ("different block offset", |layout| {
            layout.extents[0].block_offset = 1
        }),
        ("shorter extent", |layout| layout.extents[0].length = 3),
        ("empty extent", |layout| layout.extents[0].length = 0),
        ("empty block", |layout| layout.extents[0].block.0.clear()),
        ("file offset overflow", |layout| {
            layout.extents[0].file_offset = u64::MAX
        }),
        ("block offset overflow", |layout| {
            layout.extents[0].block_offset = u64::MAX
        }),
        ("invalid chunker", |layout| {
            layout.chunker.parameters.insert("chunk_size".into(), 0);
        }),
    ];
    for (label, mutate) in mutants {
        let mut guard = expected_guard.clone();
        let NodeData::File(layout) = &mut guard.node.data else {
            panic!();
        };
        mutate(layout);
        assert_matches_reference(
            label,
            &snapshot.anchor,
            snapshot.anchor.backing,
            2,
            guard,
            expected,
        );
    }
}

#[test]
fn materialized_unsealed_directory_stale_hints_and_other_valid_kinds_stay_loaded() {
    let snapshot = fixture(3);
    let guard = &snapshot.guards[&1];
    let expected = CompactInodeExpectation::selected(3, guard.identity, &guard.node);
    assert_matches_reference(
        "unsealed directory",
        &snapshot.anchor,
        snapshot.anchor.backing,
        1,
        guard.clone(),
        expected,
    );
    let read = CompactInodeRead::from_materialized_guard(
        &snapshot.anchor,
        snapshot.anchor.backing,
        1,
        guard.clone(),
        expected,
    )
    .unwrap();
    assert!(matches!(read, CompactInodeRead::Loaded(_)));

    let guard = &snapshot.guards[&2];
    for expected in [
        CompactInodeExpectation::selected(2, guard.identity, &guard.node),
        CompactInodeExpectation::selected(
            3,
            PhysicalInodeIdentity {
                revision: 9,
                ..guard.identity
            },
            &guard.node,
        ),
        CompactInodeExpectation::selected(3, guard.identity, &snapshot.guards[&3].node),
    ] {
        assert_matches_reference(
            "stale hint",
            &snapshot.anchor,
            snapshot.anchor.backing,
            2,
            guard.clone(),
            expected,
        );
    }
    for (mode, data) in [
        (
            S_IFLNK | 0o777,
            NodeData::Symlink {
                target: "target".into(),
            },
        ),
        (S_IFIFO | 0o600, NodeData::Special),
    ] {
        let mut guard = guard.clone();
        guard.node.stats.mode = mode;
        guard.node.data = data;
        let expected_guard = guard.clone();
        let expected = CompactInodeExpectation::selected(3, guard.identity, &expected_guard.node);
        assert_matches_reference(
            "other valid kind",
            &snapshot.anchor,
            snapshot.anchor.backing,
            2,
            guard,
            expected,
        );
    }
}

#[test]
fn materialized_equal_malformed_file_hint_retains_the_original_validation_error() {
    let snapshot = fixture(3);
    let mut guard = snapshot.guards[&2].clone();
    let NodeData::File(layout) = &mut guard.node.data else {
        panic!();
    };
    layout.extents[0].length = 0;
    let expected_guard = guard.clone();
    let expected = CompactInodeExpectation::selected(3, guard.identity, &expected_guard.node);
    let reference = encoded_reference(
        &snapshot.anchor,
        snapshot.anchor.backing,
        2,
        guard.clone(),
        expected,
    )
    .unwrap_err();
    assert_eq!(reference.to_string(), "file extents must be nonempty");
    assert_matches_reference(
        "equal malformed file hint",
        &snapshot.anchor,
        snapshot.anchor.backing,
        2,
        guard,
        expected,
    );
}

#[test]
fn materialized_multiple_invalid_fields_preserve_the_original_error_precedence() {
    type Mutator = fn(&mut CompactAnchor, &mut CompactGuard);
    let cases: &[(&str, Mutator)] = &[
        ("chunk size must be positive", |anchor, guard| {
            anchor
                .default_chunker
                .parameters
                .insert("chunk_size".into(), 0);
            anchor.generation = 0;
            guard.identity.incarnation = 0;
            guard.node.stats.ino = 99;
        }),
        (
            "invalid compact anchor identity/membership",
            |anchor, guard| {
                anchor.generation = 0;
                guard.identity.incarnation = 0;
                guard.node.stats.ino = 99;
            },
        ),
        (
            "selected guard absent from compact membership",
            |anchor, guard| {
                anchor.members.remove(1);
                guard.identity.incarnation = 0;
                guard.node.stats.ino = 99;
            },
        ),
        (
            "invalid compact physical epoch/incarnation",
            |_anchor, guard| {
                guard.identity.incarnation = 0;
                guard.node.stats.ino = 99;
                guard.node.stats.mode = S_IFDIR | 0o755;
            },
        ),
        ("compact guard inode mismatch", |_anchor, guard| {
            guard.node.stats.ino = 99;
            guard.node.stats.mode = S_IFDIR | 0o755;
        }),
        (
            "node metadata kind does not match stats.mode",
            |_anchor, guard| {
                guard.node.stats.mode = S_IFDIR | 0o755;
                let NodeData::File(layout) = &mut guard.node.data else {
                    panic!();
                };
                layout.extents[0].length = 0;
            },
        ),
        ("chunk size must be positive", |_anchor, guard| {
            let NodeData::File(layout) = &mut guard.node.data else {
                panic!();
            };
            layout.chunker.parameters.insert("chunk_size".into(), 0);
            layout.extents[0].length = 0;
        }),
    ];
    let snapshot = fixture(3);
    for (message, mutate) in cases {
        let mut anchor = snapshot.anchor.clone();
        let mut guard = snapshot.guards[&2].clone();
        mutate(&mut anchor, &mut guard);
        let expected_guard = guard.clone();
        let expected = CompactInodeExpectation::selected(
            anchor.generation,
            guard.identity,
            &expected_guard.node,
        );
        let reference =
            encoded_reference(&anchor, anchor.backing, 2, guard.clone(), expected).unwrap_err();
        assert_eq!(reference.to_string(), *message);
        assert_matches_reference(message, &anchor, anchor.backing, 2, guard, expected);
    }
}

#[test]
fn materialized_fresh_backing_drift_matches_the_supplied_fresh_backing() {
    let snapshot = fixture(3);
    let (_, _, structure) = snapshot.clone().into_validated_namespace().unwrap();
    let mut anchor = snapshot.anchor.clone();
    anchor.backing = ConcurrentBackingId::from_bytes([2; 16]).unwrap();
    assert_ne!(anchor.backing, structure.anchor().backing);
    assert_matches_reference(
        "fresh backing differs from sealed audit backing",
        &anchor,
        anchor.backing,
        1,
        snapshot.guards[&1].clone(),
        structure.expect_root(),
    );
    let read = CompactInodeRead::from_materialized_guard(
        &anchor,
        anchor.backing,
        1,
        snapshot.guards[&1].clone(),
        structure.expect_root(),
    )
    .unwrap();
    let CompactInodeRead::Unchanged(checked) = read else {
        panic!("the supplied fresh backing matches: old audit backing is not a match guard");
    };
    assert_eq!(checked.inode(), 1);
    let verified = checked.into_verified_root().expect("sealed root receipt");
    assert_eq!(verified.guard(), &snapshot.guards[&1]);
}
