//! Filesystem semantics over a flat asynchronous key-value store.
//!
//! This is the Rust counterpart of mountx's `unstorage` driver. The store
//! remains deliberately small: it owns raw bytes, point existence, listing,
//! mutation, and optional native metadata. The adapter supplies the tree,
//! handles, buffering, and process-local metadata overlay. It is not a
//! replacement for the independent metadata/block-store integrations.

use std::collections::HashMap;
use std::fmt;
use std::hash::{BuildHasher, RandomState};
use std::ops::Bound;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use arc_swap::{ArcSwap, ArcSwapOption};
use async_trait::async_trait;
use im::{HashMap as SharedMap, OrdSet};
use mount_rs_core::driver::{FileHandle, FsDriver};
use mount_rs_core::error::{ErrorCode, FsError, Result, backend_error};
use mount_rs_core::handle::{OpenFlags, checked_position};
use mount_rs_core::park_cell::ParkCell;
use mount_rs_core::path::{basename, dirname, is_path_inside, normalize_path, split_path};
use mount_rs_core::turn_gate::TurnGate;
use mount_rs_core::types::{
    Capabilities, DirEntry, FileType, MkdirOptions, S_IFDIR, S_IFMT, S_IFREG, Stats, now_ms,
};

const BLOCK_SIZE: u64 = 4096;
const KEY_SEPARATOR: &str = ":";

/// Optional native metadata supplied by a key-value backend.
///
/// Metadata is never serialized by this crate. Backends that cannot provide a
/// field leave it as `None`; the adapter uses its process-local overlay or a
/// stable creation timestamp instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyValueMetadata {
    pub size: Option<u64>,
    pub atime_ms: Option<i64>,
    pub mtime_ms: Option<i64>,
    pub ctime_ms: Option<i64>,
    pub birthtime_ms: Option<i64>,
}

/// The minimal asynchronous store surface required by [`KeyValueFs`].
///
/// Keys are the store's native strings. `get_keys(prefix)` returns every
/// visible key below that prefix, including the prefix itself when a backend
/// chooses to do so; the adapter filters metadata keys ending in `$` and
/// interprets the remaining flat key space as a directory tree.
#[async_trait]
pub trait KeyValueStore: Send + Sync + 'static {
    type Error: fmt::Display + Send + Sync + 'static;

    async fn has_item(&self, key: &str) -> std::result::Result<bool, Self::Error>;

    async fn get_item_raw(&self, key: &str) -> std::result::Result<Option<Vec<u8>>, Self::Error>;

    async fn set_item_raw(&self, key: &str, value: Vec<u8>)
    -> std::result::Result<(), Self::Error>;

    /// Remove a value and any metadata associated with the same key.
    ///
    /// The upstream unstorage driver uses `removeItem(key, { removeMeta: true
    /// })` for unlink and rename-source cleanup, so a recreated key must not
    /// inherit the removed file's native metadata.
    async fn remove_item(&self, key: &str) -> std::result::Result<(), Self::Error>;

    async fn get_keys(&self, prefix: &str) -> std::result::Result<Vec<String>, Self::Error>;

    /// Return a provider-bounded key listing when the backend can enforce one
    /// before materializing the result. `None` means the provider has no safe
    /// bounded-listing operation and callers must fail closed. A `Some` list
    /// may contain at most `max_keys + 1` keys; a result longer than
    /// `max_keys` is treated as an overflow signal by the filesystem adapter.
    async fn get_keys_bounded(
        &self,
        _prefix: &str,
        _max_keys: usize,
    ) -> std::result::Result<Option<Vec<String>>, Self::Error> {
        Ok(None)
    }

    /// Return backend-native metadata without reading the value when possible.
    async fn get_meta(&self, _key: &str) -> std::result::Result<KeyValueMetadata, Self::Error> {
        Ok(KeyValueMetadata::default())
    }
}

/// Options for the unstorage-compatible adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnstorageOptions {
    /// Owner used when the backend does not supply an overlay value.
    pub uid: u32,
    pub gid: u32,
    /// Permission bits reported for files without an explicit `chmod`.
    pub file_mode: u32,
    /// Permission bits reported for directories without an explicit `chmod`.
    pub dir_mode: u32,
    /// Reject every operation that could mutate the store with `EROFS`.
    pub read_only: bool,
}

impl Default for UnstorageOptions {
    fn default() -> Self {
        Self {
            uid: process_uid(),
            gid: process_gid(),
            file_mode: 0o644,
            dir_mode: 0o755,
            read_only: false,
        }
    }
}

/// What `chmod`, `chown`, `utimes` and writes have set on a path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Overlay {
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    atime_ms: Option<i64>,
    mtime_ms: Option<i64>,
    ctime_ms: Option<i64>,
}

/// One path's entry in the attribute table. The entry is shared: a write
/// replaces its overlay in place, so the table itself changes only when a
/// path is added, removed or renamed.
struct PathAttributes {
    ino: u64,
    overlay: ArcSwap<Overlay>,
    /// Set once the entry has left the table (unlink, rmdir, a rename over
    /// it). An open file that remembers the entry looks its path up again.
    removed: AtomicBool,
}

impl PathAttributes {
    fn new(ino: u64, overlay: Overlay) -> Arc<Self> {
        Arc::new(Self {
            ino,
            overlay: ArcSwap::from_pointee(overlay),
            removed: AtomicBool::new(false),
        })
    }

    /// Applies `update` to the overlay as one step: a reader sees all of it
    /// or none of it, and two updates never mix their fields. An update
    /// that changes nothing (a write in the millisecond of the last one)
    /// stores nothing and allocates nothing.
    fn update(&self, update: &impl Fn(&mut Overlay)) {
        let mut current = self.overlay.load();
        loop {
            let mut next = **current;
            update(&mut next);
            if next == **current {
                return;
            }
            let previous = self.overlay.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return;
            }
            current = previous;
        }
    }

    fn snapshot(&self) -> Attributes {
        Attributes {
            ino: self.ino,
            overlay: **self.overlay.load(),
        }
    }
}

/// A path's attributes at one moment.
#[derive(Debug, Clone, Copy)]
struct Attributes {
    ino: u64,
    overlay: Overlay,
}

impl std::ops::Deref for Attributes {
    type Target = Overlay;
    fn deref(&self) -> &Overlay {
        &self.overlay
    }
}

struct OpenFile {
    path: String,
    data: Vec<u8>,
    refs: u64,
    dirty: bool,
    /// An unlinked/replaced file remains readable but must never reappear.
    orphan: bool,
    /// Set, under this entry's lock, by the last close as it takes the
    /// entry out of the open-file table. An open that picked the entry up
    /// from the table just before must not use it: the next open would build
    /// a second buffer for the same file and one of their saves would be
    /// lost. It looks the path up again instead.
    detached: bool,
    /// The attribute entry of `path`, remembered so a write stamps it
    /// without a lookup. It is the table's entry for `path` for as long as
    /// it is not marked removed: a rename moves the entry with the path and
    /// clears this, and an orphan never uses it.
    attributes: Option<Arc<PathAttributes>>,
}

/// An open file's buffer, with the turn its saves take.
struct OpenEntry {
    file: ParkCell<OpenFile>,
    /// Held from a save's snapshot of the buffer until the store has
    /// answered, so two saves of one file reach the store in the order of
    /// their snapshots: an older save cannot land on top of a newer one.
    saving: TurnGate,
}

impl std::ops::Deref for OpenEntry {
    type Target = ParkCell<OpenFile>;
    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

/// The attribute and empty-directory tables, as one immutable value. A
/// change builds the next value (sharing all but the touched branches with
/// the last) and publishes it with a compare-and-swap, so a rename moves a
/// path in both in one step and a reader never sees it half moved.
///
/// The attributes are found by hash. Their paths are also kept in order,
/// as the empty directories are, so everything below a directory is one
/// contiguous range: a rename visits what it moves, not every path.
#[derive(Clone, Default)]
struct Tables {
    attributes: SharedMap<Arc<str>, Arc<PathAttributes>>,
    /// The keys of `attributes`.
    attribute_paths: OrdSet<Arc<str>>,
    empty_directories: OrdSet<Arc<str>>,
}

const OPEN_BUCKETS: usize = 1024;

type OpenSlot = (Arc<str>, Arc<OpenEntry>);

/// One bucket's files, with how many have ever been taken out of it.
#[derive(Default)]
struct OpenBucket {
    /// An open that found no buffer for its path reads the store and then
    /// puts its own buffer in. The store may only be trusted if no buffer
    /// for the path came and went in between, saving newer contents after
    /// the read: this count is how the open knows. It never goes back, so
    /// a bucket that is empty again is still not the bucket it was.
    removals: u64,
    slots: Vec<OpenSlot>,
}

/// What became of an attempt to put a new buffer in the table.
enum Inserted {
    Done,
    /// The path has an open buffer already.
    Existing(Arc<OpenEntry>),
    /// A buffer left the bucket since the caller looked: what the caller
    /// read from the store may be older than what that buffer saved.
    Stale,
}

/// The open files by path: a fixed array of buckets, each an immutable
/// list replaced whole by a compare-and-swap. An open or a close copies
/// one bucket, which holds one file in every thousand open, and waits for
/// nobody.
struct OpenTable {
    hasher: RandomState,
    buckets: Box<[ArcSwapOption<OpenBucket>]>,
}

impl OpenTable {
    fn new() -> Self {
        Self {
            hasher: RandomState::new(),
            buckets: (0..OPEN_BUCKETS).map(|_| ArcSwapOption::empty()).collect(),
        }
    }

