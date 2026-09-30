//! Pure core comparison controls. Provider packet parsing, transaction isolation,
//! terminal driver state, and rollback are outside this prepared scan window.

use super::super::{CompactGuard, CompactInodeRead, CompactSnapshot, NodeData, NodeMetadata};
use super::*;
use crate::storage::{DirectoryEntry, FileLayout};
use crate::{S_IFDIR, S_IFREG, Stats};
use std::collections::BTreeMap;
use std::sync::Arc;

type NamedMutation<T> = (&'static str, fn(&mut T));

#[derive(Clone)]
struct FreshRows {
    authority: CompactAuthority,
    members: Vec<u64>,
    backing: ConcurrentBackingId,
    selected_inode: u64,
    identity: PhysicalInodeIdentity,
    header: CompactDirectoryHeader,
    entries: Vec<DirectoryEntry>,
    ordinals: Vec<u64>,
}

fn fixture(children: usize) -> CompactSnapshot {
    let chunker = crate::chunking::ChunkerConfig {
        algorithm: "fixed-size".into(),
        version: 1,
        parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
    };
    let identity = PhysicalInodeIdentity {
        incarnation: 1,
        epoch: 3,
        revision: 4,
    };
    let node = |inode, mode, nlink, data| CompactGuard {
        identity,
        node: NodeMetadata {
            stats: Stats {
                dev: 9,
                ino: inode,
                mode,
                nlink,
                uid: 1000,
                gid: 1001,
                rdev: 7,
                size: 0,
                blksize: 4096,
                blocks: 0,
                atime_ms: -4,
                mtime_ms: 5,
                ctime_ms: -6,
                birthtime_ms: 7,
            },
            data,
        },
    };
    let mut guards = BTreeMap::new();
    let mut entries = Vec::new();
    for inode in (2..=children as u64 + 1).rev() {
        entries.push(DirectoryEntry {
            name: format!("child-{inode}-é🙂"),
            inode,
        });
        guards.insert(
            inode,
            node(
                inode,
                S_IFREG | 0o644,
                1,
                NodeData::File(FileLayout {
                    chunker: chunker.clone(),
                    extents: Vec::new(),
                }),
            ),
        );
    }
    guards.insert(
        1,
        node(1, S_IFDIR | 0o755, 2, NodeData::Directory { entries }),
    );
    CompactSnapshot {
        anchor: super::super::CompactAnchor {
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            generation: 3,
            root: 1,
            next_inode: children as u64 + 2,
            default_uid: 1000,
            default_gid: 1001,
            umask: 0o022,
            default_chunker: chunker,
            members: guards.keys().copied().collect(),
        },
        guards,
    }
}

fn prepare(snapshot: &CompactSnapshot) -> (FreshRows, ValidatedCompactStructure) {
    let (_, _, witness) = snapshot.clone().into_validated_namespace().unwrap();
    let root = &snapshot.guards[&snapshot.anchor.root];
    let NodeData::Directory { entries } = &root.node.data else {
        panic!("fixture root must be a directory")
    };
    (
        FreshRows {
            authority: CompactAuthority::from_anchor(&snapshot.anchor).unwrap(),
            members: snapshot.anchor.members.clone(),
            backing: snapshot.anchor.backing,
            selected_inode: snapshot.anchor.root,
            identity: root.identity,
            header: CompactDirectoryHeader {
                stats: root.node.stats.clone(),
                entry_count: entries.len() as u64,
                next_ordinal: entries.len() as u64 * 3 + 1,
            },
            entries: entries.clone(),
            ordinals: (0..entries.len() as u64).map(|index| index * 3).collect(),
        },
        witness,
    )
}

fn member_stage<'a>(
    rows: &FreshRows,
    witness: &'a ValidatedCompactStructure,
) -> CompactRootMemberCursor<'a> {
    CompactRootMemberCursor::begin(
        &rows.authority,
        rows.backing,
        rows.selected_inode,
        witness.expect_root(),
    )
    .expect("valid fresh authority must begin the root cursor")
}

