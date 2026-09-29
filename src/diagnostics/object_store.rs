//! Fixed, opt-in observations of object-store resource ownership and HTTP work.
//!
//! No label contains an endpoint, key, bucket, partition, drive or provider.
//! Handles retain only this bank. Lifetime release is a Rust owner observation,
//! never an acknowledgement that sockets, peers or backing storage drained.
//! Durations are inclusive wall time and cannot be summed as CPU time.
//! Allocation fields count known wrapper construction sites, not allocator calls.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

pub const CLIENT_ROLES: usize = 6;
pub const HTTP_METHODS: usize = 6;
pub const STATUS_CLASSES: usize = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum ClientRole {
    PrimaryDataMixed,
    PrimaryProbeMixed,
    QualificationData,
    QualificationProbe,
    StandaloneData,
    StandaloneProbe,
}

impl ClientRole {
    pub const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum HttpMethod {
    Get,
    Head,
    Put,
    Delete,
    Post,
    Other,
}

impl HttpMethod {
    pub const fn index(self) -> usize {
        self as usize
    }
}

/// Classes are 1xx, 2xx, 3xx, 4xx, 5xx and other, in that order.
pub const fn status_index(status: u16) -> usize {
    match status {
        100..=199 => 0,
        200..=299 => 1,
        300..=399 => 2,
        400..=499 => 3,
        500..=599 => 4,
        _ => 5,
    }
}

macro_rules! fixed_row {
    ($snapshot:ident, $atomic:ident, $($field:ident),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
        pub struct $snapshot { $(pub $field: u64,)+ }
        #[derive(Default)]
        struct $atomic { $($field: AtomicU64,)+ }
        impl $atomic {
            fn snapshot(&self) -> $snapshot {
                $snapshot { $($field: self.$field.load(Ordering::SeqCst),)+ }
            }
        }
    }
}

fixed_row!(
    HttpCounters,
    AtomicHttpCounters,
    attempts_started,
    attempts_inflight,
    header_responses,
    transport_errors,
    cancelled_before_headers,
    offered_bytes,
    offered_known,
    offered_unknown,
    known_extra_future_boxes,
    known_extra_response_body_boxes,
    bodies_started,
    bodies_inflight,
    body_eof,
    body_errors,
    body_dropped,
    body_bytes,
    body_chunks,
    dispatch_elapsed_ns,
    dispatch_max_ns,
    body_elapsed_ns,
    body_max_ns
);

/// Fixed HTTP row, with a bounded numeric status class array.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct HttpSnapshot {
    pub attempts_started: u64,
    pub attempts_inflight: u64,
    pub header_responses: u64,
    pub transport_errors: u64,
    pub cancelled_before_headers: u64,
    pub offered_bytes: u64,
    pub offered_known: u64,
    pub offered_unknown: u64,
    pub known_extra_future_boxes: u64,
    pub known_extra_response_body_boxes: u64,
    pub bodies_started: u64,
    pub bodies_inflight: u64,
    pub body_eof: u64,
    pub body_errors: u64,
    pub body_dropped: u64,
    pub body_bytes: u64,
    pub body_chunks: u64,
    pub dispatch_elapsed_ns: u64,
    pub dispatch_max_ns: u64,
    pub body_elapsed_ns: u64,
    pub body_max_ns: u64,
    pub status: [u64; STATUS_CLASSES],
}

#[derive(Default)]
struct AtomicHttp {
    counters: AtomicHttpCounters,
    status: [AtomicU64; STATUS_CLASSES],
}

impl AtomicHttp {
    fn snapshot(&self) -> HttpSnapshot {
        let row = self.counters.snapshot();
        HttpSnapshot {
            attempts_started: row.attempts_started,
            attempts_inflight: row.attempts_inflight,
            header_responses: row.header_responses,
            transport_errors: row.transport_errors,
            cancelled_before_headers: row.cancelled_before_headers,
            offered_bytes: row.offered_bytes,
            offered_known: row.offered_known,
            offered_unknown: row.offered_unknown,
            known_extra_future_boxes: row.known_extra_future_boxes,
            known_extra_response_body_boxes: row.known_extra_response_body_boxes,
            bodies_started: row.bodies_started,
            bodies_inflight: row.bodies_inflight,
            body_eof: row.body_eof,
            body_errors: row.body_errors,
            body_dropped: row.body_dropped,
            body_bytes: row.body_bytes,
            body_chunks: row.body_chunks,
            dispatch_elapsed_ns: row.dispatch_elapsed_ns,
            dispatch_max_ns: row.dispatch_max_ns,
            body_elapsed_ns: row.body_elapsed_ns,
            body_max_ns: row.body_max_ns,
            status: std::array::from_fn(|index| self.status[index].load(Ordering::SeqCst)),
        }
    }
}

