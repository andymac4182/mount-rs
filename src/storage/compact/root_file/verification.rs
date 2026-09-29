//! Bounded formal checks for compact root-file transitions.
//! Execution is pending. Kani is absent on the current host. These harnesses
//! make no provider, allocation-cost, cancellation, or durability claim.

use super::*;

// Fixtures and assertion oracles below do not implement a production decision.
// Every acceptance, transition, packed edit and receipt decision is made by
// the actual production function. No production function is stubbed.

fn bit_at(words: &[u64], index: usize) -> bool {
    words[index / 64] & (1_u64 << (index % 64)) != 0
}

fn arbitrary_valid_words(children: usize) -> ([u64; 3], usize) {
    let mut words = [kani::any(), kani::any(), kani::any()];
    let used = children / 64 + usize::from(children % 64 != 0);
    if children % 64 != 0 {
        words[used - 1] &= (1_u64 << (children % 64)) - 1;
    }
    (words, used)
}

/// Universally selected old-bit query checks preservation without unrolling
/// 129 individual bit comparisons. Three arbitrary words represent all valid
/// packed inputs for the bounded child count, including mixed eligibility.
#[kani::proof]
#[kani::unwind(32)]
fn compact_root_file_append_preserves_packed_eligibility() {
    let children: usize = kani::any();
    kani::assume(children <= 129);
    let (words, used) = arbitrary_valid_words(children);
    let old = &words[..used];
    let next = append_eligible_bit(old, children);

    let next_count = children + 1;
    let required_words = next_count / 64 + usize::from(next_count % 64 != 0);
    assert_eq!(next.len(), required_words);
    assert!(bit_at(&next, children));
    let index: usize = kani::any();
    kani::assume(children == 0 || index < children);
    if children != 0 {
        assert_eq!(bit_at(&next, index), bit_at(old, index));
    }
    if next_count % 64 != 0 {
        assert_eq!(next[next.len() - 1] >> (next_count % 64), 0);
    }

    kani::cover!(children == 0 && next[0] == 1);
    kani::cover!(children == 63);
    kani::cover!(children == 64);
    kani::cover!(children == 65);
    kani::cover!(children == 127);
    kani::cover!(children == 128);
    kani::cover!(children == 129);
    kani::cover!(children > 1 && !bit_at(old, 0) && bit_at(old, 1));
}

/// The independent sequence oracle maps every arbitrary surviving position
/// to its pre-removal position. It never reproduces the production word shift.
#[kani::proof]
#[kani::unwind(32)]
fn compact_root_file_remove_preserves_packed_eligibility() {
    let children: usize = kani::any();
    let position: usize = kani::any();
    kani::assume(children >= 1 && children <= 129);
    kani::assume(position < children);
    let (words, used) = arbitrary_valid_words(children);
    let old = &words[..used];
    let next = remove_eligible_bit(old, children, position);

    let remaining = children - 1;
    let required_words = remaining / 64 + usize::from(remaining % 64 != 0);
    assert_eq!(next.len(), required_words);
    let index: usize = kani::any();
    kani::assume(remaining == 0 || index < remaining);
    if remaining != 0 {
        let original_index = if index < position { index } else { index + 1 };
        assert_eq!(bit_at(&next, index), bit_at(old, original_index));
    }
    if remaining % 64 != 0 {
        assert_eq!(next[next.len() - 1] >> (remaining % 64), 0);
    }

    kani::cover!(children == 1 && next.is_empty());
    kani::cover!(children > 1 && position == 0);
    kani::cover!(children > 1 && position == children - 1);
    kani::cover!(children == 65 && position == 63 && bit_at(old, 64));
    kani::cover!(children == 65 && position == 64);
    kani::cover!(children == 129 && position == 127 && bit_at(old, 128));
    kani::cover!(children == 129 && position == 128);
    kani::cover!(children == 129 && position == 64 && !bit_at(old, 65));
}

fn fixed_chunker() -> ChunkerConfig {
    ChunkerConfig {
        algorithm: "fixed-size".into(),
        version: 1,
        parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
    }
}

fn node(inode: InodeId, mode: u32, links: u64, data: NodeData) -> NodeMetadata {
    NodeMetadata {
        stats: Stats {
            dev: 7,
            ino: inode,
            mode,
            nlink: links,
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
        },
        data,
    }
}

