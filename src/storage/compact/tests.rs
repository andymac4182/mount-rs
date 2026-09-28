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
