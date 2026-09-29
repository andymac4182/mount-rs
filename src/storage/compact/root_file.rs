//! Fixed root/file capture and sealed structural transitions. Their graph
//! provenance is inherited from a complete audit; unrelated bodies are neither
//! fetched nor asserted fresh by these operations.

use super::*;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompactRootFileCapability {
    #[default]
    Unsupported,
    Supported,
}

/// Complete owned bodies from one provider view. A missing LEFT JOIN group is
/// retained so the caller can handle changed generation before missing data.
#[derive(Debug, Clone)]
pub struct CompactRootFileRead {
    anchor: CompactAnchor,
    root: Option<CompactGuard>,
    file: Option<CompactGuard>,
}

impl CompactRootFileRead {
    /// Providers must decode/check fresh authority before constructing this
    /// value. The requested IDs are candidates, never authority for a name.
    pub fn from_guards(
        anchor: CompactAnchor,
        expected_root: InodeId,
        candidate_file: InodeId,
        root: Option<CompactGuard>,
        file: Option<CompactGuard>,
    ) -> Result<Self> {
        anchor.validate()?;
        if expected_root == 0 || candidate_file == 0 || expected_root == candidate_file {
            return Err(invalid_namespace("invalid compact root-file requested IDs"));
        }
        for (inode, guard) in [
            (expected_root, root.as_ref()),
            (candidate_file, file.as_ref()),
        ] {
            if let Some(guard) = guard {
                guard.validate(inode, &anchor)?;
                if anchor.members.binary_search(&inode).is_err() {
                    return Err(invalid_namespace(
                        "root-file guard absent from compact membership",
                    ));
                }
            }
        }
        Ok(Self { anchor, root, file })
    }

    pub fn anchor(&self) -> &CompactAnchor {
        &self.anchor
    }
    pub fn root(&self) -> Option<&CompactGuard> {
        self.root.as_ref()
    }
    pub fn file(&self) -> Option<&CompactGuard> {
        self.file.as_ref()
    }
    pub fn into_parts(self) -> (CompactAnchor, Option<CompactGuard>, Option<CompactGuard>) {
        (self.anchor, self.root, self.file)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactRootFileIntent {
    RenameAbsent { from: String, to: String },
    UnlinkLastLink { name: String },
}

/// Supply the existing monotonic wall-clock helper's results. The constructor
/// enforces minimum advances; it does not replace those helpers with constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CompactRootFileTimes {
    pub parent_mtime_ms: i64,
    pub parent_ctime_ms: i64,
    pub file_ctime_ms: i64,
}

#[derive(Debug)]
pub struct VerifiedCompactRootFile {
    structure: ValidatedCompactStructure,
    root: CompactGuard,
    file: CompactGuard,
}

impl VerifiedCompactRootFile {
    pub fn structure(&self) -> &ValidatedCompactStructure {
        &self.structure
    }
    pub fn generation(&self) -> u64 {
        self.structure.anchor.generation
    }
    pub fn root(&self) -> &CompactGuard {
        &self.root
    }
    pub fn file(&self) -> &CompactGuard {
        &self.file
    }
}

impl ValidatedCompactStructure {
    /// Classification from the audited, sorted child index. Sparse/high inode
    /// IDs are binary-searched; their numeric value never indexes the bitset.
    pub fn root_single_link_file(&self, inode: InodeId) -> bool {
        self.root_children.binary_search(&inode).is_ok_and(|index| {
            self.root_single_link_file_bits[index / 64] & (1_u64 << (index % 64)) != 0
        })
    }

    /// Identity of the immutable audited value, for a local-revision fence.
    pub fn same_witness(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.anchor, &other.anchor)
            && Arc::ptr_eq(&self.root, &other.root)
            && Arc::ptr_eq(&self.root_children, &other.root_children)
            && Arc::ptr_eq(
                &self.root_single_link_file_bits,
                &other.root_single_link_file_bits,
            )
    }

    pub(super) fn root_file_rename_index(&self) -> (Arc<[InodeId]>, Arc<[u64]>) {
        (
            Arc::clone(&self.root_children),
            Arc::clone(&self.root_single_link_file_bits),
        )
    }

