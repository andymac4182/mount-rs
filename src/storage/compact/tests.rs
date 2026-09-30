use super::*;

fn fixture() -> CompactSnapshot {
    let chunker = ChunkerConfig {
        algorithm: "fixed-size".into(),
        version: 1,
        parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
    };
    let node = |ino, mode, nlink, data| NodeMetadata {
        stats: Stats {
            dev: 0,
            ino,
            mode,
            nlink,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            size: 0,
            blksize: 4096,
            blocks: 0,
            atime_ms: 0,
            mtime_ms: 0,
            ctime_ms: 0,
            birthtime_ms: 0,
        },
        data,
    };
    let identity = PhysicalInodeIdentity {
        incarnation: 1,
        epoch: 3,
        revision: 4,
    };
    CompactSnapshot {
        anchor: CompactAnchor {
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            generation: 3,
            root: 1,
            next_inode: 4,
            default_uid: 1000,
            default_gid: 1000,
            umask: 0o022,
            default_chunker: chunker.clone(),
            members: vec![1, 2, 3],
        },
        guards: BTreeMap::from([
            (
                1,
                CompactGuard {
                    identity,
                    node: node(
                        1,
                        S_IFDIR | 0o755,
                        2,
                        NodeData::Directory {
                            entries: vec![
                                DirectoryEntry {
                                    name: "a".into(),
                                    inode: 2,
                                },
                                DirectoryEntry {
                                    name: "b".into(),
                                    inode: 3,
                                },
                            ],
                        },
                    ),
                },
            ),
            (
                2,
                CompactGuard {
                    identity,
                    node: node(
                        2,
                        S_IFREG | 0o644,
                        1,
                        NodeData::File(FileLayout {
                            chunker: chunker.clone(),
                            extents: vec![],
                        }),
                    ),
                },
            ),
            (
                3,
                CompactGuard {
                    identity,
                    node: node(
                        3,
                        S_IFREG | 0o644,
                        1,
                        NodeData::File(FileLayout {
                            chunker: chunker.clone(),
                            extents: vec![],
                        }),
                    ),
                },
            ),
        ]),
    }
}

fn create_candidate(base: &CompactSnapshot, name: &str) -> Namespace {
    let mut candidate = base.namespace().unwrap();
    let id = candidate.next_inode;
    candidate.next_inode += 1;
    let mut file = candidate.nodes[&2].clone();
    file.stats.ino = id;
    candidate.nodes.insert(id, file);
    let NodeData::Directory { entries } = &mut candidate.nodes.get_mut(&1).unwrap().data else {
        panic!()
    };
    entries.push(DirectoryEntry {
        name: name.into(),
        inode: id,
    });
    candidate
}

fn retained_create_inputs(
    base: &CompactSnapshot,
) -> (Namespace, ValidatedCompactStructure, LoadedCompactInode) {
    let (cached, _, structure) = base.clone().into_validated_namespace().unwrap();
    let parent = LoadedCompactInode::from_guard(
        &base.anchor,
        base.anchor.root,
        base.guards[&base.anchor.root].clone(),
    )
    .unwrap();
    (cached, structure, parent)
}

fn update(base: &CompactSnapshot, inode: u64, size: u64) -> CompactSnapshot {
    let mut node = base.guards[&inode].node.clone();
    node.stats.size = size;
    node.stats.mtime_ms += 1;
    let result = base.selected_update(
        base.anchor.backing,
        base.anchor.generation,
        inode,
        base.guards[&inode].identity,
        node,
    );
    assert!(
        result.is_ok(),
        "valid selected update must succeed: {result:?}"
    );
    result.unwrap()
}

fn code<T: std::fmt::Debug>(result: Result<T>, expected: ErrorCode) {
    assert_eq!(result.unwrap_err().code, expected);
}

#[test]
fn old_physical_epoch_projects_to_current_zero_without_rewriting_guard() {
    let guard = PhysicalInodeIdentity {
        incarnation: 1,
        epoch: 2,
        revision: 99,
    };
    assert_eq!(
        guard.logical_version(3).unwrap(),
        InodeVersion {
            structural_generation: 3,
            inode_revision: 0
        }
    );
    assert_eq!(guard.logical_version(2).unwrap().inode_revision, 99);
}

#[test]
fn future_epochs_and_invalid_incarnations_fail_closed() {
    for identity in [
        PhysicalInodeIdentity {
            incarnation: 1,
            epoch: 4,
            revision: 0,
        },
        PhysicalInodeIdentity {
            incarnation: 0,
            epoch: 3,
            revision: 0,
        },
        PhysicalInodeIdentity {
            incarnation: 3,
            epoch: 2,
            revision: 0,
        },
        PhysicalInodeIdentity {
            incarnation: 1,
            epoch: 0,
            revision: 0,
        },
    ] {
        code(identity.logical_version(3), ErrorCode::Einval);
    }
    code(
        fixture().guards[&2].identity.logical_version(0),
        ErrorCode::Einval,
    );
}

#[test]
fn exact_membership_rejects_equal_count_missing_and_phantom() {
    let mut base = fixture();
    base.anchor.members = vec![1, 2, 4];
    code(base.namespace(), ErrorCode::Einval);
    base.anchor.members = vec![1, 3, 2];
    code(base.namespace(), ErrorCode::Einval);
    base.anchor.members = vec![1, 2, 2, 3];
    code(base.namespace(), ErrorCode::Einval);
}

#[test]
fn targeted_create_preserves_concurrent_untouched_selected_body_and_physical_identity() {
    let base = fixture();
    let captured = CompactStructuralDelta::capture(
        &base,
        &create_candidate(&base, "new"),
        StructuralScope::FileCreate,
    );
    assert!(
        captured.is_ok(),
        "valid create must be captured: {captured:?}"
    );
    let delta = captured.unwrap();
    let current = update(&base, 2, 917);
    let next = delta.evaluate(&current).unwrap();
    assert_eq!(next.guards[&2], current.guards[&2]);
    assert_eq!(next.guards[&3], current.guards[&3]);
    assert_eq!(next.anchor.members, vec![1, 2, 3, 4]);
    assert_eq!(next.anchor.generation, 4);
    assert_eq!(
        next.guards[&2]
            .identity
            .logical_version(4)
            .unwrap()
            .inode_revision,
        0
    );
    let after = update(&next, 2, 918);
    assert_eq!(
        after.guards[&2].identity,
        PhysicalInodeIdentity {
            incarnation: 1,
            epoch: 4,
            revision: 1
        }
    );
    assert_eq!(after.guards[&2].node.stats.size, 918);
}

#[test]
fn full_delta_conflicts_on_untouched_selected_write_instead_of_losing_it() {
    let base = fixture();
    let delta = CompactStructuralDelta::capture(
        &base,
        &create_candidate(&base, "new"),
        StructuralScope::Full,
    )
    .unwrap();
    let current = update(&base, 2, 891);
    code(delta.evaluate(&current), ErrorCode::Eagain);
    assert_eq!(current.guards[&2].node.stats.size, 891);
    let fresh = CompactStructuralDelta::capture(
        &current,
        &create_candidate(&current, "new"),
        StructuralScope::Full,
    )
    .unwrap()
    .evaluate(&current)
    .unwrap();
    assert_eq!(fresh.guards[&2], current.guards[&2]);
}

#[test]
fn affected_parent_and_same_allocation_or_name_conflict() {
    let base = fixture();
    let delta = CompactStructuralDelta::capture(
        &base,
        &create_candidate(&base, "new"),
        StructuralScope::FileCreate,
    )
    .unwrap();
    let mut parent_changed = base.clone();
    parent_changed.guards.get_mut(&1).unwrap().identity.revision += 1;
    code(delta.evaluate(&parent_changed), ErrorCode::Eagain);
    for name in ["new", "other"] {
        let winner = CompactStructuralDelta::capture(
            &base,
            &create_candidate(&base, name),
            StructuralScope::FileCreate,
        )
        .unwrap()
        .evaluate(&base)
        .unwrap();
        code(delta.evaluate(&winner), ErrorCode::Eagain);
    }
    code(
        CompactStructuralDelta::capture(
            &base,
            &create_candidate(&base, "a"),
            StructuralScope::FileCreate,
        ),
        ErrorCode::Einval,
    );
}

#[test]
fn selected_checks_authority_generation_exact_physical_identity_and_content_only() {
    let base = fixture();
    let expected = base.guards[&2].identity;
    let node = base.guards[&2].node.clone();
    code(
        base.selected_update(
            ConcurrentBackingId::from_bytes([2; 16]).unwrap(),
            3,
            2,
            expected,
            node.clone(),
        ),
        ErrorCode::Estale,
    );
    code(
        base.selected_update(base.anchor.backing, 2, 2, expected, node.clone()),
        ErrorCode::Eagain,
    );
    for wrong in [
        PhysicalInodeIdentity {
            incarnation: 2,
            ..expected
        },
        PhysicalInodeIdentity {
            revision: 3,
            ..expected
        },
        PhysicalInodeIdentity {
            epoch: 2,
            ..expected
        },
    ] {
        code(
            base.selected_update(base.anchor.backing, 3, 2, wrong, node.clone()),
            ErrorCode::Eagain,
        );
    }
    let mut structural = node;
    structural.stats.nlink = 0;
    code(
        base.selected_update(base.anchor.backing, 3, 2, expected, structural),
        ErrorCode::Einval,
    );
    let selected = update(&base, 2, 99);
    assert_eq!(selected.anchor, base.anchor);
    assert_eq!(selected.guards[&3], base.guards[&3]);
}