fixed_row!(
    ClientBuildSnapshot,
    AtomicClientBuild,
    started,
    inflight,
    succeeded,
    failed,
    abandoned,
    elapsed_ns,
    max_ns
);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ClientSnapshot {
    pub constructed: u64,
    pub released: u64,
    pub live: u64,
    pub build: ClientBuildSnapshot,
    pub http: [HttpSnapshot; HTTP_METHODS],
}

#[derive(Default)]
struct AtomicClient {
    constructed: AtomicU64,
    released: AtomicU64,
    live: AtomicU64,
    build: AtomicClientBuild,
    http: [AtomicHttp; HTTP_METHODS],
}

impl AtomicClient {
    fn snapshot(&self) -> ClientSnapshot {
        ClientSnapshot {
            constructed: self.constructed.load(Ordering::SeqCst),
            released: self.released.load(Ordering::SeqCst),
            live: self.live.load(Ordering::SeqCst),
            build: self.build.snapshot(),
            http: std::array::from_fn(|index| self.http[index].snapshot()),
        }
    }
}

fixed_row!(
    BundleSnapshot,
    AtomicBundle,
    builds_started,
    builds_inflight,
    committed,
    failed,
    abandoned,
    live,
    released,
    elapsed_ns,
    max_ns
);
fixed_row!(
    CacheSnapshot,
    AtomicCache,
    created,
    released,
    live,
    resident_entries,
    payload_bytes,
    unknown_live
);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
/// Additive observation schema 1, scoped to this process. Cache rows include
/// all generic object-store block adapters (including qualification adapters).
/// Client rows follow the closed mixed data/probe roles, not provider identity.
/// Snapshot uses serial atomic loads; it is not transactional or a drain proof.
/// Export serialization is outside any warmed update allocation claim.
pub struct Snapshot {
    pub clients: [ClientSnapshot; CLIENT_ROLES],
    pub bundles: BundleSnapshot,
    /// Known occupancy only. Any unknown_live cache makes totals incomplete.
    pub cache: CacheSnapshot,
    /// Sticky overflow, underflow or unrepresentable value observation.
    pub saturated: bool,
    /// Observed concurrent update; false is not a transactional snapshot proof.
    pub concurrent_activity: bool,
}

#[derive(Default)]
struct Bank {
    clients: [AtomicClient; CLIENT_ROLES],
    bundles: AtomicBundle,
    cache: AtomicCache,
    saturated: AtomicBool,
    writers: AtomicU64,
    revision: AtomicU64,
}

impl Bank {
    fn add(&self, counter: &AtomicU64, amount: u64) {
        let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
            if old.checked_add(amount).is_none() {
                self.saturated.store(true, Ordering::SeqCst);
            }
            Some(old.saturating_add(amount))
        });
    }
    fn sub(&self, counter: &AtomicU64, amount: u64) {
        let _ = counter.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| {
            if old < amount {
                self.saturated.store(true, Ordering::SeqCst);
            }
            Some(old.saturating_sub(amount))
        });
    }
    fn elapsed(&self, start: Option<Instant>, total: &AtomicU64, max: &AtomicU64) {
        if let Some(start) = start {
            let nanos = start.elapsed().as_nanos();
            let value = u64::try_from(nanos).unwrap_or_else(|_| {
                self.saturated.store(true, Ordering::SeqCst);
                u64::MAX
            });
            self.add(total, value);
            max.fetch_max(value, Ordering::SeqCst);
        }
    }
    fn begin_update(&self) -> Update<'_> {
        self.add(&self.writers, 1);
        self.add(&self.revision, 1);
        Update(self)
    }
    fn snapshot(&self) -> Snapshot {
        let revision_before = self.revision.load(Ordering::SeqCst);
        let writers_before = self.writers.load(Ordering::SeqCst);
        let clients = std::array::from_fn(|index| self.clients[index].snapshot());
        let bundles = self.bundles.snapshot();
        let cache = self.cache.snapshot();
        let writers_after = self.writers.load(Ordering::SeqCst);
        let revision_after = self.revision.load(Ordering::SeqCst);
        Snapshot {
            clients,
            bundles,
            cache,
            saturated: self.saturated.load(Ordering::SeqCst),
            concurrent_activity: self.saturated.load(Ordering::SeqCst)
                || writers_before != 0
                || writers_after != 0
                || revision_before != revision_after,
        }
    }
}

