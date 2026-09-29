//! Scoped contracts for providers that persist membership and entries separately.

use super::*;

/// A provider can serve coherent selected rows without loading complete arrays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompactPointReadCapability {
    #[default]
    Unsupported,
    Supported,
}

/// Small persisted authority. This is not a complete membership witness.
/// A provider's distinct physical envelope must tag this representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactAuthority {
    pub backing: ConcurrentBackingId,
    pub generation: u64,
    pub root: InodeId,
    pub next_inode: InodeId,
    pub default_uid: u32,
    pub default_gid: u32,
    pub umask: u32,
    #[serde(deserialize_with = "deserialize_strict_chunker")]
    pub default_chunker: ChunkerConfig,
    pub members_count: u64,
}

impl CompactAuthority {
    pub fn validate(&self) -> Result<()> {
        ConcurrentBackingId::from_bytes(self.backing.as_bytes())?;
        validate_chunker_config(&self.default_chunker)?;
        if self.generation == 0
            || self.root == 0
            || self.root >= self.next_inode
            || self.members_count == 0
            || self.members_count >= self.next_inode
        {
            return Err(invalid_namespace("invalid compact authority bounds/count"));
        }
        Ok(())
    }

    pub fn from_anchor(anchor: &CompactAnchor) -> Result<Self> {
        anchor.validate()?;
        Ok(Self {
            backing: anchor.backing,
            generation: anchor.generation,
            root: anchor.root,
            next_inode: anchor.next_inode,
            default_uid: anchor.default_uid,
            default_gid: anchor.default_gid,
            umask: anchor.umask,
            default_chunker: anchor.default_chunker.clone(),
            members_count: anchor.members.len() as u64,
        })
    }

    /// Full audit reconstruction must supply the actual enumerated member IDs.
    /// Cached or selected IDs cannot replace that complete transaction read.
    pub fn into_anchor(self, actual_members: Vec<InodeId>) -> Result<CompactAnchor> {
        self.validate()?;
        if actual_members.len() as u64 != self.members_count {
            return Err(invalid_namespace(
                "compact member count differs from authority",
            ));
        }
        let anchor = CompactAnchor {
            backing: self.backing,
            generation: self.generation,
            root: self.root,
            next_inode: self.next_inode,
            default_uid: self.default_uid,
            default_gid: self.default_gid,
            umask: self.umask,
            default_chunker: self.default_chunker,
            members: actual_members,
        };
        anchor.validate()?;
        Ok(anchor)
    }

    /// Equality of every authority field only, not fresh membership equality.
    pub fn matches_anchor(&self, anchor: &CompactAnchor) -> bool {
        self.backing == anchor.backing
            && self.generation == anchor.generation
            && self.root == anchor.root
            && self.next_inode == anchor.next_inode
            && self.default_uid == anchor.default_uid
            && self.default_gid == anchor.default_gid
            && self.umask == anchor.umask
            && self.default_chunker == anchor.default_chunker
            && self.members_count == anchor.members.len() as u64
    }
}

/// Persisted directory attributes and ordinal allocator, without its entries.
/// A header alone never establishes a complete graph or root-body witness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactDirectoryHeader {
    pub stats: Stats,
    pub entry_count: u64,
    pub next_ordinal: u64,
}