#[test]
fn overflows_do_not_wrap_or_mutate() {
    let mut base = fixture();
    base.guards.get_mut(&2).unwrap().identity.revision = u64::MAX;
    code(
        base.selected_update(
            base.anchor.backing,
            3,
            2,
            base.guards[&2].identity,
            base.guards[&2].node.clone(),
        ),
        ErrorCode::Eoverflow,
    );
    base.anchor.generation = u64::MAX;
    code(
        CompactStructuralDelta::capture(&base, &base.namespace().unwrap(), StructuralScope::Full),
        ErrorCode::Eoverflow,
    );
    base = fixture();
    base.anchor.next_inode = u64::MAX;
    let mut candidate = base.namespace().unwrap();
    let mut node = candidate.nodes[&2].clone();
    node.stats.ino = u64::MAX;
    candidate.nodes.insert(u64::MAX, node);
    code(
        CompactStructuralDelta::capture(&base, &candidate, StructuralScope::FileCreate),
        ErrorCode::Eoverflow,
    );
}

#[test]
fn targeted_scope_rejects_defaults_existing_content_and_nonallocation_changes() {
    let base = fixture();
    for change in 0..3 {
        let mut candidate = create_candidate(&base, "new");
        match change {
            0 => candidate.default_uid += 1,
            1 => candidate.nodes.get_mut(&2).unwrap().stats.size = 10,
            _ => candidate.next_inode += 1,
        }
        code(
            CompactStructuralDelta::capture(&base, &candidate, StructuralScope::FileCreate),
            ErrorCode::Einval,
        );
    }
}

#[test]
fn retained_file_create_capture_matches_snapshot_intent_for_any_insertion_position() {
    let base = fixture();
    let (cached, structure, parent) = retained_create_inputs(&base);
    for position in 0..3 {
        let mut candidate = create_candidate(&base, "new");
        let NodeData::Directory { entries } = &mut candidate.nodes.get_mut(&1).unwrap().data else {
            panic!()
        };
        let inserted = entries.pop().unwrap();
        entries.insert(position, inserted);
        let captured = CompactStructuralDelta::capture_file_create(
            &structure,
            &cached,
            &parent,
            parent.guard.identity,
            &candidate,
        )
        .unwrap();
        let reference =
            CompactStructuralDelta::capture(&base, &candidate, StructuralScope::FileCreate)
                .unwrap();
        assert_eq!(captured.scope(), StructuralScope::FileCreate);
        assert_eq!(captured.expected(), reference.expected());
        assert_eq!(captured.parent_entries(), reference.parent_entries());
        let guards = BTreeMap::from([(1, parent.guard.clone())]);
        assert_eq!(
            captured.validate_current(&base.anchor, &guards).unwrap(),
            reference.validate_current(&base.anchor, &guards).unwrap()
        );
    }
}

#[test]
fn retained_file_create_capture_preserves_untouched_selected_body_and_identity() {
    let base = fixture();
    let (_, structure, parent) = retained_create_inputs(&base);
    let current = update(&base, 2, 917);
    let cached = current.namespace().unwrap();
    let candidate = create_candidate(&current, "new");
    let captured = CompactStructuralDelta::capture_file_create(
        &structure,
        &cached,
        &parent,
        parent.guard.identity,
        &candidate,
    )
    .unwrap();
    assert_eq!(
        captured.expected().keys().copied().collect::<Vec<_>>(),
        vec![1]
    );
    let latest = update(&current, 3, 918);
    let next = captured.evaluate(&latest).unwrap();
    assert_eq!(next.guards[&2], latest.guards[&2]);
    assert_eq!(next.guards[&3], latest.guards[&3]);
    assert_eq!(next.anchor.members, vec![1, 2, 3, 4]);
}

#[test]
fn retained_file_create_capture_rejects_candidate_changes_outside_parent_and_new_file() {
    let base = fixture();
    let (cached, structure, parent) = retained_create_inputs(&base);
    for change in 0..7 {
        let mut candidate = create_candidate(&base, "new");
        match change {
            0 => candidate.default_uid += 1,
            1 => candidate.nodes.get_mut(&2).unwrap().stats.size = 10,
            2 => candidate.nodes.get_mut(&1).unwrap().stats.uid += 1,
            3 => {
                let NodeData::Directory { entries } =
                    &mut candidate.nodes.get_mut(&1).unwrap().data
                else {
                    panic!()
                };
                entries.swap(0, 1);
            }
            4 => candidate.next_inode += 1,
            5 => {
                let file = candidate.nodes.get_mut(&4).unwrap();
                file.stats.mode = S_IFLNK | 0o777;
                file.data = NodeData::Symlink { target: "a".into() };
            }
            _ => candidate.nodes.get_mut(&1).unwrap().stats.atime_ms += 1,
        }
        candidate.validate().unwrap();
        code(
            CompactStructuralDelta::capture_file_create(
                &structure,
                &cached,
                &parent,
                parent.guard.identity,
                &candidate,
            ),
            ErrorCode::Einval,
        );
    }
}

#[test]
fn retained_file_create_capture_requires_fresh_parent_generation_and_physical_identity() {
    let base = fixture();
    let (cached, structure, parent) = retained_create_inputs(&base);
    let candidate = create_candidate(&base, "new");
    for change in 0..3 {
        let mut loaded = parent.clone();
        let mut expected = parent.guard.identity;
        match change {
            0 => loaded.generation += 1,
            1 => loaded.guard.identity.revision += 1,
            _ => expected.revision += 1,
        }
        code(
            CompactStructuralDelta::capture_file_create(
                &structure, &cached, &loaded, expected, &candidate,
            ),
            ErrorCode::Eagain,
        );
    }
}

#[test]
fn retained_file_create_capture_rejects_changed_parent_body_with_same_identity() {
    let base = fixture();
    let (cached, structure, parent) = retained_create_inputs(&base);
    let candidate = create_candidate(&base, "new");
    for change in 0..2 {
        let mut current = base.clone();
        let changed = current.guards.get_mut(&1).unwrap();
        if change == 0 {
            changed.node.stats.mode ^= 0o001;
        } else {
            let NodeData::Directory { entries } = &mut changed.node.data else {
                panic!()
            };
            entries[0].name = "renamed".into();
        }
        current.namespace().unwrap();
        let loaded =
            LoadedCompactInode::from_guard(&current.anchor, 1, current.guards[&1].clone()).unwrap();
        assert_eq!(loaded.guard.identity, parent.guard.identity);
        code(
            CompactStructuralDelta::capture_file_create(
                &structure,
                &cached,
                &loaded,
                parent.guard.identity,
                &candidate,
            ),
            ErrorCode::Einval,
        );
    }
}

#[test]
fn retained_file_create_capture_rejects_cached_anchor_drift() {
    let base = fixture();
    let (mut cached, structure, parent) = retained_create_inputs(&base);
    cached.default_uid += 1;
    let mut candidate = create_candidate(&base, "new");
    candidate.default_uid = cached.default_uid;
    candidate.validate().unwrap();
    code(
        CompactStructuralDelta::capture_file_create(
            &structure,
            &cached,
            &parent,
            parent.guard.identity,
            &candidate,
        ),
        ErrorCode::Einval,
    );
}

#[test]
fn retained_file_create_capture_validates_complete_candidate_graph() {
    let base = fixture();
    let (mut cached, structure, parent) = retained_create_inputs(&base);
    cached.nodes.get_mut(&2).unwrap().stats.nlink = 2;
    let mut candidate = create_candidate(&base, "new");
    candidate.nodes.get_mut(&2).unwrap().stats.nlink = 2;
    assert_eq!(candidate.nodes[&2], cached.nodes[&2]);
    code(
        CompactStructuralDelta::capture_file_create(
            &structure,
            &cached,
            &parent,
            parent.guard.identity,
            &candidate,
        ),
        ErrorCode::Einval,
    );
}

#[test]
fn retained_file_create_capture_checks_generation_and_allocation_overflow() {
    let mut base = fixture();
    base.anchor.generation = u64::MAX;
    let (cached, structure, parent) = retained_create_inputs(&base);
    code(
        CompactStructuralDelta::capture_file_create(
            &structure,
            &cached,
            &parent,
            parent.guard.identity,
            &create_candidate(&base, "new"),
        ),
        ErrorCode::Eoverflow,
    );

    base = fixture();
    base.anchor.next_inode = u64::MAX;
    let (cached, structure, parent) = retained_create_inputs(&base);
    let mut candidate = cached.clone();
    let mut file = candidate.nodes[&2].clone();
    file.stats.ino = u64::MAX;
    candidate.nodes.insert(u64::MAX, file);
    let NodeData::Directory { entries } = &mut candidate.nodes.get_mut(&1).unwrap().data else {
        panic!()
    };
    entries.push(DirectoryEntry {
        name: "new".into(),
        inode: u64::MAX,
    });
    code(
        CompactStructuralDelta::capture_file_create(
            &structure,
            &cached,
            &parent,
            parent.guard.identity,
            &candidate,
        ),
        ErrorCode::Eoverflow,
    );
}

fn root_create_proposal(base: &CompactSnapshot, name: &str) -> CompactRootFileCreate {
    let (_, structure, parent) = retained_create_inputs(base);
    let mut created = base.guards[&2].node.clone();
    created.stats.ino = base.anchor.next_inode;
    CompactRootFileCreate::capture(
        &structure,
        &parent,
        parent.guard.identity,
        name.into(),
        created,
        parent.guard.node.stats.mtime_ms + 1,
        parent.guard.node.stats.ctime_ms + 1,
    )
    .unwrap()
}

