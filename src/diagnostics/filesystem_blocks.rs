//! Fixed, opt-in process-local observations of filesystem block write stages.
//!
//! Rows contain no paths, names, identities, payloads or provider resources.
//! Durations are inclusive wall time. Snapshots are not transactional and are
//! never evidence of caller acknowledgement, durable storage or worker drain.
//! Offered bytes are counted at start, including failed or abandoned work.
//! Serialization and observer construction are outside warmed update claims.
//! The filesystem provider uses OS writeback and leaves file_sync,
//! file_device_sync, shard_sync, root_sync and post_directory_device_sync
//! inactive. Their IDs remain reserved in the fixed stage schema.

use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

pub const OP_COUNT: usize = 2;
pub const STAGE_COUNT: usize = 14;
pub const PATH_COUNT: usize = 3;
pub const SCHEMA: &str = "mount-rs.filesystem-blocks-bank.v1";
pub const OPERATION_NAMES: [&str; OP_COUNT] = ["put", "flush"];
pub const STAGE_NAMES: [&str; STAGE_COUNT] = [
    "input_copy",
    "initial_authority",
    "content_id",
    "shard_open",
    "existing_verify",
    "stage_create_write",
    "file_sync",
    "file_device_sync",
    "before_publish_authority",
    "publish_name",
    "shard_sync",
    "root_sync",
    "post_directory_device_sync",
    "final_authority",
];
pub const PUT_PATH_NAMES: [&str; PATH_COUNT] = ["existing", "created", "race_existing"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Operation {
    Put,
    Flush,
}

/// PUT stages only. Flush authority checks are observed by its worker row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Stage {
    InputCopy,
    InitialAuthority,
    ContentId,
    ShardOpen,
    ExistingVerify,
    StageCreateWrite,
    FileSync,
    FileDeviceSync,
    BeforePublishAuthority,
    PublishName,
    ShardSync,
    RootSync,
    PostDirectoryDeviceSync,
    FinalAuthority,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Path {
    Existing,
    Created,
    RaceExisting,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct CounterSnapshot {
    pub started: u64,
    pub inflight: u64,
    pub succeeded: u64,
    pub failed: u64,
    pub abandoned: u64,
    pub elapsed_ns: u64,
    pub max_ns: u64,
    pub offered_bytes: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct OperationSnapshot {
    pub waiter: CounterSnapshot,
    pub queue: CounterSnapshot,
    pub worker: CounterSnapshot,
    /// Selected PUT branch only; this is not publication or durability success.
    /// Flush always leaves this array zero.
    pub put_path: [u64; PATH_COUNT],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Snapshot {
    pub schema: &'static str,
    pub operation_names: [&'static str; OP_COUNT],
    pub stage_names: [&'static str; STAGE_COUNT],
    pub put_path_names: [&'static str; PATH_COUNT],
    pub operations: [OperationSnapshot; OP_COUNT],
    pub stages: [CounterSnapshot; STAGE_COUNT],
    /// Sticky overflow, underflow or unrepresentable duration observation.
    pub saturated: bool,
    /// Observed concurrent update; false is not a transactional snapshot proof.
    pub concurrent_activity: bool,
    /// Whether the provider performs a forced device barrier.
    /// Always false for OS writeback, independently of platform syscall availability.
    /// Reserved device-stage counters are not emitted by the filesystem provider.
    pub device_barrier_supported: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            operation_names: OPERATION_NAMES,
            stage_names: STAGE_NAMES,
            put_path_names: PUT_PATH_NAMES,
            operations: [OperationSnapshot::default(); OP_COUNT],
            stages: [CounterSnapshot::default(); STAGE_COUNT],
            saturated: false,
            concurrent_activity: false,
            device_barrier_supported: false,
        }
    }
}

#[derive(Default)]
struct AtomicCounter {
    started: AtomicU64,
    inflight: AtomicU64,
    succeeded: AtomicU64,
    failed: AtomicU64,
    abandoned: AtomicU64,
    elapsed_ns: AtomicU64,
    max_ns: AtomicU64,
    offered_bytes: AtomicU64,
}

impl AtomicCounter {
    fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            started: self.started.load(Ordering::SeqCst),
            inflight: self.inflight.load(Ordering::SeqCst),
            succeeded: self.succeeded.load(Ordering::SeqCst),
            failed: self.failed.load(Ordering::SeqCst),
            abandoned: self.abandoned.load(Ordering::SeqCst),
            elapsed_ns: self.elapsed_ns.load(Ordering::SeqCst),
            max_ns: self.max_ns.load(Ordering::SeqCst),
            offered_bytes: self.offered_bytes.load(Ordering::SeqCst),
        }
    }
}

#[derive(Default)]
struct AtomicOperation {
    waiter: AtomicCounter,
    queue: AtomicCounter,
    worker: AtomicCounter,
    put_path: [AtomicU64; PATH_COUNT],
}

impl AtomicOperation {
    fn snapshot(&self) -> OperationSnapshot {
        OperationSnapshot {
            waiter: self.waiter.snapshot(),
            queue: self.queue.snapshot(),
            worker: self.worker.snapshot(),
            put_path: std::array::from_fn(|index| self.put_path[index].load(Ordering::SeqCst)),
        }
    }
}

#[derive(Default)]
struct Bank {
    operations: [AtomicOperation; OP_COUNT],
    stages: [AtomicCounter; STAGE_COUNT],
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

    fn counter(&self, row: Row) -> &AtomicCounter {
        match row {
            Row::Waiter(operation) => &self.operations[operation as usize].waiter,
            Row::Queue(operation) => &self.operations[operation as usize].queue,
            Row::Worker(operation) => &self.operations[operation as usize].worker,
            Row::Stage(stage) => &self.stages[stage as usize],
        }
    }

    fn finish(&self, row: Row, outcome: Outcome, elapsed_ns: u128) {
        let counter = self.counter(row);
        let value = u64::try_from(elapsed_ns).unwrap_or_else(|_| {
            self.saturated.store(true, Ordering::SeqCst);
            u64::MAX
        });
        let terminal = match outcome {
            Outcome::Succeeded => &counter.succeeded,
            Outcome::Failed => &counter.failed,
            Outcome::Abandoned => &counter.abandoned,
        };
        self.add(terminal, 1);
        self.add(&counter.elapsed_ns, value);
        counter.max_ns.fetch_max(value, Ordering::SeqCst);
        self.sub(&counter.inflight, 1);
    }

    fn snapshot(&self) -> Snapshot {
        let revision_before = self.revision.load(Ordering::SeqCst);
        let writers_before = self.writers.load(Ordering::SeqCst);
        let operations = std::array::from_fn(|index| self.operations[index].snapshot());
        let stages = std::array::from_fn(|index| self.stages[index].snapshot());
        let writers_after = self.writers.load(Ordering::SeqCst);
        let revision_after = self.revision.load(Ordering::SeqCst);
        let saturated = self.saturated.load(Ordering::SeqCst);
        Snapshot {
            operations,
            stages,
            saturated,
            concurrent_activity: saturated
                || writers_before != 0
                || writers_after != 0
                || revision_before != revision_after,
            ..Snapshot::default()
        }
    }

    fn begin_update(&self) -> Update<'_> {
        self.add(&self.writers, 1);
        self.add(&self.revision, 1);
        Update(self)
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

/// Bank-only handle. It owns no root descriptor, input, store or worker task.
#[derive(Clone, Default)]
pub struct Observer {
    bank: Option<BankHandle>,
}

impl fmt::Debug for Observer {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("FilesystemBlocksObserver")
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

    /// Explicit bank for controls; constructor allocation is outside warm gates.
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

    pub fn waiter(&self, operation: Operation, offered_bytes: u64) -> Span {
        Span::new(self, Row::Waiter(operation), offered_bytes)
    }

    pub fn queue(&self, operation: Operation, offered_bytes: u64) -> Span {
        Span::new(self, Row::Queue(operation), offered_bytes)
    }

    pub fn worker(&self, operation: Operation, offered_bytes: u64) -> Span {
        Span::new(self, Row::Worker(operation), offered_bytes)
    }

    pub fn stage(&self, stage: Stage, offered_bytes: u64) -> Span {
        Span::new(self, Row::Stage(stage), offered_bytes)
    }

    /// Record the selected branch, once known; never infer acknowledgement.
    pub fn put_path(&self, path: Path) {
        self.update(|bank| {
            bank.add(
                &bank.operations[Operation::Put as usize].put_path[path as usize],
                1,
            );
        });
    }
}

#[derive(Clone, Copy)]
enum Row {
    Waiter(Operation),
    Queue(Operation),
    Worker(Operation),
    Stage(Stage),
}

#[derive(Clone, Copy)]
enum Outcome {
    Succeeded,
    Failed,
    Abandoned,
}

#[must_use = "retain until this exact observation reaches a terminal"]
pub struct Span {
    observer: Observer,
    row: Row,
    started: Option<Instant>,
    finished: bool,
}

impl Span {
    fn new(observer: &Observer, row: Row, offered_bytes: u64) -> Self {
        Self::new_with_clock(observer, row, offered_bytes, Instant::now)
    }

    fn new_with_clock(
        observer: &Observer,
        row: Row,
        offered_bytes: u64,
        clock: impl FnOnce() -> Instant,
    ) -> Self {
        let started = observer.is_enabled().then(clock);
        observer.update(|bank| {
            let counter = bank.counter(row);
            bank.add(&counter.started, 1);
            bank.add(&counter.inflight, 1);
            bank.add(&counter.offered_bytes, offered_bytes);
        });
        Self {
            observer: observer.clone(),
            row,
            started,
            finished: false,
        }
    }

    pub fn finish_success(mut self) {
        self.finish_with_clock(Outcome::Succeeded, |start| start.elapsed().as_nanos());
    }

    pub fn finish_error(mut self) {
        self.finish_with_clock(Outcome::Failed, |start| start.elapsed().as_nanos());
    }

    fn finish_with_clock(&mut self, outcome: Outcome, elapsed: impl FnOnce(Instant) -> u128) {
        if self.finished {
            return;
        }
        self.finished = true;
        if let Some(start) = self.started.take() {
            let elapsed_ns = elapsed(start);
            self.observer
                .update(|bank| bank.finish(self.row, outcome, elapsed_ns));
        }
    }
}

impl Drop for Span {
    fn drop(&mut self) {
        self.finish_with_clock(Outcome::Abandoned, |start| start.elapsed().as_nanos());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::atomic::Ordering;

    const OPERATIONS: [Operation; OP_COUNT] = [Operation::Put, Operation::Flush];
    const STAGES: [Stage; STAGE_COUNT] = [
        Stage::InputCopy,
        Stage::InitialAuthority,
        Stage::ContentId,
        Stage::ShardOpen,
        Stage::ExistingVerify,
        Stage::StageCreateWrite,
        Stage::FileSync,
        Stage::FileDeviceSync,
        Stage::BeforePublishAuthority,
        Stage::PublishName,
        Stage::ShardSync,
        Stage::RootSync,
        Stage::PostDirectoryDeviceSync,
        Stage::FinalAuthority,
    ];

    fn counter(observer: &Observer, row: Row) -> CounterSnapshot {
        let snapshot = observer.snapshot().unwrap();
        match row {
            Row::Waiter(operation) => snapshot.operations[operation as usize].waiter,
            Row::Queue(operation) => snapshot.operations[operation as usize].queue,
            Row::Worker(operation) => snapshot.operations[operation as usize].worker,
            Row::Stage(stage) => snapshot.stages[stage as usize],
        }
    }

    fn fixed_terminal(observer: &Observer, row: Row, bytes: u64, outcome: Outcome, ns: u128) {
        let mut span = Span::new_with_clock(observer, row, bytes, Instant::now);
        span.finish_with_clock(outcome, |_| ns);
        drop(span);
    }

    #[test]
    fn every_operation_and_stage_has_separate_terminal_accounting() {
        let observer = Observer::isolated();
        let mut rows = [Row::Stage(Stage::InputCopy); OP_COUNT * 3 + STAGE_COUNT];
        let mut next = 0;
        for operation in OPERATIONS {
            for row in [
                Row::Waiter(operation),
                Row::Queue(operation),
                Row::Worker(operation),
            ] {
                rows[next] = row;
                next += 1;
            }
        }
        for stage in STAGES {
            rows[next] = Row::Stage(stage);
            next += 1;
        }
        assert_eq!(next, rows.len());
        for (index, row) in rows.into_iter().enumerate() {
            let unique = index as u64 + 1;
            fixed_terminal(&observer, row, unique, Outcome::Succeeded, 3);
            fixed_terminal(&observer, row, unique * 2, Outcome::Failed, 5);
            fixed_terminal(&observer, row, unique * 3, Outcome::Abandoned, 7);
            let value = counter(&observer, row);
            assert_eq!(value.started, 3, "row {index} was not started");
            assert_eq!(value.inflight, 0, "row {index} did not settle");
            assert_eq!((value.succeeded, value.failed, value.abandoned), (1, 1, 1));
            assert_eq!(value.offered_bytes, unique * 6);
            assert_eq!((value.elapsed_ns, value.max_ns), (15, 7));
            assert_eq!(
                value.started,
                value.succeeded + value.failed + value.abandoned
            );
        }
        assert!(!observer.snapshot().unwrap().saturated);
    }

    #[test]
    fn consuming_terminals_and_unfinished_drop_settle_once() {
        let observer = Observer::isolated();
        let row = Row::Worker(Operation::Put);
        observer.worker(Operation::Put, 2).finish_success();
        observer.worker(Operation::Put, 3).finish_error();
        drop(observer.worker(Operation::Put, 5));
        let mut span = Span::new_with_clock(&observer, row, 7, Instant::now);
        let reads = Cell::new(0);
        span.finish_with_clock(Outcome::Succeeded, |_| {
            reads.set(reads.get() + 1);
            11
        });
        span.finish_with_clock(Outcome::Failed, |_| panic!("terminal called twice"));
        drop(span);
        assert_eq!(reads.get(), 1);
        let value = counter(&observer, row);
        assert_eq!((value.started, value.inflight), (4, 0));
        assert_eq!((value.succeeded, value.failed, value.abandoned), (2, 1, 1));
        assert_eq!(value.offered_bytes, 17);
    }

    #[test]
    fn disabled_observation_reads_neither_clock_and_allocates_no_bank() {
        let disabled = Observer::disabled();
        let starts = Cell::new(0);
        let finishes = Cell::new(0);
        for outcome in [Outcome::Succeeded, Outcome::Failed, Outcome::Abandoned] {
            let mut span = Span::new_with_clock(&disabled, Row::Waiter(Operation::Put), 9, || {
                starts.set(starts.get() + 1);
                Instant::now()
            });
            span.finish_with_clock(outcome, |_| {
                finishes.set(finishes.get() + 1);
                19
            });
            drop(span);
        }
        drop(disabled.stage(Stage::FileSync, 0));
        disabled.put_path(Path::Created);
        assert_eq!((starts.get(), finishes.get()), (0, 0));
        assert!(disabled.bank().is_none() && disabled.snapshot().is_none());
        let enabled = Observer::isolated();
        let mut positive = Span::new_with_clock(&enabled, Row::Waiter(Operation::Put), 1, || {
            starts.set(starts.get() + 1);
            Instant::now()
        });
        positive.finish_with_clock(Outcome::Succeeded, |_| {
            finishes.set(finishes.get() + 1);
            23
        });
        assert_eq!((starts.get(), finishes.get()), (1, 1));
        assert_eq!(
            counter(&enabled, Row::Waiter(Operation::Put)).elapsed_ns,
            23
        );
    }

    #[test]
    fn inflight_is_an_absolute_gauge_and_selected_paths_are_not_successes() {
        let observer = Observer::isolated();
        let put = observer.worker(Operation::Put, 4);
        let flush = observer.worker(Operation::Flush, 0);
        let stage = observer.stage(Stage::PublishName, 4);
        observer.put_path(Path::Existing);
        observer.put_path(Path::Created);
        observer.put_path(Path::RaceExisting);
        observer.put_path(Path::Created);
        let active = observer.snapshot().unwrap();
        assert_eq!(
            active.operations[Operation::Put as usize].worker.inflight,
            1
        );
        assert_eq!(
            active.operations[Operation::Flush as usize].worker.inflight,
            1
        );
        assert_eq!(active.stages[Stage::PublishName as usize].inflight, 1);
        assert_eq!(
            active.operations[Operation::Put as usize].put_path,
            [1, 2, 1]
        );
        assert_eq!(
            active.operations[Operation::Flush as usize].put_path,
            [0, 0, 0]
        );
        assert_eq!(
            active.operations[Operation::Put as usize].worker.succeeded,
            0
        );
        put.finish_error();
        flush.finish_success();
        drop(stage);
        let settled = observer.snapshot().unwrap();
        assert_eq!(
            settled.operations[Operation::Put as usize].worker.inflight,
            0
        );
        assert_eq!(settled.operations[Operation::Put as usize].worker.failed, 1);
        assert_eq!(settled.stages[Stage::PublishName as usize].abandoned, 1);
        assert_eq!(
            settled.operations[Operation::Put as usize].put_path,
            [1, 2, 1]
        );
    }

    #[test]
    fn arithmetic_and_unrepresentable_duration_saturate_with_sticky_quality() {
        let observer = Observer::isolated();
        let bank = observer.bank().unwrap();
        let bytes = &bank.operations[Operation::Put as usize]
            .worker
            .offered_bytes;
        bytes.store(u64::MAX - 1, Ordering::SeqCst);
        bank.add(bytes, 9);
        assert_eq!(bytes.load(Ordering::SeqCst), u64::MAX);
        bank.sub(&bank.stages[Stage::FileSync as usize].inflight, 1);
        assert_eq!(
            bank.stages[Stage::FileSync as usize]
                .inflight
                .load(Ordering::SeqCst),
            0
        );
        fixed_terminal(
            &observer,
            Row::Stage(Stage::RootSync),
            0,
            Outcome::Succeeded,
            u128::MAX,
        );
        let saturated = observer.snapshot().unwrap();
        assert!(saturated.saturated && saturated.concurrent_activity);
        assert_eq!(
            saturated.stages[Stage::RootSync as usize].elapsed_ns,
            u64::MAX
        );
        assert_eq!(saturated.stages[Stage::RootSync as usize].max_ns, u64::MAX);
        observer.stage(Stage::RootSync, 0).finish_success();
        assert!(observer.snapshot().unwrap().saturated);
    }

    #[test]
    fn snapshot_discloses_active_update_and_revision_saturation() {
        let observer = Observer::isolated();
        let bank = observer.bank().unwrap();
        assert!(!observer.snapshot().unwrap().concurrent_activity);
        let active = bank.begin_update();
        assert!(observer.snapshot().unwrap().concurrent_activity);
        drop(active);
        assert!(!observer.snapshot().unwrap().concurrent_activity);
        bank.revision.store(u64::MAX, Ordering::SeqCst);
        drop(bank.begin_update());
        let saturated = observer.snapshot().unwrap();
        assert!(saturated.saturated && saturated.concurrent_activity);
    }

    #[test]
    fn span_and_observer_retain_only_the_isolated_bank() {
        let observer = Observer::isolated();
        let bank = match observer.bank.as_ref().unwrap() {
            BankHandle::Isolated(bank) => bank,
            BankHandle::Global(_) => panic!("isolated observer used global bank"),
        };
        assert_eq!(Arc::strong_count(bank), 1);
        let clone = observer.clone();
        let span = clone.queue(Operation::Put, 0);
        assert_eq!(Arc::strong_count(bank), 3);
        drop(clone);
        assert_eq!(Arc::strong_count(bank), 2);
        drop(span);
        assert_eq!(Arc::strong_count(bank), 1);
        assert_eq!(counter(&observer, Row::Queue(Operation::Put)).abandoned, 1);
    }

    #[test]
    fn snapshots_are_fixed_copy_values_with_fixed_labels_and_no_identifier_values() {
        fn requires_copy<T: Copy>(_: T) {}
        fn numeric_tree(value: &serde_json::Value) {
            match value {
                serde_json::Value::Object(fields) => {
                    for child in fields.values() {
                        numeric_tree(child);
                    }
                }
                serde_json::Value::Array(children) => {
                    for child in children {
                        numeric_tree(child);
                    }
                }
                serde_json::Value::Number(_) | serde_json::Value::Bool(_) => {}
                _ => panic!("snapshot contained an identifier or unknown value"),
            }
        }
        let observer = Observer::isolated();
        observer.put_path(Path::Created);
        observer.stage(Stage::InputCopy, 101).finish_success();
        let snapshot = observer.snapshot().unwrap();
        requires_copy(snapshot);
        assert_eq!(snapshot.operations.len(), 2);
        assert_eq!(snapshot.stages.len(), 14);
        assert_eq!(snapshot.schema, "mount-rs.filesystem-blocks-bank.v1");
        assert_eq!(snapshot.operation_names, ["put", "flush"]);
        assert_eq!(snapshot.stage_names, STAGE_NAMES);
        assert_eq!(
            snapshot.put_path_names,
            ["existing", "created", "race_existing"]
        );
        assert!(!snapshot.device_barrier_supported);
        let encoded = serde_json::to_value(snapshot).unwrap();
        numeric_tree(&encoded["operations"]);
        numeric_tree(&encoded["stages"]);
        assert_eq!(
            snapshot.stages[Stage::InputCopy as usize].offered_bytes,
            101
        );
        let zero = Snapshot::default();
        assert_eq!(zero.schema, snapshot.schema);
        assert_eq!(zero.stage_names, snapshot.stage_names);
        assert_eq!(
            zero.device_barrier_supported,
            snapshot.device_barrier_supported
        );
    }
}