impl CompactDirectoryHeader {
    /// Ordinals start at zero and surviving rows can leave holes. Reserve the
    /// maximum value so every stored allocator has a representable successor.
    pub fn validate(&self, inode: InodeId) -> Result<()> {
        if inode == 0
            || self.stats.ino != inode
            || self.stats.mode & S_IFMT != S_IFDIR
            || self.entry_count > self.next_ordinal
            || self.next_ordinal == u64::MAX
        {
            return Err(invalid_namespace("invalid compact directory header"));
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum AuthorityExpectation<'a> {
    Audited(&'a CompactAnchor),
    Explicit(&'a CompactAuthority),
}

/// Borrowed comparison input. Constructing this checks the complete regular
/// file but grants no freshness; only fresh row/body comparison can do that.
#[derive(Clone, Copy)]
pub struct CompactFileExpectation<'a> {
    authority: AuthorityExpectation<'a>,
    identity: PhysicalInodeIdentity,
    node: &'a NodeMetadata,
}

impl<'a> CompactFileExpectation<'a> {
    pub fn from_structure(
        structure: &'a ValidatedCompactStructure,
        identity: PhysicalInodeIdentity,
        node: &'a NodeMetadata,
    ) -> Result<Self> {
        let anchor = structure.anchor();
        if anchor.members.binary_search(&node.stats.ino).is_err() {
            return Err(invalid_namespace(
                "expected compact file absent from audited membership",
            ));
        }
        let expected = Self {
            authority: AuthorityExpectation::Audited(anchor),
            identity,
            node,
        };
        expected.validate_file()?;
        Ok(expected)
    }

    pub fn from_authority(
        authority: &'a CompactAuthority,
        identity: PhysicalInodeIdentity,
        node: &'a NodeMetadata,
    ) -> Result<Self> {
        authority.validate()?;
        let expected = Self {
            authority: AuthorityExpectation::Explicit(authority),
            identity,
            node,
        };
        expected.validate_file()?;
        Ok(expected)
    }

    fn validate_file(self) -> Result<()> {
        let authority = self.authority_values();
        validate_file(self.node.stats.ino, self.identity, self.node, authority)
    }

    fn authority_values(self) -> AuthorityValues {
        match self.authority {
            AuthorityExpectation::Audited(anchor) => AuthorityValues {
                backing: anchor.backing,
                generation: anchor.generation,
                root: anchor.root,
                next_inode: anchor.next_inode,
                default_uid: anchor.default_uid,
                default_gid: anchor.default_gid,
                umask: anchor.umask,
                chunk_size: anchor.default_chunker.parameters["chunk_size"],
                members_count: anchor.members.len() as u64,
            },
            AuthorityExpectation::Explicit(authority) => AuthorityValues::validated(authority),
        }
    }

    pub fn backing(self) -> ConcurrentBackingId {
        self.authority_values().backing
    }
    pub fn generation(self) -> u64 {
        self.authority_values().generation
    }
    pub fn identity(self) -> PhysicalInodeIdentity {
        self.identity
    }
    pub fn node(self) -> &'a NodeMetadata {
        self.node
    }
}

/// All fields of a validated supported authority, without an owned chunker.
/// Algorithm/version are fixed by validate_chunker_config; its sole parameter
/// is retained exactly. Extending supported chunkers must extend this witness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AuthorityValues {
    backing: ConcurrentBackingId,
    generation: u64,
    root: InodeId,
    next_inode: InodeId,
    default_uid: u32,
    default_gid: u32,
    umask: u32,
    chunk_size: u64,
    members_count: u64,
}

impl AuthorityValues {
    fn validated(authority: &CompactAuthority) -> Self {
        Self {
            backing: authority.backing,
            generation: authority.generation,
            root: authority.root,
            next_inode: authority.next_inode,
            default_uid: authority.default_uid,
            default_gid: authority.default_gid,
            umask: authority.umask,
            chunk_size: authority.default_chunker.parameters["chunk_size"],
            members_count: authority.members_count,
        }
    }
}

fn validate_member(
    inode: InodeId,
    member: Option<InodeId>,
    authority: AuthorityValues,
) -> Result<()> {
    if inode == 0 || inode >= authority.next_inode || member != Some(inode) {
        return Err(invalid_namespace(
            "selected inode absent from compact membership",
        ));
    }
    Ok(())
}

fn validate_file(
    inode: InodeId,
    identity: PhysicalInodeIdentity,
    node: &NodeMetadata,
    authority: AuthorityValues,
) -> Result<()> {
    identity.logical_version(authority.generation)?;
    if inode == 0
        || inode == authority.root
        || inode >= authority.next_inode
        || node.stats.ino != inode
    {
        return Err(invalid_namespace("invalid selected compact file inode"));
    }
    if !matches!(node.data, NodeData::File(_)) {
        return Err(invalid_namespace(
            "selected compact body is not a regular file",
        ));
    }
    validate_node_kind(node)
}

/// Scoped selected-file result. Its generation must be checked before the
/// deferred selected outcome. It never asserts unrelated membership or guards.
#[derive(Debug)]
pub struct CompactFileRead {
    authority: AuthorityValues,
    inode: InodeId,
    outcome: Result<CompactInodeRead>,
}

impl CompactFileRead {
    /// All inputs must originate in one provider statement snapshot, including
    /// the actual selected membership row. Group/decode errors belong in guard
    /// so callers can recover a changed generation before interpreting them.
    /// LoadedCompactInode is only the inner file result here; its validation
    /// provenance is this scoped receipt, not a fabricated complete anchor.
    pub fn from_guard(
        authority: CompactAuthority,
        backing: ConcurrentBackingId,
        inode: InodeId,
        selected_member: Option<InodeId>,
        guard: Result<Option<CompactGuard>>,
    ) -> Result<Self> {
        authority.validate()?;
        if authority.backing != backing {
            return Err(FsError::new(ErrorCode::Estale));
        }
        let authority = AuthorityValues::validated(&authority);
        let outcome = (|| {
            validate_member(inode, selected_member, authority)?;
            let guard =
                guard?.ok_or_else(|| invalid_namespace("missing selected compact file guard"))?;
            validate_file(inode, guard.identity, &guard.node, authority)?;
            Ok(CompactInodeRead::Loaded(LoadedCompactInode {
                generation: authority.generation,
                guard,
            }))
        })();
        Ok(Self {
            authority,
            inode,
            outcome,
        })
    }

    pub fn generation(&self) -> u64 {
        self.authority.generation
    }