#[test]
fn audited_root_proposal_matches_full_reference_and_extends_graph_inductively() {
    let mut current = fixture();
    for name in ["new", "next"] {
        let proposal = root_create_proposal(&current, name);
        let mut candidate = create_candidate(&current, name);
        candidate.nodes.get_mut(&1).unwrap().stats.mtime_ms += 1;
        candidate.nodes.get_mut(&1).unwrap().stats.ctime_ms += 1;
        let reference =
            CompactStructuralDelta::capture(&current, &candidate, StructuralScope::FileCreate)
                .unwrap();
        let read_set = BTreeMap::from([(1, current.guards[&1].clone())]);
        let receipt = proposal
            .delta()
            .validate_current(&current.anchor, &read_set)
            .unwrap();
        assert_eq!(
            receipt,
            reference
                .validate_current(&current.anchor, &read_set)
                .unwrap()
        );
        let next = proposal.validate_publication(&receipt).unwrap();
        assert_eq!(next.anchor(), &receipt.anchor);
        assert_eq!(next.root.as_ref(), &receipt.upserts[&1]);
        current = proposal.delta().evaluate(&current).unwrap();
        current.namespace().unwrap();
        assert_eq!(next.anchor(), &current.anchor);
        assert_eq!(next.root.as_ref(), &current.guards[&1]);
        // The next create consumes the proof that the prior acknowledged
        // constructor extended, rather than attaching a new arbitrary tree.
        let parent =
            LoadedCompactInode::from_guard(&current.anchor, 1, current.guards[&1].clone()).unwrap();
        let mut created = current.guards[&2].node.clone();
        created.stats.ino = current.anchor.next_inode;
        let next_proposal = CompactRootFileCreate::capture(
            &next,
            &parent,
            parent.guard.identity,
            "inductive".into(),
            created,
            parent.guard.node.stats.mtime_ms + 1,
            parent.guard.node.stats.ctime_ms + 1,
        )
        .unwrap();
        assert_eq!(next_proposal.delta().expected().len(), 1);
        next_proposal
            .delta()
            .evaluate(&current)
            .unwrap()
            .namespace()
            .unwrap();
    }
}

#[test]
fn audited_root_capture_matches_loaded_and_verified_proposals_exactly() {
    let base = fixture();
    let (_, structure, parent) = retained_create_inputs(&base);
    assert_eq!(structure.audited_root(), &base.guards[&1]);
    let verified = structure.verify_loaded_root(&parent).unwrap();
    let mut created = base.guards[&2].node.clone();
    created.stats.ino = 4;
    created.stats.mode = S_IFREG | 0o640;
    let audited = CompactRootFileCreate::capture_audited(
        &structure,
        parent.guard.identity,
        "new".into(),
        created.clone(),
        11,
        17,
    )
    .unwrap();
    let loaded = CompactRootFileCreate::capture(
        &structure,
        &parent,
        parent.guard.identity,
        "new".into(),
        created.clone(),
        11,
        17,
    )
    .unwrap();
    let checked = CompactRootFileCreate::capture_verified(
        &structure,
        &verified,
        parent.guard.identity,
        "new".into(),
        created.clone(),
        11,
        17,
    )
    .unwrap();
    for reference in [&loaded, &checked] {
        let actual = audited.delta();
        let expected = reference.delta();
        assert_eq!(actual.base, expected.base);
        assert_eq!(actual.next, expected.next);
        assert_eq!(actual.expected, expected.expected);
        assert_eq!(actual.changed, expected.changed);
        assert_eq!(actual.created, expected.created);
        assert_eq!(actual.removed, expected.removed);
        assert_eq!(actual.entries, expected.entries);
        assert_eq!(actual.parent_body, expected.parent_body);
        assert_eq!(actual.captured_bodies, expected.captured_bodies);
        assert_eq!(actual.next_root_children, expected.next_root_children);
        assert_eq!(actual.next_root_file_bits, expected.next_root_file_bits);
        assert_eq!(actual.scope, expected.scope);
    }

    let expected_anchor = CompactAnchor {
        generation: 4,
        next_inode: 5,
        members: vec![1, 2, 3, 4],
        ..base.anchor.clone()
    };
    let mut root = base.guards[&1].node.clone();
    root.stats.mtime_ms = 11;
    root.stats.ctime_ms = 17;
    root.data = NodeData::Directory {
        entries: vec![
            DirectoryEntry {
                name: "a".into(),
                inode: 2,
            },
            DirectoryEntry {
                name: "b".into(),
                inode: 3,
            },
            DirectoryEntry {
                name: "new".into(),
                inode: 4,
            },
        ],
    };
    let expected_root = CompactGuard {
        identity: PhysicalInodeIdentity {
            incarnation: 1,
            epoch: 4,
            revision: 0,
        },
        node: root,
    };
    let expected_file = CompactGuard {
        identity: PhysicalInodeIdentity {
            incarnation: 4,
            epoch: 4,
            revision: 0,
        },
        node: created,
    };
    let expected_receipt = CompactPublication {
        anchor: expected_anchor.clone(),
        upserts: BTreeMap::from([(1, expected_root.clone()), (4, expected_file.clone())]),
        removed: BTreeSet::new(),
    };
    let expected_snapshot = CompactSnapshot {
        anchor: expected_anchor,
        guards: BTreeMap::from([
            (1, expected_root.clone()),
            (2, base.guards[&2].clone()),
            (3, base.guards[&3].clone()),
            (4, expected_file),
        ]),
    };
    expected_snapshot.namespace().unwrap();
    let fresh_parent = BTreeMap::from([(1, parent.guard.clone())]);
    for proposal in [&audited, &loaded, &checked] {
        let receipt = proposal
            .delta()
            .validate_current(&base.anchor, &fresh_parent)
            .unwrap();
        assert_eq!(receipt, expected_receipt);
        assert_eq!(proposal.delta().evaluate(&base).unwrap(), expected_snapshot);
        let next = proposal.validate_publication(&receipt).unwrap();
        assert_eq!(next.anchor(), &expected_snapshot.anchor);
        assert_eq!(next.audited_root(), &expected_root);
    }
    assert_eq!(structure.audited_root(), &base.guards[&1]);

    // A parent-only create does not certify unrelated selected file bodies as
    // fresh. A legitimate concurrent selected write must still be preserved.
    let selected = update(&base, 2, 917);
    let committed = audited.delta().evaluate(&selected).unwrap();
    assert_eq!(committed.guards[&2], selected.guards[&2]);
    assert_eq!(committed.guards[&3], selected.guards[&3]);
    assert_eq!(committed.guards[&1], expected_root);
}

#[test]
fn audited_root_capture_requires_fresh_generation_and_parent_identity_at_publication() {
    let base = fixture();
    let (_, structure, parent) = retained_create_inputs(&base);
    let mut created = base.guards[&2].node.clone();
    created.stats.ino = base.anchor.next_inode;
    let proposal = CompactRootFileCreate::capture_audited(
        &structure,
        parent.guard.identity,
        "new".into(),
        created,
        1,
        1,
    )
    .unwrap();
    let fresh_parent = BTreeMap::from([(1, parent.guard.clone())]);
    let mut newer = base.anchor.clone();
    newer.generation = 4;
    newer.validate().unwrap();
    code(
        proposal.delta().validate_current(&newer, &fresh_parent),
        ErrorCode::Eagain,
    );

    let mut changed_parent = parent.guard.clone();
    changed_parent.identity.revision = 5;
    changed_parent.validate(1, &base.anchor).unwrap();
    code(
        proposal
            .delta()
            .validate_current(&base.anchor, &BTreeMap::from([(1, changed_parent)])),
        ErrorCode::Eagain,
    );
    // An audited proposal remains a usable expectation for the unchanged
    // state; neither refusal grants freshness or mutates the retained witness.
    proposal
        .delta()
        .validate_current(&base.anchor, &fresh_parent)
        .unwrap();
    assert_eq!(structure.audited_root(), &base.guards[&1]);
}

#[test]
fn audited_root_capture_refuses_same_identity_sibling_dentry_body_drift() {
    let base = fixture();
    let (_, structure, parent) = retained_create_inputs(&base);
    let mut created = base.guards[&2].node.clone();
    created.stats.ino = base.anchor.next_inode;
    let proposal = CompactRootFileCreate::capture_audited(
        &structure,
        parent.guard.identity,
        "new".into(),
        created,
        1,
        1,
    )
    .unwrap();
    for reorder in [false, true] {
        let mut current = base.clone();
        let NodeData::Directory { entries } = &mut current.guards.get_mut(&1).unwrap().node.data
        else {
            panic!()
        };
        if reorder {
            entries.swap(0, 1);
        } else {
            entries[0].name = "renamed-sibling".into();
        }
        // Both states are otherwise valid graphs and keep the proposed name
        // absent. The complete parent body fence must reject sibling drift.
        current.namespace().unwrap();
        let fresh_parent = current.guards.remove(&1).unwrap();
        assert_eq!(fresh_parent.identity, parent.guard.identity);
        code(
            proposal
                .delta()
                .validate_current(&current.anchor, &BTreeMap::from([(1, fresh_parent)])),
            ErrorCode::Einval,
        );
    }
    assert_eq!(structure.audited_root(), &base.guards[&1]);
}

#[test]
fn audited_root_capture_rejects_wrong_expected_parent_identity() {
    let base = fixture();
    let (_, structure, parent) = retained_create_inputs(&base);
    for component in 0..3 {
        let mut expected = parent.guard.identity;
        match component {
            0 => expected.incarnation += 1,
            1 => expected.epoch += 1,
            _ => expected.revision += 1,
        }
        let mut created = base.guards[&2].node.clone();
        created.stats.ino = base.anchor.next_inode;
        code(
            CompactRootFileCreate::capture_audited(
                &structure,
                expected,
                "new".into(),
                created,
                1,
                1,
            ),
            ErrorCode::Eagain,
        );
    }
    assert_eq!(structure.audited_root(), &base.guards[&1]);
}

