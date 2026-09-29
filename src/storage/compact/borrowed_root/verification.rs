//! Bounded checks of the actual complete-root streaming stages.
//!
//! Every witness below originates in CompactSnapshot::into_validated_namespace.
//! No stage, audit, or receipt is fabricated and no production decision is
//! stubbed. The streams are finite logical observations: at most six members
//! and four dentries, over a real four-inode graph with fixed IDs 1..=4,
//! optional hardlinks, and an optional empty root. Fresh scalar observations
//! retain their full width; audited identity/root Stats obey the real audit's
//! invariants. Names are the declared finite UTF8 fixtures, including a
//! same-length changed Unicode name. Arbitrary graph sizes and near-u64::MAX
//! observed row counts are outside these finite-stream proofs.
//!
//! These proofs do not establish that observations came from SQL, that EOF was
//! reached, or that rollback succeeded. They prove cursor decisions for the
//! complete supplied finite stream only. Packet/hash validation, transaction
//! provenance, cancellation, allocations, durability, and capacity remain
//! separate provider/driver/runtime obligations. Unwind annotations require
//! strict execution checks; source presence is not verified proof evidence.

use super::super::{CompactAnchor, CompactGuard, CompactRootFileCreate, CompactSnapshot};
use super::*;
use crate::storage::{DirectoryEntry, FileLayout, NodeMetadata};
use crate::{ErrorCode, S_IFDIR, S_IFMT, S_IFREG, Stats};
use std::collections::BTreeMap;

const C: &str = "c-💚";
const A: &str = "a-💚";
const B: &str = "b-💚";
const CHANGED_C: &str = "c-💙";

fn fixed_chunker(size: u64) -> crate::chunking::ChunkerConfig {
    crate::chunking::ChunkerConfig {
        algorithm: "fixed-size".into(),
        version: 1,
        parameters: BTreeMap::from([("chunk_size".into(), size)]),
    }
}

fn plain_stats(inode: u64, mode: u32, nlink: u64) -> Stats {
    Stats {
        dev: 7,
        ino: inode,
        mode,
        nlink,
        uid: 10,
        gid: 20,
        rdev: 0,
        size: 0,
        blksize: 4096,
        blocks: 0,
        atime_ms: -5,
        mtime_ms: 0,
        ctime_ms: 0,
        birthtime_ms: -10,
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

fn identity_fields_equal(left: PhysicalInodeIdentity, right: PhysicalInodeIdentity) -> bool {
    left.incarnation == right.incarnation
        && left.epoch == right.epoch
        && left.revision == right.revision
}

fn arbitrary_identity() -> PhysicalInodeIdentity {
    PhysicalInodeIdentity {
        incarnation: kani::any(),
        epoch: kani::any(),
        revision: kani::any(),
    }
}

/// A full graph, not merely an equal root body. The zero-link file at 4 is an
/// audited unused member. Empty roots retain 2/3 as additional zero-link files.
fn snapshot(
    generation: u64,
    hardlinks: bool,
    empty: bool,
    root_stats: Stats,
    identity: PhysicalInodeIdentity,
) -> CompactSnapshot {
    let entries = if empty {
        vec![]
    } else {
        let mut entries = vec![
            DirectoryEntry {
                name: C.into(),
                inode: 3,
            },
            DirectoryEntry {
                name: A.into(),
                inode: 2,
            },
        ];
        if hardlinks {
            entries.push(DirectoryEntry {
                name: B.into(),
                inode: 2,
            });
        }
        entries
    };
    let file = |inode, links| CompactGuard {
        identity,
        node: NodeMetadata {
            stats: plain_stats(inode, S_IFREG | 0o644, links),
            data: NodeData::File(FileLayout {
                chunker: fixed_chunker(4096),
                extents: vec![],
            }),
        },
    };
    CompactSnapshot {
        anchor: CompactAnchor {
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            generation,
            root: 1,
            next_inode: 5,
            default_uid: 10,
            default_gid: 20,
            umask: 0o022,
            default_chunker: fixed_chunker(4096),
            members: vec![1, 2, 3, 4],
        },
        guards: BTreeMap::from([
            (
                1,
                CompactGuard {
                    identity,
                    node: NodeMetadata {
                        stats: root_stats,
                        data: NodeData::Directory { entries },
                    },
                },
            ),
            (
                2,
                file(
                    2,
                    if empty {
                        0
                    } else if hardlinks {
                        2
                    } else {
                        1
                    },
                ),
            ),
            (3, file(3, u64::from(!empty))),
            (4, file(4, 0)),
        ]),
    }
}

fn fixed_identity(generation: u64) -> PhysicalInodeIdentity {
    PhysicalInodeIdentity {
        incarnation: 1,
        epoch: generation,
        revision: 4,
    }
}

fn complete_members<'audit>(
    witness: &'audit ValidatedCompactStructure,
) -> CompactRootGuardCursor<'audit> {
    let authority = CompactAuthority::from_anchor(witness.anchor()).unwrap();
    let mut members =
        CompactRootMemberCursor::begin(&authority, authority.backing, 1, witness.expect_root())
            .unwrap();
    for &inode in &witness.anchor().members {
        members.observe_member(inode);
    }
    members.finish_members().unwrap()
}

