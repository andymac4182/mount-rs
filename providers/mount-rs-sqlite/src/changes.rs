//! Namespace publications that store what changed and not the whole volume.
//!
//! The metadata row holds the namespace as one JSON document, so writing it
//! costs time proportional to the volume: every inode is encoded again and
//! SQLite writes every page of it again. A publication that follows the
//! previous one instead appends the inodes it changed to
//! `mount_rs_namespace_changes`, and the current namespace is the stored
//! document with those rows applied in revision order. The log is folded
//! back into the document once it has grown to the document's size, so a
//! fold's cost is spread over at least that many bytes of changes.
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
    /// Size and number of the change rows on top of it.
    pub pending_bytes: usize,
    pub pending_rows: usize,
}

/// A log smaller than this is never folded for its size alone.
const FOLD_AFTER_BYTES: usize = 64 * 1024;
const FOLD_AFTER_ROWS: usize = 4096;

impl Published {
    /// Whether a change record of `bytes` should be appended, as opposed to
    /// storing the whole document again.
    pub fn appends(&self, bytes: usize) -> bool {
        // A record that is a large part of the document saves little, and
        // keeps small volumes in the plain single-document form.
        bytes.saturating_mul(4) <= self.base_bytes
            && self.pending_rows < FOLD_AFTER_ROWS
            && self.pending_bytes.saturating_add(bytes) <= self.base_bytes.max(FOLD_AFTER_BYTES)
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

/// The stored form of one publication's changes: the namespace's scalar
/// fields, then every changed inode with its new metadata, or `null` for an
/// inode that was removed.
type Record = (
    u32,
    InodeId,
    InodeId,
    u32,
    u32,
    u32,
    ChunkerConfig,
    Vec<(InodeId, Option<NodeMetadata>)>,
);

/// Encodes what turns `before` into `after`, or `None` when so much changed
/// that the whole document should be stored. The cost follows the number of
/// changed inodes when `after` was made from `before`.
pub(crate) fn encode(before: &Namespace, after: &Namespace) -> Result<Option<String>> {
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
    let nodes: Vec<(InodeId, Option<&NodeMetadata>)> = changed
        .into_iter()
        .map(|inode| (inode, after.nodes.get(&inode)))
        .collect();
    serde_json::to_string(&(
        after.format_version,
        after.root,
        after.next_inode,
        after.default_uid,
        after.default_gid,
        after.umask,
        &after.default_chunker,
        nodes,
    ))
    .map(Some)
    .map_err(backend_error)
}

/// Applies one stored change record.
pub(crate) fn apply(namespace: &mut Namespace, record: &str) -> Result<()> {
    let (format_version, root, next_inode, default_uid, default_gid, umask, chunker, nodes): Record =
        serde_json::from_str(record).map_err(backend_error)?;
    namespace.format_version = format_version;
    namespace.root = root;
    namespace.next_inode = next_inode;
    namespace.default_uid = default_uid;
    namespace.default_gid = default_gid;
    namespace.umask = umask;
    namespace.default_chunker = chunker;
    for (inode, node) in nodes {
        match node {
            Some(node) => {
                namespace.nodes.insert(inode, node);
            }
            None => {
                namespace.nodes.remove(&inode);
            }
        }
    }
    Ok(())
}