/// Representation-level lookup proof: the three sorted IDs and three aligned
/// eligibility flags are assumed to have been established by a prior audit.
/// The dummy root/anchor are not used by the production lookup. This harness
/// does not claim to establish graph audit or classification provenance.
#[kani::proof]
#[kani::unwind(32)]
fn compact_root_file_sparse_lookup_uses_child_position() {
    let first: u64 = kani::any();
    let second: u64 = kani::any();
    let third: u64 = kani::any();
    kani::assume(1 < first && first < second && second < third && third < u64::MAX);
    let flags: [bool; 3] = kani::any();
    let query: u64 = kani::any();
    let word = u64::from(flags[0]) | (u64::from(flags[1]) << 1) | (u64::from(flags[2]) << 2);
    let structure = ValidatedCompactStructure {
        anchor: Arc::new(CompactAnchor {
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            generation: 1,
            root: 1,
            next_inode: 2,
            default_uid: 10,
            default_gid: 20,
            umask: 0o022,
            default_chunker: fixed_chunker(),
            members: vec![1],
        }),
        root: Arc::new(CompactGuard {
            identity: PhysicalInodeIdentity {
                incarnation: 1,
                epoch: 1,
                revision: 0,
            },
            node: node(
                1,
                S_IFDIR | 0o755,
                2,
                NodeData::Directory { entries: vec![] },
            ),
        }),
        root_children: Arc::from([first, second, third]),
        root_single_link_file_bits: Arc::from([word]),
    };
    let observed = structure.root_single_link_file(query);
    let expected = (query == first && flags[0])
        || (query == second && flags[1])
        || (query == third && flags[2]);
    assert_eq!(observed, expected);

    kani::cover!(
        first == 2
            && second == 1_000_000
            && third == u64::MAX - 1
            && query == third
            && flags[2]
            && observed
    );
    kani::cover!(query == second && !flags[1] && !observed);
    kani::cover!(query > first && query < second && !observed);
    kani::cover!(query == 0 && !observed);
    kani::cover!(query == u64::MAX && !observed);
}

// Exactly one root with entries a/b/c, three regular single-link children and
// one unreferenced regular zero-link tombstone. Each file has a populated
// one-extent sparse layout. Arbitrary topology and provider I/O are excluded.
fn snapshot(generation: u64, revision: u64) -> CompactSnapshot {
    let identity = PhysicalInodeIdentity {
        incarnation: 1,
        epoch: generation,
        revision,
    };
    let root = node(
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
                DirectoryEntry {
                    name: "c".into(),
                    inode: 4,
                },
            ],
        },
    );
    let mut guards = BTreeMap::from([(
        1,
        CompactGuard {
            identity,
            node: root,
        },
    )]);
    for inode in 2..=5 {
        let mut file = node(
            inode,
            S_IFREG | 0o644,
            if inode == 5 { 0 } else { 1 },
            NodeData::File(FileLayout {
                chunker: fixed_chunker(),
                extents: vec![BlockExtent {
                    file_offset: 2,
                    block: BlockId("p".into()),
                    block_offset: 1,
                    length: 2,
                }],
            }),
        );
        file.stats.size = 4;
        file.stats.blocks = 1;
        guards.insert(
            inode,
            CompactGuard {
                identity,
                node: file,
            },
        );
    }
    CompactSnapshot {
        anchor: CompactAnchor {
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            generation,
            root: 1,
            next_inode: 6,
            default_uid: 10,
            default_gid: 20,
            umask: 0o022,
            default_chunker: fixed_chunker(),
            members: vec![1, 2, 3, 4, 5],
        },
        guards,
    }
}

fn verify(base: &CompactSnapshot, source: InodeId) -> VerifiedCompactRootFile {
    let (_, _, audited) = base.clone().into_validated_namespace().unwrap();
    let read = CompactRootFileRead::from_guards(
        base.anchor.clone(),
        1,
        source,
        Some(base.guards[&1].clone()),
        Some(base.guards[&source].clone()),
    )
    .unwrap();
    audited
        .verify_root_file(
            read,
            source,
            &base.guards[&source].node,
            base.guards[&source].identity,
        )
        .unwrap()
}