fn entries(witness: &ValidatedCompactStructure) -> &[DirectoryEntry] {
    let NodeData::Directory { entries } = &witness.audited_root().node.data else {
        panic!("full audit did not retain a directory root");
    };
    entries
}

/// Independent scalar/order oracle for this finite stream. It does not call
/// the authority, anchor, member-stage, or materialized-factory validators.
fn member_oracle(
    authority: &CompactAuthority,
    supplied_backing: ConcurrentBackingId,
    selected_inode: u64,
    generation: u64,
    sealed_hint: bool,
    empty: bool,
    rows: &[u64],
) -> bool {
    let chunk_size = authority.default_chunker.parameters["chunk_size"];
    if !sealed_hint
        || selected_inode != 1
        || authority.backing != supplied_backing
        || authority.generation != generation
        || authority.default_chunker.algorithm != "fixed-size"
        || authority.default_chunker.version != 1
        || chunk_size == 0
        || u128::from(chunk_size) > usize::MAX as u128
        || authority.generation == 0
        || authority.root == 0
        || authority.root >= authority.next_inode
        || authority.members_count == 0
        || authority.members_count >= authority.next_inode
        || rows.len() as u64 != authority.members_count
    {
        return false;
    }
    let mut saw_current_root = false;
    let mut saw_selected_root = false;
    let mut saw_2 = false;
    let mut saw_3 = false;
    for index in 0..rows.len() {
        let inode = rows[index];
        if inode == 0 || inode >= authority.next_inode || (index != 0 && rows[index - 1] >= inode) {
            return false;
        }
        saw_current_root |= inode == authority.root;
        saw_selected_root |= inode == 1;
        saw_2 |= inode == 2;
        saw_3 |= inode == 3;
    }
    saw_current_root && saw_selected_root && (empty || (saw_2 && saw_3))
}