    /// Bind the fresh complete pair to the current cached body/physical pair.
    /// The coordinator must also recheck its local revision before publication.
    pub fn verify_root_file(
        &self,
        read: CompactRootFileRead,
        candidate_file: InodeId,
        cached_file: &NodeMetadata,
        cached_identity: PhysicalInodeIdentity,
    ) -> Result<VerifiedCompactRootFile> {
        let CompactRootFileRead { anchor, root, file } = read;
        if anchor.backing != self.anchor.backing {
            return Err(FsError::new(ErrorCode::Estale));
        }
        if anchor.generation != self.anchor.generation {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if anchor != *self.anchor {
            return Err(invalid_namespace(
                "compact root-file anchor differs from audited graph",
            ));
        }
        let root = root.ok_or_else(|| invalid_namespace("missing compact root-file root guard"))?;
        let file =
            file.ok_or_else(|| invalid_namespace("missing compact root-file source guard"))?;
        if root.identity != self.root.identity {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        if &root != self.root.as_ref() {
            return Err(invalid_namespace(
                "compact root body differs from audited graph",
            ));
        }
        root.validate(anchor.root, &anchor)?;
        file.validate(candidate_file, &anchor)?;
        if !self.root_single_link_file(candidate_file)
            || anchor.members.binary_search(&candidate_file).is_err()
        {
            return Err(invalid_namespace(
                "compact source lacks audited root-file eligibility",
            ));
        }
        let stale = || {
            FsError::new(ErrorCode::Estale)
                .with_message("inode structure changed without its generation")
        };
        cached_identity
            .logical_version(anchor.generation)
            .map_err(|_| stale())?;
        validate_inode_publication(candidate_file, cached_file, &file.node).map_err(|_| stale())?;
        if file.node.stats.nlink != 1
            || file.identity.incarnation != cached_identity.incarnation
            || (file.identity.epoch, file.identity.revision)
                < (cached_identity.epoch, cached_identity.revision)
            || (file.identity == cached_identity && &file.node != cached_file)
        {
            return Err(stale());
        }
        Ok(VerifiedCompactRootFile {
            structure: self.clone(),
            root,
            file,
        })
    }

    pub(super) fn with_created_root_file(
        &self,
        anchor: CompactAnchor,
        root: CompactGuard,
    ) -> Result<Self> {
        let inode = self.anchor.next_inode;
        if self.root_children.last().is_some_and(|last| *last >= inode) {
            return Err(invalid_namespace(
                "compact created child index is not increasing",
            ));
        }
        let count = self
            .root_children
            .len()
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact child index exhausted"))?;
        let mut children = Vec::with_capacity(count);
        children.extend_from_slice(&self.root_children);
        children.push(inode);
        Ok(Self {
            anchor: Arc::new(anchor),
            root: Arc::new(root),
            root_children: children.into(),
            root_single_link_file_bits: append_eligible_bit(
                &self.root_single_link_file_bits,
                self.root_children.len(),
            ),
        })
    }
}

/// Only this constructor creates root-file scopes. The complete source layout
/// is retained, including block references and a last-link tombstone's content.
#[derive(Debug)]
pub struct CompactRootFileTransition {
    delta: CompactStructuralDelta,
    structure: ValidatedCompactStructure,
    intent: CompactRootFileIntent,
}

impl CompactRootFileTransition {
    pub fn capture(
        verified: VerifiedCompactRootFile,
        intent: CompactRootFileIntent,
        times: CompactRootFileTimes,
    ) -> Result<Self> {
        let _profile = Span::new(Event::CompactStructuralDeltaCaptureNodes).units(2);
        let VerifiedCompactRootFile {
            structure,
            root,
            file,
        } = verified;
        let anchor = structure.anchor();
        let source = file.node.stats.ino;
        let (name, destination, scope, touches) = match &intent {
            CompactRootFileIntent::RenameAbsent { from, to } => {
                validate_entry_name(to)?;
                if from == to {
                    return Err(invalid_namespace("compact rename names must differ"));
                }
                (from, Some(to), StructuralScope::RootFileRenameAbsent, 2)
            }
            CompactRootFileIntent::UnlinkLastLink { name } => {
                (name, None, StructuralScope::RootFileUnlinkLastLink, 1)
            }
        };
        validate_entry_name(name)?;
        let NodeData::Directory { entries } = &root.node.data else {
            return Err(invalid_namespace(
                "compact root-file parent is not a directory",
            ));
        };
        let source_position = entries
            .iter()
            .position(|entry| entry.name == *name)
            .filter(|position| entries[*position].inode == source)
            .ok_or_else(|| FsError::new(ErrorCode::Enoent))?;
        if destination.is_some_and(|to| entries.iter().any(|entry| entry.name == *to)) {
            return Err(FsError::new(ErrorCode::Eexist));
        }
        validate_times(&root.node, &file.node, times, touches)?;
        let generation = anchor
            .generation
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact structural generation exhausted"))?;
        let mut next = anchor.clone();
        next.generation = generation;
        let mut changed_root = root.node.clone();
        let NodeData::Directory { entries } = &mut changed_root.data else {
            unreachable!()
        };
        entries.remove(source_position);
        if let Some(to) = destination {
            entries.push(DirectoryEntry {
                name: to.clone(),
                inode: source,
            });
        }
        changed_root.stats.mtime_ms = times.parent_mtime_ms;
        changed_root.stats.ctime_ms = times.parent_ctime_ms;
        let mut changed_file = file.node.clone();
        changed_file.stats.ctime_ms = times.file_ctime_ms;
        if destination.is_none() {
            changed_file.stats.nlink = 0;
        }
        let mut entries = vec![ParentEntryPrecondition {
            parent: anchor.root,
            name: name.clone(),
            expected: Some(source),
        }];
        if let Some(to) = destination {
            entries.push(ParentEntryPrecondition {
                parent: anchor.root,
                name: to.clone(),
                expected: None,
            });
        }
        let delta = CompactStructuralDelta {
            base: anchor.clone(),
            next,
            expected: BTreeMap::from([(anchor.root, root.identity), (source, file.identity)]),
            changed: BTreeMap::from([(anchor.root, changed_root), (source, changed_file)]),
            created: BTreeMap::new(),
            removed: BTreeSet::new(),
            entries,
            parent_body: None,
            captured_bodies: BTreeMap::from([(anchor.root, root.node), (source, file.node)]),
            next_root_children: None,
            next_root_file_bits: None,
            scope,
        };
        delta.validate_root_file_transition()?;
        profile::add(Event::CompactStructuralExpectedGuardNodes, 2);
        Ok(Self {
            delta,
            structure,
            intent,
        })
    }

    pub fn delta(&self) -> &CompactStructuralDelta {
        &self.delta
    }
    pub fn intent(&self) -> &CompactRootFileIntent {
        &self.intent
    }

    /// Derive topology only after checking the exact acknowledged write set.
    pub fn validate_publication(
        &self,
        receipt: &CompactPublication,
    ) -> Result<ValidatedCompactStructure> {
        self.delta.validate_receipt(receipt)?;
        let source = self.delta.entries[0]
            .expected
            .ok_or_else(|| invalid_namespace("compact source precondition missing"))?;
        let (root_children, root_single_link_file_bits) = match self.intent {
            CompactRootFileIntent::RenameAbsent { .. } => self.structure.root_file_rename_index(),
            CompactRootFileIntent::UnlinkLastLink { .. } => {
                let position = self
                    .structure
                    .root_children
                    .binary_search(&source)
                    .map_err(|_| invalid_namespace("compact unlink child is not indexed"))?;
                let mut children = Vec::with_capacity(self.structure.root_children.len() - 1);
                children.extend_from_slice(&self.structure.root_children[..position]);
                children.extend_from_slice(&self.structure.root_children[position + 1..]);
                (
                    children.into(),
                    remove_eligible_bit(
                        &self.structure.root_single_link_file_bits,
                        self.structure.root_children.len(),
                        position,
                    ),
                )
            }
        };
        Ok(ValidatedCompactStructure {
            anchor: Arc::new(receipt.anchor.clone()),
            root: Arc::new(receipt.upserts[&receipt.anchor.root].clone()),
            root_children,
            root_single_link_file_bits,
        })
    }
}

fn validate_times(
    root: &NodeMetadata,
    file: &NodeMetadata,
    times: CompactRootFileTimes,
    touches: i64,
) -> Result<()> {
    let mtime = root
        .stats
        .mtime_ms
        .checked_add(touches)
        .ok_or_else(|| overflow_namespace("compact parent modification time exhausted"))?;
    let ctime = root
        .stats
        .ctime_ms
        .checked_add(touches)
        .ok_or_else(|| overflow_namespace("compact parent change time exhausted"))?;
    let file_ctime = file
        .stats
        .ctime_ms
        .checked_add(1)
        .ok_or_else(|| overflow_namespace("compact file change time exhausted"))?;
    if times.parent_mtime_ms < mtime
        || times.parent_ctime_ms < ctime
        || times.file_ctime_ms < file_ctime
    {
        return Err(invalid_namespace("compact root-file times did not advance"));
    }
    Ok(())
}

impl CompactStructuralDelta {
    pub(super) fn validate_root_file_transition(&self) -> Result<()> {
        let fail = || invalid_namespace("delta is not a sealed root-file transition");
        let generation = self
            .base
            .generation
            .checked_add(1)
            .ok_or_else(|| overflow_namespace("compact structural generation exhausted"))?;
        if self.next.generation != generation
            || self.next.backing != self.base.backing
            || self.next.root != self.base.root
            || self.next.next_inode != self.base.next_inode
            || self.next.default_uid != self.base.default_uid
            || self.next.default_gid != self.base.default_gid
            || self.next.umask != self.base.umask
            || self.next.default_chunker != self.base.default_chunker
            || self.next.members != self.base.members
            || self.expected.len() != 2
            || self.changed.len() != 2
            || self.captured_bodies.len() != 2
            || !self.created.is_empty()
            || !self.removed.is_empty()
            || !self.expected.keys().eq(self.changed.keys())
            || !self.expected.keys().eq(self.captured_bodies.keys())
        {
            return Err(fail());
        }
        let source_entry = self.entries.first().ok_or_else(fail)?;
        let source = source_entry.expected.ok_or_else(fail)?;
        let root = self.base.root;
        if source == root
            || source_entry.parent != root
            || self.base.members.binary_search(&source).is_err()
        {
            return Err(fail());
        }
        validate_entry_name(&source_entry.name)?;
        let old_root = self.captured_bodies.get(&root).ok_or_else(fail)?;
        let old_file = self.captured_bodies.get(&source).ok_or_else(fail)?;
        let new_root = self.changed.get(&root).ok_or_else(fail)?;
        let new_file = self.changed.get(&source).ok_or_else(fail)?;
        validate_node_kind(old_root)?;
        validate_node_kind(old_file)?;
        validate_node_kind(new_root)?;
        validate_node_kind(new_file)?;
        let (
            NodeData::Directory {
                entries: old_entries,
            },
            NodeData::Directory {
                entries: new_entries,
            },
        ) = (&old_root.data, &new_root.data)
        else {
            return Err(fail());
        };
        if old_root.stats.ino != root
            || old_file.stats.ino != source
            || !matches!(old_file.data, NodeData::File(_))
            || old_file.stats.nlink != 1
            || old_entries
                .iter()
                .filter(|entry| entry.inode == source)
                .count()
                != 1
            || !old_entries
                .iter()
                .any(|entry| entry.name == source_entry.name && entry.inode == source)
        {
            return Err(fail());
        }
        let (touches, links) = match self.scope {
            StructuralScope::RootFileRenameAbsent => {
                if self.entries.len() != 2 {
                    return Err(fail());
                }
                let destination = &self.entries[1];
                validate_entry_name(&destination.name)?;
                if destination.parent != root
                    || destination.expected.is_some()
                    || destination.name == source_entry.name
                    || old_entries
                        .iter()
                        .any(|entry| entry.name == destination.name)
                    || new_entries.len() != old_entries.len()
                    || new_entries[..new_entries.len() - 1].iter().ne(old_entries
                        .iter()
                        .filter(|entry| entry.name != source_entry.name))
                    || new_entries
                        .last()
                        .is_none_or(|entry| entry.name != destination.name || entry.inode != source)
                {
                    return Err(fail());
                }
                (2, 1)
            }
            StructuralScope::RootFileUnlinkLastLink => {
                if self.entries.len() != 1
                    || new_entries.len().checked_add(1) != Some(old_entries.len())
                    || new_entries.iter().ne(old_entries
                        .iter()
                        .filter(|entry| entry.name != source_entry.name))
                {
                    return Err(fail());
                }
                (1, 0)
            }
            _ => return Err(fail()),
        };
        let times = CompactRootFileTimes {
            parent_mtime_ms: new_root.stats.mtime_ms,
            parent_ctime_ms: new_root.stats.ctime_ms,
            file_ctime_ms: new_file.stats.ctime_ms,
        };
        validate_times(old_root, old_file, times, touches)?;
        let mut root_stats = old_root.stats.clone();
        root_stats.mtime_ms = times.parent_mtime_ms;
        root_stats.ctime_ms = times.parent_ctime_ms;
        let mut file_stats = old_file.stats.clone();
        file_stats.ctime_ms = times.file_ctime_ms;
        file_stats.nlink = links;
        if new_root.stats != root_stats
            || new_file.stats != file_stats
            || new_file.data != old_file.data
        {
            return Err(fail());
        }
        Ok(())
    }
}

// One sized Vec and one Arc slice allocation; no per-child allocations.
pub(super) fn append_eligible_bit(bits: &[u64], children: usize) -> Arc<[u64]> {
    let mut next = vec![0; (children + 1).div_ceil(64)];
    next[..bits.len()].copy_from_slice(bits);
    next[children / 64] |= 1_u64 << (children % 64);
    next.into()
}

#[cfg(kani)]
mod verification;

pub(super) fn remove_eligible_bit(bits: &[u64], children: usize, position: usize) -> Arc<[u64]> {
    let count = children - 1;
    let mut next = vec![0; count.div_ceil(64)];
    for (word, value) in next.iter_mut().enumerate() {
        if word < position / 64 {
            *value = bits[word];
        } else {
            let low_mask = if word == position / 64 {
                (1_u64 << (position % 64)) - 1
            } else {
                0
            };
            *value = (bits[word] & low_mask)
                | ((bits[word] >> 1) & !low_mask)
                | ((bits.get(word + 1).copied().unwrap_or(0) & 1) << 63);
        }
    }
    if !count.is_multiple_of(64)
        && let Some(last) = next.last_mut()
    {
        *last &= (1_u64 << (count % 64)) - 1;
    }
    next.into()
}