#[test]
fn audited_root_proposal_rejects_equal_identity_body_grafting_and_wrong_witnesses() {
    let base = fixture();
    let (_, structure, parent) = retained_create_inputs(&base);
    for change in 0..5 {
        let mut loaded = parent.clone();
        let mut expected = parent.guard.identity;
        match change {
            0 => loaded.guard.node.stats.mode ^= 1,
            1 => {
                let NodeData::Directory { entries } = &mut loaded.guard.node.data else {
                    panic!()
                };
                entries.swap(0, 1);
            }
            2 => loaded.generation += 1,
            3 => loaded.guard.identity.revision += 1,
            _ => expected.revision += 1,
        }
        let mut created = base.guards[&2].node.clone();
        created.stats.ino = base.anchor.next_inode;
        code(
            CompactRootFileCreate::capture(
                &structure,
                &loaded,
                expected,
                "new".into(),
                created,
                1,
                1,
            ),
            if change < 2 {
                ErrorCode::Einval
            } else {
                ErrorCode::Eagain
            },
        );
    }
}

#[test]
fn audited_root_proposal_rejects_nonempty_or_nondefault_new_files_and_invalid_names() {
    let base = fixture();
    let (_, structure, parent) = retained_create_inputs(&base);
    for change in 0..8 {
        let mut created = base.guards[&2].node.clone();
        created.stats.ino = base.anchor.next_inode;
        match change {
            0 => created.stats.ino += 1,
            1 => created.stats.nlink = 0,
            2 => created.stats.uid += 1,
            3 => created.stats.gid += 1,
            4 => created.stats.size = 1,
            5 => created.stats.blocks = 1,
            6 => {
                let NodeData::File(layout) = &mut created.data else {
                    panic!()
                };
                layout.chunker.parameters.insert("chunk_size".into(), 16);
            }
            _ => {
                created.stats.mode = S_IFLNK | 0o777;
                created.stats.size = 1;
                created.data = NodeData::Symlink { target: "a".into() };
            }
        }
        assert!(
            CompactRootFileCreate::capture(
                &structure,
                &parent,
                parent.guard.identity,
                "new".into(),
                created,
                1,
                1,
            )
            .is_err()
        );
    }
    for name in ["", ".", "..", "a/b", "a\0b", "a"] {
        let mut created = base.guards[&2].node.clone();
        created.stats.ino = base.anchor.next_inode;
        assert!(
            CompactRootFileCreate::capture(
                &structure,
                &parent,
                parent.guard.identity,
                name.into(),
                created,
                1,
                1,
            )
            .is_err()
        );
    }
}

#[test]
fn audited_root_proposal_rejects_overflow_timestamps_and_forged_acknowledgements() {
    for change in 0..4 {
        let mut base = fixture();
        match change {
            0 => base.anchor.generation = u64::MAX,
            1 => base.anchor.next_inode = u64::MAX,
            2 => base.guards.get_mut(&1).unwrap().node.stats.mtime_ms = i64::MAX,
            _ => base.guards.get_mut(&1).unwrap().node.stats.ctime_ms = i64::MAX,
        }
        let (_, structure, parent) = retained_create_inputs(&base);
        let mut created = base.guards[&2].node.clone();
        created.stats.ino = base.anchor.next_inode;
        code(
            CompactRootFileCreate::capture(
                &structure,
                &parent,
                parent.guard.identity,
                "new".into(),
                created,
                i64::MAX,
                i64::MAX,
            ),
            ErrorCode::Eoverflow,
        );
    }
    let base = fixture();
    let proposal = root_create_proposal(&base, "new");
    let receipt = proposal
        .delta()
        .validate_current(
            &base.anchor,
            &BTreeMap::from([(1, base.guards[&1].clone())]),
        )
        .unwrap();
    for change in 0..3 {
        let mut forged = receipt.clone();
        match change {
            0 => forged.anchor.default_uid += 1,
            1 => forged.upserts.get_mut(&1).unwrap().node.stats.atime_ms += 1,
            _ => {
                forged.upserts.insert(2, base.guards[&2].clone());
            }
        }
        assert!(proposal.validate_publication(&forged).is_err());
    }
    let (_, structure, parent) = retained_create_inputs(&base);
    let mut created = base.guards[&2].node.clone();
    created.stats.ino = base.anchor.next_inode;
    assert!(
        CompactRootFileCreate::capture(
            &structure,
            &parent,
            parent.guard.identity,
            "new".into(),
            created,
            0,
            1,
        )
        .is_err()
    );
}

#[test]
fn full_next_witness_cannot_attach_a_different_valid_candidate_root() {
    let base = fixture();
    let (_, structure, _) = retained_create_inputs(&base);
    let candidate = create_candidate(&base, "new");
    let delta = CompactStructuralDelta::capture(&base, &candidate, StructuralScope::Full).unwrap();
    let receipt = delta.validate_current(&base.anchor, &base.guards).unwrap();
    let mut grafted = candidate;
    let NodeData::Directory { entries } = &mut grafted.nodes.get_mut(&1).unwrap().data else {
        panic!()
    };
    entries[0].name = "different-valid-name".into();
    grafted.validate().unwrap();
    assert!(
        delta
            .validate_next_structure(&structure, &receipt, &grafted)
            .is_err()
    );
}

#[test]
fn opaque_root_create_preserves_audited_root_after_unchanged_root_full_publication() {
    let mut base = fixture();
    let mut directory = base.guards[&1].clone();
    directory.node.stats.ino = 4;
    directory.node.stats.nlink = 2;
    directory.node.data = NodeData::Directory {
        entries: vec![DirectoryEntry {
            name: "nested".into(),
            inode: 5,
        }],
    };
    let mut nested_file = base.guards[&2].clone();
    nested_file.node.stats.ino = 5;
    base.guards.insert(4, directory);
    base.guards.insert(5, nested_file);
    let root = base.guards.get_mut(&1).unwrap();
    root.node.stats.nlink += 1;
    let NodeData::Directory { entries } = &mut root.node.data else {
        panic!()
    };
    entries.push(DirectoryEntry {
        name: "directory".into(),
        inode: 4,
    });
    base.anchor.members.extend([4, 5]);
    base.anchor.next_inode = 6;
    let (cached, _, structure) = base.clone().into_validated_namespace().unwrap();
    let audited_root = base.guards[&1].clone();

    // A Full rename inside a nested directory advances the authority while
    // leaving the root guard physically and logically unchanged.
    let mut candidate = cached;
    let NodeData::Directory { entries } = &mut candidate.nodes.get_mut(&4).unwrap().data else {
        panic!()
    };
    entries[0].name = "renamed".into();
    let full = CompactStructuralDelta::capture(&base, &candidate, StructuralScope::Full).unwrap();
    let receipt = full.validate_current(&base.anchor, &base.guards).unwrap();
    assert!(!receipt.upserts.contains_key(&1));
    assert_eq!(receipt.upserts.keys().copied().collect::<Vec<_>>(), vec![4]);
    let next_structure = full
        .validate_next_structure(&structure, &receipt, &candidate)
        .unwrap();
    assert_eq!(next_structure.root.as_ref(), &audited_root);
    let after_full = full.evaluate(&base).unwrap();
    assert_eq!(after_full.guards[&1], audited_root);
    assert_eq!(after_full.anchor.generation, base.anchor.generation + 1);

    let parent =
        LoadedCompactInode::from_guard(&after_full.anchor, 1, after_full.guards[&1].clone())
            .unwrap();
    let mut created = after_full.guards[&2].node.clone();
    created.stats.ino = after_full.anchor.next_inode;
    let proposal = CompactRootFileCreate::capture(
        &next_structure,
        &parent,
        parent.guard.identity,
        "new-root-file".into(),
        created,
        audited_root.node.stats.mtime_ms + 1,
        audited_root.node.stats.ctime_ms + 1,
    )
    .unwrap();
    let root_receipt = proposal
        .delta()
        .validate_current(&after_full.anchor, &BTreeMap::from([(1, parent.guard)]))
        .unwrap();
    let final_structure = proposal.validate_publication(&root_receipt).unwrap();
    let final_snapshot = proposal.delta().evaluate(&after_full).unwrap();
    let final_namespace = final_snapshot.namespace().unwrap();
    assert_eq!(final_structure.anchor(), &final_snapshot.anchor);
    assert_eq!(final_structure.root.as_ref(), &final_snapshot.guards[&1]);
    assert_eq!(final_snapshot.guards[&4], after_full.guards[&4]);
    assert_eq!(final_snapshot.guards[&5], after_full.guards[&5]);
    let NodeData::Directory { entries } = &final_namespace.nodes[&4].data else {
        panic!()
    };
    assert_eq!(
        entries,
        &vec![DirectoryEntry {
            name: "renamed".into(),
            inode: 5
        }]
    );
    let NodeData::Directory { entries } = &final_namespace.nodes[&1].data else {
        panic!()
    };
    assert_eq!(
        entries.last(),
        Some(&DirectoryEntry {
            name: "new-root-file".into(),
            inode: after_full.anchor.next_inode,
        })
    );
}

