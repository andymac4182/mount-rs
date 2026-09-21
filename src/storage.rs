//! Independent metadata and immutable-byte storage contracts.
//!
//! Providers are scoped to a filesystem/volume by their constructors. These
//! contracts contain no database or cloud dependencies. Implementations and
//! the driver that composes them remain separate from this module.

use crate::chunking::{ChunkerConfig, from_config};
use crate::error::{ErrorCode, FsError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use crate::Result;
use crate::types::{S_IFBLK, S_IFCHR, S_IFDIR, S_IFIFO, S_IFLNK, S_IFMT, S_IFREG, S_IFSOCK, Stats};

pub type InodeId = u64;

/// Opaque identity in the selected block store, never a virtual filesystem path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockExtent {
    pub file_offset: u64,
    pub block: BlockId,
    pub block_offset: u64,
    pub length: u64,
}

/// Extents are nonempty, sorted, non-overlapping and contained in file length.
/// Gaps represent zero-filled sparse regions. Layout config must remain attached
/// to existing data even when the filesystem's default chunker changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLayout {
    pub chunker: ChunkerConfig,
    pub extents: Vec<BlockExtent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub inode: InodeId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NodeData {
    /// Preserve insertion order to match the reference memory driver.
    Directory {
        entries: Vec<DirectoryEntry>,
    },
    File(FileLayout),
    Symlink {
        target: String,
    },
    Special,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeMetadata {
    pub stats: Stats,
    pub data: NodeData,
}

/// Namespace/attributes and block references only: file bytes MUST NOT be
/// embedded here. Full namespace publication is an initial atomic transaction
/// boundary; providers may store these typed records in normalized tables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Namespace {
    pub format_version: u32,
    pub root: InodeId,
    pub next_inode: InodeId,
    pub default_uid: u32,
    pub default_gid: u32,
    pub umask: u32,
    pub default_chunker: ChunkerConfig,
    pub nodes: BTreeMap<InodeId, NodeMetadata>,
}

/// The only namespace representation currently understood by core.
pub const NAMESPACE_FORMAT_VERSION: u32 = 1;

impl Namespace {
    /// Validate namespace structure without consulting a block store.
    ///
    /// A namespace contains only metadata and block references, so this check
    /// intentionally does not fetch or otherwise trust referenced block data.
    /// Non-directory nodes with no directory entries may be retained as
    /// unlinked-open orphans, but only with `stats.nlink == 0`; directories
    /// must be rooted and referenced exactly once. This preserves a safe
    /// representation for an in-flight unlink without allowing unreachable
    /// directory subtrees or hard-linked directories.
    pub fn validate(&self) -> Result<()> {
        validate_format_version(self.format_version)?;
        from_config(&self.default_chunker)?;

        let max_inode = self
            .nodes
            .keys()
            .next_back()
            .copied()
            .ok_or_else(|| invalid_namespace("namespace must contain a root node"))?;
        if max_inode == u64::MAX {
            return Err(overflow_namespace(
                "namespace cannot allocate an inode after u64::MAX",
            ));
        }
        if self.next_inode <= max_inode {
            return Err(invalid_namespace(
                "next_inode must be greater than every inode",
            ));
        }

        if self.root == 0 {
            return Err(invalid_namespace("root inode must be nonzero"));
        }
        let root = self
            .nodes
            .get(&self.root)
            .ok_or_else(|| invalid_namespace("root inode is missing"))?;
        if root.stats.mode & S_IFMT != S_IFDIR {
            return Err(invalid_namespace("root inode must be a directory"));
        }

        for (&inode, node) in &self.nodes {
            if inode == 0 {
                return Err(invalid_namespace("inode keys must be nonzero"));
            }
            if node.stats.ino != inode {
                return Err(invalid_namespace("inode key and stats.ino disagree"));
            }
            validate_node_kind(node)?;
        }

        let mut references = BTreeMap::<InodeId, u64>::new();
        let mut directory_references = BTreeMap::<InodeId, u64>::new();
        let mut directory_children = BTreeMap::<InodeId, u64>::new();
        for (&parent, node) in &self.nodes {
            let NodeData::Directory { entries } = &node.data else {
                continue;
            };
            let mut names = BTreeSet::new();
            let mut child_directories = 0_u64;
            for entry in entries {
                validate_entry_name(&entry.name)?;
                if !names.insert(entry.name.as_str()) {
                    return Err(invalid_namespace("directory entry names must be unique"));
                }
                let child = self.nodes.get(&entry.inode).ok_or_else(|| {
                    invalid_namespace("directory entry references a missing inode")
                })?;
                increment_count(&mut references, entry.inode, "inode reference count")?;
                if matches!(child.data, NodeData::Directory { .. }) {
                    child_directories = child_directories
                        .checked_add(1)
                        .ok_or_else(|| overflow_namespace("directory child count overflow"))?;
                    increment_count(
                        &mut directory_references,
                        entry.inode,
                        "directory parent count",
                    )?;
                }
            }
            directory_children.insert(parent, child_directories);
        }

        validate_directory_links(self, &directory_references, &directory_children)?;
        validate_node_links(self, &references)?;
        Ok(())
    }
}

