//! Bounded proofs of the real typed materialized-guard read factory.
//!
//! Execution is tracked by the named formal runner, not by source presence.
//! Every read classification, ordinary validation, complete graph audit and
//! receipt/capture decision below calls production code; fixture and equality
//! helpers only construct inputs or state independent assertions.
//!
//! The file proof admits two independently generated 14-field Stats values,
//! full-width physical identities/generations and at most two complete extents.
//! Strings and map cardinalities are explicitly finite. The error proof uses a
//! declared finite fault matrix, rather than arbitrary malformed metadata.
//! The root proof audits a genuine four-inode graph, including optional hard
//! links, and permits valid fresh-anchor drift before the production factory.
//!
//! Excluded: arbitrary-sized graphs/strings/collections, arbitrary serialized
//! bytes, SQL isolation/read provenance, filesystem local-revision fences,
//! allocation counts, asynchronous cancellation, durability and I/O behavior.
//! Initial unwind annotations are hypotheses. Strict unwinding checks and all
//! covers must pass before any proof-completion claim; do not weaken a domain
//! or disable checks to make an annotation fit.

use super::*;

fn fixed_chunker(size: u64) -> ChunkerConfig {
    ChunkerConfig {
        algorithm: "fixed-size".into(),
        version: 1,
        parameters: BTreeMap::from([("chunk_size".into(), size)]),
    }
}

fn arbitrary_stats() -> Stats {
    Stats {
        dev: kani::any(),
        ino: kani::any(),
        mode: kani::any(),
        nlink: kani::any(),
        uid: kani::any(),
        gid: kani::any(),
        rdev: kani::any(),
        size: kani::any(),
        blksize: kani::any(),
        blocks: kani::any(),
        atime_ms: kani::any(),
        mtime_ms: kani::any(),
        ctime_ms: kani::any(),
        birthtime_ms: kani::any(),
    }
}

fn stats_fields_equal(left: &Stats, right: &Stats) -> bool {
    left.dev == right.dev
        && left.ino == right.ino
        && left.mode == right.mode
        && left.nlink == right.nlink
        && left.uid == right.uid
        && left.gid == right.gid
        && left.rdev == right.rdev
        && left.size == right.size
        && left.blksize == right.blksize
        && left.blocks == right.blocks
        && left.atime_ms == right.atime_ms
        && left.mtime_ms == right.mtime_ms
        && left.ctime_ms == right.ctime_ms
        && left.birthtime_ms == right.birthtime_ms
}

fn arbitrary_identity() -> PhysicalInodeIdentity {
    PhysicalInodeIdentity {
        incarnation: kani::any(),
        epoch: kani::any(),
        revision: kani::any(),
    }
}

fn identity_fields_equal(left: PhysicalInodeIdentity, right: PhysicalInodeIdentity) -> bool {
    left.incarnation == right.incarnation
        && left.epoch == right.epoch
        && left.revision == right.revision
}

fn arbitrary_extent() -> BlockExtent {
    BlockExtent {
        file_offset: kani::any(),
        block: BlockId(if kani::any::<bool>() { "p" } else { "q" }.into()),
        block_offset: kani::any(),
        length: kani::any(),
    }
}

fn arbitrary_file_node() -> NodeMetadata {
    let count: u8 = kani::any();
    kani::assume(count <= 2);
    let first = arbitrary_extent();
    let second = arbitrary_extent();
    let extents = match count {
        0 => vec![],
        1 => vec![first],
        2 => vec![first, second],
        _ => unreachable!(),
    };
    NodeMetadata {
        stats: arbitrary_stats(),
        data: NodeData::File(FileLayout {
            chunker: ChunkerConfig {
                algorithm: if kani::any::<bool>() {
                    "fixed-size"
                } else {
                    "future"
                }
                .into(),
                version: kani::any(),
                parameters: BTreeMap::from([("chunk_size".into(), kani::any::<u64>())]),
            },
            extents,
        }),
    }
}

