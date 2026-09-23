//! WebDAV write-lock state (RFC 4918 sections 6, 7, and 9.10).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mount_rs_core::path::is_path_inside;
use url::Url;
use uuid::Uuid;

use crate::constants::{
    DEFAULT_LOCK_TIMEOUT_SECONDS, LOCK_TOKEN_PREFIX, MAX_LOCK_TIMEOUT_SECONDS, MAX_LOCKS,
};
use crate::protocol::{LockTimeout, XmlNode};

const MAX_INJECTED_TOKEN_BYTES: usize = 1_024;

fn usable_lock_token_uri(token: &str) -> bool {
    let Some((scheme, value)) = token.split_once(':') else {
        return false;
    };
    let mut scheme_bytes = scheme.bytes();
    if !scheme_bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic())
        || !scheme_bytes.all(|byte| byte.is_ascii_alphanumeric() || b"+-.".contains(&byte))
        || (scheme.eq_ignore_ascii_case("DAV") && value == "no-lock")
    {
        return false;
    }

    let bytes = value.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        let byte = bytes[at];
        if byte == b'%' {
            if !bytes.get(at + 1).is_some_and(u8::is_ascii_hexdigit)
                || !bytes.get(at + 2).is_some_and(u8::is_ascii_hexdigit)
            {
                return false;
            }
            at += 3;
        } else if byte.is_ascii_alphanumeric() || b"-._~:/?[]@!$&'()*+,;=".contains(&byte) {
            at += 1;
        } else {
            return false;
        }
    }
    Url::parse(token).is_ok_and(|uri| uri.fragment().is_none())
}

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
    /// Active-lock capacity or an unusable or exhausted configured generator.
    Full,
}

#[derive(Clone)]
pub struct DavLockTableOptions {
    pub default_timeout_seconds: u64,
    pub max_timeout_seconds: u64,
    pub max_locks: usize,
    /// Supplies a lock-token URI when configured. A fresh value is used as
    /// given; repeats within one table use a UUID fallback. The generator
    /// must avoid reuse across tables, servers, and process restarts, which
    /// one table cannot check. Values must be usable ASCII absolute URIs and
    /// cannot be the reserved `DAV:no-lock` sentinel. Configured tables fail
    /// closed after [`MAX_LOCKS`] lifetime grants or when a value exceeds
    /// 1,024 bytes.
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
    issued: HashSet<String>,
    order: Vec<String>,
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
            issued: HashSet::new(),
            order: Vec::new(),
            options,
        }
    }

    pub fn size(&mut self, now: i64) -> usize {
        self.sweep(now);
        self.locks.len()
    }

    pub fn all(&mut self, now: i64) -> Vec<DavLock> {
        self.sweep(now);
        self.order
            .iter()
            .filter_map(|token| self.locks.get(token).cloned())
            .collect()
    }

    pub fn covering(&mut self, path: &str, now: i64) -> Vec<DavLock> {
        self.sweep(now);
        self.order
            .iter()
            .filter_map(|token| self.locks.get(token))
            .filter(|lock| {
                lock.path == path
                    || (lock.depth == LockDepth::Infinity && is_path_inside(path, &lock.path))
            })
            .cloned()
            .collect()
    }

    pub fn within(&mut self, path: &str, now: i64) -> Vec<DavLock> {
        self.sweep(now);
        self.order
            .iter()
            .filter_map(|token| self.locks.get(token))
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
        let injected_generator = self.options.new_token.is_some();
        if injected_generator && self.issued.len() >= MAX_LOCKS {
            return DavLockGrant::Full;
        }
        let timeout_seconds = self.granted_timeout(request.timeout);
        let mut token = self.options.new_token.as_ref().map_or_else(
            || format!("{LOCK_TOKEN_PREFIX}{}", Uuid::new_v4()),
            |new_token| new_token(),
        );
        if injected_generator
            && (token.len() > MAX_INJECTED_TOKEN_BYTES || !usable_lock_token_uri(&token))
        {
            return DavLockGrant::Full;
        }
        // A configured generator may repeat a token from an expired lock.
        // Retain its issued values; default UUIDs use random uniqueness.
        while self.locks.contains_key(&token)
            || (injected_generator && self.issued.contains(&token))
        {
            token = format!("{LOCK_TOKEN_PREFIX}{}", Uuid::new_v4());
        }
        let lock = DavLock {
            token: token.clone(),
            path: request.path,
            collection: request.collection,
            depth: request.depth,
            exclusive: request.exclusive,
            owner: request.owner,
            timeout_seconds,
            expires_at: lock_expiration(now, timeout_seconds),
        };
        if injected_generator {
            self.issued.insert(token.clone());
        }
        self.locks.insert(token.clone(), lock.clone());
        self.order.push(token);
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
            expires_at: lock_expiration(now, timeout_seconds),
            ..existing
        };
        self.locks.insert(token.to_owned(), refreshed.clone());
        Some(refreshed)
    }

    pub fn remove(&mut self, token: &str) -> bool {
        let removed = self.locks.remove(token).is_some();
        if removed {
            self.order.retain(|entry| entry != token);
        }
        removed
    }

    pub fn remaining(lock: &DavLock, now: i64) -> u64 {
        if lock.expires_at <= now {
            0
        } else {
            ((i128::from(lock.expires_at) - i128::from(now)) / 1000) as u64
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
        self.order.retain(|token| self.locks.contains_key(token));
    }
}

fn lock_expiration(now: i64, timeout_seconds: u64) -> i64 {
    // The public timeout options are unrestricted. Calculate in a wider type
    // so a large u64 cannot wrap into a negative i64 before saturation.
    let milliseconds = i128::from(timeout_seconds) * 1000;
    (i128::from(now) + milliseconds).min(i128::from(i64::MAX)) as i64
}

#[cfg(kani)]
mod verification {
    use super::*;

    #[kani::proof]
    fn timeout_and_remaining_are_wide_before_clamping() {
        let now: i64 = kani::any();
        let timeout_seconds: u64 = kani::any();
        kani::assume((-1_000..=1_000).contains(&now));
        let expires_at = lock_expiration(now, timeout_seconds);
        let precise = i128::from(now) + i128::from(timeout_seconds) * 1_000;
        let expected = i64::try_from(precise).unwrap_or(i64::MAX);
        assert_eq!(expires_at, expected);
        assert!(expires_at >= now);

        let lock = DavLock {
            token: "urn:uuid:bounded-time".to_owned(),
            path: "/bounded-time".to_owned(),
            collection: false,
            depth: LockDepth::Zero,
            exclusive: true,
            owner: None,
            timeout_seconds,
            expires_at,
        };
        let remaining = DavLockTable::remaining(&lock, now);
        let expected_remaining = ((i128::from(expires_at) - i128::from(now)) / 1_000) as u64;
        assert_eq!(remaining, expected_remaining);
        kani::cover!(timeout_seconds == u64::MAX && expires_at == i64::MAX);
        kani::cover!(timeout_seconds == 0 && expires_at == now);
        kani::cover!(timeout_seconds == 1 && expires_at == now + 1_000);
    }
}

impl Default for DavLockTable {
    fn default() -> Self {
        Self::new(DavLockTableOptions::default())
    }
}
