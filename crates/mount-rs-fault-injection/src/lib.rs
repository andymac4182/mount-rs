//! Explicit, deterministic fault injection at the mount-rs storage seams.
//!
//! This crate is intentionally outside the normal production dependency graph.
//! A caller must construct a [`FaultPlan`] and pass its [`FaultInjector`] to a
//! wrapper; no global or default plan injects anything. Events contain only
//! operation metadata and error outcomes, never user bytes, paths, URLs, or
//! credentials.
//!
//! These wrappers model provider-boundary behavior only. They do not simulate
//! kernel/VFS, transport, process, service, partition, or power-loss faults.
//!
//! A plan's `seed` is trace metadata, not a scheduler seed: this crate does not
//! randomize or control concurrent task scheduling. For concurrent calls, the
//! trace sequence records the order in which reservations commit to the injector
//! state; replaying that ordering requires the caller to control invocation
//! order or its scheduler separately.

use arc_swap::{ArcSwap, Guard};
use async_trait::async_trait;
use mount_rs_core::storage::{
    BlockId, BlockReconcileReport, BlockStore, ConcurrentBackingId, ConcurrentModeState,
    LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::versioning::VolumeId;
use mount_rs_core::{ErrorCode, FsError, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

pub const COMMIT_UNKNOWN_SYSCALL: &str = "fault-injection:commit-unknown";
pub const COMMIT_UNKNOWN_MESSAGE: &str =
    "fault injection: commit status unknown after successful publish";
const MAX_BUDGET: u32 = 1_000_000;
const MAX_DELAY_MS: u64 = 60_000;

/// Storage boundary selected by a fault rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FaultBoundary {
    Metadata,
    Blocks,
}

impl FaultBoundary {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Blocks => "blocks",
        }
    }
}

impl fmt::Display for FaultBoundary {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Named operation exposed by the storage contracts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FaultOperation {
    Load,
    AcquireWriter,
    RenewWriter,
    ReleaseWriter,
    Publish,
    Flush,
    Put,
    Get,
    Delete,
}

impl FaultOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::AcquireWriter => "acquire-writer",
            Self::RenewWriter => "renew-writer",
            Self::ReleaseWriter => "release-writer",
            Self::Publish => "publish",
            Self::Flush => "flush",
            Self::Put => "put",
            Self::Get => "get",
            Self::Delete => "delete",
        }
    }

    pub const fn full_name(self, boundary: FaultBoundary) -> StringName {
        StringName {
            boundary,
            operation: self,
        }
    }

    const fn valid_for(self, boundary: FaultBoundary) -> bool {
        match boundary {
            FaultBoundary::Metadata => matches!(
                self,
                Self::Load
                    | Self::AcquireWriter
                    | Self::RenewWriter
                    | Self::ReleaseWriter
                    | Self::Publish
                    | Self::Flush
            ),
            FaultBoundary::Blocks => {
                matches!(self, Self::Put | Self::Get | Self::Delete | Self::Flush)
            }
        }
    }
}

impl fmt::Display for FaultOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Small allocation-free operation name formatter used in error syscalls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StringName {
    boundary: FaultBoundary,
    operation: FaultOperation,
}

impl StringName {
    fn into_string(self) -> String {
        format!("fault-injection:{}", self)
    }
}

impl fmt::Display for StringName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.boundary, self.operation)
    }
}

/// Whether a rule is applied before the provider call or after it succeeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FaultPhase {
    Before,
    After,
}

impl fmt::Display for FaultPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Before => "before",
            Self::After => "after",
        })
    }
}

/// Deterministic occurrence selector for one boundary/operation/phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FaultOccurrence {
    Once,
    Nth(u64),
    Every(u64),
}

impl FaultOccurrence {
    fn valid(self) -> bool {
        !matches!(self, Self::Nth(0) | Self::Every(0))
    }

    fn matches(self, occurrence: u64) -> bool {
        match self {
            Self::Once => occurrence == 1,
            Self::Nth(expected) => occurrence == expected,
            Self::Every(period) => occurrence.is_multiple_of(period),
        }
    }
}

/// Fault behavior. `LostAcknowledgment` is intentionally a distinct action:
/// it preserves the possibility that a successful publication committed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultAction {
    Error(ErrorCode),
    LeaseFailure,
    CasConflict,
    LostAcknowledgment,
    DelayMs(u64),
}

/// Redacted outcome retained in a deterministic event trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultOutcome {
    /// The selected rule has been reserved but its wrapper future has not yet
    /// reached a terminal outcome. This can be observed while an operation is
    /// in flight.
    Pending,
    /// The wrapper future was dropped before the selected rule reached a
    /// terminal outcome. The provider may have side-effected before this
    /// cancellation, so callers must reconcile rather than infer rollback.
    Cancelled,
    InjectedError {
        code: ErrorCode,
    },
    LeaseFailure,
    CasConflict,
    CommitUnknown,
    Delayed,
    DelegateError {
        code: ErrorCode,
    },
    InvalidPlan,
}

