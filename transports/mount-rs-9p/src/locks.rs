//! Shared POSIX-style byte-range locks for 9P `Tlock`/`Tgetlock`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use mount_rs_core::{ErrorCode, FsError, Result};

use crate::constants::{
    P9_LOCK_BLOCKED, P9_LOCK_ERROR, P9_LOCK_SUCCESS, P9_LOCK_TYPE_RDLCK, P9_LOCK_TYPE_UNLCK,
    P9_LOCK_TYPE_WRLCK,
};

/// Exclusive end offset used for an open-ended lock range (`length == 0`).
///
/// A `u64` end offset cannot represent the byte immediately after
/// `u64::MAX`.  Keeping the internal range in `u128` preserves the valid
/// one-byte interval `[u64::MAX, 2^64)` and lets overflow checks distinguish
/// it from a wrapped range.
pub const P9_LOCK_EOF_END: u128 = 1u128 << 64;
pub const DEFAULT_MAX_LOCKS_PER_FILE: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P9LockRequest {
    pub path: String,
    pub fid: u32,
    pub type_: u8,
    pub start: u64,
    pub length: u64,
    pub proc_id: u32,
    pub client_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P9LockHolder {
    pub type_: u8,
    pub start: u64,
    pub length: u64,
    pub proc_id: u32,
    pub client_id: String,
}

/// One granted byte range, including the connection and fid that own it.
///
/// This is the inspection form of the private table record.  Keeping the
/// holder and fid alongside the wire-visible range is important: a clunk
/// releases ranges by fid, while conflicts are judged by the `(proc_id,
/// client_id)` owner pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P9Lock {
    pub type_: u8,
    pub start: u64,
    pub length: u64,
    pub proc_id: u32,
    pub client_id: String,
    pub holder: u64,
    pub fid: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct P9LockTableOptions {
    pub max_locks_per_file: usize,
}

impl Default for P9LockTableOptions {
    fn default() -> Self {
        Self {
            max_locks_per_file: DEFAULT_MAX_LOCKS_PER_FILE,
        }
    }
}

#[derive(Debug, Clone)]
struct Lock {
    start: u128,
    end: u128,
    type_: u8,
    owner: String,
    proc_id: u32,
    client_id: String,
    holder: u64,
    fid: u32,
}

/// Granted ranges by path. Never changed in place: an operation builds the
/// next map and swaps it in, so readers never wait. The map is persistent:
/// the next map shares all of the current one but the path to the entry
/// that changed, so an operation costs the same however many paths hold
/// locks. One path's list is copied whole, and is capped by
/// `max_locks_per_file`.
type LockFiles = im::HashMap<Arc<str>, Arc<Vec<Lock>>>;

struct LockState {
    files: ArcSwap<LockFiles>,
    next_client: AtomicU64,
}

#[derive(Clone)]
pub struct P9LockTable {
    state: Arc<LockState>,
    max_locks_per_file: usize,
}

impl P9LockTable {
    pub fn new(options: P9LockTableOptions) -> Self {
        Self {
            state: Arc::new(LockState {
                files: ArcSwap::from_pointee(LockFiles::new()),
                next_client: AtomicU64::new(0),
            }),
            max_locks_per_file: options.max_locks_per_file.max(1),
        }
    }

    pub fn client(&self) -> P9LockClient {
        P9LockClient {
            table: self.clone(),
            id: self.state.next_client.fetch_add(1, Ordering::Relaxed) + 1,
        }
    }

    pub fn size(&self) -> usize {
        self.state
            .files
            .load()
            .values()
            .map(|locks| locks.len())
            .sum()
    }

    /// Number of paths that currently carry at least one granted range.
    pub fn files(&self) -> usize {
        self.state.files.load().len()
    }

    /// Return the granted ranges for one path in ascending start order.
    pub fn at(&self, path: &str) -> Vec<P9Lock> {
        let mut locks = self
            .state
            .files
            .load()
            .get(path)
            .map(|locks| locks.iter().map(lock_record).collect::<Vec<_>>())
            .unwrap_or_default();
        locks.sort_by_key(|lock| lock.start);
        locks
    }

    fn lock(&self, holder: u64, request: &P9LockRequest) -> Result<u8> {
        let (start, end) = range(request.start, request.length)?;
        let unlock = request.type_ == P9_LOCK_TYPE_UNLCK;
        if !unlock && request.type_ != P9_LOCK_TYPE_RDLCK && request.type_ != P9_LOCK_TYPE_WRLCK {
            return Err(FsError::new(ErrorCode::Einval).with_message("EINVAL: invalid lock type"));
        }
        let owner = owner_key(request);
        let path = request.path.as_str();
        let wanted = Lock {
            start,
            end,
            type_: request.type_,
            owner,
            proc_id: request.proc_id,
            client_id: request.client_id.clone(),
            holder,
            fid: request.fid,
        };
        // The conflict check and the grant are one step: the closure judges
        // the map it was given, and its result only replaces that same map.
        // It may run again on a newer map, so it sets `status` afresh each
        // time and does nothing else.
        let mut status = P9_LOCK_SUCCESS;
        self.state.files.rcu(|current| {
            let existing = current.get_key_value(path);
            let held = existing.map_or(&[][..], |(_, locks)| locks.as_slice());
            status = P9_LOCK_SUCCESS;
            let locks = if unlock {
                if !held.iter().any(|lock| {
                    lock.owner == wanted.owner && overlaps(lock.start, lock.end, start, end)
                }) {
                    return Arc::clone(current);
                }
                let mut locks = held.to_vec();
                subtract(&mut locks, &wanted.owner, start, end);
                locks
            } else {
                if held.iter().any(|lock| {
                    overlaps(lock.start, lock.end, start, end)
                        && lock.owner != wanted.owner
                        && (lock.type_ == P9_LOCK_TYPE_WRLCK || wanted.type_ == P9_LOCK_TYPE_WRLCK)
                }) {
                    status = P9_LOCK_BLOCKED;
                    return Arc::clone(current);
                }
                if held.len() >= self.max_locks_per_file {
                    status = P9_LOCK_ERROR;
                    return Arc::clone(current);
                }
                let mut locks = held.to_vec();
                subtract(&mut locks, &wanted.owner, start, end);
                locks.push(wanted.clone());
                coalesce(&mut locks);
                locks
            };
            let mut next = LockFiles::clone(current);
            if locks.is_empty() {
                next.remove(path);
            } else {
                let key = existing.map_or_else(|| Arc::from(path), |(key, _)| Arc::clone(key));
                next.insert(key, Arc::new(locks));
            }
            Arc::new(next)
        });
        Ok(status)
    }

    pub fn getlock(&self, request: &P9LockRequest) -> Result<Option<P9LockHolder>> {
        let (start, end) = range(request.start, request.length)?;
        if request.type_ != P9_LOCK_TYPE_RDLCK
            && request.type_ != P9_LOCK_TYPE_WRLCK
            && request.type_ != P9_LOCK_TYPE_UNLCK
        {
            return Err(FsError::new(ErrorCode::Einval).with_message("EINVAL: invalid lock type"));
        }
        let owner = owner_key(request);
        Ok(self
            .state
            .files
            .load()
            .get(request.path.as_str())
            .and_then(|locks| {
                locks
                    .iter()
                    .find(|lock| {
                        overlaps(lock.start, lock.end, start, end)
                            && lock.owner != owner
                            && request.type_ != P9_LOCK_TYPE_UNLCK
                            && (lock.type_ == P9_LOCK_TYPE_WRLCK
                                || request.type_ == P9_LOCK_TYPE_WRLCK)
                    })
                    .map(holder)
            }))
    }

    fn release_fid(&self, holder: u64, fid: u32) {
        self.drop_where(|lock| lock.holder == holder && lock.fid == fid);
    }

    fn release_all(&self, holder: u64) {
        self.drop_where(|lock| lock.holder == holder);
    }

    /// Every clunk and every session teardown comes through here, almost
    /// always with nothing to drop: look first and copy only when a range
    /// goes.
    fn drop_where(&self, predicate: impl Fn(&Lock) -> bool) {
        let any = |files: &LockFiles| files.values().any(|locks| locks.iter().any(&predicate));
        if !any(&self.state.files.load()) {
            return;
        }
        self.state.files.rcu(|current| {
            if !any(current) {
                return Arc::clone(current);
            }
            // Only the paths that lose a range are rewritten.
            let mut next = LockFiles::clone(current);
            for (path, locks) in current.iter() {
                if !locks.iter().any(&predicate) {
                    continue;
                }
                let kept = locks
                    .iter()
                    .filter(|lock| !predicate(lock))
                    .cloned()
                    .collect::<Vec<_>>();
                if kept.is_empty() {
                    next.remove(path);
                } else {
                    next.insert(Arc::clone(path), Arc::new(kept));
                }
            }
            Arc::new(next)
        });
    }

    pub fn remap(&self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let prefix = format!("{from}/");
        let moves = |path: &str| {
            path == from || (from == "/" && path.starts_with('/')) || path.starts_with(&prefix)
        };
        self.state.files.rcu(|current| {
            let source = current
                .keys()
                .filter(|path| moves(path))
                .cloned()
                .collect::<Vec<_>>();
            if source.is_empty() {
                return Arc::clone(current);
            }
            let mut next = LockFiles::clone(current);
            for old in source {
                if let Some(locks) = next.remove(&old) {
                    let new = format!("{to}{}", &old[from.len()..]);
                    next.insert(Arc::from(new), locks);
                }
            }
            Arc::new(next)
        });
    }

    pub fn release(&self, path: &str) {
        self.state.files.rcu(|current| {
            if !current.contains_key(path) {
                return Arc::clone(current);
            }
            let mut next = LockFiles::clone(current);
            next.remove(path);
            Arc::new(next)
        });
    }
}

#[derive(Clone)]
pub struct P9LockClient {
    pub table: P9LockTable,
    pub id: u64,
}

impl P9LockClient {
    /// Number of ranges held by this connection across all paths.
    pub fn held(&self) -> usize {
        self.table
            .state
            .files
            .load()
            .values()
            .flat_map(|locks| locks.iter())
            .filter(|lock| lock.holder == self.id)
            .count()
    }

    pub fn lock(&self, request: &P9LockRequest) -> Result<u8> {
        self.table.lock(self.id, request)
    }
    pub fn getlock(&self, request: &P9LockRequest) -> Result<Option<P9LockHolder>> {
        self.table.getlock(request)
    }
    pub fn release_fid(&self, fid: u32) {
        self.table.release_fid(self.id, fid);
    }
    pub fn release_all(&self) {
        self.table.release_all(self.id);
    }
    pub fn renamed(&self, from: &str, to: &str) {
        self.table.remap(from, to);
    }
    pub fn released(&self, path: &str) {
        self.table.release(path);
    }
}

fn owner_key(request: &P9LockRequest) -> String {
    format!("{}:{}", request.proc_id, request.client_id)
}

fn range(start: u64, length: u64) -> Result<(u128, u128)> {
    let start = u128::from(start);
    let end = if length == 0 {
        P9_LOCK_EOF_END
    } else {
        start.checked_add(u128::from(length)).ok_or_else(|| {
            FsError::new(ErrorCode::Einval).with_message("EINVAL: lock range overflows")
        })?
    };
    if end > P9_LOCK_EOF_END {
        return Err(FsError::new(ErrorCode::Einval).with_message("EINVAL: invalid lock range"));
    }
    Ok((start, end))
}

fn overlaps(left_start: u128, left_end: u128, right_start: u128, right_end: u128) -> bool {
    left_start < right_end && right_start < left_end
}

fn subtract(locks: &mut Vec<Lock>, owner: &str, start: u128, end: u128) {
    let mut result = Vec::with_capacity(locks.len());
    for lock in locks.drain(..) {
        if lock.owner != owner || !overlaps(lock.start, lock.end, start, end) {
            result.push(lock);
            continue;
        }
        if lock.start < start {
            result.push(Lock {
                end: start,
                ..lock.clone()
            });
        }
        if lock.end > end {
            result.push(Lock { start: end, ..lock });
        }
    }
    *locks = result;
}

fn coalesce(locks: &mut Vec<Lock>) {
    locks.sort_by_key(|lock| lock.start);
    let mut result: Vec<Lock> = Vec::with_capacity(locks.len());
    for lock in locks.drain(..) {
        if let Some(previous) = result.last_mut()
            && previous.end >= lock.start
            && previous.type_ == lock.type_
            && previous.owner == lock.owner
            && previous.holder == lock.holder
            && previous.fid == lock.fid
        {
            previous.end = previous.end.max(lock.end);
        } else {
            result.push(lock);
        }
    }
    *locks = result;
}

fn holder(lock: &Lock) -> P9LockHolder {
    P9LockHolder {
        type_: lock.type_,
        start: lock.start as u64,
        length: if lock.end == P9_LOCK_EOF_END {
            0
        } else {
            (lock.end - lock.start) as u64
        },
        proc_id: lock.proc_id,
        client_id: lock.client_id.clone(),
    }
}

fn lock_record(lock: &Lock) -> P9Lock {
    P9Lock {
        type_: lock.type_,
        start: lock.start as u64,
        length: if lock.end == P9_LOCK_EOF_END {
            0
        } else {
            (lock.end - lock.start) as u64
        },
        proc_id: lock.proc_id,
        client_id: lock.client_id.clone(),
        holder: lock.holder,
        fid: lock.fid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(start: u64, length: u64, type_: u8, client_id: &str) -> P9LockRequest {
        P9LockRequest {
            path: "/file".to_owned(),
            fid: 1,
            type_,
            start,
            length,
            proc_id: 7,
            client_id: client_id.to_owned(),
        }
    }

    #[test]
    fn lock_range_preserves_last_u64_byte_and_eof() {
        let table = P9LockTable::new(Default::default());
        let first = table.client();
        let second = table.client();
        assert_eq!(
            first
                .lock(&request(u64::MAX, 1, P9_LOCK_TYPE_WRLCK, "first"))
                .unwrap(),
            P9_LOCK_SUCCESS
        );
        let holder = second
            .getlock(&request(u64::MAX, 1, P9_LOCK_TYPE_RDLCK, "second"))
            .unwrap()
            .unwrap();
        assert_eq!(holder.start, u64::MAX);
        // The wire representation reserves zero for a to-EOF range, and the
        // finite interval ending at 2^64 has the same exclusive end.
        assert_eq!(holder.length, 0);

        first.release_all();
        assert_eq!(
            first
                .lock(&request(u64::MAX, 0, P9_LOCK_TYPE_WRLCK, "first"))
                .unwrap(),
            P9_LOCK_SUCCESS
        );
        let holder = second
            .getlock(&request(0, 0, P9_LOCK_TYPE_RDLCK, "second"))
            .unwrap()
            .unwrap();
        assert_eq!(holder.start, u64::MAX);
        assert_eq!(holder.length, 0);
    }

    #[test]
    fn nonzero_range_that_exceeds_eof_is_rejected() {
        let table = P9LockTable::new(Default::default());
        let client = table.client();
        let error = client
            .lock(&request(u64::MAX, u64::MAX, P9_LOCK_TYPE_WRLCK, "first"))
            .unwrap_err();
        assert!(error.is(ErrorCode::Einval));
    }

    /// Many sessions ask for the same exclusive range at once: the table
    /// never grants two of them, whichever order they arrive in.
    #[test]
    fn conflicting_locks_are_never_both_granted() {
        const THREADS: usize = 8;
        const ROUNDS: usize = 300;
        let table = P9LockTable::new(Default::default());
        let holders = std::sync::atomic::AtomicUsize::new(0);
        let granted = std::sync::atomic::AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(THREADS);
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let client = table.client();
                let (holders, granted, barrier, table) = (&holders, &granted, &barrier, &table);
                scope.spawn(move || {
                    let owner = format!("client-{thread}");
                    barrier.wait();
                    for round in 0..ROUNDS {
                        // Overlapping, not identical, ranges; every one
                        // covers byte 50.
                        let start = (thread as u64 * 7 + round as u64) % 50;
                        let write = request(start, 60, P9_LOCK_TYPE_WRLCK, &owner);
                        match client.lock(&write).unwrap() {
                            P9_LOCK_SUCCESS => {
                                assert_eq!(
                                    holders.fetch_add(1, Ordering::SeqCst),
                                    0,
                                    "two owners hold overlapping write locks"
                                );
                                granted.fetch_add(1, Ordering::Relaxed);
                                let at = table.at("/file");
                                assert_eq!(at.len(), 1);
                                assert_eq!(at[0].client_id, owner);
                                assert_eq!(client.held(), 1);
                                holders.fetch_sub(1, Ordering::SeqCst);
                                let unlock = request(0, 0, P9_LOCK_TYPE_UNLCK, &owner);
                                assert_eq!(client.lock(&unlock).unwrap(), P9_LOCK_SUCCESS);
                            }
                            P9_LOCK_BLOCKED => {
                                // Releasing what it does not hold changes
                                // nothing for the holder.
                                client.release_fid(1);
                            }
                            other => panic!("unexpected lock status {other}"),
                        }
                    }
                });
            }
        });
        assert!(granted.load(Ordering::Relaxed) > 0);
        assert_eq!(table.size(), 0);
        assert_eq!(table.files(), 0);
    }

    /// Readers share a range with each other and never with a writer.
    #[test]
    fn readers_and_a_writer_never_overlap() {
        const THREADS: usize = 8;
        let table = P9LockTable::new(Default::default());
        // Positive: that many readers; negative: a writer.
        let state = std::sync::atomic::AtomicIsize::new(0);
        let barrier = std::sync::Barrier::new(THREADS);
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let client = table.client();
                let (state, barrier) = (&state, &barrier);
                scope.spawn(move || {
                    let owner = format!("client-{thread}");
                    barrier.wait();
                    for round in 0..300 {
                        let writer = (thread + round) % 4 == 0;
                        let type_ = if writer {
                            P9_LOCK_TYPE_WRLCK
                        } else {
                            P9_LOCK_TYPE_RDLCK
                        };
                        if client.lock(&request(10, 10, type_, &owner)).unwrap() != P9_LOCK_SUCCESS
                        {
                            continue;
                        }
                        if writer {
                            assert_eq!(state.swap(-1, Ordering::SeqCst), 0, "writer is alone");
                            assert_eq!(state.swap(0, Ordering::SeqCst), -1);
                        } else {
                            assert!(
                                state.fetch_add(1, Ordering::SeqCst) >= 0,
                                "reader met writer"
                            );
                            assert!(state.fetch_sub(1, Ordering::SeqCst) >= 1);
                        }
                        client.release_all();
                    }
                });
            }
        });
        assert_eq!(table.size(), 0);
    }

    #[test]
    fn invalid_lock_type_leaves_no_entry_for_the_path() {
        let table = P9LockTable::new(Default::default());
        let client = table.client();
        let error = client.lock(&request(0, 1, 0x7f, "first")).unwrap_err();
        assert!(error.is(ErrorCode::Einval));
        assert_eq!(
            table.files(),
            0,
            "a refused lock must not leave a path entry"
        );
        assert_eq!(table.size(), 0);
        // Unlocking a path with no locks leaves none either.
        assert_eq!(
            client
                .lock(&request(0, 1, P9_LOCK_TYPE_UNLCK, "first"))
                .unwrap(),
            P9_LOCK_SUCCESS
        );
        assert_eq!(table.files(), 0);
    }

    #[test]
    fn operations_that_change_nothing_keep_the_same_map() {
        let table = P9LockTable::new(Default::default());
        let first = table.client();
        let second = table.client();
        assert_eq!((first.id, second.id), (1, 2));
        assert_eq!(
            first
                .lock(&request(0, 10, P9_LOCK_TYPE_WRLCK, "first"))
                .unwrap(),
            P9_LOCK_SUCCESS
        );
        let before = table.state.files.load_full();
        assert_eq!(
            second
                .lock(&request(5, 10, P9_LOCK_TYPE_WRLCK, "second"))
                .unwrap(),
            P9_LOCK_BLOCKED
        );
        second.release_all();
        second.release_fid(1);
        first.release_fid(2);
        second
            .lock(&request(0, 0, P9_LOCK_TYPE_UNLCK, "second"))
            .unwrap();
        table.release("/other");
        table.remap("/other", "/elsewhere");
        assert!(Arc::ptr_eq(&before, &table.state.files.load_full()));
        table.remap("/file", "/moved");
        assert_eq!(table.at("/moved").len(), 1);
        assert!(table.at("/file").is_empty());
        first.release_all();
        assert_eq!(table.files(), 0);
    }

    fn path_request(path: &str, type_: u8, client_id: &str) -> P9LockRequest {
        P9LockRequest {
            path: path.to_owned(),
            ..request(0, 10, type_, client_id)
        }
    }

    /// Thousands of paths locked at once, taken and released from several
    /// threads: every grant lands on its own path, a second owner is still
    /// refused there, and releasing leaves nothing behind.
    #[test]
    fn many_paths_locked_at_once_stay_independent() {
        const THREADS: usize = 8;
        const PATHS: usize = 1_000;
        let table = P9LockTable::new(Default::default());
        let rival = table.client();
        let barrier = std::sync::Barrier::new(THREADS);
        let clients = std::thread::scope(|scope| {
            let workers = (0..THREADS)
                .map(|thread| {
                    let client = table.client();
                    let (table, rival, barrier) = (&table, &rival, &barrier);
                    scope.spawn(move || {
                        let owner = format!("client-{thread}");
                        barrier.wait();
                        for index in 0..PATHS {
                            let path = format!("/dir-{thread}/file-{index}");
                            let write = path_request(&path, P9_LOCK_TYPE_WRLCK, &owner);
                            assert_eq!(client.lock(&write).unwrap(), P9_LOCK_SUCCESS);
                            let other = path_request(&path, P9_LOCK_TYPE_WRLCK, "rival");
                            assert_eq!(rival.lock(&other).unwrap(), P9_LOCK_BLOCKED);
                            assert_eq!(table.at(&path).len(), 1);
                            // Every third path is unlocked again at once.
                            if index % 3 == 0 {
                                let unlock = path_request(&path, P9_LOCK_TYPE_UNLCK, &owner);
                                assert_eq!(client.lock(&unlock).unwrap(), P9_LOCK_SUCCESS);
                                assert!(table.at(&path).is_empty());
                            }
                        }
                        client
                    })
                })
                .collect::<Vec<_>>();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        let kept = PATHS - PATHS.div_ceil(3);
        assert_eq!(table.files(), THREADS * kept);
        assert_eq!(table.size(), THREADS * kept);
        assert_eq!(rival.held(), 0);
        for (thread, client) in clients.iter().enumerate() {
            assert_eq!(client.held(), kept);
            let path = format!("/dir-{thread}/file-1");
            assert_eq!(table.at(&path)[0].holder, client.id);
        }
        // A directory rename moves that directory's locks and no others.
        table.remap("/dir-0", "/moved");
        assert_eq!(table.files(), THREADS * kept);
        assert_eq!(table.at("/moved/file-1").len(), 1);
        assert!(table.at("/dir-0/file-1").is_empty());
        assert_eq!(table.at("/dir-1/file-1").len(), 1);
        table.release("/moved/file-1");
        assert_eq!(table.files(), THREADS * kept - 1);
        // One session going away drops its paths and leaves the rest.
        clients[1].release_all();
        assert_eq!(table.files(), (THREADS - 1) * kept - 1);
        assert!(table.at("/dir-1/file-1").is_empty());
        assert_eq!(table.at("/dir-2/file-1").len(), 1);
        for client in &clients {
            client.release_all();
        }
        assert_eq!(table.files(), 0);
        assert_eq!(table.size(), 0);
    }
}