/// Complete equality for the declared file input shape. This does not use
/// NodeMetadata/FileLayout/Stats equality or implement any acceptance decision.
fn file_fields_equal(left: &NodeMetadata, right: &NodeMetadata) -> bool {
    let (NodeData::File(left_layout), NodeData::File(right_layout)) = (&left.data, &right.data)
    else {
        return false;
    };
    if !stats_fields_equal(&left.stats, &right.stats)
        || left_layout.chunker.algorithm != right_layout.chunker.algorithm
        || left_layout.chunker.version != right_layout.chunker.version
        || left_layout.chunker.parameters.len() != right_layout.chunker.parameters.len()
        || left_layout.chunker.parameters.get("chunk_size")
            != right_layout.chunker.parameters.get("chunk_size")
        || left_layout.extents.len() != right_layout.extents.len()
    {
        return false;
    }
    // Each input has exactly one fixed parameter key and 0..=2 extents.
    // Explicit positions avoid adding a separate symbolic oracle loop.
    for position in [0_usize, 1] {
        if position < left_layout.extents.len() {
            let left = &left_layout.extents[position];
            let right = &right_layout.extents[position];
            if left.file_offset != right.file_offset
                || left.block.0.as_bytes() != right.block.0.as_bytes()
                || left.block_offset != right.block_offset
                || left.length != right.length
            {
                return false;
            }
        }
    }
    true
}

/// Exact bounded output comparison without derived equality over a whole
/// Result, guard, or inode. Directory fixtures contain at most three entries;
/// the file fixtures retain their complete two-extent comparison above.
fn node_fields_equal(left: &NodeMetadata, right: &NodeMetadata) -> bool {
    if !stats_fields_equal(&left.stats, &right.stats) {
        return false;
    }
    match (&left.data, &right.data) {
        (NodeData::File(_), NodeData::File(_)) => file_fields_equal(left, right),
        (NodeData::Directory { entries: left }, NodeData::Directory { entries: right }) => {
            if left.len() != right.len() {
                return false;
            }
            for position in 0..left.len() {
                if left[position].name.as_bytes() != right[position].name.as_bytes()
                    || left[position].inode != right[position].inode
                {
                    return false;
                }
            }
            true
        }
        (NodeData::Symlink { target: left }, NodeData::Symlink { target: right }) => {
            left.as_bytes() == right.as_bytes()
        }
        (NodeData::Special, NodeData::Special) => true,
        _ => false,
    }
}

fn assert_guard_fields_equal(left: &CompactGuard, right: &CompactGuard) {
    assert!(identity_fields_equal(left.identity, right.identity));
    assert!(node_fields_equal(&left.node, &right.node));
}

fn assert_loaded_fields_equal(left: &LoadedCompactInode, right: &LoadedCompactInode) {
    assert_eq!(left.generation, right.generation);
    assert_guard_fields_equal(&left.guard, &right.guard);
}

fn plain_stats(inode: InodeId, mode: u32, links: u64, size: u64) -> Stats {
    Stats {
        dev: 7,
        ino: inode,
        mode,
        nlink: links,
        uid: 10,
        gid: 20,
        rdev: 0,
        size,
        blksize: 4096,
        blocks: u64::from(size != 0),
        atime_ms: -5,
        mtime_ms: 0,
        ctime_ms: 0,
        birthtime_ms: -10,
    }
}

fn populated_file(inode: InodeId, links: u64) -> NodeMetadata {
    NodeMetadata {
        stats: plain_stats(inode, S_IFREG | 0o644, links, 4),
        data: NodeData::File(FileLayout {
            chunker: fixed_chunker(4096),
            extents: vec![BlockExtent {
                file_offset: 2,
                block: BlockId("p".into()),
                block_offset: 1,
                length: 2,
            }],
        }),
    }
}

