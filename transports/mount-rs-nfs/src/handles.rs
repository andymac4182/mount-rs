//! NFSv3 file handles and directory-cookie snapshots.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use arc_swap::ArcSwapOption;
use mount_rs_core::park_cell::ParkCell;
use mount_rs_core::{ErrorCode, FsDriver, FsError, GuardedDirectoryEntry, Stats};

use crate::constants::{NFS3_COOKIEVERFSIZE, NFS3_FHSIZE};

const FH_MAGIC: u32 = 0x554e_4653; // "UNFS"
pub const FH_SIZE: usize = 20;
pub const ROOT_HANDLE_ID: u64 = 1;

/// A rename between two names of this inode is a backend no-op. Compare
/// lstat identities, not paths or followed symlink targets.
pub(crate) fn same_backend_inode(source: &Stats, destination: &Stats) -> bool {
    source.ino > 0 && source.ino == destination.ino && source.dev == destination.dev
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandleEntry {
    pub id: u64,
    pub fileid: u64,
    pub key: Option<String>,
    pub path: String,
}

#[derive(Debug)]
struct Entry {
    id: u64,
    fileid: u64,
    key: Option<String>,
    paths: BTreeSet<String>,
    pins: usize,
    /// This entry's key in `HandleState::lru`; zero while it has none.
    used: u64,
}

#[derive(Debug)]
struct HandleState {
    next_id: u64,
    by_id: HashMap<u64, Entry>,
    by_path: HashMap<String, u64>,
    by_key: HashMap<String, u64>,
    /// Handle ids by when each was last used, oldest first. Keyed by a
    /// counter rather than kept as a list so that using a handle does not
    /// scan every other handle: each NFS operation decodes at least one.
    lru: BTreeMap<u64, u64>,
    next_use: u64,
}

/// Formats a cell the way a mutex formats itself: the value when it is free.
pub(crate) struct CellDebug<'a, T>(pub(crate) &'a ParkCell<T>);

impl<T: std::fmt::Debug> std::fmt::Debug for CellDebug<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0.try_lock() {
            Some(Ok(value)) => std::fmt::Debug::fmt(&*value, f),
            Some(Err(())) => f.write_str("<poisoned>"),
            None => f.write_str("<locked>"),
        }
    }
}

#[derive(Clone)]
pub struct FileHandleTable {
    state: Arc<ParkCell<HandleState>>,
    /// Fixed for the life of the table, so every clone carries its own copy
    /// and building or checking a handle does not take the cell.
    verifier: [u8; 8],
    /// Set at most once and never cleared; shared by every clone.
    stable_inode_ids: Arc<AtomicBool>,
    use_driver_ino: bool,
    max_handles: usize,
}

impl std::fmt::Debug for FileHandleTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileHandleTable")
            .field("state", &CellDebug(&self.state))
            .field("verifier", &self.verifier)
            .field("stable_inode_ids", &self.stable_inode_ids)
            .field("use_driver_ino", &self.use_driver_ino)
            .field("max_handles", &self.max_handles)
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FileHandleTableOptions {
    pub use_driver_ino: bool,
    pub verifier: Option<[u8; 8]>,
    /// Positive values bound the table with a soft LRU cap. Zero means no cap.
    pub max_handles: Option<usize>,
}

impl Default for FileHandleTableOptions {
    fn default() -> Self {
        Self {
            use_driver_ino: true,
            verifier: None,
            max_handles: None,
        }
    }
}

impl FileHandleTable {
    pub fn new(options: FileHandleTableOptions) -> Self {
        let verifier = options.verifier.unwrap_or_else(default_verifier);
        let root = Entry {
            id: ROOT_HANDLE_ID,
            fileid: ROOT_HANDLE_ID,
            key: None,
            paths: BTreeSet::from([String::from("/")]),
            pins: 0,
            used: 0,
        };
        let mut by_id = HashMap::new();
        by_id.insert(ROOT_HANDLE_ID, root);
        Self {
            state: Arc::new(ParkCell::new(HandleState {
                next_id: ROOT_HANDLE_ID + 1,
                by_id,
                by_path: HashMap::from([(String::from("/"), ROOT_HANDLE_ID)]),
                by_key: HashMap::new(),
                lru: BTreeMap::new(),
                next_use: 1,
            })),
            verifier,
            stable_inode_ids: Arc::new(AtomicBool::new(false)),
            use_driver_ino: options.use_driver_ino,
            max_handles: options.max_handles.filter(|value| *value > 0).unwrap_or(0),
        }
    }

    pub fn verifier(&self) -> [u8; 8] {
        self.verifier
    }

    pub fn size(&self) -> usize {
        self.state.lock().expect("handle table lock").by_id.len()
    }

    /// Trust backend inode keys across remote rename and name replacement.
    /// Enable this only when the driver guarantees IDs cannot be reused.
    pub(crate) fn trust_stable_inode_ids(&self) {
        self.stable_inode_ids.store(true, Ordering::Release);
    }