/// Arbitrary complete streams with at most six full-width inode observations,
/// plus a correlated valid branch so legal drift and empty/hardlinked roots
/// have cheap positive witnesses. The arbitrary branch remains unrestricted.
#[kani::proof]
#[kani::unwind(128)]
fn compact_borrowed_root_members_match_complete_authority() {
    let generation: u64 = kani::any();
    kani::assume(generation > 0);
    let hardlinks: bool = kani::any();
    let empty: bool = kani::any();
    let (_, _, witness) = snapshot(
        generation,
        hardlinks,
        empty,
        plain_stats(1, S_IFDIR | 0o755, 2),
        fixed_identity(generation),
    )
    .into_validated_namespace()
    .unwrap();
    let force_valid: bool = kani::any();
    let remove_unused: bool = kani::any();
    let add_unused: bool = kani::any();
    let root_drift: bool = kani::any();
    let backing_drift: bool = kani::any();
    let sealed_hint = force_valid || kani::any::<bool>();
    let mut authority = CompactAuthority::from_anchor(witness.anchor()).unwrap();
    authority.backing =
        ConcurrentBackingId::from_bytes(if backing_drift { [2; 16] } else { [1; 16] }).unwrap();
    authority.default_uid = kani::any();
    authority.default_gid = kani::any();
    authority.umask = kani::any();
    let default_size: u64 = kani::any();
    authority.default_chunker = fixed_chunker(default_size);
    let mut rows = vec![];
    let supplied_backing;
    let selected_inode;
    if force_valid {
        kani::assume(default_size > 0 && u128::from(default_size) <= usize::MAX as u128);
        rows = vec![1, 2, 3];
        if !remove_unused {
            rows.push(4);
        }
        if add_unused {
            let extra: u64 = kani::any();
            kani::assume(extra > 4 && extra < u64::MAX);
            rows.push(extra);
        }
        authority.root = if root_drift { 3 } else { 1 };
        authority.members_count = rows.len() as u64;
        authority.next_inode = kani::any();
        kani::assume(authority.next_inode > *rows.last().unwrap());
        supplied_backing = authority.backing;
        selected_inode = 1;
    } else {
        let count: usize = kani::any();
        kani::assume(count <= 6);
        for _ in 0..count {
            rows.push(kani::any::<u64>());
        }
        authority.generation = kani::any();
        authority.root = kani::any();
        authority.members_count = kani::any();
        authority.next_inode = kani::any();
        authority.default_chunker.algorithm = if kani::any::<bool>() {
            "fixed-size"
        } else {
            "future"
        }
        .into();
        authority.default_chunker.version = kani::any();
        supplied_backing = ConcurrentBackingId::from_bytes(if kani::any::<bool>() {
            [1; 16]
        } else {
            [2; 16]
        })
        .unwrap();
        selected_inode = kani::any();
    }
    let expected = if sealed_hint {
        witness.expect_root()
    } else {
        CompactInodeExpectation::selected(
            generation,
            witness.audited_root().identity,
            &witness.audited_root().node,
        )
    };
    let oracle = member_oracle(
        &authority,
        supplied_backing,
        selected_inode,
        generation,
        sealed_hint,
        empty,
        &rows,
    );
    let initial =
        CompactRootMemberCursor::begin(&authority, supplied_backing, selected_inode, expected);
    let began = initial.is_some();
    let actual = initial
        .and_then(|mut cursor| {
            for &inode in &rows {
                cursor.observe_member(inode);
            }
            cursor.finish_members()
        })
        .is_some();
    let physically_valid_rows = rows
        .iter()
        .all(|&inode| inode > 0 && inode < authority.next_inode)
        && rows.windows(2).all(|pair| pair[0] < pair[1]);
    assert_eq!(actual, oracle);
    assert!(!force_valid || actual);

    kani::cover!(force_valid && actual && empty);
    kani::cover!(force_valid && actual && !empty && hardlinks);
    kani::cover!(force_valid && actual && remove_unused && !add_unused);
    kani::cover!(
        force_valid && actual && remove_unused && !add_unused && authority.next_inode == 4
    );
    kani::cover!(force_valid && actual && add_unused);
    kani::cover!(force_valid && actual && root_drift && authority.root != 1);
    kani::cover!(force_valid && actual && backing_drift);
    kani::cover!(force_valid && actual && default_size != 4096);
    kani::cover!(force_valid && actual && authority.next_inode == u64::MAX);
    kani::cover!(!sealed_hint && !actual);
    kani::cover!(!force_valid && rows.is_empty() && !actual);
    kani::cover!(!force_valid && rows.len() == 6 && !actual);
    kani::cover!(began && rows.iter().any(|&inode| inode == 0) && !actual);
    kani::cover!(began && rows.iter().any(|&inode| inode == u64::MAX) && !actual);
    kani::cover!(began && rows.windows(2).any(|pair| pair[0] >= pair[1]) && !actual);
    kani::cover!(began && authority.members_count != rows.len() as u64 && !actual);
    kani::cover!(
        began
            && physically_valid_rows
            && authority.members_count == rows.len() as u64
            && !rows.contains(&authority.root)
            && !actual
    );
    kani::cover!(
        began
            && physically_valid_rows
            && authority.members_count == rows.len() as u64
            && rows.contains(&1)
            && rows.contains(&authority.root)
            && !empty
            && !rows.contains(&2)
            && !actual
    );
    kani::cover!(!force_valid && default_size == 0 && !actual);
    kani::cover!(!force_valid && supplied_backing != authority.backing && !actual);
}

