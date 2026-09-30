//! Bounded, opt-in observations before a service listener exists.
//! Durations are inclusive wall time; a record is not an atomic or drained cut.

use serde::{Deserialize, Serialize, ser::SerializeStruct};
use std::{
    future::Future,
    io::{self, Write},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::time::{Duration, Instant};

pub const RECORD_LIMIT: usize = 16 * 1024;
pub const PROGRESS_INTERVAL: Duration = Duration::from_secs(5);
const STAGE_COUNT: usize = 13;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(usize)]
pub enum Stage {
    Configuration,
    CatalogOpen,
    CatalogLoad,
    CatalogValidate,
    TlsMaterial,
    CacheStart,
    DriveConfig,
    DriveOpen,
    BackingReceipt,
    DriveRegister,
    ListenerBind,
    Ready,
    Cleanup,
}
const STAGES: [Stage; STAGE_COUNT] = [
    Stage::Configuration,
    Stage::CatalogOpen,
    Stage::CatalogLoad,
    Stage::CatalogValidate,
    Stage::TlsMaterial,
    Stage::CacheStart,
    Stage::DriveConfig,
    Stage::DriveOpen,
    Stage::BackingReceipt,
    Stage::DriveRegister,
    Stage::ListenerBind,
    Stage::Ready,
    Stage::Cleanup,
];
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Running,
    Ready,
    Error,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CleanupOutcome {
    Success,
    Error,
    Cancelled,
}
#[derive(Clone, Copy)]
pub struct Identity {
    pid: u32,
    worker: Option<u8>,
    generation: u64,
}
impl Identity {
    pub fn cli() -> Self {
        Self {
            pid: std::process::id(),
            worker: None,
            generation: 0,
        }
    }
    pub fn worker(index: usize, generation: u64) -> Self {
        Self {
            pid: std::process::id(),
            worker: Some(u8::try_from(index).unwrap_or(u8::MAX)),
            generation,
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageRow {
    pub stage: Stage,
    pub started: u64,
    pub success: u64,
    pub error: u64,
    pub cancelled: u64,
    pub in_flight: u64,
    pub elapsed_ns: u64,
    pub max_ns: u64,
}
impl StageRow {
    fn new(stage: Stage) -> Self {
        Self {
            stage,
            started: 0,
            success: 0,
            error: 0,
            cancelled: 0,
            in_flight: 0,
            elapsed_ns: 0,
            max_ns: 0,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Schema {
    #[serde(rename = "mount-rs.startup.v1")]
    V1,
    #[serde(rename = "mount-rs.startup.v2")]
    V2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConstructionMode {
    Lazy,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema: Schema,
    pub pid: u32,
    #[serde(deserialize_with = "required_nullable")]
    pub worker: Option<u8>,
    pub generation: u64,
    pub observed_unix_ms: u64,
    pub current_stage: Stage,
    pub terminal_outcome: Outcome,
    #[serde(deserialize_with = "required_nullable")]
    pub cleanup_outcome: Option<CleanupOutcome>,
    #[serde(deserialize_with = "required_nullable")]
    pub configured_partitions: Option<u64>,
    #[serde(deserialize_with = "required_nullable")]
    pub planned_drives: Option<u64>,
    pub open_started: u64,
    pub open_success: u64,
    pub open_error: u64,
    pub open_cancelled: u64,
    pub open_in_flight: u64,
    pub registered_drives: u64,
    pub elapsed_ns: u64,
    pub current_stage_elapsed_ns: u64,
    pub accounting_complete: bool,
    pub banks_captured: bool,
    pub stages: [StageRow; STAGE_COUNT],
    #[serde(default)]
    pub construction_mode: Option<ConstructionMode>,
    #[serde(default)]
    pub max_active_drives: Option<u64>,
    #[serde(default)]
    pub construction_plans: Option<u64>,
}
impl Serialize for Snapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let lazy = self.schema == Schema::V2;
        let mut value = serializer.serialize_struct("Snapshot", if lazy { 24 } else { 21 })?;
        value.serialize_field("schema", &self.schema)?;
        value.serialize_field("pid", &self.pid)?;
        value.serialize_field("worker", &self.worker)?;
        value.serialize_field("generation", &self.generation)?;
        value.serialize_field("observed_unix_ms", &self.observed_unix_ms)?;
        value.serialize_field("current_stage", &self.current_stage)?;
        value.serialize_field("terminal_outcome", &self.terminal_outcome)?;
        value.serialize_field("cleanup_outcome", &self.cleanup_outcome)?;
        value.serialize_field("configured_partitions", &self.configured_partitions)?;
        value.serialize_field("planned_drives", &self.planned_drives)?;
        value.serialize_field("open_started", &self.open_started)?;
        value.serialize_field("open_success", &self.open_success)?;
        value.serialize_field("open_error", &self.open_error)?;
        value.serialize_field("open_cancelled", &self.open_cancelled)?;
        value.serialize_field("open_in_flight", &self.open_in_flight)?;
        value.serialize_field("registered_drives", &self.registered_drives)?;
        value.serialize_field("elapsed_ns", &self.elapsed_ns)?;
        value.serialize_field("current_stage_elapsed_ns", &self.current_stage_elapsed_ns)?;
        value.serialize_field("accounting_complete", &self.accounting_complete)?;
        value.serialize_field("banks_captured", &self.banks_captured)?;
        value.serialize_field("stages", &self.stages)?;
        if lazy {
            value.serialize_field("construction_mode", &self.construction_mode)?;
            value.serialize_field("max_active_drives", &self.max_active_drives)?;
            value.serialize_field("construction_plans", &self.construction_plans)?;
        }
        value.end()
    }
}
fn required_nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}
impl Snapshot {
    pub fn parse(bytes: &[u8]) -> io::Result<Self> {
        let invalid = || io::Error::other("startup observation invalid");
        if bytes.len() > RECORD_LIMIT {
            return Err(invalid());
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let projection: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        let fields = projection.as_object().ok_or_else(invalid)?;
        let lazy = value.schema == Schema::V2;
        let expected_fields = if lazy { 24 } else { 21 };
        if fields.len() != expected_fields
            || (lazy
                && ![
                    "construction_mode",
                    "max_active_drives",
                    "construction_plans",
                ]
                .iter()
                .all(|field| fields.contains_key(*field)))
        {
            return Err(invalid());
        }
        if value.pid == 0
            || value.worker.is_some_and(|worker| worker >= 10)
            || value.observed_unix_ms == 0
            || value.banks_captured
        {
            return Err(invalid());
        }
        for (row, stage) in value.stages.iter().zip(STAGES) {
            if row.stage != stage
                || (value.accounting_complete
                    && (row.max_ns > row.elapsed_ns
                        || row
                            .success
                            .checked_add(row.error)
                            .and_then(|count| count.checked_add(row.cancelled))
                            .and_then(|count| count.checked_add(row.in_flight))
                            != Some(row.started)))
            {
                return Err(invalid());
            }
        }
        let open = value.stages[Stage::DriveOpen as usize];
        if (
            value.open_started,
            value.open_success,
            value.open_error,
            value.open_cancelled,
            value.open_in_flight,
        ) != (
            open.started,
            open.success,
            open.error,
            open.cancelled,
            open.in_flight,
        ) {
            return Err(invalid());
        }
        if lazy {
            let plans = value.construction_plans.ok_or_else(invalid)?;
            if value.construction_mode != Some(ConstructionMode::Lazy)
                || value.max_active_drives == Some(0)
                || value.max_active_drives.is_some() != value.planned_drives.is_some()
                || value.configured_partitions.is_some() != value.planned_drives.is_some()
                || value.registered_drives > plans
                || plans > value.planned_drives.unwrap_or(0)
                || (
                    open.started,
                    open.success,
                    open.error,
                    open.cancelled,
                    open.in_flight,
                ) != (0, 0, 0, 0, 0)
            {
                return Err(invalid());
            }
        } else if value.registered_drives > value.open_success {
            return Err(invalid());
        }
        if (lazy || value.accounting_complete)
            && value.terminal_outcome == Outcome::Ready
            && (!value.planned_drives.is_some_and(|planned| {
                planned == value.registered_drives
                    && if lazy {
                        Some(planned) == value.construction_plans
                            && value.max_active_drives.is_some_and(|limit| limit > 0)
                    } else {
                        planned == value.open_success
                    }
            }) || value.open_error != 0
                || value.open_cancelled != 0
                || value.open_in_flight != 0)
        {
            return Err(invalid());
        }
        Ok(value)
    }
}
struct State {
    identity: Identity,
    schema: Schema,
    started: Instant,
    stage_started: Instant,
    stage_elapsed: Option<u64>,
    startup_elapsed: Option<u64>,
    next_progress: Instant,
    current_stage: Stage,
    outcome: Outcome,
    cleanup: Option<CleanupOutcome>,
    partitions: Option<u64>,
    planned: Option<u64>,
    max_active_drives: Option<u64>,
    construction_plans: u64,
    registered: u64,
    complete: bool,
    rows: [StageRow; STAGE_COUNT],
}
fn clock_now() -> Instant {
    #[cfg(test)]
    CLOCK_READS.set(CLOCK_READS.get() + 1);
    Instant::now()
}
fn ns(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
fn add(value: &mut u64, amount: u64, complete: &mut bool) {
    match value.checked_add(amount) {
        Some(next) => *value = next,
        None => {
            *value = u64::MAX;
            *complete = false;
        }
    }
}
impl State {
    fn new(identity: Identity, schema: Schema) -> Self {
        let now = clock_now();
        Self {
            identity,
            schema,
            started: now,
            stage_started: now,
            stage_elapsed: Some(0),
            startup_elapsed: None,
            next_progress: now,
            current_stage: Stage::Configuration,
            outcome: Outcome::Running,
            cleanup: None,
            partitions: None,
            planned: None,
            max_active_drives: None,
            construction_plans: 0,
            registered: 0,
            complete: identity.worker.is_none_or(|i| i < 10),
            rows: STAGES.map(StageRow::new),
        }
    }
    fn snapshot(&self) -> Snapshot {
        let now = clock_now();
        let open = self.rows[Stage::DriveOpen as usize];
        #[cfg(test)]
        CLOCK_READS.set(CLOCK_READS.get() + 1);
        let observed_unix_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_millis().min(u128::from(u64::MAX)) as u64);
        Snapshot {
            schema: self.schema,
            pid: self.identity.pid,
            worker: self.identity.worker,
            generation: self.identity.generation,
            observed_unix_ms,
            current_stage: self.current_stage,
            terminal_outcome: self.outcome,
            cleanup_outcome: self.cleanup,
            configured_partitions: self.partitions,
            planned_drives: self.planned,
            open_started: open.started,
            open_success: open.success,
            open_error: open.error,
            open_cancelled: open.cancelled,
            open_in_flight: open.in_flight,
            registered_drives: self.registered,
            elapsed_ns: self
                .startup_elapsed
                .unwrap_or_else(|| ns(now.duration_since(self.started))),
            current_stage_elapsed_ns: self
                .stage_elapsed
                .unwrap_or_else(|| ns(now.duration_since(self.stage_started))),
            accounting_complete: self.complete,
            banks_captured: false,
            stages: self.rows,
            construction_mode: (self.schema == Schema::V2).then_some(ConstructionMode::Lazy),
            max_active_drives: self.max_active_drives,
            construction_plans: (self.schema == Schema::V2).then_some(self.construction_plans),
        }
    }
}
/// A disabled recorder holds no heap storage and never reads a clock or creates a timer.
/// The fixed state exists only for an explicitly enabled caller.
pub struct Startup {
    schema: Schema,
    lazy_plan_set: AtomicBool,
    state: Option<Mutex<State>>,
}
#[must_use]
pub struct Attempt<'a> {
    startup: &'a Startup,
    stage: Stage,
    started: Option<Instant>,
    finished: bool,
}
impl Attempt<'_> {
    pub fn finish(mut self, success: bool) {
        self.complete(if success { None } else { Some(false) });
        self.finished = true;
    }
    fn complete(&mut self, failure: Option<bool>) {
        let Some(started) = self.started else {
            return;
        };
        self.startup.with_state(|state| {
            let elapsed = ns(clock_now().duration_since(started));
            let row = &mut state.rows[self.stage as usize];
            match row.in_flight.checked_sub(1) {
                Some(next) => row.in_flight = next,
                None => state.complete = false,
            }
            add(&mut row.elapsed_ns, elapsed, &mut state.complete);
            row.max_ns = row.max_ns.max(elapsed);
            match failure {
                None => add(&mut row.success, 1, &mut state.complete),
                Some(false) => add(&mut row.error, 1, &mut state.complete),
                Some(true) => {
                    add(&mut row.cancelled, 1, &mut state.complete);
                    state.complete = false;
                    if self.stage == Stage::Cleanup {
                        state.cleanup = Some(CleanupOutcome::Cancelled);
                    } else if state.outcome == Outcome::Running {
                        state.outcome = Outcome::Cancelled;
                    }
                }
            }
            if state.current_stage == self.stage {
                state.stage_elapsed = Some(elapsed);
            }
        });
    }
}
impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.complete(Some(true));
        }
    }
}
impl Startup {
    pub fn new(enabled: bool, identity: Identity) -> Self {
        Self::with_schema(enabled, identity, Schema::V1)
    }
    /// Additive lazy-startup observations; this does not activate a runtime pool.
    pub fn new_lazy(enabled: bool, identity: Identity) -> Self {
        Self::with_schema(enabled, identity, Schema::V2)
    }
    fn with_schema(enabled: bool, identity: Identity, schema: Schema) -> Self {
        Self {
            schema,
            lazy_plan_set: AtomicBool::new(false),
            state: enabled.then(|| Mutex::new(State::new(identity, schema))),
        }
    }
    pub fn enabled(&self) -> bool {
        self.state.is_some()
    }
    fn with_state<R>(&self, apply: impl FnOnce(&mut State) -> R) -> Option<R> {
        let mutex = self.state.as_ref()?;
        let mut state = match mutex.lock() {
            Ok(state) => state,
            Err(poison) => {
                let mut state = poison.into_inner();
                state.complete = false;
                state
            }
        };
        Some(apply(&mut state))
    }
    pub fn plan(&self, partitions: u64, drives: u64) {
        self.with_state(|state| {
            if state.schema != Schema::V1 {
                state.complete = false;
                return;
            }
            if state.planned.is_some() {
                state.complete = false;
            }
            state.partitions = Some(partitions);
            state.planned = Some(drives);
        });
    }
    /// Set immutable lazy-plan totals once. Validation also runs when observation is disabled.
    pub fn plan_lazy(
        &self,
        partitions: u64,
        drives: u64,
        max_active_drives: u64,
    ) -> io::Result<()> {
        if self.schema != Schema::V2 || max_active_drives == 0 {
            return Err(io::Error::other("lazy startup plan invalid"));
        }
        if self
            .lazy_plan_set
            .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
        {
            return Err(io::Error::other("lazy startup plan already configured"));
        }
        self.with_state(|state| {
            state.partitions = Some(partitions);
            state.planned = Some(drives);
            state.max_active_drives = Some(max_active_drives);
        });
        Ok(())
    }
    /// Count one successfully completed immutable construction plan, without opening its provider.
    pub fn construction_plan(&self) {
        self.with_state(|state| {
            if state.schema != Schema::V2 || state.planned.is_none() {
                state.complete = false;
                return;
            }
            add(&mut state.construction_plans, 1, &mut state.complete);
            if state.construction_plans > state.planned.unwrap_or(0) {
                state.complete = false;
            }
        });
    }
    pub fn begin(&self, stage: Stage) -> Attempt<'_> {
        let started = self.with_state(|state| {
            let now = clock_now();
            state.current_stage = stage;
            state.stage_started = now;
            state.stage_elapsed = None;
            let row = &mut state.rows[stage as usize];
            add(&mut row.started, 1, &mut state.complete);
            add(&mut row.in_flight, 1, &mut state.complete);
            now
        });
        Attempt {
            startup: self,
            stage,
            started,
            finished: false,
        }
    }
    pub fn registered(&self) {
        self.with_state(|state| {
            add(&mut state.registered, 1, &mut state.complete);
            let completed = if state.schema == Schema::V2 {
                state.construction_plans
            } else {
                state.rows[Stage::DriveOpen as usize].success
            };
            if state.registered > completed {
                state.complete = false;
            }
        });
    }
    pub fn finish_startup(&self, success: bool) {
        self.with_state(|state| {
            if !matches!(state.outcome, Outcome::Running | Outcome::Cancelled) {
                return;
            }
            state.startup_elapsed = Some(ns(clock_now().duration_since(state.started)));
            let row = state.rows[Stage::DriveOpen as usize];
            let ready = state.planned.is_some_and(|planned| {
                state.registered == planned
                    && if state.schema == Schema::V2 {
                        state.construction_plans == planned
                            && state.max_active_drives.is_some_and(|limit| limit > 0)
                            && row.started == 0
                            && row.success == 0
                    } else {
                        row.success == planned
                    }
            }) && row.error == 0
                && row.cancelled == 0
                && row.in_flight == 0;
            state.outcome = if success && ready {
                Outcome::Ready
            } else {
                Outcome::Error
            };
            if success && !ready {
                state.complete = false;
            }
            if state.outcome == Outcome::Ready {
                state.current_stage = Stage::Ready;
                state.stage_elapsed = Some(0);
            }
        });
    }
    pub fn finish_cleanup(&self, success: bool) {
        self.with_state(|state| {
            state.cleanup = Some(if success {
                CleanupOutcome::Success
            } else {
                CleanupOutcome::Error
            });
        });
    }
    pub fn snapshot(&self) -> Option<Snapshot> {
        self.with_state(|state| state.snapshot())
    }
    /// Explicit start/terminal boundary. Periodic publication is throttled in observe.
    pub fn publish(&self, sink: &mut impl FnMut(&Snapshot) -> io::Result<()>) {
        self.publish_if_due(sink, true);
    }
    fn publish_if_due(&self, sink: &mut impl FnMut(&Snapshot) -> io::Result<()>, force: bool) {
        let snapshot = self
            .with_state(|state| {
                let now = clock_now();
                if !force && now < state.next_progress {
                    return None;
                }
                state.next_progress = now + PROGRESS_INTERVAL;
                Some(state.snapshot())
            })
            .flatten();
        if let Some(snapshot) = snapshot
            && sink(&snapshot).is_err()
        {
            self.with_state(|state| state.complete = false);
        }
    }
    /// Observe one existing future without spawning or changing its result/deadline.
    /// Enabled wall measurements include publication costs and overlapping inner work.
    pub async fn observe<T, E>(
        &self,
        stage: Stage,
        future: impl Future<Output = Result<T, E>>,
        sink: &mut impl FnMut(&Snapshot) -> io::Result<()>,
    ) -> Result<T, E> {
        if !self.enabled() {
            return future.await;
        }
        let attempt = self.begin(stage);
        self.publish_if_due(sink, false);
        tokio::pin!(future);
        loop {
            let next = self
                .with_state(|state| state.next_progress)
                .expect("enabled recorder");
            tokio::select! {
                biased;
                result = &mut future => { attempt.finish(result.is_ok()); return result; }
                _ = tokio::time::sleep_until(next) => self.publish_if_due(sink, false),
            }
        }
    }
    pub fn write_record(writer: &mut impl Write, snapshot: &Snapshot) -> io::Result<()> {
        write_bounded::<RECORD_LIMIT>(writer, b"\nstartup_diagnostics ", snapshot)
    }
    pub fn write_json(writer: &mut impl Write, snapshot: &Snapshot) -> io::Result<()> {
        write_bounded::<RECORD_LIMIT>(writer, b"", snapshot)
    }
    /// Existing banks on a normal failure boundary. No quiescence/atomic-cut claim.
    /// Disabled callers do not capture or serialize either bank.
    pub fn write_banks(&self, writer: &mut impl Write) -> io::Result<()> {
        if !self.enabled() {
            return Ok(());
        }
        #[derive(Serialize)]
        struct Banks {
            schema: &'static str,
            startup: Snapshot,
            capture_elapsed_ns: u64,
            capture_atomic: bool,
            application_drain_proven: bool,
            scope: &'static str,
            profile: Option<mount_rs_core::diagnostics::profile::Snapshot>,
            storage: Option<mount_rs_core::diagnostics::storage::Snapshot>,
        }
        let began = clock_now();
        let profile = mount_rs_core::diagnostics::profile::enabled()
            .then(mount_rs_core::diagnostics::profile::snapshot);
        let storage = mount_rs_core::diagnostics::storage::enabled()
            .then(mount_rs_core::diagnostics::storage::snapshot);
        let value = Banks {
            schema: "mount-rs.startup-banks.v1",
            startup: self.snapshot().expect("enabled recorder"),
            capture_elapsed_ns: ns(clock_now().duration_since(began)),
            capture_atomic: false,
            application_drain_proven: false,
            scope: "existing process-local cumulative banks; inclusive wall and logical API counts; includes startup observers/background; no physical IOPS or isolated CPU",
            profile,
            storage,
        };
        let result =
            write_bounded::<{ 256 * 1024 }>(writer, b"\nstartup_bank_diagnostics ", &value);
        if result.is_err() {
            self.with_state(|state| state.complete = false);
        }
        result
    }
}
impl Drop for Startup {
    fn drop(&mut self) {
        // A future dropped before the caller's normal failure flush is incomplete.
        // This synchronous best effort does not guarantee flush after process loss.
        let cancelled = self
            .with_state(|state| {
                if !matches!(state.outcome, Outcome::Running | Outcome::Cancelled) {
                    return false;
                }
                state.outcome = Outcome::Cancelled;
                state.complete = false;
                state.startup_elapsed = Some(ns(clock_now().duration_since(state.started)));
                true
            })
            .unwrap_or(false);
        if cancelled && let Some(snapshot) = self.snapshot() {
            let _ = Self::write_record(&mut io::stderr().lock(), &snapshot);
        }
    }
}
struct Buffer<const N: usize> {
    bytes: [u8; N],
    len: usize,
    limit: usize,
}
impl<const N: usize> Write for Buffer<N> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self
            .len
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("startup observation exceeds bound"))?;
        if end > self.limit {
            return Err(io::Error::other("startup observation exceeds bound"));
        }
        self.bytes[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
/// Write one bounded frame. Callers own their fixed, nonsecret schema and prefix.
pub fn write_bounded<const N: usize>(
    writer: &mut impl Write,
    prefix: &[u8],
    value: &impl Serialize,
) -> io::Result<()> {
    let mut buffer = Buffer {
        bytes: [0; N],
        len: 0,
        limit: N.saturating_sub(1),
    };
    buffer.write_all(prefix)?;
    serde_json::to_writer(&mut buffer, value)?;
    buffer.limit = N;
    buffer.write_all(b"\n")?;
    writer.write_all(&buffer.bytes[..buffer.len])?;
    writer.flush()
}

#[cfg(test)]
thread_local! { static CLOCK_READS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lazy_producer_registers_completed_plans_without_opening_providers() {
        let startup = Startup::new_lazy(true, Identity::cli());
        let initial = serde_json::to_value(startup.snapshot().unwrap()).unwrap();
        assert_eq!(initial["schema"], "mount-rs.startup.v2");
        assert_eq!(initial["max_active_drives"], serde_json::Value::Null);
        assert_eq!(initial["construction_plans"], 0);
        startup.plan_lazy(2, 3, 2_048).unwrap();
        for _ in 0..3 {
            startup.begin(Stage::DriveConfig).finish(true);
            startup.construction_plan();
            startup.begin(Stage::DriveRegister).finish(true);
            startup.registered();
        }
        startup.finish_startup(true);
        let snapshot = startup.snapshot().unwrap();
        assert_eq!(snapshot.terminal_outcome, Outcome::Ready);
        assert!(snapshot.accounting_complete);
        assert_eq!((snapshot.open_started, snapshot.registered_drives), (0, 3));
        let value = serde_json::to_value(snapshot).unwrap();
        assert_eq!(value["max_active_drives"], 2_048);
        assert_eq!(value["construction_plans"], 3);
        Snapshot::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    }
    #[test]
    fn lazy_plan_rejects_zero_duplicate_and_eager_schema_without_overwrite() {
        let startup = Startup::new_lazy(true, Identity::cli());
        assert!(startup.plan_lazy(2, 3, 0).is_err());
        startup.plan_lazy(2, 3, 1).unwrap();
        assert!(startup.plan_lazy(7, 8, 9).is_err());
        let value = serde_json::to_value(startup.snapshot().unwrap()).unwrap();
        assert_eq!(value["configured_partitions"], 2);
        assert_eq!(value["planned_drives"], 3);
        assert_eq!(value["max_active_drives"], 1);
        let eager = Startup::new(true, Identity::cli());
        assert!(eager.plan_lazy(2, 3, 1).is_err());
        assert_eq!(eager.snapshot().unwrap().planned_drives, None);
    }
    #[test]
    fn disabled_lazy_producer_validates_plan_without_clocks_or_publication() {
        CLOCK_READS.set(0);
        let startup = Startup::new_lazy(false, Identity::cli());
        assert!(startup.plan_lazy(2, 3, 0).is_err());
        startup.plan_lazy(2, 3, 1).unwrap();
        assert!(startup.plan_lazy(2, 3, 1).is_err());
        startup.construction_plan();
        startup.registered();
        startup.finish_startup(true);
        startup.publish(&mut |_| panic!("disabled producer published"));
        assert!(startup.snapshot().is_none());
        assert_eq!(CLOCK_READS.get(), 0);
    }
    fn lazy_ready_projection() -> serde_json::Value {
        let startup = Startup::new(true, Identity::cli());
        let mut value = serde_json::to_value(startup.snapshot().unwrap()).unwrap();
        value["schema"] = serde_json::json!("mount-rs.startup.v2");
        value["construction_mode"] = serde_json::json!("lazy");
        value["max_active_drives"] = serde_json::json!(2_048);
        value["construction_plans"] = serde_json::json!(3);
        value["configured_partitions"] = serde_json::json!(2);
        value["planned_drives"] = serde_json::json!(3);
        value["registered_drives"] = serde_json::json!(3);
        value["current_stage"] = serde_json::json!("ready");
        value["terminal_outcome"] = serde_json::json!("ready");
        value["stages"][Stage::DriveConfig as usize]["started"] = serde_json::json!(3);
        value["stages"][Stage::DriveConfig as usize]["success"] = serde_json::json!(3);
        value["stages"][Stage::DriveRegister as usize]["started"] = serde_json::json!(3);
        value["stages"][Stage::DriveRegister as usize]["success"] = serde_json::json!(3);
        value
    }
    #[test]
    fn lazy_v2_ready_accepts_registered_plans_without_startup_opens() {
        let value = lazy_ready_projection();
        let snapshot = Snapshot::parse(&serde_json::to_vec(&value).unwrap())
            .expect("registered lazy plans must be ready without opening providers");
        assert_eq!(snapshot.registered_drives, 3);
        assert_eq!((snapshot.open_started, snapshot.open_success), (0, 0));
        assert_eq!(serde_json::to_value(snapshot).unwrap(), value);
    }
    #[test]
    fn lazy_v2_projection_is_closed_and_rejects_false_readiness() {
        let original = lazy_ready_projection();
        Snapshot::parse(&serde_json::to_vec(&original).unwrap()).unwrap();
        for key in [
            "construction_mode",
            "max_active_drives",
            "construction_plans",
        ] {
            let mut value = original.clone();
            value.as_object_mut().unwrap().remove(key);
            assert!(
                Snapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err(),
                "missing {key}"
            );
        }
        for (key, replacement) in [
            ("construction_mode", serde_json::json!("eager")),
            ("max_active_drives", serde_json::json!(0)),
            ("max_active_drives", serde_json::Value::Null),
            ("construction_plans", serde_json::json!(2)),
            ("registered_drives", serde_json::json!(2)),
            ("planned_drives", serde_json::Value::Null),
        ] {
            let mut value = original.clone();
            value[key] = replacement;
            assert!(
                Snapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err(),
                "invalid {key}"
            );
        }
        let mut opened = original.clone();
        opened["open_started"] = serde_json::json!(1);
        opened["open_success"] = serde_json::json!(1);
        opened["stages"][Stage::DriveOpen as usize]["started"] = serde_json::json!(1);
        opened["stages"][Stage::DriveOpen as usize]["success"] = serde_json::json!(1);
        assert!(Snapshot::parse(&serde_json::to_vec(&opened).unwrap()).is_err());
        let mut incomplete = original.clone();
        incomplete["accounting_complete"] = serde_json::json!(false);
        Snapshot::parse(&serde_json::to_vec(&incomplete).unwrap()).unwrap();
        incomplete["registered_drives"] = serde_json::json!(2);
        assert!(Snapshot::parse(&serde_json::to_vec(&incomplete).unwrap()).is_err());
        let mut private = original;
        private["private_path"] = serde_json::json!("arbitrary private value");
        assert!(Snapshot::parse(&serde_json::to_vec(&private).unwrap()).is_err());
    }
    #[test]
    fn lazy_v2_running_requires_explicit_nullable_capacity() {
        let mut value = lazy_ready_projection();
        value["current_stage"] = serde_json::json!("configuration");
        value["terminal_outcome"] = serde_json::json!("running");
        value["max_active_drives"] = serde_json::Value::Null;
        value["construction_plans"] = serde_json::json!(0);
        value["configured_partitions"] = serde_json::Value::Null;
        value["planned_drives"] = serde_json::Value::Null;
        value["registered_drives"] = serde_json::json!(0);
        for stage in [Stage::DriveConfig, Stage::DriveRegister] {
            value["stages"][stage as usize]["started"] = serde_json::json!(0);
            value["stages"][stage as usize]["success"] = serde_json::json!(0);
        }
        let parsed = Snapshot::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), value);
        value.as_object_mut().unwrap().remove("max_active_drives");
        assert!(Snapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    #[test]
    fn eager_v1_projection_keeps_its_original_closed_shape() {
        let startup = Startup::new(true, Identity::cli());
        let original = serde_json::to_value(startup.snapshot().unwrap()).unwrap();
        let parsed = Snapshot::parse(&serde_json::to_vec(&original).unwrap()).unwrap();
        assert_eq!(serde_json::to_value(parsed).unwrap(), original);
        assert_eq!(original.as_object().unwrap().len(), 21);
        for key in [
            "construction_mode",
            "max_active_drives",
            "construction_plans",
        ] {
            let mut value = original.clone();
            value[key] = serde_json::Value::Null;
            assert!(
                Snapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err(),
                "v1 accepted {key}"
            );
        }
    }
    #[test]
    fn nullable_projection_fields_are_required_and_incomplete_counts_are_preserved() {
        let startup = Startup::new(true, Identity::cli());
        let original = serde_json::to_value(startup.snapshot().unwrap()).unwrap();
        for key in [
            "worker",
            "cleanup_outcome",
            "configured_partitions",
            "planned_drives",
        ] {
            let mut value = original.clone();
            value.as_object_mut().unwrap().remove(key);
            assert!(
                Snapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err(),
                "missing {key}"
            );
        }
        let mut value = original;
        value["accounting_complete"] = serde_json::json!(false);
        value["stages"][0]["started"] = serde_json::json!(u64::MAX);
        value["stages"][0]["success"] = serde_json::json!(u64::MAX);
        value["stages"][0]["error"] = serde_json::json!(1);
        assert!(
            !Snapshot::parse(&serde_json::to_vec(&value).unwrap())
                .unwrap()
                .accounting_complete
        );
        value["accounting_complete"] = serde_json::json!(true);
        assert!(Snapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        startup.finish_startup(false);
    }
    #[test]
    fn successful_registered_open_keeps_ready_after_observer_failure() {
        let startup = Startup::new(true, Identity::cli());
        startup.plan(1, 1);
        startup.begin(Stage::DriveOpen).finish(true);
        startup.begin(Stage::DriveRegister).finish(true);
        startup.registered();
        startup.publish(&mut |_| Err(io::Error::other("observer failed")));
        startup.finish_startup(true);
        let snapshot = startup.snapshot().unwrap();
        assert_eq!(snapshot.terminal_outcome, Outcome::Ready);
        assert!(!snapshot.accounting_complete);
        assert_eq!((snapshot.open_success, snapshot.registered_drives), (1, 1));
        Snapshot::parse(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
    }
    #[test]
    fn scalar_frame_starts_on_a_fresh_line_and_uses_one_bounded_write() {
        #[derive(Default)]
        struct Sink {
            bytes: Vec<u8>,
            writes: usize,
        }
        impl Write for Sink {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.writes += 1;
                self.bytes.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let startup = Startup::new(true, Identity::cli());
        let mut sink = Sink::default();
        Startup::write_record(&mut sink, &startup.snapshot().unwrap()).unwrap();
        assert_eq!(sink.writes, 1);
        assert!(sink.bytes.starts_with(b"\nstartup_diagnostics "));
        assert!(sink.bytes.ends_with(b"\n"));
        assert!(sink.bytes.len() <= RECORD_LIMIT);
        startup.finish_startup(false);
    }
    #[test]
    fn owned_projection_parses_exact_scalars_and_rejects_private_fields_or_counts() {
        let startup = Startup::new(true, Identity::worker(3, 1));
        startup.plan(2, 3);
        let snapshot = startup.snapshot().unwrap();
        let mut bytes = Vec::new();
        Startup::write_json(&mut bytes, &snapshot).unwrap();
        let parsed = Snapshot::parse(&bytes).unwrap();
        assert_eq!(parsed.worker, Some(3));
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        value["private_path"] = serde_json::json!("must never be projected");
        assert!(Snapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        value.as_object_mut().unwrap().remove("private_path");
        value["open_success"] = serde_json::json!(1);
        assert!(Snapshot::parse(&serde_json::to_vec(&value).unwrap()).is_err());
        assert!(Snapshot::parse(&vec![b' '; RECORD_LIMIT + 1]).is_err());
        startup.finish_startup(false);
    }
    #[test]
    fn failed_and_cancelled_opens_never_look_registered_or_ready() {
        let startup = Startup::new(true, Identity::cli());
        startup.plan(2, 3);
        startup.begin(Stage::DriveOpen).finish(true);
        startup.registered();
        startup.begin(Stage::DriveOpen).finish(false);
        drop(startup.begin(Stage::DriveOpen));
        startup.finish_startup(false);
        let snapshot = startup.snapshot().unwrap();
        assert_eq!(snapshot.open_started, 3);
        assert_eq!(snapshot.open_success, 1);
        assert_eq!(snapshot.open_error, 1);
        assert_eq!(snapshot.open_cancelled, 1);
        assert_eq!(snapshot.open_in_flight, 0);
        assert_eq!(snapshot.registered_drives, 1);
        assert_eq!(snapshot.terminal_outcome, Outcome::Error);
        assert!(!snapshot.accounting_complete);
    }
    #[tokio::test]
    async fn disabled_observation_skips_clocks_sink_and_snapshot() {
        CLOCK_READS.set(0);
        let startup = Startup::new(false, Identity::cli());
        let mut calls = 0;
        let mut sink = |_: &Snapshot| {
            calls += 1;
            Ok(())
        };
        assert_eq!(
            startup
                .observe(Stage::DriveOpen, async { Ok::<_, ()>(17) }, &mut sink)
                .await,
            Ok(17)
        );
        startup.plan(1, 1);
        startup.registered();
        startup.finish_startup(true);
        startup.publish(&mut sink);
        assert!(startup.snapshot().is_none());
        assert_eq!(calls, 0);
        assert_eq!(CLOCK_READS.get(), 0);
        let enabled = Startup::new(true, Identity::cli());
        assert!(enabled.snapshot().is_some());
        assert!(
            CLOCK_READS.get() > 0,
            "probe must observe the enabled clock path"
        );
    }
    #[tokio::test(start_paused = true)]
    async fn pending_open_reports_bounded_live_progress_until_completion() {
        let startup = Startup::new(true, Identity::worker(2, 3));
        startup.plan(1, 1);
        let (send, receive) = tokio::sync::oneshot::channel::<()>();
        let mut records = Vec::new();
        {
            let mut sink = |s: &Snapshot| {
                records.push(s.clone());
                Ok(())
            };
            let operation = startup.observe(
                Stage::DriveOpen,
                async { receive.await.map_err(|_| ()) },
                &mut sink,
            );
            tokio::pin!(operation);
            assert!(futures_util::poll!(&mut operation).is_pending());
            tokio::time::advance(Duration::from_secs(5)).await;
            assert!(futures_util::poll!(&mut operation).is_pending());
            tokio::time::advance(Duration::from_secs(5)).await;
            assert!(futures_util::poll!(&mut operation).is_pending());
            send.send(()).unwrap();
            operation.await.unwrap();
        }
        assert_eq!(records.len(), 3);
        assert!(records.iter().all(|s| s.current_stage == Stage::DriveOpen
            && s.open_in_flight == 1
            && s.open_success == 0));
        assert_eq!(records[2].worker, Some(2));
        assert_eq!(records[2].generation, 3);
        assert!(records[2].current_stage_elapsed_ns >= 10_000_000_000);
        assert_eq!(startup.snapshot().unwrap().open_success, 1);
    }
    #[tokio::test(start_paused = true)]
    async fn cancelled_pending_open_decrements_live_without_success() {
        let startup = Startup::new(true, Identity::cli());
        let mut sink = |_: &Snapshot| Ok(());
        let mut operation = Box::pin(startup.observe(
            Stage::DriveOpen,
            std::future::pending::<Result<(), ()>>(),
            &mut sink,
        ));
        assert!(futures_util::poll!(&mut operation).is_pending());
        drop(operation);
        let snapshot = startup.snapshot().unwrap();
        assert_eq!(snapshot.open_started, 1);
        assert_eq!(snapshot.open_cancelled, 1);
        assert_eq!(snapshot.open_in_flight, 0);
        assert_eq!(snapshot.terminal_outcome, Outcome::Cancelled);
        assert!(!snapshot.accounting_complete);
    }
    #[tokio::test(start_paused = true)]
    async fn startup_duration_freezes_and_cleanup_failure_stays_separate() {
        let startup = Startup::new(true, Identity::cli());
        startup.plan(1, 1);
        let attempt = startup.begin(Stage::DriveOpen);
        tokio::time::advance(Duration::from_secs(2)).await;
        attempt.finish(true);
        startup.registered();
        startup.finish_startup(true);
        let ready = startup.snapshot().unwrap();
        tokio::time::advance(Duration::from_secs(9)).await;
        startup.finish_cleanup(false);
        let cleanup = startup.snapshot().unwrap();
        assert_eq!(ready.terminal_outcome, Outcome::Ready);
        assert_eq!(cleanup.terminal_outcome, Outcome::Ready);
        assert_eq!(cleanup.cleanup_outcome, Some(CleanupOutcome::Error));
        assert_eq!(ready.elapsed_ns, cleanup.elapsed_ns);
        assert_eq!(ready.elapsed_ns, 2_000_000_000);
    }
    #[test]
    fn unregistered_open_and_failed_sink_cannot_claim_complete_readiness() {
        let startup = Startup::new(true, Identity::cli());
        startup.plan(1, 1);
        startup.begin(Stage::DriveOpen).finish(true);
        startup.publish(&mut |_| Err(io::Error::other("secret arbitrary failure")));
        startup.finish_startup(true);
        let snapshot = startup.snapshot().unwrap();
        assert_eq!(snapshot.terminal_outcome, Outcome::Error);
        assert!(!snapshot.accounting_complete);
        let mut output = Vec::new();
        Startup::write_record(&mut output, &snapshot).unwrap();
        assert!(output.len() <= RECORD_LIMIT);
        assert!(!String::from_utf8(output).unwrap().contains("secret"));
    }
}