    fn bucket(&self, path: &str) -> &ArcSwapOption<OpenBucket> {
        &self.buckets[(self.hasher.hash_one(path) % OPEN_BUCKETS as u64) as usize]
    }

    /// The file open under `path`, and the bucket's removal count as of
    /// the same moment.
    fn find(&self, path: &str) -> (Option<Arc<OpenEntry>>, u64) {
        let bucket = self.bucket(path).load();
        let Some(bucket) = bucket.as_ref() else {
            return (None, 0);
        };
        let entry = bucket
            .slots
            .iter()
            .find(|(held, _)| &**held == path)
            .map(|(_, entry)| Arc::clone(entry));
        (entry, bucket.removals)
    }

    fn get(&self, path: &str) -> Option<Arc<OpenEntry>> {
        self.find(path).0
    }

    fn contains(&self, path: &str) -> bool {
        let bucket = self.bucket(path).load();
        bucket
            .as_ref()
            .is_some_and(|bucket| bucket.slots.iter().any(|(held, _)| &**held == path))
    }

    /// Replaces the bucket of `path` with what `decide` makes of it, or
    /// leaves it when `decide` says `Err`. `decide` runs again whenever
    /// another change to the bucket was published first, so it must only
    /// compute.
    fn change<R>(
        &self,
        path: &str,
        decide: impl Fn(&OpenBucket) -> std::result::Result<(OpenBucket, R), R>,
    ) -> R {
        let empty = OpenBucket::default();
        let bucket = self.bucket(path);
        let mut current = bucket.load();
        loop {
            let (next, result) = match decide(current.as_deref().unwrap_or(&empty)) {
                Ok(change) => change,
                Err(result) => return result,
            };
            let previous = bucket.compare_and_swap(&current, Some(Arc::new(next)));
            let published = match (&*previous, &*current) {
                (None, None) => true,
                (Some(previous), Some(current)) => Arc::ptr_eq(previous, current),
                _ => false,
            };
            if published {
                return result;
            }
            current = previous;
        }
    }

    /// Puts `entry` under `path`, unless a file is already open there or a
    /// buffer has left the bucket since the caller saw `removals`.
    fn insert_if_absent(&self, path: &str, entry: &Arc<OpenEntry>, removals: u64) -> Inserted {
        let slot: OpenSlot = (Arc::from(path), Arc::clone(entry));
        self.change(path, |bucket| {
            if let Some((_, existing)) = bucket.slots.iter().find(|(held, _)| &**held == path) {
                return Err(Inserted::Existing(Arc::clone(existing)));
            }
            if bucket.removals != removals {
                return Err(Inserted::Stale);
            }
            let mut slots = Vec::with_capacity(bucket.slots.len() + 1);
            slots.extend_from_slice(&bucket.slots);
            slots.push(slot.clone());
            Ok((OpenBucket { removals, slots }, Inserted::Done))
        })
    }

    /// Puts `entry` under `path`, in place of whatever was open there.
    fn insert(&self, path: &str, entry: &Arc<OpenEntry>) {
        let slot: OpenSlot = (Arc::from(path), Arc::clone(entry));
        self.change(path, |bucket| {
            let mut slots = Vec::with_capacity(bucket.slots.len() + 1);
            slots.extend(
                bucket
                    .slots
                    .iter()
                    .filter(|(held, _)| &**held != path)
                    .cloned(),
            );
            let replaced = (slots.len() != bucket.slots.len()) as u64;
            slots.push(slot.clone());
            let removals = bucket.removals.wrapping_add(replaced);
            Ok((OpenBucket { removals, slots }, ()))
        });
    }

    /// The bucket without the slots `leaving` picks.
    fn without(bucket: &OpenBucket, leaving: impl Fn(&OpenSlot) -> bool) -> OpenBucket {
        OpenBucket {
            removals: bucket.removals.wrapping_add(1),
            slots: bucket
                .slots
                .iter()
                .filter(|slot| !leaving(slot))
                .cloned()
                .collect(),
        }
    }

    /// Takes out whatever is open under `path`.
    fn remove(&self, path: &str) -> Option<Arc<OpenEntry>> {
        self.change(path, |bucket| {
            let Some((_, removed)) = bucket.slots.iter().find(|(held, _)| &**held == path) else {
                return Err(None);
            };
            let next = Self::without(bucket, |(held, _)| &**held == path);
            Ok((next, Some(Arc::clone(removed))))
        })
    }

    /// Whether the table held `entry` under `path` and no longer does.
    fn remove_entry(&self, path: &str, entry: &Arc<OpenEntry>) -> bool {
        self.change(path, |bucket| {
            let held = |(held, current): &OpenSlot| &**held == path && Arc::ptr_eq(current, entry);
            if !bucket.slots.iter().any(held) {
                return Err(false);
            }
            Ok((Self::without(bucket, held), true))
        })
    }

    /// The files open at `from` or below it, as seen bucket by bucket.
    fn inside(&self, from: &str) -> Vec<OpenSlot> {
        let below = below(from);
        let mut inside = Vec::new();
        for bucket in self.buckets.iter() {
            let bucket = bucket.load();
            let Some(bucket) = bucket.as_ref() else {
                continue;
            };
            inside.extend(
                bucket
                    .slots
                    .iter()
                    .filter(|(path, _)| match &below {
                        None => true,
                        Some((prefix, _)) => &**path == from || path.starts_with(prefix.as_str()),
                    })
                    .cloned(),
            );
        }
        inside
    }
}

/// What one attempt at a change decided, given the tables it looked at.
enum Step<R> {
    /// Nothing to publish.
    Keep(R),
    /// Publish these tables; decide again if another change got in first.
    Swap(Tables, R),
}

/// The bounds of the paths strictly below `parent`: those starting with
/// `parent/`. `None` for the root, which everything else is below.
fn below(parent: &str) -> Option<(String, String)> {
    // '0' is the byte after '/', so this is every string with that prefix.
    (parent != "/").then(|| (format!("{parent}/"), format!("{parent}0")))
}

/// The members of `set` at `from` or below it.
fn members_inside(set: &OrdSet<Arc<str>>, from: &str) -> Vec<Arc<str>> {
    let mut inside = Vec::new();
    inside.extend(lookup(set, from).cloned());
    inside.extend(members_below(set, from));
    inside
}

/// The member of `set` equal to `path`.
fn lookup<'a>(set: &'a OrdSet<Arc<str>>, path: &str) -> Option<&'a Arc<str>> {
    let bounds = (Bound::Included(path), Bound::Included(path));
    set.range::<_, str>(bounds).next()
}

/// The members of `set` strictly below `parent`.
fn members_below(set: &OrdSet<Arc<str>>, parent: &str) -> Vec<Arc<str>> {
    let Some((low, high)) = below(parent) else {
        return set.iter().filter(|path| &***path != "/").cloned().collect();
    };
    let bounds = (
        Bound::Included(low.as_str()),
        Bound::Excluded(high.as_str()),
    );
    set.range::<_, str>(bounds).cloned().collect()
}

struct Inner<S> {
    store: S,
    options: UnstorageOptions,
    created_ms: i64,
    next_ino: AtomicU64,
    next_fd: AtomicU64,
    tables: ArcSwap<Tables>,
    open_files: OpenTable,
}

/// A cloneable generic key-value filesystem driver.
pub struct KeyValueFs<S> {
    inner: Arc<Inner<S>>,
}

/// Alias matching the upstream driver name in Rust APIs.
pub type UnstorageFs<S> = KeyValueFs<S>;

#[cfg(unix)]
fn process_uid() -> u32 {
    // SAFETY: getuid has no preconditions and returns a value by copy.
    unsafe { libc::getuid() }
}

#[cfg(not(unix))]
fn process_uid() -> u32 {
    0
}