/// One plan rule. It contains no operation arguments or data values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FaultRule {
    pub boundary: FaultBoundary,
    pub operation: FaultOperation,
    pub phase: FaultPhase,
    pub occurrence: FaultOccurrence,
    pub action: FaultAction,
}

impl FaultRule {
    pub const fn new(
        boundary: FaultBoundary,
        operation: FaultOperation,
        phase: FaultPhase,
        occurrence: FaultOccurrence,
        action: FaultAction,
    ) -> Self {
        Self {
            boundary,
            operation,
            phase,
            occurrence,
            action,
        }
    }
}

/// A bounded, replayable plan. Empty plans are valid and transparent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultPlan {
    pub seed: u64,
    pub budget: u32,
    pub rules: Vec<FaultRule>,
}

impl FaultPlan {
    pub fn new(
        seed: u64,
        budget: u32,
        rules: Vec<FaultRule>,
    ) -> std::result::Result<Self, PlanError> {
        let plan = Self {
            seed,
            budget,
            rules,
        };
        plan.validate()?;
        Ok(plan)
    }

    pub const fn disabled(seed: u64) -> Self {
        Self {
            seed,
            budget: 0,
            rules: Vec::new(),
        }
    }

    pub fn validate(&self) -> std::result::Result<(), PlanError> {
        if self.budget > MAX_BUDGET {
            return Err(PlanError::BudgetTooLarge {
                budget: self.budget,
            });
        }
        if self.budget == 0 && !self.rules.is_empty() {
            return Err(PlanError::ZeroBudgetWithRules);
        }

        let mut selectors = BTreeMap::new();
        for (rule_index, rule) in self.rules.iter().enumerate() {
            if !rule.operation.valid_for(rule.boundary) {
                return Err(PlanError::InvalidBoundary {
                    rule_index,
                    boundary: rule.boundary,
                    operation: rule.operation,
                });
            }
            if !rule.occurrence.valid() {
                return Err(PlanError::InvalidOccurrence { rule_index });
            }
            let selector = (rule.boundary, rule.operation, rule.phase, rule.occurrence);
            if selectors.insert(selector, rule_index).is_some() {
                return Err(PlanError::DuplicateSelector { rule_index });
            }
            match rule.action {
                FaultAction::CasConflict
                    if rule.boundary != FaultBoundary::Metadata
                        || rule.operation != FaultOperation::Publish
                        || rule.phase != FaultPhase::Before =>
                {
                    return Err(PlanError::InvalidAction {
                        rule_index,
                        reason: "CAS conflicts must fail before metadata publish",
                    });
                }
                FaultAction::LeaseFailure
                    if rule.boundary != FaultBoundary::Metadata
                        || !matches!(
                            rule.operation,
                            FaultOperation::AcquireWriter
                                | FaultOperation::RenewWriter
                                | FaultOperation::ReleaseWriter
                                | FaultOperation::Publish
                        ) =>
                {
                    return Err(PlanError::InvalidAction {
                        rule_index,
                        reason: "lease failures require a metadata lease operation",
                    });
                }
                FaultAction::LostAcknowledgment
                    if rule.boundary != FaultBoundary::Metadata
                        || rule.operation != FaultOperation::Publish
                        || rule.phase != FaultPhase::After =>
                {
                    return Err(PlanError::InvalidAction {
                        rule_index,
                        reason: "lost acknowledgments require after metadata publish",
                    });
                }
                FaultAction::DelayMs(milliseconds) if milliseconds > MAX_DELAY_MS => {
                    return Err(PlanError::DelayTooLong { rule_index });
                }
                FaultAction::DelayMs(_) if !cfg!(feature = "tokio-delay") => {
                    return Err(PlanError::DelayFeatureDisabled { rule_index });
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Plan construction failures are deterministic and contain no sensitive data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    ZeroBudgetWithRules,
    BudgetTooLarge {
        budget: u32,
    },
    InvalidBoundary {
        rule_index: usize,
        boundary: FaultBoundary,
        operation: FaultOperation,
    },
    InvalidOccurrence {
        rule_index: usize,
    },
    DuplicateSelector {
        rule_index: usize,
    },
    InvalidAction {
        rule_index: usize,
        reason: &'static str,
    },
    DelayTooLong {
        rule_index: usize,
    },
    DelayFeatureDisabled {
        rule_index: usize,
    },
}

impl fmt::Display for PlanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroBudgetWithRules => {
                formatter.write_str("fault plan has rules but zero budget")
            }
            Self::BudgetTooLarge { budget } => {
                write!(formatter, "fault budget {budget} exceeds bound")
            }
            Self::InvalidBoundary {
                rule_index,
                boundary,
                operation,
            } => write!(
                formatter,
                "rule {rule_index} cannot use {boundary}.{operation}"
            ),
            Self::InvalidOccurrence { rule_index } => {
                write!(
                    formatter,
                    "rule {rule_index} has a zero occurrence selector"
                )
            }
            Self::DuplicateSelector { rule_index } => {
                write!(
                    formatter,
                    "rule {rule_index} duplicates an earlier selector"
                )
            }
            Self::InvalidAction { rule_index, reason } => {
                write!(formatter, "rule {rule_index} has invalid action: {reason}")
            }
            Self::DelayTooLong { rule_index } => {
                write!(formatter, "rule {rule_index} exceeds the bounded delay")
            }
            Self::DelayFeatureDisabled { rule_index } => write!(
                formatter,
                "rule {rule_index} requires the tokio-delay feature"
            ),
        }
    }
}

impl std::error::Error for PlanError {}

/// A single redacted selected-fault event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultEvent {
    pub sequence: u64,
    pub seed: u64,
    pub boundary: FaultBoundary,
    pub operation: FaultOperation,
    pub phase: FaultPhase,
    pub occurrence: u64,
    pub rule_index: usize,
    pub action: FaultAction,
    pub outcome: FaultOutcome,
}