fn intent(rename: bool, source_position: usize) -> CompactRootFileIntent {
    let name = ["a", "b", "c"][source_position].to_owned();
    if rename {
        CompactRootFileIntent::RenameAbsent {
            from: name,
            to: "z".into(),
        }
    } else {
        CompactRootFileIntent::UnlinkLastLink { name }
    }
}

fn affected(base: &CompactSnapshot, source: InodeId) -> BTreeMap<InodeId, CompactGuard> {
    BTreeMap::from([
        (1, base.guards[&1].clone()),
        (source, base.guards[&source].clone()),
    ])
}

fn assert_fixed_chunker(config: &ChunkerConfig) {
    assert_eq!(config.algorithm.len(), 10);
    for index in 0..10 {
        assert_eq!(config.algorithm.as_bytes()[index], b"fixed-size"[index]);
    }
    assert_eq!(config.version, 1);
    assert_eq!(config.parameters.len(), 1);
    assert_eq!(config.parameters.get("chunk_size").copied(), Some(4096));
}

fn assert_identity(identity: PhysicalInodeIdentity, incarnation: u64, epoch: u64, revision: u64) {
    assert_eq!(identity.incarnation, incarnation);
    assert_eq!(identity.epoch, epoch);
    assert_eq!(identity.revision, revision);
}

fn assert_anchor_frame(old: &CompactAnchor, next: &CompactAnchor) {
    for index in 0..16 {
        assert_eq!(
            old.backing.as_bytes()[index],
            next.backing.as_bytes()[index]
        );
    }
    assert_eq!(next.root, old.root);
    assert_eq!(next.next_inode, old.next_inode);
    assert_eq!(next.default_uid, old.default_uid);
    assert_eq!(next.default_gid, old.default_gid);
    assert_eq!(next.umask, old.umask);
    assert_fixed_chunker(&next.default_chunker);
    assert_eq!(next.members.len(), old.members.len());
    for index in 0..5 {
        assert_eq!(next.members[index], old.members[index]);
    }
}

// Assert every Stats field other than the explicitly changed timestamps and
// link count separately, avoiding aggregate heap/map/NodeMetadata equality.
fn assert_stats_frame(old: &Stats, next: &Stats) {
    assert_eq!(next.dev, old.dev);
    assert_eq!(next.ino, old.ino);
    assert_eq!(next.mode, old.mode);
    assert_eq!(next.uid, old.uid);
    assert_eq!(next.gid, old.gid);
    assert_eq!(next.rdev, old.rdev);
    assert_eq!(next.size, old.size);
    assert_eq!(next.blksize, old.blksize);
    assert_eq!(next.blocks, old.blocks);
    assert_eq!(next.atime_ms, old.atime_ms);
    assert_eq!(next.birthtime_ms, old.birthtime_ms);
}

fn assert_populated_layout(file: &NodeMetadata) {
    let NodeData::File(layout) = &file.data else {
        panic!("file kind changed")
    };
    assert_fixed_chunker(&layout.chunker);
    assert_eq!(layout.extents.len(), 1);
    let extent = &layout.extents[0];
    assert_eq!(extent.file_offset, 2);
    assert_eq!(extent.block.0.len(), 1);
    assert_eq!(extent.block.0.as_bytes()[0], b'p');
    assert_eq!(extent.block_offset, 1);
    assert_eq!(extent.length, 2);
}

fn symbolize_conserved_stats(stats: &mut Stats) {
    stats.dev = kani::any();
    // Production kind/audit validation masks only S_IFMT. Preserve that type
    // while admitting every u32 non-type bit, including 0o7000 and high bits.
    stats.mode = (stats.mode & S_IFMT) | (kani::any::<u32>() & !S_IFMT);
    stats.uid = kani::any();
    stats.gid = kani::any();
    stats.rdev = kani::any();
    stats.blksize = kani::any();
    stats.blocks = kani::any();
    stats.atime_ms = kani::any();
    stats.birthtime_ms = kani::any();
}

fn symbolize_valid_identity(identity: &mut PhysicalInodeIdentity, generation: u64) {
    identity.incarnation = kani::any();
    identity.epoch = kani::any();
    identity.revision = kani::any();
    kani::assume(
        identity.incarnation > 0
            && identity.incarnation <= identity.epoch
            && identity.epoch <= generation,
    );
}