    /// Return one stable snapshot of every live handle, ordered by handle id.
    ///
    /// The table is shared by the NFSv3 and NFSv4.1 sessions owned by one
    /// server. Keeping the snapshot sorted makes read-only embedding views
    /// deterministic without exposing the table's internal mutex or maps.
    pub fn entries(&self) -> Vec<HandleEntry> {
        let state = self.state.lock().expect("handle table lock");
        let mut entries = state
            .by_id
            .values()
            .map(|entry| HandleEntry {
                id: entry.id,
                fileid: entry.fileid,
                key: entry.key.clone(),
                path: entry.paths.iter().next().cloned().unwrap_or_default(),
            })
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.id);
        entries
    }

    pub fn root(&self) -> HandleEntry {
        self.entry(ROOT_HANDLE_ID).expect("root handle")
    }

    pub fn encode(&self, entry: &HandleEntry) -> Vec<u8> {
        let mut handle = vec![0; FH_SIZE];
        handle[0..4].copy_from_slice(&FH_MAGIC.to_be_bytes());
        handle[4..12].copy_from_slice(&self.verifier);
        handle[12..20].copy_from_slice(&entry.id.to_be_bytes());
        handle
    }

    pub fn decode(&self, handle: &[u8]) -> mount_rs_core::Result<HandleEntry> {
        if handle.len() != FH_SIZE || handle.len() > NFS3_FHSIZE {
            return Err(stale("file handle has an invalid length"));
        }
        if handle[0..4] != FH_MAGIC.to_be_bytes() {
            return Err(stale("file handle has the wrong magic"));
        }
        if handle[4..12] != self.verifier {
            return Err(stale("file handle is from a previous server instance"));
        }
        let id = u64::from_be_bytes(handle[12..20].try_into().unwrap());
        let mut state = self.state.lock().expect("handle table lock");
        let entry = state.by_id.get(&id).map(|entry| HandleEntry {
            id: entry.id,
            fileid: entry.fileid,
            key: entry.key.clone(),
            path: entry.paths.iter().next().cloned().unwrap_or_default(),
        });
        let entry = entry.ok_or_else(|| stale("unknown file handle"))?;
        touch_locked(&mut state, id);
        Ok(entry)
    }

    pub fn resolve(&self, handle: &[u8]) -> mount_rs_core::Result<String> {
        let entry = self.decode(handle)?;
        self.path_of(&entry)
    }

    pub fn path_of(&self, entry: &HandleEntry) -> mount_rs_core::Result<String> {
        let mut state = self.state.lock().expect("handle table lock");
        let current = state
            .by_id
            .get(&entry.id)
            .ok_or_else(|| stale("unknown file handle"))?;
        let path = current
            .paths
            .iter()
            .find(|path| state.by_path.get(*path) == Some(&entry.id))
            .cloned()
            .ok_or_else(|| stale("file handle names a removed file"))?;
        touch_locked(&mut state, entry.id);
        Ok(path)
    }

    /// Resolve a shared backing object's handle through an alias that still
    /// names its original inode. A different server may rename or unlink a
    /// remembered path without updating this server's local handle table.
    pub(crate) async fn live_path_of(
        &self,
        entry: &HandleEntry,
        driver: &dyn FsDriver,
        preserve_orphan_key: bool,
    ) -> mount_rs_core::Result<String> {
        if entry.id == ROOT_HANDLE_ID {
            return self.path_of(entry);
        }
        let aliases = {
            let state = self.state.lock().expect("handle table lock");
            let current = state
                .by_id
                .get(&entry.id)
                .ok_or_else(|| stale("unknown file handle"))?;
            current
                .paths
                .iter()
                .filter(|path| state.by_path.get(*path) == Some(&entry.id))
                .cloned()
                .collect::<Vec<_>>()
        };
        for path in aliases {
            let observed = match driver.lstat(&path).await {
                // Match NFS's existing stat_of fallback for drivers without
                // lstat. Such drivers cannot distinguish symlink identity.
                Err(error) if error.code == ErrorCode::Enosys => driver.stat(&path).await,
                result => result,
            };
            let live = match observed {
                Ok(stats) => {
                    if let Some(key) = entry.key.as_deref() {
                        stats.ino > 0 && key == format!("{}:{}", stats.dev, stats.ino)
                    } else {
                        stats.ino == 0 || stats.ino == entry.fileid
                    }
                }
                Err(error) if matches!(error.code, ErrorCode::Enoent | ErrorCode::Estale) => false,
                Err(error) => return Err(error),
            };
            if live {
                // A concurrent local request may have rebound this path to a
                // different handle while the backend stat was in flight.
                if self.at(&path).is_some_and(|bound| bound.id == entry.id) {
                    return Ok(path);
                }
            } else {
                self.discard_stale_alias(entry.id, &path, preserve_orphan_key);
            }
        }
        Err(stale("file handle has no live name"))
    }

    fn discard_stale_alias(&self, id: u64, path: &str, preserve_orphan_key: bool) {
        let mut state = self.state.lock().expect("handle table lock");
        if state.by_path.get(path) != Some(&id) {
            return;
        }
        detach_preserve_locked(&mut state, id, path);
        let orphan_key = state
            .by_id
            .get(&id)
            .filter(|entry| entry.paths.is_empty())
            .and_then(|entry| entry.key.clone());
        // A retained file descriptor pins an inode; a backend with stable
        // inode IDs also cannot reuse this key after unlink. In either case,
        // a remote rename can later bind its new name to the original opaque
        // handle. Other backends drop the key to prevent inode reuse from
        // reviving an orphaned handle.
        if let Some(key) = orphan_key
            && !preserve_orphan_key
            && state.by_key.get(&key) == Some(&id)
        {
            state.by_key.remove(&key);
        }
        touch_locked(&mut state, id);
    }

    pub fn entry(&self, id: u64) -> Option<HandleEntry> {
        let state = self.state.lock().expect("handle table lock");
        state.by_id.get(&id).map(|entry| HandleEntry {
            id: entry.id,
            fileid: entry.fileid,
            key: entry.key.clone(),
            path: entry.paths.iter().next().cloned().unwrap_or_default(),
        })
    }

    pub fn at(&self, path: &str) -> Option<HandleEntry> {
        let mut state = self.state.lock().expect("handle table lock");
        let id = *state.by_path.get(path)?;
        let entry = state.by_id.get(&id).map(|entry| HandleEntry {
            id: entry.id,
            fileid: entry.fileid,
            key: entry.key.clone(),
            path: path.to_owned(),
        })?;
        touch_locked(&mut state, id);
        Some(entry)
    }

    pub fn bind(&self, path: &str, stats: &Stats) -> HandleEntry {
        self.bind_inner(path, stats, false)
    }

    /// Bind and provisionally pin the entry under one table lock. A parallel
    /// rebind cannot retire a newly issued regular-file handle before the
    /// caller starts opening its retained backend descriptor.
    pub(crate) fn bind_pinned(&self, path: &str, stats: &Stats) -> HandleEntry {
        self.bind_inner(path, stats, true)
    }

    fn bind_inner(&self, path: &str, stats: &Stats, pin: bool) -> HandleEntry {
        let key = if self.use_driver_ino && stats.ino > 0 {
            Some(format!("{}:{}", stats.dev, stats.ino))
        } else {
            None
        };
        let mut state = self.state.lock().expect("handle table lock");
        let previous = state.by_path.get(path).copied();
        let by_key = key
            .as_ref()
            .and_then(|value| state.by_key.get(value).copied());
        let same_file = previous.is_some_and(|id| {
            state
                .by_id
                .get(&id)
                .is_some_and(|entry| entry.key.is_none() || entry.key == key)
        });
        let id = by_key
            .or_else(|| same_file.then(|| previous.expect("same path has an entry")))
            .unwrap_or_else(|| {
                let id = state.next_id;
                state.next_id = state.next_id.saturating_add(1);
                state.by_id.insert(
                    id,
                    Entry {
                        id,
                        fileid: if stats.ino > 0 { stats.ino } else { id },
                        key: key.clone(),
                        paths: BTreeSet::new(),
                        pins: 0,
                        used: 0,
                    },
                );
                id
            });
        if let Some(previous) = previous.filter(|old| *old != id) {
            // A retained descriptor still owns the old inode even after
            // another mount reuses this name. Keep its opaque handle alive
            // without leaving the new name or inode key bound to it.
            detach_replaced_locked(
                &mut state,
                previous,
                path,
                self.stable_inode_ids.load(Ordering::Acquire),
            );
        }
        let bound_key = if let Some(entry) = state.by_id.get_mut(&id) {
            if entry.key.is_none() {
                entry.key = key.clone();
            }
            entry.fileid = if stats.ino > 0 { stats.ino } else { id };
            entry.paths.insert(path.to_owned());
            if pin {
                entry.pins = entry.pins.saturating_add(1);
            }
            entry.key.clone()
        } else {
            None
        };
        state.by_path.insert(path.to_owned(), id);
        if let Some(key) = bound_key {
            state.by_key.insert(key, id);
        }
        touch_locked(&mut state, id);
        enforce_limit_locked(&mut state, self.max_handles, id);
        entry_locked(&state, id, path)
    }

    /// Hold an entry against LRU eviction while backend state refers to it.
    ///
    /// A retained descriptor can keep a displaced inode's opaque handle
    /// pathless until the descriptor closes. A missing entry makes this a
    /// no-op, which keeps teardown and error paths safe to balance.
    pub fn pin(&self, id: u64) {
        let _ = self.try_pin(id);
    }

    /// Atomically pin a live entry, so callers retaining a descriptor do not
    /// insert one for an ID that has already been retired by a concurrent bind.
    pub(crate) fn try_pin(&self, id: u64) -> bool {
        let mut state = self.state.lock().expect("handle table lock");
        if let Some(entry) = state.by_id.get_mut(&id) {
            entry.pins = entry.pins.saturating_add(1);
            true
        } else {
            false
        }
    }

    /// Release one NFSv4 pin without allowing the count to underflow.
    pub fn unpin(&self, id: u64) {
        let mut state = self.state.lock().expect("handle table lock");
        let mut retire = false;
        if let Some(entry) = state.by_id.get_mut(&id)
            && entry.pins > 0
        {
            entry.pins -= 1;
            retire = entry.pins == 0 && entry.paths.is_empty();
        }
        if retire {
            drop_entry_locked(&mut state, id);
        }
        enforce_limit_locked(&mut state, self.max_handles, ROOT_HANDLE_ID);
    }

    pub fn forget(&self, path: &str) {
        let mut state = self.state.lock().expect("handle table lock");
        let paths: Vec<String> = state
            .by_path
            .keys()
            .filter(|candidate| *candidate == path || candidate.starts_with(&format!("{path}/")))
            .cloned()
            .collect();
        for path in paths {
            if let Some(id) = state.by_path.get(&path).copied() {
                detach_locked(&mut state, id, &path);
            }
        }
    }

    /// Remove a name without invalidating the opaque handle for the object.
    ///
    /// NFSv3 has no OPEN operation, so a server that wants POSIX unlink
    /// semantics must keep the backend object alive independently of the
    /// namespace name.  The caller owns that backend lifetime; this method
    /// only makes the filehandle path-less while retaining its identity for
    /// a held backend handle.  A new object is never allowed to inherit the
    /// old inode key after the last name is removed.
    pub fn orphan(&self, path: &str) -> Option<HandleEntry> {
        let mut state = self.state.lock().expect("handle table lock");
        let id = state.by_path.get(path).copied()?;
        detach_preserve_locked(&mut state, id, path);
        let key = state
            .by_id
            .get(&id)
            .filter(|entry| entry.paths.is_empty())
            .and_then(|entry| entry.key.clone());
        if let Some(key) = key {
            state.by_key.remove(&key);
        }
        touch_locked(&mut state, id);
        enforce_limit_locked(&mut state, self.max_handles, id);
        state.by_id.get(&id).map(|entry| HandleEntry {
            id: entry.id,
            fileid: entry.fileid,
            key: entry.key.clone(),
            path: path.to_owned(),
        })
    }

    /// Move all remembered names below `old_path` to `new_path` after a
    /// successful driver rename. The driver remains the authority for whether
    /// the operation was legal; this only preserves handle identity.
    pub fn remap(&self, old_path: &str, new_path: &str) {
        if old_path == new_path {
            return;
        }
        let mut state = self.state.lock().expect("handle table lock");
        let old_prefix = format!("{old_path}/");
        let affected: Vec<(String, u64)> = state
            .by_path
            .iter()
            .filter(|(path, _)| *path == old_path || path.starts_with(&old_prefix))
            .map(|(path, id)| (path.clone(), *id))
            .collect();

        // A rename must preserve a handle even when its old name was the
        // entry's only remembered path. Remove the destination subtree first
        // (the filesystem rename has already decided what it replaced), then
        // move source names without allowing `detach_locked` to delete their
        // entries before the replacement name is installed.
        let new_prefix = format!("{new_path}/");
        let destinations: Vec<(String, u64)> = state
            .by_path
            .iter()
            .filter(|(path, _)| **path == new_path || path.starts_with(&new_prefix))
            .map(|(path, id)| (path.clone(), *id))
            .collect();
        for (path, id) in destinations {
            detach_replaced_locked(
                &mut state,
                id,
                &path,
                self.stable_inode_ids.load(Ordering::Acquire),
            );
        }
        for (path, id) in affected {
            let suffix = path.strip_prefix(old_path).unwrap_or_default();
            let replacement = format!("{new_path}{suffix}");
            detach_preserve_locked(&mut state, id, &path);
            if let Some(entry) = state.by_id.get_mut(&id) {
                entry.paths.insert(replacement.clone());
                state.by_path.insert(replacement, id);
            }
            touch_locked(&mut state, id);
        }
    }

    pub fn clear(&self) {
        let root = Entry {
            id: ROOT_HANDLE_ID,
            fileid: ROOT_HANDLE_ID,
            key: None,
            paths: BTreeSet::from([String::from("/")]),
            pins: 0,
            used: 0,
        };
        let mut state = self.state.lock().expect("handle table lock");
        state.next_id = ROOT_HANDLE_ID + 1;
        state.by_id.clear();
        state.by_id.insert(ROOT_HANDLE_ID, root);
        state.by_path.clear();
        state.by_path.insert("/".to_owned(), ROOT_HANDLE_ID);
        state.by_key.clear();
        state.lru.clear();
    }
}