fn anchor(generation: u64) -> CompactAnchor {
    CompactAnchor {
        backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
        generation,
        root: 1,
        next_inode: 3,
        default_uid: 10,
        default_gid: 20,
        umask: 0o022,
        default_chunker: fixed_chunker(4096),
        members: vec![1, 2],
    }
}

fn assert_same_error(actual: &FsError, ordinary: &FsError) {
    assert_eq!(actual.code, ordinary.code);
    assert_eq!(actual.to_string(), ordinary.to_string());
    assert_eq!(actual.syscall, ordinary.syscall);
    assert_eq!(actual.path, ordinary.path);
    assert_eq!(actual.dest, ordinary.dest);
}

/// Every Stats field on the independent branch may differ, and the two-extent
/// shape includes complete sparse layout/chunker/block-reference comparison.
/// The clone branch only makes exact-match covers cheaper to solve; the
/// independent branch retains every pair in the declared domain.
#[kani::proof]
#[kani::unwind(128)]
fn compact_materialized_file_match_and_fallback() {
    let generation: u64 = kani::any();
    kani::assume(generation > 0);
    let anchor = anchor(generation);
    let fresh_identity = arbitrary_identity();
    kani::assume(
        fresh_identity.incarnation > 0
            && fresh_identity.incarnation <= fresh_identity.epoch
            && fresh_identity.epoch <= generation,
    );
    let fresh = CompactGuard {
        identity: fresh_identity,
        node: arbitrary_file_node(),
    };
    let force_match: bool = kani::any();
    let expected_node = if force_match {
        fresh.node.clone()
    } else {
        arbitrary_file_node()
    };
    let expected_generation = if force_match { generation } else { kani::any() };
    let expected_identity = if force_match {
        fresh_identity
    } else {
        arbitrary_identity()
    };
    let supplied_backing = if force_match || kani::any::<bool>() {
        anchor.backing
    } else {
        ConcurrentBackingId::from_bytes([2; 16]).unwrap()
    };
    let complete_equal = file_fields_equal(&fresh.node, &expected_node);
    let comparison_matches = complete_equal
        && expected_generation == generation
        && identity_fields_equal(fresh_identity, expected_identity)
        && supplied_backing == anchor.backing
        && expected_node.stats.ino == 2;
    let ordinary = LoadedCompactInode::from_guard(&anchor, 2, fresh.clone());
    let actual = CompactInodeRead::from_materialized_guard(
        &anchor,
        supplied_backing,
        2,
        fresh.clone(),
        CompactInodeExpectation::selected(expected_generation, expected_identity, &expected_node),
    );
    let ordinary_valid = ordinary.is_ok();
    let observed_unchanged = matches!(&actual, Ok(CompactInodeRead::Unchanged(_)));
    assert_eq!(observed_unchanged, ordinary_valid && comparison_matches);

    match (actual, ordinary) {
        (Ok(CompactInodeRead::Unchanged(checked)), Ok(loaded)) => {
            assert_eq!(checked.generation(), loaded.generation);
            assert_eq!(checked.generation(), generation);
            assert_eq!(checked.inode(), 2);
            assert!(identity_fields_equal(checked.identity(), fresh_identity));
            // A receipt contains no Stats: these assertions prove the captured
            // expected fields can be reused only because they equal fresh data.
            assert!(stats_fields_equal(
                &loaded.guard.node.stats,
                &expected_node.stats
            ));
            assert!(file_fields_equal(&loaded.guard.node, &expected_node));
            assert!(checked.into_verified_root().is_none());
        }
        (Ok(CompactInodeRead::Loaded(loaded)), Ok(ordinary)) => {
            assert_loaded_fields_equal(&loaded, &ordinary);
            assert_eq!(loaded.generation, generation);
            assert!(identity_fields_equal(loaded.guard.identity, fresh_identity));
            assert!(stats_fields_equal(
                &loaded.guard.node.stats,
                &fresh.node.stats
            ));
            assert!(file_fields_equal(&loaded.guard.node, &fresh.node));
        }
        (Err(actual), Err(ordinary)) => assert_same_error(&actual, &ordinary),
        _ => panic!("factory classification differs from real ordinary validation"),
    }

    kani::cover!(ordinary_valid && observed_unchanged);
    kani::cover!(ordinary_valid && observed_unchanged && fresh_identity.epoch < generation);
    kani::cover!(
        ordinary_valid && !observed_unchanged && fresh.node.stats.uid != expected_node.stats.uid
    );
    kani::cover!(
        ordinary_valid && !observed_unchanged && fresh.node.stats.size != expected_node.stats.size
    );
    kani::cover!(
        ordinary_valid
            && !observed_unchanged
            && fresh.node.stats.nlink != expected_node.stats.nlink
    );
    kani::cover!(
        ordinary_valid
            && !observed_unchanged
            && fresh.node.stats.atime_ms != expected_node.stats.atime_ms
    );
    kani::cover!(ordinary_valid && !observed_unchanged && expected_generation != generation);
    kani::cover!(
        ordinary_valid
            && !observed_unchanged
            && !identity_fields_equal(fresh_identity, expected_identity)
    );
    kani::cover!(ordinary_valid && !observed_unchanged && supplied_backing != anchor.backing);
    kani::cover!(!ordinary_valid);
    kani::cover!(
        ordinary_valid
            && observed_unchanged
            && matches!(&fresh.node.data, NodeData::File(layout) if layout.extents.len() == 2)
    );
}

