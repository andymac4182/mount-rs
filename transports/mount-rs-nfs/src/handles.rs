//! NFSv3 file handles and directory-cookie snapshots.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_core::{ErrorCode, FsError, Stats};

use crate::constants::{NFS3_COOKIEVERFSIZE, NFS3_FHSIZE};

const FH_MAGIC: u32 = 0x554e_4653; // "UNFS"
pub const FH_SIZE: usize = 20;
pub const ROOT_HANDLE_ID: u64 = 1;

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
}

#[derive(Debug)]
struct HandleState {
    verifier: [u8; 8],
    next_id: u64,
    by_id: HashMap<u64, Entry>,
    by_path: HashMap<String, u64>,
    by_key: HashMap<String, u64>,
}

#[derive(Debug, Clone)]
pub struct FileHandleTable {
    state: Arc<Mutex<HandleState>>,
    use_driver_ino: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct FileHandleTableOptions {
    pub use_driver_ino: bool,
    pub verifier: Option<[u8; 8]>,
}

impl Default for FileHandleTableOptions {
    fn default() -> Self {
        Self {
            use_driver_ino: true,
            verifier: None,
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
        };
        let mut by_id = HashMap::new();
        by_id.insert(ROOT_HANDLE_ID, root);
        Self {
            state: Arc::new(Mutex::new(HandleState {
                verifier,
                next_id: ROOT_HANDLE_ID + 1,
                by_id,
                by_path: HashMap::from([(String::from("/"), ROOT_HANDLE_ID)]),
                by_key: HashMap::new(),
            })),
            use_driver_ino: options.use_driver_ino,
        }
    }

    pub fn verifier(&self) -> [u8; 8] {
        self.state.lock().expect("handle table lock").verifier
    }

    pub fn size(&self) -> usize {
        self.state.lock().expect("handle table lock").by_id.len()
    }

    pub fn root(&self) -> HandleEntry {
        self.entry(ROOT_HANDLE_ID).expect("root handle")
    }

    pub fn encode(&self, entry: &HandleEntry) -> Vec<u8> {
        let verifier = self.verifier();
        let mut handle = vec![0; FH_SIZE];
        handle[0..4].copy_from_slice(&FH_MAGIC.to_be_bytes());
        handle[4..12].copy_from_slice(&verifier);
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
        let verifier = self.verifier();
        if handle[4..12] != verifier {
            return Err(stale("file handle is from a previous server instance"));
        }
        let id = u64::from_be_bytes(handle[12..20].try_into().unwrap());
        self.entry(id).ok_or_else(|| stale("unknown file handle"))
    }

    pub fn resolve(&self, handle: &[u8]) -> mount_rs_core::Result<String> {
        let entry = self.decode(handle)?;
        self.path_of(&entry)
    }

    pub fn path_of(&self, entry: &HandleEntry) -> mount_rs_core::Result<String> {
        let state = self.state.lock().expect("handle table lock");
        let current = state
            .by_id
            .get(&entry.id)
            .ok_or_else(|| stale("unknown file handle"))?;
        current
            .paths
            .iter()
            .find(|path| state.by_path.get(*path) == Some(&entry.id))
            .cloned()
            .ok_or_else(|| stale("file handle names a removed file"))
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
        let state = self.state.lock().expect("handle table lock");
        let id = *state.by_path.get(path)?;
        state.by_id.get(&id).map(|entry| HandleEntry {
            id: entry.id,
            fileid: entry.fileid,
            key: entry.key.clone(),
            path: path.to_owned(),
        })
    }