#[cfg(unix)]
fn process_gid() -> u32 {
    // SAFETY: getgid has no preconditions and returns a value by copy.
    unsafe { libc::getgid() }
}

#[cfg(not(unix))]
fn process_gid() -> u32 {
    0
}

/// Construct an unstorage-compatible filesystem driver over `store`.
pub fn create_unstorage_driver<S>(store: S, options: UnstorageOptions) -> KeyValueFs<S>
where
    S: KeyValueStore,
{
    KeyValueFs {
        inner: Arc::new(Inner {
            store,
            options,
            created_ms: now_ms(),
            next_ino: AtomicU64::new(1),
            next_fd: AtomicU64::new(3),
            tables: ArcSwap::from_pointee(Tables::default()),
            open_files: OpenTable::new(),
        }),
    }
}

impl<S> Clone for KeyValueFs<S> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

#[derive(Default)]
struct Scope {
    items: HashMap<String, bool>,
    keys: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Missing,
}

struct KvFileHandle<S> {
    inner: Arc<Inner<S>>,
    entry: Arc<OpenEntry>,
    flags: OpenFlags,
    fd: u64,
    closed: AtomicBool,
    // Only read and written while `entry` is held, which orders this handle's
    // reads and writes, so a load and a later store are one step.
    position: AtomicU64,
}

struct KvDirectoryHandle<S> {
    inner: Arc<Inner<S>>,
    path: String,
    fd: u64,
}

fn key_of(path: &str, syscall: &str) -> Result<String> {
    let normalized = normalize_path(path);
    let segments = split_path(&normalized);
    for segment in &segments {
        if segment.contains(KEY_SEPARATOR) || segment.contains('?') || segment.ends_with('$') {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall(syscall)
                .with_path(normalized)
                .with_message(format!(
                    "Name is not representable as an unstorage key: '{segment}'"
                )));
        }
    }
    Ok(segments.join(KEY_SEPARATOR))
}

fn path_of(key: &str) -> String {
    let path = format!("/{}/", key.replace(KEY_SEPARATOR, "/"));
    normalize_path(path.trim_end_matches('/'))
}

fn prefix_of(key: &str) -> String {
    if key.is_empty() {
        String::new()
    } else {
        format!("{key}{KEY_SEPARATOR}")
    }
}

fn representable_key(key: &str) -> bool {
    key_of(&path_of(key), "scandir").is_ok_and(|canonical| canonical == key)
}

fn resize_bytes(data: &mut Vec<u8>, length: u64, syscall: &str) -> Result<()> {
    let length = usize::try_from(length)
        .map_err(|_| FsError::new(ErrorCode::Efbig).with_syscall(syscall))?;
    if length > data.len() {
        data.try_reserve(length - data.len())
            .map_err(|_| FsError::new(ErrorCode::Efbig).with_syscall(syscall))?;
    }
    data.resize(length, 0);
    Ok(())
}