    /// Compare all observed authority fields with the captured authority before
    /// admitting the file. A changed generation takes precedence over outcome.
    pub fn validate_expectation(&self, expected: CompactFileExpectation<'_>) -> Result<()> {
        let captured = expected.authority_values();
        if self.authority.backing != captured.backing {
            return Err(FsError::new(ErrorCode::Estale));
        }
        if self.authority.generation != captured.generation {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if self.authority != captured {
            return Err(invalid_namespace(
                "compact file authority differs from audited authority",
            ));
        }
        if self.inode != expected.node.stats.ino {
            return Err(invalid_namespace(
                "compact file receipt differs from expected inode",
            ));
        }
        Ok(())
    }

    /// Consume only after checking generation and validate_expectation. The
    /// caller must then apply its captured local-revision/physical-body fence.
    pub fn into_inode_read(self) -> Result<CompactInodeRead> {
        self.outcome
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BorrowedAuthority<'a> {
    backing: ConcurrentBackingId,
    generation: u64,
    root: InodeId,
    next_inode: InodeId,
    default_uid: u32,
    default_gid: u32,
    umask: u32,
    #[serde(borrow)]
    default_chunker: BorrowedChunker<'a>,
    members_count: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BorrowedChunker<'a> {
    #[serde(borrow)]
    algorithm: &'a str,
    version: u32,
    parameters: FixedParameters,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FixedParameters {
    chunk_size: u64,
}

fn deserialize_strict_chunker<'de, D: serde::Deserializer<'de>>(
    decoder: D,
) -> std::result::Result<ChunkerConfig, D::Error> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct StrictChunker {
        algorithm: String,
        version: u32,
        parameters: FixedParameters,
    }
    let chunker = StrictChunker::deserialize(decoder)?;
    Ok(ChunkerConfig {
        algorithm: chunker.algorithm,
        version: chunker.version,
        parameters: BTreeMap::from([("chunk_size".into(), chunker.parameters.chunk_size)]),
    })
}

impl BorrowedAuthority<'_> {
    fn values(self) -> Option<AuthorityValues> {
        if self.default_chunker.algorithm != "fixed-size" || self.default_chunker.version != 1 {
            return None;
        }
        Some(AuthorityValues {
            backing: self.backing,
            generation: self.generation,
            root: self.root,
            next_inode: self.next_inode,
            default_uid: self.default_uid,
            default_gid: self.default_gid,
            umask: self.umask,
            chunk_size: self.default_chunker.parameters.chunk_size,
            members_count: self.members_count,
        })
    }
}

/// Certify equality of the complete fresh file body against a borrowed
/// expectation. Authority bytes are the plain strict CompactAuthority JSON;
/// the provider must first check its physical envelope tag. These bytes, the
/// selected membership row, identity and body must share one statement view.
///
/// None requires the ordinary decoder on these SAME received bytes. Canonical
/// authority/file encodings use borrowed fields and allocate no owned metadata.
/// No constructor accepting just an identity can create an unchanged receipt.
pub fn check_compact_file_unchanged(
    authority_bytes: &[u8],
    backing: ConcurrentBackingId,
    inode: InodeId,
    selected_member: Option<InodeId>,
    identity: PhysicalInodeIdentity,
    node_bytes: &[u8],
    expected: CompactFileExpectation<'_>,
) -> Option<CompactFileRead> {
    let captured = expected.authority_values();
    if captured.backing != backing
        || expected.identity != identity
        || expected.node.stats.ino != inode
    {
        return None;
    }
    let observed: BorrowedAuthority<'_> = serde_json::from_slice(authority_bytes).ok()?;
    let observed = observed.values()?;
    if observed != captured {
        return None;
    }
    validate_member(inode, selected_member, observed).ok()?;
    // Expected files were validated at construction. Equality to every fresh
    // field preserves those bounds, including extents and physical identity.
    if !super::streamed::node_equal(node_bytes, expected.node) {
        return None;
    }
    Some(CompactFileRead {
        authority: observed,
        inode,
        outcome: Ok(CompactInodeRead::Unchanged(CheckedCompactInode {
            generation: observed.generation,
            inode,
            identity,
            root: None,
        })),
    })
}

/// One actual indexed dentry. Full names establish equality, never a hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactDirectoryEntry {
    pub parent: InodeId,
    pub ordinal: u64,
    pub name: String,
    pub inode: InodeId,
}

/// Coherent optional groups from one root-entry statement. Providers reject
/// partially NULL groups while decoding and defer that error in from_rows.
#[derive(Debug)]
pub struct CompactRootEntryRows {
    pub root_member: Option<InodeId>,
    pub root: Option<(PhysicalInodeIdentity, CompactDirectoryHeader)>,
    pub entry: Option<CompactDirectoryEntry>,
    pub file_member: Option<InodeId>,
    pub file: Option<CompactGuard>,
}

#[derive(Debug)]
struct SelectedRootEntry {
    root_identity: PhysicalInodeIdentity,
    root: CompactDirectoryHeader,
    entry: CompactDirectoryEntry,
    file: CompactGuard,
}

/// Selected header/link/file evidence only. This cannot become a
/// VerifiedCompactRoot and does not certify a complete fresh directory body.
#[derive(Debug)]
pub struct CompactRootEntryRead {
    authority: CompactAuthority,
    selected: Result<SelectedRootEntry>,
}

impl CompactRootEntryRead {
    pub fn from_rows(
        authority: CompactAuthority,
        backing: ConcurrentBackingId,
        expected_root: InodeId,
        candidate_file: InodeId,
        name: &str,
        rows: Result<CompactRootEntryRows>,
    ) -> Result<Self> {
        authority.validate()?;
        if authority.backing != backing {
            return Err(FsError::new(ErrorCode::Estale));
        }
        let selected = (|| {
            let observed = AuthorityValues::validated(&authority);
            validate_entry_name(name)?;
            if expected_root != authority.root || candidate_file == expected_root {
                return Err(invalid_namespace(
                    "invalid compact root-entry requested IDs",
                ));
            }
            let rows = rows?;
            validate_member(expected_root, rows.root_member, observed)?;
            let (root_identity, root) = rows
                .root
                .ok_or_else(|| invalid_namespace("missing compact directory header"))?;
            root_identity.logical_version(authority.generation)?;
            root.validate(expected_root)?;
            let entry = rows
                .entry
                .ok_or_else(|| invalid_namespace("missing compact selected directory entry"))?;
            validate_entry_name(&entry.name)?;
            if entry.parent != expected_root
                || entry.name != name
                || entry.inode != candidate_file
                || root.entry_count == 0
                || entry.ordinal >= root.next_ordinal
            {
                return Err(invalid_namespace(
                    "compact selected directory entry mismatch",
                ));
            }
            validate_member(candidate_file, rows.file_member, observed)?;
            let file = rows
                .file
                .ok_or_else(|| invalid_namespace("missing compact root-entry file guard"))?;
            validate_file(candidate_file, file.identity, &file.node, observed)?;
            Ok(SelectedRootEntry {
                root_identity,
                root,
                entry,
                file,
            })
        })();
        Ok(Self {
            authority,
            selected,
        })
    }