#[derive(Clone, Copy)]
struct ObservedEntry {
    parent: u64,
    ordinal: u64,
    name: &'static str,
    inode: u64,
}

fn arbitrary_entry() -> ObservedEntry {
    let name: u8 = kani::any();
    kani::assume(name <= 4);
    ObservedEntry {
        parent: kani::any(),
        ordinal: kani::any(),
        name: match name {
            0 => C,
            1 => A,
            2 => B,
            3 => CHANGED_C,
            _ => "extra",
        },
        inode: kani::any(),
    }
}

fn observe_entries(cursor: &mut CompactRootDentryCursor<'_>, rows: &[ObservedEntry]) {
    for row in rows {
        cursor.observe_entry(BorrowedCompactRootEntry {
            parent: row.parent,
            ordinal: row.ordinal,
            name: row.name,
            inode: row.inode,
        });
    }
}

/// Guard identity, every Stats field, and each complete logical observation
/// are independent on the general branch. Exact-match inputs are also supplied
/// on a correlated branch. Allocator holes are legal; signed overflow is not.
#[kani::proof]
#[kani::unwind(128)]
fn compact_borrowed_root_guard_and_dentries_require_exact_match() {
    let generation: u64 = kani::any();
    kani::assume(generation > 0);
    let identity = arbitrary_identity();
    kani::assume(identity.incarnation > 0);
    kani::assume(identity.incarnation <= identity.epoch && identity.epoch <= generation);
    let mut root_stats = arbitrary_stats();
    root_stats.ino = 1;
    root_stats.mode = S_IFDIR | (root_stats.mode & !S_IFMT);
    root_stats.nlink = 2;
    let hardlinks: bool = kani::any();
    let empty: bool = kani::any();
    let (_, _, witness) = snapshot(generation, hardlinks, empty, root_stats, identity)
        .into_validated_namespace()
        .unwrap();
    let expected_entries = entries(&witness);
    let force_valid: bool = kani::any();
    let actual_inode = if force_valid { 1 } else { kani::any() };
    let actual_identity = if force_valid {
        identity
    } else {
        arbitrary_identity()
    };
    let header = CompactDirectoryHeader {
        stats: if force_valid {
            witness.audited_root().node.stats.clone()
        } else {
            arbitrary_stats()
        },
        entry_count: if force_valid {
            expected_entries.len() as u64
        } else {
            kani::any()
        },
        next_ordinal: kani::any(),
    };
    let mut rows = [
        arbitrary_entry(),
        arbitrary_entry(),
        arbitrary_entry(),
        arbitrary_entry(),
    ];
    let count: usize;
    if force_valid {
        count = expected_entries.len();
        kani::assume(header.next_ordinal >= count as u64 && header.next_ordinal <= i64::MAX as u64);
        for index in 0..count {
            rows[index].parent = 1;
            rows[index].name = match index {
                0 => C,
                1 => A,
                _ => B,
            };
            rows[index].inode = if index == 0 { 3 } else { 2 };
            kani::assume(rows[index].ordinal < header.next_ordinal);
            if index != 0 {
                kani::assume(rows[index - 1].ordinal < rows[index].ordinal);
            }
        }
    } else {
        count = kani::any();
        kani::assume(count <= 4);
    }
    // Clone/borrow comparisons above never validate the fresh observation.
    // Independent field predicates below specify the complete declared shape.
    let mut oracle = actual_inode == 1
        && identity_fields_equal(actual_identity, identity)
        && stats_fields_equal(&header.stats, &witness.audited_root().node.stats)
        && header.entry_count == expected_entries.len() as u64
        && header.entry_count <= header.next_ordinal
        && header.next_ordinal <= i64::MAX as u64
        && count == expected_entries.len();
    for index in 0..count {
        oracle &= index < expected_entries.len();
        if index < expected_entries.len() {
            oracle &= rows[index].parent == 1
                && rows[index].inode == expected_entries[index].inode
                && rows[index].name.as_bytes() == expected_entries[index].name.as_bytes()
                && rows[index].ordinal < header.next_ordinal
                && (index == 0 || rows[index - 1].ordinal < rows[index].ordinal);
        }
    }
    let initial = complete_members(&witness).observe_guard(actual_inode, actual_identity, &header);
    let guard_admitted = initial.is_some();
    let actual = initial.and_then(|mut cursor| {
        observe_entries(&mut cursor, &rows[..count]);
        cursor.finish_dentries()
    });
    let accepted = actual.is_some();
    assert_eq!(accepted, oracle);
    assert!(!force_valid || accepted);
    if let Some(checked) = actual {
        assert_eq!(checked.generation(), generation);
        assert_eq!(checked.inode(), 1);
        assert!(identity_fields_equal(checked.identity(), identity));
        assert!(
            checked
                .into_verified_root()
                .unwrap()
                .structure
                .same_witness(&witness)
        );
    }

    kani::cover!(force_valid && accepted && empty);
    kani::cover!(force_valid && accepted && !empty && hardlinks);
    kani::cover!(force_valid && accepted && identity.epoch < generation);
    kani::cover!(force_valid && accepted && count != 0 && rows[0].ordinal > 0);
    kani::cover!(force_valid && accepted && header.next_ordinal == i64::MAX as u64);
    kani::cover!(!force_valid && header.next_ordinal > i64::MAX as u64 && !accepted);
    kani::cover!(!force_valid && actual_inode != 1 && !accepted);
    kani::cover!(!force_valid && actual_identity.incarnation != identity.incarnation && !accepted);
    kani::cover!(!force_valid && actual_identity.epoch != identity.epoch && !accepted);
    kani::cover!(!force_valid && actual_identity.revision != identity.revision && !accepted);
    kani::cover!(
        !force_valid && header.stats.uid != witness.audited_root().node.stats.uid && !accepted
    );
    kani::cover!(
        !force_valid
            && header.stats.birthtime_ms != witness.audited_root().node.stats.birthtime_ms
            && !accepted
    );
    kani::cover!(guard_admitted && count != expected_entries.len() && !accepted);
    kani::cover!(guard_admitted && count != 0 && rows[0].name == CHANGED_C && !accepted);
    kani::cover!(guard_admitted && count >= 2 && rows[0].ordinal >= rows[1].ordinal && !accepted);
}