/// Ordered event evidence. Events are sorted by reservation sequence, so a
/// concurrent caller can see the observed reservation ordering. The `seed`
/// labels the run; it does not provide a random schedule or guarantee that an
/// uncontrolled concurrent replay will take the same order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultTrace {
    pub seed: u64,
    pub events: Vec<FaultEvent>,
}

impl FaultTrace {
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

const BOUNDARY_COUNT: usize = 2;
const OPERATION_COUNT: usize = 9;
const PHASE_COUNT: usize = 2;
/// One occurrence counter per boundary/operation/phase. The table is a small
/// fixed array so a state snapshot is a bounded copy, never a map clone.
const SELECTOR_SLOTS: usize = BOUNDARY_COUNT * OPERATION_COUNT * PHASE_COUNT;

#[derive(Debug, Clone, Copy)]
struct Selector {
    boundary: FaultBoundary,
    operation: FaultOperation,
    phase: FaultPhase,
}

impl Selector {
    /// Dense index into the occurrence table. The counts above must cover
    /// every variant; `selector_slots_are_distinct_and_in_range` checks it.
    const fn slot(self) -> usize {
        (self.boundary as usize * OPERATION_COUNT + self.operation as usize) * PHASE_COUNT
            + self.phase as usize
    }
}

#[derive(Debug, Clone, Copy)]
struct Invocation {
    sequence: u64,
    rule_index: usize,
    boundary: FaultBoundary,
    operation: FaultOperation,
    phase: FaultPhase,
    occurrence: u64,
    action: FaultAction,
}

/// One selected-fault event in an immutable, newest-first list. The decision
/// that created it never changes; only the terminal outcome is filled in
/// later, once.
struct EventNode {
    invocation: Invocation,
    /// Unset means [`FaultOutcome::Pending`].
    outcome: OnceLock<FaultOutcome>,
    next: Option<Arc<EventNode>>,
}

impl Drop for EventNode {
    fn drop(&mut self) {
        // Unlink the tail in a loop: the default recursive drop would use one
        // stack frame per event and a full budget is a million events.
        let mut next = self.next.take();
        while let Some(node) = next {
            // `into_inner` yields the node only to the last owner, so a tail
            // still shared with a newer snapshot or a pending guard is left
            // for that owner to unlink.
            next = Arc::into_inner(node).and_then(|mut node| node.next.take());
        }
    }
}

/// Everything a decision reads and writes, replaced as one value so a
/// decision, its sequence number and its event are a single atomic step.
#[derive(Clone)]
struct Snapshot {
    occurrences: [u64; SELECTOR_SLOTS],
    fired: u32,
    next_sequence: u64,
    events: Option<Arc<EventNode>>,
}

struct Engine {
    plan: FaultPlan,
    /// Slots that at least one rule selects. A call on any other slot can
    /// never fire and its occurrence count is never read, so it skips the
    /// state entirely.
    watched: [bool; SELECTOR_SLOTS],
    state: ArcSwap<Snapshot>,
}

/// Shared plan state for one explicitly configured wrapper set.
#[derive(Clone)]
pub struct FaultInjector {
    engine: Arc<Engine>,
}

impl FaultInjector {
    pub fn new(plan: FaultPlan) -> std::result::Result<Self, PlanError> {
        plan.validate()?;
        let mut watched = [false; SELECTOR_SLOTS];
        for rule in &plan.rules {
            let selector = Selector {
                boundary: rule.boundary,
                operation: rule.operation,
                phase: rule.phase,
            };
            watched[selector.slot()] = true;
        }
        Ok(Self {
            engine: Arc::new(Engine {
                plan,
                watched,
                state: ArcSwap::from_pointee(Snapshot {
                    occurrences: [0; SELECTOR_SLOTS],
                    fired: 0,
                    next_sequence: 0,
                    events: None,
                }),
            }),
        })
    }

    pub fn disabled(seed: u64) -> Self {
        Self::new(FaultPlan::disabled(seed)).expect("disabled fault plan is valid")
    }

    pub fn plan(&self) -> FaultPlan {
        self.engine.plan.clone()
    }