    pub fn bind(&self, path: &str, stats: &Stats) -> HandleEntry {
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
                    },
                );
                id
            });
        if let Some(previous) = previous.filter(|old| *old != id) {
            detach_locked(&mut state, previous, path);
        }
        let bound_key = if let Some(entry) = state.by_id.get_mut(&id) {
            if entry.key.is_none() {
                entry.key = key.clone();
            }
            entry.fileid = if stats.ino > 0 { stats.ino } else { id };
            entry.paths.insert(path.to_owned());
            entry.key.clone()
        } else {
            None
        };
        state.by_path.insert(path.to_owned(), id);
        if let Some(key) = bound_key {
            state.by_key.insert(key, id);
        }
        entry_locked(&state, id, path)
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
            detach_locked(&mut state, id, &path);
        }
        for (path, id) in affected {
            let suffix = path.strip_prefix(old_path).unwrap_or_default();
            let replacement = format!("{new_path}{suffix}");
            detach_preserve_locked(&mut state, id, &path);
            if let Some(entry) = state.by_id.get_mut(&id) {
                entry.paths.insert(replacement.clone());
                state.by_path.insert(replacement, id);
            }
        }
    }

    pub fn clear(&self) {
        let verifier = self.verifier();
        let root = Entry {
            id: ROOT_HANDLE_ID,
            fileid: ROOT_HANDLE_ID,
            key: None,
            paths: BTreeSet::from([String::from("/")]),
        };
        let mut state = self.state.lock().expect("handle table lock");
        state.next_id = ROOT_HANDLE_ID + 1;
        state.by_id.clear();
        state.by_id.insert(ROOT_HANDLE_ID, root);
        state.by_path.clear();
        state.by_path.insert("/".to_owned(), ROOT_HANDLE_ID);
        state.by_key.clear();
        state.verifier = verifier;
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

fn detach_locked(state: &mut HandleState, id: u64, path: &str) {
    detach_preserve_locked(state, id, path);
    let remove = state
        .by_id
        .get(&id)
        .is_some_and(|entry| entry.paths.is_empty() && entry.id != ROOT_HANDLE_ID);
    if remove
        && let Some(entry) = state.by_id.remove(&id)
        && let Some(key) = entry.key
    {
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

#[derive(Debug, Clone)]
pub struct DirectorySnapshots {
    inner: Arc<Mutex<HashMap<u64, DirectorySnapshot>>>,
    order: Arc<Mutex<VecDeque<u64>>>,
    max: usize,
}

impl DirectorySnapshots {
    pub fn new(max: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            order: Arc::new(Mutex::new(VecDeque::new())),
            max: max.max(1),
        }
    }

    pub fn set(&self, id: u64, names: Vec<String>) -> DirectorySnapshot {
        let verifier = cookie_verifier(&names);
        let snapshot = DirectorySnapshot { names, verifier };
        let mut inner = self.inner.lock().expect("directory snapshot lock");
        let mut order = self.order.lock().expect("directory snapshot order lock");
        if inner.contains_key(&id) {
            order.retain(|candidate| *candidate != id);
        } else if inner.len() >= self.max
            && let Some(oldest) = order.pop_front()
        {
            inner.remove(&oldest);
        }
        inner.insert(id, snapshot.clone());
        order.push_back(id);
        snapshot
    }

    pub fn get(&self, id: u64) -> Option<DirectorySnapshot> {
        let snapshot = self
            .inner
            .lock()
            .expect("directory snapshot lock")
            .get(&id)
            .cloned();
        if snapshot.is_some() {
            let mut order = self.order.lock().expect("directory snapshot order lock");
            order.retain(|candidate| *candidate != id);
            order.push_back(id);
        }
        snapshot
    }

    pub fn invalidate(&self, id: u64) {
        self.inner
            .lock()
            .expect("directory snapshot lock")
            .remove(&id);
        self.order
            .lock()
            .expect("directory snapshot order lock")
            .retain(|candidate| *candidate != id);
    }

    pub fn clear(&self) {
        self.inner.lock().expect("directory snapshot lock").clear();
        self.order
            .lock()
            .expect("directory snapshot order lock")
            .clear();
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
    fn rename_preserves_a_single_path_handle() {
        let table = FileHandleTable::default();
        let entry = table.bind("/a", &stats(11));
        let handle = table.encode(&entry);

        table.remap("/a", "/b");

        assert_eq!(table.resolve(&handle).unwrap(), "/b");
        assert_eq!(table.at("/b").unwrap().id, entry.id);
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
}
