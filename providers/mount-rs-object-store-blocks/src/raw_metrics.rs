//! Per-instance adapter API observations. Updates allocate no heap memory.
//! Enabled construction boxes the fixed bank once; snapshots are fixed arrays.
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

pub const RAW_API_SCHEMA: &str = "mount-rs.object-store-api.v1";
pub const RAW_API_SCOPE: &str = "one_object_store_block_store_instance";
pub const LOCAL_WORK_SCHEMA: &str = "mount-rs.object-store-local.v1";
pub const LOCAL_WORK_NAMES: [&str; 7] = [
    "sha256.digest",
    "block_id.encode",
    "copy.upload_payload",
    "copy.cache_insert",
    "copy.return_vec",
    "cache.lock_acquire",
    "put.follower_wait",
];
#[derive(Clone, Copy)]
pub(crate) enum Local {
    Digest,
    Encode,
    UploadCopy,
    CacheCopy,
    ReturnCopy,
    CacheLock,
    FollowerWait,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalWorkEntry {
    pub name: &'static str,
    pub calls: u64,
    pub success: u64,
    pub error: u64,
    pub cancelled: u64,
    pub elapsed_ns: u64,
    pub latency_max_ns: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub latency_log2_us: [u64; 32],
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalWorkSnapshot {
    pub schema: &'static str,
    pub scope: &'static str,
    pub saturated: bool,
    pub in_flight: u64,
    pub entries: [LocalWorkEntry; 7],
}
#[derive(Default)]
struct LocalRow {
    calls: AtomicU64,
    success: AtomicU64,
    error: AtomicU64,
    cancelled: AtomicU64,
    elapsed_ns: AtomicU64,
    latency_max_ns: AtomicU64,
    input_bytes: AtomicU64,
    output_bytes: AtomicU64,
    latency_log2_us: [AtomicU64; 32],
}
pub(crate) struct LocalState {
    saturated: AtomicBool,
    in_flight: AtomicU64,
    rows: [LocalRow; 7],
}
impl Default for LocalState {
    fn default() -> Self {
        Self {
            saturated: AtomicBool::new(false),
            in_flight: AtomicU64::new(0),
            rows: std::array::from_fn(|_| LocalRow::default()),
        }
    }
}
impl LocalState {
    fn add(&self, counter: &AtomicU64, amount: u64) {
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            let next = old.saturating_add(amount);
            if next == u64::MAX {
                self.saturated.store(true, Ordering::Relaxed);
            }
            Some(next)
        });
    }
    fn remove(&self) {
        let _ = self
            .in_flight
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
                Some(old.checked_sub(1).unwrap_or_else(|| {
                    self.saturated.store(true, Ordering::Relaxed);
                    0
                }))
            });
    }
    pub(crate) fn snapshot(&self) -> LocalWorkSnapshot {
        LocalWorkSnapshot {
            schema: LOCAL_WORK_SCHEMA,
            scope: RAW_API_SCOPE,
            saturated: self.saturated.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            entries: std::array::from_fn(|index| {
                let row = &self.rows[index];
                LocalWorkEntry {
                    name: LOCAL_WORK_NAMES[index],
                    calls: row.calls.load(Ordering::Relaxed),
                    success: row.success.load(Ordering::Relaxed),
                    error: row.error.load(Ordering::Relaxed),
                    cancelled: row.cancelled.load(Ordering::Relaxed),
                    elapsed_ns: row.elapsed_ns.load(Ordering::Relaxed),
                    latency_max_ns: row.latency_max_ns.load(Ordering::Relaxed),
                    input_bytes: row.input_bytes.load(Ordering::Relaxed),
                    output_bytes: row.output_bytes.load(Ordering::Relaxed),
                    latency_log2_us: row
                        .latency_log2_us
                        .each_ref()
                        .map(|value| value.load(Ordering::Relaxed)),
                }
            }),
        }
    }
}
/// Stack-only local work span. The disabled path never evaluates its clock.
pub(crate) struct LocalSpan<'a> {
    state: Option<&'a LocalState>,
    operation: Local,
    started: Option<Instant>,
    finished: bool,
}
impl<'a> LocalSpan<'a> {
    pub(crate) fn new(state: Option<&'a LocalState>, operation: Local, input_bytes: u64) -> Self {
        Self::new_with_clock(state, operation, input_bytes, Instant::now)
    }
    pub(crate) fn new_with_clock(
        state: Option<&'a LocalState>,
        operation: Local,
        input_bytes: u64,
        clock: impl FnOnce() -> Instant,
    ) -> Self {
        if let Some(state) = state {
            let row = &state.rows[operation as usize];
            state.add(&row.calls, 1);
            state.add(&row.input_bytes, input_bytes);
            state.add(&state.in_flight, 1);
        }
        Self {
            state,
            operation,
            started: state.map(|_| clock()),
            finished: false,
        }
    }
    fn finish(&mut self, outcome: Outcome, output_bytes: u64) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let (Some(state), Some(started)) = (self.state, self.started) {
            let elapsed = started.elapsed().as_nanos();
            if elapsed >= u64::MAX as u128 {
                state.saturated.store(true, Ordering::Relaxed);
            }
            let elapsed = elapsed.min(u64::MAX as u128) as u64;
            let row = &state.rows[self.operation as usize];
            state.add(
                match outcome {
                    Outcome::Success => &row.success,
                    Outcome::Error => &row.error,
                    Outcome::Cancelled => &row.cancelled,
                },
                1,
            );
            state.add(&row.elapsed_ns, elapsed);
            row.latency_max_ns.fetch_max(elapsed, Ordering::Relaxed);
            state.add(&row.output_bytes, output_bytes);
            let micros = elapsed / 1000;
            let bucket = if micros == 0 {
                0
            } else {
                (64 - micros.leading_zeros() as usize).min(31)
            };
            state.add(&row.latency_log2_us[bucket], 1);
            state.remove();
        }
    }
    pub(crate) fn success(&mut self, output_bytes: u64) {
        self.finish(Outcome::Success, output_bytes);
    }
    pub(crate) fn result<T, E>(&mut self, result: &std::result::Result<T, E>, output_bytes: u64) {
        self.finish(
            if result.is_ok() {
                Outcome::Success
            } else {
                Outcome::Error
            },
            if result.is_ok() { output_bytes } else { 0 },
        );
    }
}
impl Drop for LocalSpan<'_> {
    fn drop(&mut self) {
        self.finish(Outcome::Cancelled, 0);
    }
}
pub const RAW_API_NAMES: [&str; 10] = [
    "put_opts.block_create",
    "get.block_read",
    "body_read.block_read",
    "get.conflict_verify",
    "body_read.conflict_verify",
    "get.migration",
    "body_read.migration",
    "head.direct_delete",
    "delete.direct",
    "delete.reconcile",
];
#[derive(Clone, Copy)]
pub(crate) enum Api {
    Put,
    Get,
    Body,
    ConflictGet,
    ConflictBody,
    MigrationGet,
    MigrationBody,
    Head,
    Delete,
    ReconcileDelete,
}
#[derive(Clone, Copy)]
pub(crate) enum Claim {
    Leader,
    Follower,
}
#[derive(Clone, Copy)]
enum Outcome {
    Success,
    Error,
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawApiEntry {
    pub name: &'static str,
    pub calls: u64,
    pub success: u64,
    pub error: u64,
    pub cancelled: u64,
    pub elapsed_ns: u64,
    pub latency_max_ns: u64,
    pub attempted_bytes: u64,
    pub confirmed_bytes: u64,
    pub returned_bytes: u64,
    pub latency_log2_us: [u64; 32],
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawApiClaims {
    pub leader_claims: u64,
    pub leader_success: u64,
    pub leader_error: u64,
    pub leader_cancelled: u64,
    pub follower_claims: u64,
    pub follower_success: u64,
    pub follower_error: u64,
    pub follower_cancelled: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawApiSnapshot {
    pub schema: &'static str,
    pub scope: &'static str,
    pub saturated: bool,
    pub in_flight: u64,
    pub pending_claims: u64,
    pub claims: RawApiClaims,
    pub entries: [RawApiEntry; 10],
}
#[derive(Default)]
struct Row {
    calls: AtomicU64,
    success: AtomicU64,
    error: AtomicU64,
    cancelled: AtomicU64,
    elapsed_ns: AtomicU64,
    latency_max_ns: AtomicU64,
    attempted_bytes: AtomicU64,
    confirmed_bytes: AtomicU64,
    returned_bytes: AtomicU64,
    latency_log2_us: [AtomicU64; 32],
}
pub(crate) struct RawState {
    pub(crate) local: LocalState,
    saturated: AtomicBool,
    in_flight: AtomicU64,
    pending_claims: AtomicU64,
    claims: [AtomicU64; 8],
    rows: [Row; 10],
}
impl Default for RawState {
    fn default() -> Self {
        Self {
            local: LocalState::default(),
            saturated: AtomicBool::new(false),
            in_flight: AtomicU64::new(0),
            pending_claims: AtomicU64::new(0),
            claims: std::array::from_fn(|_| AtomicU64::new(0)),
            rows: std::array::from_fn(|_| Row::default()),
        }
    }
}
impl RawState {
    fn add(&self, counter: &AtomicU64, amount: u64) {
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            let next = old.saturating_add(amount);
            if next == u64::MAX {
                self.saturated.store(true, Ordering::Relaxed);
            }
            Some(next)
        });
    }
    fn remove(&self, counter: &AtomicU64) {
        let _ = counter.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |old| {
            Some(old.checked_sub(1).unwrap_or_else(|| {
                self.saturated.store(true, Ordering::Relaxed);
                0
            }))
        });
    }
    pub(crate) fn snapshot(&self) -> RawApiSnapshot {
        let c = self.claims.each_ref().map(|x| x.load(Ordering::Relaxed));
        RawApiSnapshot {
            schema: RAW_API_SCHEMA,
            scope: RAW_API_SCOPE,
            saturated: self.saturated.load(Ordering::Relaxed),
            in_flight: self.in_flight.load(Ordering::Relaxed),
            pending_claims: self.pending_claims.load(Ordering::Relaxed),
            claims: RawApiClaims {
                leader_claims: c[0],
                leader_success: c[1],
                leader_error: c[2],
                leader_cancelled: c[3],
                follower_claims: c[4],
                follower_success: c[5],
                follower_error: c[6],
                follower_cancelled: c[7],
            },
            entries: std::array::from_fn(|index| {
                let row = &self.rows[index];
                RawApiEntry {
                    name: RAW_API_NAMES[index],
                    calls: row.calls.load(Ordering::Relaxed),
                    success: row.success.load(Ordering::Relaxed),
                    error: row.error.load(Ordering::Relaxed),
                    cancelled: row.cancelled.load(Ordering::Relaxed),
                    elapsed_ns: row.elapsed_ns.load(Ordering::Relaxed),
                    latency_max_ns: row.latency_max_ns.load(Ordering::Relaxed),
                    attempted_bytes: row.attempted_bytes.load(Ordering::Relaxed),
                    confirmed_bytes: row.confirmed_bytes.load(Ordering::Relaxed),
                    returned_bytes: row.returned_bytes.load(Ordering::Relaxed),
                    latency_log2_us: row
                        .latency_log2_us
                        .each_ref()
                        .map(|x| x.load(Ordering::Relaxed)),
                }
            }),
        }
    }
}
/// Stack-only span around an invoked adapter await, not argument preparation.
pub(crate) struct RawSpan<'a> {
    state: Option<&'a RawState>,
    api: Api,
    started: Option<Instant>,
    finished: bool,
}
impl<'a> RawSpan<'a> {
    pub(crate) fn new(state: Option<&'a RawState>, api: Api, attempted: u64) -> Self {
        if let Some(state) = state {
            state.add(&state.rows[api as usize].calls, 1);
            state.add(&state.rows[api as usize].attempted_bytes, attempted);
            state.add(&state.in_flight, 1);
        }
        Self {
            state,
            api,
            started: state.map(|_| Instant::now()),
            finished: false,
        }
    }
    fn finish(&mut self, outcome: Outcome, confirmed: u64, returned: u64) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let (Some(state), Some(started)) = (self.state, self.started) {
            let elapsed = started.elapsed().as_nanos();
            if elapsed >= u64::MAX as u128 {
                state.saturated.store(true, Ordering::Relaxed);
            }
            let elapsed = elapsed.min(u64::MAX as u128) as u64;
            let row = &state.rows[self.api as usize];
            let counter = match outcome {
                Outcome::Success => &row.success,
                Outcome::Error => &row.error,
                Outcome::Cancelled => &row.cancelled,
            };
            state.add(counter, 1);
            state.add(&row.elapsed_ns, elapsed);
            row.latency_max_ns.fetch_max(elapsed, Ordering::Relaxed);
            state.add(&row.confirmed_bytes, confirmed);
            state.add(&row.returned_bytes, returned);
            let micros = elapsed / 1000;
            let bucket = if micros == 0 {
                0
            } else {
                (64 - micros.leading_zeros() as usize).min(31)
            };
            state.add(&row.latency_log2_us[bucket], 1);
            // Quiescence is caller-owned, not a multi-atomic snapshot guarantee.
            state.remove(&state.in_flight);
        }
    }
    pub(crate) fn result<T>(
        &mut self,
        result: &object_store::Result<T>,
        confirmed: u64,
        returned: u64,
    ) {
        self.finish(
            if result.is_ok() {
                Outcome::Success
            } else {
                Outcome::Error
            },
            if result.is_ok() { confirmed } else { 0 },
            if result.is_ok() { returned } else { 0 },
        );
    }
}
impl Drop for RawSpan<'_> {
    fn drop(&mut self) {
        self.finish(Outcome::Cancelled, 0, 0);
    }
}
pub(crate) struct ClaimSpan<'a> {
    state: Option<&'a RawState>,
    base: usize,
    finished: bool,
}
impl<'a> ClaimSpan<'a> {
    pub(crate) fn new(state: Option<&'a RawState>, claim: Claim) -> Self {
        let base = match claim {
            Claim::Leader => 0,
            Claim::Follower => 4,
        };
        if let Some(state) = state {
            state.add(&state.claims[base], 1);
            state.add(&state.pending_claims, 1);
        }
        Self {
            state,
            base,
            finished: false,
        }
    }
    fn finish(&mut self, outcome: usize) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(state) = self.state {
            state.add(&state.claims[self.base + outcome], 1);
            state.remove(&state.pending_claims);
        }
    }
    pub(crate) fn result<T>(&mut self, result: &mount_rs_core::Result<T>) {
        self.finish(if result.is_ok() { 1 } else { 2 });
    }
}
impl Drop for ClaimSpan<'_> {
    fn drop(&mut self) {
        self.finish(3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_disabled_clock_has_an_enabled_positive_control() {
        let calls = std::cell::Cell::new(0);
        let clock = || {
            calls.set(calls.get() + 1);
            Instant::now()
        };
        let mut disabled = LocalSpan::new_with_clock(None, Local::Digest, 9, clock);
        disabled.success(32);
        drop(LocalSpan::new_with_clock(
            None,
            Local::FollowerWait,
            0,
            clock,
        ));
        assert_eq!(calls.get(), 0);
        let state = LocalState::default();
        let mut enabled = LocalSpan::new_with_clock(Some(&state), Local::Digest, 9, clock);
        assert_eq!(calls.get(), 1);
        assert_eq!(state.snapshot().in_flight, 1);
        enabled.success(32);
        enabled.result(&Err::<(), ()>(()), 0);
        drop(enabled);
        let snapshot = state.snapshot();
        assert_eq!(snapshot.in_flight, 0);
        assert_eq!(
            (
                snapshot.entries[0].calls,
                snapshot.entries[0].success,
                snapshot.entries[0].error,
                snapshot.entries[0].cancelled
            ),
            (1, 1, 0, 0)
        );
        assert_eq!(
            (
                snapshot.entries[0].input_bytes,
                snapshot.entries[0].output_bytes
            ),
            (9, 32)
        );
        assert_eq!(snapshot.entries[0].latency_log2_us.iter().sum::<u64>(), 1);
        eprintln!("LOCAL_CLOCK_CONTROL disabled_clock_calls=0 enabled_clock_calls=1");
    }
    #[test]
    fn local_saturation_does_not_change_raw_qualification_flags() {
        let state = RawState::default();
        state.local.rows[0]
            .input_bytes
            .store(u64::MAX - 1, Ordering::Relaxed);
        let mut span = LocalSpan::new(Some(&state.local), Local::Digest, 2);
        span.success(32);
        assert_eq!(state.local.snapshot().entries[0].input_bytes, u64::MAX);
        assert!(state.local.snapshot().saturated);
        assert!(!state.snapshot().saturated);
        assert_eq!(state.snapshot().in_flight, 0);
        assert_eq!(state.local.snapshot().in_flight, 0);
        state.local.remove();
        assert_eq!(state.local.snapshot().in_flight, 0);
        assert!(!state.snapshot().saturated);
    }
    #[test]
    fn saturation_is_sticky_and_never_wraps() {
        let state = RawState::default();
        state.rows[0].calls.store(u64::MAX - 2, Ordering::Relaxed);
        let mut span = RawSpan::new(Some(&state), Api::Put, 7);
        span.result(&Ok::<_, object_store::Error>(()), 7, 0);
        assert_eq!(state.snapshot().entries[0].calls, u64::MAX - 1);
        assert!(!state.snapshot().saturated);
        drop(RawSpan::new(Some(&state), Api::Put, 1));
        assert_eq!(state.snapshot().entries[0].calls, u64::MAX);
        assert!(state.snapshot().saturated);
        drop(RawSpan::new(Some(&state), Api::Put, 1));
        assert_eq!(state.snapshot().entries[0].calls, u64::MAX);
        assert_eq!(state.snapshot().in_flight, 0);
        state.remove(&state.in_flight);
        assert_eq!(state.snapshot().in_flight, 0);
        assert!(state.snapshot().saturated);
    }
}