struct Update<'a>(&'a Bank);
impl Drop for Update<'_> {
    fn drop(&mut self) {
        self.0.add(&self.0.revision, 1);
        self.0.sub(&self.0.writers, 1);
    }
}

static GLOBAL: OnceLock<Bank> = OnceLock::new();

#[derive(Clone)]
enum BankHandle {
    Global(&'static Bank),
    Isolated(Arc<Bank>),
}

/// A cloneable bank-only observer; disabled contains no bank or allocation.
#[derive(Clone, Default)]
pub struct Observer {
    bank: Option<BankHandle>,
}

impl fmt::Debug for Observer {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("ObjectStoreObserver")
            .field("enabled", &self.is_enabled())
            .finish()
    }
}

impl Observer {
    pub fn enabled() -> Self {
        if super::storage::enabled() {
            Self {
                bank: Some(BankHandle::Global(GLOBAL.get_or_init(Bank::default))),
            }
        } else {
            Self::disabled()
        }
    }
    pub const fn disabled() -> Self {
        Self { bank: None }
    }
    /// Explicit isolated fixed bank for controls, without reading process flags.
    pub fn isolated() -> Self {
        Self {
            bank: Some(BankHandle::Isolated(Arc::new(Bank::default()))),
        }
    }
    pub fn is_enabled(&self) -> bool {
        self.bank.is_some()
    }
    fn bank(&self) -> Option<&Bank> {
        match self.bank.as_ref()? {
            BankHandle::Global(bank) => Some(bank),
            BankHandle::Isolated(bank) => Some(bank),
        }
    }
    fn update(&self, record: impl FnOnce(&Bank)) {
        if let Some(bank) = self.bank() {
            let _update = bank.begin_update();
            record(bank);
        }
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.bank().map(Bank::snapshot)
    }
    /// Bracket the actual synchronous HTTP connector construction only.
    pub fn client_build(&self, role: ClientRole) -> ClientBuildSpan {
        ClientBuildSpan::new(self, role)
    }
    /// Call only after successful actual HTTP service construction.
    pub fn client(&self, role: ClientRole) -> HttpClientGuard {
        self.update(|bank| {
            let row = &bank.clients[role.index()];
            bank.add(&row.constructed, 1);
            bank.add(&row.live, 1);
        });
        HttpClientGuard {
            observer: self.clone(),
            role,
        }
    }
    pub fn bundle_build(&self) -> BundleBuildSpan {
        BundleBuildSpan::new(self)
    }
    pub fn cache_residency(&self) -> CacheResidencyGuard {
        CacheResidencyGuard::new(self)
    }
    /// Increment at wrapper future Box construction, even when never polled.
    pub fn known_extra_future_box(&self, role: ClientRole, method: HttpMethod) {
        self.update(|bank| {
            bank.add(
                &bank.clients[role.index()].http[method.index()]
                    .counters
                    .known_extra_future_boxes,
                1,
            )
        });
    }
    /// Increment at the actual response body Box wrapper construction site.
    pub fn known_extra_response_body_box(&self, role: ClientRole, method: HttpMethod) {
        self.update(|bank| {
            bank.add(
                &bank.clients[role.index()].http[method.index()]
                    .counters
                    .known_extra_response_body_boxes,
                1,
            )
        });
    }
}

