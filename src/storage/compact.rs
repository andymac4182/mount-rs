//! Shared validators and a pure transaction reference model for an optional
//! compact inode layout. MRC4 encoding and publication contracts are unchanged.
//!
//! Providers must negotiate support before I/O and explicitly initialize only
//! fresh, owned volumes under an atomic mode/lease fence. These types do not
//! enroll, migrate or publish a volume. A provider must persist a
//! distinct layout tag; it must never infer this layout from MRC4 records.
//!
//! All inputs to transaction validators must come from ONE transaction view:
//! anchor, exact membership and guarded records. Selected writes do not advance
//! the anchor generation, so generation-before/after checks cannot establish
//! snapshot coherence. Validation proves well-formedness, not read provenance.
//!
//! EAGAIN means a proven conflict before any commit. An unknown commit outcome
//! must retain the original publication identity/resources: do not replay or
//! switch to MRC4. Provider/core fault tests must enforce that I/O rule.

use super::*;
use crate::diagnostics::profile::{self, Event, Span};

/// Capability advertisement only; it does not assert a volume is enrolled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompactInodeCapability {
    #[default]
    Unsupported,
    V1,
}

/// Exact sorted membership and namespace defaults. IDs remain O(N) to read,
/// copy and rewrite; changed directory entry arrays also retain their cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactAnchor {
    pub backing: ConcurrentBackingId,
    pub generation: u64,
    pub root: InodeId,
    pub next_inode: InodeId,
    pub default_uid: u32,
    pub default_gid: u32,
    pub umask: u32,
    pub default_chunker: ChunkerConfig,
    pub members: Vec<InodeId>,
}

/// An unconditional selected read carries the body and its physical identity
/// together with the anchor generation observed in the same transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedCompactInode {
    pub generation: u64,
    pub guard: CompactGuard,
}

impl LoadedCompactInode {
    /// Validate membership, body kind and physical identity without fetching
    /// unrelated guards. The anchor and guard must share a transaction view.
    pub fn from_guard(anchor: &CompactAnchor, inode: InodeId, guard: CompactGuard) -> Result<Self> {
        anchor.validate()?;
        if anchor.members.binary_search(&inode).is_err() {
            return Err(invalid_namespace(
                "selected guard absent from compact membership",
            ));
        }
        guard.validate(inode, anchor)?;
        Ok(Self {
            generation: anchor.generation,
            guard,
        })
    }
}

/// Encode only the distinct compact v1 anchor representation.
pub fn encode_compact_anchor(anchor: &CompactAnchor) -> Result<Vec<u8>> {
    anchor.validate()?;
    serde_json::to_vec(&CompactAnchorEnvelope {
        layout: "mount-rs-compact-inodes".into(),
        version: 1,
        anchor: anchor.clone(),
    })
    .map_err(crate::backend_error)
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactAnchorEnvelope {
    layout: String,
    version: u32,
    anchor: CompactAnchor,
}

/// Reject legacy namespaces, MRC4 envelopes, malformed and future formats.
pub fn decode_compact_anchor(bytes: &[u8]) -> Result<CompactAnchor> {
    let envelope: CompactAnchorEnvelope =
        serde_json::from_slice(bytes).map_err(crate::backend_error)?;
    if envelope.layout != "mount-rs-compact-inodes" || envelope.version != 1 {
        return Err(invalid_namespace("unsupported compact anchor envelope"));
    }
    envelope.anchor.validate()?;
    Ok(envelope.anchor)
}

impl CompactAnchor {
    pub fn validate(&self) -> Result<()> {
        ConcurrentBackingId::from_bytes(self.backing.as_bytes())?;
        from_config(&self.default_chunker)?;
        if self.generation == 0
            || self.root == 0
            || self.members.is_empty()
            || self.members[0] == 0
            || self.members.windows(2).any(|pair| pair[0] >= pair[1])
            || self.members.binary_search(&self.root).is_err()
        {
            return Err(invalid_namespace(
                "invalid compact anchor identity/membership",
            ));
        }
        let max_inode = *self.members.last().expect("nonempty membership checked");
        if max_inode == u64::MAX {
            return Err(overflow_namespace("compact inode allocation exhausted"));
        }
        if self.next_inode <= max_inode {
            return Err(invalid_namespace(
                "compact next_inode must exceed membership",
            ));
        }
        Ok(())
    }

    fn with_namespace(&self, namespace: &Namespace, generation: u64) -> Self {
        Self {
            backing: self.backing,
            generation,
            root: namespace.root,
            next_inode: namespace.next_inode,
            default_uid: namespace.default_uid,
            default_gid: namespace.default_gid,
            umask: namespace.umask,
            default_chunker: namespace.default_chunker.clone(),
            members: namespace.nodes.keys().copied().collect(),
        }
    }
}

/// Exact persisted identity, separate from the projected logical token.
/// `incarnation` is the generation at allocation and never changes. IDs are
/// never reused: allocations are at or above the captured next_inode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalInodeIdentity {
    pub incarnation: u64,
    pub epoch: u64,
    pub revision: u64,
}