/// Both sealed scopes, every source entry position, complete source body,
/// nonempty content, arbitrary conserved non-type mode/other stats fields and
/// fitting monotonic times. Inode, node type and link count follow the fixture.
/// Exact two-key writes leave every unrelated guard out of the write set.
#[kani::proof]
#[kani::unwind(128)]
fn compact_root_file_transition_conserves_exact_write_frame() {
    let rename: bool = kani::any();
    let source_position: usize = kani::any();
    kani::assume(source_position < 3);
    let source = source_position as u64 + 2;
    let generation: u64 = kani::any();
    kani::assume(generation > 0 && generation < u64::MAX);
    let mut base = snapshot(generation, kani::any());
    symbolize_conserved_stats(&mut base.guards.get_mut(&1).unwrap().node.stats);
    symbolize_conserved_stats(&mut base.guards.get_mut(&source).unwrap().node.stats);
    symbolize_valid_identity(&mut base.guards.get_mut(&1).unwrap().identity, generation);
    symbolize_valid_identity(
        &mut base.guards.get_mut(&source).unwrap().identity,
        generation,
    );
    base.guards.get_mut(&1).unwrap().node.stats.size = kani::any();
    base.guards.get_mut(&source).unwrap().node.stats.size = kani::any();
    kani::assume(base.guards[&source].node.stats.size >= 4);
    base.guards.get_mut(&1).unwrap().node.stats.mtime_ms = kani::any();
    base.guards.get_mut(&1).unwrap().node.stats.ctime_ms = kani::any();
    base.guards.get_mut(&source).unwrap().node.stats.mtime_ms = kani::any();
    base.guards.get_mut(&source).unwrap().node.stats.ctime_ms = kani::any();
    let times = CompactRootFileTimes {
        parent_mtime_ms: kani::any(),
        parent_ctime_ms: kani::any(),
        file_ctime_ms: kani::any(),
    };
    let touches = if rename { 2_i128 } else { 1_i128 };
    // Mathematical preconditions admit every signed clock value for which a
    // valid proposal exists. The separate arithmetic proof retains overflow.
    kani::assume(
        i128::from(times.parent_mtime_ms)
            >= i128::from(base.guards[&1].node.stats.mtime_ms) + touches,
    );
    kani::assume(
        i128::from(times.parent_ctime_ms)
            >= i128::from(base.guards[&1].node.stats.ctime_ms) + touches,
    );
    kani::assume(
        i128::from(times.file_ctime_ms) >= i128::from(base.guards[&source].node.stats.ctime_ms) + 1,
    );

    let proposal = CompactRootFileTransition::capture(
        verify(&base, source),
        intent(rename, source_position),
        times,
    )
    .unwrap();
    let delta = proposal.delta();
    assert!(delta.validate_root_file_transition().is_ok());
    assert_eq!(delta.expected.len(), 2);
    assert!(delta.expected.contains_key(&1) && delta.expected.contains_key(&source));
    assert_eq!(delta.changed.len(), 2);
    assert!(delta.changed.contains_key(&1) && delta.changed.contains_key(&source));
    assert_eq!(delta.captured_bodies.len(), 2);
    assert_identity(
        delta.expected[&1],
        base.guards[&1].identity.incarnation,
        base.guards[&1].identity.epoch,
        base.guards[&1].identity.revision,
    );
    assert_identity(
        delta.expected[&source],
        base.guards[&source].identity.incarnation,
        base.guards[&source].identity.epoch,
        base.guards[&source].identity.revision,
    );
    assert!(delta.created.is_empty() && delta.removed.is_empty());
    assert_eq!(delta.entries.len(), if rename { 2 } else { 1 });
    assert_eq!(delta.entries[0].parent, 1);
    assert_eq!(delta.entries[0].expected, Some(source));
    assert_eq!(delta.entries[0].name.len(), 1);
    assert_eq!(
        delta.entries[0].name.as_bytes()[0],
        b'a' + source_position as u8
    );
    if rename {
        assert_eq!(delta.scope, StructuralScope::RootFileRenameAbsent);
        assert_eq!(delta.entries[1].parent, 1);
        assert!(delta.entries[1].expected.is_none());
        assert_eq!(delta.entries[1].name.len(), 1);
        assert_eq!(delta.entries[1].name.as_bytes()[0], b'z');
    } else {
        assert_eq!(delta.scope, StructuralScope::RootFileUnlinkLastLink);
    }

    let receipt = delta
        .validate_current(&base.anchor, &affected(&base, source))
        .unwrap();
    assert_eq!(receipt.anchor.generation, generation + 1);
    assert_anchor_frame(&base.anchor, &receipt.anchor);
    assert_eq!(receipt.upserts.len(), 2);
    assert!(receipt.removed.is_empty());
    let root = &receipt.upserts[&1];
    let file = &receipt.upserts[&source];
    assert_identity(
        root.identity,
        base.guards[&1].identity.incarnation,
        generation + 1,
        0,
    );
    assert_identity(
        file.identity,
        base.guards[&source].identity.incarnation,
        generation + 1,
        0,
    );
    assert_stats_frame(&base.guards[&1].node.stats, &root.node.stats);
    assert_eq!(root.node.stats.nlink, base.guards[&1].node.stats.nlink);
    assert_eq!(root.node.stats.mtime_ms, times.parent_mtime_ms);
    assert_eq!(root.node.stats.ctime_ms, times.parent_ctime_ms);
    assert_stats_frame(&base.guards[&source].node.stats, &file.node.stats);
    assert_eq!(file.node.stats.nlink, if rename { 1 } else { 0 });
    assert_eq!(
        file.node.stats.mtime_ms,
        base.guards[&source].node.stats.mtime_ms
    );
    assert_eq!(file.node.stats.ctime_ms, times.file_ctime_ms);
    assert_populated_layout(&file.node);
    let NodeData::Directory { entries } = &root.node.data else {
        panic!("root kind changed")
    };
    assert_eq!(entries.len(), if rename { 3 } else { 2 });
    for position in 0..2 {
        let original = if position < source_position {
            position
        } else {
            position + 1
        };
        assert_eq!(entries[position].inode, original as u64 + 2);
        assert_eq!(entries[position].name.len(), 1);
        assert_eq!(entries[position].name.as_bytes()[0], b'a' + original as u8);
    }
    if rename {
        assert_eq!(entries[2].inode, source);
        assert_eq!(entries[2].name.len(), 1);
        assert_eq!(entries[2].name.as_bytes()[0], b'z');
    }
    for inode in 2..=5 {
        if inode != source {
            assert!(!receipt.upserts.contains_key(&inode));
            assert!(!receipt.removed.contains(&inode));
            assert!(!delta.expected.contains_key(&inode));
        }
    }
    let successor = proposal.validate_publication(&receipt).unwrap();
    for inode in 2..=4 {
        assert_eq!(
            successor.root_single_link_file(inode),
            rename || inode != source
        );
    }
    assert!(!successor.root_single_link_file(5));
    assert!(proposal.structure.root_single_link_file(source));

    kani::cover!(rename && source_position == 0);
    kani::cover!(rename && source_position == 1);
    kani::cover!(rename && source_position == 2);
    kani::cover!(!rename && source_position == 0 && file.node.stats.nlink == 0);
    kani::cover!(!rename && source_position == 1);
    kani::cover!(!rename && source_position == 2);
    kani::cover!(generation == u64::MAX - 1 && receipt.anchor.generation == u64::MAX);
    kani::cover!(times.parent_mtime_ms < 0 && times.file_ctime_ms < 0);
    kani::cover!(
        base.guards[&source].identity.epoch < generation
            && base.guards[&source].identity.incarnation != base.guards[&1].identity.incarnation
    );
    kani::cover!(
        rename
            && file.node.stats.mode & 0o7000 == 0o7000
            && root.node.stats.mode & 0o7000 == 0o7000
    );
    kani::cover!(
        !rename
            && file.node.stats.mode & (1_u32 << 31) != 0
            && root.node.stats.mode & 0o7000 == 0o7000
    );
}