fn guard_stage<'a>(
    rows: &FreshRows,
    witness: &'a ValidatedCompactStructure,
) -> CompactRootGuardCursor<'a> {
    let mut cursor = member_stage(rows, witness);
    for &member in &rows.members {
        cursor.observe_member(member);
    }
    cursor.finish_members().expect("complete valid membership")
}

fn dentry_stage<'a>(
    rows: &FreshRows,
    witness: &'a ValidatedCompactStructure,
) -> CompactRootDentryCursor<'a> {
    guard_stage(rows, witness)
        .observe_guard(rows.selected_inode, rows.identity, &rows.header)
        .expect("equal root guard/header")
}

fn observe_entries(cursor: &mut CompactRootDentryCursor<'_>, rows: &FreshRows) {
    assert_eq!(rows.entries.len(), rows.ordinals.len());
    for (entry, &ordinal) in rows.entries.iter().zip(&rows.ordinals) {
        cursor.observe_entry(BorrowedCompactRootEntry {
            parent: rows.selected_inode,
            ordinal,
            name: &entry.name,
            inode: entry.inode,
        });
    }
}

fn scan(rows: &FreshRows, expected: CompactInodeExpectation<'_>) -> Option<CheckedCompactInode> {
    let mut cursor = CompactRootMemberCursor::begin(
        &rows.authority,
        rows.backing,
        rows.selected_inode,
        expected,
    )?;
    for &member in &rows.members {
        cursor.observe_member(member);
    }
    let mut cursor =
        cursor
            .finish_members()?
            .observe_guard(rows.selected_inode, rows.identity, &rows.header)?;
    observe_entries(&mut cursor, rows);
    cursor.finish_dentries()
}

fn materialized_hit(rows: &FreshRows, expected: CompactInodeExpectation<'_>) -> bool {
    let Ok(anchor) = rows.authority.clone().into_anchor(rows.members.clone()) else {
        return false;
    };
    let guard = CompactGuard {
        identity: rows.identity,
        node: NodeMetadata {
            stats: rows.header.stats.clone(),
            data: NodeData::Directory {
                entries: rows.entries.clone(),
            },
        },
    };
    matches!(
        CompactInodeRead::from_materialized_guard(
            &anchor,
            rows.backing,
            rows.selected_inode,
            guard,
            expected,
        ),
        Ok(CompactInodeRead::Unchanged(_))
    )
}

fn assert_matches_materialized(label: &str, rows: &FreshRows, witness: &ValidatedCompactStructure) {
    let expected = witness.expect_root();
    let checked = scan(rows, expected);
    assert_eq!(
        checked.is_some(),
        materialized_hit(rows, expected),
        "{label}"
    );
    if let Some(checked) = checked {
        assert_eq!(checked.generation(), rows.authority.generation, "{label}");
        assert_eq!(checked.inode(), rows.selected_inode, "{label}");
        assert_eq!(checked.identity(), rows.identity, "{label}");
        let verified = checked.into_verified_root().expect("root receipt");
        assert!(
            Arc::ptr_eq(&verified.structure.anchor, &witness.anchor),
            "{label}"
        );
        assert!(
            Arc::ptr_eq(&verified.structure.root, &witness.root),
            "{label}"
        );
        assert!(
            Arc::ptr_eq(&verified.structure.root_children, &witness.root_children),
            "{label}"
        );
        assert!(
            Arc::ptr_eq(
                &verified.structure.root_single_link_file_bits,
                &witness.root_single_link_file_bits
            ),
            "{label}"
        );
    }
}

#[test]
fn complete_root_matches_materialized_empty_single_and_large_roots() {
    for count in [0, 1, 2, 63, 64, 65, 1000] {
        let (rows, witness) = prepare(&fixture(count));
        assert!(materialized_hit(&rows, witness.expect_root()));
        assert_matches_materialized("unchanged complete root", &rows, &witness);
        assert!(scan(&rows, witness.expect_root()).is_some());
    }
}