impl<S> Inner<S>
where
    S: KeyValueStore,
{
    /// Changes the tables: `decide` looks at the current ones and says
    /// what to publish. It runs again whenever another change was published
    /// first, so it must only compute: ids are allocated and entries built
    /// before the call.
    fn change<R>(&self, decide: impl Fn(&Tables) -> Step<R>) -> R {
        let mut current = self.tables.load_full();
        loop {
            match decide(&current) {
                Step::Keep(result) => return result,
                Step::Swap(next, result) => {
                    let previous = self.tables.compare_and_swap(&current, Arc::new(next));
                    if Arc::ptr_eq(&previous, &current) {
                        return result;
                    }
                    current = arc_swap::Guard::into_inner(previous);
                }
            }
        }
    }

    fn allocate(counter: &AtomicU64, syscall: &str) -> Result<u64> {
        counter
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
                next.checked_add(1)
            })
            .map_err(|_| FsError::new(ErrorCode::Eoverflow).with_syscall(syscall))
    }

    fn store_error<E: fmt::Display>(&self, error: E, syscall: &str, path: &str) -> FsError {
        backend_error(error)
            .with_syscall(syscall)
            .with_path(normalize_path(path))
    }

    /// The attribute entry of `path`, which is `candidate` unless the path
    /// already had one. The flag says whether `candidate` went in.
    fn attributes_or_insert(
        &self,
        path: &str,
        candidate: Arc<PathAttributes>,
    ) -> (Arc<PathAttributes>, bool) {
        let key: Arc<str> = Arc::from(path);
        self.change(|tables| match tables.attributes.get(path) {
            Some(existing) => Step::Keep((Arc::clone(existing), false)),
            None => {
                let mut next = tables.clone();
                next.attributes
                    .insert(Arc::clone(&key), Arc::clone(&candidate));
                next.attribute_paths.insert(Arc::clone(&key));
                Step::Swap(next, (Arc::clone(&candidate), true))
            }
        })
    }

    fn attributes_of(&self, path: &str) -> Result<Attributes> {
        if let Some(attributes) = self.tables.load().attributes.get(path) {
            return Ok(attributes.snapshot());
        }
        // Allocated before the insert is attempted: an attempt that loses
        // to another caller's entry leaves a gap in the numbering.
        let ino = Self::allocate(&self.next_ino, "stat")?;
        let candidate = PathAttributes::new(ino, Overlay::default());
        Ok(self.attributes_or_insert(path, candidate).0.snapshot())
    }

    fn update_attributes<F>(&self, path: &str, syscall: &str, update: F) -> Result<()>
    where
        F: Fn(&mut Overlay),
    {
        self.updated_attributes(path, syscall, update).map(drop)
    }

    /// Applies `update` to the attributes of `path` and returns its entry.
    fn updated_attributes<F>(
        &self,
        path: &str,
        syscall: &str,
        update: F,
    ) -> Result<Arc<PathAttributes>>
    where
        F: Fn(&mut Overlay),
    {
        if let Some(attributes) = self.tables.load().attributes.get(path) {
            attributes.update(&update);
            return Ok(Arc::clone(attributes));
        }
        // A new entry goes in with the update already applied, so no reader
        // sees the path numbered but not yet updated.
        let ino = Self::allocate(&self.next_ino, syscall)?;
        let mut overlay = Overlay::default();
        update(&mut overlay);
        let (attributes, inserted) =
            self.attributes_or_insert(path, PathAttributes::new(ino, overlay));
        if !inserted {
            attributes.update(&update);
        }
        Ok(attributes)
    }

    fn remove_attributes(&self, path: &str) {
        let removed = self.change(|tables| {
            let Some(removed) = tables.attributes.get(path) else {
                return Step::Keep(None);
            };
            let removed = Arc::clone(removed);
            let mut next = tables.clone();
            next.attributes.remove(path);
            next.attribute_paths.remove(path);
            Step::Swap(next, Some(removed))
        });
        if let Some(removed) = removed {
            removed.removed.store(true, Ordering::SeqCst);
        }
    }

    fn set_empty_directory(&self, path: &str, present: bool) {
        let key: Arc<str> = Arc::from(path);
        self.change(|tables| {
            if tables.empty_directories.contains(path) == present {
                return Step::Keep(());
            }
            let mut next = tables.clone();
            if present {
                next.empty_directories.insert(Arc::clone(&key));
            } else {
                next.empty_directories.remove(path);
            }
            Step::Swap(next, ())
        });
    }

    fn has_empty_directory(&self, path: &str) -> bool {
        self.tables.load().empty_directories.contains(path)
    }

    fn has_open_file(&self, path: &str) -> bool {
        self.open_files.contains(path)
    }

    fn open_file(&self, path: &str) -> Option<Arc<OpenEntry>> {
        self.open_files.get(path)
    }

    fn allocate_fd(&self) -> Result<u64> {
        Self::allocate(&self.next_fd, "open")
    }

    /// Counts one more handle on an entry found in the open-file table.
    /// `false` means the last close took the entry out of the table in the
    /// meantime, and the caller must look the path up again.
    fn join_open_file(&self, entry: &Arc<OpenEntry>, syscall: &str) -> Result<bool> {
        let mut file = entry
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall(syscall))?;
        if file.detached {
            return Ok(false);
        }
        file.refs += 1;
        Ok(true)
    }

    fn mark_orphan(&self, path: &str) -> Result<()> {
        if let Some(entry) = self.open_files.remove(path) {
            entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio))?
                .orphan = true;
        }
        Ok(())
    }

    fn remove_closed(&self, entry: &Arc<OpenEntry>) -> Result<()> {
        // The caller holds the entry's saving turn, so a clean buffer here
        // is one the store already holds. The count is read, the entry
        // leaves the table and it is marked detached all under the entry's
        // lock, which an open must take to count itself: it either is
        // counted before this looks, and the entry stays, or finds the
        // mark. The table has no lock of its own: leaving it is a
        // compare-and-swap that waits for nobody, and a table that no
        // longer holds this entry under this path (unlinked, replaced) is
        // left as it is.
        let mut file = entry.lock().map_err(|_| FsError::new(ErrorCode::Eio))?;
        if file.refs == 0
            && (!file.dirty || file.orphan)
            && self.open_files.remove_entry(&file.path, entry)
        {
            file.detached = true;
        }
        Ok(())
    }

    fn move_paths(&self, from: &str, to: &str) -> Result<()> {
        // The open files move first, one at a time, each under its own
        // lock: an open or a last close of that file waits, and afterwards
        // finds the table and the file's own path agreeing on the new name.
        // Until the attributes follow, a stat of the new name numbers the
        // path afresh and the move below replaces that entry, which is what
        // a stat between the store's move and this one has always done.
        for (path, entry) in self.open_files.inside(from) {
            let mut file = entry.lock().map_err(|_| FsError::new(ErrorCode::Eio))?;
            // Closed or unlinked since the scan: it is no longer this
            // path's open file.
            if !self.open_files.remove_entry(&path, &entry) {
                continue;
            }
            let destination = remap_path(from, to, &path);
            self.open_files.insert(&destination, &entry);
            file.path = destination;
            // What a write remembered was found under the old name.
            file.attributes = None;
        }

        // One published value moves the path in both tables. The attribute
        // entries themselves move, so an update in flight on a moved path
        // lands on the entry under its new name.
        let replaced = self.change(|tables| {
            let attributes = members_inside(&tables.attribute_paths, from);
            let directories = members_inside(&tables.empty_directories, from);
            if attributes.is_empty() && directories.is_empty() {
                return Step::Keep(Vec::new());
            }
            let mut next = tables.clone();
            let mut moved = Vec::with_capacity(attributes.len());
            for path in &attributes {
                next.attribute_paths.remove(path);
                moved.extend(next.attributes.remove(path).map(|value| (path, value)));
            }
            let mut replaced = Vec::new();
            for (path, value) in moved {
                let destination: Arc<str> = Arc::from(remap_path(from, to, path));
                next.attribute_paths.insert(Arc::clone(&destination));
                replaced.extend(next.attributes.insert(destination, value));
            }
            for path in &directories {
                next.empty_directories.remove(path);
            }
            for path in directories {
                next.empty_directories
                    .insert(Arc::from(remap_path(from, to, &path)));
            }
            Step::Swap(next, replaced)
        });
        for replaced in replaced {
            replaced.removed.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    async fn has_key(
        &self,
        key: &str,
        scope: &mut Scope,
        syscall: &str,
        path: &str,
    ) -> Result<bool> {
        if let Some(value) = scope.items.get(key) {
            return Ok(*value);
        }
        let value = self
            .store
            .has_item(key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))?;
        scope.items.insert(key.to_owned(), value);
        Ok(value)
    }

    async fn keys_under(
        &self,
        key: &str,
        scope: &mut Scope,
        syscall: &str,
        path: &str,
    ) -> Result<Vec<String>> {
        if let Some(value) = scope.keys.get(key) {
            return Ok(value.clone());
        }
        let prefix = prefix_of(key);
        let value = self
            .store
            .get_keys(key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))?
            .into_iter()
            .filter(|entry| entry.starts_with(&prefix) && representable_key(entry))
            .collect::<Vec<_>>();
        scope.keys.insert(key.to_owned(), value.clone());
        Ok(value)
    }

    async fn keys_under_bounded(
        &self,
        key: &str,
        max_keys: usize,
        syscall: &str,
        path: &str,
    ) -> Result<Option<Vec<String>>> {
        let keys = self
            .store
            .get_keys_bounded(key, max_keys)
            .await
            .map_err(|error| self.store_error(error, syscall, path))?;
        let Some(keys) = keys else {
            return Ok(None);
        };
        if keys.len() > max_keys {
            return Err(FsError::new(ErrorCode::Eoverflow)
                .with_syscall(syscall)
                .with_path(path)
                .with_message("directory exceeds the configured entry limit"));
        }
        Ok(Some(
            keys.into_iter()
                .filter(|entry| representable_key(entry))
                .collect(),
        ))
    }

    async fn read_value(&self, path: &str, syscall: &str) -> Result<Vec<u8>> {
        let key = key_of(path, syscall)?;
        Ok(self
            .store
            .get_item_raw(&key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))?
            .unwrap_or_default())
    }

    async fn write_value(&self, path: &str, data: &[u8], syscall: &str) -> Result<()> {
        let key = key_of(path, syscall)?;
        self.store
            .set_item_raw(&key, data.to_vec())
            .await
            .map_err(|error| self.store_error(error, syscall, path))?;
        self.touch(path, syscall)
    }

    async fn remove_value(&self, path: &str, syscall: &str) -> Result<()> {
        let key = key_of(path, syscall)?;
        self.store
            .remove_item(&key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))
    }

    async fn move_value(&self, from: &str, to: &str) -> Result<()> {
        let source_key = key_of(from, "rename")?;
        let destination_key = key_of(to, "rename")?;
        let value = self
            .store
            .get_item_raw(&source_key)
            .await
            .map_err(|error| self.store_error(error, "rename", from))?
            .unwrap_or_default();
        self.store
            .set_item_raw(&destination_key, value)
            .await
            .map_err(|error| self.store_error(error, "rename", to))?;
        self.store
            .remove_item(&source_key)
            .await
            .map_err(|error| self.store_error(error, "rename", from))
    }

    fn touch(&self, path: &str, syscall: &str) -> Result<()> {
        let timestamp = now_ms();
        self.update_attributes(path, syscall, |attributes| {
            attributes.mtime_ms = Some(timestamp);
            attributes.ctime_ms = Some(timestamp);
        })
    }

    /// [`Self::touch`] for an open file whose lock the caller holds, in
    /// two steps so the stamp itself is made after the lock is released.
    /// This one returns the attribute entry the file remembers, which is
    /// the one a lookup of its path would find, so a write does no lookup;
    /// the caller passes it to [`stamp`] once the lock is released. When
    /// the file remembers none it is found by path, stamped here and
    /// remembered.
    fn touch_open(
        &self,
        file: &mut OpenFile,
        syscall: &str,
    ) -> Result<Option<Arc<PathAttributes>>> {
        if file.orphan {
            // The path may belong to another file by now. Whatever is
            // there is stamped, as it always was.
            self.touch(&file.path, syscall)?;
            return Ok(None);
        }
        if let Some(attributes) = &file.attributes
            && !attributes.removed.load(Ordering::SeqCst)
        {
            return Ok(Some(Arc::clone(attributes)));
        }
        let timestamp = now_ms();
        file.attributes = Some(self.updated_attributes(&file.path, syscall, |attributes| {
            attributes.mtime_ms = Some(timestamp);
            attributes.ctime_ms = Some(timestamp);
        })?);
        Ok(None)
    }
}

/// Marks the attributes [`Inner::touch_open`] returned as modified now.
fn stamp(attributes: Option<Arc<PathAttributes>>) {
    if let Some(attributes) = attributes {
        let timestamp = now_ms();
        attributes.update(&|attributes: &mut Overlay| {
            attributes.mtime_ms = Some(timestamp);
            attributes.ctime_ms = Some(timestamp);
        });
    }
}

impl<S> KvFileHandle<S>
where
    S: KeyValueStore,
{
    fn path(&self) -> Result<String> {
        Ok(self
            .entry
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio))?
            .path
            .clone())
    }

    fn begin(&self, write: bool, syscall: &str) -> Result<String> {
        let path = self.path()?;
        if self.closed.load(Ordering::Acquire)
            || (write && !self.flags.write)
            || (!write && !self.flags.read)
        {
            return Err(FsError::new(ErrorCode::Ebadf)
                .with_syscall(syscall)
                .with_path(path));
        }
        #[cfg(test)]
        tests::after_begin();
        Ok(path)
    }

    /// Checked again once the entry's lock is held. A close marks the
    /// handle closed before it takes that lock to give up its reference,
    /// so a call that holds the lock and still finds the handle open is
    /// ahead of the close, which then saves what it wrote. Without this a
    /// call could pass `begin`, lose the processor while the close ran to
    /// the end and dropped the buffer from the table, and then write into
    /// a buffer nothing will ever save.
    fn still_open(&self, syscall: &str) -> Result<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(FsError::new(ErrorCode::Ebadf).with_syscall(syscall));
        }
        Ok(())
    }

    fn ensure_open(&self, syscall: &str) -> Result<String> {
        let path = self.path()?;
        if self.closed.load(Ordering::Acquire) {
            return Err(FsError::new(ErrorCode::Ebadf)
                .with_syscall(syscall)
                .with_path(path));
        }
        Ok(path)
    }
}