/// One symbolic tamper selector changes exactly one acknowledged field/key.
/// Both validators must accept only selector zero, before deriving topology.
/// This finite tamper set includes every receipt field category; it is not a
/// universal quantification over every arbitrary malformed receipt/graph.
#[kani::proof]
#[kani::unwind(128)]
fn compact_root_file_receipt_rejects_single_field_tampering() {
    let rename: bool = kani::any();
    let source_position: usize = kani::any();
    kani::assume(source_position < 3);
    let source = source_position as u64 + 2;
    let base = snapshot(3, kani::any());
    let proposal = CompactRootFileTransition::capture(
        verify(&base, source),
        intent(rename, source_position),
        CompactRootFileTimes {
            parent_mtime_ms: if rename { 2 } else { 1 },
            parent_ctime_ms: if rename { 2 } else { 1 },
            file_ctime_ms: 1,
        },
    )
    .unwrap();
    let mut receipt = proposal
        .delta()
        .validate_current(&base.anchor, &affected(&base, source))
        .unwrap();
    let tamper: u8 = kani::any();
    kani::assume(tamper <= 22);
    match tamper {
        0 => {}
        1 => receipt.anchor.generation ^= 1,
        2 => receipt.anchor.backing = ConcurrentBackingId::from_bytes([2; 16]).unwrap(),
        3 => receipt.anchor.root = 2,
        4 => receipt.anchor.next_inode += 1,
        5 => receipt.anchor.default_uid ^= 1,
        6 => receipt.anchor.default_gid ^= 1,
        7 => receipt.anchor.umask ^= 1,
        8 => receipt.anchor.default_chunker.version += 1,
        9 => receipt.anchor.members[4] += 1,
        10 => receipt.upserts.get_mut(&1).unwrap().node.stats.mode ^= 1,
        11 => receipt.upserts.get_mut(&source).unwrap().node.stats.size += 1,
        12 => {
            let NodeData::File(layout) = &mut receipt.upserts.get_mut(&source).unwrap().node.data
            else {
                panic!("fixture file kind")
            };
            layout.extents[0].block = BlockId("q".into());
        }
        13 => {
            receipt
                .upserts
                .get_mut(&source)
                .unwrap()
                .identity
                .incarnation ^= 1
        }
        14 => receipt.upserts.get_mut(&source).unwrap().identity.epoch ^= 1,
        15 => receipt.upserts.get_mut(&source).unwrap().identity.revision = 1,
        16 => {
            receipt.upserts.remove(&source);
        }
        17 => {
            receipt.upserts.insert(6, receipt.upserts[&source].clone());
        }
        18 => {
            receipt.removed.insert(source);
        }
        19 => {
            let NodeData::Directory { entries } =
                &mut receipt.upserts.get_mut(&1).unwrap().node.data
            else {
                panic!("fixture root kind")
            };
            entries[0].name = "x".into();
        }
        20 => receipt.upserts.get_mut(&source).unwrap().node.stats.nlink ^= 1,
        21 => receipt.upserts.get_mut(&1).unwrap().node.stats.mtime_ms ^= 1,
        22 => {
            receipt
                .upserts
                .get_mut(&source)
                .unwrap()
                .node
                .stats
                .ctime_ms ^= 1
        }
        _ => unreachable!(),
    }
    let checked = proposal.delta().validate_receipt(&receipt);
    let published = proposal.validate_publication(&receipt);
    let accepted = checked.is_ok();
    assert_eq!(accepted, tamper == 0);
    assert_eq!(published.is_ok(), tamper == 0);
    if tamper != 0 {
        assert_eq!(checked.unwrap_err().code, ErrorCode::Einval);
        assert_eq!(published.unwrap_err().code, ErrorCode::Einval);
    } else {
        let successor = published.unwrap();
        assert_eq!(successor.anchor().generation, 4);
        assert_eq!(successor.root_single_link_file(source), rename);
        for inode in 2..=4 {
            if inode != source {
                assert!(successor.root_single_link_file(inode));
            }
        }
        assert!(!successor.root_single_link_file(5));
    }

    kani::cover!(rename && tamper == 0 && accepted);
    kani::cover!(!rename && tamper == 0 && accepted);
    kani::cover!(tamper == 1 && !accepted);
    kani::cover!(tamper == 2 && !accepted);
    kani::cover!(tamper == 9 && !accepted);
    kani::cover!(tamper == 10 && !accepted);
    kani::cover!(tamper == 12 && !accepted);
    kani::cover!(tamper == 13 && !accepted);
    kani::cover!(tamper == 14 && !accepted);
    kani::cover!(tamper == 15 && !accepted);
    kani::cover!(tamper == 16 && !accepted);
    kani::cover!(tamper == 17 && !accepted);
    kani::cover!(tamper == 18 && !accepted);
    kani::cover!(tamper == 19 && !accepted);
    kani::cover!(!rename && tamper == 20 && !accepted);
    kani::cover!(tamper == 22 && !accepted);
}