    /// Events reserved so far, in sequence order. This reads one published
    /// list head and walks it; it never delays a concurrent decision.
    pub fn trace(&self) -> FaultTrace {
        let seed = self.engine.plan.seed;
        let (head, count) = {
            let state = self.engine.state.load();
            (state.events.clone(), state.fired as usize)
        };
        let mut events = Vec::with_capacity(count);
        let mut cursor = head.as_deref();
        while let Some(node) = cursor {
            let invocation = node.invocation;
            events.push(FaultEvent {
                sequence: invocation.sequence,
                seed,
                boundary: invocation.boundary,
                operation: invocation.operation,
                phase: invocation.phase,
                occurrence: invocation.occurrence,
                rule_index: invocation.rule_index,
                action: invocation.action,
                outcome: node.outcome.get().copied().unwrap_or(FaultOutcome::Pending),
            });
            cursor = node.next.as_deref();
        }
        // The list is newest first and sequence numbers are issued in the
        // same step that links a node, so reversing sorts by sequence.
        events.reverse();
        FaultTrace { seed, events }
    }

    /// Apply a before-operation rule, if selected.
    pub async fn before(&self, boundary: FaultBoundary, operation: FaultOperation) -> Result<()> {
        let invocation = self.prepare(boundary, operation, FaultPhase::Before);
        match invocation {
            None => Ok(()),
            Some((invocation, pending)) => self.apply_before(invocation, pending).await,
        }
    }

    /// Reserve an after-operation rule before invoking `call`, then apply it
    /// to the provider result. Reservation before the await records a pending
    /// event and operation ordering without holding anything across
    /// user/provider code. If this future is cancelled, the event is finalized as
    /// [`FaultOutcome::Cancelled`] rather than disappearing from the trace.
    pub async fn after<T, F, Fut>(
        &self,
        boundary: FaultBoundary,
        operation: FaultOperation,
        call: F,
    ) -> Result<T>
    where
        F: FnOnce() -> Fut + Send,
        Fut: Future<Output = Result<T>> + Send,
    {
        let invocation = self.prepare(boundary, operation, FaultPhase::After);
        let result = call().await;
        match invocation {
            None => result,
            Some((invocation, pending)) => self.apply_after(invocation, pending, result).await,
        }
    }

    /// Count this call and, if a rule selects it, reserve the fault: budget,
    /// sequence number and pending event are published in one
    /// compare-and-swap. The outcome for a given order of committed calls is
    /// the same as if the calls had run one after another.
    fn prepare(
        &self,
        boundary: FaultBoundary,
        operation: FaultOperation,
        phase: FaultPhase,
    ) -> Option<(Invocation, PendingEventGuard)> {
        let engine = &*self.engine;
        let slot = Selector {
            boundary,
            operation,
            phase,
        }
        .slot();
        if !engine.watched[slot] {
            return None;
        }
        let mut current = engine.state.load_full();
        loop {
            // The budget never refills, so once it is spent no occurrence
            // count is read again and there is nothing left to record.
            if current.fired >= engine.plan.budget {
                return None;
            }
            // Everything from here to the swap is recomputed from `current`
            // on a lost race; it has no effect outside `next`.
            let occurrence = current.occurrences[slot].saturating_add(1);
            let mut next = Snapshot::clone(&current);
            next.occurrences[slot] = occurrence;
            let selected = engine
                .plan
                .rules
                .iter()
                .enumerate()
                .find(|(_, rule)| {
                    rule.boundary == boundary
                        && rule.operation == operation
                        && rule.phase == phase
                        && rule.occurrence.matches(occurrence)
                })
                .map(|(rule_index, rule)| {
                    next.fired += 1;
                    next.next_sequence += 1;
                    let invocation = Invocation {
                        sequence: next.next_sequence,
                        rule_index,
                        boundary,
                        operation,
                        phase,
                        occurrence,
                        action: rule.action,
                    };
                    let node = Arc::new(EventNode {
                        invocation,
                        outcome: OnceLock::new(),
                        next: next.events.take(),
                    });
                    next.events = Some(Arc::clone(&node));
                    (invocation, node)
                });
            let previous = engine.state.compare_and_swap(&current, Arc::new(next));
            if Arc::ptr_eq(&previous, &current) {
                return selected.map(|(invocation, node)| {
                    (invocation, PendingEventGuard { node, armed: true })
                });
            }
            current = Guard::into_inner(previous);
        }
    }

    async fn apply_before(&self, invocation: Invocation, pending: PendingEventGuard) -> Result<()> {
        match invocation.action {
            FaultAction::Error(code) => {
                pending.finish(FaultOutcome::InjectedError { code });
                Err(injected_error(
                    invocation.boundary,
                    invocation.operation,
                    code,
                ))
            }
            FaultAction::LeaseFailure => {
                pending.finish(FaultOutcome::LeaseFailure);
                Err(lease_failure_error())
            }
            FaultAction::CasConflict => {
                pending.finish(FaultOutcome::CasConflict);
                Err(cas_conflict_error())
            }
            FaultAction::LostAcknowledgment => {
                pending.finish(FaultOutcome::InvalidPlan);
                Err(invalid_runtime_plan_error())
            }
            FaultAction::DelayMs(milliseconds) => match self.delay(milliseconds).await {
                Ok(()) => {
                    pending.finish(FaultOutcome::Delayed);
                    Ok(())
                }
                Err(error) => {
                    pending.finish(FaultOutcome::DelegateError { code: error.code });
                    Err(error)
                }
            },
        }
    }