impl LoadedMetadata {
    /// Validate data returned by a metadata provider before using it.
    ///
    /// `None` is the valid uninitialized state. Malformed namespaces return
    /// `EINVAL`; a future format returns `ENOTSUP`, so callers fail closed
    /// instead of silently interpreting unknown metadata.
    pub fn validate(&self) -> Result<()> {
        if (self.revision == 0) != self.namespace.is_none() {
            return Err(invalid_namespace(
                "metadata revision and initialization state disagree",
            ));
        }
        self.namespace.as_ref().map_or(Ok(()), Namespace::validate)
    }
}

fn validate_format_version(version: u32) -> Result<()> {
    if version > NAMESPACE_FORMAT_VERSION {
        return Err(FsError::new(ErrorCode::Enotsup)
            .with_message("namespace format version is newer than this core"));
    }
    if version != NAMESPACE_FORMAT_VERSION {
        return Err(invalid_namespace("unsupported namespace format version"));
    }
    Ok(())
}

fn validate_node_kind(node: &NodeMetadata) -> Result<()> {
    let mode_type = node.stats.mode & S_IFMT;
    let valid = match &node.data {
        NodeData::Directory { .. } => mode_type == S_IFDIR,
        NodeData::File(layout) => {
            if mode_type == S_IFREG {
                validate_file_layout(layout, node.stats.size)?;
                true
            } else {
                false
            }
        }
        NodeData::Symlink { .. } => mode_type == S_IFLNK,
        NodeData::Special => matches!(mode_type, S_IFBLK | S_IFCHR | S_IFIFO | S_IFSOCK),
    };
    if valid {
        Ok(())
    } else {
        Err(invalid_namespace(
            "node metadata kind does not match stats.mode",
        ))
    }
}

fn validate_file_layout(layout: &FileLayout, file_size: u64) -> Result<()> {
    // An empty list represents an empty or entirely sparse/zero-filled file.
    // Every stored extent, however, must be a nonempty bounded range.
    from_config(&layout.chunker)?;
    let mut previous_end = None;
    for extent in &layout.extents {
        if extent.length == 0 {
            return Err(invalid_namespace("file extents must be nonempty"));
        }
        if extent.block.0.is_empty() {
            return Err(invalid_namespace("file extents must name a block"));
        }
        let file_end = extent
            .file_offset
            .checked_add(extent.length)
            .ok_or_else(|| overflow_namespace("file extent offset overflows"))?;
        extent
            .block_offset
            .checked_add(extent.length)
            .ok_or_else(|| overflow_namespace("block extent offset overflows"))?;
        if file_end > file_size {
            return Err(invalid_namespace("file extent exceeds file size"));
        }
        if previous_end.is_some_and(|end| extent.file_offset < end) {
            return Err(invalid_namespace(
                "file extents must be sorted and non-overlapping",
            ));
        }
        previous_end = Some(file_end);
    }
    Ok(())
}

fn validate_entry_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        return Err(invalid_namespace("invalid directory entry name"));
    }
    Ok(())
}