#[test]
fn all_fourteen_stats_fields_are_exact_comparison_inputs() {
    let (rows, witness) = prepare(&fixture(3));
    let mutations: &[NamedMutation<Stats>] = &[
        ("dev", |s| s.dev += 1),
        ("ino", |s| s.ino += 1),
        ("mode", |s| s.mode ^= 0o001),
        ("nlink", |s| s.nlink += 1),
        ("uid", |s| s.uid += 1),
        ("gid", |s| s.gid += 1),
        ("rdev", |s| s.rdev += 1),
        ("size", |s| s.size += 1),
        ("blksize", |s| s.blksize += 1),
        ("blocks", |s| s.blocks += 1),
        ("atime", |s| s.atime_ms += 1),
        ("mtime", |s| s.mtime_ms -= 1),
        ("ctime", |s| s.ctime_ms += 1),
        ("birthtime", |s| s.birthtime_ms -= 1),
    ];
    for &(label, mutate) in mutations {
        let mut changed = rows.clone();
        mutate(&mut changed.header.stats);
        assert!(
            !materialized_hit(&changed, witness.expect_root()),
            "{label}"
        );
        assert_matches_materialized(label, &changed, &witness);
    }
}

#[test]
fn generation_and_every_physical_identity_field_match_materialized_factory() {
    let (rows, witness) = prepare(&fixture(3));
    let mutations: &[NamedMutation<FreshRows>] = &[
        ("fresh generation", |r| r.authority.generation += 1),
        ("zero generation", |r| r.authority.generation = 0),
        ("incarnation", |r| r.identity.incarnation += 1),
        ("epoch", |r| r.identity.epoch -= 1),
        ("revision", |r| r.identity.revision += 1),
        ("zero incarnation", |r| r.identity.incarnation = 0),
        ("future epoch", |r| r.identity.epoch = 4),
        ("selected inode", |r| r.selected_inode = 2),
        ("zero selected inode", |r| r.selected_inode = 0),
        ("supplied backing", |r| {
            r.backing = ConcurrentBackingId::from_bytes([2; 16]).unwrap()
        }),
    ];
    for &(label, mutate) in mutations {
        let mut changed = rows.clone();
        mutate(&mut changed);
        assert_matches_materialized(label, &changed, &witness);
        assert!(scan(&changed, witness.expect_root()).is_none(), "{label}");
    }
    let unsealed = CompactInodeExpectation::selected(
        rows.authority.generation,
        rows.identity,
        &witness.root.node,
    );
    assert!(
        scan(&rows, unsealed).is_none(),
        "a selected directory has no complete graph seal"
    );
    let file = fixture(3);
    let file = &file.guards[&2];
    assert!(
        scan(
            &rows,
            CompactInodeExpectation::selected(3, file.identity, &file.node)
        )
        .is_none()
    );
}

#[test]
fn legal_fresh_authority_drift_and_ordinal_holes_remain_hits() {
    let (rows, witness) = prepare(&fixture(3));
    let mutations: &[NamedMutation<FreshRows>] = &[
        ("uid", |r| r.authority.default_uid += 1),
        ("gid", |r| r.authority.default_gid += 1),
        ("umask", |r| r.authority.umask = 0o077),
        ("default chunk size", |r| {
            *r.authority
                .default_chunker
                .parameters
                .get_mut("chunk_size")
                .unwrap() = 8192
        }),
        ("next inode", |r| r.authority.next_inode += 100),
        ("unused member", |r| {
            r.members.push(99);
            r.authority.members_count += 1;
            r.authority.next_inode = 100;
        }),
        ("current root", |r| {
            r.members.push(99);
            r.authority.members_count += 1;
            r.authority.next_inode = 100;
            r.authority.root = 99;
        }),
        ("fresh backing", |r| {
            r.authority.backing = ConcurrentBackingId::from_bytes([2; 16]).unwrap();
            r.backing = r.authority.backing;
        }),
        ("allocator and holes", |r| {
            r.header.next_ordinal = i64::MAX as u64;
            r.ordinals = vec![17, 123, i64::MAX as u64 - 1];
        }),
    ];
    for &(label, mutate) in mutations {
        let mut changed = rows.clone();
        mutate(&mut changed);
        assert!(materialized_hit(&changed, witness.expect_root()), "{label}");
        assert_matches_materialized(label, &changed, &witness);
        assert!(scan(&changed, witness.expect_root()).is_some(), "{label}");
    }
    // Changes may combine, and the retained receipt still owns the original
    // audited Arcs rather than constructing an anchor from these fresh IDs.
    let mut changed = rows;
    for &(_, mutate) in mutations {
        mutate(&mut changed);
    }
    // Two cases added the same unused member; provide one complete sorted set.
    changed.members.dedup();
    changed.authority.members_count = changed.members.len() as u64;
    assert_matches_materialized("combined legal drift", &changed, &witness);
    assert!(scan(&changed, witness.expect_root()).is_some());
}