    pub fn generation(&self) -> u64 {
        self.authority.generation
    }

    /// Compare the actual authority, root attributes/identity and exact link
    /// against audited topology. No other root entries are asserted fresh.
    pub fn into_inode_read(
        self,
        structure: &ValidatedCompactStructure,
    ) -> Result<CompactInodeRead> {
        let anchor = structure.anchor();
        if self.authority.backing != anchor.backing {
            return Err(FsError::new(ErrorCode::Estale));
        }
        if self.authority.generation != anchor.generation {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if !self.authority.matches_anchor(anchor) {
            return Err(invalid_namespace(
                "compact root-entry authority differs from audited graph",
            ));
        }
        let selected = self.selected?;
        let NodeData::Directory { entries } = &structure.root.node.data else {
            return Err(invalid_namespace("audited compact root is not a directory"));
        };
        if selected.root_identity != structure.root.identity
            || selected.root.stats != structure.root.node.stats
            || selected.root.entry_count != entries.len() as u64
            || !entries.iter().any(|entry| {
                entry.name == selected.entry.name && entry.inode == selected.entry.inode
            })
        {
            return Err(invalid_namespace(
                "compact root header/link differs from audited graph",
            ));
        }
        Ok(CompactInodeRead::Loaded(LoadedCompactInode {
            generation: self.authority.generation,
            guard: selected.file,
        }))
    }
}

/// Validate a selected-file update against fresh authority and membership read
/// after obtaining the file guard lock. It retains the existing physical CAS,
/// epoch projection, publication restrictions and revision overflow semantics.
/// Providers persist only the returned guard and retain unknown-commit rules.
#[allow(clippy::too_many_arguments)]
pub fn validate_compact_file_update(
    authority: &CompactAuthority,
    backing: ConcurrentBackingId,
    generation: u64,
    inode: InodeId,
    selected_member: Option<InodeId>,
    current: &CompactGuard,
    expected: PhysicalInodeIdentity,
    node: NodeMetadata,
) -> Result<CompactGuard> {
    authority.validate()?;
    let observed = AuthorityValues::validated(authority);
    validate_file(inode, current.identity, &current.node, observed)?;
    if authority.backing != backing {
        return Err(FsError::new(ErrorCode::Estale));
    }
    if authority.generation != generation || current.identity != expected {
        return Err(FsError::new(ErrorCode::Eagain));
    }
    validate_member(inode, selected_member, observed)?;
    validate_inode_publication(inode, &current.node, &node)?;
    let revision = current
        .identity
        .logical_version(authority.generation)?
        .inode_revision
        .checked_add(1)
        .ok_or_else(|| overflow_namespace("compact inode revision exhausted"))?;
    Ok(CompactGuard {
        identity: PhysicalInodeIdentity {
            incarnation: current.identity.incarnation,
            epoch: authority.generation,
            revision,
        },
        node,
    })
}

#[cfg(all(test, not(kani)))]
mod tests {
    use super::*;

    fn fixture() -> CompactSnapshot {
        let chunker = ChunkerConfig {
            algorithm: "fixed-size".into(),
            version: 1,
            parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
        };
        let stats = |ino, mode, nlink| Stats {
            dev: 0,
            ino,
            mode,
            nlink,
            uid: 1000,
            gid: 1001,
            rdev: 0,
            size: 4,
            blksize: 4096,
            blocks: 1,
            atime_ms: 1,
            mtime_ms: 2,
            ctime_ms: 3,
            birthtime_ms: 0,
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
                next_inode: 3,
                default_uid: 1000,
                default_gid: 1001,
                umask: 0o022,
                default_chunker: chunker.clone(),
                members: vec![1, 2],
            },
            guards: BTreeMap::from([
                (
                    1,
                    CompactGuard {
                        identity,
                        node: NodeMetadata {
                            stats: stats(1, S_IFDIR | 0o755, 2),
                            data: NodeData::Directory {
                                entries: vec![DirectoryEntry {
                                    name: "a".into(),
                                    inode: 2,
                                }],
                            },
                        },
                    },
                ),
                (
                    2,
                    CompactGuard {
                        identity,
                        node: NodeMetadata {
                            stats: stats(2, S_IFREG | 0o644, 1),
                            data: NodeData::File(FileLayout {
                                chunker,
                                extents: vec![BlockExtent {
                                    file_offset: 0,
                                    block: BlockId("block-a".into()),
                                    block_offset: 0,
                                    length: 4,
                                }],
                            }),
                        },
                    },
                ),
            ]),
        }
    }