fn validate_directory_links(
    namespace: &Namespace,
    directory_references: &BTreeMap<InodeId, u64>,
    directory_children: &BTreeMap<InodeId, u64>,
) -> Result<()> {
    for (&inode, node) in &namespace.nodes {
        if !matches!(node.data, NodeData::Directory { .. }) {
            continue;
        }
        let parent_count = directory_references.get(&inode).copied().unwrap_or(0);
        if inode == namespace.root {
            if parent_count != 0 {
                return Err(invalid_namespace(
                    "root directory cannot be linked as a child",
                ));
            }
        } else if parent_count != 1 {
            return Err(invalid_namespace(
                "non-root directories must have exactly one parent",
            ));
        }
        let child_count = directory_children.get(&inode).copied().unwrap_or(0);
        let expected_nlink = 2_u64
            .checked_add(child_count)
            .ok_or_else(|| overflow_namespace("directory nlink overflows"))?;
        if node.stats.nlink != expected_nlink {
            return Err(invalid_namespace("directory nlink is inconsistent"));
        }
    }

    // Iterative traversal both avoids recursion depth limits and makes a
    // repeated directory an explicit cycle/hard-link rejection.
    let mut visited = BTreeSet::new();
    let mut pending = vec![namespace.root];
    while let Some(inode) = pending.pop() {
        if !visited.insert(inode) {
            return Err(invalid_namespace(
                "directory graph contains a cycle or hard-linked directory",
            ));
        }
        let node = namespace
            .nodes
            .get(&inode)
            .ok_or_else(|| invalid_namespace("directory traversal reached missing inode"))?;
        if let NodeData::Directory { entries } = &node.data {
            for entry in entries {
                if matches!(
                    namespace.nodes[&entry.inode].data,
                    NodeData::Directory { .. }
                ) {
                    pending.push(entry.inode);
                }
            }
        }
    }
    let directory_count = namespace
        .nodes
        .values()
        .filter(|node| matches!(node.data, NodeData::Directory { .. }))
        .count();
    if visited.len() != directory_count {
        return Err(invalid_namespace("directory is unreachable from root"));
    }
    Ok(())
}

fn validate_node_links(namespace: &Namespace, references: &BTreeMap<InodeId, u64>) -> Result<()> {
    for (&inode, node) in &namespace.nodes {
        if matches!(node.data, NodeData::Directory { .. }) {
            continue;
        }
        let reference_count = references.get(&inode).copied().unwrap_or(0);
        if reference_count == 0 {
            if node.stats.nlink != 0 {
                return Err(invalid_namespace("unlinked orphan must have zero nlink"));
            }
        } else if node.stats.nlink != reference_count {
            return Err(invalid_namespace("file nlink is inconsistent"));
        }
    }
    Ok(())
}

fn increment_count(
    counts: &mut BTreeMap<InodeId, u64>,
    inode: InodeId,
    what: &'static str,
) -> Result<()> {
    let count = counts.entry(inode).or_insert(0);
    *count = count
        .checked_add(1)
        .ok_or_else(|| overflow_namespace(what))?;
    Ok(())
}

fn invalid_namespace(message: &'static str) -> FsError {
    FsError::new(ErrorCode::Einval).with_message(message)
}

fn overflow_namespace(message: &'static str) -> FsError {
    FsError::new(ErrorCode::Eoverflow).with_message(message)
}

#[derive(Debug, Clone)]
pub struct LoadedMetadata {
    /// Zero denotes an uninitialized namespace. Publication increments this
    /// monotonically; providers must reject overflow, never reuse a revision.
    pub revision: u64,
    pub namespace: Option<Namespace>,
}

/// Result of an explicit block-store reconciliation pass.
///
/// Reconciliation is deliberately separate from filesystem shutdown. A
/// coordinator must hold the provider lease, derive all authoritative roots,
/// and supply a grace period long enough to protect blocks from an in-flight
/// or ambiguous publication in another process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BlockReconcileReport {
    pub scanned: u64,
    pub protected: u64,
    pub recent: u64,
    pub deleted: u64,
}

/// A provider-enforced, volume-wide single-writer lease. Provider transactions
/// must validate the fence AND expiry, including during renewal/publication.
/// Acquiring after expiry increments the persisted fence; stale owners must
/// never publish even if they resume after a long pause. Time is determined
/// by the provider, not supplied by a requesting client's wall clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterLease {
    pub owner: String,
    pub fence: u64,
    pub expires_at_ms: u64,
}

#[async_trait]
pub trait MetadataStore: Send + Sync {
    /// False for volatile stores; never advertise durable commits for memfs.
    fn durable(&self) -> bool;
    async fn load(&self) -> Result<LoadedMetadata>;
    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease>;
    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease>;
    async fn release_writer(&self, lease: &WriterLease) -> Result<()>;
    /// Atomically validate lease/revision and publish all namespace changes.
    /// Revision conflicts return EAGAIN; stale/expired fencing must fail closed.
    /// The driver must flush new blocks BEFORE publishing references to them.
    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64>;
    /// Complete the store's durability barrier; errors must reach fsync callers.
    async fn flush(&self) -> Result<()>;
}