#[test]
fn removal_of_an_audited_unused_member_remains_a_materialized_hit() {
    let mut snapshot = fixture(3);
    let orphan_inode = snapshot.anchor.next_inode;
    let mut orphan = snapshot.guards[&2].clone();
    orphan.node.stats.ino = orphan_inode;
    orphan.node.stats.nlink = 0;
    snapshot.guards.insert(orphan_inode, orphan);
    snapshot.anchor.members.push(orphan_inode);
    snapshot.anchor.next_inode += 1;
    // prepare performs the ordinary complete graph audit, including validation
    // of this retained unlinked-open orphan. No fabricated seal is supplied.
    let (mut rows, witness) = prepare(&snapshot);
    assert!(witness.anchor.members.contains(&orphan_inode));
    assert!(!witness.root_children.contains(&orphan_inode));
    rows.members.retain(|&inode| inode != orphan_inode);
    rows.authority.members_count -= 1;
    assert_eq!(rows.authority.members_count, rows.members.len() as u64);
    assert!(materialized_hit(&rows, witness.expect_root()));
    assert_matches_materialized("removed audited unused member", &rows, &witness);
    assert!(scan(&rows, witness.expect_root()).is_some());
}

#[test]
fn every_audited_child_must_be_present_in_complete_fresh_membership() {
    let (rows, witness) = prepare(&fixture(65));
    for &missing in &rows.members {
        let mut changed = rows.clone();
        changed.members.retain(|&inode| inode != missing);
        changed.authority.members_count -= 1;
        assert_matches_materialized("missing selected root or audited child", &changed, &witness);
        assert!(
            scan(&changed, witness.expect_root()).is_none(),
            "missing {missing}"
        );
    }
    let mut changed = rows.clone();
    changed.authority.root = 99;
    changed.authority.next_inode = 100;
    assert!(
        scan(&changed, witness.expect_root()).is_none(),
        "fresh current root must be a member"
    );
}

#[test]
fn members_are_nonzero_complete_strictly_ordered_and_bounded() {
    let (rows, witness) = prepare(&fixture(3));
    let mutations: &[NamedMutation<FreshRows>] = &[
        ("zero member", |r| r.members[0] = 0),
        ("duplicate member", |r| r.members[2] = r.members[1]),
        ("member order", |r| r.members.swap(1, 2)),
        ("underreported count", |r| r.authority.members_count -= 1),
        ("overreported count", |r| r.authority.members_count += 1),
        ("empty membership", |r| r.members.clear()),
        ("max inode", |r| *r.members.last_mut().unwrap() = u64::MAX),
        ("next inode not above max", |r| {
            r.authority.next_inode = *r.members.last().unwrap()
        }),
        ("root zero", |r| r.authority.root = 0),
        ("invalid defaults", |r| {
            *r.authority
                .default_chunker
                .parameters
                .get_mut("chunk_size")
                .unwrap() = 0
        }),
    ];
    for &(label, mutate) in mutations {
        let mut changed = rows.clone();
        mutate(&mut changed);
        assert!(scan(&changed, witness.expect_root()).is_none(), "{label}");
    }
}