/// Exact expected errors are contractual fixtures, independent of either
/// validator's observed result. All malformed cases use an equal malformed
/// hint, so speculative eligibility checks cannot hide the ordinary priority.
/// Valid unsealed directories/symlinks/specials must retain owned fallback.
#[kani::proof]
#[kani::unwind(128)]
fn compact_materialized_fallback_preserves_error_precedence() {
    let fault: u8 = kani::any();
    kani::assume(fault <= 20);
    let mut anchor = anchor(3);
    let mut guard = CompactGuard {
        identity: PhysicalInodeIdentity {
            incarnation: 1,
            epoch: 3,
            revision: 4,
        },
        node: populated_file(2, 1),
    };
    let expected_error: Option<(ErrorCode, &str)> = match fault {
        0 => None,
        1 => {
            anchor
                .default_chunker
                .parameters
                .insert("chunk_size".into(), 0);
            anchor.generation = 0;
            guard.identity.incarnation = 0;
            guard.node.stats.ino = 99;
            Some((ErrorCode::Einval, "chunk size must be positive"))
        }
        2 => {
            anchor.generation = 0;
            guard.identity.incarnation = 0;
            guard.node.stats.ino = 99;
            Some((
                ErrorCode::Einval,
                "invalid compact anchor identity/membership",
            ))
        }
        3 => {
            anchor.members.pop();
            guard.identity.incarnation = 0;
            guard.node.stats.ino = 99;
            Some((
                ErrorCode::Einval,
                "selected guard absent from compact membership",
            ))
        }
        4 => {
            guard.identity.incarnation = 0;
            guard.node.stats.ino = 99;
            guard.node.stats.mode = S_IFDIR | 0o755;
            Some((
                ErrorCode::Einval,
                "invalid compact physical epoch/incarnation",
            ))
        }
        5 => {
            guard.node.stats.ino = 99;
            guard.node.stats.mode = S_IFDIR | 0o755;
            Some((ErrorCode::Einval, "compact guard inode mismatch"))
        }
        6 => {
            guard.node.stats.mode = S_IFDIR | 0o755;
            let NodeData::File(layout) = &mut guard.node.data else {
                unreachable!()
            };
            layout.extents[0].length = 0;
            Some((
                ErrorCode::Einval,
                "node metadata kind does not match stats.mode",
            ))
        }
        7 => {
            let NodeData::File(layout) = &mut guard.node.data else {
                unreachable!()
            };
            layout.chunker.parameters.insert("chunk_size".into(), 0);
            layout.extents[0].length = 0;
            Some((ErrorCode::Einval, "chunk size must be positive"))
        }
        8 => {
            let NodeData::File(layout) = &mut guard.node.data else {
                unreachable!()
            };
            layout.extents[0].length = 0;
            Some((ErrorCode::Einval, "file extents must be nonempty"))
        }
        9 => {
            let NodeData::File(layout) = &mut guard.node.data else {
                unreachable!()
            };
            layout.extents[0].file_offset = u64::MAX;
            Some((ErrorCode::Eoverflow, "file extent offset overflows"))
        }
        10 => {
            let NodeData::File(layout) = &mut guard.node.data else {
                unreachable!()
            };
            layout.extents[0].block_offset = u64::MAX;
            Some((ErrorCode::Eoverflow, "block extent offset overflows"))
        }
        11 => {
            let NodeData::File(layout) = &mut guard.node.data else {
                unreachable!()
            };
            layout.extents[0].block.0.clear();
            Some((ErrorCode::Einval, "file extents must name a block"))
        }
        12 => {
            guard.node.stats.size = 0;
            Some((ErrorCode::Einval, "file extent exceeds file size"))
        }
        13 => {
            let NodeData::File(layout) = &mut guard.node.data else {
                unreachable!()
            };
            layout.chunker.version = 2;
            layout.extents[0].length = 0;
            Some((ErrorCode::Enotsup, "unsupported chunker algorithm/version"))
        }
        14 => {
            anchor.default_chunker.version = 2;
            anchor.generation = 0;
            guard.identity.incarnation = 0;
            Some((ErrorCode::Enotsup, "unsupported chunker algorithm/version"))
        }
        15 => {
            anchor.members.push(u64::MAX);
            guard.identity.incarnation = 0;
            Some((ErrorCode::Eoverflow, "compact inode allocation exhausted"))
        }
        16 => {
            guard.node.stats.mode = S_IFDIR | 0o755;
            guard.node.stats.nlink = 2;
            guard.node.data = NodeData::Directory { entries: vec![] };
            None
        }
        17 => {
            guard.node.stats.mode = S_IFLNK | 0o777;
            guard.node.data = NodeData::Symlink { target: "x".into() };
            None
        }
        18 => {
            let kind: u8 = kani::any();
            kani::assume(kind <= 3);
            guard.node.stats.mode = match kind {
                0 => S_IFBLK,
                1 => S_IFCHR,
                2 => S_IFIFO,
                3 => S_IFSOCK,
                _ => unreachable!(),
            };
            guard.node.data = NodeData::Special;
            None
        }
        19 => {
            guard.node.stats.mode = S_IFDIR | 0o755;
            guard.node.data = NodeData::Directory {
                entries: vec![DirectoryEntry {
                    name: "bad/name".into(),
                    inode: 1,
                }],
            };
            Some((ErrorCode::Einval, "invalid directory entry name"))
        }
        20 => {
            guard.node.stats.mode = S_IFDIR | 0o755;
            guard.node.data = NodeData::Directory {
                entries: vec![
                    DirectoryEntry {
                        name: "a".into(),
                        inode: 1,
                    },
                    DirectoryEntry {
                        name: "a".into(),
                        inode: 1,
                    },
                ],
            };
            Some((ErrorCode::Einval, "invalid compact directory entries"))
        }
        _ => unreachable!(),
    };
    let retained = guard.clone();
    let hint = CompactInodeExpectation::selected(anchor.generation, guard.identity, &retained.node);
    let ordinary = LoadedCompactInode::from_guard(&anchor, 2, guard.clone());
    let actual = CompactInodeRead::from_materialized_guard(&anchor, anchor.backing, 2, guard, hint);
    let actual_failed = actual.is_err();
    let actual_unchanged = matches!(&actual, Ok(CompactInodeRead::Unchanged(_)));
    match (expected_error, actual, ordinary) {
        (Some((code, message)), Err(actual), Err(ordinary)) => {
            assert_same_error(&actual, &ordinary);
            assert_eq!(actual.code, code);
            assert_eq!(actual.to_string(), message);
        }
        (None, Ok(CompactInodeRead::Unchanged(checked)), Ok(ordinary)) if fault == 0 => {
            assert_eq!(checked.generation(), ordinary.generation);
            assert_eq!(checked.inode(), 2);
            assert_eq!(checked.identity(), retained.identity);
            assert!(checked.into_verified_root().is_none());
        }
        (None, Ok(CompactInodeRead::Loaded(actual)), Ok(ordinary))
            if fault >= 16 && fault <= 18 =>
        {
            assert_loaded_fields_equal(&actual, &ordinary);
            assert_guard_fields_equal(&actual.guard, &retained);
        }
        _ => panic!("factory changed the declared ordinary error/fallback contract"),
    }

    kani::cover!(fault == 0 && actual_unchanged);
    kani::cover!(fault == 1 && actual_failed);
    kani::cover!(fault == 2 && actual_failed);
    kani::cover!(fault == 3 && actual_failed);
    kani::cover!(fault == 4 && actual_failed);
    kani::cover!(fault == 5 && actual_failed);
    kani::cover!(fault == 6 && actual_failed);
    kani::cover!(fault == 7 && actual_failed);
    kani::cover!(fault == 8 && actual_failed);
    kani::cover!(fault == 9 && actual_failed);
    kani::cover!(fault == 10 && actual_failed);
    kani::cover!(fault == 11 && actual_failed);
    kani::cover!(fault == 12 && actual_failed);
    kani::cover!(fault == 13 && actual_failed);
    kani::cover!(fault == 14 && actual_failed);
    kani::cover!(fault == 15 && actual_failed);
    kani::cover!(fault == 16 && !actual_unchanged && !actual_failed);
    kani::cover!(fault == 17 && !actual_unchanged && !actual_failed);
    kani::cover!(fault == 18 && !actual_unchanged && !actual_failed);
    kani::cover!(fault == 19 && actual_failed);
    kani::cover!(fault == 20 && actual_failed);
}