    async fn apply_after<T>(
        &self,
        invocation: Invocation,
        pending: PendingEventGuard,
        result: Result<T>,
    ) -> Result<T> {
        let value = match result {
            Err(error) => {
                pending.finish(FaultOutcome::DelegateError { code: error.code });
                return Err(error);
            }
            Ok(value) => value,
        };
        match invocation.action {
            FaultAction::Error(code) => {
                pending.finish(FaultOutcome::InjectedError { code });
                Err(injected_error(
                    invocation.boundary,
                    invocation.operation,
                    code,
                ))
            }
            FaultAction::LeaseFailure => {
                pending.finish(FaultOutcome::LeaseFailure);
                Err(lease_failure_error())
            }
            FaultAction::CasConflict => {
                pending.finish(FaultOutcome::CasConflict);
                Err(cas_conflict_error())
            }
            FaultAction::LostAcknowledgment => {
                pending.finish(FaultOutcome::CommitUnknown);
                Err(commit_unknown_error())
            }
            FaultAction::DelayMs(milliseconds) => match self.delay(milliseconds).await {
                Ok(()) => {
                    pending.finish(FaultOutcome::Delayed);
                    Ok(value)
                }
                Err(error) => {
                    pending.finish(FaultOutcome::DelegateError { code: error.code });
                    Err(error)
                }
            },
        }
    }

    async fn delay(&self, milliseconds: u64) -> Result<()> {
        #[cfg(feature = "tokio-delay")]
        {
            tokio::time::sleep(Duration::from_millis(milliseconds)).await;
            Ok(())
        }
        #[cfg(not(feature = "tokio-delay"))]
        {
            let _ = milliseconds;
            Err(FsError::new(ErrorCode::Enotsup).with_syscall("fault-injection delay"))
        }
    }
}

/// Owns the terminal outcome of one reserved event. Dropping it unfinished
/// (the wrapper future was cancelled) records [`FaultOutcome::Cancelled`].
struct PendingEventGuard {
    node: Arc<EventNode>,
    armed: bool,
}

impl PendingEventGuard {
    fn finish(mut self, outcome: FaultOutcome) {
        // This guard is the only writer of its node's outcome.
        let _ = self.node.outcome.set(outcome);
        self.armed = false;
    }
}

impl Drop for PendingEventGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.node.outcome.set(FaultOutcome::Cancelled);
        }
    }
}

fn injected_error(boundary: FaultBoundary, operation: FaultOperation, code: ErrorCode) -> FsError {
    FsError::new(code).with_syscall(boundary_operation(boundary, operation).into_string())
}

fn lease_failure_error() -> FsError {
    FsError::new(ErrorCode::Estale).with_syscall("fault-injection:lease")
}

fn cas_conflict_error() -> FsError {
    FsError::new(ErrorCode::Eagain).with_syscall("fault-injection:cas")
}

fn commit_unknown_error() -> FsError {
    FsError::new(ErrorCode::Eio)
        .with_syscall(COMMIT_UNKNOWN_SYSCALL)
        .with_message(COMMIT_UNKNOWN_MESSAGE)
}

fn invalid_runtime_plan_error() -> FsError {
    FsError::new(ErrorCode::Enotsup).with_syscall("fault-injection invalid action")
}

fn boundary_operation(boundary: FaultBoundary, operation: FaultOperation) -> StringName {
    operation.full_name(boundary)
}

pub fn is_commit_unknown(error: &FsError) -> bool {
    error.code == ErrorCode::Eio && error.syscall.as_deref() == Some(COMMIT_UNKNOWN_SYSCALL)
}

/// MetadataStore decorator. Construct it only with an explicit injector.
#[derive(Clone)]
pub struct FaultMetadataStore<S> {
    inner: S,
    injector: FaultInjector,
}

impl<S> FaultMetadataStore<S> {
    pub fn new(inner: S, injector: FaultInjector) -> Self {
        Self { inner, injector }
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn injector(&self) -> &FaultInjector {
        &self.injector
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

#[async_trait]
impl<S> MetadataStore for FaultMetadataStore<S>
where
    S: MetadataStore,
{
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Load)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Load, || {
                self.inner.load()
            })
            .await
    }