#[async_trait]
impl<S> FileHandle for KvFileHandle<S>
where
    S: KeyValueStore,
{
    fn fd(&self) -> Option<u64> {
        Some(self.fd)
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let _path = self.begin(false, "read")?;
        let file = self
            .entry
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("read"))?;
        self.still_open("read")?;
        let current = self.position.load(Ordering::Relaxed);
        let from = position.unwrap_or(current);
        let from = usize::try_from(from).unwrap_or(usize::MAX);
        let count = if from >= file.data.len() {
            0
        } else {
            buffer.len().min(file.data.len() - from)
        };
        if count > 0 {
            buffer[..count].copy_from_slice(&file.data[from..from + count]);
        }
        if position.is_none() {
            self.position
                .store(current.saturating_add(count as u64), Ordering::Relaxed);
        }
        drop(file);
        Ok(count)
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let path = self.begin(true, "write")?;
        let mut file = self
            .entry
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("write"))?;
        self.still_open("write")?;
        let from = if self.flags.append {
            file.data.len() as u64
        } else {
            position.unwrap_or_else(|| self.position.load(Ordering::Relaxed))
        };
        let end = from.checked_add(buffer.len() as u64).ok_or_else(|| {
            FsError::new(ErrorCode::Efbig)
                .with_syscall("write")
                .with_path(&path)
        })?;
        if end > file.data.len() as u64 {
            resize_bytes(&mut file.data, end, "write")?;
        }
        if !buffer.is_empty() {
            let start = checked_position(from)?;
            file.data[start..start + buffer.len()].copy_from_slice(buffer);
        }
        if self.flags.append || position.is_none() {
            self.position.store(end, Ordering::Relaxed);
        }
        file.dirty = true;
        let attributes = self.inner.touch_open(&mut file, "write")?;
        drop(file);
        stamp(attributes);
        Ok(buffer.len())
    }

    async fn stat(&self) -> Result<Stats> {
        let path = self.ensure_open("fstat")?;
        self.inner
            .file_stats(&path, "fstat", Some(Arc::clone(&self.entry)))
            .await
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        let _path = self.begin(true, "ftruncate")?;
        let attributes = {
            let mut file = self
                .entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("ftruncate"))?;
            self.still_open("ftruncate")?;
            resize_bytes(&mut file.data, length, "ftruncate")?;
            file.dirty = true;
            self.inner.touch_open(&mut file, "ftruncate")?
        };
        stamp(attributes);
        Ok(())
    }

    async fn sync(&self) -> Result<()> {
        self.inner.flush(&self.entry).await
    }

    async fn datasync(&self) -> Result<()> {
        self.inner.flush(&self.entry).await
    }

    async fn close(&self) -> Result<()> {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.inner.release(&self.entry).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl<S> FileHandle for KvDirectoryHandle<S>
where
    S: KeyValueStore,
{
    fn fd(&self) -> Option<u64> {
        Some(self.fd)
    }

    async fn read(&self, _buffer: &mut [u8], _position: Option<u64>) -> Result<usize> {
        Err(FsError::new(ErrorCode::Eisdir)
            .with_syscall("read")
            .with_path(&self.path))
    }

    async fn write(&self, _buffer: &[u8], _position: Option<u64>) -> Result<usize> {
        Err(FsError::new(ErrorCode::Eisdir)
            .with_syscall("write")
            .with_path(&self.path))
    }

    async fn stat(&self) -> Result<Stats> {
        self.inner.directory_stats(&self.path)
    }

    async fn truncate(&self, _length: u64) -> Result<()> {
        Err(FsError::new(ErrorCode::Eisdir)
            .with_syscall("ftruncate")
            .with_path(&self.path))
    }

    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl<S> FsDriver for KeyValueFs<S>
where
    S: KeyValueStore,
{
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            handles: true,
            hardlinks: false,
            symlinks: false,
            permissions: true,
            times: true,
            truncate: true,
            atomic_rename: false,
            case_sensitive: true,
            statfs: false,
            read_only: self.inner.options.read_only,
            durable_writes: false,
            mknod: false,
        }
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        let path = normalize_path(path);
        self.inner
            .stat_of(&path, "stat", &mut Scope::default())
            .await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        let path = normalize_path(path);
        self.inner
            .lstat_of(&path, "lstat", &mut Scope::default())
            .await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let parent_path = normalize_path(path);
        let mut scope = Scope::default();
        match self
            .inner
            .lookup(&parent_path, "scandir", &mut scope)
            .await?
        {
            Kind::Missing => {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("scandir")
                    .with_path(&parent_path));
            }
            Kind::File => {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall("scandir")
                    .with_path(&parent_path));
            }
            Kind::Directory => {}
        }

        let base = key_of(&parent_path, "scandir")?;
        let prefix = prefix_of(&base);
        let keys = self
            .inner
            .keys_under(&base, &mut scope, "scandir", &parent_path)
            .await?;
        let mut entries: Vec<(String, bool)> = Vec::new();
        for key in keys {
            if !key.starts_with(&prefix) {
                continue;
            }
            let relative = &key[prefix.len()..];
            if relative.is_empty() {
                continue;
            }
            let (name, is_directory) = match relative.find(KEY_SEPARATOR) {
                Some(index) => (&relative[..index], true),
                None => (relative, false),
            };
            if let Some((_, current_directory)) =
                entries.iter_mut().find(|(entry, _)| entry == name)
            {
                if !is_directory {
                    *current_directory = false;
                }
            } else {
                entries.push((name.to_owned(), is_directory));
            }
        }

        let empty_directories =
            members_below(&self.inner.tables.load().empty_directories, &parent_path);
        for directory in empty_directories
            .iter()
            .filter(|directory| dirname(directory) == parent_path)
        {
            let name = basename(directory);
            if !entries.iter().any(|(entry, _)| entry == &name) {
                entries.push((name, true));
            }
        }

        Ok(entries
            .into_iter()
            .map(|(name, directory)| DirEntry {
                name,
                parent_path: parent_path.clone(),
                file_type: if directory {
                    FileType::Directory
                } else {
                    FileType::File
                },
            })
            .collect())
    }

    async fn readdir_bounded(&self, path: &str, max_entries: usize) -> Result<Vec<DirEntry>> {
        let parent_path = normalize_path(path);
        if max_entries == 0 {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("scandir")
                .with_path(&parent_path)
                .with_message("directory entry limit must be positive"));
        }
        let mut scope = Scope::default();
        match self
            .inner
            .lookup(&parent_path, "scandir", &mut scope)
            .await?
        {
            Kind::Missing => {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("scandir")
                    .with_path(&parent_path));
            }
            Kind::File => {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall("scandir")
                    .with_path(&parent_path));
            }
            Kind::Directory => {}
        }

        let base = key_of(&parent_path, "scandir")?;
        let prefix = prefix_of(&base);
        let Some(keys) = self
            .inner
            .keys_under_bounded(&base, max_entries, "scandir", &parent_path)
            .await?
        else {
            return Err(FsError::enotsup("scandir")
                .with_path(&parent_path)
                .with_message("provider does not expose bounded key enumeration"));
        };
        if keys.len() > max_entries {
            return Err(FsError::new(ErrorCode::Eoverflow)
                .with_syscall("scandir")
                .with_path(&parent_path)
                .with_message("directory exceeds the configured entry limit"));
        }

        let mut entries: Vec<(String, bool)> = Vec::new();
        for key in keys {
            if !key.starts_with(&prefix) {
                continue;
            }
            let relative = &key[prefix.len()..];
            if relative.is_empty() {
                continue;
            }
            let (name, is_directory) = match relative.find(KEY_SEPARATOR) {
                Some(index) => (&relative[..index], true),
                None => (relative, false),
            };
            if let Some((_, current_directory)) =
                entries.iter_mut().find(|(entry, _)| entry == name)
            {
                if !is_directory {
                    *current_directory = false;
                }
            } else {
                if entries.len() == max_entries {
                    return Err(FsError::new(ErrorCode::Eoverflow)
                        .with_syscall("scandir")
                        .with_path(&parent_path)
                        .with_message("directory exceeds the configured entry limit"));
                }
                entries.push((name.to_owned(), is_directory));
            }
        }

        let empty_directories =
            members_below(&self.inner.tables.load().empty_directories, &parent_path);
        for directory in empty_directories
            .iter()
            .filter(|directory| dirname(directory) == parent_path)
        {
            let name = basename(directory);
            if entries.iter().any(|(entry, _)| entry == &name) {
                continue;
            }
            if entries.len() == max_entries {
                return Err(FsError::new(ErrorCode::Eoverflow)
                    .with_syscall("scandir")
                    .with_path(&parent_path)
                    .with_message("directory exceeds the configured entry limit"));
            }
            entries.push((name, true));
        }

        Ok(entries
            .into_iter()
            .map(|(name, directory)| DirEntry {
                name,
                parent_path: parent_path.clone(),
                file_type: if directory {
                    FileType::Directory
                } else {
                    FileType::File
                },
            })
            .collect())
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        let path = normalize_path(path);
        let flags = OpenFlags::parse(flags, &path)?;
        self.open_flags(&path, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let path = normalize_path(path);
        if !flags.has_valid_truncate_access() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("open")
                .with_path(&path)
                .with_message("truncate requires write access"));
        }
        if flags.write || flags.create || flags.truncate {
            self.inner.mutable("open", &path)?;
        }
        let mut scope = Scope::default();
        let kind = self.inner.lookup(&path, "open", &mut scope).await?;
        if kind == Kind::Missing {
            if !flags.create {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("open")
                    .with_path(&path));
            }
            self.inner.update_attributes(&path, "open", |attributes| {
                attributes.mode = Some(mode & 0o7777);
            })?;
            self.inner.write_value(&path, &[], "open").await?;
            let entry = self.inner.acquire(&path, Some(Vec::new()), "open").await?;
            return Ok(Arc::new(KvFileHandle {
                inner: Arc::clone(&self.inner),
                entry,
                flags,
                fd: self.inner.allocate_fd()?,
                closed: AtomicBool::new(false),
                position: AtomicU64::new(0),
            }));
        }
        if flags.exclusive {
            return Err(FsError::new(ErrorCode::Eexist)
                .with_syscall("open")
                .with_path(&path));
        }
        if kind == Kind::Directory {
            if flags.write {
                return Err(FsError::new(ErrorCode::Eisdir)
                    .with_syscall("open")
                    .with_path(&path));
            }
            return Ok(Arc::new(KvDirectoryHandle {
                inner: Arc::clone(&self.inner),
                path,
                fd: self.inner.allocate_fd()?,
            }));
        }

        let known = flags.truncate.then(Vec::new);
        let entry = self.inner.acquire(&path, known, "open").await?;
        if flags.truncate {
            {
                let mut file = entry
                    .lock()
                    .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("truncate"))?;
                resize_bytes(&mut file.data, 0, "truncate")?;
                file.dirty = true;
            }
            self.inner.touch(&path, "truncate")?;
        }
        Ok(Arc::new(KvFileHandle {
            inner: Arc::clone(&self.inner),
            entry,
            flags,
            fd: self.inner.allocate_fd()?,
            closed: AtomicBool::new(false),
            position: AtomicU64::new(0),
        }))
    }

    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        let path = normalize_path(path);
        self.inner.mutable("mkdir", &path)?;
        let mode = options.mode.unwrap_or(0o777) & 0o7777;
        let mut scope = Scope::default();
        if options.recursive {
            let mut current = String::new();
            let mut first_created = None;
            let segments = split_path(&path);
            for (index, segment) in segments.iter().enumerate() {
                current.push('/');
                current.push_str(segment);
                match self.inner.classify(&current, "mkdir", &mut scope).await? {
                    Kind::Missing => {
                        self.inner.set_empty_directory(&current, true);
                        self.inner
                            .update_attributes(&current, "mkdir", |attributes| {
                                attributes.mode = Some(mode);
                            })?;
                        if first_created.is_none() {
                            first_created = Some(current.clone());
                        }
                    }
                    Kind::File => {
                        return Err(FsError::new(if index + 1 == segments.len() {
                            ErrorCode::Eexist
                        } else {
                            ErrorCode::Enotdir
                        })
                        .with_syscall("mkdir")
                        .with_path(&current));
                    }
                    Kind::Directory => {}
                }
            }
            return Ok(first_created);
        }
        if self.inner.lookup(&path, "mkdir", &mut scope).await? != Kind::Missing {
            return Err(FsError::new(ErrorCode::Eexist)
                .with_syscall("mkdir")
                .with_path(&path));
        }
        self.inner.set_empty_directory(&path, true);
        self.inner.update_attributes(&path, "mkdir", |attributes| {
            attributes.mode = Some(mode);
        })?;
        Ok(None)
    }

    async fn rmdir(&self, path: &str) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("rmdir", &path)?;
        let mut scope = Scope::default();
        match self.inner.lookup(&path, "rmdir", &mut scope).await? {
            Kind::Missing => {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("rmdir")
                    .with_path(&path));
            }
            Kind::File => {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall("rmdir")
                    .with_path(&path));
            }
            Kind::Directory => {}
        }
        if path == "/" {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("rmdir")
                .with_path(&path));
        }
        if self.inner.has_entries(&path, "rmdir", &mut scope).await? {
            return Err(FsError::new(ErrorCode::Enotempty)
                .with_syscall("rmdir")
                .with_path(&path));
        }
        self.inner.set_empty_directory(&path, false);
        self.inner.remove_attributes(&path);
        Ok(())
    }

    async fn unlink(&self, path: &str) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("unlink", &path)?;
        let mut scope = Scope::default();
        match self.inner.lookup(&path, "unlink", &mut scope).await? {
            Kind::Missing => {
                return Err(FsError::new(ErrorCode::Enoent)
                    .with_syscall("unlink")
                    .with_path(&path));
            }
            Kind::Directory => {
                return Err(FsError::new(ErrorCode::Eisdir)
                    .with_syscall("unlink")
                    .with_path(&path));
            }
            Kind::File => {}
        }
        self.inner.remove_value(&path, "unlink").await?;
        self.inner.mark_orphan(&path)?;
        self.inner.set_empty_directory(&path, false);
        self.inner.remove_attributes(&path);
        Ok(())
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        let from = normalize_path(old_path);
        let to = normalize_path(new_path);
        self.inner.mutable("rename", &from)?;
        let mut scope = Scope::default();
        let source = self.inner.lookup(&from, "rename", &mut scope).await?;
        if source == Kind::Missing {
            return Err(FsError::new(ErrorCode::Enoent)
                .with_syscall("rename")
                .with_path(&from)
                .with_dest(&to));
        }
        if from == to {
            return Ok(());
        }
        let directory = source == Kind::Directory;
        if directory && is_path_inside(&to, &from) {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("rename")
                .with_path(&from)
                .with_dest(&to));
        }

        let destination = self.inner.lookup(&to, "rename", &mut scope).await?;
        if directory
            && destination == Kind::Directory
            && self.inner.has_entries(&to, "rename", &mut scope).await?
        {
            return Err(FsError::new(ErrorCode::Enotempty)
                .with_syscall("rename")
                .with_path(&from)
                .with_dest(&to));
        }
        match (directory, destination) {
            (true, Kind::File) => {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall("rename")
                    .with_path(&from)
                    .with_dest(&to));
            }
            (true, Kind::Directory) => {}
            (false, Kind::Directory) => {
                return Err(FsError::new(ErrorCode::Eisdir)
                    .with_syscall("rename")
                    .with_path(&from)
                    .with_dest(&to));
            }
            _ => {}
        }

        if destination != Kind::Missing {
            self.inner.mark_orphan(&to)?;
            self.inner.remove_attributes(&to);
            self.inner.set_empty_directory(&to, false);
        }

        if directory {
            let base = key_of(&from, "rename")?;
            let prefix = prefix_of(&base);
            let keys = self
                .inner
                .keys_under(&base, &mut scope, "rename", &from)
                .await?;
            for key in keys {
                if !key.starts_with(&prefix) {
                    continue;
                }
                let child = path_of(&key);
                let destination = normalize_path(&format!("{to}{}", &child[from.len()..]));
                self.inner.move_value(&child, &destination).await?;
            }
        } else {
            self.inner.move_value(&from, &to).await?;
        }
        self.inner.move_paths(&from, &to)?;
        if directory {
            self.inner.set_empty_directory(&to, true);
        }
        self.inner.touch(&to, "rename")
    }

    async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("truncate", &path)?;
        let mut scope = Scope::default();
        self.inner
            .resolve_file(&path, "truncate", &mut scope)
            .await?;
        if let Some(entry) = self.inner.open_file(&path) {
            {
                let mut file = entry
                    .lock()
                    .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("truncate"))?;
                resize_bytes(&mut file.data, length, "truncate")?;
                file.dirty = true;
            }
            self.inner.touch(&path, "truncate")?;
            let refs = entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("truncate"))?
                .refs;
            if refs == 0 {
                return self.inner.flush_and_remove_closed(&entry).await;
            }
            return Ok(());
        }
        if length == 0 {
            return self.inner.write_value(&path, &[], "truncate").await;
        }
        let mut data = self.inner.read_value(&path, "truncate").await?;
        resize_bytes(&mut data, length, "truncate")?;
        self.inner.write_value(&path, &data, "truncate").await
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("chmod", &path)?;
        self.inner
            .stat_of(&path, "chmod", &mut Scope::default())
            .await?;
        let timestamp = now_ms();
        self.inner.update_attributes(&path, "chmod", |attributes| {
            attributes.mode = Some(mode & 0o7777);
            attributes.ctime_ms = Some(timestamp);
        })
    }

    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("chown", &path)?;
        self.inner
            .stat_of(&path, "chown", &mut Scope::default())
            .await?;
        let timestamp = now_ms();
        self.inner.update_attributes(&path, "chown", |attributes| {
            if uid != u32::MAX {
                attributes.uid = Some(uid);
            }
            if gid != u32::MAX {
                attributes.gid = Some(gid);
            }
            attributes.ctime_ms = Some(timestamp);
        })
    }

    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.chown(path, uid, gid).await
    }

    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        let path = normalize_path(path);
        self.inner.mutable("utime", &path)?;
        self.inner
            .stat_of(&path, "utime", &mut Scope::default())
            .await?;
        let timestamp = now_ms();
        self.inner.update_attributes(&path, "utime", |attributes| {
            attributes.atime_ms = Some(atime_ms);
            attributes.mtime_ms = Some(mtime_ms);
            attributes.ctime_ms = Some(timestamp);
        })
    }

    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.utimes(path, atime_ms, mtime_ms).await
    }
}

