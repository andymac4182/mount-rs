//! Normalized metadata records for incremental providers and staged migration.
//!
//! This is a representation, not an authority or transaction implementation.
//! Providers must atomically fence publication and select one committed revision.
//! Record visitors can stream imports without constructing a namespace-sized JSON
//! row. Existing snapshot providers are unchanged until explicitly integrated.
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VolumeRecord {
    pub format_version: u32,
    pub root: InodeId,
    pub next_inode: InodeId,
    pub default_uid: u32,
    pub default_gid: u32,
    pub umask: u32,
    pub default_chunker: ChunkerConfig,
}

impl From<&Namespace> for VolumeRecord {
    fn from(namespace: &Namespace) -> Self {
        Self {
            format_version: namespace.format_version,
            root: namespace.root,
            next_inode: namespace.next_inode,
            default_uid: namespace.default_uid,
            default_gid: namespace.default_gid,
            umask: namespace.umask,
            default_chunker: namespace.default_chunker.clone(),
        }
    }
}

/// Inode attributes deliberately exclude directory children and file extents.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InodeRecord {
    pub stats: Stats,
    pub kind: InodeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InodeKind {
    Directory,
    File { chunker: ChunkerConfig },
    Symlink { target: String },
    Special,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DentryRecord {
    pub parent: InodeId,
    /// Stable listing order within this committed representation.
    pub ordinal: u64,
    pub entry: DirectoryEntry,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetadataRecord {
    Volume(VolumeRecord),
    Inode { inode: InodeId, value: InodeRecord },
    Dentry(DentryRecord),
    Extent { inode: InodeId, value: BlockExtent },
}

/// Exact provider row identity; names must use binary equality in SQL.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum RecordKey {
    Volume,
    Inode(InodeId),
    Dentry { parent: InodeId, name: String },
    Extent { inode: InodeId, file_offset: u64 },
}

impl MetadataRecord {
    pub fn key(&self) -> RecordKey {
        match self {
            Self::Volume(_) => RecordKey::Volume,
            Self::Inode { inode, .. } => RecordKey::Inode(*inode),
            Self::Dentry(value) => RecordKey::Dentry {
                parent: value.parent,
                name: value.entry.name.clone(),
            },
            Self::Extent { inode, value } => RecordKey::Extent {
                inode: *inode,
                file_offset: value.file_offset,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RecordChange {
    Put(MetadataRecord),
    Delete(RecordKey),
}

/// One atomic mutation at the caller's expected revision and fencing token.
/// Each row key may occur only once. A rename carries both its deletion and
/// insertion; truncate includes inode attributes and affected extent changes.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetadataBatch {
    pub changes: Vec<RecordChange>,
}

impl MetadataBatch {
    /// Publish an empty file appended to a parent directory by open/create.
    pub fn created_file(namespace: &Namespace, parent: InodeId, inode: InodeId) -> Result<Self> {
        let directory = namespace
            .nodes
            .get(&parent)
            .ok_or_else(|| invalid_namespace("missing create parent"))?;
        let NodeData::Directory { entries } = &directory.data else {
            return Err(invalid_namespace("create parent is not a directory"));
        };
        let entry = entries
            .last()
            .filter(|entry| entry.inode == inode)
            .ok_or_else(|| invalid_namespace("new file must be the appended directory entry"))?;
        let node = namespace
            .nodes
            .get(&inode)
            .ok_or_else(|| invalid_namespace("missing created inode"))?;
        if !matches!(&node.data, NodeData::File(layout) if layout.extents.is_empty() && node.stats.size == 0)
        {
            return Err(invalid_namespace("created inode must be an empty file"));
        }
        let ordinal = u64::try_from(entries.len() - 1)
            .map_err(|_| overflow_namespace("directory ordinal overflow"))?;
        Ok(Self {
            changes: vec![
                RecordChange::Put(MetadataRecord::Volume(VolumeRecord::from(namespace))),
                RecordChange::Put(MetadataRecord::Inode {
                    inode: parent,
                    value: InodeRecord::from_node(directory),
                }),
                RecordChange::Put(MetadataRecord::Inode {
                    inode,
                    value: InodeRecord::from_node(node),
                }),
                RecordChange::Put(MetadataRecord::Dentry(DentryRecord {
                    parent,
                    ordinal,
                    entry: entry.clone(),
                })),
            ],
        })
    }

    /// Update one linked file. Both layouts must have sorted, unique extent offsets.
    /// Unchanged extents keep their existing provider rows.
    pub fn file_update(
        inode: InodeId,
        stats: &Stats,
        old: &[BlockExtent],
        new: &FileLayout,
    ) -> Self {
        let mut changes = vec![RecordChange::Put(MetadataRecord::Inode {
            inode,
            value: InodeRecord {
                stats: stats.clone(),
                kind: InodeKind::File {
                    chunker: new.chunker.clone(),
                },
            },
        })];
        let mut previous = old.iter().peekable();
        let delete = |extent: &BlockExtent| {
            RecordChange::Delete(RecordKey::Extent {
                inode,
                file_offset: extent.file_offset,
            })
        };
        for extent in &new.extents {
            while let Some(removed) = previous.next_if(|old| old.file_offset < extent.file_offset) {
                changes.push(delete(removed));
            }
            if previous.next_if(|old| old.file_offset == extent.file_offset) != Some(extent) {
                changes.push(RecordChange::Put(MetadataRecord::Extent {
                    inode,
                    value: extent.clone(),
                }));
            }
        }
        changes.extend(previous.map(delete));
        Self { changes }
    }

    pub fn validate_keys(&self) -> Result<()> {
        let mut keys = BTreeSet::new();
        for change in &self.changes {
            let key = match change {
                RecordChange::Put(record) => record.key(),
                RecordChange::Delete(key) => key.clone(),
            };
            if !keys.insert(key) {
                return Err(invalid_namespace("duplicate metadata batch key"));
            }
        }
        Ok(())
    }

    /// Reference/compatibility application, intentionally O(namespace).
    /// Incremental providers must override publication instead of treating
    /// this full reconstruction as an incremental persistence implementation.
    /// The source is never mutated, including when validation fails.
    pub fn apply_to(&self, source: Option<&Namespace>) -> Result<Namespace> {
        self.validate_keys()?;
        let mut records = BTreeMap::new();
        if let Some(source) = source {
            visit_records(source, |record| {
                records.insert(record.key(), record);
                Ok(())
            })?;
        }
        for change in &self.changes {
            match change {
                RecordChange::Put(record) => {
                    records.insert(record.key(), record.clone());
                }
                RecordChange::Delete(key) => {
                    if records.remove(key).is_none() {
                        return Err(invalid_namespace("metadata batch deletes missing record"));
                    }
                }
            }
        }
        from_records(records.into_values())
    }
}

impl InodeRecord {
    pub fn from_node(node: &NodeMetadata) -> Self {
        Self {
            stats: node.stats.clone(),
            kind: match &node.data {
                NodeData::Directory { .. } => InodeKind::Directory,
                NodeData::File(layout) => InodeKind::File {
                    chunker: layout.chunker.clone(),
                },
                NodeData::Symlink { target } => InodeKind::Symlink {
                    target: target.clone(),
                },
                NodeData::Special => InodeKind::Special,
            },
        }
    }
    fn into_node(self) -> NodeMetadata {
        NodeMetadata {
            stats: self.stats,
            data: match self.kind {
                InodeKind::Directory => NodeData::Directory {
                    entries: Vec::new(),
                },
                InodeKind::File { chunker } => NodeData::File(FileLayout {
                    chunker,
                    extents: Vec::new(),
                }),
                InodeKind::Symlink { target } => NodeData::Symlink { target },
                InodeKind::Special => NodeData::Special,
            },
        }
    }
}

/// Visit a validated snapshot as independent rows. Stops on the first sink error.
/// Atomic activation of a staged import is the provider's responsibility.
pub fn visit_records(
    namespace: &Namespace,
    mut sink: impl FnMut(MetadataRecord) -> Result<()>,
) -> Result<()> {
    namespace.validate()?;
    sink(MetadataRecord::Volume(VolumeRecord::from(namespace)))?;
    for (&inode, node) in &namespace.nodes {
        sink(MetadataRecord::Inode {
            inode,
            value: InodeRecord::from_node(node),
        })?;
        match &node.data {
            NodeData::Directory { entries } => {
                for (ordinal, entry) in entries.iter().enumerate() {
                    sink(MetadataRecord::Dentry(DentryRecord {
                        parent: inode,
                        ordinal: u64::try_from(ordinal)
                            .map_err(|_| overflow_namespace("directory ordinal overflow"))?,
                        entry: entry.clone(),
                    }))?;
                }
            }
            NodeData::File(layout) => {
                for value in &layout.extents {
                    sink(MetadataRecord::Extent {
                        inode,
                        value: value.clone(),
                    })?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Reassemble one committed generation, independently of SQL row return order.
/// Reject duplicate keys and malformed cross-record references before serving.
pub fn from_records(records: impl IntoIterator<Item = MetadataRecord>) -> Result<Namespace> {
    let mut volume = None;
    let mut nodes = BTreeMap::new();
    let mut dentries = BTreeMap::new();
    let mut extents = BTreeMap::new();
    for record in records {
        let duplicate = match record {
            MetadataRecord::Volume(value) => volume.replace(value).is_some(),
            MetadataRecord::Inode { inode, value } => {
                nodes.insert(inode, value.into_node()).is_some()
            }
            MetadataRecord::Dentry(value) => dentries
                .insert((value.parent, value.ordinal), value.entry)
                .is_some(),
            MetadataRecord::Extent { inode, value } => {
                extents.insert((inode, value.file_offset), value).is_some()
            }
        };
        if duplicate {
            return Err(invalid_namespace("duplicate metadata record key"));
        }
    }
    for ((parent, _ordinal), entry) in dentries {
        match nodes.get_mut(&parent).map(|node| &mut node.data) {
            Some(NodeData::Directory { entries }) => entries.push(entry),
            _ => return Err(invalid_namespace("directory entry has no directory parent")),
        }
    }
    for ((inode, _offset), extent) in extents {
        match nodes.get_mut(&inode).map(|node| &mut node.data) {
            Some(NodeData::File(layout)) => layout.extents.push(extent),
            _ => return Err(invalid_namespace("extent has no file inode")),
        }
    }
    let volume = volume.ok_or_else(|| invalid_namespace("missing volume record"))?;
    let namespace = Namespace {
        format_version: volume.format_version,
        root: volume.root,
        next_inode: volume.next_inode,
        default_uid: volume.default_uid,
        default_gid: volume.default_gid,
        umask: volume.umask,
        default_chunker: volume.default_chunker,
        nodes,
    };
    namespace.validate()?;
    Ok(namespace)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rename_and_truncate(namespace: &Namespace) -> MetadataBatch {
        let mut inode = InodeRecord::from_node(&namespace.nodes[&2]);
        inode.stats.size = 0;
        inode.stats.blocks = 0;
        MetadataBatch {
            changes: vec![
                RecordChange::Delete(RecordKey::Dentry {
                    parent: 1,
                    name: "file".into(),
                }),
                RecordChange::Put(MetadataRecord::Dentry(DentryRecord {
                    parent: 1,
                    ordinal: 0,
                    entry: DirectoryEntry {
                        name: "renamed ".into(),
                        inode: 2,
                    },
                })),
                RecordChange::Put(MetadataRecord::Inode {
                    inode: 2,
                    value: inode,
                }),
                RecordChange::Delete(RecordKey::Extent {
                    inode: 2,
                    file_offset: 0,
                }),
            ],
        }
    }

    #[test]
    fn batch_is_atomic_and_rejects_duplicate_or_incomplete_changes() {
        let (source, _) = fixture();
        let before = serde_json::to_value(&source).unwrap();
        let batch = rename_and_truncate(&source);
        let candidate = batch.apply_to(Some(&source)).unwrap();
        assert_eq!(candidate.nodes[&2].stats.size, 0);
        let NodeData::Directory { entries } = &candidate.nodes[&1].data else {
            unreachable!()
        };
        assert_eq!(entries[0].name, "renamed ");
        let NodeData::File(layout) = &candidate.nodes[&2].data else {
            unreachable!()
        };
        assert!(layout.extents.is_empty());
        let mut incomplete = batch.clone();
        incomplete.changes.pop();
        assert!(incomplete.apply_to(Some(&source)).is_err());
        let mut duplicate = batch;
        duplicate.changes.push(duplicate.changes[0].clone());
        assert!(duplicate.apply_to(Some(&source)).is_err());
        assert_eq!(serde_json::to_value(source).unwrap(), before);
    }

    fn fixture() -> (Namespace, Vec<MetadataRecord>) {
        let namespace = super::super::tests::valid_namespace();
        let mut records = Vec::new();
        visit_records(&namespace, |record| {
            records.push(record);
            Ok(())
        })
        .unwrap();
        (namespace, records)
    }
    #[test]
    fn unordered_roundtrip_and_attribute_record_excludes_extents() {
        let (namespace, mut records) = fixture();
        records.reverse();
        let loaded = from_records(records).unwrap();
        assert_eq!(
            serde_json::to_value(namespace).unwrap(),
            serde_json::to_value(&loaded).unwrap()
        );
        let attrs = serde_json::to_value(InodeRecord::from_node(&loaded.nodes[&2])).unwrap();
        assert!(attrs.to_string().find("missing-is-okay").is_none());
        assert!(attrs.to_string().find("extents").is_none());
    }
    #[test]
    fn duplicates_missing_volume_and_wrong_parent_are_rejected() {
        let (_, records) = fixture();
        for record in &records {
            let mut duplicate = records.clone();
            duplicate.push(record.clone());
            assert!(from_records(duplicate).is_err());
        }
        let mut missing = records.clone();
        missing.retain(|record| !matches!(record, MetadataRecord::Volume(_)));
        assert!(from_records(missing).is_err());
        let mut malformed = records;
        for record in &mut malformed {
            if let MetadataRecord::Extent { inode, .. } = record {
                *inode = 1;
            }
        }
        assert!(from_records(malformed).is_err());
    }
    #[test]
    fn sparse_extents_hard_links_and_exact_names_survive_reordering() {
        let (mut namespace, _) = fixture();
        let file = namespace.nodes.get_mut(&2).unwrap();
        file.stats.nlink = 3;
        file.stats.size = 10;
        let NodeData::File(layout) = &mut file.data else {
            unreachable!()
        };
        layout.extents[0].file_offset = 4;
        layout.extents.push(BlockExtent {
            file_offset: 8,
            block: BlockId("other".into()),
            block_offset: 5,
            length: 2,
        });
        let NodeData::Directory { entries } = &mut namespace.nodes.get_mut(&1).unwrap().data else {
            unreachable!()
        };
        entries.push(DirectoryEntry {
            name: "file ".into(),
            inode: 2,
        });
        entries.push(DirectoryEntry {
            name: "File".into(),
            inode: 2,
        });
        let mut records = Vec::new();
        visit_records(&namespace, |record| {
            records.push(record);
            Ok(())
        })
        .unwrap();
        records.reverse();
        let loaded = from_records(records).unwrap();
        assert_eq!(
            serde_json::to_value(namespace).unwrap(),
            serde_json::to_value(loaded).unwrap()
        );
    }

    #[test]
    fn sink_failure_stops_streaming_import() {
        let (namespace, _) = fixture();
        let mut calls = 0;
        let error = visit_records(&namespace, |_| {
            calls += 1;
            Err(FsError::new(ErrorCode::Enospc))
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Enospc);
        assert_eq!(calls, 1);
    }
}
