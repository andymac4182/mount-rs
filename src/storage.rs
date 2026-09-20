//! Independent metadata and immutable-byte storage contracts.
//!
//! Providers are scoped to a filesystem/volume by their constructors. These
//! contracts contain no database or cloud dependencies. Implementations and
//! the driver that composes them remain separate from this module.

use crate::{Result, Stats, chunking::ChunkerConfig};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

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

#[derive(Debug, Clone)]
pub struct LoadedMetadata {
    /// Zero denotes an uninitialized namespace. Publication increments this
    /// monotonically; providers must reject overflow, never reuse a revision.
    pub revision: u64,
    pub namespace: Option<Namespace>,
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
}