fn root_snapshot(generation: u64, duplicate: bool) -> CompactSnapshot {
    let identity = PhysicalInodeIdentity {
        incarnation: 1,
        epoch: generation,
        revision: 4,
    };
    let mut entries = vec![
        DirectoryEntry {
            name: "c".into(),
            inode: 3,
        },
        DirectoryEntry {
            name: "a".into(),
            inode: 2,
        },
    ];
    if duplicate {
        entries.push(DirectoryEntry {
            name: "b".into(),
            inode: 2,
        });
    }
    let root = CompactGuard {
        identity,
        node: NodeMetadata {
            stats: plain_stats(1, S_IFDIR | 0o755, 2, 0),
            data: NodeData::Directory { entries },
        },
    };
    let mut anchor = anchor(generation);
    anchor.members = vec![1, 2, 3, 4];
    anchor.next_inode = 5;
    CompactSnapshot {
        anchor,
        guards: BTreeMap::from([
            (1, root),
            (
                2,
                CompactGuard {
                    identity,
                    node: populated_file(2, if duplicate { 2 } else { 1 }),
                },
            ),
            (
                3,
                CompactGuard {
                    identity,
                    node: populated_file(3, 1),
                },
            ),
            (
                4,
                CompactGuard {
                    identity,
                    node: populated_file(4, 0),
                },
            ),
        ]),
    }
}

