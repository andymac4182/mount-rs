//! WebDAV write-lock state (RFC 4918 sections 6, 7, and 9.10).

use std::collections::HashMap;
use std::sync::Arc;

use mount_rs_core::path::is_path_inside;
use uuid::Uuid;

use crate::constants::{
    DEFAULT_LOCK_TIMEOUT_SECONDS, LOCK_TOKEN_PREFIX, MAX_LOCK_TIMEOUT_SECONDS, MAX_LOCKS,
};
use crate::protocol::{LockTimeout, XmlNode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockDepth {
    Zero,
    Infinity,
}

impl std::fmt::Display for LockDepth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Zero => f.write_str("0"),
            Self::Infinity => f.write_str("infinity"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DavLock {
    pub token: String,
    pub path: String,
    pub collection: bool,
    pub depth: LockDepth,
    pub exclusive: bool,
    pub owner: Option<XmlNode>,
    pub timeout_seconds: u64,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct DavLockRequest {
    pub path: String,
    pub collection: bool,
    pub depth: LockDepth,
    pub exclusive: bool,
    pub owner: Option<XmlNode>,
    pub timeout: Option<LockTimeout>,
}

#[derive(Debug, Clone)]
pub enum DavLockGrant {
    Granted(DavLock),
    Conflict(DavLock),
    Full,
}

#[derive(Clone)]
pub struct DavLockTableOptions {
    pub default_timeout_seconds: u64,
    pub max_timeout_seconds: u64,
    pub max_locks: usize,
    pub new_token: Option<Arc<dyn Fn() -> String + Send + Sync>>,
}

impl std::fmt::Debug for DavLockTableOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DavLockTableOptions")
            .field("default_timeout_seconds", &self.default_timeout_seconds)
            .field("max_timeout_seconds", &self.max_timeout_seconds)
            .field("max_locks", &self.max_locks)
            .field("new_token", &self.new_token.is_some())
            .finish()
    }
}

impl Default for DavLockTableOptions {
    fn default() -> Self {
        Self {
            default_timeout_seconds: DEFAULT_LOCK_TIMEOUT_SECONDS,
            max_timeout_seconds: MAX_LOCK_TIMEOUT_SECONDS,
            max_locks: MAX_LOCKS,
            new_token: None,
        }
    }
}

pub struct DavLockTable {
    locks: HashMap<String, DavLock>,
    options: DavLockTableOptions,
}

impl std::fmt::Debug for DavLockTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DavLockTable")
            .field("locks", &self.locks)
            .field("options", &self.options)
            .finish()
    }
}

impl DavLockTable {
    pub fn new(options: DavLockTableOptions) -> Self {
        Self {
            locks: HashMap::new(),
            options,
        }
    }

    pub fn size(&mut self, now: i64) -> usize {
        self.sweep(now);
        self.locks.len()
    }

    pub fn all(&mut self, now: i64) -> Vec<DavLock> {
        self.sweep(now);
        self.locks.values().cloned().collect()
    }

    pub fn covering(&mut self, path: &str, now: i64) -> Vec<DavLock> {
        self.sweep(now);
        self.locks
            .values()
            .filter(|lock| {
                lock.path == path
                    || (lock.depth == LockDepth::Infinity && is_path_inside(path, &lock.path))
            })
            .cloned()
            .collect()
    }

    pub fn within(&mut self, path: &str, now: i64) -> Vec<DavLock> {
        self.sweep(now);
        self.locks
            .values()
            .filter(|lock| is_path_inside(&lock.path, path))
            .cloned()
            .collect()
    }

    pub fn find(&mut self, token: &str, now: i64) -> Option<DavLock> {
        self.sweep(now);
        self.locks.get(token).cloned()
    }

    pub fn in_scope(lock: &DavLock, path: &str) -> bool {
        lock.path == path || (lock.depth == LockDepth::Infinity && is_path_inside(path, &lock.path))
    }

    pub fn conflict(
        &mut self,
        path: &str,
        depth: LockDepth,
        exclusive: bool,
        now: i64,
    ) -> Option<DavLock> {
        let mut candidates = self.covering(path, now);
        if depth == LockDepth::Infinity {
            candidates.extend(self.within(path, now));
        }
        candidates
            .into_iter()
            .find(|lock| exclusive || lock.exclusive)
    }

    pub fn create(&mut self, request: DavLockRequest, now: i64) -> DavLockGrant {
        if let Some(lock) = self.conflict(&request.path, request.depth, request.exclusive, now) {
            return DavLockGrant::Conflict(lock);
        }
        if self.size(now) >= self.options.max_locks {
            return DavLockGrant::Full;
        }
        let timeout_seconds = self.granted_timeout(request.timeout);
        let token = self.options.new_token.as_ref().map_or_else(
            || format!("{LOCK_TOKEN_PREFIX}{}", Uuid::new_v4()),
            |new_token| new_token(),
        );
        let lock = DavLock {
            token: token.clone(),
            path: request.path,
            collection: request.collection,
            depth: request.depth,
            exclusive: request.exclusive,
            owner: request.owner,
            timeout_seconds,
            expires_at: now.saturating_add((timeout_seconds as i64).saturating_mul(1000)),
        };
        self.locks.insert(token, lock.clone());
        DavLockGrant::Granted(lock)
    }

    pub fn refresh(
        &mut self,
        token: &str,
        requested: Option<LockTimeout>,
        now: i64,
    ) -> Option<DavLock> {
        let existing = self.find(token, now)?;
        let timeout_seconds = self.granted_timeout(requested);
        let refreshed = DavLock {
            timeout_seconds,
            expires_at: now.saturating_add((timeout_seconds as i64).saturating_mul(1000)),
            ..existing
        };
        self.locks.insert(token.to_owned(), refreshed.clone());
        Some(refreshed)
    }

    pub fn remove(&mut self, token: &str) -> bool {
        self.locks.remove(token).is_some()
    }

    pub fn remaining(lock: &DavLock, now: i64) -> u64 {
        if lock.expires_at <= now {
            0
        } else {
            ((lock.expires_at - now) / 1000) as u64
        }
    }

    fn granted_timeout(&self, requested: Option<LockTimeout>) -> u64 {
        match requested {
            None => self.options.default_timeout_seconds,
            Some(LockTimeout::Infinite) => self.options.max_timeout_seconds,
            Some(LockTimeout::Seconds(seconds)) => {
                // Keep the public option boundary non-panicking even when a
                // caller supplies a zero maximum. This mirrors the oracle's
                // finite-timeout ordering: cap first, then enforce the
                // protocol's one-second minimum.
                seconds.min(self.options.max_timeout_seconds).max(1)
            }
        }
    }

    fn sweep(&mut self, now: i64) {
        self.locks.retain(|_, lock| lock.expires_at > now);
    }
}

impl Default for DavLockTable {
    fn default() -> Self {
        Self::new(DavLockTableOptions::default())
    }
}