#[test]
fn full_name_child_inode_and_logical_entry_order_match_the_owned_oracle() {
    let (rows, witness) = prepare(&fixture(65));
    for index in 0..rows.entries.len() {
        let mut changed = rows.clone();
        changed.entries[index].name.push('x');
        assert_matches_materialized("every full name", &changed, &witness);
        assert!(scan(&changed, witness.expect_root()).is_none());
        let mut changed = rows.clone();
        changed.entries[index].inode = if index == 0 { 2 } else { rows.entries[0].inode };
        assert_matches_materialized("every child inode", &changed, &witness);
        assert!(scan(&changed, witness.expect_root()).is_none());
    }
    let mut changed = rows.clone();
    changed.entries.reverse();
    assert_matches_materialized("entry order", &changed, &witness);
    assert!(scan(&changed, witness.expect_root()).is_none());
    for name in ["", ".", "..", "a/b", "a\0b", "child-66-é", "CHILD-66-é🙂"] {
        let mut changed = rows.clone();
        changed.entries[0].name = name.into();
        assert_matches_materialized("name rules and complete bytes", &changed, &witness);
        assert!(scan(&changed, witness.expect_root()).is_none());
    }
}

#[test]
fn hardlinks_and_entry_order_unrelated_to_inode_order_are_valid() {
    let mut snapshot = fixture(3);
    let NodeData::Directory { entries } = &mut snapshot.guards.get_mut(&1).unwrap().node.data
    else {
        panic!()
    };
    entries.push(DirectoryEntry {
        name: "second-link".into(),
        inode: 3,
    });
    snapshot.guards.get_mut(&3).unwrap().node.stats.nlink = 2;
    let (rows, witness) = prepare(&snapshot);
    assert_eq!(witness.root_children.as_ref(), &[2, 3, 4]);
    assert_eq!(
        rows.entries
            .iter()
            .map(|entry| entry.inode)
            .collect::<Vec<_>>(),
        [4, 3, 2, 3]
    );
    assert_matches_materialized("hardlink in arbitrary logical order", &rows, &witness);
    assert!(scan(&rows, witness.expect_root()).is_some());
}

#[test]
fn invalid_headers_and_physical_ordinal_sequences_cannot_finish() {
    let (rows, witness) = prepare(&fixture(3));
    let mutations: &[NamedMutation<FreshRows>] = &[
        ("header inode", |r| r.header.stats.ino = 2),
        ("header kind", |r| r.header.stats.mode = S_IFREG | 0o755),
        ("header fewer entries", |r| r.header.entry_count -= 1),
        ("header extra entries", |r| r.header.entry_count += 1),
        ("allocator smaller than count", |r| {
            r.header.next_ordinal = 2
        }),
        ("allocator exhausted", |r| r.header.next_ordinal = u64::MAX),
        ("unsigned allocator", |r| {
            r.header.next_ordinal = i64::MAX as u64 + 1
        }),
        ("duplicate ordinal", |r| r.ordinals[1] = r.ordinals[0]),
        ("reverse ordinals", |r| r.ordinals.reverse()),
        ("ordinal beyond allocator", |r| {
            r.ordinals[2] = r.header.next_ordinal
        }),
        ("zero child", |r| r.entries[0].inode = 0),
        ("unallocated child", |r| {
            r.entries[0].inode = r.authority.next_inode
        }),
    ];
    for &(label, mutate) in mutations {
        let mut changed = rows.clone();
        mutate(&mut changed);
        assert!(scan(&changed, witness.expect_root()).is_none(), "{label}");
    }
    let mut cursor = dentry_stage(&rows, &witness);
    cursor.observe_entry(BorrowedCompactRootEntry {
        parent: 99,
        ordinal: rows.ordinals[0],
        name: &rows.entries[0].name,
        inode: rows.entries[0].inode,
    });
    for (entry, &ordinal) in rows.entries.iter().zip(&rows.ordinals).skip(1) {
        cursor.observe_entry(BorrowedCompactRootEntry {
            parent: 1,
            ordinal,
            name: &entry.name,
            inode: entry.inode,
        });
    }
    assert!(
        cursor.finish_dentries().is_none(),
        "every parent must equal the selected root"
    );
}

