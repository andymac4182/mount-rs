//! Namespace publications that store what changed and not the whole volume.
//!
//! The metadata row holds the namespace as one JSON document, so writing it
//! costs time proportional to the volume: every inode is encoded again and
//! SQLite writes every page of it again. A publication that follows the
//! previous one instead stores the inodes it changed, one row each, in
//! `mount_rs_namespace_nodes`, and the namespace's few scalar fields in the
//! single row of `mount_rs_namespace_changes`, which also carries the
//! revision. The current namespace is the stored document with those rows
//! laid over it.
//!
//! A row replaces the inode's earlier row, so the overlay never holds more
//! rows than inodes were touched, and a volume whose files are rewritten in
//! place never has to rewrite the document. Only removed inodes accumulate
//! (their rows stay, empty); the overlay is folded back into the document
//! when it has outgrown the namespace.
use mount_rs_core::chunking::ChunkerConfig;
use mount_rs_core::storage::{InodeId, Namespace, NodeMetadata};
use mount_rs_core::{Result, backend_error};
use std::sync::atomic::{AtomicPtr, Ordering};

/// The namespace this connection last stored or loaded, which the next
/// publication is compared with.
pub(crate) struct Published {
    pub revision: u64,
    pub namespace: Namespace,
    /// Size of the stored document.
    pub base_bytes: usize,
    /// `None` when the store holds the plain document. Otherwise an upper
    /// bound on the overlay's rows: each publication adds the inodes it
    /// changed, without knowing which already had a row.
    pub overlay: Option<usize>,
}

/// An overlay smaller than this is never folded.
const FOLD_AFTER_ROWS: usize = 4096;

impl Published {
    /// Whether a change is small enough beside the document to be worth
    /// storing as rows. A change that is a large part of the document saves
    /// little, and this keeps small volumes in the plain form.
    pub fn worth_rows(&self, change: &Change) -> bool {
        change.bytes.saturating_mul(4) <= self.base_bytes
    }

    /// Whether an overlay of `rows` rows still has room for `change`.
    pub fn has_room(rows: usize, change: &Change, after: &Namespace) -> bool {
        rows.saturating_add(change.nodes.len())
            <= after.nodes.len().saturating_mul(2).max(FOLD_AFTER_ROWS)
    }
}

/// One slot holding the `Published` copy without a lock. It is only used
/// while the connection mutex is held, so `take` and `put` never race; the
/// atomic is what lets the slot live beside the connection in shared state.
pub(crate) struct Slot(AtomicPtr<Published>);

impl Slot {
    pub fn new() -> Self {
        Self(AtomicPtr::new(std::ptr::null_mut()))
    }

    pub fn take(&self) -> Option<Published> {
        let pointer = self.0.swap(std::ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: a non-null pointer in the slot came from `Box::into_raw`
        // in `put`, and the swap above made this call its only owner.
        (!pointer.is_null()).then(|| *unsafe { Box::from_raw(pointer) })
    }

    pub fn put(&self, published: Published) {
        let pointer = Box::into_raw(Box::new(published));
        let displaced = self.0.swap(pointer, Ordering::AcqRel);
        if !displaced.is_null() {
            // SAFETY: as in `take`.
            drop(unsafe { Box::from_raw(displaced) });
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.take();
    }
}

/// The namespace's scalar fields, as stored in the overlay's header row.
type Header = (u32, InodeId, InodeId, u32, u32, u32, ChunkerConfig);

/// What one publication changed.
pub(crate) struct Change {
    pub header: String,
    /// Every changed inode with its new metadata, or `None` for an inode
    /// that was removed.
    pub nodes: Vec<(i64, Option<String>)>,
    /// Size of all of it.
    pub bytes: usize,
}

/// Encodes what turns `before` into `after`, or `None` when so much changed
/// that the whole document should be stored. The cost follows the number of
/// changed inodes when `after` was made from `before`.
pub(crate) fn encode(before: &Namespace, after: &Namespace) -> Result<Option<Change>> {
    let limit = (before.nodes.len() / 8).max(4);
    let mut changed = Vec::new();
    let complete = before.nodes.differences(&after.nodes, |inode, old, new| {
        // An inode that was only borrowed mutably is reported too.
        if old != new {
            if changed.len() == limit {
                return false;
            }
            changed.push(inode);
        }
        true
    });
    if !complete {
        return Ok(None);
    }
    let header = serde_json::to_string(&(
        after.format_version,
        after.root,
        after.next_inode,
        after.default_uid,
        after.default_gid,
        after.umask,
        &after.default_chunker,
    ))
    .map_err(backend_error)?;
    let mut bytes = header.len();
    let mut nodes = Vec::with_capacity(changed.len());
    for inode in changed {
        // SQLite keys are signed; an inode past that is stored the long way.
        let Ok(key) = i64::try_from(inode) else {
            return Ok(None);
        };
        let node = after
            .nodes
            .get(&inode)
            .map(serde_json::to_string)
            .transpose()
            .map_err(backend_error)?;
        bytes += 8 + node.as_ref().map_or(0, String::len);
        nodes.push((key, node));
    }
    Ok(Some(Change {
        header,
        nodes,
        bytes,
    }))
}

/// Applies the overlay's header row.
pub(crate) fn apply_header(namespace: &mut Namespace, header: &str) -> Result<()> {
    let (format_version, root, next_inode, default_uid, default_gid, umask, chunker): Header =
        serde_json::from_str(header).map_err(backend_error)?;
    namespace.format_version = format_version;
    namespace.root = root;
    namespace.next_inode = next_inode;
    namespace.default_uid = default_uid;
    namespace.default_gid = default_gid;
    namespace.umask = umask;
    namespace.default_chunker = chunker;
    Ok(())
}

/// Applies one overlay row.
pub(crate) fn apply_node(namespace: &mut Namespace, inode: i64, node: Option<&str>) -> Result<()> {
    let inode = InodeId::try_from(inode).map_err(backend_error)?;
    match node {
        Some(node) => {
            let node: NodeMetadata = serde_json::from_str(node).map_err(backend_error)?;
            namespace.nodes.insert(inode, node);
        }
        None => {
            namespace.nodes.remove(&inode);
        }
    }
    Ok(())
}