    fn authority(base: &CompactSnapshot) -> CompactAuthority {
        CompactAuthority::from_anchor(&base.anchor).unwrap()
    }

    fn root_rows(base: &CompactSnapshot) -> CompactRootEntryRows {
        CompactRootEntryRows {
            root_member: Some(1),
            root: Some((
                base.guards[&1].identity,
                CompactDirectoryHeader {
                    stats: base.guards[&1].node.stats.clone(),
                    entry_count: 1,
                    next_ordinal: 3,
                },
            )),
            entry: Some(CompactDirectoryEntry {
                parent: 1,
                ordinal: 2,
                name: "a".into(),
                inode: 2,
            }),
            file_member: Some(2),
            file: Some(base.guards[&2].clone()),
        }
    }

    fn point(
        base: &CompactSnapshot,
        member: Option<InodeId>,
        guard: Result<Option<CompactGuard>>,
    ) -> Result<CompactFileRead> {
        CompactFileRead::from_guard(authority(base), base.anchor.backing, 2, member, guard)
    }

    #[test]
    fn authority_reconstruction_requires_actual_complete_sorted_members() {
        let base = fixture();
        let small = authority(&base);
        assert_eq!(small.members_count, 2);
        assert_eq!(small.clone().into_anchor(vec![1, 2]).unwrap(), base.anchor);
        for members in [
            vec![],
            vec![1],
            vec![1, 1],
            vec![2, 1],
            vec![0, 2],
            vec![2, 3],
            vec![1, 3],
        ] {
            assert!(small.clone().into_anchor(members).is_err());
        }
        assert!(small.matches_anchor(&base.anchor));
        let mut changed = base.anchor.clone();
        changed.default_uid += 1;
        assert!(!small.matches_anchor(&changed));
        changed = base.anchor.clone();
        changed
            .default_chunker
            .parameters
            .insert("chunk_size".into(), 8192);
        assert!(!small.matches_anchor(&changed));
    }

    #[test]
    fn authority_rejects_invalid_bounds_and_unknown_fields() {
        let base = fixture();
        for change in 0..7 {
            let mut small = authority(&base);
            match change {
                0 => small.generation = 0,
                1 => small.root = 0,
                2 => small.next_inode = small.root,
                3 => small.members_count = 0,
                4 => small.members_count = small.next_inode,
                5 => small
                    .default_chunker
                    .parameters
                    .insert("chunk_size".into(), 0)
                    .map(|_| ())
                    .unwrap(),
                _ => {
                    small.backing =
                        serde_json::from_str("[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]").unwrap()
                }
            }
            assert!(small.validate().is_err(), "case {change}");
        }
        let mut value = serde_json::to_value(authority(&base)).unwrap();
        value["members"] = serde_json::json!([1, 2]);
        assert!(serde_json::from_value::<CompactAuthority>(value).is_err());
    }

    #[test]
    fn authority_rejects_unknown_nested_chunker_fields() {
        let base = fixture();
        let small = authority(&base);
        let file = &base.guards[&2];
        let expected =
            CompactFileExpectation::from_authority(&small, file.identity, &file.node).unwrap();
        let mut value = serde_json::to_value(&small).unwrap();
        value["default_chunker"]["unexpected"] = serde_json::json!(true);
        assert!(
            check_compact_file_unchanged(
                &serde_json::to_vec(&value).unwrap(),
                small.backing,
                2,
                Some(2),
                file.identity,
                &serde_json::to_vec(&file.node).unwrap(),
                expected,
            )
            .is_none()
        );
        assert!(serde_json::from_value::<CompactAuthority>(value).is_err());
    }

    #[test]
    fn authority_rejects_duplicate_nested_chunker_parameters() {
        let base = fixture();
        let small = authority(&base);
        let file = &base.guards[&2];
        let expected =
            CompactFileExpectation::from_authority(&small, file.identity, &file.node).unwrap();
        let encoded = serde_json::to_string(&small).unwrap();
        let duplicate = encoded.replace(
            "\"chunk_size\":4096",
            "\"chunk_size\":0,\"chunk_size\":4096",
        );
        assert_ne!(encoded, duplicate);
        assert!(
            check_compact_file_unchanged(
                duplicate.as_bytes(),
                small.backing,
                2,
                Some(2),
                file.identity,
                &serde_json::to_vec(&file.node).unwrap(),
                expected,
            )
            .is_none()
        );
        assert!(serde_json::from_str::<CompactAuthority>(&duplicate).is_err());
    }

    #[test]
    fn file_authority_errors_precede_deferred_selected_errors() {
        let base = fixture();
        let other = ConcurrentBackingId::from_bytes([2; 16]).unwrap();
        let error = CompactFileRead::from_guard(
            authority(&base),
            other,
            2,
            None,
            Err(FsError::new(ErrorCode::Eio)),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Estale);
        let mut invalid = authority(&base);
        invalid.generation = 0;
        assert!(
            CompactFileRead::from_guard(
                invalid,
                base.anchor.backing,
                2,
                None,
                Err(FsError::new(ErrorCode::Eio))
            )
            .is_err()
        );
    }