#[derive(Debug)]
#[must_use = "retain until actual connector construction succeeds or fails"]
pub struct ClientBuildSpan {
    observer: Observer,
    role: ClientRole,
    started: Option<Instant>,
    finished: bool,
}
enum ClientBuildOutcome {
    Succeeded,
    Failed,
    Abandoned,
}
impl ClientBuildSpan {
    fn new(observer: &Observer, role: ClientRole) -> Self {
        Self::new_with_clock(observer, role, Instant::now)
    }
    fn new_with_clock(
        observer: &Observer,
        role: ClientRole,
        clock: impl FnOnce() -> Instant,
    ) -> Self {
        let started = observer.is_enabled().then(clock);
        observer.update(|bank| {
            let row = &bank.clients[role.index()].build;
            bank.add(&row.started, 1);
            bank.add(&row.inflight, 1);
        });
        Self {
            observer: observer.clone(),
            role,
            started,
            finished: false,
        }
    }
    pub fn finish_success(mut self) {
        self.finished = true;
        self.finish(ClientBuildOutcome::Succeeded);
    }
    pub fn finish_error(mut self) {
        self.finished = true;
        self.finish(ClientBuildOutcome::Failed);
    }
    fn finish(&self, outcome: ClientBuildOutcome) {
        self.observer.update(|bank| {
            let row = &bank.clients[self.role.index()].build;
            let counter = match outcome {
                ClientBuildOutcome::Succeeded => &row.succeeded,
                ClientBuildOutcome::Failed => &row.failed,
                ClientBuildOutcome::Abandoned => &row.abandoned,
            };
            bank.add(counter, 1);
            bank.sub(&row.inflight, 1);
            bank.elapsed(self.started, &row.elapsed_ns, &row.max_ns);
        });
    }
}
impl Drop for ClientBuildSpan {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            self.finish(ClientBuildOutcome::Abandoned);
        }
    }
}

#[derive(Debug)]
pub struct HttpClientGuard {
    observer: Observer,
    role: ClientRole,
}
impl HttpClientGuard {
    /// Call within the returned future, once its actual dispatch is first polled.
    pub fn attempt(&self, method: HttpMethod, offered: Option<u64>) -> HttpAttemptGuard {
        self.attempt_with_clock(method, offered, Instant::now)
    }
    fn attempt_with_clock(
        &self,
        method: HttpMethod,
        offered: Option<u64>,
        clock: impl FnOnce() -> Instant,
    ) -> HttpAttemptGuard {
        let started = self.observer.is_enabled().then(clock);
        self.observer.update(|bank| {
            let row = &bank.clients[self.role.index()].http[method.index()].counters;
            bank.add(&row.attempts_started, 1);
            bank.add(&row.attempts_inflight, 1);
            if let Some(bytes) = offered {
                bank.add(&row.offered_known, 1);
                bank.add(&row.offered_bytes, bytes);
            } else {
                bank.add(&row.offered_unknown, 1);
            }
        });
        HttpAttemptGuard {
            observer: self.observer.clone(),
            role: self.role,
            method,
            started,
            finished: false,
        }
    }
}
impl Drop for HttpClientGuard {
    fn drop(&mut self) {
        self.observer.update(|bank| {
            let row = &bank.clients[self.role.index()];
            bank.add(&row.released, 1);
            bank.sub(&row.live, 1);
        });
    }
}