    async fn load_if_changed(&self, known_revision: u64) -> Result<Option<LoadedMetadata>> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Load)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Load, || {
                self.inner.load_if_changed(known_revision)
            })
            .await
    }

    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        self.inner.concurrent_mode_state().await
    }

    async fn preflight_new_bound_mode(&self) -> Result<()> {
        self.inner.preflight_new_bound_mode().await
    }

    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        self.inner.prepare_bound_concurrent_mode(backing).await
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::AcquireWriter)
            .await?;
        self.injector
            .after(
                FaultBoundary::Metadata,
                FaultOperation::AcquireWriter,
                || self.inner.acquire_writer(owner, ttl),
            )
            .await
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::RenewWriter)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::RenewWriter, || {
                self.inner.renew_writer(lease, ttl)
            })
            .await
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::ReleaseWriter)
            .await?;
        self.injector
            .after(
                FaultBoundary::Metadata,
                FaultOperation::ReleaseWriter,
                || self.inner.release_writer(lease),
            )
            .await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Publish)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Publish, || {
                self.inner.publish(expected_revision, lease, namespace)
            })
            .await
    }

    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Publish)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Publish, || {
                self.inner
                    .publish_bound_if_revision(backing, expected_revision, namespace)
            })
            .await
    }

    async fn migrate_mrc1_to_bound_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        self.inner
            .migrate_mrc1_to_bound_mode(backing, expected_revision)
            .await
    }

    async fn preflight_mrc1_to_bound_mode(&self, expected_revision: u64) -> Result<()> {
        self.inner
            .preflight_mrc1_to_bound_mode(expected_revision)
            .await
    }

    async fn preflight_trusted_unstamped_mrc1(
        &self,
        expected_revision: u64,
        expected_volume: VolumeId,
    ) -> Result<()> {
        self.inner
            .preflight_trusted_unstamped_mrc1(expected_revision, expected_volume)
            .await
    }

    async fn migrate_trusted_unstamped_mrc1(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        expected_volume: VolumeId,
    ) -> Result<()> {
        self.inner
            .migrate_trusted_unstamped_mrc1(backing, expected_revision, expected_volume)
            .await
    }

    async fn flush(&self) -> Result<()> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Flush)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Flush, || {
                self.inner.flush()
            })
            .await
    }
}

/// BlockStore decorator. Immutable block semantics are preserved: the wrapper
/// returns an error; it never fabricates a short successful block write.
#[derive(Clone)]
pub struct FaultBlockStore<S> {
    inner: S,
    injector: FaultInjector,
}

impl<S> FaultBlockStore<S> {
    pub fn new(inner: S, injector: FaultInjector) -> Self {
        Self { inner, injector }
    }

    pub fn inner(&self) -> &S {
        &self.inner
    }

    pub fn injector(&self) -> &FaultInjector {
        &self.injector
    }

    pub fn into_inner(self) -> S {
        self.inner
    }
}

