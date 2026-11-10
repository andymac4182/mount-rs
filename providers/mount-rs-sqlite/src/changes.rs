//! Namespace publications that store what changed and not the whole volume.
//!
//! The metadata row holds the namespace as one JSON document, so writing it
//! costs time proportional to the volume: every inode is encoded again and
//! SQLite writes every page of it again. A publication that follows the
//! previous one instead appends one row to `mount_rs_namespace_changes`
//! with the inodes it changed, and the current namespace is the stored
//! document with the rows applied in revision order.
//!
//! The rows are kept from growing without ever rewriting the document, the
//! way a log-structured store cleans its segments. An entry is garbage once
//! a newer row sets the same inode. A row that is all garbage is deleted. A
//! row that is at least half garbage is cleaned: the next publication
//! stores the current state of its remaining inodes again, which makes the
//! rest of it garbage too. So the rows hold at most about twice what the
//! changed inodes need, a publication writes at most about twice what it
//! changed, and an inode that is large and never changes is written again
//! only after as much has been written around it.
use mount_rs_core::chunking::ChunkerConfig;
use mount_rs_core::storage::{InodeId, Namespace, NodeMetadata};
use mount_rs_core::{Result, backend_error};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicPtr, Ordering};

/// The namespace this connection last stored or loaded, which the next
/// publication is compared with.
pub(crate) struct Published {
    pub revision: u64,
    pub namespace: Namespace,
    /// The revision at which the stored document was written.
    pub base_revision: u64,
    /// Size of the stored document.
    pub base_bytes: usize,
    /// The rows on top of it.
    pub log: Log,
}

/// Rows a log may hold beyond what its size relative to the document allows.
const LOG_SLACK_BYTES: usize = 1024 * 1024;
/// Dead rows left in the store until there are this many to delete at once:
/// deleting one with every publication would add its pages to every commit.
const DEAD_ROWS: usize = 16;
/// Inodes one publication stores again for cleaning.
const CLEAN_INODES: usize = 64;
/// What a publication may clean however little it changed.
const CLEAN_FLOOR_BYTES: usize = 512;

impl Published {
    /// Whether a row of `bytes` should be appended, as opposed to storing
    /// the whole document again.
    pub fn appends(&self, bytes: usize) -> bool {
        // A row that is a large part of the document saves little, and this
        // keeps small volumes in the plain single-document form. The second
        // bound is a backstop: cleaning keeps the rows near twice the
        // inodes they hold, which is at most twice the volume.
        bytes.saturating_mul(4) <= self.base_bytes
            && self.log.bytes.saturating_add(bytes)
                <= self
                    .base_bytes
                    .saturating_mul(8)
                    .saturating_add(LOG_SLACK_BYTES)
    }
}

/// The newest entry for an inode.
struct Newest {
    revision: u64,
    bytes: usize,
    removed: bool,
}

struct Row {
    inodes: Vec<InodeId>,
    /// Inodes for which this row is the newest, and the size of their
    /// entries.
    live: usize,
    live_bytes: usize,
    /// The size of all its entries.
    entry_bytes: usize,
    /// The size of the row.
    bytes: usize,
}

impl Row {
    fn ripe(&self) -> bool {
        self.live > 0 && self.live_bytes.saturating_mul(2) <= self.entry_bytes
    }
}

/// What the rows in the store hold, without their contents: which row is
/// the newest for each inode, and so what is garbage.
pub(crate) struct Log {
    rows: BTreeMap<u64, Row>,
    newest: HashMap<InodeId, Newest>,
    /// How many rows in the store mention an inode.
    mentions: HashMap<InodeId, u32>,
    /// Rows that are all garbage and still in the store.
    dead: Vec<u64>,
    /// Rows to clean, oldest first.
    ripe: BTreeSet<u64>,
    /// Bytes that may be stored again for cleaning. Publications earn what
    /// they change, so cleaning at most doubles what is written, and a row
    /// too large for one publication's share waits for more.
    credit: usize,
    /// The size of the rows in the store.
    pub bytes: usize,
    /// The document's `next_inode`: an inode from here on is not in the
    /// document.
    base_next_inode: InodeId,
}