#[derive(Debug)]
pub struct HttpAttemptGuard {
    observer: Observer,
    role: ClientRole,
    method: HttpMethod,
    started: Option<Instant>,
    finished: bool,
}
impl HttpAttemptGuard {
    pub fn headers(self, status: u16) -> HttpBodyGuard {
        self.headers_with_clock(status, Instant::now)
    }
    fn headers_with_clock(mut self, status: u16, clock: impl FnOnce() -> Instant) -> HttpBodyGuard {
        self.finished = true;
        let started = self.observer.is_enabled().then(clock);
        self.observer.update(|bank| {
            let http = &bank.clients[self.role.index()].http[self.method.index()];
            let row = &http.counters;
            bank.add(&row.header_responses, 1);
            bank.add(&http.status[status_index(status)], 1);
            bank.sub(&row.attempts_inflight, 1);
            bank.add(&row.bodies_started, 1);
            bank.add(&row.bodies_inflight, 1);
            bank.elapsed(self.started, &row.dispatch_elapsed_ns, &row.dispatch_max_ns);
        });
        HttpBodyGuard {
            observer: self.observer.clone(),
            role: self.role,
            method: self.method,
            started,
            finished: false,
        }
    }
    pub fn transport_error(mut self) {
        self.finished = true;
        self.finish(false);
    }
    fn finish(&self, cancelled: bool) {
        self.observer.update(|bank| {
            let row = &bank.clients[self.role.index()].http[self.method.index()].counters;
            bank.add(
                if cancelled {
                    &row.cancelled_before_headers
                } else {
                    &row.transport_errors
                },
                1,
            );
            bank.sub(&row.attempts_inflight, 1);
            bank.elapsed(self.started, &row.dispatch_elapsed_ns, &row.dispatch_max_ns);
        });
    }
}
impl Drop for HttpAttemptGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(true);
        }
    }
}

#[derive(Debug)]
pub struct HttpBodyGuard {
    observer: Observer,
    role: ClientRole,
    method: HttpMethod,
    started: Option<Instant>,
    finished: bool,
}
impl HttpBodyGuard {
    /// Bytes/chunks yielded by this HTTP body, before adapter validation/retries.
    pub fn data(&mut self, bytes: u64) {
        self.observer.update(|bank| {
            let row = &bank.clients[self.role.index()].http[self.method.index()].counters;
            bank.add(&row.body_bytes, bytes);
            bank.add(&row.body_chunks, 1);
        });
    }
    pub fn eof(mut self) {
        self.finished = true;
        self.finish(0);
    }
    pub fn error(mut self) {
        self.finished = true;
        self.finish(1);
    }
    fn finish(&self, outcome: u8) {
        self.observer.update(|bank| {
            let row = &bank.clients[self.role.index()].http[self.method.index()].counters;
            let count = match outcome {
                0 => &row.body_eof,
                1 => &row.body_errors,
                _ => &row.body_dropped,
            };
            bank.add(count, 1);
            bank.sub(&row.bodies_inflight, 1);
            bank.elapsed(self.started, &row.body_elapsed_ns, &row.body_max_ns);
        });
    }
}
impl Drop for HttpBodyGuard {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(2);
        }
    }
}

#[derive(Debug)]
pub struct BundleBuildSpan {
    observer: Observer,
    started: Option<Instant>,
    finished: bool,
}
impl BundleBuildSpan {
    pub fn new(observer: &Observer) -> Self {
        Self::new_with_clock(observer, Instant::now)
    }
    fn new_with_clock(observer: &Observer, clock: impl FnOnce() -> Instant) -> Self {
        let started = observer.is_enabled().then(clock);
        observer.update(|bank| {
            bank.add(&bank.bundles.builds_started, 1);
            bank.add(&bank.bundles.builds_inflight, 1);
        });
        Self {
            observer: observer.clone(),
            started,
            finished: false,
        }
    }
    /// Commit only after all clients and the provider facade construct successfully.
    pub fn finish_success(mut self) -> Option<Arc<BundleResidency>> {
        self.finished = true;
        if !self.observer.is_enabled() {
            return None;
        }
        // Allocate before recording success, so an interrupted allocation cannot
        // publish an unowned live contribution.
        let owner = Arc::new(BundleResidency {
            observer: self.observer.clone(),
        });
        self.observer.update(|bank| {
            bank.add(&bank.bundles.committed, 1);
            bank.add(&bank.bundles.live, 1);
            bank.sub(&bank.bundles.builds_inflight, 1);
            bank.elapsed(self.started, &bank.bundles.elapsed_ns, &bank.bundles.max_ns);
        });
        Some(owner)
    }
    pub fn finish_error(mut self) {
        self.finished = true;
        self.finish(false);
    }
    fn finish(&self, abandoned: bool) {
        self.observer.update(|bank| {
            bank.add(
                if abandoned {
                    &bank.bundles.abandoned
                } else {
                    &bank.bundles.failed
                },
                1,
            );
            bank.sub(&bank.bundles.builds_inflight, 1);
            bank.elapsed(self.started, &bank.bundles.elapsed_ns, &bank.bundles.max_ns);
        });
    }
}
impl Drop for BundleBuildSpan {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(true);
        }
    }
}