    #[test]
    fn file_changed_generation_is_visible_before_missing_or_malformed_rows() {
        let mut base = fixture();
        base.anchor.generation += 1;
        for (member, guard) in [
            (None, Ok(None)),
            (Some(99), Ok(Some(base.guards[&2].clone()))),
            (Some(2), Err(FsError::new(ErrorCode::Eio))),
        ] {
            let read = point(&base, member, guard).unwrap();
            assert_eq!(read.generation(), 4);
            assert!(read.into_inode_read().is_err());
        }
    }

    #[test]
    fn scoped_file_validates_kind_extents_inode_and_physical_bounds() {
        let base = fixture();
        assert!(matches!(
            point(&base, Some(2), Ok(Some(base.guards[&2].clone())))
                .unwrap()
                .into_inode_read()
                .unwrap(),
            CompactInodeRead::Loaded(_)
        ));
        for change in 0..7 {
            let mut file = base.guards[&2].clone();
            match change {
                0 => file.identity.incarnation = 0,
                1 => file.identity.incarnation = 4,
                2 => file.identity.epoch = 4,
                3 => file.node.stats.ino = 3,
                4 => file.node.stats.mode = S_IFDIR,
                5 => file.node = base.guards[&1].node.clone(),
                _ => {
                    if let NodeData::File(layout) = &mut file.node.data {
                        layout.extents[0].length = 5;
                    }
                }
            }
            let read = point(&base, Some(2), Ok(Some(file))).unwrap();
            assert!(read.into_inode_read().is_err(), "case {change}");
        }
    }

    #[test]
    fn file_expectation_borrows_audited_inputs_and_rejects_directory() {
        let base = fixture();
        let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
        let file = &base.guards[&2];
        let expected =
            CompactFileExpectation::from_structure(&structure, file.identity, &file.node).unwrap();
        assert!(std::ptr::eq(expected.node(), &file.node));
        assert_eq!(expected.identity(), file.identity);
        assert_eq!(expected.generation(), 3);
        assert_eq!(expected.backing(), base.anchor.backing);
        assert!(
            CompactFileExpectation::from_structure(
                &structure,
                base.guards[&1].identity,
                &base.guards[&1].node
            )
            .is_err()
        );
    }

    #[test]
    fn loaded_file_receipt_retains_all_authority_fields_for_admission() {
        let base = fixture();
        let small = authority(&base);
        let file = &base.guards[&2];
        let expected =
            CompactFileExpectation::from_authority(&small, file.identity, &file.node).unwrap();
        point(&base, Some(2), Ok(Some(file.clone())))
            .unwrap()
            .validate_expectation(expected)
            .unwrap();
        let mut changed = small.clone();
        changed.default_gid += 1;
        let read = CompactFileRead::from_guard(
            changed,
            base.anchor.backing,
            2,
            Some(2),
            Ok(Some(file.clone())),
        )
        .unwrap();
        assert!(read.validate_expectation(expected).is_err());
    }

    #[test]
    fn loaded_file_receipt_rejects_different_expected_inode() {
        let mut base = fixture();
        base.anchor.members.push(3);
        base.anchor.next_inode = 4;
        let small = authority(&base);
        let file = &base.guards[&2];
        let expected =
            CompactFileExpectation::from_authority(&small, file.identity, &file.node).unwrap();
        let mut other = file.clone();
        other.node.stats.ino = 3;
        let read =
            CompactFileRead::from_guard(small.clone(), small.backing, 3, Some(3), Ok(Some(other)))
                .unwrap();
        assert!(read.validate_expectation(expected).is_err());
    }

    #[test]
    fn unchanged_file_requires_complete_fresh_authority_and_body_equality() {
        let base = fixture();
        let small = authority(&base);
        let file = &base.guards[&2];
        let expected =
            CompactFileExpectation::from_authority(&small, file.identity, &file.node).unwrap();
        let a = serde_json::to_vec(&small).unwrap();
        let body = serde_json::to_vec(&file.node).unwrap();
        let check = |a: &[u8], member, identity, body: &[u8]| {
            check_compact_file_unchanged(
                a,
                base.anchor.backing,
                2,
                member,
                identity,
                body,
                expected,
            )
        };
        let read = check(&a, Some(2), file.identity, &body).unwrap();
        assert_eq!(read.generation(), 3);
        let CompactInodeRead::Unchanged(checked) = read.into_inode_read().unwrap() else {
            panic!()
        };
        assert!(checked.into_verified_root().is_none());
        assert!(check(&a, None, file.identity, &body).is_none());
        assert!(check(&a, Some(3), file.identity, &body).is_none());
        for field in [
            "generation",
            "root",
            "next_inode",
            "default_uid",
            "default_gid",
            "umask",
            "members_count",
        ] {
            let mut changed = serde_json::to_value(&small).unwrap();
            changed[field] = serde_json::json!(changed[field].as_u64().unwrap() + 1);
            assert!(
                check(
                    &serde_json::to_vec(&changed).unwrap(),
                    Some(2),
                    file.identity,
                    &body
                )
                .is_none(),
                "{field}"
            );
        }
        for change in 0..4 {
            let mut changed = file.node.clone();
            match change {
                0 => changed.stats.mtime_ms += 1,
                1 => changed.data = NodeData::Special,
                2 => {
                    if let NodeData::File(layout) = &mut changed.data {
                        layout.extents[0].block.0 = "block-b".into();
                    }
                }
                _ => {
                    if let NodeData::File(layout) = &mut changed.data {
                        layout.extents[0].length = 5;
                    }
                }
            }
            assert!(
                check(
                    &a,
                    Some(2),
                    file.identity,
                    &serde_json::to_vec(&changed).unwrap()
                )
                .is_none()
            );
        }
        let mut advanced = file.identity;
        advanced.revision += 1;
        assert!(check(&a, Some(2), advanced, &body).is_none());
        assert!(check(&a, Some(2), file.identity, b"{}").is_none());
        assert!(check(b"{}", Some(2), file.identity, &body).is_none());
    }