#[async_trait]
pub trait BlockStore: Send + Sync {
    fn durable(&self) -> bool;
    /// Store immutable bytes; an existing identity may only name identical
    /// bytes. No caller can overwrite data referenced by an older layout.
    async fn put(&self, bytes: &[u8]) -> Result<BlockId>;
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>>;
    /// Barrier covering prior successful puts before metadata publication.
    async fn flush(&self) -> Result<()>;
    /// Only the coordinator may reclaim blocks proven unreachable from every
    /// live/persisted layout and in-flight write. This is not implicit on close.
    async fn delete(&self, id: &BlockId) -> Result<()>;

    /// Reconcile provider-owned immutable blocks against authoritative live
    /// roots. Providers that cannot enumerate their scoped objects must fail
    /// closed with `ENOTSUP`; callers must not infer cleanup from a successful
    /// filesystem shutdown. The grace period protects newly uploaded blocks
    /// whose metadata publication outcome is still ambiguous.
    async fn reconcile(
        &self,
        _live: &BTreeSet<BlockId>,
        _grace: Duration,
    ) -> Result<BlockReconcileReport> {
        Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("reconcile blocks")
            .with_message("block provider does not expose scoped reconciliation"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunking::ChunkerConfig;
    use crate::types::{S_IFDIR, S_IFREG};

    fn fixed_chunker(size: u64) -> ChunkerConfig {
        ChunkerConfig {
            algorithm: "fixed-size".to_owned(),
            version: 1,
            parameters: BTreeMap::from([("chunk_size".to_owned(), size)]),
        }
    }

    fn stats(ino: u64, mode: u32, nlink: u64, size: u64) -> Stats {
        Stats {
            dev: 1,
            ino,
            mode,
            nlink,
            uid: 1000,
            gid: 1000,
            rdev: 0,
            size,
            blksize: 4096,
            blocks: size.div_ceil(512),
            atime_ms: 1,
            mtime_ms: 1,
            ctime_ms: 1,
            birthtime_ms: 1,
        }
    }

    fn file_layout(extents: Vec<BlockExtent>) -> FileLayout {
        FileLayout {
            chunker: fixed_chunker(4096),
            extents,
        }
    }

    fn valid_namespace() -> Namespace {
        Namespace {
            format_version: NAMESPACE_FORMAT_VERSION,
            root: 1,
            next_inode: 3,
            default_uid: 1000,
            default_gid: 1000,
            umask: 0o022,
            default_chunker: fixed_chunker(4096),
            nodes: BTreeMap::from([
                (
                    1,
                    NodeMetadata {
                        stats: stats(1, S_IFDIR | 0o755, 2, 0),
                        data: NodeData::Directory {
                            entries: vec![DirectoryEntry {
                                name: "file".to_owned(),
                                inode: 2,
                            }],
                        },
                    },
                ),
                (
                    2,
                    NodeMetadata {
                        stats: stats(2, S_IFREG | 0o644, 1, 3),
                        data: NodeData::File(file_layout(vec![BlockExtent {
                            file_offset: 0,
                            block: BlockId("missing-is-okay".to_owned()),
                            block_offset: 0,
                            length: 3,
                        }])),
                    },
                ),
            ]),
        }
    }

    fn assert_error_code<T>(result: Result<T>, expected: ErrorCode) {
        match result {
            Ok(_) => panic!("expected {expected:?}"),
            Err(error) => assert_eq!(error.code, expected),
        }
    }

    #[test]
    fn accepts_valid_namespace_hardlinked_files_and_unlinked_orphans() {
        let mut namespace = valid_namespace();
        let root = namespace.nodes.get_mut(&1).unwrap();
        if let NodeData::Directory { entries } = &mut root.data {
            entries.push(DirectoryEntry {
                name: "alias".to_owned(),
                inode: 2,
            });
        }
        namespace.nodes.get_mut(&1).unwrap().stats.nlink = 2;
        namespace.nodes.get_mut(&2).unwrap().stats.nlink = 2;
        namespace.next_inode = 4;
        namespace.nodes.insert(
            3,
            NodeMetadata {
                stats: stats(3, S_IFREG | 0o600, 0, 0),
                data: NodeData::File(file_layout(Vec::new())),
            },
        );
        assert!(namespace.validate().is_ok());
    }

    #[test]
    fn loaded_metadata_validation_fails_closed_and_preserves_future_version_signal() {
        assert_error_code(
            LoadedMetadata {
                revision: 1,
                namespace: None,
            }
            .validate(),
            ErrorCode::Einval,
        );
        assert_error_code(
            LoadedMetadata {
                revision: 0,
                namespace: Some(valid_namespace()),
            }
            .validate(),
            ErrorCode::Einval,
        );
        assert!(
            LoadedMetadata {
                revision: 0,
                namespace: None,
            }
            .validate()
            .is_ok()
        );

        let mut malformed = valid_namespace();
        malformed.root = 99;
        assert_error_code(
            (LoadedMetadata {
                revision: 1,
                namespace: Some(malformed),
            })
            .validate(),
            ErrorCode::Einval,
        );

        let mut future = valid_namespace();
        future.format_version = NAMESPACE_FORMAT_VERSION + 1;
        assert_error_code(
            (LoadedMetadata {
                revision: 1,
                namespace: Some(future),
            })
            .validate(),
            ErrorCode::Enotsup,
        );
    }

    #[test]
    fn rejects_versions_root_and_inode_kind_mismatches() {
        let mut future = valid_namespace();
        future.format_version = NAMESPACE_FORMAT_VERSION + 1;
        assert_error_code(future.validate(), ErrorCode::Enotsup);

        let mut legacy = valid_namespace();
        legacy.format_version = 0;
        assert_error_code(legacy.validate(), ErrorCode::Einval);

        let mut missing_root = valid_namespace();
        missing_root.root = 99;
        assert_error_code(missing_root.validate(), ErrorCode::Einval);

        let mut non_directory_root = valid_namespace();
        non_directory_root.nodes.get_mut(&1).unwrap().stats.mode = S_IFREG | 0o755;
        assert_error_code(non_directory_root.validate(), ErrorCode::Einval);

        let mut wrong_inode = valid_namespace();
        wrong_inode.nodes.get_mut(&2).unwrap().stats.ino = 99;
        assert_error_code(wrong_inode.validate(), ErrorCode::Einval);

        let mut wrong_kind = valid_namespace();
        wrong_kind.nodes.get_mut(&2).unwrap().stats.mode = S_IFDIR | 0o644;
        assert_error_code(wrong_kind.validate(), ErrorCode::Einval);
    }

    #[test]
    fn rejects_invalid_names_missing_references_and_directory_graphs() {
        for name in ["", ".", "..", "a/b", "a\0b"] {
            let mut namespace = valid_namespace();
            if let NodeData::Directory { entries } = &mut namespace.nodes.get_mut(&1).unwrap().data
            {
                entries[0].name = name.to_owned();
            }
            assert_error_code(namespace.validate(), ErrorCode::Einval);
        }

        let mut duplicate = valid_namespace();
        if let NodeData::Directory { entries } = &mut duplicate.nodes.get_mut(&1).unwrap().data {
            entries.push(DirectoryEntry {
                name: "file".to_owned(),
                inode: 2,
            });
        }
        assert_error_code(duplicate.validate(), ErrorCode::Einval);

        let mut missing = valid_namespace();
        if let NodeData::Directory { entries } = &mut missing.nodes.get_mut(&1).unwrap().data {
            entries[0].inode = 99;
        }
        assert_error_code(missing.validate(), ErrorCode::Einval);

        let mut hard_linked_directory = valid_namespace();
        hard_linked_directory.nodes.insert(
            3,
            NodeMetadata {
                stats: stats(3, S_IFDIR | 0o755, 2, 0),
                data: NodeData::Directory { entries: vec![] },
            },
        );
        hard_linked_directory.next_inode = 4;
        if let NodeData::Directory { entries } =
            &mut hard_linked_directory.nodes.get_mut(&1).unwrap().data
        {
            entries.push(DirectoryEntry {
                name: "dir-a".to_owned(),
                inode: 3,
            });
            entries.push(DirectoryEntry {
                name: "dir-b".to_owned(),
                inode: 3,
            });
        }
        hard_linked_directory.nodes.get_mut(&1).unwrap().stats.nlink = 4;
        assert_error_code(hard_linked_directory.validate(), ErrorCode::Einval);

        let mut cycle = valid_namespace();
        cycle.nodes.insert(
            3,
            NodeMetadata {
                stats: stats(3, S_IFDIR | 0o755, 2, 0),
                data: NodeData::Directory {
                    entries: vec![DirectoryEntry {
                        name: "back".to_owned(),
                        inode: 1,
                    }],
                },
            },
        );
        cycle.next_inode = 4;
        if let NodeData::Directory { entries } = &mut cycle.nodes.get_mut(&1).unwrap().data {
            entries.push(DirectoryEntry {
                name: "cycle".to_owned(),
                inode: 3,
            });
        }
        cycle.nodes.get_mut(&1).unwrap().stats.nlink = 3;
        assert_error_code(cycle.validate(), ErrorCode::Einval);
    }

    #[test]
    fn validates_directory_nlinks_and_orphan_policy() {
        let mut child = valid_namespace();
        child.nodes.insert(
            3,
            NodeMetadata {
                stats: stats(3, S_IFDIR | 0o755, 2, 0),
                data: NodeData::Directory { entries: vec![] },
            },
        );
        child.next_inode = 4;
        if let NodeData::Directory { entries } = &mut child.nodes.get_mut(&1).unwrap().data {
            entries.push(DirectoryEntry {
                name: "child".to_owned(),
                inode: 3,
            });
        }
        child.nodes.get_mut(&1).unwrap().stats.nlink = 3;
        assert!(child.validate().is_ok());

        child.nodes.get_mut(&3).unwrap().stats.nlink = 1;
        assert_error_code(child.validate(), ErrorCode::Einval);

        let mut orphan = valid_namespace();
        orphan.next_inode = 4;
        orphan.nodes.insert(
            3,
            NodeMetadata {
                stats: stats(3, S_IFREG | 0o600, 1, 0),
                data: NodeData::File(file_layout(Vec::new())),
            },
        );
        assert_error_code(orphan.validate(), ErrorCode::Einval);
    }

    #[test]
    fn validates_chunkers_and_extent_ranges_without_fetching_blocks() {
        let mut empty = valid_namespace();
        let file = empty.nodes.get_mut(&2).unwrap();
        file.stats.size = 0;
        file.stats.blocks = 0;
        file.data = NodeData::File(file_layout(Vec::new()));
        assert!(empty.validate().is_ok());

        let mut zero_chunker = valid_namespace();
        zero_chunker
            .default_chunker
            .parameters
            .insert("chunk_size".into(), 0);
        assert_error_code(zero_chunker.validate(), ErrorCode::Einval);

        let mut future_chunker = valid_namespace();
        future_chunker.nodes.get_mut(&2).unwrap().data = NodeData::File(FileLayout {
            chunker: ChunkerConfig {
                algorithm: "fixed-size".into(),
                version: 2,
                parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
            },
            extents: vec![],
        });
        assert_error_code(future_chunker.validate(), ErrorCode::Enotsup);

        let cases = [
            (
                vec![BlockExtent {
                    file_offset: 0,
                    block: BlockId("b".into()),
                    block_offset: 0,
                    length: 0,
                }],
                3,
                ErrorCode::Einval,
            ),
            (
                vec![
                    BlockExtent {
                        file_offset: 0,
                        block: BlockId("a".into()),
                        block_offset: 0,
                        length: 2,
                    },
                    BlockExtent {
                        file_offset: 1,
                        block: BlockId("b".into()),
                        block_offset: 0,
                        length: 1,
                    },
                ],
                3,
                ErrorCode::Einval,
            ),
            (
                vec![BlockExtent {
                    file_offset: 2,
                    block: BlockId("b".into()),
                    block_offset: 0,
                    length: 2,
                }],
                3,
                ErrorCode::Einval,
            ),
            (
                vec![BlockExtent {
                    file_offset: u64::MAX,
                    block: BlockId("b".into()),
                    block_offset: 0,
                    length: 1,
                }],
                u64::MAX,
                ErrorCode::Eoverflow,
            ),
            (
                vec![BlockExtent {
                    file_offset: 0,
                    block: BlockId("b".into()),
                    block_offset: u64::MAX,
                    length: 1,
                }],
                1,
                ErrorCode::Eoverflow,
            ),
        ];
        for (extents, size, error) in cases {
            let mut namespace = valid_namespace();
            let file = namespace.nodes.get_mut(&2).unwrap();
            file.stats.size = size;
            file.data = NodeData::File(file_layout(extents));
            assert_error_code(namespace.validate(), error);
        }
    }

    #[test]
    fn rejects_next_inode_collisions_and_inode_space_exhaustion() {
        let mut collision = valid_namespace();
        collision.next_inode = 2;
        assert_error_code(collision.validate(), ErrorCode::Einval);

        let mut exhausted = valid_namespace();
        exhausted.nodes.insert(
            u64::MAX,
            NodeMetadata {
                stats: stats(u64::MAX, S_IFREG | 0o600, 0, 0),
                data: NodeData::File(file_layout(Vec::new())),
            },
        );
        exhausted.next_inode = u64::MAX;
        assert_error_code(exhausted.validate(), ErrorCode::Eoverflow);
    }
}