#[test]
fn full_structure_handles_rename_hardlink_orphan_removal_and_defaults() {
    let mut current = fixture();
    for step in 0..4 {
        let mut candidate = current.namespace().unwrap();
        let NodeData::Directory { entries } = &mut candidate.nodes.get_mut(&1).unwrap().data else {
            panic!()
        };
        match step {
            0 => entries[0].name = "renamed".into(),
            1 => {
                entries.push(DirectoryEntry {
                    name: "alias".into(),
                    inode: 2,
                });
                candidate.nodes.get_mut(&2).unwrap().stats.nlink = 2;
            }
            2 => {
                entries.retain(|entry| entry.inode != 2);
                candidate.nodes.get_mut(&2).unwrap().stats.nlink = 0;
            }
            _ => {
                candidate.nodes.remove(&2);
                candidate.default_gid = 27;
            }
        }
        current = CompactStructuralDelta::capture(&current, &candidate, StructuralScope::Full)
            .unwrap()
            .evaluate(&current)
            .unwrap();
        assert_eq!(current.namespace().unwrap().nodes, candidate.nodes);
        assert_eq!(current.anchor.default_gid, candidate.default_gid);
    }
    assert_eq!(current.anchor.members, vec![1, 3]);
}

#[test]
fn generation_check_cannot_prove_coherence_across_selected_writes() {
    let before = fixture();
    let held_transaction_view = before.clone();
    let first = update(&before, 2, 10);
    let second = update(&first, 3, 20);
    assert_eq!(before.anchor, second.anchor);
    let mut hybrid = before.clone();
    hybrid.guards.insert(3, second.guards[&3].clone());
    hybrid.namespace().unwrap();
    assert_ne!(hybrid, before);
    assert_ne!(hybrid, first);
    assert_ne!(hybrid, second);
    assert_eq!(held_transaction_view, before);
}

#[test]
fn receipt_contains_only_affected_guards_and_cannot_refresh_untouched_cached_body() {
    let base = fixture();
    let delta = CompactStructuralDelta::capture(
        &base,
        &create_candidate(&base, "new"),
        StructuralScope::FileCreate,
    )
    .unwrap();
    assert_eq!(
        delta.expected().keys().copied().collect::<Vec<_>>(),
        vec![1]
    );
    assert_eq!(delta.changed().keys().copied().collect::<Vec<_>>(), vec![1]);
    assert_eq!(delta.created().keys().copied().collect::<Vec<_>>(), vec![4]);
    assert!(delta.removed().is_empty());
    assert_eq!(
        delta.parent_entries(),
        &[ParentEntryPrecondition {
            parent: 1,
            name: "new".into(),
            expected: None
        }]
    );
    let parent_only = BTreeMap::from([(1, base.guards[&1].clone())]);
    let receipt = delta.validate_current(&base.anchor, &parent_only).unwrap();
    assert_eq!(
        receipt.upserts.keys().copied().collect::<Vec<_>>(),
        vec![1, 4]
    );
    assert_eq!(receipt.anchor, *delta.next_anchor());
    assert_eq!(delta.base_anchor(), &base.anchor);
    assert_eq!(delta.scope(), StructuralScope::FileCreate);
    let selected = update(&base, 2, 900);
    let committed = delta.evaluate(&selected).unwrap();
    // Both physical records project to the same logical zero after structure.
    assert_eq!(
        base.guards[&2].identity.logical_version(4).unwrap(),
        committed.guards[&2].identity.logical_version(4).unwrap()
    );
    // The old physical identity still fails CAS and cannot overwrite new bytes.
    code(
        committed.selected_update(
            base.anchor.backing,
            4,
            2,
            base.guards[&2].identity,
            base.guards[&2].node.clone(),
        ),
        ErrorCode::Eagain,
    );
    assert_eq!(committed.guards[&2].node.stats.size, 900);
}

#[test]
fn transaction_guard_sets_are_exact_and_full_scope_detects_phantoms() {
    let base = fixture();
    let candidate = create_candidate(&base, "new");
    for scope in [StructuralScope::Full, StructuralScope::FileCreate] {
        let delta = CompactStructuralDelta::capture(&base, &candidate, scope).unwrap();
        code(
            delta.validate_current(&base.anchor, &BTreeMap::new()),
            ErrorCode::Einval,
        );
        let mut guards: BTreeMap<_, _> = base
            .guards
            .iter()
            .filter(|(id, _)| delta.expected().contains_key(id))
            .map(|(&id, g)| (id, g.clone()))
            .collect();
        guards.insert(99, base.guards[&3].clone());
        code(
            delta.validate_current(&base.anchor, &guards),
            ErrorCode::Einval,
        );
    }
    let delta = CompactStructuralDelta::capture(&base, &candidate, StructuralScope::Full).unwrap();
    let mut corrupted = base.clone();
    let mut phantom = corrupted.guards.remove(&3).unwrap();
    phantom.node.stats.ino = 4;
    corrupted.guards.insert(4, phantom);
    code(delta.evaluate(&corrupted), ErrorCode::Einval);
}

#[test]
fn missing_parent_entry_precondition_conflicts_even_when_identity_is_unchanged() {
    let base = fixture();
    let delta = CompactStructuralDelta::capture(
        &base,
        &create_candidate(&base, "new"),
        StructuralScope::FileCreate,
    )
    .unwrap();
    let mut parent = base.guards[&1].clone();
    let NodeData::Directory { entries } = &mut parent.node.data else {
        panic!()
    };
    entries.push(DirectoryEntry {
        name: "new".into(),
        inode: 2,
    });
    code(
        delta.validate_current(&base.anchor, &BTreeMap::from([(1, parent)])),
        ErrorCode::Eagain,
    );
}

#[test]
fn targeted_transaction_rejects_changed_valid_parent_attributes_with_same_identity() {
    let base = fixture();
    let delta = CompactStructuralDelta::capture(
        &base,
        &create_candidate(&base, "new"),
        StructuralScope::FileCreate,
    )
    .unwrap();
    let mut current = base.clone();
    current.guards.get_mut(&1).unwrap().node.stats.mode ^= 0o001;
    current.namespace().unwrap();
    let parent = current.guards.remove(&1).unwrap();
    assert_eq!(parent.identity, delta.expected()[&1]);
    let result = delta.validate_current(&base.anchor, &BTreeMap::from([(1, parent)]));
    assert!(
        result.is_err(),
        "a valid parent attribute change with the same identity must not be overwritten: {result:?}"
    );
}

#[test]
fn targeted_transaction_rejects_changed_valid_parent_entries_with_same_identity() {
    let base = fixture();
    let delta = CompactStructuralDelta::capture(
        &base,
        &create_candidate(&base, "new"),
        StructuralScope::FileCreate,
    )
    .unwrap();
    let mut current = base.clone();
    let NodeData::Directory { entries } = &mut current.guards.get_mut(&1).unwrap().node.data else {
        panic!()
    };
    entries[0].name = "renamed".into();
    assert!(entries.iter().all(|entry| entry.name != "new"));
    current.namespace().unwrap();
    let parent = current.guards.remove(&1).unwrap();
    assert_eq!(parent.identity, delta.expected()[&1]);
    let result = delta.validate_current(&base.anchor, &BTreeMap::from([(1, parent)]));
    assert!(
        result.is_err(),
        "a valid parent entry change with the same identity must not be overwritten: {result:?}"
    );
}

#[test]
fn targeted_transaction_rejects_malformed_affected_directory() {
    let base = fixture();
    let delta = CompactStructuralDelta::capture(
        &base,
        &create_candidate(&base, "new"),
        StructuralScope::FileCreate,
    )
    .unwrap();
    for entry in [
        DirectoryEntry {
            name: "a".into(),
            inode: 2,
        },
        DirectoryEntry {
            name: "bad/name".into(),
            inode: 2,
        },
        DirectoryEntry {
            name: "phantom".into(),
            inode: 99,
        },
    ] {
        let mut parent = base.guards[&1].clone();
        let NodeData::Directory { entries } = &mut parent.node.data else {
            panic!()
        };
        entries.push(entry);
        code(
            delta.validate_current(&base.anchor, &BTreeMap::from([(1, parent)])),
            ErrorCode::Einval,
        );
    }
}

#[test]
fn targeted_transaction_rejects_file_as_affected_parent() {
    let base = fixture();
    let mut node = base.guards[&2].node.clone();
    node.stats.ino = base.anchor.root;
    assert_non_directory_parent_rejected(&base, node);
}

#[test]
fn targeted_transaction_rejects_symlink_as_affected_parent() {
    let base = fixture();
    let mut node = base.guards[&2].node.clone();
    node.stats.ino = base.anchor.root;
    node.stats.mode = S_IFLNK | 0o777;
    node.data = NodeData::Symlink { target: "a".into() };
    assert_non_directory_parent_rejected(&base, node);
}

fn assert_non_directory_parent_rejected(base: &CompactSnapshot, node: NodeMetadata) {
    // Node-kind validity and exact physical identity both pass. The precondition
    // must still reject a non-directory rather than interpreting it as empty.
    validate_node_kind(&node).unwrap();
    let delta = CompactStructuralDelta::capture(
        base,
        &create_candidate(base, "new"),
        StructuralScope::FileCreate,
    )
    .unwrap();
    let mut parent = base.guards[&base.anchor.root].clone();
    parent.node = node;
    assert_eq!(parent.identity, delta.expected()[&base.anchor.root]);
    let guards = BTreeMap::from([(base.anchor.root, parent)]);
    code(
        delta.validate_current(&base.anchor, &guards),
        ErrorCode::Einval,
    );
}

