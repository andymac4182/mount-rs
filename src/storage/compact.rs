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

impl CompactSnapshot {
    pub fn namespace(&self) -> Result<Namespace> {
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

impl CompactStructuralDelta {
    pub fn capture(
        base: &CompactSnapshot,
        candidate: &Namespace,
        scope: StructuralScope,
    ) -> Result<Self> {
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
        }
        Ok(delta)
    }

    fn validate_file_create(&self, base: &CompactSnapshot) -> Result<()> {
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
        if entry.expected.is_some() {
            return Err(fail());
        }
        let old = &base.guards.get(&entry.parent).ok_or_else(fail)?.node;
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
