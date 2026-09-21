//! Shared POSIX-style byte-range locks for 9P `Tlock`/`Tgetlock`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

#[derive(Default)]
struct LockState {
    files: HashMap<String, Vec<Lock>>,
    next_client: u64,
}

#[derive(Clone)]
pub struct P9LockTable {
    state: Arc<Mutex<LockState>>,
    max_locks_per_file: usize,
}

impl P9LockTable {
    pub fn new(options: P9LockTableOptions) -> Self {
        Self {
            state: Arc::new(Mutex::new(LockState::default())),
            max_locks_per_file: options.max_locks_per_file.max(1),
        }
    }

    pub fn client(&self) -> P9LockClient {
        let mut state = self.state.lock().expect("9P lock table mutex poisoned");
        state.next_client = state.next_client.saturating_add(1);
        P9LockClient {
            table: self.clone(),
            id: state.next_client,
        }
    }

    pub fn size(&self) -> usize {
        self.state
            .lock()
            .expect("9P lock table mutex poisoned")
            .files
            .values()
            .map(Vec::len)
            .sum()
    }

    /// Number of paths that currently carry at least one granted range.
    pub fn files(&self) -> usize {
        self.state
            .lock()
            .expect("9P lock table mutex poisoned")
            .files
            .len()
    }

    /// Return the granted ranges for one path in ascending start order.
    pub fn at(&self, path: &str) -> Vec<P9Lock> {
        let state = self.state.lock().expect("9P lock table mutex poisoned");
        let mut locks = state
            .files
            .get(path)
            .map(|locks| locks.iter().map(lock_record).collect::<Vec<_>>())
            .unwrap_or_default();
        locks.sort_by_key(|lock| lock.start);
        locks
    }

    fn lock(&self, holder: u64, request: &P9LockRequest) -> Result<u8> {
        let (start, end) = range(request.start, request.length)?;
        let owner = owner_key(request);
        let mut state = self.state.lock().expect("9P lock table mutex poisoned");
        let locks = state.files.entry(request.path.clone()).or_default();
        if request.type_ == P9_LOCK_TYPE_UNLCK {
            subtract(locks, &owner, start, end);
            if locks.is_empty() {
                state.files.remove(&request.path);
            }
            return Ok(P9_LOCK_SUCCESS);
        }
        if request.type_ != P9_LOCK_TYPE_RDLCK && request.type_ != P9_LOCK_TYPE_WRLCK {
            return Err(FsError::new(ErrorCode::Einval).with_message("EINVAL: invalid lock type"));
        }
        if locks.iter().any(|lock| {
            overlaps(lock.start, lock.end, start, end)
                && lock.owner != owner
                && (lock.type_ == P9_LOCK_TYPE_WRLCK || request.type_ == P9_LOCK_TYPE_WRLCK)
        }) {
            return Ok(P9_LOCK_BLOCKED);
        }
        if locks.len() >= self.max_locks_per_file {
            return Ok(P9_LOCK_ERROR);
        }
        subtract(locks, &owner, start, end);
        locks.push(Lock {
            start,
            end,
            type_: request.type_,
            owner,
            proc_id: request.proc_id,
            client_id: request.client_id.clone(),
            holder,
            fid: request.fid,
        });
        coalesce(locks);
        Ok(P9_LOCK_SUCCESS)
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
        let state = self.state.lock().expect("9P lock table mutex poisoned");
        Ok(state.files.get(&request.path).and_then(|locks| {
            locks
                .iter()
                .find(|lock| {
                    overlaps(lock.start, lock.end, start, end)
                        && lock.owner != owner
                        && request.type_ != P9_LOCK_TYPE_UNLCK
                        && (lock.type_ == P9_LOCK_TYPE_WRLCK || request.type_ == P9_LOCK_TYPE_WRLCK)
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

    fn drop_where(&self, predicate: impl Fn(&Lock) -> bool) {
        let mut state = self.state.lock().expect("9P lock table mutex poisoned");
        state.files.retain(|_, locks| {
            locks.retain(|lock| !predicate(lock));
            !locks.is_empty()
        });
    }

    pub fn remap(&self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let mut state = self.state.lock().expect("9P lock table mutex poisoned");
        let source: Vec<String> = state
            .files
            .keys()
            .filter(|path| {
                *path == from
                    || (from == "/" && path.starts_with('/'))
                    || path.starts_with(&format!("{from}/"))
            })
            .cloned()
            .collect();
        for old in source {
            if let Some(locks) = state.files.remove(&old) {
                let new = format!("{to}{}", &old[from.len()..]);
                state.files.remove(&new);
                state.files.insert(new, locks);
            }
        }
    }

    pub fn release(&self, path: &str) {
        let mut state = self.state.lock().expect("9P lock table mutex poisoned");
        state.files.remove(path);
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
        let state = self
            .table
            .state
            .lock()
            .expect("9P lock table mutex poisoned");
        state
            .files
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
}