impl Default for FileHandleTable {
    fn default() -> Self {
        Self::new(FileHandleTableOptions::default())
    }
}

fn entry_locked(state: &HandleState, id: u64, path: &str) -> HandleEntry {
    let entry = state.by_id.get(&id).expect("bound handle entry");
    HandleEntry {
        id,
        fileid: entry.fileid,
        key: entry.key.clone(),
        path: path.to_owned(),
    }
}

fn touch_locked(state: &mut HandleState, id: u64) {
    if id == ROOT_HANDLE_ID || !state.by_id.contains_key(&id) {
        return;
    }
    let used = state.next_use;
    state.next_use += 1;
    let Some(entry) = state.by_id.get_mut(&id) else {
        return;
    };
    let previous = std::mem::replace(&mut entry.used, used);
    state.lru.remove(&previous);
    state.lru.insert(used, id);
}

fn drop_entry_locked(state: &mut HandleState, id: u64) {
    let Some(entry) = state.by_id.remove(&id) else {
        return;
    };
    let used = entry.used;
    for path in entry.paths {
        if state.by_path.get(&path) == Some(&id) {
            state.by_path.remove(&path);
        }
    }
    if let Some(key) = entry.key
        && state.by_key.get(&key) == Some(&id)
    {
        state.by_key.remove(&key);
    }
    state.lru.remove(&used);
}

fn enforce_limit_locked(state: &mut HandleState, max_handles: usize, protected: u64) {
    if max_handles == 0 {
        return;
    }
    while state.by_id.len() > max_handles {
        let victim = state.lru.values().copied().find(|candidate| {
            *candidate != protected
                && state
                    .by_id
                    .get(candidate)
                    .is_some_and(|entry| entry.pins == 0)
        });
        let Some(victim) = victim else {
            break;
        };
        drop_entry_locked(state, victim);
    }
}

fn detach_locked(state: &mut HandleState, id: u64, path: &str) {
    detach_preserve_locked(state, id, path);
    let remove = state
        .by_id
        .get(&id)
        .is_some_and(|entry| entry.paths.is_empty() && entry.id != ROOT_HANDLE_ID);
    if remove && id != ROOT_HANDLE_ID {
        drop_entry_locked(state, id);
    }
}

fn detach_replaced_locked(state: &mut HandleState, id: u64, path: &str, stable_inode_ids: bool) {
    detach_preserve_locked(state, id, path);
    let Some(entry) = state.by_id.get(&id) else {
        return;
    };
    if !entry.paths.is_empty() || id == ROOT_HANDLE_ID {
        return;
    }
    if stable_inode_ids {
        // Another server can rename this inode to an unknown alias before
        // placing a new inode at its old name. Its stable key lets a later
        // lookup reconnect the original opaque handle to the moved name.
        touch_locked(state, id);
    } else if entry.pins == 0 {
        drop_entry_locked(state, id);
    } else if let Some(key) = entry.key.clone()
        && state.by_key.get(&key) == Some(&id)
    {
        // A replaced inode can outlive its name through OPEN, but a new
        // object must not inherit that orphan's identity via the inode key.
        state.by_key.remove(&key);
    }
}

fn detach_preserve_locked(state: &mut HandleState, id: u64, path: &str) {
    state.by_path.remove(path);
    if let Some(entry) = state.by_id.get_mut(&id) {
        entry.paths.remove(path);
    }
}

fn stale(message: &str) -> FsError {
    FsError::new(ErrorCode::Estale).with_message(format!("ESTALE: {message}"))
}

fn default_verifier() -> [u8; 8] {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let pid = std::process::id() as u64;
    (nanos ^ pid.rotate_left(17)).to_be_bytes()
}

#[derive(Debug, Clone)]
pub struct DirectorySnapshot {
    pub names: Vec<String>,
    pub verifier: Vec<u8>,
}

/// The cache of directory listings that READDIR continues from.
///
/// A fixed table of slots, one listing each, with no lock: a reader loads a
/// slot and a writer publishes into one with a compare-and-swap. Every slot
/// has a use stamp, and a new directory takes an empty slot or else the slot
/// with the lowest stamp, so for one caller at a time the least recently
/// used listing is the one replaced.
#[derive(Clone)]
pub struct DirectorySnapshots {
    state: Arc<SnapshotSlots>,
}

struct CachedSnapshot {
    id: u64,
    /// When this listing was stored, from `SnapshotSlots::clock`. Of two
    /// listings of one directory, the one with the higher value is newer.
    stored: u64,
    snapshot: DirectorySnapshot,
}

type Listing = Option<Arc<CachedSnapshot>>;