/// Independent wide-integer arithmetic oracle: full signed clocks/proposals,
/// parent advances +1/+2, file advance +1, and full positive u64 generation.
/// Generation overflow is checked through the real capture constructor after
/// real full audit, read construction and source verification.
#[kani::proof]
#[kani::unwind(128)]
fn compact_root_file_checked_times_and_generation_do_not_wrap() {
    let rename: bool = kani::any();
    let touches = if rename { 2_i64 } else { 1_i64 };
    let mut root = node(
        1,
        S_IFDIR | 0o755,
        2,
        NodeData::Directory { entries: vec![] },
    );
    let mut file = node(
        2,
        S_IFREG | 0o644,
        1,
        NodeData::File(FileLayout {
            chunker: fixed_chunker(),
            extents: vec![],
        }),
    );
    root.stats.mtime_ms = kani::any();
    root.stats.ctime_ms = kani::any();
    file.stats.ctime_ms = kani::any();
    let times = CompactRootFileTimes {
        parent_mtime_ms: kani::any(),
        parent_ctime_ms: kani::any(),
        file_ctime_ms: kani::any(),
    };
    let minimum_mtime = i128::from(root.stats.mtime_ms) + i128::from(touches);
    let minimum_ctime = i128::from(root.stats.ctime_ms) + i128::from(touches);
    let minimum_file_ctime = i128::from(file.stats.ctime_ms) + 1;
    let overflows = minimum_mtime > i128::from(i64::MAX)
        || minimum_ctime > i128::from(i64::MAX)
        || minimum_file_ctime > i128::from(i64::MAX);
    let advances = i128::from(times.parent_mtime_ms) >= minimum_mtime
        && i128::from(times.parent_ctime_ms) >= minimum_ctime
        && i128::from(times.file_ctime_ms) >= minimum_file_ctime;
    let observed = validate_times(&root, &file, times, touches);
    let time_accepted = observed.is_ok();
    assert_eq!(time_accepted, !overflows && advances);
    if overflows {
        assert_eq!(observed.unwrap_err().code, ErrorCode::Eoverflow);
    } else if !advances {
        assert_eq!(observed.unwrap_err().code, ErrorCode::Einval);
    }

    let generation: u64 = kani::any();
    kani::assume(generation > 0);
    let base = snapshot(generation, 0);
    let captured = CompactRootFileTransition::capture(
        verify(&base, 2),
        intent(rename, 0),
        CompactRootFileTimes {
            parent_mtime_ms: touches,
            parent_ctime_ms: touches,
            file_ctime_ms: 1,
        },
    );
    let generation_accepted = captured.is_ok();
    let next_generation = u128::from(generation) + 1;
    assert_eq!(generation_accepted, next_generation <= u128::from(u64::MAX));
    match captured {
        Ok(proposal) => assert_eq!(u128::from(proposal.delta.next.generation), next_generation),
        Err(error) => assert_eq!(error.code, ErrorCode::Eoverflow),
    }

    kani::cover!(!rename && time_accepted);
    kani::cover!(rename && time_accepted);
    kani::cover!(time_accepted && times.parent_mtime_ms < 0 && times.file_ctime_ms < 0);
    kani::cover!(!overflows && !advances && !time_accepted);
    kani::cover!(!rename && root.stats.mtime_ms == i64::MAX && overflows);
    kani::cover!(rename && root.stats.mtime_ms == i64::MAX - 1 && overflows);
    kani::cover!(file.stats.ctime_ms == i64::MAX && overflows);
    kani::cover!(time_accepted && times.parent_mtime_ms == i64::MAX);
    kani::cover!(generation == u64::MAX - 1 && generation_accepted);
    kani::cover!(generation == u64::MAX && !generation_accepted);
}