impl PhysicalInodeIdentity {
    /// This projects a token only. It does NOT prove the corresponding cached
    /// body is fresh; selected CAS/conditional reads must also compare physical
    /// identity. Old physical epochs remain persisted until that guard changes.
    pub fn logical_version(self, generation: u64) -> Result<InodeVersion> {
        if self.incarnation == 0 || self.incarnation > self.epoch || self.epoch > generation {
            return Err(invalid_namespace(
                "invalid compact physical epoch/incarnation",
            ));
        }
        Ok(InodeVersion {
            structural_generation: generation,
            inode_revision: if self.epoch < generation {
                0
            } else {
                self.revision
            },
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactGuard {
    pub identity: PhysicalInodeIdentity,
    pub node: NodeMetadata,
}

impl CompactGuard {
    fn validate(&self, inode: InodeId, anchor: &CompactAnchor) -> Result<()> {
        self.identity.logical_version(anchor.generation)?;
        if inode == 0 || self.node.stats.ino != inode {
            return Err(invalid_namespace("compact guard inode mismatch"));
        }
        validate_node_kind(&self.node)?;
        if let NodeData::Directory { entries } = &self.node.data {
            let mut names = BTreeSet::new();
            for entry in entries {
                validate_entry_name(&entry.name)?;
                if !names.insert(&entry.name) || anchor.members.binary_search(&entry.inode).is_err()
                {
                    return Err(invalid_namespace("invalid compact directory entries"));
                }
            }
        }
        Ok(())
    }
}

/// Materialized reference state. A provider must supply a single transactional
/// view; a type/validator cannot establish whether separately read rows coexisted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactSnapshot {
    pub anchor: CompactAnchor,
    pub guards: BTreeMap<InodeId, CompactGuard>,
}

/// Proof that a complete compact snapshot passed namespace graph validation.
/// It describes topology only; it cannot certify the freshness of guard bodies.
#[derive(Debug, Clone)]
pub struct ValidatedCompactStructure {
    anchor: std::sync::Arc<CompactAnchor>,
    // Retain only the audited root guard, never a Namespace Arc. Exact root
    // equality binds a root-child proposal to the graph that was audited;
    // selected file updates cannot change this directory or its identity.
    root: std::sync::Arc<CompactGuard>,
}

impl ValidatedCompactStructure {
    pub fn anchor(&self) -> &CompactAnchor {
        &self.anchor
    }
}

impl CompactSnapshot {
    /// Validate the complete graph, then move guard bodies into the runtime
    /// namespace without retaining a second complete guard-body mirror.
    pub fn into_validated_namespace(
        self,
    ) -> Result<(
        Namespace,
        BTreeMap<InodeId, PhysicalInodeIdentity>,
        ValidatedCompactStructure,
    )> {
        let CompactSnapshot { anchor, guards } = self;
        anchor.validate()?;
        if !anchor.members.iter().copied().eq(guards.keys().copied()) {
            return Err(invalid_namespace(
                "compact guard membership differs from anchor",
            ));
        }
        let mut namespace = Namespace {
            format_version: NAMESPACE_FORMAT_VERSION,
            root: anchor.root,
            next_inode: anchor.next_inode,
            default_uid: anchor.default_uid,
            default_gid: anchor.default_gid,
            umask: anchor.umask,
            default_chunker: anchor.default_chunker.clone(),
            nodes: BTreeMap::new(),
        };
        let mut identities = BTreeMap::new();
        for (inode, guard) in guards {
            guard.validate(inode, &anchor)?;
            identities.insert(inode, guard.identity);
            namespace.nodes.insert(inode, guard.node);
        }
        namespace.validate()?;
        let root = CompactGuard {
            identity: identities[&anchor.root],
            node: namespace.nodes[&anchor.root].clone(),
        };
        Ok((
            namespace,
            identities,
            ValidatedCompactStructure {
                anchor: std::sync::Arc::new(anchor),
                root: std::sync::Arc::new(root),
            },
        ))
    }

    pub fn namespace(&self) -> Result<Namespace> {
        let _profile =
            Span::new(Event::CompactNamespaceMaterializeNodes).units(self.guards.len() as u64);
        self.anchor.validate()?;
        if !self
            .anchor
            .members
            .iter()
            .copied()
            .eq(self.guards.keys().copied())
        {
            return Err(invalid_namespace(
                "compact guard membership differs from anchor",
            ));
        }
        for (&inode, guard) in &self.guards {
            guard.validate(inode, &self.anchor)?;
        }
        let namespace = Namespace {
            format_version: NAMESPACE_FORMAT_VERSION,
            root: self.anchor.root,
            next_inode: self.anchor.next_inode,
            default_uid: self.anchor.default_uid,
            default_gid: self.anchor.default_gid,
            umask: self.anchor.umask,
            default_chunker: self.anchor.default_chunker.clone(),
            nodes: self
                .guards
                .iter()
                .map(|(&id, guard)| (id, guard.node.clone()))
                .collect(),
        };
        namespace.validate()?;
        Ok(namespace)
    }

    /// Pure reference transition, not an I/O implementation. Clones the full
    /// state for tests. Providers use `validate_selected_update` on locked rows.
    pub fn selected_update(
        &self,
        backing: ConcurrentBackingId,
        generation: u64,
        inode: InodeId,
        expected: PhysicalInodeIdentity,
        node: NodeMetadata,
    ) -> Result<Self> {
        self.namespace()?;
        let current = self
            .guards
            .get(&inode)
            .ok_or_else(|| invalid_namespace("missing compact guard"))?;
        let updated = validate_selected_update(
            &self.anchor,
            backing,
            generation,
            inode,
            current,
            expected,
            node,
        )?;
        let mut next = self.clone();
        next.guards.insert(inode, updated);
        Ok(next)
    }
}

/// Shared selected-write transaction check. Read anchor and this guard in one
/// transaction, persist only the returned guard, and protect the anchor read
/// against concurrent structure publication. No unrelated guard write is needed.
pub fn validate_selected_update(
    anchor: &CompactAnchor,
    backing: ConcurrentBackingId,
    generation: u64,
    inode: InodeId,
    current: &CompactGuard,
    expected: PhysicalInodeIdentity,
    node: NodeMetadata,
) -> Result<CompactGuard> {
    anchor.validate()?;
    current.validate(inode, anchor)?;
    if anchor.backing != backing {
        return Err(FsError::new(ErrorCode::Estale));
    }
    if anchor.generation != generation || current.identity != expected {
        return Err(FsError::new(ErrorCode::Eagain));
    }
    if anchor.members.binary_search(&inode).is_err() {
        return Err(invalid_namespace(
            "selected guard absent from compact membership",
        ));
    }
    validate_inode_publication(inode, &current.node, &node)?;
    let revision = current
        .identity
        .logical_version(anchor.generation)?
        .inode_revision
        .checked_add(1)
        .ok_or_else(|| overflow_namespace("compact inode revision exhausted"))?;
    Ok(CompactGuard {
        identity: PhysicalInodeIdentity {
            incarnation: current.identity.incarnation,
            epoch: anchor.generation,
            revision,
        },
        node,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuralScope {
    /// Complete physical version map, including untouched guards.
    Full,
    /// One independent regular-file create; only its parent guard is expected.
    FileCreate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentEntryPrecondition {
    pub parent: InodeId,
    pub name: String,
    pub expected: Option<InodeId>,
}

/// Immutable intent captured from a validated base BEFORE I/O. Untouched bodies
/// are absent. Read-only accessors expose exact expected rows and planned writes.
#[derive(Debug, Clone)]
pub struct CompactStructuralDelta {
    base: CompactAnchor,
    next: CompactAnchor,
    expected: BTreeMap<InodeId, PhysicalInodeIdentity>,
    changed: BTreeMap<InodeId, NodeMetadata>,
    created: BTreeMap<InodeId, NodeMetadata>,
    removed: BTreeSet<InodeId>,
    entries: Vec<ParentEntryPrecondition>,
    parent_body: Option<NodeMetadata>,
    scope: StructuralScope,
}

/// Exact write set/receipt for an acknowledged structural publication. Untouched
/// bodies are deliberately absent: core must invalidate/reload those bodies,
/// even though their logical tokens now project to generation/revision zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactPublication {
    pub anchor: CompactAnchor,
    pub upserts: BTreeMap<InodeId, CompactGuard>,
    pub removed: BTreeSet<InodeId>,
}

/// A root-child empty regular-file create constructed from an audited graph.
/// There is no arbitrary cached/candidate Namespace input: unchanged topology
/// is inherited from the opaque structural witness and the constructor builds
/// the only permitted parent/new-file delta. Membership and parent entries
/// still require O(N) copying in the current durable format.
#[derive(Debug, Clone)]
pub struct CompactRootFileCreate {
    delta: CompactStructuralDelta,
}

impl CompactRootFileCreate {
    pub fn capture(
        structure: &ValidatedCompactStructure,
        parent: &LoadedCompactInode,
        expected_parent: PhysicalInodeIdentity,
        name: String,
        created: NodeMetadata,
        mtime_ms: i64,
        ctime_ms: i64,
    ) -> Result<Self> {
        let _profile = Span::new(Event::CompactStructuralDeltaCaptureNodes).units(2);
        let fail = || invalid_namespace("proposal is not an audited root-child file create");
        let anchor = structure.anchor();
        anchor.validate()?;
        if parent.generation != anchor.generation
            || parent.guard.identity != expected_parent
            || parent.guard.identity != structure.root.identity
        {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if &parent.guard != structure.root.as_ref() {
            return Err(invalid_namespace(
                "compact root body differs from audited graph",
            ));
        }
        parent.guard.validate(anchor.root, anchor)?;
        validate_entry_name(&name)?;
        let NodeData::Directory { entries } = &parent.guard.node.data else {
            return Err(fail());
        };
        if entries.iter().any(|entry| entry.name == name) {
            return Err(FsError::new(ErrorCode::Eexist));
        }
        validate_node_kind(&created)?;
        let NodeData::File(layout) = &created.data else {
            return Err(fail());
        };
        if created.stats.ino != anchor.next_inode
            || created.stats.mode & S_IFMT != S_IFREG
            || created.stats.nlink != 1
            || created.stats.uid != anchor.default_uid
            || created.stats.gid != anchor.default_gid
            || created.stats.size != 0
            || created.stats.blocks != 0
            || !layout.extents.is_empty()
            || layout.chunker != anchor.default_chunker
        {
            return Err(fail());
        }
        let minimum_mtime = parent
            .guard
            .node
            .stats
            .mtime_ms
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact parent modification time exhausted"))?;
        let minimum_ctime = parent
            .guard
            .node
            .stats
            .ctime_ms
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact parent change time exhausted"))?;
        if mtime_ms < minimum_mtime || ctime_ms < minimum_ctime {
            return Err(fail());
        }
        let generation = anchor
            .generation
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact structural generation exhausted"))?;
        let next_inode = anchor
            .next_inode
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact inode allocation exhausted"))?;
        let mut next = anchor.clone();
        next.generation = generation;
        next.next_inode = next_inode;
        next.members.push(anchor.next_inode);
        let mut changed = parent.guard.node.clone();
        let NodeData::Directory { entries } = &mut changed.data else {
            return Err(fail());
        };
        entries.push(DirectoryEntry {
            name: name.clone(),
            inode: anchor.next_inode,
        });
        changed.stats.mtime_ms = mtime_ms;
        changed.stats.ctime_ms = ctime_ms;
        let delta = CompactStructuralDelta {
            base: anchor.clone(),
            next,
            expected: BTreeMap::from([(anchor.root, expected_parent)]),
            changed: BTreeMap::from([(anchor.root, changed)]),
            created: BTreeMap::from([(anchor.next_inode, created)]),
            removed: BTreeSet::new(),
            entries: vec![ParentEntryPrecondition {
                parent: anchor.root,
                name,
                expected: None,
            }],
            parent_body: Some(parent.guard.node.clone()),
            scope: StructuralScope::FileCreate,
        };
        delta.validate_file_create_parent(&parent.guard.node)?;
        profile::add(Event::CompactStructuralExpectedGuardNodes, 1);
        Ok(Self { delta })
    }

    pub fn delta(&self) -> &CompactStructuralDelta {
        &self.delta
    }

    /// Exact acknowledgement of this constructor-built delta extends the
    /// audited graph inductively. No unrelated body is asserted fresh.
    pub fn validate_publication(
        &self,
        receipt: &CompactPublication,
    ) -> Result<ValidatedCompactStructure> {
        self.delta.validate_receipt(receipt)?;
        let root = receipt
            .upserts
            .get(&receipt.anchor.root)
            .ok_or_else(|| invalid_namespace("root-child receipt omits its parent"))?;
        Ok(ValidatedCompactStructure {
            anchor: std::sync::Arc::new(receipt.anchor.clone()),
            root: std::sync::Arc::new(root.clone()),
        })
    }
}

impl CompactStructuralDelta {
    /// Derive structural provenance only after exact receipt validation and
    /// a complete candidate graph check. The witness says nothing about
    /// freshness of bodies that were not changed by this publication.
    pub fn validate_next_structure(
        &self,
        base: &ValidatedCompactStructure,
        receipt: &CompactPublication,
        candidate: &Namespace,
    ) -> Result<ValidatedCompactStructure> {
        if base.anchor() != &self.base {
            return Err(invalid_namespace(
                "compact structural witness differs from capture",
            ));
        }
        self.validate_receipt(receipt)?;
        candidate.validate()?;
        if candidate.root != receipt.anchor.root
            || candidate.next_inode != receipt.anchor.next_inode
            || candidate.default_uid != receipt.anchor.default_uid
            || candidate.default_gid != receipt.anchor.default_gid
            || candidate.umask != receipt.anchor.umask
            || candidate.default_chunker != receipt.anchor.default_chunker
            || !candidate
                .nodes
                .keys()
                .copied()
                .eq(receipt.anchor.members.iter().copied())
        {
            return Err(invalid_namespace(
                "compact structural receipt differs from candidate",
            ));
        }
        let root = receipt
            .upserts
            .get(&receipt.anchor.root)
            .unwrap_or(base.root.as_ref());
        if candidate.nodes.get(&receipt.anchor.root) != Some(&root.node) {
            return Err(invalid_namespace(
                "compact structural candidate root differs from receipt",
            ));
        }
        Ok(ValidatedCompactStructure {
            anchor: std::sync::Arc::new(receipt.anchor.clone()),
            root: std::sync::Arc::new(root.clone()),
        })
    }

    /// Acknowledged providers may return only this exact write set. This
    /// validates the receipt before local installation or guard disarming.
    pub fn validate_receipt(&self, receipt: &CompactPublication) -> Result<()> {
        if receipt.anchor != self.next || receipt.removed != self.removed {
            return Err(invalid_namespace(
                "compact structural receipt anchor/removals differ",
            ));
        }
        if receipt.upserts.len() != self.changed.len() + self.created.len() {
            return Err(invalid_namespace(
                "compact structural receipt upserts differ",
            ));
        }
        for (&inode, node) in self.changed.iter().chain(&self.created) {
            let incarnation = self
                .expected
                .get(&inode)
                .map_or(self.next.generation, |identity| identity.incarnation);
            let expected = CompactGuard {
                identity: PhysicalInodeIdentity {
                    incarnation,
                    epoch: self.next.generation,
                    revision: 0,
                },
                node: node.clone(),
            };
            if receipt.upserts.get(&inode) != Some(&expected) {
                return Err(invalid_namespace(
                    "compact structural receipt body/identity differs",
                ));
            }
        }
        Ok(())
    }

    pub fn capture(
        base: &CompactSnapshot,
        candidate: &Namespace,
        scope: StructuralScope,
    ) -> Result<Self> {
        let _profile = Span::new(Event::CompactStructuralDeltaCaptureNodes)
            .units(candidate.nodes.len() as u64);
        base.namespace()?;
        candidate.validate()?;
        let generation = base
            .anchor
            .generation
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact structural generation exhausted"))?;
        if candidate.root != base.anchor.root || candidate.next_inode < base.anchor.next_inode {
            return Err(invalid_namespace("compact root/allocation moved backwards"));
        }
        let mut delta = Self {
            base: base.anchor.clone(),
            next: base.anchor.with_namespace(candidate, generation),
            expected: BTreeMap::new(),
            changed: BTreeMap::new(),
            created: BTreeMap::new(),
            removed: BTreeSet::new(),
            entries: Vec::new(),
            parent_body: None,
            scope,
        };
        for (&inode, old) in &base.guards {
            match candidate.nodes.get(&inode) {
                None => {
                    delta.removed.insert(inode);
                }
                Some(node) if node != &old.node => {
                    delta.changed.insert(inode, node.clone());
                }
                _ => {}
            }
            if scope == StructuralScope::Full
                || delta.changed.contains_key(&inode)
                || delta.removed.contains(&inode)
            {
                delta.expected.insert(inode, old.identity);
            }
            let old_entries = directory_entries(&old.node);
            let new_entries = candidate
                .nodes
                .get(&inode)
                .map(directory_entries)
                .unwrap_or_default();
            for name in old_entries
                .keys()
                .chain(new_entries.keys())
                .copied()
                .collect::<BTreeSet<_>>()
            {
                if old_entries.get(name) != new_entries.get(name) {
                    delta.entries.push(ParentEntryPrecondition {
                        parent: inode,
                        name: name.to_owned(),
                        expected: old_entries.get(name).copied(),
                    });
                }
            }
        }
        for (&inode, node) in &candidate.nodes {
            if !base.guards.contains_key(&inode) {
                if inode < base.anchor.next_inode {
                    return Err(invalid_namespace(
                        "compact allocation reuses old inode range",
                    ));
                }
                delta.created.insert(inode, node.clone());
            }
        }
        if scope == StructuralScope::FileCreate {
            delta.validate_file_create(base)?;
            delta.parent_body = Some(base.guards[&delta.entries[0].parent].node.clone());
        }
        profile::add(
            Event::CompactStructuralExpectedGuardNodes,
            delta.expected.len() as u64,
        );
        Ok(delta)
    }

    /// Capture one file create from retained topology and a fresh selected parent.
    /// The cached namespace supplies unchanged bodies, not their freshness. Only
    /// the parent is expected at publication; unrelated selected bodies remain
    /// outside the transaction read/write set.
    pub fn capture_file_create(
        structure: &ValidatedCompactStructure,
        cached: &Namespace,
        parent: &LoadedCompactInode,
        expected_parent: PhysicalInodeIdentity,
        candidate: &Namespace,
    ) -> Result<Self> {
        let _profile = Span::new(Event::CompactStructuralDeltaCaptureNodes)
            .units(candidate.nodes.len() as u64);
        let fail = || invalid_namespace("delta is not an independent file create");
        let anchor = structure.anchor();
        anchor.validate()?;
        candidate.validate()?;
        if cached.format_version != NAMESPACE_FORMAT_VERSION
            || cached.root != anchor.root
            || cached.next_inode != anchor.next_inode
            || cached.default_uid != anchor.default_uid
            || cached.default_gid != anchor.default_gid
            || cached.umask != anchor.umask
            || cached.default_chunker != anchor.default_chunker
            || !cached
                .nodes
                .keys()
                .copied()
                .eq(anchor.members.iter().copied())
        {
            return Err(fail());
        }
        if parent.generation != anchor.generation {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let inode = parent.guard.node.stats.ino;
        parent.guard.validate(inode, anchor)?;
        if parent.guard.identity != expected_parent {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let old_parent = cached.nodes.get(&inode).ok_or_else(fail)?;
        if old_parent != &parent.guard.node {
            return Err(invalid_namespace(
                "compact parent body differs from cached namespace",
            ));
        }
        let generation = anchor
            .generation
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact structural generation exhausted"))?;
        let next_inode = anchor
            .next_inode
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact inode allocation exhausted"))?;
        let mut next = anchor.clone();
        next.generation = generation;
        next.next_inode = next_inode;
        next.members.push(anchor.next_inode);
        if candidate.root != next.root
            || candidate.next_inode != next.next_inode
            || candidate.default_uid != next.default_uid
            || candidate.default_gid != next.default_gid
            || candidate.umask != next.umask
            || candidate.default_chunker != next.default_chunker
            || !candidate
                .nodes
                .keys()
                .copied()
                .eq(next.members.iter().copied())
        {
            return Err(fail());
        }
        for (&id, node) in &cached.nodes {
            if id != inode && candidate.nodes.get(&id) != Some(node) {
                return Err(fail());
            }
        }
        let changed_parent = candidate.nodes.get(&inode).ok_or_else(fail)?;
        let NodeData::Directory { entries } = &changed_parent.data else {
            return Err(fail());
        };
        let NodeData::Directory {
            entries: old_entries,
        } = &old_parent.data
        else {
            return Err(fail());
        };
        let mut remaining = old_entries.iter().peekable();
        let mut added_entry = None;
        for entry in entries {
            if remaining.peek().is_some_and(|old| *old == entry) {
                remaining.next();
            } else if added_entry.replace(entry).is_some() {
                return Err(fail());
            }
        }
        if remaining.next().is_some() {
            return Err(fail());
        }
        let added_entry = added_entry.ok_or_else(fail)?;
        let created = candidate.nodes.get(&anchor.next_inode).ok_or_else(fail)?;
        let delta = Self {
            base: anchor.clone(),
            next,
            expected: BTreeMap::from([(inode, expected_parent)]),
            changed: BTreeMap::from([(inode, changed_parent.clone())]),
            created: BTreeMap::from([(anchor.next_inode, created.clone())]),
            removed: BTreeSet::new(),
            entries: vec![ParentEntryPrecondition {
                parent: inode,
                name: added_entry.name.clone(),
                expected: None,
            }],
            parent_body: Some(old_parent.clone()),
            scope: StructuralScope::FileCreate,
        };
        delta.validate_file_create_parent(old_parent)?;
        profile::add(Event::CompactStructuralExpectedGuardNodes, 1);
        Ok(delta)
    }

    fn validate_file_create(&self, base: &CompactSnapshot) -> Result<()> {
        let old = self
            .entries
            .first()
            .and_then(|entry| base.guards.get(&entry.parent))
            .ok_or_else(|| invalid_namespace("delta is not an independent file create"))?;
        self.validate_file_create_parent(&old.node)
    }

    fn validate_file_create_parent(&self, old: &NodeMetadata) -> Result<()> {
        let fail = || invalid_namespace("delta is not an independent file create");
        let next_inode = self
            .base
            .next_inode
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact inode allocation exhausted"))?;
        let mut allowed_anchor = self.base.clone();
        allowed_anchor.generation = self.next.generation;
        allowed_anchor.next_inode = next_inode;
        allowed_anchor.members.push(self.base.next_inode);
        if self.next != allowed_anchor
            || self.created.len() != 1
            || self.changed.len() != 1
            || !self.removed.is_empty()
            || self.entries.len() != 1
        {
            return Err(fail());
        }
        let created = self.created.get(&self.base.next_inode).ok_or_else(fail)?;
        if !matches!(created.data, NodeData::File(_)) || created.stats.nlink != 1 {
            return Err(fail());
        }
        let entry = &self.entries[0];
        if entry.expected.is_some() || old.stats.ino != entry.parent {
            return Err(fail());
        }
        let changed = self.changed.get(&entry.parent).ok_or_else(fail)?;
        let (
            NodeData::Directory {
                entries: old_entries,
            },
            NodeData::Directory {
                entries: new_entries,
            },
        ) = (&old.data, &changed.data)
        else {
            return Err(fail());
        };
        let mut allowed_stats = old.stats.clone();
        allowed_stats.mtime_ms = changed.stats.mtime_ms;
        allowed_stats.ctime_ms = changed.stats.ctime_ms;
        if changed.stats != allowed_stats
            || new_entries.len() != old_entries.len() + 1
            || new_entries
                .iter()
                .filter(|item| item.name != entry.name)
                .ne(old_entries.iter())
            || !new_entries
                .iter()
                .any(|item| item.name == entry.name && item.inode == self.base.next_inode)
        {
            return Err(fail());
        }
        Ok(())
    }

    pub fn scope(&self) -> StructuralScope {
        self.scope
    }
    pub fn base_anchor(&self) -> &CompactAnchor {
        &self.base
    }
    pub fn next_anchor(&self) -> &CompactAnchor {
        &self.next
    }
    pub fn expected(&self) -> &BTreeMap<InodeId, PhysicalInodeIdentity> {
        &self.expected
    }
    pub fn changed(&self) -> &BTreeMap<InodeId, NodeMetadata> {
        &self.changed
    }
    pub fn created(&self) -> &BTreeMap<InodeId, NodeMetadata> {
        &self.created
    }
    pub fn removed(&self) -> &BTreeSet<InodeId> {
        &self.removed
    }
    pub fn parent_entries(&self) -> &[ParentEntryPrecondition] {
        &self.entries
    }

    /// Shared transaction validator. Supply EXACTLY expected guards from the
    /// same transaction as anchor. Full scope MUST enumerate the entire physical
    /// guard keyspace, including phantoms, with range/phantom protection; selecting
    /// only expected IDs cannot prove exact membership. Create scope requires
    /// only its affected parent. Lock/conflict-protect all
    /// those reads and the anchor through commit. Reopen separately audits full
    /// membership/body corruption; a targeted operation is not a complete audit.
    pub fn validate_current(
        &self,
        anchor: &CompactAnchor,
        guards: &BTreeMap<InodeId, CompactGuard>,
    ) -> Result<CompactPublication> {
        anchor.validate()?;
        if anchor.backing != self.base.backing {
            return Err(FsError::new(ErrorCode::Estale));
        }
        if anchor != &self.base {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if !guards.keys().eq(self.expected.keys()) {
            return Err(invalid_namespace(
                "compact transaction guard set is not exact",
            ));
        }
        if self.scope == StructuralScope::Full {
            CompactSnapshot {
                anchor: anchor.clone(),
                guards: guards.clone(),
            }
            .namespace()?;
        }
        for (&inode, guard) in guards {
            guard.validate(inode, anchor)?;
            if guard.identity != self.expected[&inode] {
                return Err(FsError::new(ErrorCode::Eagain));
            }
        }
        for entry in &self.entries {
            let guard = guards
                .get(&entry.parent)
                .ok_or_else(|| invalid_namespace("missing parent precondition guard"))?;
            let NodeData::Directory { entries } = &guard.node.data else {
                return Err(invalid_namespace(
                    "parent precondition guard is not a directory",
                ));
            };
            if entries
                .iter()
                .find(|item| item.name == entry.name)
                .map(|item| item.inode)
                != entry.expected
            {
                return Err(FsError::new(ErrorCode::Eagain));
            }
        }
        if self.scope == StructuralScope::FileCreate {
            let entry = &self.entries[0];
            if self.parent_body.as_ref() != Some(&guards[&entry.parent].node) {
                return Err(invalid_namespace(
                    "compact parent body differs from captured guard",
                ));
            }
        }
        let mut upserts = BTreeMap::new();
        for (&inode, node) in self.changed.iter().chain(&self.created) {
            let incarnation = self
                .expected
                .get(&inode)
                .map_or(self.next.generation, |identity| identity.incarnation);
            upserts.insert(
                inode,
                CompactGuard {
                    identity: PhysicalInodeIdentity {
                        incarnation,
                        epoch: self.next.generation,
                        revision: 0,
                    },
                    node: node.clone(),
                },
            );
        }
        Ok(CompactPublication {
            anchor: self.next.clone(),
            upserts,
            removed: self.removed.clone(),
        })
    }

    /// Pure transaction reference evaluator. Reads/validates/clones complete
    /// state; it makes NO provider performance claim. The same shared validator
    /// above produces the minimal write set for provider implementations.
    pub fn evaluate(&self, current: &CompactSnapshot) -> Result<CompactSnapshot> {
        current.namespace()?;
        let read_set = current
            .guards
            .iter()
            .filter(|(id, _)| self.expected.contains_key(id))
            .map(|(&id, guard)| (id, guard.clone()))
            .collect();
        let writes = self.validate_current(&current.anchor, &read_set)?;
        let mut next = current.clone();
        next.anchor = writes.anchor;
        for inode in writes.removed {
            next.guards.remove(&inode);
        }
        next.guards.extend(writes.upserts);
        next.namespace()?;
        Ok(next)
    }
}

fn directory_entries(node: &NodeMetadata) -> BTreeMap<&str, InodeId> {
    match &node.data {
        NodeData::Directory { entries } => entries
            .iter()
            .map(|entry| (entry.name.as_str(), entry.inode))
            .collect(),
        _ => BTreeMap::new(),
    }
}

#[cfg(test)]
mod tests;