#[async_trait]
impl<S> BlockStore for FaultBlockStore<S>
where
    S: BlockStore,
{
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        self.inner.prepare_concurrent_backing().await
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        self.inner.verify_concurrent_backing(expected).await
    }

    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.injector
            .before(FaultBoundary::Blocks, FaultOperation::Get)
            .await?;
        self.injector
            .after(FaultBoundary::Blocks, FaultOperation::Get, || {
                self.inner.get_for_migration(id)
            })
            .await
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.injector
            .before(FaultBoundary::Blocks, FaultOperation::Put)
            .await?;
        self.injector
            .after(FaultBoundary::Blocks, FaultOperation::Put, || {
                self.inner.put(bytes)
            })
            .await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.injector
            .before(FaultBoundary::Blocks, FaultOperation::Get)
            .await?;
        self.injector
            .after(FaultBoundary::Blocks, FaultOperation::Get, || {
                self.inner.get(id)
            })
            .await
    }

    async fn flush(&self) -> Result<()> {
        self.injector
            .before(FaultBoundary::Blocks, FaultOperation::Flush)
            .await?;
        self.injector
            .after(FaultBoundary::Blocks, FaultOperation::Flush, || {
                self.inner.flush()
            })
            .await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.injector
            .before(FaultBoundary::Blocks, FaultOperation::Delete)
            .await?;
        self.injector
            .after(FaultBoundary::Blocks, FaultOperation::Delete, || {
                self.inner.delete(id)
            })
            .await
    }

    async fn reconcile(
        &self,
        live: &BTreeSet<BlockId>,
        grace: Duration,
    ) -> Result<BlockReconcileReport> {
        // Reconciliation is an explicit operator action rather than a normal
        // data-plane operation, so fault plans do not silently change its
        // safety boundary. The wrapped provider still retains its capability.
        self.inner.reconcile(live, grace).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::thread;

    const BOUNDARIES: [FaultBoundary; BOUNDARY_COUNT] =
        [FaultBoundary::Metadata, FaultBoundary::Blocks];
    const OPERATIONS: [FaultOperation; OPERATION_COUNT] = [
        FaultOperation::Load,
        FaultOperation::AcquireWriter,
        FaultOperation::RenewWriter,
        FaultOperation::ReleaseWriter,
        FaultOperation::Publish,
        FaultOperation::Flush,
        FaultOperation::Put,
        FaultOperation::Get,
        FaultOperation::Delete,
    ];
    const PHASES: [FaultPhase; PHASE_COUNT] = [FaultPhase::Before, FaultPhase::After];

    fn rule(
        boundary: FaultBoundary,
        operation: FaultOperation,
        phase: FaultPhase,
        occurrence: FaultOccurrence,
    ) -> FaultRule {
        FaultRule::new(
            boundary,
            operation,
            phase,
            occurrence,
            FaultAction::Error(ErrorCode::Eio),
        )
    }

    #[test]
    fn selector_slots_are_distinct_and_in_range() {
        let mut seen = BTreeSet::new();
        for boundary in BOUNDARIES {
            for operation in OPERATIONS {
                for phase in PHASES {
                    let slot = Selector {
                        boundary,
                        operation,
                        phase,
                    }
                    .slot();
                    assert!(slot < SELECTOR_SLOTS);
                    assert!(seen.insert(slot), "slot {slot} is used twice");
                }
            }
        }
        assert_eq!(seen.len(), SELECTOR_SLOTS);
    }

    /// Hammer `prepare` from several threads over three ruled selectors and
    /// one unruled one, then check what a single lock used to guarantee.
    /// Returns every reservation the threads observed plus the final trace.
    fn hammer(
        injector: &FaultInjector,
        threads: usize,
        rounds: usize,
    ) -> (Vec<Invocation>, FaultTrace) {
        let calls = [
            (
                FaultBoundary::Metadata,
                FaultOperation::Load,
                FaultPhase::Before,
            ),
            (
                FaultBoundary::Blocks,
                FaultOperation::Put,
                FaultPhase::Before,
            ),
            (
                FaultBoundary::Blocks,
                FaultOperation::Get,
                FaultPhase::After,
            ),
            (
                FaultBoundary::Blocks,
                FaultOperation::Delete,
                FaultPhase::Before,
            ),
        ];
        let barrier = Barrier::new(threads);
        let mut reserved = Vec::new();
        thread::scope(|scope| {
            let workers: Vec<_> = (0..threads)
                .map(|worker| {
                    let barrier = &barrier;
                    scope.spawn(move || {
                        let mut mine = Vec::new();
                        barrier.wait();
                        for round in 0..rounds {
                            for (boundary, operation, phase) in calls {
                                let Some((invocation, pending)) =
                                    injector.prepare(boundary, operation, phase)
                                else {
                                    continue;
                                };
                                // Alternate a finished outcome with a
                                // dropped (cancelled) reservation.
                                if (worker + round) % 2 == 0 {
                                    pending.finish(FaultOutcome::Delayed);
                                } else {
                                    drop(pending);
                                }
                                mine.push(invocation);
                            }
                        }
                        mine
                    })
                })
                .collect();
            for worker in workers {
                reserved.extend(worker.join().expect("worker panicked"));
            }
        });
        (reserved, injector.trace())
    }

    fn assert_serial_equivalent(
        plan: &FaultPlan,
        reserved: &[Invocation],
        trace: &FaultTrace,
        expected_fired: usize,
    ) {
        assert_eq!(reserved.len(), expected_fired);
        assert_eq!(trace.events.len(), expected_fired);

        // Every sequence number 1..=fired is issued exactly once.
        let sequences: BTreeSet<u64> = reserved.iter().map(|inv| inv.sequence).collect();
        assert_eq!(sequences.len(), reserved.len());
        assert_eq!(
            sequences.first().copied(),
            (expected_fired > 0).then_some(1)
        );
        assert_eq!(
            sequences.last().copied(),
            (expected_fired > 0).then_some(expected_fired as u64)
        );

        // The trace holds exactly one event per decision, in sequence order,
        // with a terminal outcome, and agrees with what the caller was told.
        let mut by_sequence: Vec<&Invocation> = reserved.iter().collect();
        by_sequence.sort_by_key(|inv| inv.sequence);
        for (index, (event, invocation)) in trace.events.iter().zip(by_sequence).enumerate() {
            assert_eq!(event.sequence, index as u64 + 1);
            assert_eq!(event.sequence, invocation.sequence);
            assert_eq!(event.rule_index, invocation.rule_index);
            assert_eq!(event.occurrence, invocation.occurrence);
            assert_eq!(event.boundary, invocation.boundary);
            assert_eq!(event.operation, invocation.operation);
            assert_eq!(event.phase, invocation.phase);
            assert!(
                matches!(
                    event.outcome,
                    FaultOutcome::Delayed | FaultOutcome::Cancelled
                ),
                "event {} left {:?}",
                event.sequence,
                event.outcome
            );
        }

        // Serial equivalence: within one rule, occurrences fire in order with
        // no gaps (a later matching occurrence never fires while an earlier
        // one is skipped), and each fired occurrence matches its rule.
        for (rule_index, rule) in plan.rules.iter().enumerate() {
            let fired: Vec<u64> = trace
                .events
                .iter()
                .filter(|event| event.rule_index == rule_index)
                .map(|event| event.occurrence)
                .collect();
            let expected: Vec<u64> = match rule.occurrence {
                FaultOccurrence::Once => vec![1],
                FaultOccurrence::Nth(nth) => vec![nth],
                FaultOccurrence::Every(period) => {
                    (1..=fired.len() as u64).map(|n| n * period).collect()
                }
            };
            assert_eq!(
                fired,
                expected[..fired.len().min(expected.len())],
                "rule {rule_index} fired out of order or with gaps"
            );
            assert!(fired.len() <= expected.len());
        }
    }

    fn concurrent_plan(budget: u32) -> FaultPlan {
        FaultPlan::new(
            7,
            budget,
            vec![
                rule(
                    FaultBoundary::Metadata,
                    FaultOperation::Load,
                    FaultPhase::Before,
                    FaultOccurrence::Every(1),
                ),
                rule(
                    FaultBoundary::Blocks,
                    FaultOperation::Put,
                    FaultPhase::Before,
                    FaultOccurrence::Every(3),
                ),
                rule(
                    FaultBoundary::Blocks,
                    FaultOperation::Get,
                    FaultPhase::After,
                    FaultOccurrence::Nth(7),
                ),
            ],
        )
        .expect("valid plan")
    }

    #[test]
    fn concurrent_decisions_stop_exactly_at_the_budget() {
        let plan = concurrent_plan(1_000);
        let injector = FaultInjector::new(plan.clone()).expect("valid plan");
        let (reserved, trace) = hammer(&injector, 8, 2_000);
        assert_serial_equivalent(&plan, &reserved, &trace, 1_000);
        // Once the budget is spent nothing else fires.
        assert!(
            injector
                .prepare(
                    FaultBoundary::Metadata,
                    FaultOperation::Load,
                    FaultPhase::Before
                )
                .is_none()
        );
        assert_eq!(injector.trace().events.len(), 1_000);
    }

    #[test]
    fn concurrent_decisions_fire_every_matching_occurrence_once() {
        let threads = 8;
        let rounds = 1_500;
        let per_selector = threads * rounds;
        let plan = concurrent_plan(MAX_BUDGET);
        let injector = FaultInjector::new(plan.clone()).expect("valid plan");
        let (reserved, trace) = hammer(&injector, threads, rounds);
        // Every(1) fires on each call, Every(3) on each third, Nth(7) once.
        let expected = per_selector + per_selector / 3 + 1;
        assert_serial_equivalent(&plan, &reserved, &trace, expected);
        let fired_by_rule = |rule_index: usize| {
            trace
                .events
                .iter()
                .filter(|event| event.rule_index == rule_index)
                .count()
        };
        assert_eq!(fired_by_rule(0), per_selector);
        assert_eq!(fired_by_rule(1), per_selector / 3);
        assert_eq!(fired_by_rule(2), 1);
    }

    #[test]
    fn large_trace_does_not_block_decisions_or_overflow_on_drop() {
        const PREFILL: usize = 200_000;
        const EXTRA_PER_THREAD: usize = 500;
        const DECIDERS: usize = 4;
        let plan = FaultPlan::new(
            1,
            MAX_BUDGET,
            vec![rule(
                FaultBoundary::Blocks,
                FaultOperation::Put,
                FaultPhase::Before,
                FaultOccurrence::Every(1),
            )],
        )
        .expect("valid plan");
        let injector = FaultInjector::new(plan).expect("valid plan");
        let decide = |injector: &FaultInjector| {
            let (_, pending) = injector
                .prepare(
                    FaultBoundary::Blocks,
                    FaultOperation::Put,
                    FaultPhase::Before,
                )
                .expect("budget remains");
            pending.finish(FaultOutcome::Delayed);
        };
        for _ in 0..PREFILL {
            decide(&injector);
        }

        let stop = AtomicBool::new(false);
        let traces_taken = thread::scope(|scope| {
            let tracer = scope.spawn(|| {
                let mut taken = 0usize;
                let mut last_len = PREFILL;
                loop {
                    let done = stop.load(Ordering::SeqCst);
                    let trace = injector.trace();
                    assert!(trace.events.len() >= last_len, "trace went backwards");
                    last_len = trace.events.len();
                    for (index, event) in trace.events.iter().enumerate() {
                        assert_eq!(event.sequence, index as u64 + 1);
                    }
                    taken += 1;
                    if done {
                        return taken;
                    }
                }
            });
            let deciders: Vec<_> = (0..DECIDERS)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..EXTRA_PER_THREAD {
                            decide(&injector);
                        }
                    })
                })
                .collect();
            for decider in deciders {
                decider.join().expect("decider panicked");
            }
            stop.store(true, Ordering::SeqCst);
            tracer.join().expect("tracer panicked")
        });
        assert!(traces_taken >= 1);
        let total = PREFILL + DECIDERS * EXTRA_PER_THREAD;
        assert_eq!(injector.trace().events.len(), total);

        // Drop the whole list on a stack far too small for one frame per
        // event; a recursive drop of 200k nodes would overflow it.
        thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || drop(injector))
            .expect("spawn drop thread")
            .join()
            .expect("dropping a large event list overflowed the stack");
    }
}