/// Every prefix and explicit-reject position in the declared streams is
/// exercised. Rejection cannot be repaired by later valid observations. A
/// successful receipt must retain the original audited Arcs, and an equal
/// independently allocated audit cannot consume it as its own.
#[kani::proof]
#[kani::unwind(128)]
fn compact_borrowed_root_stages_reject_incomplete_and_preserve_provenance() {
    let generation: u64 = kani::any();
    kani::assume(generation > 0);
    let hardlinks: bool = kani::any();
    let empty: bool = kani::any();
    let input = snapshot(
        generation,
        hardlinks,
        empty,
        plain_stats(1, S_IFDIR | 0o755, 2),
        fixed_identity(generation),
    );
    let (_, _, witness) = input.clone().into_validated_namespace().unwrap();
    let (_, _, foreign) = input.into_validated_namespace().unwrap();
    assert!(!witness.same_witness(&foreign));
    let authority = CompactAuthority::from_anchor(witness.anchor()).unwrap();
    let take_members: usize = kani::any();
    kani::assume(take_members <= 4);
    let reject_members: bool = kani::any();
    let member_reject_position: usize = kani::any();
    kani::assume(member_reject_position <= take_members);
    let mut members =
        CompactRootMemberCursor::begin(&authority, authority.backing, 1, witness.expect_root())
            .unwrap();
    for index in 0..take_members {
        if reject_members && member_reject_position == index {
            members.reject();
        }
        members.observe_member(witness.anchor().members[index]);
    }
    if reject_members && member_reject_position == take_members {
        members.reject();
    }
    let member_finished = members.finish_members().is_some();
    assert_eq!(member_finished, take_members == 4 && !reject_members);

    let expected_entries = entries(&witness);
    let header = CompactDirectoryHeader {
        stats: witness.audited_root().node.stats.clone(),
        entry_count: expected_entries.len() as u64,
        next_ordinal: 8,
    };
    let mut dentries = complete_members(&witness)
        .observe_guard(1, witness.audited_root().identity, &header)
        .unwrap();
    let take_entries: usize = kani::any();
    kani::assume(take_entries <= expected_entries.len());
    let reject_entries: bool = kani::any();
    let entry_reject_position: usize = kani::any();
    kani::assume(entry_reject_position <= take_entries);
    for (index, entry) in expected_entries.iter().enumerate().take(take_entries) {
        if reject_entries && entry_reject_position == index {
            dentries.reject();
        }
        dentries.observe_entry(BorrowedCompactRootEntry {
            parent: 1,
            ordinal: index as u64 * 2 + 1,
            name: &entry.name,
            inode: entry.inode,
        });
    }
    if reject_entries && entry_reject_position == take_entries {
        dentries.reject();
    }
    let extra_row: bool = kani::any();
    if extra_row {
        dentries.observe_entry(BorrowedCompactRootEntry {
            parent: 0,
            ordinal: u64::MAX,
            name: "extra",
            inode: 0,
        });
    }
    let actual = dentries.finish_dentries();
    let accepted = actual.is_some();
    assert_eq!(
        accepted,
        take_entries == expected_entries.len() && !reject_entries && !extra_row
    );
    if let Some(checked) = actual {
        assert_eq!(checked.generation(), generation);
        assert_eq!(checked.inode(), 1);
        assert!(identity_fields_equal(
            checked.identity(),
            witness.audited_root().identity
        ));
        let verified = checked.into_verified_root().unwrap();
        assert!(verified.structure.same_witness(&witness));
        assert!(!verified.structure.same_witness(&foreign));
        let created = NodeMetadata {
            stats: plain_stats(witness.anchor().next_inode, S_IFREG | 0o644, 1),
            data: NodeData::File(FileLayout {
                chunker: fixed_chunker(4096),
                extents: vec![],
            }),
        };
        let foreign_capture = CompactRootFileCreate::capture_verified(
            &foreign,
            &verified,
            witness.audited_root().identity,
            "new".into(),
            created.clone(),
            1,
            1,
        )
        .unwrap_err();
        assert_eq!(foreign_capture.code, ErrorCode::Einval);
        assert_eq!(
            foreign_capture.to_string(),
            "verified compact root differs from audited graph"
        );
        let own_capture = CompactRootFileCreate::capture_verified(
            &witness,
            &verified,
            witness.audited_root().identity,
            "new".into(),
            created,
            1,
            1,
        );
        if generation == u64::MAX {
            assert_eq!(own_capture.unwrap_err().code, ErrorCode::Eoverflow);
        } else {
            assert!(own_capture.is_ok());
        }
    }

    kani::cover!(member_finished);
    kani::cover!(take_members == 0 && !member_finished);
    kani::cover!(take_members == 3 && !reject_members && !member_finished);
    kani::cover!(
        take_members == 4 && reject_members && member_reject_position == 0 && !member_finished
    );
    kani::cover!(
        take_members == 4 && reject_members && member_reject_position == 4 && !member_finished
    );
    kani::cover!(accepted && empty);
    kani::cover!(accepted && !empty && hardlinks);
    kani::cover!(!empty && take_entries < expected_entries.len() && !accepted);
    kani::cover!(reject_entries && entry_reject_position == 0 && !accepted);
    kani::cover!(reject_entries && entry_reject_position == take_entries && !accepted);
    kani::cover!(extra_row && !accepted);
    kani::cover!(accepted && generation == u64::MAX);
    kani::cover!(accepted && generation == u64::MAX - 1);
}
