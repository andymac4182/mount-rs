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