#[test]
fn incomplete_or_explicitly_rejected_stages_never_issue_a_receipt() {
    let (rows, witness) = prepare(&fixture(3));
    for take in 0..rows.members.len() {
        let mut cursor = member_stage(&rows, &witness);
        for &member in rows.members.iter().take(take) {
            cursor.observe_member(member);
        }
        assert!(
            cursor.finish_members().is_none(),
            "incomplete members {take}"
        );
    }
    let mut cursor = member_stage(&rows, &witness);
    cursor.reject();
    for &member in &rows.members {
        cursor.observe_member(member);
    }
    assert!(
        cursor.finish_members().is_none(),
        "member parser rejection stays sticky"
    );
    for take in 0..rows.entries.len() {
        let mut cursor = dentry_stage(&rows, &witness);
        for (entry, &ordinal) in rows.entries.iter().zip(&rows.ordinals).take(take) {
            cursor.observe_entry(BorrowedCompactRootEntry {
                parent: 1,
                ordinal,
                name: &entry.name,
                inode: entry.inode,
            });
        }
        assert!(
            cursor.finish_dentries().is_none(),
            "incomplete dentries {take}"
        );
    }
    let mut cursor = dentry_stage(&rows, &witness);
    cursor.reject();
    observe_entries(&mut cursor, &rows);
    assert!(
        cursor.finish_dentries().is_none(),
        "dentry parser rejection stays sticky"
    );
    let mut cursor = dentry_stage(&rows, &witness);
    observe_entries(&mut cursor, &rows);
    cursor.observe_entry(BorrowedCompactRootEntry {
        parent: 1,
        ordinal: 8,
        name: "extra",
        inode: 2,
    });
    assert!(
        cursor.finish_dentries().is_none(),
        "extra physical row cannot hide behind count"
    );
    let mut cursor = member_stage(&rows, &witness);
    for &member in &rows.members {
        cursor.observe_member(member);
    }
    cursor.observe_member(99);
    assert!(
        cursor.finish_members().is_none(),
        "extra member cannot hide behind count"
    );
}

#[test]
fn receipt_retains_original_audit_and_rejects_grafting_onto_equal_other_audit() {
    let snapshot = fixture(3);
    let (rows, witness) = prepare(&snapshot);
    let (_, _, other) = snapshot.clone().into_validated_namespace().unwrap();
    let receipt = scan(&rows, witness.expect_root())
        .expect("complete comparison")
        .into_verified_root()
        .expect("root receipt");
    assert!(Arc::ptr_eq(&receipt.structure.anchor, &witness.anchor));
    assert!(Arc::ptr_eq(&receipt.structure.root, &witness.root));
    assert!(!Arc::ptr_eq(&receipt.structure.anchor, &other.anchor));
    let mut created = snapshot.guards[&2].node.clone();
    created.stats.ino = snapshot.anchor.next_inode;
    let mtime = rows.header.stats.mtime_ms.checked_add(1).unwrap();
    let ctime = rows.header.stats.ctime_ms.checked_add(1).unwrap();
    let foreign = super::super::CompactRootFileCreate::capture_verified(
        &other,
        &receipt,
        rows.identity,
        "new".into(),
        created.clone(),
        mtime,
        ctime,
    )
    .expect_err("an equal but independent audit cannot accept this receipt");
    assert_eq!(
        foreign.to_string(),
        "verified compact root differs from audited graph"
    );
    super::super::CompactRootFileCreate::capture_verified(
        &witness,
        &receipt,
        rows.identity,
        "new".into(),
        created,
        mtime,
        ctime,
    )
    .expect("the original audit must accept its receipt and a valid create proposal");
}

#[test]
fn prepared_complete_borrowed_root_scan_allocates_zero_with_allocating_owned_control() {
    for count in [0, 1, 65, 1000] {
        let (rows, witness) = prepare(&fixture(count));
        let (receipt, calls, bytes) = super::super::streamed_tests::measured(|| {
            scan(std::hint::black_box(&rows), witness.expect_root())
        });
        assert!(
            receipt.is_some(),
            "allocation test must observe a successful full scan"
        );
        assert_eq!(
            (calls, bytes),
            (0, 0),
            "prepared core scan with {count} children"
        );
        let (owned_hit, calls, bytes) = super::super::streamed_tests::measured(|| {
            materialized_hit(std::hint::black_box(&rows), witness.expect_root())
        });
        assert!(owned_hit, "owned control must verify the same logical root");
        assert!(
            calls > 0 && bytes > 0,
            "same meter must detect owned buffers"
        );
    }
}