    #[test]
    fn canonical_file_certification_allocates_no_metadata() {
        let base = fixture();
        let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
        let authority_bytes = serde_json::to_vec(&authority(&base)).unwrap();
        let file = &base.guards[&2];
        let body = serde_json::to_vec(&file.node).unwrap();
        let ((), allocations, bytes) = super::super::streamed_tests::measured(|| {
            for _ in 0..32 {
                let expected =
                    CompactFileExpectation::from_structure(&structure, file.identity, &file.node)
                        .unwrap();
                let read = check_compact_file_unchanged(
                    &authority_bytes,
                    base.anchor.backing,
                    2,
                    Some(2),
                    file.identity,
                    &body,
                    expected,
                )
                .unwrap();
                assert_eq!(read.generation(), 3);
                read.validate_expectation(expected).unwrap();
                assert!(matches!(
                    read.into_inode_read().unwrap(),
                    CompactInodeRead::Unchanged(_)
                ));
            }
        });
        assert_eq!((allocations, bytes), (0, 0));
        let (_, allocations, bytes) = super::super::streamed_tests::measured(|| {
            let authority: CompactAuthority = serde_json::from_slice(&authority_bytes).unwrap();
            let node: NodeMetadata = serde_json::from_slice(&body).unwrap();
            (authority, node)
        });
        assert!(
            allocations > 0 && bytes > 0,
            "owned decoder is the allocator control"
        );
    }

    #[test]
    fn unchanged_rejects_incomplete_extra_or_different_authority_and_extents() {
        let base = fixture();
        let small = authority(&base);
        let file = &base.guards[&2];
        let expected =
            CompactFileExpectation::from_authority(&small, file.identity, &file.node).unwrap();
        let body = serde_json::to_vec(&file.node).unwrap();
        let check = |authority: &[u8], body: &[u8]| {
            check_compact_file_unchanged(
                authority,
                base.anchor.backing,
                2,
                Some(2),
                file.identity,
                body,
                expected,
            )
        };
        let authority_json = serde_json::to_value(&small).unwrap();
        for field in authority_json.as_object().unwrap().keys() {
            let mut missing = authority_json.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(
                check(&serde_json::to_vec(&missing).unwrap(), &body).is_none(),
                "missing {field}"
            );
        }
        for change in 0..7 {
            let mut different = authority_json.clone();
            match change {
                0 => different["unexpected"] = serde_json::json!(true),
                1 => different["backing"] = serde_json::to_value([2_u8; 16]).unwrap(),
                2 => different["default_chunker"]["algorithm"] = serde_json::json!("other"),
                3 => different["default_chunker"]["version"] = serde_json::json!(2),
                4 => {
                    different["default_chunker"]["parameters"]["chunk_size"] =
                        serde_json::json!(8192)
                }
                5 => different["default_chunker"]["parameters"]["unknown"] = serde_json::json!(1),
                _ => {
                    different["default_chunker"]["parameters"]["chunk_size"] = serde_json::json!(0)
                }
            }
            assert!(
                check(&serde_json::to_vec(&different).unwrap(), &body).is_none(),
                "authority case {change}"
            );
        }
        let authority_bytes = serde_json::to_vec(&small).unwrap();
        let mut duplicate = String::from_utf8(authority_bytes.clone()).unwrap();
        duplicate.insert_str(1, "\"generation\":3,");
        assert!(check(duplicate.as_bytes(), &body).is_none());
        let mut trailing = authority_bytes.clone();
        trailing.extend_from_slice(b" {}");
        assert!(check(&trailing, &body).is_none());
        for change in 0..5 {
            let mut different = file.node.clone();
            let NodeData::File(layout) = &mut different.data else {
                panic!()
            };
            match change {
                0 => layout.extents[0].file_offset = 1,
                1 => layout.extents[0].block_offset = 1,
                2 => layout.extents[0].length = 3,
                3 => layout.extents.clear(),
                _ => layout.extents.push(layout.extents[0].clone()),
            }
            assert!(
                check(&authority_bytes, &serde_json::to_vec(&different).unwrap()).is_none(),
                "extent case {change}"
            );
        }
    }