struct SnapshotSlots {
    /// The listings. This is the only authority on what is cached.
    listings: Box<[ArcSwapOption<CachedSnapshot>]>,
    /// For each slot, the directory id it holds plus one, or zero when it is
    /// empty. Comparing these is much cheaper than loading every listing,
    /// but a tag is written after its listing, so the tags are only trusted
    /// when `writers` shows that nothing was written while they were read.
    tags: Box<[AtomicU64]>,
    /// For each slot, when its listing was last stored or read.
    used: Box<[AtomicU64]>,
    /// Source of `used` and `CachedSnapshot::stored` values.
    clock: AtomicU64,
    /// The low bits count the writes in progress and the high bits count
    /// the writes that have finished, so the value is unchanged across a
    /// scan of `tags` only if no write overlapped the scan.
    writers: AtomicU64,
}

const WRITE_STARTED: u64 = 1;
const WRITES_IN_PROGRESS: u64 = (1 << 20) - 1;
const WRITE_FINISHED: u64 = (1 << 20) - WRITE_STARTED;

fn tag_of(id: u64) -> Option<u64> {
    id.checked_add(1)
}

fn same_listing(a: &Listing, b: &Listing) -> bool {
    a.as_ref().map(Arc::as_ptr) == b.as_ref().map(Arc::as_ptr)
}

impl SnapshotSlots {
    fn tick(&self) -> u64 {
        self.clock.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// The `writers` value if no write is in progress.
    fn quiet(&self) -> Option<u64> {
        let writers = self.writers.load(Ordering::SeqCst);
        (writers & WRITES_IN_PROGRESS == 0).then_some(writers)
    }

    /// Make a slot's tag agree with its listing. Every writer calls this
    /// after it changes a slot. A writer leaves only after it has seen the
    /// listing its tag describes, so once the writers of a slot have all
    /// left, the tag is right.
    fn retag(&self, index: usize) {
        let tag_now = || {
            self.listings[index]
                .load()
                .as_ref()
                .map_or(0, |cached| cached.id + 1)
        };
        loop {
            let tag = tag_now();
            self.tags[index].store(tag, Ordering::SeqCst);
            if tag_now() == tag {
                return;
            }
        }
    }

    /// Replace what a slot holds, if it still holds `current`.
    fn publish(&self, index: usize, current: &Listing, new: Listing) -> bool {
        self.writers.fetch_add(WRITE_STARTED, Ordering::SeqCst);
        let previous = self.listings[index].compare_and_swap(current, new);
        let published = same_listing(&previous, current);
        if published {
            self.retag(index);
        }
        self.writers.fetch_add(WRITE_FINISHED, Ordering::SeqCst);
        published
    }

    /// The slot holding a directory's listing, and that listing. Callers
    /// that store one directory at the same moment can leave it in two slots
    /// until one of them tidies up; the newest of them is the one returned,
    /// so a caller never reads a listing older than one it has already seen.
    fn find(&self, id: u64, tag: u64) -> Option<(usize, Arc<CachedSnapshot>)> {
        if let Some(writers) = self.quiet() {
            let mut tagged = self
                .tags
                .iter()
                .enumerate()
                .filter(|(_, candidate)| candidate.load(Ordering::SeqCst) == tag)
                .map(|(index, _)| index);
            let (first, second) = (tagged.next(), tagged.next());
            if second.is_none() && self.writers.load(Ordering::SeqCst) == writers {
                let index = first?;
                if let Some(cached) = self.listings[index].load_full()
                    && cached.id == id
                {
                    return Some((index, cached));
                }
            }
        }
        // A write overlapped the look at the tags, or the directory is in
        // two slots: ask the listings.
        self.listings
            .iter()
            .enumerate()
            .filter_map(|(index, listing)| {
                listing
                    .load_full()
                    .filter(|cached| cached.id == id)
                    .map(|cached| (index, cached))
            })
            .max_by_key(|(_, cached)| cached.stored)
    }

    /// The slot a new listing of `id` should go in, with what it holds now:
    /// the slot that already holds the directory, else an empty slot, else
    /// the least recently used one. Also reports whether more than one slot
    /// holds the directory, which the caller must then put right.
    fn choose(&self, id: u64, tag: u64) -> (usize, Listing, bool) {
        if let Some(writers) = self.quiet() {
            let mut same = None;
            let mut crowded = false;
            let mut empty = None;
            for (index, candidate) in self.tags.iter().enumerate() {
                let candidate = candidate.load(Ordering::SeqCst);
                if candidate == tag {
                    crowded = same.is_some();
                    same = same.or(Some(index));
                } else if candidate == 0 && empty.is_none() {
                    empty = Some(index);
                }
            }
            let index = same.or(empty).or_else(|| {
                (0..self.used.len()).min_by_key(|index| self.used[*index].load(Ordering::SeqCst))
            });
            // Loaded before the check that nothing was written, so the
            // listing is the one the stamps were read against: a listing
            // published into this slot after that fails the caller's
            // compare-and-swap instead of being evicted the moment it lands.
            let current = index.map(|index| self.listings[index].load_full());
            if self.writers.load(Ordering::SeqCst) == writers
                && let (Some(index), Some(current)) = (index, current)
            {
                let holds = current.as_ref().map(|cached| cached.id);
                let as_seen = if same.is_some() {
                    holds == Some(id)
                } else if empty.is_some() {
                    holds.is_none()
                } else {
                    true
                };
                if as_seen {
                    return (index, current, crowded);
                }
            }
        }
        // A write overlapped the look at the tags: ask the listings.
        let mut same: Option<(usize, Listing)> = None;
        let mut crowded = false;
        let mut empty = None;
        let mut oldest: Option<(u64, usize, Listing)> = None;
        for (index, listing) in self.listings.iter().enumerate() {
            let current = listing.load_full();
            match &current {
                Some(cached) if cached.id == id => {
                    if same.is_some() {
                        crowded = true;
                    } else {
                        same = Some((index, current));
                    }
                }
                Some(_) => {
                    let used = self.used[index].load(Ordering::SeqCst);
                    if oldest.as_ref().is_none_or(|(least, ..)| used < *least) {
                        oldest = Some((used, index, current));
                    }
                }
                None => {
                    if empty.is_none() {
                        empty = Some(index);
                    }
                }
            }
        }
        match (same, empty, oldest) {
            (Some((index, current)), ..) => (index, current, crowded),
            (None, Some(index), _) => (index, None, false),
            (None, None, Some((_, index, current))) => (index, current, false),
            (None, None, None) => unreachable!("the snapshot table has at least one slot"),
        }
    }

    /// Callers that stored one directory at the same moment can each have
    /// picked a different slot. Keep the newest listing and empty every
    /// other slot that holds the directory, so it never has two listings to
    /// choose between. The newest listing is never the one removed: a
    /// listing is removed here only by a caller that has seen a newer one.
    fn drop_older_duplicate(&self, index: usize, cached: &Arc<CachedSnapshot>) {
        // The newest listing seen so far and its slot. It starts as the
        // caller's own, and every other slot is compared against it.
        let mut newest = (index, Arc::clone(cached));
        for (other, listing) in self.listings.iter().enumerate() {
            if other == index {
                continue;
            }
            loop {
                let found = listing.load_full();
                let Some(duplicate) = &found else {
                    break;
                };
                if duplicate.id != cached.id {
                    break;
                }
                if duplicate.stored > newest.1.stored {
                    // This one is newer: remove the one held until now and
                    // carry on, so that an older listing in a later slot is
                    // not left for a caller that may never come.
                    self.publish(newest.0, &Some(Arc::clone(&newest.1)), None);
                    newest = (other, Arc::clone(duplicate));
                    break;
                }
                if self.publish(other, &found, None) {
                    break;
                }
            }
        }
    }
}

impl std::fmt::Debug for DirectorySnapshots {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cached = self
            .state
            .listings
            .iter()
            .filter_map(|listing| listing.load_full().map(|cached| cached.id))
            .collect::<Vec<_>>();
        f.debug_struct("DirectorySnapshots")
            .field("cached", &cached)
            .field("max", &self.state.listings.len())
            .finish()
    }
}