/// The receipt originates in a real full audit, not a fabricated structural
/// value. Drift affects only fresh well-formed anchor fields; no equality to
/// the old full anchor or fresh_root == selected_inode is imposed.
#[kani::proof]
#[kani::unwind(128)]
fn compact_materialized_audited_root_and_provenance() {
    let generation: u64 = kani::any();
    kani::assume(generation > 0);
    let duplicate: bool = kani::any();
    let snapshot = root_snapshot(generation, duplicate);
    let (_, _, audited) = snapshot.clone().into_validated_namespace().unwrap();
    let (_, _, other) = snapshot.into_validated_namespace().unwrap();
    assert!(!audited.same_witness(&other));
    assert_eq!(audited.root_children.as_ref(), &[2, 3]);

    let mut fresh_anchor = audited.anchor().clone();
    let backing_drift: bool = kani::any();
    if backing_drift {
        fresh_anchor.backing = ConcurrentBackingId::from_bytes([2; 16]).unwrap();
    }
    let root_drift: bool = kani::any();
    if root_drift {
        // A present zero-link member is a legal typed anchor root even though
        // the old audited graph's root remains selected. No fresh graph claim.
        fresh_anchor.root = 4;
    }
    fresh_anchor.default_uid = kani::any();
    fresh_anchor.default_gid = kani::any();
    fresh_anchor.umask = kani::any();
    let default_chunk_size: u64 = kani::any();
    kani::assume(default_chunk_size > 0);
    // A legal fresh default chunk size must be representable by the target.
    // This retains every positive u64 size on the 64-bit qualification target.
    kani::assume(u128::from(default_chunk_size) <= usize::MAX as u128);
    fresh_anchor.default_chunker = fixed_chunker(default_chunk_size);
    let unused_member: bool = kani::any();
    if unused_member {
        let extra: u64 = kani::any();
        kani::assume(extra > 4 && extra < u64::MAX);
        fresh_anchor.members.push(extra);
    }
    let next_inode: u64 = kani::any();
    kani::assume(next_inode > *fresh_anchor.members.last().unwrap());
    fresh_anchor.next_inode = next_inode;

    let read = CompactInodeRead::from_materialized_guard(
        &fresh_anchor,
        fresh_anchor.backing,
        1,
        audited.audited_root().clone(),
        audited.expect_root(),
    )
    .unwrap();
    let CompactInodeRead::Unchanged(checked) = read else {
        panic!("valid fresh anchor drift incorrectly prevents an exact root match");
    };
    assert_eq!(checked.generation(), generation);
    assert_eq!(checked.inode(), 1);
    assert_eq!(checked.identity(), audited.audited_root().identity);
    let verified = checked.into_verified_root().expect("audited root receipt");
    assert_eq!(verified.generation(), generation);
    assert_guard_fields_equal(verified.guard(), audited.audited_root());
    assert!(verified.structure.same_witness(&audited));
    assert!(!verified.structure.same_witness(&other));
    let created = NodeMetadata {
        stats: plain_stats(other.anchor().next_inode, S_IFREG | 0o644, 1, 0),
        data: NodeData::File(FileLayout {
            chunker: other.anchor().default_chunker.clone(),
            extents: vec![],
        }),
    };
    let foreign = CompactRootFileCreate::capture_verified(
        &other,
        &verified,
        other.audited_root().identity,
        "z".into(),
        created,
        1,
        1,
    )
    .unwrap_err();
    assert_eq!(foreign.code, ErrorCode::Einval);
    assert_eq!(
        foreign.to_string(),
        "verified compact root differs from audited graph"
    );

    let mutation: u8 = kani::any();
    kani::assume(mutation <= 7);
    let mut changed_anchor = fresh_anchor.clone();
    let mut changed_guard = audited.audited_root().clone();
    let mut supplied_backing = changed_anchor.backing;
    let expected_error: Option<(ErrorCode, &str)> = match mutation {
        0 => None,
        1 => {
            changed_guard.node.stats.uid ^= 1;
            None
        }
        2 => {
            let NodeData::Directory { entries } = &mut changed_guard.node.data else {
                unreachable!();
            };
            entries.swap(0, 1);
            None
        }
        3 => {
            changed_anchor.members.remove(1);
            Some((ErrorCode::Einval, "invalid compact directory entries"))
        }
        4 => {
            changed_anchor.generation = if generation == u64::MAX {
                generation - 1
            } else {
                generation + 1
            };
            changed_guard.identity.epoch =
                changed_guard.identity.epoch.min(changed_anchor.generation);
            None
        }
        5 => {
            supplied_backing =
                ConcurrentBackingId::from_bytes(if changed_anchor.backing.as_bytes() == [1; 16] {
                    [2; 16]
                } else {
                    [1; 16]
                })
                .unwrap();
            None
        }
        6 => {
            let NodeData::Directory { entries } = &mut changed_guard.node.data else {
                unreachable!();
            };
            entries[0].name = "bad/name".into();
            Some((ErrorCode::Einval, "invalid directory entry name"))
        }
        7 => {
            changed_anchor.root = 4;
            changed_anchor.members.remove(0);
            Some((
                ErrorCode::Einval,
                "selected guard absent from compact membership",
            ))
        }
        _ => unreachable!(),
    };
    let retained = changed_guard.clone();
    let ordinary = LoadedCompactInode::from_guard(&changed_anchor, 1, changed_guard.clone());
    let actual = CompactInodeRead::from_materialized_guard(
        &changed_anchor,
        supplied_backing,
        1,
        changed_guard,
        audited.expect_root(),
    );
    let observed_unchanged = matches!(&actual, Ok(CompactInodeRead::Unchanged(_)));
    let observed_error = actual.is_err();
    assert_eq!(observed_unchanged, mutation == 0);
    match (expected_error, actual, ordinary) {
        (Some((code, message)), Err(actual), Err(ordinary)) => {
            assert_same_error(&actual, &ordinary);
            assert_eq!(actual.code, code);
            assert_eq!(actual.to_string(), message);
        }
        (None, Ok(CompactInodeRead::Unchanged(checked)), Ok(ordinary)) if mutation == 0 => {
            assert_eq!(checked.generation(), ordinary.generation);
            assert_eq!(checked.identity(), retained.identity);
            let root = checked.into_verified_root().unwrap();
            assert!(root.structure.same_witness(&audited));
        }
        (None, Ok(CompactInodeRead::Loaded(actual)), Ok(ordinary)) if mutation != 0 => {
            assert_loaded_fields_equal(&actual, &ordinary);
            assert_guard_fields_equal(&actual.guard, &retained);
        }
        _ => panic!("audited root factory changed the real ordinary contract"),
    }

    kani::cover!(duplicate);
    kani::cover!(!duplicate);
    kani::cover!(root_drift && fresh_anchor.root != 1);
    kani::cover!(backing_drift && fresh_anchor.backing != audited.anchor().backing);
    kani::cover!(unused_member);
    kani::cover!(fresh_anchor.default_uid != audited.anchor().default_uid);
    kani::cover!(fresh_anchor.default_gid != audited.anchor().default_gid);
    kani::cover!(fresh_anchor.umask != audited.anchor().umask);
    kani::cover!(default_chunk_size != 4096);
    kani::cover!(fresh_anchor.next_inode != audited.anchor().next_inode);
    kani::cover!(mutation == 0 && observed_unchanged);
    kani::cover!(mutation == 1 && !observed_unchanged && !observed_error);
    kani::cover!(mutation == 2 && !observed_unchanged && !observed_error);
    kani::cover!(mutation == 3 && observed_error);
    kani::cover!(mutation == 4 && !observed_unchanged && !observed_error);
    kani::cover!(mutation == 5 && !observed_unchanged && !observed_error);
    kani::cover!(mutation == 6 && observed_error);
    kani::cover!(mutation == 7 && observed_error);
}