/// One inode as a row holds it.
pub(crate) struct Entry {
    inode: InodeId,
    bytes: usize,
    removed: bool,
}

/// A row ready to store.
pub(crate) struct Append {
    pub row: String,
    /// Dead rows to delete with it.
    pub dead: Vec<u64>,
    entries: Vec<Entry>,
    credit: usize,
}

impl Log {
    pub fn new(document: &Namespace) -> Self {
        Self {
            rows: BTreeMap::new(),
            newest: HashMap::new(),
            mentions: HashMap::new(),
            dead: Vec::new(),
            ripe: BTreeSet::new(),
            credit: 0,
            bytes: 0,
            base_next_inode: document.next_inode,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// An entry of `row` stopped being the newest for its inode.
    fn supersede(&mut self, row: u64, bytes: usize, last: u64) {
        let Some(found) = self.rows.get_mut(&row) else {
            return;
        };
        found.live -= 1;
        found.live_bytes -= bytes;
        if found.live > 0 {
            if found.ripe() {
                self.ripe.insert(row);
            }
            return;
        }
        self.ripe.remove(&row);
        // The last row is kept while it is the last, for the scalar fields
        // and the revision it carries.
        if row != last {
            self.dead.push(row);
        }
    }

    /// Forgets the entry that keeps `inode` removed if nothing needs it.
    ///
    /// A removed inode keeps an empty entry so that it stays removed over
    /// whatever older rows and the document say. An inode the document
    /// never had, which no other row in the store mentions, is absent
    /// without it.
    fn forget_removed(&mut self, inode: InodeId, last: u64) {
        if inode < self.base_next_inode || self.mentions.get(&inode) != Some(&1) {
            return;
        }
        if let Some(newest) = self.newest.get(&inode)
            && newest.removed
        {
            let (row, bytes) = (newest.revision, newest.bytes);
            self.newest.remove(&inode);
            self.supersede(row, bytes, last);
        }
    }

    /// Notes a row that is in the store.
    fn record(&mut self, revision: u64, entries: Vec<Entry>, bytes: usize) {
        if let Some((&last, row)) = self.rows.last_key_value()
            && row.live == 0
            && !self.dead.contains(&last)
        {
            self.dead.push(last);
        }
        let mut row = Row {
            inodes: Vec::with_capacity(entries.len()),
            live: entries.len(),
            live_bytes: 0,
            entry_bytes: 0,
            bytes,
        };
        for entry in &entries {
            row.inodes.push(entry.inode);
            row.live_bytes += entry.bytes;
            row.entry_bytes += entry.bytes;
        }
        self.bytes += bytes;
        self.rows.insert(revision, row);
        for entry in entries {
            *self.mentions.entry(entry.inode).or_default() += 1;
            let old = self.newest.insert(
                entry.inode,
                Newest {
                    revision,
                    bytes: entry.bytes,
                    removed: entry.removed,
                },
            );
            if let Some(old) = old {
                self.supersede(old.revision, old.bytes, revision);
            }
            if entry.removed {
                self.forget_removed(entry.inode, revision);
            }
        }
    }

    /// The first `count` dead rows are out of the store.
    fn deleted(&mut self, count: usize) {
        let last = self.rows.last_key_value().map_or(0, |(last, _)| *last);
        let gone: Vec<u64> = self.dead.drain(..count).collect();
        for revision in gone {
            let Some(row) = self.rows.remove(&revision) else {
                continue;
            };
            self.bytes -= row.bytes;
            for inode in row.inodes {
                if let Some(mentions) = self.mentions.get_mut(&inode) {
                    *mentions -= 1;
                    if *mentions == 0 {
                        self.mentions.remove(&inode);
                    } else {
                        self.forget_removed(inode, last);
                    }
                }
            }
        }
    }

    /// Chooses the inodes a publication that changes `changed`, in
    /// `changed_bytes`, stores again to clean the oldest ripe rows, and
    /// what credit is left.
    fn clean(&self, changed: &[InodeId], changed_bytes: usize) -> (Vec<InodeId>, usize) {
        let mut credit = self
            .credit
            .saturating_add(changed_bytes.max(CLEAN_FLOOR_BYTES))
            .min(self.bytes);
        let mut cleaned = Vec::new();
        for revision in &self.ripe {
            let Some(row) = self.rows.get(revision) else {
                continue;
            };
            // The oldest waits for credit, and the ones after it wait for it.
            if row.live_bytes > credit || cleaned.len() + row.live > CLEAN_INODES {
                break;
            }
            credit -= row.live_bytes;
            cleaned.extend(row.inodes.iter().copied().filter(|inode| {
                self.newest
                    .get(inode)
                    .is_some_and(|newest| newest.revision == *revision)
                    && !changed.contains(inode)
            }));
        }
        (cleaned, credit)
    }

    /// The row was stored, and `append.dead` deleted: account for it.
    pub fn stored(&mut self, revision: u64, append: Append) {
        self.deleted(append.dead.len());
        self.record(revision, append.entries, append.row.len());
        self.credit = append.credit;
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

/// The stored form of one publication: the namespace's scalar fields, then
/// every inode it sets with its metadata, or `null` for an inode that was
/// removed.
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

/// Counts what serializing would write.
struct Count(usize);

impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Encodes the row that turns `before` into `after`, or `None` when so much
/// changed that the whole document should be stored. The cost follows the
/// number of changed inodes when `after` was made from `before`.
pub(crate) fn encode(before: &Published, after: &Namespace) -> Result<Option<Append>> {
    let limit = (before.namespace.nodes.len() / 8).max(4);
    let mut changed = Vec::new();
    let complete = before
        .namespace
        .nodes
        .differences(&after.nodes, |inode, old, new| {
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
    // The row is the scalar fields and then the entries, each encoded on
    // its own so its size is known.
    let mut row = serde_json::to_string(&(
        after.format_version,
        after.root,
        after.next_inode,
        after.default_uid,
        after.default_gid,
        after.umask,
        &after.default_chunker,
    ))
    .map_err(backend_error)?;
    row.pop();
    row.push_str(",[");
    let mut entries = Vec::with_capacity(changed.len());
    let mut add = |row: &mut String, inode: InodeId| -> Result<usize> {
        let node = after.nodes.get(&inode);
        let entry = serde_json::to_string(&(inode, node)).map_err(backend_error)?;
        if !entries.is_empty() {
            row.push(',');
        }
        row.push_str(&entry);
        entries.push(Entry {
            inode,
            bytes: entry.len(),
            removed: node.is_none(),
        });
        Ok(entry.len())
    };
    let mut changed_bytes = 0;
    for inode in &changed {
        changed_bytes += add(&mut row, *inode)?;
    }
    let (cleaned, credit) = before.log.clean(&changed, changed_bytes);
    for inode in cleaned {
        add(&mut row, inode)?;
    }
    row.push_str("]]");
    let dead = if before.log.dead.len() >= DEAD_ROWS {
        before.log.dead.clone()
    } else {
        Vec::new()
    };
    Ok(Some(Append {
        row,
        dead,
        entries,
        credit,
    }))
}

/// Applies one stored row, and notes it in `log`.
pub(crate) fn apply(
    namespace: &mut Namespace,
    log: &mut Log,
    revision: u64,
    record: &str,
) -> Result<()> {
    let (format_version, root, next_inode, default_uid, default_gid, umask, chunker, nodes): Record =
        serde_json::from_str(record).map_err(backend_error)?;
    namespace.format_version = format_version;
    namespace.root = root;
    namespace.next_inode = next_inode;
    namespace.default_uid = default_uid;
    namespace.default_gid = default_gid;
    namespace.umask = umask;
    namespace.default_chunker = chunker;
    let mut entries = Vec::with_capacity(nodes.len());
    for (inode, node) in nodes {
        let mut count = Count(0);
        serde_json::to_writer(&mut count, &(inode, &node)).map_err(backend_error)?;
        entries.push(Entry {
            inode,
            bytes: count.0,
            removed: node.is_none(),
        });
        match node {
            Some(node) => {
                namespace.nodes.insert(inode, node);
            }
            None => {
                namespace.nodes.remove(&inode);
            }
        }
    }
    log.record(revision, entries, record.len());
    Ok(())
}