impl DirectorySnapshots {
    pub fn new(max: usize) -> Self {
        let max = max.max(1);
        Self {
            state: Arc::new(SnapshotSlots {
                listings: (0..max).map(|_| ArcSwapOption::const_empty()).collect(),
                tags: (0..max).map(|_| AtomicU64::new(0)).collect(),
                used: (0..max).map(|_| AtomicU64::new(0)).collect(),
                clock: AtomicU64::new(0),
                writers: AtomicU64::new(0),
            }),
        }
    }

    pub fn set(&self, id: u64, names: Vec<String>) -> DirectorySnapshot {
        let verifier = cookie_verifier(&names);
        let snapshot = DirectorySnapshot { names, verifier };
        // The one id that has no tag is never a handle id; do not cache it.
        let Some(tag) = tag_of(id) else {
            return snapshot;
        };
        let slots = &*self.state;
        let stored = slots.tick();
        let cached = Arc::new(CachedSnapshot {
            id,
            stored,
            snapshot,
        });
        let published = loop {
            let (index, current, crowded) = slots.choose(id, tag);
            // A caller that started later has already stored this directory.
            // Its listing is the newer one, so this one is not cached.
            if current
                .as_ref()
                .is_some_and(|held| held.id == id && held.stored > stored)
            {
                break None;
            }
            // Stamped before it is published, so that no other caller sees
            // the new listing in a slot that still looks unused.
            slots.used[index].fetch_max(stored, Ordering::SeqCst);
            if slots.publish(index, &current, Some(Arc::clone(&cached))) {
                break Some((index, crowded));
            }
        };
        if let Some((index, crowded)) = published {
            // The clock has moved only if another caller was at work between
            // the tick above and this one. A caller that stored this
            // directory in that time may have picked a different slot;
            // whichever of the two ticks last here is certain to see both
            // listings. A duplicate left by callers before that was seen
            // when the slot was chosen.
            let disturbed = slots.clock.fetch_add(1, Ordering::SeqCst) != stored;
            if crowded || disturbed {
                slots.drop_older_duplicate(index, &cached);
            }
        }
        // The listing is copied for the caller; the table keeps the original.
        cached.snapshot.clone()
    }

    pub fn get(&self, id: u64) -> Option<DirectorySnapshot> {
        let slots = &*self.state;
        let (index, cached) = slots.find(id, tag_of(id)?)?;
        slots.used[index].fetch_max(slots.tick(), Ordering::SeqCst);
        Some(cached.snapshot.clone())
    }

    pub fn invalidate(&self, id: u64) {
        let Some(tag) = tag_of(id) else {
            return;
        };
        let slots = &*self.state;
        while let Some((index, cached)) = slots.find(id, tag) {
            slots.publish(index, &Some(cached), None);
        }
    }

    pub fn clear(&self) {
        let slots = &*self.state;
        slots.writers.fetch_add(WRITE_STARTED, Ordering::SeqCst);
        for listing in slots.listings.iter() {
            listing.store(None);
        }
        for index in 0..slots.listings.len() {
            slots.retag(index);
        }
        slots.writers.fetch_add(WRITE_FINISHED, Ordering::SeqCst);
    }
}

pub fn cookie_verifier(names: &[String]) -> Vec<u8> {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for (index, name) in names.iter().enumerate() {
        if index != 0 {
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
        for byte in name.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x1000_0000_01b3);
        }
    }
    hash.to_be_bytes().to_vec()
}

/// Shared directory pages must invalidate cookies when an inode changes under
/// an unchanged name. The backend supplies these attributes from one guarded
/// namespace snapshot, so no independent path lookups enter this hash.
pub fn guarded_cookie_verifier(directory: &Stats, entries: &[GuardedDirectoryEntry]) -> Vec<u8> {
    const PRIME: u64 = 0x1000_0000_01b3;
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut feed = |bytes: &[u8]| {
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(PRIME);
        }
    };
    feed(&directory.dev.to_be_bytes());
    feed(&directory.ino.to_be_bytes());
    feed(&directory.ctime_ms.to_be_bytes());
    for entry in entries {
        feed(&(entry.name.len() as u64).to_be_bytes());
        feed(entry.name.as_bytes());
        feed(&entry.stats.dev.to_be_bytes());
        feed(&entry.stats.ino.to_be_bytes());
    }
    hash.to_be_bytes().to_vec()
}