    #[test]
    fn directory_header_validates_kind_inode_count_and_allocator() {
        let base = fixture();
        let (_, header) = root_rows(&base).root.unwrap();
        header.validate(1).unwrap();
        for change in 0..5 {
            let mut invalid = header.clone();
            match change {
                0 => invalid.stats.ino = 0,
                1 => invalid.stats.ino = 2,
                2 => invalid.stats.mode = S_IFREG,
                3 => invalid.entry_count = 4,
                _ => invalid.next_ordinal = u64::MAX,
            }
            assert!(invalid.validate(1).is_err(), "case {change}");
        }
        let mut value = serde_json::to_value(header).unwrap();
        value["entries"] = serde_json::json!([]);
        assert!(serde_json::from_value::<CompactDirectoryHeader>(value).is_err());
    }

    #[test]
    fn root_entry_checks_exact_name_ids_header_and_cached_link() {
        let base = fixture();
        let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
        let read = CompactRootEntryRead::from_rows(
            authority(&base),
            base.anchor.backing,
            1,
            2,
            "a",
            Ok(root_rows(&base)),
        )
        .unwrap();
        assert_eq!(read.generation(), 3);
        assert!(matches!(
            read.into_inode_read(&structure).unwrap(),
            CompactInodeRead::Loaded(_)
        ));
        for change in 0..10 {
            let mut rows = root_rows(&base);
            match change {
                0 => rows.root_member = None,
                1 => rows.file_member = Some(3),
                2 => rows.entry.as_mut().unwrap().name = "b".into(),
                3 => rows.entry.as_mut().unwrap().inode = 3,
                4 => rows.entry.as_mut().unwrap().parent = 2,
                5 => rows.entry.as_mut().unwrap().ordinal = 3,
                6 => rows.root.as_mut().unwrap().1.stats.mtime_ms += 1,
                7 => rows.root.as_mut().unwrap().1.entry_count = 2,
                8 => rows.root.as_mut().unwrap().0.revision += 1,
                _ => rows.file = None,
            }
            let read = CompactRootEntryRead::from_rows(
                authority(&base),
                base.anchor.backing,
                1,
                2,
                "a",
                Ok(rows),
            )
            .unwrap();
            assert!(read.into_inode_read(&structure).is_err(), "case {change}");
        }
    }

    #[test]
    fn root_entry_preserves_generation_before_missing_groups_and_decode_errors() {
        let base = fixture();
        let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
        let mut small = authority(&base);
        small.generation += 1;
        let mut missing = root_rows(&base);
        missing.file = None;
        for rows in [Ok(missing), Err(FsError::new(ErrorCode::Eio))] {
            let read = CompactRootEntryRead::from_rows(
                small.clone(),
                base.anchor.backing,
                1,
                2,
                "a",
                rows,
            )
            .unwrap();
            assert_eq!(read.generation(), 4);
            assert_eq!(
                read.into_inode_read(&structure).unwrap_err().code,
                ErrorCode::Eagain
            );
        }
    }

    #[test]
    fn root_entry_has_no_arbitrary_filename_length_limit() {
        let mut base = fixture();
        let name = "long".repeat(4096);
        if let NodeData::Directory { entries } = &mut base.guards.get_mut(&1).unwrap().node.data {
            entries[0].name = name.clone();
        }
        let (_, _, structure) = base.clone().into_validated_namespace().unwrap();
        let mut rows = root_rows(&base);
        rows.entry.as_mut().unwrap().name = name.clone();
        let read = CompactRootEntryRead::from_rows(
            authority(&base),
            base.anchor.backing,
            1,
            2,
            &name,
            Ok(rows),
        )
        .unwrap();
        read.into_inode_read(&structure).unwrap();
    }

    #[test]
    fn indexed_file_update_matches_existing_revision_and_conflict_semantics() {
        let base = fixture();
        let small = authority(&base);
        let current = &base.guards[&2];
        let mut node = current.node.clone();
        node.stats.size += 1;
        let expected = validate_selected_update(
            &base.anchor,
            base.anchor.backing,
            3,
            2,
            current,
            current.identity,
            node.clone(),
        )
        .unwrap();
        assert_eq!(
            validate_compact_file_update(
                &small,
                small.backing,
                3,
                2,
                Some(2),
                current,
                current.identity,
                node.clone()
            )
            .unwrap(),
            expected
        );
        assert_eq!(
            validate_compact_file_update(
                &small,
                small.backing,
                2,
                2,
                Some(2),
                current,
                current.identity,
                node.clone()
            )
            .unwrap_err()
            .code,
            ErrorCode::Eagain
        );
        assert!(
            validate_compact_file_update(
                &small,
                small.backing,
                3,
                2,
                None,
                current,
                current.identity,
                node.clone()
            )
            .is_err()
        );
        let mut stale_epoch = current.clone();
        stale_epoch.identity.epoch = 2;
        let next = validate_compact_file_update(
            &small,
            small.backing,
            3,
            2,
            Some(2),
            &stale_epoch,
            stale_epoch.identity,
            node.clone(),
        )
        .unwrap();
        assert_eq!(next.identity.revision, 1);
        let mut exhausted = current.clone();
        exhausted.identity.revision = u64::MAX;
        assert_eq!(
            validate_compact_file_update(
                &small,
                small.backing,
                3,
                2,
                Some(2),
                &exhausted,
                exhausted.identity,
                node
            )
            .unwrap_err()
            .code,
            ErrorCode::Eoverflow
        );
    }
}