#[test]
fn compact_anchor_codec_is_strict_and_cannot_be_legacy_namespace() {
    let snapshot = fixture();
    let encoded = encode_compact_anchor(&snapshot.anchor).unwrap();
    assert_eq!(decode_compact_anchor(&encoded).unwrap(), snapshot.anchor);
    assert!(serde_json::from_slice::<Namespace>(&encoded).is_err());
    assert!(decode_inode_namespace(&encoded).is_err());
    let namespace = snapshot.namespace().unwrap();
    assert!(decode_compact_anchor(&serde_json::to_vec(&namespace).unwrap()).is_err());
    assert!(decode_compact_anchor(&encode_inode_namespace(&namespace).unwrap()).is_err());
    for field in ["version", "layout", "extra", "anchor"] {
        let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        value[field] = serde_json::json!(999);
        assert!(decode_compact_anchor(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    let mut value: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    value["anchor"]["extra"] = serde_json::json!(true);
    assert!(decode_compact_anchor(&serde_json::to_vec(&value).unwrap()).is_err());
    value["anchor"].as_object_mut().unwrap().remove("extra");
    value["anchor"]["members"] = serde_json::json!([1, 1]);
    assert!(decode_compact_anchor(&serde_json::to_vec(&value).unwrap()).is_err());
    assert!(decode_compact_anchor(b"{}").is_err());
    assert!(decode_compact_anchor(b"not json").is_err());
}

#[test]
fn compact_selected_read_validates_membership_body_and_physical_identity() {
    let snapshot = fixture();
    let guard = snapshot.guards[&2].clone();
    let loaded = LoadedCompactInode::from_guard(&snapshot.anchor, 2, guard.clone()).unwrap();
    assert_eq!(loaded.generation, 3);
    assert_eq!(loaded.guard, guard);
    let mut anchor = snapshot.anchor.clone();
    anchor.members.retain(|id| *id != 2);
    assert!(LoadedCompactInode::from_guard(&anchor, 2, guard.clone()).is_err());
    let mut wrong = guard.clone();
    wrong.identity.epoch = 4;
    assert!(LoadedCompactInode::from_guard(&snapshot.anchor, 2, wrong).is_err());
    let mut wrong = guard.clone();
    wrong.node.stats.ino = 9;
    assert!(LoadedCompactInode::from_guard(&snapshot.anchor, 2, wrong).is_err());
    let mut wrong = guard;
    wrong.node.stats.mode = S_IFDIR | 0o755;
    assert!(LoadedCompactInode::from_guard(&snapshot.anchor, 2, wrong).is_err());
}

#[test]
fn consuming_snapshot_attests_graph_and_preserves_exact_physical_identities() {
    let snapshot = fixture();
    let (namespace, identities, structure) = snapshot.clone().into_validated_namespace().unwrap();
    assert_eq!(namespace.nodes[&2], snapshot.guards[&2].node);
    assert_eq!(identities[&2], snapshot.guards[&2].identity);
    assert_eq!(structure.anchor(), &snapshot.anchor);

    let mut invalid = snapshot;
    invalid.guards.get_mut(&1).unwrap().node.stats.nlink += 1;
    assert!(invalid.into_validated_namespace().is_err());
}

#[test]
fn structural_receipt_rejects_extra_or_changed_upserts() {
    let base = fixture();
    let candidate = create_candidate(&base, "new");
    let delta = CompactStructuralDelta::capture(&base, &candidate, StructuralScope::Full).unwrap();
    let receipt = delta.evaluate(&base).unwrap();
    let expected = CompactPublication {
        anchor: receipt.anchor.clone(),
        upserts: receipt
            .guards
            .iter()
            .filter(|(id, _)| delta.changed().contains_key(id) || delta.created().contains_key(id))
            .map(|(&id, guard)| (id, guard.clone()))
            .collect(),
        removed: delta.removed().clone(),
    };
    delta.validate_receipt(&expected).unwrap();
    let mut forged = expected.clone();
    forged.upserts.insert(2, base.guards[&2].clone());
    assert!(delta.validate_receipt(&forged).is_err());
    let mut forged = expected;
    forged.upserts.get_mut(&1).unwrap().identity.revision += 1;
    assert!(delta.validate_receipt(&forged).is_err());
}

fn root_file_fixture() -> CompactSnapshot {
    let mut base = fixture();
    let file = &mut base.guards.get_mut(&2).unwrap().node;
    file.stats.size = 8192;
    file.stats.blocks = 8;
    let NodeData::File(layout) = &mut file.data else {
        panic!()
    };
    layout.extents = vec![BlockExtent {
        file_offset: 4096,
        block: BlockId("source-content-block".into()),
        block_offset: 128,
        length: 4096,
    }];
    let mut tombstone = base.guards[&2].clone();
    tombstone.node.stats.ino = 4;
    tombstone.node.stats.nlink = 0;
    base.guards.insert(4, tombstone);
    base.anchor.members.push(4);
    base.anchor.next_inode = 5;
    base.namespace().unwrap();
    base
}

fn root_file_read(base: &CompactSnapshot, inode: InodeId) -> CompactRootFileRead {
    CompactRootFileRead::from_guards(
        base.anchor.clone(),
        base.anchor.root,
        inode,
        Some(base.guards[&base.anchor.root].clone()),
        base.guards.get(&inode).cloned(),
    )
    .unwrap()
}

fn root_file_proposal(
    base: &CompactSnapshot,
    structure: &ValidatedCompactStructure,
    inode: InodeId,
    intent: CompactRootFileIntent,
) -> CompactRootFileTransition {
    let verified = structure
        .verify_root_file(
            root_file_read(base, inode),
            inode,
            &base.guards[&inode].node,
            base.guards[&inode].identity,
        )
        .unwrap();
    let increment = match intent {
        CompactRootFileIntent::RenameAbsent { .. } => 2,
        CompactRootFileIntent::UnlinkLastLink { .. } => 1,
    };
    let times = CompactRootFileTimes {
        parent_mtime_ms: verified.root().node.stats.mtime_ms + increment,
        parent_ctime_ms: verified.root().node.stats.ctime_ms + increment,
        file_ctime_ms: verified.file().node.stats.ctime_ms + 1,
    };
    CompactRootFileTransition::capture(verified, intent, times).unwrap()
}

fn root_file_reference_candidate(base: &CompactSnapshot, rename: bool) -> Namespace {
    let mut candidate = base.namespace().unwrap();
    let root = candidate.nodes.get_mut(&1).unwrap();
    let NodeData::Directory { entries } = &mut root.data else {
        panic!()
    };
    entries.retain(|entry| entry.name != "a");
    if rename {
        entries.push(DirectoryEntry {
            name: "renamed".into(),
            inode: 2,
        });
    }
    root.stats.mtime_ms += if rename { 2 } else { 1 };
    root.stats.ctime_ms += if rename { 2 } else { 1 };
    let file = candidate.nodes.get_mut(&2).unwrap();
    file.stats.ctime_ms += 1;
    if !rename {
        file.stats.nlink = 0;
    }
    candidate
}

#[test]
fn root_file_rename_absent_matches_full_transition() {
    let base = root_file_fixture();
    let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
    let proposal = root_file_proposal(
        &base,
        &structure,
        2,
        CompactRootFileIntent::RenameAbsent {
            from: "a".into(),
            to: "renamed".into(),
        },
    );
    let candidate = root_file_reference_candidate(&base, true);
    let reference =
        CompactStructuralDelta::capture(&base, &candidate, StructuralScope::Full).unwrap();
    assert_eq!(
        proposal.delta().scope(),
        StructuralScope::RootFileRenameAbsent
    );
    assert_eq!(
        proposal
            .delta()
            .expected()
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    let affected = BTreeMap::from([(1, base.guards[&1].clone()), (2, base.guards[&2].clone())]);
    let receipt = proposal
        .delta()
        .validate_current(&base.anchor, &affected)
        .unwrap();
    assert_eq!(
        receipt,
        reference
            .validate_current(&base.anchor, &base.guards)
            .unwrap()
    );
    assert_eq!(
        proposal.delta().evaluate(&base).unwrap(),
        reference.evaluate(&base).unwrap()
    );
    let next = proposal.validate_publication(&receipt).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &next.root_children,
        &structure.root_children
    ));
    assert!(std::sync::Arc::ptr_eq(
        &next.root_single_link_file_bits,
        &structure.root_single_link_file_bits
    ));
    assert!(next.root_single_link_file(2));
    assert!(structure.root_single_link_file(2));
    let concurrent = update(&base, 3, 4097);
    let after = proposal.delta().evaluate(&concurrent).unwrap();
    assert_eq!(after.guards[&3], concurrent.guards[&3]);
}

#[test]
fn root_file_unlink_last_link_matches_full_tombstone_transition() {
    let base = root_file_fixture();
    let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
    let proposal = root_file_proposal(
        &base,
        &structure,
        2,
        CompactRootFileIntent::UnlinkLastLink { name: "a".into() },
    );
    let reference = CompactStructuralDelta::capture(
        &base,
        &root_file_reference_candidate(&base, false),
        StructuralScope::Full,
    )
    .unwrap();
    assert_eq!(
        proposal.delta().scope(),
        StructuralScope::RootFileUnlinkLastLink
    );
    let affected = BTreeMap::from([(1, base.guards[&1].clone()), (2, base.guards[&2].clone())]);
    let receipt = proposal
        .delta()
        .validate_current(&base.anchor, &affected)
        .unwrap();
    assert_eq!(
        receipt,
        reference
            .validate_current(&base.anchor, &base.guards)
            .unwrap()
    );
    let next = proposal.delta().evaluate(&base).unwrap();
    assert_eq!(next, reference.evaluate(&base).unwrap());
    assert_eq!(next.anchor.members, base.anchor.members);
    assert_eq!(next.anchor.next_inode, base.anchor.next_inode);
    assert_eq!(next.guards[&2].node.stats.nlink, 0);
    assert_eq!(next.guards[&2].node.data, base.guards[&2].node.data);
    let witness = proposal.validate_publication(&receipt).unwrap();
    assert!(!witness.root_single_link_file(2));
    assert!(witness.root_single_link_file(3));
    assert!(structure.root_single_link_file(2));
    let mut written = next.guards[&2].node.clone();
    written.stats.size = 9000;
    written.stats.mtime_ms += 1;
    let next = next
        .selected_update(
            next.anchor.backing,
            next.anchor.generation,
            2,
            next.guards[&2].identity,
            written,
        )
        .unwrap();
    assert_eq!(next.guards[&2].node.stats.nlink, 0);
    assert_eq!(next.guards[&2].node.stats.size, 9000);
}

#[test]
fn root_file_publication_requires_exact_bodies_and_receipts() {
    let base = fixture();
    let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
    let proposal = root_file_proposal(
        &base,
        &structure,
        2,
        CompactRootFileIntent::UnlinkLastLink { name: "a".into() },
    );
    let affected = BTreeMap::from([(1, base.guards[&1].clone()), (2, base.guards[&2].clone())]);
    for inode in [1, 2] {
        let mut changed = affected.clone();
        changed.get_mut(&inode).unwrap().node.stats.mtime_ms += 1;
        code(
            proposal.delta().validate_current(&base.anchor, &changed),
            ErrorCode::Einval,
        );
        let mut changed = affected.clone();
        changed.get_mut(&inode).unwrap().identity.revision += 1;
        code(
            proposal.delta().validate_current(&base.anchor, &changed),
            ErrorCode::Eagain,
        );
        let mut changed = affected.clone();
        changed.remove(&inode);
        code(
            proposal.delta().validate_current(&base.anchor, &changed),
            ErrorCode::Einval,
        );
    }
    let mut changed = affected.clone();
    changed.insert(3, base.guards[&3].clone());
    code(
        proposal.delta().validate_current(&base.anchor, &changed),
        ErrorCode::Einval,
    );
    let receipt = proposal
        .delta()
        .validate_current(&base.anchor, &affected)
        .unwrap();
    for field in 0..8 {
        let mut forged = receipt.clone();
        match field {
            0 => forged.anchor.members.pop().map(|_| ()).unwrap(),
            1 => forged.anchor.default_uid += 1,
            2 => forged.anchor.next_inode += 1,
            3 => forged.anchor.generation += 1,
            4 => {
                forged.removed.insert(2);
            }
            5 => {
                forged.upserts.remove(&2);
            }
            6 => forged.upserts.get_mut(&2).unwrap().identity.incarnation += 1,
            _ => forged.upserts.get_mut(&2).unwrap().node.stats.size += 1,
        }
        code(proposal.validate_publication(&forged), ErrorCode::Einval);
    }
    for scope in [
        StructuralScope::RootFileRenameAbsent,
        StructuralScope::RootFileUnlinkLastLink,
    ] {
        code(
            CompactStructuralDelta::capture(
                &base,
                &root_file_reference_candidate(&base, false),
                scope,
            ),
            ErrorCode::Einval,
        );
    }
}

#[test]
fn root_file_capture_preserves_missing_groups_and_rejects_identity_regression() {
    let base = fixture();
    let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
    let absent = CompactRootFileRead::from_guards(base.anchor.clone(), 1, 2, None, None).unwrap();
    assert!(absent.root().is_none());
    assert!(absent.file().is_none());
    code(
        structure.verify_root_file(absent, 2, &base.guards[&2].node, base.guards[&2].identity),
        ErrorCode::Einval,
    );
    let mut changed = base.clone();
    changed.guards.get_mut(&2).unwrap().node.stats.size = 10;
    code(
        structure.verify_root_file(
            root_file_read(&changed, 2),
            2,
            &base.guards[&2].node,
            base.guards[&2].identity,
        ),
        ErrorCode::Estale,
    );
    changed.guards.get_mut(&2).unwrap().identity.revision += 1;
    let verified = structure
        .verify_root_file(
            root_file_read(&changed, 2),
            2,
            &base.guards[&2].node,
            base.guards[&2].identity,
        )
        .unwrap();
    assert_eq!(verified.file(), &changed.guards[&2]);
    let mut regressed = changed.clone();
    regressed.guards.get_mut(&2).unwrap().identity.revision = 0;
    code(
        structure.verify_root_file(
            root_file_read(&regressed, 2),
            2,
            &changed.guards[&2].node,
            changed.guards[&2].identity,
        ),
        ErrorCode::Estale,
    );
}

#[test]
fn root_file_capture_rejects_wrong_names_mapping_and_clock_exhaustion() {
    let base = fixture();
    let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
    let verify = || {
        structure
            .verify_root_file(
                root_file_read(&base, 2),
                2,
                &base.guards[&2].node,
                base.guards[&2].identity,
            )
            .unwrap()
    };
    let times = CompactRootFileTimes {
        parent_mtime_ms: 2,
        parent_ctime_ms: 2,
        file_ctime_ms: 1,
    };
    for name in ["", ".", "..", "a/b", "a\0b"] {
        code(
            CompactRootFileTransition::capture(
                verify(),
                CompactRootFileIntent::UnlinkLastLink { name: name.into() },
                times,
            ),
            ErrorCode::Einval,
        );
    }
    code(
        CompactRootFileTransition::capture(
            verify(),
            CompactRootFileIntent::UnlinkLastLink { name: "b".into() },
            times,
        ),
        ErrorCode::Enoent,
    );
    code(
        CompactRootFileTransition::capture(
            verify(),
            CompactRootFileIntent::RenameAbsent {
                from: "a".into(),
                to: "b".into(),
            },
            times,
        ),
        ErrorCode::Eexist,
    );
    code(
        CompactRootFileTransition::capture(
            verify(),
            CompactRootFileIntent::RenameAbsent {
                from: "a".into(),
                to: "a".into(),
            },
            times,
        ),
        ErrorCode::Einval,
    );
    for field in 0..3 {
        let mut stale_times = times;
        match field {
            0 => stale_times.parent_mtime_ms = 1,
            1 => stale_times.parent_ctime_ms = 1,
            _ => stale_times.file_ctime_ms = 0,
        }
        code(
            CompactRootFileTransition::capture(
                verify(),
                CompactRootFileIntent::RenameAbsent {
                    from: "a".into(),
                    to: "new".into(),
                },
                stale_times,
            ),
            ErrorCode::Einval,
        );
    }
    let long_name = "n".repeat(1000);
    CompactRootFileTransition::capture(
        verify(),
        CompactRootFileIntent::RenameAbsent {
            from: "a".into(),
            to: long_name,
        },
        times,
    )
    .unwrap();
    for field in 0..4 {
        let mut exhausted = base.clone();
        match field {
            0 => exhausted.anchor.generation = u64::MAX,
            1 => exhausted.guards.get_mut(&1).unwrap().node.stats.mtime_ms = i64::MAX,
            2 => exhausted.guards.get_mut(&1).unwrap().node.stats.ctime_ms = i64::MAX,
            _ => exhausted.guards.get_mut(&2).unwrap().node.stats.ctime_ms = i64::MAX,
        }
        let (_, _, witness) = exhausted.clone().into_validated_namespace().unwrap();
        let verified = witness
            .verify_root_file(
                root_file_read(&exhausted, 2),
                2,
                &exhausted.guards[&2].node,
                exhausted.guards[&2].identity,
            )
            .unwrap();
        code(
            CompactRootFileTransition::capture(
                verified,
                CompactRootFileIntent::UnlinkLastLink { name: "a".into() },
                CompactRootFileTimes {
                    parent_mtime_ms: i64::MAX,
                    parent_ctime_ms: i64::MAX,
                    file_ctime_ms: i64::MAX,
                },
            ),
            ErrorCode::Eoverflow,
        );
    }
}

#[test]
fn root_file_capture_does_not_certify_same_generation_anchor_or_structural_changes() {
    let base = fixture();
    let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
    for field in 0..7 {
        let mut changed = base.clone();
        match field {
            0 => changed.anchor.default_uid += 1,
            1 => changed.anchor.next_inode += 1,
            2 => changed.guards.get_mut(&1).unwrap().node.stats.mode ^= 1,
            3 => changed.guards.get_mut(&2).unwrap().node.stats.mode ^= 1,
            4 => changed.guards.get_mut(&2).unwrap().node.stats.nlink = 0,
            5 => {
                let file = &mut changed.guards.get_mut(&2).unwrap().node;
                file.stats.mode = S_IFLNK | 0o777;
                file.data = NodeData::Symlink {
                    target: "target".into(),
                };
            }
            _ => changed.guards.get_mut(&2).unwrap().identity.incarnation = 2,
        }
        let expected = if field < 3 {
            ErrorCode::Einval
        } else {
            ErrorCode::Estale
        };
        code(
            structure.verify_root_file(
                root_file_read(&changed, 2),
                2,
                &base.guards[&2].node,
                base.guards[&2].identity,
            ),
            expected,
        );
    }
    let mut changed = base.clone();
    changed.anchor.generation += 1;
    let read = CompactRootFileRead::from_guards(changed.anchor.clone(), 1, 2, None, None).unwrap();
    code(
        structure.verify_root_file(read, 2, &base.guards[&2].node, base.guards[&2].identity),
        ErrorCode::Eagain,
    );
    for (root, source) in [(0, 2), (1, 0), (1, 1)] {
        code(
            CompactRootFileRead::from_guards(base.anchor.clone(), root, source, None, None),
            ErrorCode::Einval,
        );
    }
    code(
        CompactRootFileRead::from_guards(
            base.anchor.clone(),
            1,
            2,
            Some(base.guards[&3].clone()),
            Some(base.guards[&2].clone()),
        ),
        ErrorCode::Einval,
    );
}

fn mixed_sparse_root_children(count: usize) -> CompactSnapshot {
    let base = fixture();
    let mut result = CompactSnapshot {
        anchor: base.anchor.clone(),
        guards: BTreeMap::new(),
    };
    let mut entries = Vec::new();
    let mut directories = 0;
    for position in (0..count).rev() {
        let inode = (1_u64 << 40) + position as u64 * 17;
        let mut guard = base.guards[&2].clone();
        guard.node.stats.ino = inode;
        if [0, count / 2, count - 1].contains(&position) {
            // Keep the independently chosen removal positions eligible.
        } else {
            match position % 4 {
                0 => {}
                1 => {
                    guard.node.stats.nlink = 2;
                    entries.push(DirectoryEntry {
                        name: format!("alias-{position}"),
                        inode,
                    });
                }
                2 => {
                    guard.node.stats.mode = S_IFLNK | 0o777;
                    guard.node.data = NodeData::Symlink {
                        target: "target".into(),
                    };
                }
                _ => {
                    guard.node.stats.mode = S_IFDIR | 0o755;
                    guard.node.stats.nlink = 2;
                    guard.node.data = NodeData::Directory { entries: vec![] };
                    directories += 1;
                }
            }
        }
        entries.push(DirectoryEntry {
            name: format!("child-{position}"),
            inode,
        });
        result.guards.insert(inode, guard);
    }
    let mut root = base.guards[&1].clone();
    root.node.stats.nlink = 2 + directories;
    root.node.data = NodeData::Directory { entries };
    result.guards.insert(1, root);
    let orphan_inode = (1_u64 << 40) + count as u64 * 17;
    let mut orphan = base.guards[&2].clone();
    orphan.node.stats.ino = orphan_inode;
    orphan.node.stats.nlink = 0;
    result.guards.insert(orphan_inode, orphan);
    result.anchor.members = result.guards.keys().copied().collect();
    result.anchor.next_inode = orphan_inode + 1;
    result.namespace().unwrap();
    result
}

#[test]
fn root_file_eligibility_bits_follow_exact_child_index() {
    for count in [1, 63, 64, 65, 128, 1000, 1023, 1024, 1025] {
        let base = mixed_sparse_root_children(count);
        let namespace = base.namespace().unwrap();
        let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
        assert_eq!(structure.root_children.len(), count);
        assert_eq!(
            structure.root_single_link_file_bits.len(),
            count.div_ceil(64)
        );
        if count % 64 != 0 {
            assert_eq!(
                structure.root_single_link_file_bits.last().unwrap() >> (count % 64),
                0
            );
        }
        let root_ids: BTreeSet<_> = match &namespace.nodes[&1].data {
            NodeData::Directory { entries } => entries.iter().map(|entry| entry.inode).collect(),
            _ => panic!(),
        };
        for (&inode, node) in &namespace.nodes {
            let expected = root_ids.contains(&inode)
                && matches!(node.data, NodeData::File(_))
                && node.stats.nlink == 1;
            assert_eq!(structure.root_single_link_file(inode), expected);
        }
        assert!(!structure.root_single_link_file(u64::MAX));
        for position in [0, count / 2, count - 1] {
            let source = (1_u64 << 40) + position as u64 * 17;
            let proposal = root_file_proposal(
                &base,
                &structure,
                source,
                CompactRootFileIntent::UnlinkLastLink {
                    name: format!("child-{position}"),
                },
            );
            let affected = BTreeMap::from([
                (1, base.guards[&1].clone()),
                (source, base.guards[&source].clone()),
            ]);
            let receipt = proposal
                .delta()
                .validate_current(&base.anchor, &affected)
                .unwrap();
            let next = proposal.validate_publication(&receipt).unwrap();
            let reference = proposal
                .delta()
                .evaluate(&base)
                .unwrap()
                .namespace()
                .unwrap();
            let remaining: BTreeSet<_> = match &reference.nodes[&1].data {
                NodeData::Directory { entries } => {
                    entries.iter().map(|entry| entry.inode).collect()
                }
                _ => panic!(),
            };
            assert_eq!(
                next.root_children.as_ref(),
                remaining.iter().copied().collect::<Vec<_>>()
            );
            for (&inode, node) in &reference.nodes {
                let expected = remaining.contains(&inode)
                    && matches!(node.data, NodeData::File(_))
                    && node.stats.nlink == 1;
                assert_eq!(
                    next.root_single_link_file(inode),
                    expected,
                    "count={count} position={position} inode={inode}"
                );
            }
            assert!(structure.root_single_link_file(source));
            assert!(!next.root_single_link_file(source));
        }
        let parent =
            LoadedCompactInode::from_guard(&base.anchor, 1, base.guards[&1].clone()).unwrap();
        let mut created = fixture().guards[&2].node.clone();
        created.stats.ino = base.anchor.next_inode;
        let proposal = CompactRootFileCreate::capture(
            &structure,
            &parent,
            parent.guard.identity,
            "appended".into(),
            created,
            1,
            1,
        )
        .unwrap();
        let affected = BTreeMap::from([(1, base.guards[&1].clone())]);
        let receipt = proposal
            .delta()
            .validate_current(&base.anchor, &affected)
            .unwrap();
        let next = proposal.validate_publication(&receipt).unwrap();
        assert!(next.root_single_link_file(base.anchor.next_inode));
        for &inode in structure.root_children.iter() {
            assert_eq!(
                next.root_single_link_file(inode),
                structure.root_single_link_file(inode)
            );
        }
        assert_eq!(structure.root_children.len(), count);
    }
}

#[test]
fn full_witness_rejects_different_receipt_time_eligibility() {
    let base = fixture();
    let candidate = base.namespace().unwrap();
    let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
    let delta = CompactStructuralDelta::capture(&base, &candidate, StructuralScope::Full).unwrap();
    let receipt = delta.validate_current(&base.anchor, &base.guards).unwrap();
    let mut changed = candidate;
    let file = changed.nodes.get_mut(&2).unwrap();
    file.stats.mode = S_IFLNK | 0o777;
    file.data = NodeData::Symlink {
        target: "different-kind".into(),
    };
    changed.validate().unwrap();
    code(
        delta.validate_next_structure(&structure, &receipt, &changed),
        ErrorCode::Einval,
    );
}

#[test]
fn root_file_validator_rejects_arbitrary_candidate_edits() {
    let base = root_file_fixture();
    let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
    let proposal = root_file_proposal(
        &base,
        &structure,
        2,
        CompactRootFileIntent::RenameAbsent {
            from: "a".into(),
            to: "renamed".into(),
        },
    );
    let affected = BTreeMap::from([(1, base.guards[&1].clone()), (2, base.guards[&2].clone())]);
    for field in 0..12 {
        let mut forged = proposal.delta().clone();
        match field {
            0 => forged.next.default_gid += 1,
            1 => forged.next.next_inode += 1,
            2 => {
                forged.next.members.pop();
            }
            3 => forged.changed.get_mut(&1).unwrap().stats.mode ^= 1,
            4 => forged.changed.get_mut(&2).unwrap().stats.uid += 1,
            5 => forged.changed.get_mut(&2).unwrap().stats.size += 1,
            6 => {
                let NodeData::File(layout) = &mut forged.changed.get_mut(&2).unwrap().data else {
                    panic!()
                };
                layout.extents[0].block = BlockId("other-content-block".into());
            }
            7 => {
                forged.expected.remove(&2);
            }
            8 => {
                forged.created.insert(5, fixture().guards[&2].node.clone());
            }
            9 => {
                forged.removed.insert(2);
            }
            10 => forged.entries[1].name = forged.entries[0].name.clone(),
            _ => forged.changed.get_mut(&2).unwrap().stats.nlink = 0,
        }
        code(
            forged.validate_current(&base.anchor, &affected),
            ErrorCode::Einval,
        );
    }
    let mut duplicated = base.clone();
    let NodeData::Directory { entries } = &mut duplicated.guards.get_mut(&1).unwrap().node.data
    else {
        panic!()
    };
    entries.push(entries[0].clone());
    code(
        CompactRootFileRead::from_guards(
            duplicated.anchor.clone(),
            1,
            2,
            Some(duplicated.guards[&1].clone()),
            Some(duplicated.guards[&2].clone()),
        ),
        ErrorCode::Einval,
    );
}

#[test]
fn legacy_file_create_rejects_cached_root_child_eligibility_grafting() {
    for persisted_regular in [false, true] {
        let mut persisted = fixture();
        if !persisted_regular {
            let node = &mut persisted.guards.get_mut(&3).unwrap().node;
            node.stats.mode = S_IFLNK | 0o777;
            node.data = NodeData::Symlink {
                target: "persisted-symlink".into(),
            };
        }
        let (mut cached, structure, parent) = retained_create_inputs(&persisted);
        let node = cached.nodes.get_mut(&3).unwrap();
        if persisted_regular {
            node.stats.mode = S_IFLNK | 0o777;
            node.data = NodeData::Symlink {
                target: "grafted-symlink".into(),
            };
        } else {
            *node = fixture().guards[&3].node.clone();
        }
        // Both graphs validate and the fresh root is byte-for-byte identical.
        // The cached classification is nevertheless unrelated to this audit.
        cached.validate().unwrap();
        let mut candidate = cached.clone();
        let mut file = fixture().guards[&2].node.clone();
        file.stats.ino = candidate.next_inode;
        candidate.nodes.insert(candidate.next_inode, file);
        candidate.next_inode += 1;
        let NodeData::Directory { entries } = &mut candidate.nodes.get_mut(&1).unwrap().data else {
            panic!()
        };
        entries.push(DirectoryEntry {
            name: "created".into(),
            inode: cached.next_inode,
        });
        candidate.validate().unwrap();
        code(
            CompactStructuralDelta::capture_file_create(
                &structure,
                &cached,
                &parent,
                parent.guard.identity,
                &candidate,
            ),
            ErrorCode::Einval,
        );
    }
}