pub fn same_verifier(a: &[u8], b: &[u8]) -> bool {
    a.len() == NFS3_COOKIEVERFSIZE && b.len() == NFS3_COOKIEVERFSIZE && a == b
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(ino: u64) -> Stats {
        Stats {
            dev: 1,
            ino,
            mode: mount_rs_core::types::S_IFREG | 0o644,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            size: 0,
            blksize: 4096,
            blocks: 0,
            atime_ms: 0,
            mtime_ms: 0,
            ctime_ms: 0,
            birthtime_ms: 0,
        }
    }

    #[test]
    fn same_backend_inode_requires_nonzero_matching_device_and_inode() {
        let source = stats(7);
        assert!(same_backend_inode(&source, &source));
        assert!(!same_backend_inode(&source, &stats(8)));
        let mut other_device = source.clone();
        other_device.dev = 2;
        assert!(!same_backend_inode(&source, &other_device));
        assert!(!same_backend_inode(&stats(0), &stats(0)));
    }

    #[test]
    fn handles_reject_foreign_and_removed_names() {
        let table = FileHandleTable::new(FileHandleTableOptions::default());
        let entry = table.bind("/file", &stats(2));
        let handle = table.encode(&entry);
        assert_eq!(table.decode(&handle).unwrap().fileid, 2);
        let mut foreign = handle.clone();
        foreign[4] ^= 1;
        assert_eq!(table.decode(&foreign).unwrap_err().code, ErrorCode::Estale);
        table.forget("/file");
        assert_eq!(table.decode(&handle).unwrap_err().code, ErrorCode::Estale);
    }

    #[test]
    fn orphan_keeps_identity_but_not_a_resolvable_path() {
        let table = FileHandleTable::default();
        let entry = table.bind("/doomed", &stats(12));
        let handle = table.encode(&entry);

        let orphan = table.orphan("/doomed").expect("orphaned entry");
        assert_eq!(orphan.id, entry.id);
        assert_eq!(table.decode(&handle).unwrap().fileid, 12);
        assert_eq!(table.resolve(&handle).unwrap_err().code, ErrorCode::Estale);

        let replacement = table.bind("/doomed", &stats(13));
        assert_ne!(replacement.id, entry.id);
        assert_eq!(table.decode(&handle).unwrap().fileid, 12);
    }

    #[test]
    fn hardlinks_share_identity_and_rename_preserves_handle() {
        let table = FileHandleTable::default();
        let first = table.bind("/a", &stats(9));
        let second = table.bind("/b", &stats(9));
        assert_eq!(first.id, second.id);
        let handle = table.encode(&first);
        table.remap("/a", "/c");
        assert_eq!(table.at("/c").unwrap().id, first.id);
        assert!(matches!(
            table.resolve(&handle).unwrap().as_str(),
            "/b" | "/c"
        ));
        assert_eq!(table.resolve(&table.encode(&second)).unwrap(), "/b");
    }

    #[test]
    fn entries_are_sorted_and_snapshot_live_handles() {
        let table = FileHandleTable::default();
        let second = table.bind("/second", &stats(2));
        let first = table.bind("/first", &stats(1));

        let entries = table.entries();
        assert_eq!(
            entries.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![ROOT_HANDLE_ID, second.id, first.id]
        );
        assert_eq!(entries[0], table.root());
        assert_eq!(entries[1].path, "/second");
        assert_eq!(entries[2].path, "/first");
    }

    #[test]
    fn max_handles_evicts_oldest_entry_but_keeps_root_and_current_bind() {
        let table = FileHandleTable::new(FileHandleTableOptions {
            max_handles: Some(2),
            ..FileHandleTableOptions::default()
        });
        let old = table.bind("/old", &stats(2));
        let current = table.bind("/current", &stats(3));

        assert_eq!(table.size(), 2);
        assert!(table.at("/").is_some());
        assert!(table.at("/old").is_none());
        assert_eq!(table.at("/current").unwrap().id, current.id);
        assert_eq!(
            table.decode(&table.encode(&old)).unwrap_err().code,
            ErrorCode::Estale
        );
    }

    #[test]
    fn max_handles_uses_access_recency_and_respects_pins() {
        let table = FileHandleTable::new(FileHandleTableOptions {
            max_handles: Some(3),
            ..FileHandleTableOptions::default()
        });
        let first = table.bind("/first", &stats(2));
        let _second = table.bind("/second", &stats(3));
        assert!(table.at("/first").is_some());
        let _third = table.bind("/third", &stats(4));

        assert!(table.at("/first").is_some());
        assert!(table.at("/third").is_some());
        assert!(table.at("/second").is_none());

        table.pin(first.id);
        let fourth = table.bind("/fourth", &stats(5));
        assert!(table.at("/first").is_some());
        assert_eq!(table.at("/fourth").unwrap().id, fourth.id);
        assert!(table.at("/third").is_none());

        table.unpin(first.id);
        let fifth = table.bind("/fifth", &stats(6));
        assert_eq!(table.at("/fifth").unwrap().id, fifth.id);
        assert!(table.at("/first").is_none());
    }

    #[test]
    fn using_a_handle_does_not_scan_the_whole_table() {
        // Every NFS operation decodes a handle. A crawl leaves one entry per
        // file, so a cost per use that grows with the table makes the whole
        // mount slow down as it is used. Linear cost here is tens of seconds.
        const HANDLES: u64 = 100_000;
        let table = FileHandleTable::default();
        let handles: Vec<Vec<u8>> = (0..HANDLES)
            .map(|index| table.encode(&table.bind(&format!("/f{index}"), &stats(100 + index))))
            .collect();
        let start = std::time::Instant::now();
        for handle in &handles {
            table.decode(handle).unwrap();
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "decoding {HANDLES} handles took {elapsed:?}"
        );
    }

    #[test]
    fn rename_preserves_a_single_path_handle() {
        let table = FileHandleTable::default();
        let entry = table.bind("/a", &stats(11));
        let handle = table.encode(&entry);

        table.remap("/a", "/b");

        assert_eq!(table.resolve(&handle).unwrap(), "/b");
        assert_eq!(table.at("/b").unwrap().id, entry.id);
    }

    #[test]
    fn rename_replacing_a_pinned_destination_keeps_its_handle_pathless() {
        let table = FileHandleTable::default();
        let source = table.bind("/source", &stats(11));
        let destination = table.bind("/destination", &stats(12));
        let held_handle = table.encode(&destination);
        table.pin(destination.id);

        table.remap("/source", "/destination");

        assert_eq!(table.at("/destination").unwrap().id, source.id);
        assert_eq!(table.decode(&held_handle).unwrap().id, destination.id);
        assert_eq!(
            table.resolve(&held_handle).unwrap_err().code,
            ErrorCode::Estale
        );
        let replacement = table.bind("/other-name", &stats(12));
        assert_ne!(replacement.id, destination.id);
        table.unpin(destination.id);
        assert_eq!(
            table.decode(&held_handle).unwrap_err().code,
            ErrorCode::Estale
        );
    }

    #[test]
    fn bind_replacing_a_pinned_name_keeps_the_old_handle_pathless() {
        let table = FileHandleTable::default();
        let old = table.bind("/file", &stats(12));
        let held_handle = table.encode(&old);
        table.pin(old.id);

        let replacement = table.bind("/file", &stats(13));

        assert_ne!(replacement.id, old.id);
        assert_eq!(table.at("/file").unwrap().id, replacement.id);
        assert_eq!(table.decode(&held_handle).unwrap().id, old.id);
        assert_eq!(
            table.resolve(&held_handle).unwrap_err().code,
            ErrorCode::Estale
        );
        // An orphaned file must not adopt a later name through its old key.
        assert_ne!(table.bind("/other", &stats(12)).id, old.id);

        table.unpin(old.id);
        assert_eq!(
            table.decode(&held_handle).unwrap_err().code,
            ErrorCode::Estale
        );
    }

    #[test]
    fn bind_pinned_closes_the_gap_before_a_descriptor_can_be_retained() {
        let table = FileHandleTable::default();
        let old = table.bind_pinned("/file", &stats(12));
        let held_handle = table.encode(&old);

        let replacement = table.bind("/file", &stats(13));

        assert_ne!(replacement.id, old.id);
        assert_eq!(table.decode(&held_handle).unwrap().id, old.id);
        table.unpin(old.id);
        assert_eq!(
            table.decode(&held_handle).unwrap_err().code,
            ErrorCode::Estale
        );
    }

    #[test]
    fn stable_inode_keys_reconnect_a_remote_move_after_name_replacement() {
        let table = FileHandleTable::default();
        table.trust_stable_inode_ids();
        let old = table.bind_pinned("/file", &stats(12));
        let held_handle = table.encode(&old);

        let replacement = table.bind("/file", &stats(13));
        let moved = table.bind("/moved", &stats(12));

        assert_ne!(replacement.id, old.id);
        assert_eq!(moved.id, old.id);
        assert_eq!(table.resolve(&held_handle).unwrap(), "/moved");
        table.unpin(old.id);
        assert_eq!(table.decode(&held_handle).unwrap().id, old.id);
    }

    #[test]
    fn stable_inode_keys_reconnect_unpinned_directory_handles_too() {
        let table = FileHandleTable::default();
        table.trust_stable_inode_ids();
        let old = table.bind("/dir", &stats(12));

        table.bind("/dir", &stats(13));
        let moved = table.bind("/new-dir", &stats(12));

        assert_eq!(moved.id, old.id);
        assert_eq!(table.resolve(&table.encode(&old)).unwrap(), "/new-dir");
    }

    /// `bind_pinned` returns a handle that is already pinned. If binding and
    /// pinning were two steps, a bind on another thread could push the table
    /// over its limit between them and evict the new handle, and the decode
    /// below would fail.
    #[test]
    fn pinned_handles_survive_concurrent_binds_at_a_small_limit() {
        const MAX: usize = 4;
        const THREADS: u64 = 4;
        const ROUNDS: u64 = 3_000;
        let table = FileHandleTable::new(FileHandleTableOptions {
            max_handles: Some(MAX),
            ..FileHandleTableOptions::default()
        });
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let table = &table;
                scope.spawn(move || {
                    let path = format!("/pinned-{thread}");
                    for round in 0..ROUNDS {
                        // Every other round the name refers to a new file.
                        let ino = (thread + 1) * 1_000_000 + round / 2;
                        let entry = table.bind_pinned(&path, &stats(ino));
                        let handle = table.encode(&entry);
                        for _ in 0..4 {
                            let decoded =
                                table.decode(&handle).expect("a pinned handle was evicted");
                            assert_eq!(decoded.id, entry.id);
                            assert_eq!(decoded.fileid, ino);
                            std::thread::yield_now();
                        }
                        table.unpin(entry.id);
                    }
                });
                scope.spawn(move || {
                    for round in 0..ROUNDS {
                        let ino = (thread + 1) * 1_000_000_000 + round;
                        table.bind(&format!("/churn-{thread}-{round}"), &stats(ino));
                        if round % 8 == 0 {
                            // Give a pinned name to a different file.
                            table.bind(
                                &format!("/pinned-{}", round % THREADS),
                                &stats(ino + 500_000_000),
                            );
                        }
                    }
                });
            }
        });
        // Nothing is pinned now, so the next bind brings the table back
        // within its limit.
        table.bind("/last", &stats(7));
        assert!(table.size() <= MAX, "{} handles", table.size());
        assert!(table.at("/").is_some());
        assert!(table.at("/last").is_some());
    }

    #[test]
    fn cookie_changes_with_directory_contents() {
        let one = cookie_verifier(&["a".to_owned(), "b".to_owned()]);
        let two = cookie_verifier(&["a".to_owned(), "c".to_owned()]);
        assert_ne!(one, two);
        assert!(same_verifier(&one, &one));
    }

    #[test]
    fn snapshots_preserve_driver_order_and_refresh_lru_on_read() {
        let snapshots = DirectorySnapshots::new(2);
        let first = snapshots.set(1, vec!["z".to_owned(), "a".to_owned()]);
        assert_eq!(first.names, vec!["z".to_owned(), "a".to_owned()]);
        snapshots.set(2, vec!["b".to_owned()]);
        assert!(snapshots.get(1).is_some());
        snapshots.set(3, vec!["c".to_owned()]);
        assert!(snapshots.get(1).is_some());
        assert!(snapshots.get(2).is_none());
    }

    #[test]
    fn snapshots_evict_in_least_recently_used_order() {
        let snapshots = DirectorySnapshots::new(3);
        for id in 1..=3 {
            snapshots.set(id, vec![id.to_string()]);
        }
        // Reading 1 and storing 2 again leave 3 the least recently used.
        assert!(snapshots.get(1).is_some());
        snapshots.set(2, vec!["again".to_owned()]);
        snapshots.set(4, vec!["4".to_owned()]);
        assert!(snapshots.get(3).is_none());
        assert_eq!(snapshots.get(2).unwrap().names, vec!["again".to_owned()]);
        // Now 1 is the oldest, and an invalidated slot is reused first.
        snapshots.invalidate(4);
        assert!(snapshots.get(4).is_none());
        snapshots.set(5, vec!["5".to_owned()]);
        assert!(snapshots.get(1).is_some());
        snapshots.set(6, vec!["6".to_owned()]);
        assert!(snapshots.get(2).is_none());
        for id in [1, 5, 6] {
            assert!(snapshots.get(id).is_some(), "{id}");
        }
        snapshots.clear();
        for id in 1..=6 {
            assert!(snapshots.get(id).is_none(), "{id}");
        }
        assert!(snapshots.get(u64::MAX).is_none());
        snapshots.set(u64::MAX, vec!["uncached".to_owned()]);
        assert!(snapshots.get(u64::MAX).is_none());
    }

    /// Put a listing of `id` straight into a slot, as a caller that picked
    /// that slot before another caller stored the same directory would.
    fn place(snapshots: &DirectorySnapshots, index: usize, id: u64, stored: u64, name: &str) {
        let slots = &*snapshots.state;
        let current = slots.listings[index].load_full();
        let names = vec![name.to_owned()];
        let cached = Arc::new(CachedSnapshot {
            id,
            stored,
            snapshot: DirectorySnapshot {
                verifier: cookie_verifier(&names),
                names,
            },
        });
        assert!(slots.publish(index, &current, Some(cached)));
    }

    #[test]
    fn a_directory_in_two_slots_reads_as_its_newest_listing() {
        // The older listing sits in the lower slot, where a search that
        // stopped at the first match would find it.
        let snapshots = DirectorySnapshots::new(3);
        place(&snapshots, 0, 7, 1, "older");
        place(&snapshots, 2, 7, 2, "newer");
        assert_eq!(snapshots.get(7).expect("cached").names, ["newer"]);
        snapshots.invalidate(7);
        assert!(snapshots.get(7).is_none());
    }

    #[test]
    fn tidying_duplicates_goes_on_past_a_newer_listing() {
        // The caller's own listing is the middle one. It meets the newest
        // first and must still remove the oldest behind it; otherwise
        // removing the newest would bring the oldest back.
        let snapshots = DirectorySnapshots::new(3);
        place(&snapshots, 0, 7, 3, "newest");
        place(&snapshots, 1, 7, 2, "own");
        place(&snapshots, 2, 7, 1, "oldest");
        let slots = &*snapshots.state;
        let own = slots.listings[1].load_full().expect("placed");
        slots.drop_older_duplicate(1, &own);
        let left = slots
            .listings
            .iter()
            .map(|listing| listing.load_full().map(|cached| cached.stored))
            .collect::<Vec<_>>();
        assert_eq!(left, [Some(3), None, None]);
    }

    /// What the lock gave the snapshot cache, checked while eight threads
    /// use a cache too small for them: a listing is only ever returned for
    /// the directory it was stored under; a caller that stores a directory
    /// nobody else stores reads back that listing or nothing, never an older
    /// one; a directory that was invalidated is gone; and once the threads
    /// stop, no directory is cached twice.
    #[test]
    fn snapshots_stay_exact_under_concurrent_use() {
        const SLOTS: usize = 4;
        const THREADS: u64 = 8;
        const ROUNDS: u64 = 4_000;
        const SHARED: u64 = 6;
        let snapshots = DirectorySnapshots::new(SLOTS);
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let snapshots = &snapshots;
                scope.spawn(move || {
                    let own = 1_000 + thread;
                    for round in 0..ROUNDS {
                        let shared = (round * 7 + thread) % SHARED;
                        let stored = snapshots.set(
                            shared,
                            vec![shared.to_string(), format!("{thread}-{round}")],
                        );
                        assert_eq!(stored.names[0], shared.to_string());
                        let other = (shared + 1) % SHARED;
                        if let Some(found) = snapshots.get(other) {
                            assert_eq!(found.names[0], other.to_string());
                            assert_eq!(found.verifier, cookie_verifier(&found.names));
                        }
                        let latest = vec![own.to_string(), round.to_string()];
                        snapshots.set(own, latest.clone());
                        if let Some(found) = snapshots.get(own) {
                            assert_eq!(found.names, latest);
                        }
                        if round % 3 == 0 {
                            snapshots.invalidate(own);
                            assert!(snapshots.get(own).is_none());
                        }
                        if round % 5 == 0 {
                            snapshots.invalidate(shared);
                        }
                    }
                });
            }
        });
        let slots = &*snapshots.state;
        let mut cached = Vec::new();
        for (index, listing) in slots.listings.iter().enumerate() {
            let id = listing.load_full().map(|cached| cached.id);
            assert_eq!(
                slots.tags[index].load(Ordering::SeqCst),
                id.map_or(0, |id| id + 1),
                "slot {index}"
            );
            cached.extend(id);
        }
        let distinct = cached.iter().collect::<std::collections::BTreeSet<_>>();
        assert_eq!(distinct.len(), cached.len(), "{cached:?}");
        assert_eq!(slots.writers.load(Ordering::SeqCst) & WRITES_IN_PROGRESS, 0);
        // With the threads gone the cache behaves as it does for one caller.
        snapshots.set(1, vec!["fresh".to_owned()]);
        assert_eq!(snapshots.get(1).unwrap().names, vec!["fresh".to_owned()]);
        snapshots.invalidate(1);
        assert!(snapshots.get(1).is_none());
    }

    /// Callers that store one directory at the same moment, while slots
    /// ahead of them are being emptied, can each pick a different slot. The
    /// directory must still end up cached once, or a later read could be
    /// given the older of two listings.
    #[test]
    fn snapshots_stored_at_the_same_moment_are_cached_once() {
        const SETTERS: u64 = 4;
        const FILLERS: u64 = 4;
        let snapshots = DirectorySnapshots::new(16);
        for round in 0..2_000_u64 {
            snapshots.clear();
            for filler in 0..FILLERS {
                snapshots.set(filler, vec!["filler".to_owned()]);
            }
            let directory = 1_000 + round;
            let start = std::sync::Barrier::new((SETTERS + FILLERS) as usize);
            std::thread::scope(|scope| {
                for setter in 0..SETTERS {
                    let (snapshots, start) = (&snapshots, &start);
                    scope.spawn(move || {
                        start.wait();
                        snapshots.set(directory, vec![setter.to_string()]);
                    });
                }
                for filler in 0..FILLERS {
                    let (snapshots, start) = (&snapshots, &start);
                    scope.spawn(move || {
                        start.wait();
                        snapshots.invalidate(filler);
                    });
                }
            });
            let slots = &*snapshots.state;
            let holders = slots
                .listings
                .iter()
                .filter(|listing| {
                    listing
                        .load_full()
                        .is_some_and(|cached| cached.id == directory)
                })
                .count();
            assert_eq!(holders, 1, "round {round}");
            for (index, listing) in slots.listings.iter().enumerate() {
                assert_eq!(
                    slots.tags[index].load(Ordering::SeqCst),
                    listing.load_full().map_or(0, |cached| cached.id + 1),
                    "round {round} slot {index}"
                );
            }
            assert!(snapshots.get(directory).is_some());
        }
    }

    /// Benchmark for the directory-snapshot cache: READDIR continuation on a
    /// cached listing (`get`), a re-list of a cached directory (`set_same`)
    /// and a listing of a new directory into a full cache (`set_new`).
    /// Latencies are per-operation averages over batches of 100, because one
    /// operation is shorter than the clock tick on some hosts.
    ///
    ///
    /// These numbers are why the cell stays. A lock-free replacement (an
    /// atomically published persistent trie holding the map and the order)
    /// was built and measured against the cell with the listing copy taken
    /// out of both: for one caller a `set_new` took 1.7 to 2.4 microseconds
    /// against 1.2 to 1.3, although eight callers at once ran 2 to 7 times
    /// faster. A READDIR that starts a listing always does one `set`.
    ///
    /// `cargo test --release -p mount-rs-nfs --lib -- --ignored --nocapture bench_directory_snapshots`
    #[test]
    #[ignore = "benchmark"]
    fn bench_directory_snapshots() {
        use std::sync::Barrier;
        use std::time::Instant;

        const BATCH: usize = 100;
        fn listing(seed: u64) -> Vec<String> {
            (0..32).map(|i| format!("entry-{seed:04}-{i:02}")).collect()
        }
        fn run(op: &'static str, cached: u64, threads: usize, batches: usize) {
            let snapshots = DirectorySnapshots::new(cached as usize);
            for id in 0..cached {
                snapshots.set(id, listing(id));
            }
            let barrier = Arc::new(Barrier::new(threads + 1));
            let workers = (0..threads)
                .map(|thread| {
                    let snapshots = snapshots.clone();
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        let mut samples = Vec::with_capacity(batches);
                        let mut random = 0x9e37_79b9_7f4a_7c15_u64 ^ (thread as u64) << 32;
                        let mut fresh = cached + (thread as u64 + 1) * 1_000_000_000;
                        barrier.wait();
                        for _ in 0..batches {
                            let mut inputs = Vec::with_capacity(BATCH);
                            for _ in 0..BATCH {
                                random = random
                                    .wrapping_mul(6_364_136_223_846_793_005)
                                    .wrapping_add(1_442_695_040_888_963_407);
                                let existing = (random >> 33) % cached;
                                match op {
                                    "get" => inputs.push((existing, Vec::new())),
                                    "set_same" => inputs.push((existing, listing(existing))),
                                    _ => {
                                        fresh += 1;
                                        inputs.push((fresh, listing(fresh)));
                                    }
                                }
                            }
                            let started = Instant::now();
                            for (id, names) in inputs {
                                if op == "get" {
                                    std::hint::black_box(snapshots.get(id));
                                } else {
                                    std::hint::black_box(snapshots.set(id, names));
                                }
                            }
                            samples.push(started.elapsed().as_nanos() as u64 / BATCH as u64);
                        }
                        samples
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            let started = Instant::now();
            let mut samples = Vec::new();
            for worker in workers {
                samples.extend(worker.join().unwrap());
            }
            let wall = started.elapsed();
            samples.sort_unstable();
            // Wall time includes preparing the inputs, so the rate is taken
            // from the time spent inside the operations.
            let busy: u64 = samples.iter().sum::<u64>() * BATCH as u64;
            let ops = (samples.len() * BATCH) as f64;
            println!(
                "snapbench op={op} cached={cached} threads={threads} ops/s={:.0} mean={:.0}ns p50={}ns p99={}ns wall={:?}",
                ops * threads as f64 / (busy as f64 / 1e9),
                busy as f64 / ops,
                samples[samples.len() / 2],
                samples[samples.len() * 99 / 100],
                wall,
            );
        }
        for round in 0..3 {
            println!("snapbench round={round}");
            for op in ["get", "set_same", "set_new"] {
                for cached in [1_u64, 64, 1_000] {
                    for threads in [1_usize, 8] {
                        run(op, cached, threads, if op == "get" { 2_000 } else { 1_000 });
                    }
                }
            }
        }
    }

    /// Benchmark for the handle table. `hot` is the NFSv3 path for a file the
    /// table already knows: bind and pin its name, decode the handle the
    /// client sends back, release the pin. `evict` binds a name the table
    /// has not seen into a table that is at its `max_handles` limit, so each
    /// bind also retires the least recently used entry. One operation is the
    /// whole bind, decode and unpin; latencies are averages over batches of
    /// 100.
    ///
    /// `cargo test --release -p mount-rs-nfs --lib -- --ignored --nocapture bench_handle_table`
    #[test]
    #[ignore = "benchmark"]
    fn bench_handle_table() {
        use std::sync::Barrier;
        use std::time::Instant;

        const BATCH: usize = 100;
        const ENTRIES: u64 = 4_096;
        fn run(op: &'static str, threads: usize, batches: usize) {
            let table = FileHandleTable::new(FileHandleTableOptions {
                max_handles: (op == "evict").then_some(ENTRIES as usize + 1),
                ..FileHandleTableOptions::default()
            });
            for index in 0..ENTRIES {
                table.bind(&format!("/dir/file-{index:05}"), &stats(100 + index));
            }
            let barrier = Arc::new(Barrier::new(threads + 1));
            let workers = (0..threads)
                .map(|thread| {
                    let table = table.clone();
                    let barrier = Arc::clone(&barrier);
                    std::thread::spawn(move || {
                        let mut samples = Vec::with_capacity(batches);
                        let mut random = 0x9e37_79b9_7f4a_7c15_u64 ^ (thread as u64) << 32;
                        let mut fresh = (thread as u64 + 1) * 1_000_000_000;
                        barrier.wait();
                        for _ in 0..batches {
                            let mut inputs = Vec::with_capacity(BATCH);
                            for _ in 0..BATCH {
                                random = random
                                    .wrapping_mul(6_364_136_223_846_793_005)
                                    .wrapping_add(1_442_695_040_888_963_407);
                                let index = if op == "evict" {
                                    fresh += 1;
                                    fresh
                                } else {
                                    (random >> 33) % ENTRIES
                                };
                                inputs.push((format!("/dir/file-{index:05}"), stats(100 + index)));
                            }
                            let started = Instant::now();
                            for (path, stats) in &inputs {
                                let entry = table.bind_pinned(path, stats);
                                let handle = table.encode(&entry);
                                std::hint::black_box(table.decode(&handle).unwrap());
                                table.unpin(entry.id);
                            }
                            samples.push(started.elapsed().as_nanos() as u64 / BATCH as u64);
                        }
                        samples
                    })
                })
                .collect::<Vec<_>>();
            barrier.wait();
            let started = Instant::now();
            let mut samples = Vec::new();
            for worker in workers {
                samples.extend(worker.join().unwrap());
            }
            let wall = started.elapsed();
            samples.sort_unstable();
            let busy: u64 = samples.iter().sum::<u64>() * BATCH as u64;
            let ops = (samples.len() * BATCH) as f64;
            println!(
                "handlebench op={op} entries={ENTRIES} threads={threads} ops/s={:.0} mean={:.0}ns p50={}ns p99={}ns wall={:?}",
                ops * threads as f64 / (busy as f64 / 1e9),
                busy as f64 / ops,
                samples[samples.len() / 2],
                samples[samples.len() * 99 / 100],
                wall,
            );
        }
        for round in 0..3 {
            println!("handlebench round={round}");
            for op in ["hot", "evict"] {
                for threads in [1_usize, 8] {
                    run(op, threads, 1_000);
                }
            }
        }
    }
}