/// One enabled token shared across actual bundle clones. Final Drop is release.
#[derive(Debug)]
pub struct BundleResidency {
    observer: Observer,
}
impl Drop for BundleResidency {
    fn drop(&mut self) {
        self.observer.update(|bank| {
            bank.add(&bank.bundles.released, 1);
            bank.sub(&bank.bundles.live, 1);
        });
    }
}

#[derive(Debug)]
struct CacheObservation {
    observer: Observer,
    entries: AtomicU64,
    bytes: AtomicU64,
    unknown: AtomicBool,
}

/// Install last in the actual cache owner, after its state mutex. Publish and
/// mark_unknown must be serialized by that owner's existing state lock.
#[derive(Debug)]
pub struct CacheResidencyGuard {
    observation: Option<CacheObservation>,
}
impl CacheResidencyGuard {
    fn new(observer: &Observer) -> Self {
        if !observer.is_enabled() {
            return Self { observation: None };
        }
        observer.update(|bank| {
            bank.add(&bank.cache.created, 1);
            bank.add(&bank.cache.live, 1);
        });
        Self {
            observation: Some(CacheObservation {
                observer: observer.clone(),
                entries: AtomicU64::new(0),
                bytes: AtomicU64::new(0),
                unknown: AtomicBool::new(false),
            }),
        }
    }
    /// Values describe payload lengths and entries, not Vec capacity or RSS.
    pub fn publish(&self, entries: usize, payload_bytes: usize) {
        let Some(observation) = &self.observation else {
            return;
        };
        if observation.unknown.load(Ordering::SeqCst) {
            return;
        }
        observation.observer.update(|bank| {
            let entries = u64::try_from(entries).unwrap_or_else(|_| {
                bank.saturated.store(true, Ordering::SeqCst);
                u64::MAX
            });
            let bytes = u64::try_from(payload_bytes).unwrap_or_else(|_| {
                bank.saturated.store(true, Ordering::SeqCst);
                u64::MAX
            });
            let previous_entries = observation.entries.swap(entries, Ordering::SeqCst);
            let previous_bytes = observation.bytes.swap(bytes, Ordering::SeqCst);
            if entries >= previous_entries {
                bank.add(&bank.cache.resident_entries, entries - previous_entries);
            } else {
                bank.sub(&bank.cache.resident_entries, previous_entries - entries);
            }
            if bytes >= previous_bytes {
                bank.add(&bank.cache.payload_bytes, bytes - previous_bytes);
            } else {
                bank.sub(&bank.cache.payload_bytes, previous_bytes - bytes);
            }
        });
    }
    /// Poisoned state removes its contribution from known occupancy. The cache
    /// remains live and explicitly unknown; this never repairs provider state.
    pub fn mark_unknown(&self) {
        let Some(observation) = &self.observation else {
            return;
        };
        if observation.unknown.swap(true, Ordering::SeqCst) {
            return;
        }
        observation.observer.update(|bank| {
            bank.sub(
                &bank.cache.resident_entries,
                observation.entries.swap(0, Ordering::SeqCst),
            );
            bank.sub(
                &bank.cache.payload_bytes,
                observation.bytes.swap(0, Ordering::SeqCst),
            );
            bank.add(&bank.cache.unknown_live, 1);
        });
    }
}
impl Drop for CacheResidencyGuard {
    fn drop(&mut self) {
        if let Some(observation) = &self.observation {
            observation.observer.update(|bank| {
                bank.sub(
                    &bank.cache.resident_entries,
                    observation.entries.load(Ordering::SeqCst),
                );
                bank.sub(
                    &bank.cache.payload_bytes,
                    observation.bytes.load(Ordering::SeqCst),
                );
                if observation.unknown.load(Ordering::SeqCst) {
                    bank.sub(&bank.cache.unknown_live, 1);
                }
                bank.sub(&bank.cache.live, 1);
                bank.add(&bank.cache.released, 1);
            });
        }
    }
}

#[cfg(test)]
#[path = "object_store_tests.rs"]
mod tests;
