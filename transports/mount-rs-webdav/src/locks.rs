//! WebDAV write-lock state (RFC 4918 sections 6, 7, and 9.10).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use mount_rs_core::path::normalize_path;
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

/// Component containment for already normalized absolute paths. Keeping this
/// decision free of allocation lets a bounded proof exercise the same path
/// boundary used by lock scope, listing, and conflict checks.
fn canonical_path_inside(path: &[u8], parent: &[u8]) -> bool {
    path == parent
        || (parent == b"/" && path.first() == Some(&b'/'))
        || (path.starts_with(parent) && path.get(parent.len()) == Some(&b'/'))
}

fn canonical_lock_covers(lock_path: &[u8], depth: LockDepth, path: &[u8]) -> bool {
    lock_path == path || (depth == LockDepth::Infinity && canonical_path_inside(path, lock_path))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConflictPhase {
    Covering,
    Within,
}

/// One active lock's conflict decision for canonical paths. `conflict` scans
/// covering candidates first, then descendants, so it retains the existing
/// reported-lock priority while using this decision in both phases.
#[allow(clippy::too_many_arguments)]
fn canonical_conflict_candidate(
    lock_path: &[u8],
    lock_depth: LockDepth,
    lock_exclusive: bool,
    expires_at: i64,
    request_path: &[u8],
    request_depth: LockDepth,
    request_exclusive: bool,
    now: i64,
    phase: ConflictPhase,
) -> bool {
    expires_at > now
        && (lock_exclusive || request_exclusive)
        && match phase {
            ConflictPhase::Covering => canonical_lock_covers(lock_path, lock_depth, request_path),
            ConflictPhase::Within => {
                request_depth == LockDepth::Infinity
                    && canonical_path_inside(lock_path, request_path)
            }
        }
}

fn normalized_path_inside(path: &str, parent: &str) -> bool {
    let path = normalize_path(path);
    let parent = normalize_path(parent);
    canonical_path_inside(path.as_bytes(), parent.as_bytes())
}

fn lock_covers_path(lock_path: &str, depth: LockDepth, path: &str) -> bool {
    // Public lock-table callers can supply noncanonical paths. Depth zero
    // historically compares the original spelling; infinity normalizes.
    lock_path == path || (depth == LockDepth::Infinity && normalized_path_inside(path, lock_path))
}

fn conflict_candidate(
    lock: &DavLock,
    path: &str,
    depth: LockDepth,
    exclusive: bool,
    now: i64,
    phase: ConflictPhase,
) -> bool {
    if lock.expires_at <= now || !(lock.exclusive || exclusive) {
        return false;
    }
    match phase {
        ConflictPhase::Covering if lock.path == path => return true,
        ConflictPhase::Covering if lock.depth == LockDepth::Zero => return false,
        ConflictPhase::Within if depth == LockDepth::Zero => return false,
        _ => {}
    }
    let lock_path = normalize_path(&lock.path);
    let request_path = normalize_path(path);
    canonical_conflict_candidate(
        lock_path.as_bytes(),
        lock.depth,
        lock.exclusive,
        lock.expires_at,
        request_path.as_bytes(),
        depth,
        exclusive,
        now,
        phase,
    )
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
            .filter(|lock| lock_covers_path(&lock.path, lock.depth, path))
            .cloned()
            .collect()
    }

    pub fn within(&mut self, path: &str, now: i64) -> Vec<DavLock> {
        self.sweep(now);
        self.order
            .iter()
            .filter_map(|token| self.locks.get(token))
            .filter(|lock| normalized_path_inside(&lock.path, path))
            .cloned()
            .collect()
    }

    pub fn find(&mut self, token: &str, now: i64) -> Option<DavLock> {
        self.sweep(now);
        self.locks.get(token).cloned()
    }

    pub fn in_scope(lock: &DavLock, path: &str) -> bool {
        lock_covers_path(&lock.path, lock.depth, path)
    }

    pub fn conflict(
        &mut self,
        path: &str,
        depth: LockDepth,
        exclusive: bool,
        now: i64,
    ) -> Option<DavLock> {
        self.sweep(now);
        let covering = self
            .order
            .iter()
            .filter_map(|token| self.locks.get(token))
            .find(|lock| {
                conflict_candidate(lock, path, depth, exclusive, now, ConflictPhase::Covering)
            })
            .cloned();
        covering.or_else(|| {
            (depth == LockDepth::Infinity).then(|| {
                self.order
                    .iter()
                    .filter_map(|token| self.locks.get(token))
                    .find(|lock| {
                        conflict_candidate(lock, path, depth, exclusive, now, ConflictPhase::Within)
                    })
                    .cloned()
            })?
        })
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

    const PATHS: [&[u8]; 4] = [b"/", b"/a", b"/a/x", b"/ab"];
    // Rows are parents and columns are candidate members. This explicit
    // hierarchy is independent of the byte-prefix implementation above.
    const INSIDE: [[bool; 4]; 4] = [
        [true, true, true, true],
        [false, true, true, false],
        [false, false, true, false],
        [false, false, false, true],
    ];

    fn oracle_covers(lock: usize, depth: LockDepth, target: usize) -> bool {
        lock == target || (depth == LockDepth::Infinity && INSIDE[lock][target])
    }

    fn oracle_overlaps(
        lock: usize,
        lock_depth: LockDepth,
        request: usize,
        request_depth: LockDepth,
    ) -> bool {
        (0..PATHS.len()).any(|target| {
            oracle_covers(lock, lock_depth, target) && oracle_covers(request, request_depth, target)
        })
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn canonical_scope_and_two_lock_conflicts() {
        let first: u8 = kani::any();
        let second: u8 = kani::any();
        let request: u8 = kani::any();
        let first_expiry: u8 = kani::any();
        let second_expiry: u8 = kani::any();
        let now: u8 = kani::any();
        kani::assume(first < 4 && second < 4 && request < 4);
        kani::assume(first_expiry <= 2 && second_expiry <= 2 && now <= 2);
        let first = first as usize;
        let second = second as usize;
        let request = request as usize;
        let first_depth = if kani::any() {
            LockDepth::Infinity
        } else {
            LockDepth::Zero
        };
        let second_depth = if kani::any() {
            LockDepth::Infinity
        } else {
            LockDepth::Zero
        };
        let request_depth = if kani::any() {
            LockDepth::Infinity
        } else {
            LockDepth::Zero
        };
        let first_exclusive: bool = kani::any();
        let second_exclusive: bool = kani::any();
        let request_exclusive: bool = kani::any();
        let now = i64::from(now);

        assert_eq!(
            canonical_path_inside(PATHS[request], PATHS[first]),
            INSIDE[first][request]
        );
        assert_eq!(
            canonical_path_inside(PATHS[first], PATHS[request]),
            INSIDE[request][first]
        );
        assert_eq!(
            canonical_lock_covers(PATHS[first], first_depth, PATHS[request]),
            oracle_covers(first, first_depth, request)
        );
        assert_eq!(
            canonical_lock_covers(PATHS[second], second_depth, PATHS[request]),
            oracle_covers(second, second_depth, request)
        );

        let first_covering = canonical_conflict_candidate(
            PATHS[first],
            first_depth,
            first_exclusive,
            i64::from(first_expiry),
            PATHS[request],
            request_depth,
            request_exclusive,
            now,
            ConflictPhase::Covering,
        );
        let first_within = canonical_conflict_candidate(
            PATHS[first],
            first_depth,
            first_exclusive,
            i64::from(first_expiry),
            PATHS[request],
            request_depth,
            request_exclusive,
            now,
            ConflictPhase::Within,
        );
        let second_covering = canonical_conflict_candidate(
            PATHS[second],
            second_depth,
            second_exclusive,
            i64::from(second_expiry),
            PATHS[request],
            request_depth,
            request_exclusive,
            now,
            ConflictPhase::Covering,
        );
        let second_within = canonical_conflict_candidate(
            PATHS[second],
            second_depth,
            second_exclusive,
            i64::from(second_expiry),
            PATHS[request],
            request_depth,
            request_exclusive,
            now,
            ConflictPhase::Within,
        );

        let first_active = i64::from(first_expiry) > now;
        let second_active = i64::from(second_expiry) > now;
        assert_eq!(
            first_covering,
            first_active
                && (first_exclusive || request_exclusive)
                && oracle_covers(first, first_depth, request)
        );
        assert_eq!(
            first_within,
            first_active
                && (first_exclusive || request_exclusive)
                && request_depth == LockDepth::Infinity
                && INSIDE[request][first]
        );
        assert_eq!(
            second_covering,
            second_active
                && (second_exclusive || request_exclusive)
                && oracle_covers(second, second_depth, request)
        );
        assert_eq!(
            second_within,
            second_active
                && (second_exclusive || request_exclusive)
                && request_depth == LockDepth::Infinity
                && INSIDE[request][second]
        );
        let first_conflicts = first_covering || first_within;
        let second_conflicts = second_covering || second_within;
        let expected_first = first_active
            && (first_exclusive || request_exclusive)
            && oracle_overlaps(first, first_depth, request, request_depth);
        let expected_second = second_active
            && (second_exclusive || request_exclusive)
            && oracle_overlaps(second, second_depth, request, request_depth);
        assert_eq!(first_conflicts, expected_first);
        assert_eq!(second_conflicts, expected_second);
        assert_eq!(
            first_conflicts || second_conflicts,
            expected_first || expected_second
        );

        kani::cover!(first == 1 && request == 3 && !INSIDE[first][request]);
        kani::cover!(first == 1 && request == 2 && INSIDE[first][request]);
        kani::cover!(first == 2 && request == 1 && first_within);
        kani::cover!(first_active && !second_active && first_conflicts);
        kani::cover!(
            !first_exclusive
                && !second_exclusive
                && !request_exclusive
                && !first_conflicts
                && !second_conflicts
        );
    }

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