fn remap_path(from: &str, to: &str, path: &str) -> String {
    normalize_path(&format!("{to}{}", &path[from.len()..]))
}

impl<S> Inner<S>
where
    S: KeyValueStore,
{
    async fn classify(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<Kind> {
        if path == "/" {
            return Ok(Kind::Directory);
        }
        if self.has_open_file(path) {
            return Ok(Kind::File);
        }
        let key = key_of(path, syscall)?;
        if self.has_key(&key, scope, syscall, path).await? {
            return Ok(Kind::File);
        }
        if self.has_empty_directory(path) {
            return Ok(Kind::Directory);
        }
        Ok(
            if self
                .keys_under(&key, scope, syscall, path)
                .await?
                .is_empty()
            {
                Kind::Missing
            } else {
                Kind::Directory
            },
        )
    }

    async fn lookup(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<Kind> {
        let kind = self.classify(path, syscall, scope).await?;
        if kind == Kind::Missing {
            self.require_directory(&dirname(path), syscall, path, scope)
                .await?;
            return Ok(kind);
        }
        let parent = dirname(path);
        if parent != "/"
            && self
                .has_key(&key_of(&parent, syscall)?, scope, syscall, path)
                .await?
        {
            return Err(FsError::new(ErrorCode::Enotdir)
                .with_syscall(syscall)
                .with_path(path));
        }
        Ok(kind)
    }

    async fn require_directory(
        &self,
        path: &str,
        syscall: &str,
        reported: &str,
        scope: &mut Scope,
    ) -> Result<()> {
        if path == "/" {
            return Ok(());
        }
        let key = key_of(path, syscall)?;
        let segments = key.split(KEY_SEPARATOR).collect::<Vec<_>>();
        let mut current_path = String::new();
        let mut current_key = String::new();
        let mut paths = Vec::with_capacity(segments.len());
        let mut keys = Vec::with_capacity(segments.len());
        for segment in segments {
            current_path.push('/');
            current_path.push_str(segment);
            current_key = if current_key.is_empty() {
                segment.to_owned()
            } else {
                format!("{current_key}{KEY_SEPARATOR}{segment}")
            };
            paths.push(current_path.clone());
            keys.push(current_key.clone());
        }

        for (component_path, component_key) in paths.iter().zip(keys.iter()) {
            if self.has_open_file(component_path)
                || self
                    .has_key(component_key, scope, syscall, reported)
                    .await?
            {
                return Err(FsError::new(ErrorCode::Enotdir)
                    .with_syscall(syscall)
                    .with_path(normalize_path(reported)));
            }
        }
        let deepest = paths.last().expect("non-root path has a component");
        if self.has_empty_directory(deepest) {
            return Ok(());
        }
        if self
            .keys_under(
                keys.last().expect("non-root path has a key"),
                scope,
                syscall,
                reported,
            )
            .await?
            .is_empty()
        {
            return Err(FsError::new(ErrorCode::Enoent)
                .with_syscall(syscall)
                .with_path(normalize_path(reported)));
        }
        Ok(())
    }

    async fn resolve_file(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<()> {
        match self.lookup(path, syscall, scope).await? {
            Kind::File => Ok(()),
            Kind::Directory => Err(FsError::new(ErrorCode::Eisdir)
                .with_syscall(syscall)
                .with_path(path)),
            Kind::Missing => Err(FsError::new(ErrorCode::Enoent)
                .with_syscall(syscall)
                .with_path(path)),
        }
    }

    async fn has_entries(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<bool> {
        if !self
            .keys_under(&key_of(path, syscall)?, scope, syscall, path)
            .await?
            .is_empty()
        {
            return Ok(true);
        }
        Ok(!members_below(&self.tables.load().empty_directories, path).is_empty())
    }

    fn mutable(&self, syscall: &str, path: &str) -> Result<()> {
        if self.options.read_only {
            return Err(FsError::new(ErrorCode::Erofs)
                .with_syscall(syscall)
                .with_path(normalize_path(path)));
        }
        Ok(())
    }

    fn make_stats(
        &self,
        attributes: Attributes,
        mode: u32,
        size: u64,
        metadata: KeyValueMetadata,
    ) -> Stats {
        let kind = mode & S_IFMT;
        let mtime_ms = attributes
            .mtime_ms
            .or(metadata.mtime_ms)
            .unwrap_or(self.created_ms);
        Stats {
            dev: 0,
            ino: attributes.ino,
            mode,
            nlink: if kind == S_IFDIR { 2 } else { 1 },
            uid: attributes.uid.unwrap_or(self.options.uid),
            gid: attributes.gid.unwrap_or(self.options.gid),
            rdev: 0,
            size,
            blksize: BLOCK_SIZE,
            blocks: size.div_ceil(512),
            atime_ms: attributes
                .atime_ms
                .or(metadata.atime_ms)
                .unwrap_or(mtime_ms),
            mtime_ms,
            ctime_ms: attributes
                .ctime_ms
                .or(metadata.ctime_ms)
                .unwrap_or(mtime_ms),
            birthtime_ms: metadata.birthtime_ms.unwrap_or(mtime_ms),
        }
    }

    fn directory_stats(&self, path: &str) -> Result<Stats> {
        let attributes = self.attributes_of(path)?;
        let mode = S_IFDIR | (attributes.mode.unwrap_or(self.options.dir_mode) & 0o7777);
        Ok(self.make_stats(attributes, mode, BLOCK_SIZE, KeyValueMetadata::default()))
    }

    async fn file_stats(
        &self,
        path: &str,
        syscall: &str,
        entry: Option<Arc<OpenEntry>>,
    ) -> Result<Stats> {
        let attributes = self.attributes_of(path)?;
        let mode = S_IFREG | (attributes.mode.unwrap_or(self.options.file_mode) & 0o7777);
        let (orphan, open_size) = if let Some(entry) = &entry {
            let file = entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("fstat"))?;
            (file.orphan, Some(file.data.len() as u64))
        } else {
            (false, None)
        };
        if orphan {
            return Ok(self.make_stats(
                attributes,
                mode,
                open_size.unwrap_or(0),
                KeyValueMetadata::default(),
            ));
        }
        let key = key_of(path, syscall)?;
        let metadata = self
            .store
            .get_meta(&key)
            .await
            .map_err(|error| self.store_error(error, syscall, path))?;
        let size = if let Some(size) = open_size {
            size
        } else if let Some(size) = metadata.size {
            size
        } else {
            self.read_value(path, syscall).await?.len() as u64
        };
        Ok(self.make_stats(attributes, mode, size, metadata))
    }

    async fn stat_of(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<Stats> {
        match self.lookup(path, syscall, scope).await? {
            Kind::Directory => self.directory_stats(path),
            Kind::File => {
                // An open buffer is the file's contents until its last close,
                // including writes that have not reached the store yet.
                self.file_stats(path, syscall, self.open_file(path)).await
            }
            Kind::Missing => Err(FsError::new(ErrorCode::Enoent)
                .with_syscall(syscall)
                .with_path(path)),
        }
    }

    async fn lstat_of(&self, path: &str, syscall: &str, scope: &mut Scope) -> Result<Stats> {
        self.stat_of(path, syscall, scope).await
    }

    async fn acquire(
        &self,
        path: &str,
        known: Option<Vec<u8>>,
        syscall: &str,
    ) -> Result<Arc<OpenEntry>> {
        loop {
            let (existing, removals) = self.open_files.find(path);
            if let Some(entry) = existing {
                if self.join_open_file(&entry, syscall)? {
                    return Ok(entry);
                }
                // Closed under us and out of the table: look again.
                continue;
            }
            // Read again on every pass: the buffer that was just dropped
            // may have been saved since the last read.
            let data = match &known {
                Some(data) => data.clone(),
                None => self.read_value(path, syscall).await?,
            };
            let candidate = Arc::new(OpenEntry {
                file: ParkCell::new(OpenFile {
                    path: path.to_owned(),
                    data,
                    refs: 1,
                    dirty: false,
                    orphan: false,
                    detached: false,
                    attributes: None,
                }),
                saving: TurnGate::new(),
            });
            // The candidate already counts its one handle and nobody can
            // reach it until the table holds it. It goes in only if no
            // buffer has left the bucket since the look above: one for
            // this path that was opened, written, saved and dropped during
            // the read would leave this one holding contents older than
            // the store's, and its save would undo that write.
            match self.open_files.insert_if_absent(path, &candidate, removals) {
                Inserted::Done => return Ok(candidate),
                Inserted::Existing(entry) => {
                    if self.join_open_file(&entry, syscall)? {
                        return Ok(entry);
                    }
                }
                Inserted::Stale => {}
            }
        }
    }

    async fn flush(&self, entry: &Arc<OpenEntry>) -> Result<()> {
        // Taken before looking at the buffer, even a clean one: a save in
        // flight has already marked it clean, and whoever sees it clean
        // must find the store already holding it.
        let _saving = entry.saving.lock().await;
        self.save(entry).await
    }

    /// Saves the buffer, then drops it from the open-file table if its
    /// last handle is closed. Both happen in one turn of the entry's
    /// saves: the buffer is dropped for being clean, and it is marked
    /// clean when a save starts, so a save by a later handle still on its
    /// way to the store would otherwise let the next open read the store
    /// too early and build its buffer without that save.
    async fn flush_and_remove_closed(&self, entry: &Arc<OpenEntry>) -> Result<()> {
        let _saving = entry.saving.lock().await;
        let result = self.save(entry).await;
        let cleanup = self.remove_closed(entry);
        result.and(cleanup)
    }

    /// The body of a save. The caller holds the entry's saving turn.
    async fn save(&self, entry: &Arc<OpenEntry>) -> Result<()> {
        let snapshot = {
            let mut file = entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("fsync"))?;
            if !file.dirty || file.orphan {
                return Ok(());
            }
            file.dirty = false;
            (file.path.clone(), file.data.clone())
        };
        if let Err(error) = self.write_value(&snapshot.0, &snapshot.1, "fsync").await {
            if let Ok(mut file) = entry.lock() {
                file.dirty = true;
            }
            return Err(error);
        }
        Ok(())
    }

    async fn release(&self, entry: &Arc<OpenEntry>) -> Result<()> {
        {
            let mut file = entry
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("close"))?;
            file.refs = file.refs.saturating_sub(1);
        }
        self.flush_and_remove_closed(entry).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::future::Future;
    use std::sync::Mutex;
    use std::task::{Context, Poll, Waker};

    thread_local! {
        /// Run once, on this thread, by the next handle call that passes
        /// its first look at `closed`, before it takes the entry's lock:
        /// the one place a whole close can fit between the two.
        static AFTER_BEGIN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    }

    pub(super) fn after_begin() {
        if let Some(hook) = AFTER_BEGIN.with(|hook| hook.borrow_mut().take()) {
            hook();
        }
    }

    fn set_after_begin(hook: impl FnOnce() + 'static) {
        AFTER_BEGIN.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    fn hook_ran() -> bool {
        AFTER_BEGIN.with(|slot| slot.borrow().is_none())
    }

    #[derive(Clone, Default)]
    struct Store(Arc<Mutex<HashMap<String, Vec<u8>>>>);

    impl Store {
        fn bytes(&self, key: &str) -> Option<Vec<u8>> {
            self.0.lock().expect("store").get(key).cloned()
        }
    }

    #[async_trait]
    impl KeyValueStore for Store {
        type Error = std::convert::Infallible;

        async fn has_item(&self, key: &str) -> std::result::Result<bool, Self::Error> {
            Ok(self.0.lock().expect("store").contains_key(key))
        }

        async fn get_item_raw(
            &self,
            key: &str,
        ) -> std::result::Result<Option<Vec<u8>>, Self::Error> {
            Ok(self.bytes(key))
        }

        async fn set_item_raw(
            &self,
            key: &str,
            value: Vec<u8>,
        ) -> std::result::Result<(), Self::Error> {
            self.0.lock().expect("store").insert(key.to_owned(), value);
            Ok(())
        }

        async fn remove_item(&self, key: &str) -> std::result::Result<(), Self::Error> {
            self.0.lock().expect("store").remove(key);
            Ok(())
        }

        async fn get_keys(&self, prefix: &str) -> std::result::Result<Vec<String>, Self::Error> {
            let store = self.0.lock().expect("store");
            Ok(store
                .keys()
                .filter(|key| key.starts_with(prefix))
                .cloned()
                .collect())
        }

        async fn get_keys_bounded(
            &self,
            prefix: &str,
            _max_keys: usize,
        ) -> std::result::Result<Option<Vec<String>>, Self::Error> {
            self.get_keys(prefix).await.map(Some)
        }
    }

    /// The store answers at once, so every call here finishes in one poll
    /// on the calling thread: the hook runs a whole close in the middle of
    /// a write with no second thread and no timing.
    fn now<F: Future>(future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(output) => output,
            Poll::Pending => panic!("the in-memory store never waits"),
        }
    }

    fn open_seeded() -> (Store, KeyValueFs<Store>, Arc<dyn FileHandle>) {
        let store = Store::default();
        now(store.set_item_raw("f", vec![0])).expect("seed");
        let fs = create_unstorage_driver(store.clone(), UnstorageOptions::default());
        let handle = now(fs.open("/f", "r+", 0)).expect("open");
        (store, fs, handle)
    }

    /// The whole close runs after the write's first look at `closed` and
    /// before the write takes the entry's lock. The buffer has left the
    /// open-file table by then and nothing will save it again, so the
    /// write must not be acknowledged.
    #[test]
    fn a_write_overtaken_by_the_whole_close_is_refused_not_lost() {
        let (store, _fs, handle) = open_seeded();
        let closing = Arc::clone(&handle);
        set_after_begin(move || now(closing.close()).expect("close"));
        let outcome = now(handle.write(&[7], Some(0)));
        assert!(hook_ran(), "the close did not run inside the write");
        if outcome.is_ok() {
            assert_eq!(
                store.bytes("f"),
                Some(vec![7]),
                "a write was acknowledged after its handle's close had returned, \
                 and is not in the store"
            );
        }
        assert_eq!(outcome.map_err(|error| error.code), Err(ErrorCode::Ebadf));
    }

    #[test]
    fn a_truncate_overtaken_by_the_whole_close_is_refused_not_lost() {
        let (store, _fs, handle) = open_seeded();
        let closing = Arc::clone(&handle);
        set_after_begin(move || now(closing.close()).expect("close"));
        let outcome = now(handle.truncate(0));
        assert!(hook_ran(), "the close did not run inside the truncate");
        if outcome.is_ok() {
            assert_eq!(
                store.bytes("f"),
                Some(Vec::new()),
                "a truncate was acknowledged after its handle's close had returned, \
                 and is not in the store"
            );
        }
        assert_eq!(outcome.map_err(|error| error.code), Err(ErrorCode::Ebadf));
    }

    /// The same gap for a read. After the close, the file is rewritten
    /// through another handle; a read let through would answer from the
    /// buffer the close dropped, which no longer follows the file.
    #[test]
    fn a_read_overtaken_by_the_whole_close_does_not_answer_from_the_dropped_buffer() {
        let (_store, fs, handle) = open_seeded();
        let closing = Arc::clone(&handle);
        set_after_begin(move || {
            now(closing.close()).expect("close");
            let other = now(fs.open("/f", "r+", 0)).expect("open other");
            now(other.write(&[9], Some(0))).expect("write other");
            now(other.close()).expect("close other");
        });
        let mut seen = [0xff];
        let outcome = now(handle.read(&mut seen, Some(0)));
        assert!(hook_ran(), "the close did not run inside the read");
        if outcome.is_ok() {
            assert_eq!(seen, [9], "a read answered from a buffer its close dropped");
        }
        assert_eq!(outcome.map_err(|error| error.code), Err(ErrorCode::Ebadf));
    }
}
