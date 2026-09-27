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
//! trace sequence records the order in which reservations acquire the injector
//! lock; replaying that ordering requires the caller to control invocation
//! order or its scheduler separately.

use async_trait::async_trait;
use mount_rs_core::storage::compact::{
    CompactInodeCapability, CompactPublication, CompactSnapshot, CompactStructuralDelta,
    LoadedCompactInode, PhysicalInodeIdentity,
};
use mount_rs_core::storage::{
    BlockId, BlockReconcileReport, BlockStore, ConcurrentBackingId, ConcurrentModeState,
    InodeModeState, LoadedMetadata, MetadataStore, Namespace, NodeMetadata, WriterLease,
};
use mount_rs_core::versioning::VolumeId;
use mount_rs_core::{ErrorCode, FsError, Result};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};
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
                FaultAction::Error(ErrorCode::Eagain | ErrorCode::Estale)
                    if rule.boundary == FaultBoundary::Metadata
                        && rule.operation == FaultOperation::Publish
                        && rule.phase == FaultPhase::After =>
                {
                    return Err(PlanError::InvalidAction {
                        rule_index,
                        reason: "a successful metadata publish cannot report a definite conflict or stale lease",
                    });
                }
                FaultAction::LeaseFailure
                    if rule.boundary == FaultBoundary::Metadata
                        && rule.operation == FaultOperation::Publish
                        && rule.phase == FaultPhase::After =>
                {
                    return Err(PlanError::InvalidAction {
                        rule_index,
                        reason: "a successful metadata publish cannot report a definite lease failure",
                    });
                }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Selector {
    boundary: FaultBoundary,
    operation: FaultOperation,
    phase: FaultPhase,
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

struct EngineState {
    plan: FaultPlan,
    occurrences: BTreeMap<Selector, u64>,
    fired: u32,
    next_sequence: u64,
    events: Vec<FaultEvent>,
}

/// Shared plan state for one explicitly configured wrapper set.
#[derive(Clone)]
pub struct FaultInjector {
    state: Arc<Mutex<EngineState>>,
}

impl FaultInjector {
    pub fn new(plan: FaultPlan) -> std::result::Result<Self, PlanError> {
        plan.validate()?;
        Ok(Self {
            state: Arc::new(Mutex::new(EngineState {
                plan,
                occurrences: BTreeMap::new(),
                fired: 0,
                next_sequence: 0,
                events: Vec::new(),
            })),
        })
    }

    pub fn disabled(seed: u64) -> Self {
        Self::new(FaultPlan::disabled(seed)).expect("disabled fault plan is valid")
    }

    pub fn plan(&self) -> FaultPlan {
        self.state
            .lock()
            .expect("fault injector state lock poisoned")
            .plan
            .clone()
    }

    pub fn trace(&self) -> FaultTrace {
        let state = self
            .state
            .lock()
            .expect("fault injector state lock poisoned");
        let mut events = state.events.clone();
        events.sort_by_key(|event| event.sequence);
        FaultTrace {
            seed: state.plan.seed,
            events,
        }
    }

    /// Apply a before-operation rule, if selected.
    pub async fn before(&self, boundary: FaultBoundary, operation: FaultOperation) -> Result<()> {
        let invocation = self.prepare(boundary, operation, FaultPhase::Before)?;
        match invocation {
            None => Ok(()),
            Some((invocation, pending)) => self.apply_before(invocation, pending).await,
        }
    }

    /// Reserve an after-operation rule before invoking `call`, then apply it
    /// to the provider result. Reservation before the await records a pending
    /// event and operation ordering without holding a mutex across user/provider
    /// code. If this future is cancelled, the event is finalized as
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
        let invocation = self.prepare(boundary, operation, FaultPhase::After)?;
        let result = call().await;
        match invocation {
            None => result,
            Some((invocation, pending)) => self.apply_after(invocation, pending, result).await,
        }
    }

    fn prepare(
        &self,
        boundary: FaultBoundary,
        operation: FaultOperation,
        phase: FaultPhase,
    ) -> Result<Option<(Invocation, PendingEventGuard)>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("fault-injection state"))?;
        let selector = Selector {
            boundary,
            operation,
            phase,
        };
        let occurrence = {
            let count = state.occurrences.entry(selector).or_insert(0);
            *count = count.saturating_add(1);
            *count
        };
        if state.fired >= state.plan.budget {
            return Ok(None);
        }
        let Some((rule_index, action)) = state
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
            .map(|(rule_index, rule)| (rule_index, rule.action))
        else {
            return Ok(None);
        };
        state.fired += 1;
        state.next_sequence += 1;
        let invocation = Invocation {
            sequence: state.next_sequence,
            rule_index,
            boundary,
            operation,
            phase,
            occurrence,
            action,
        };
        let seed = state.plan.seed;
        state.events.push(FaultEvent {
            sequence: invocation.sequence,
            seed,
            boundary: invocation.boundary,
            operation: invocation.operation,
            phase: invocation.phase,
            occurrence: invocation.occurrence,
            rule_index: invocation.rule_index,
            action: invocation.action,
            outcome: FaultOutcome::Pending,
        });
        Ok(Some((
            invocation,
            PendingEventGuard {
                injector: self.clone(),
                sequence: invocation.sequence,
                armed: true,
            },
        )))
    }

    async fn apply_before(&self, invocation: Invocation, pending: PendingEventGuard) -> Result<()> {
        match invocation.action {
            FaultAction::Error(code) => {
                pending.finish(FaultOutcome::InjectedError { code })?;
                Err(injected_error(
                    invocation.boundary,
                    invocation.operation,
                    code,
                ))
            }
            FaultAction::LeaseFailure => {
                pending.finish(FaultOutcome::LeaseFailure)?;
                Err(lease_failure_error())
            }
            FaultAction::CasConflict => {
                pending.finish(FaultOutcome::CasConflict)?;
                Err(cas_conflict_error())
            }
            FaultAction::LostAcknowledgment => {
                pending.finish(FaultOutcome::InvalidPlan)?;
                Err(invalid_runtime_plan_error())
            }
            FaultAction::DelayMs(milliseconds) => match self.delay(milliseconds).await {
                Ok(()) => {
                    pending.finish(FaultOutcome::Delayed)?;
                    Ok(())
                }
                Err(error) => {
                    pending.finish(FaultOutcome::DelegateError { code: error.code })?;
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
                pending.finish(FaultOutcome::DelegateError { code: error.code })?;
                return Err(error);
            }
            Ok(value) => value,
        };
        match invocation.action {
            FaultAction::Error(code) => {
                pending.finish(FaultOutcome::InjectedError { code })?;
                Err(injected_error(
                    invocation.boundary,
                    invocation.operation,
                    code,
                ))
            }
            FaultAction::LeaseFailure => {
                pending.finish(FaultOutcome::LeaseFailure)?;
                Err(lease_failure_error())
            }
            FaultAction::CasConflict => {
                pending.finish(FaultOutcome::CasConflict)?;
                Err(cas_conflict_error())
            }
            FaultAction::LostAcknowledgment => {
                pending.finish(FaultOutcome::CommitUnknown)?;
                Err(commit_unknown_error())
            }
            FaultAction::DelayMs(milliseconds) => match self.delay(milliseconds).await {
                Ok(()) => {
                    pending.finish(FaultOutcome::Delayed)?;
                    Ok(value)
                }
                Err(error) => {
                    pending.finish(FaultOutcome::DelegateError { code: error.code })?;
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

    fn finish(&self, sequence: u64, outcome: FaultOutcome) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("fault-injection state"))?;
        let Some(event) = state
            .events
            .iter_mut()
            .find(|event| event.sequence == sequence)
        else {
            return Err(FsError::new(ErrorCode::Eio).with_syscall("fault-injection event"));
        };
        event.outcome = outcome;
        Ok(())
    }

    fn cancel(&self, sequence: u64) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if let Some(event) = state
            .events
            .iter_mut()
            .find(|event| event.sequence == sequence)
        {
            event.outcome = FaultOutcome::Cancelled;
        }
    }
}

struct PendingEventGuard {
    injector: FaultInjector,
    sequence: u64,
    armed: bool,
}

impl PendingEventGuard {
    fn finish(mut self, outcome: FaultOutcome) -> Result<()> {
        let result = self.injector.finish(self.sequence, outcome);
        if result.is_ok() {
            self.armed = false;
        }
        result
    }
}

impl Drop for PendingEventGuard {
    fn drop(&mut self) {
        if self.armed {
            self.injector.cancel(self.sequence);
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
    fn compact_inode_capability(&self) -> CompactInodeCapability {
        self.inner.compact_inode_capability()
    }

    async fn compact_inode_mode_state(&self) -> Result<Option<InodeModeState>> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Load)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Load, || {
                self.inner.compact_inode_mode_state()
            })
            .await
    }

    async fn prepare_compact_inode_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Publish)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Publish, || {
                self.inner
                    .prepare_compact_inode_mode(backing, expected_revision)
            })
            .await
    }

    async fn load_compact_snapshot(&self, backing: ConcurrentBackingId) -> Result<CompactSnapshot> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Load)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Load, || {
                self.inner.load_compact_snapshot(backing)
            })
            .await
    }

    async fn load_compact_inode(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
    ) -> Result<LoadedCompactInode> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Load)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Load, || {
                self.inner.load_compact_inode(backing, inode)
            })
            .await
    }

    async fn publish_compact_inode(
        &self,
        backing: ConcurrentBackingId,
        inode: u64,
        generation: u64,
        expected: PhysicalInodeIdentity,
        node: NodeMetadata,
    ) -> Result<LoadedCompactInode> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Publish)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Publish, || {
                self.inner
                    .publish_compact_inode(backing, inode, generation, expected, node)
            })
            .await
    }

    async fn publish_compact_structure(
        &self,
        delta: &CompactStructuralDelta,
    ) -> Result<CompactPublication> {
        self.injector
            .before(FaultBoundary::Metadata, FaultOperation::Publish)
            .await?;
        self.injector
            .after(FaultBoundary::Metadata, FaultOperation::Publish, || {
                self.inner.publish_compact_structure(delta)
            })
            .await
    }

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

#[cfg(test)]
mod compact_wrapper_tests {
    use super::*;
    use mount_rs_core::chunking::ChunkerConfig;
    use mount_rs_core::storage::{DirectoryEntry, FileLayout, NodeData, NodeMetadata, compact::*};
    use mount_rs_sqlite::SqliteMetadataStore;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct PendingCompactStore {
        backing: ConcurrentBackingId,
        gate: Arc<AtomicBool>,
        calls: Arc<AtomicUsize>,
    }

    struct ModeProbeStore {
        state: InodeModeState,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl MetadataStore for ModeProbeStore {
        fn compact_inode_capability(&self) -> CompactInodeCapability {
            CompactInodeCapability::V1
        }
        async fn compact_inode_mode_state(&self) -> Result<Option<InodeModeState>> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Ok(Some(self.state))
        }
        fn durable(&self) -> bool {
            true
        }
        async fn load(&self) -> Result<LoadedMetadata> {
            unreachable!()
        }
        async fn acquire_writer(&self, _: &str, _: Duration) -> Result<WriterLease> {
            unreachable!()
        }
        async fn renew_writer(&self, _: &WriterLease, _: Duration) -> Result<WriterLease> {
            unreachable!()
        }
        async fn release_writer(&self, _: &WriterLease) -> Result<()> {
            unreachable!()
        }
        async fn publish(&self, _: u64, _: &WriterLease, _: Namespace) -> Result<u64> {
            unreachable!()
        }
        async fn flush(&self) -> Result<()> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn compact_mode_faults_preserve_exact_state_and_before_skips_delegate() {
        let state = InodeModeState {
            backing: ConcurrentBackingId::from_bytes([0xc9; 16]).unwrap(),
            structural_generation: 9,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let before = FaultMetadataStore::new(
            ModeProbeStore {
                state,
                calls: calls.clone(),
            },
            injector(
                FaultPhase::Before,
                FaultOperation::Load,
                FaultAction::Error(ErrorCode::Eio),
            ),
        );
        assert_eq!(
            before.compact_inode_mode_state().await.unwrap_err().code,
            ErrorCode::Eio
        );
        assert_eq!(calls.load(Ordering::Acquire), 0);
        let after = FaultMetadataStore::new(
            ModeProbeStore {
                state,
                calls: calls.clone(),
            },
            injector(
                FaultPhase::After,
                FaultOperation::Load,
                FaultAction::Error(ErrorCode::Eio),
            ),
        );
        assert_eq!(
            after.compact_inode_mode_state().await.unwrap_err().code,
            ErrorCode::Eio
        );
        assert_eq!(calls.load(Ordering::Acquire), 1);
        let plain = FaultMetadataStore::new(
            ModeProbeStore {
                state,
                calls: calls.clone(),
            },
            FaultInjector::disabled(99),
        );
        assert_eq!(plain.compact_inode_mode_state().await.unwrap(), Some(state));
        assert_eq!(calls.load(Ordering::Acquire), 2);
    }

    #[async_trait]
    impl MetadataStore for PendingCompactStore {
        fn compact_inode_capability(&self) -> CompactInodeCapability {
            CompactInodeCapability::V1
        }

        async fn prepare_compact_inode_mode(
            &self,
            backing: ConcurrentBackingId,
            revision: u64,
        ) -> Result<()> {
            assert_eq!((backing, revision), (self.backing, 37));
            self.calls.fetch_add(1, Ordering::AcqRel);
            std::future::poll_fn(|_| {
                if self.gate.load(Ordering::Acquire) {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            })
            .await;
            Err(FsError::new(ErrorCode::Eperm)
                .with_syscall("compact-probe:prepare")
                .with_message("delegate refused compact prepare"))
        }

        fn durable(&self) -> bool {
            true
        }
        async fn load(&self) -> Result<LoadedMetadata> {
            unreachable!("alternate load")
        }
        async fn acquire_writer(&self, _: &str, _: Duration) -> Result<WriterLease> {
            unreachable!("alternate acquire")
        }
        async fn renew_writer(&self, _: &WriterLease, _: Duration) -> Result<WriterLease> {
            unreachable!("alternate renew")
        }
        async fn release_writer(&self, _: &WriterLease) -> Result<()> {
            unreachable!("alternate release")
        }
        async fn publish(&self, _: u64, _: &WriterLease, _: Namespace) -> Result<u64> {
            unreachable!("alternate publish")
        }
        async fn flush(&self) -> Result<()> {
            unreachable!("alternate flush")
        }
    }

    #[tokio::test]
    async fn fault_compact_prepare_waits_for_one_delegate_and_preserves_its_error() {
        use std::future::Future;
        use std::task::{Context, Poll, Waker};

        let backing = ConcurrentBackingId::from_bytes([0xc9; 16]).unwrap();
        let gate = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let store = FaultMetadataStore::new(
            PendingCompactStore {
                backing,
                gate: gate.clone(),
                calls: calls.clone(),
            },
            injector(
                FaultPhase::After,
                FaultOperation::Publish,
                FaultAction::Error(ErrorCode::Eio),
            ),
        );
        assert_eq!(store.compact_inode_capability(), CompactInodeCapability::V1);
        let mut call = Box::pin(store.prepare_compact_inode_mode(backing, 37));
        let mut context = Context::from_waker(Waker::noop());
        assert!(matches!(call.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert!(matches!(call.as_mut().poll(&mut context), Poll::Pending));
        assert_eq!(calls.load(Ordering::Acquire), 1);
        gate.store(true, Ordering::Release);
        let Poll::Ready(Err(error)) = call.as_mut().poll(&mut context) else {
            panic!("delegate error must be returned after release");
        };
        assert_eq!(error.code, ErrorCode::Eperm);
        assert_eq!(error.syscall.as_deref(), Some("compact-probe:prepare"));
        assert_eq!(error.to_string(), "delegate refused compact prepare");
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(
            store.injector().trace().events[0].outcome,
            FaultOutcome::DelegateError {
                code: ErrorCode::Eperm
            }
        );
    }

    fn namespace() -> Namespace {
        let stats = mount_rs_core::types::Stats {
            dev: 0,
            ino: 1,
            mode: mount_rs_core::S_IFDIR | 0o755,
            nlink: 2,
            uid: 3,
            gid: 4,
            rdev: 0,
            size: 0,
            blksize: 4096,
            blocks: 0,
            atime_ms: 1,
            mtime_ms: 2,
            ctime_ms: 3,
            birthtime_ms: 4,
        };
        Namespace {
            format_version: 1,
            root: 1,
            next_inode: 2,
            default_uid: 3,
            default_gid: 4,
            umask: 0o027,
            default_chunker: ChunkerConfig {
                algorithm: "fixed-size".into(),
                version: 1,
                parameters: BTreeMap::from([("chunk_size".into(), 4096)]),
            },
            nodes: BTreeMap::from([(
                1,
                NodeMetadata {
                    stats,
                    data: NodeData::Directory { entries: vec![] },
                },
            )]),
        }
    }

    fn injector(
        phase: FaultPhase,
        operation: FaultOperation,
        action: FaultAction,
    ) -> FaultInjector {
        FaultInjector::new(
            FaultPlan::new(
                97,
                1,
                vec![FaultRule::new(
                    FaultBoundary::Metadata,
                    operation,
                    phase,
                    FaultOccurrence::Once,
                    action,
                )],
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn after_publish_cannot_report_a_definite_conflict_after_successful_delegate() {
        let after = FaultRule::new(
            FaultBoundary::Metadata,
            FaultOperation::Publish,
            FaultPhase::After,
            FaultOccurrence::Once,
            FaultAction::Error(ErrorCode::Eagain),
        );
        assert!(matches!(
            FaultPlan::new(99, 1, vec![after]),
            Err(PlanError::InvalidAction { .. }),
        ));
        let before = FaultRule {
            phase: FaultPhase::Before,
            ..after
        };
        assert!(FaultPlan::new(100, 1, vec![before]).is_ok());
    }

    #[test]
    fn after_publish_cannot_report_a_definite_lease_failure_after_commit() {
        for action in [
            FaultAction::LeaseFailure,
            FaultAction::Error(ErrorCode::Estale),
        ] {
            let after = FaultRule::new(
                FaultBoundary::Metadata,
                FaultOperation::Publish,
                FaultPhase::After,
                FaultOccurrence::Once,
                action,
            );
            assert!(matches!(
                FaultPlan::new(101, 1, vec![after]),
                Err(PlanError::InvalidAction { .. }),
            ));
            assert!(
                FaultPlan::new(
                    102,
                    1,
                    vec![FaultRule {
                        phase: FaultPhase::Before,
                        ..after
                    }]
                )
                .is_ok()
            );
        }
        assert!(
            FaultPlan::new(
                103,
                1,
                vec![FaultRule::new(
                    FaultBoundary::Metadata,
                    FaultOperation::Publish,
                    FaultPhase::After,
                    FaultOccurrence::Once,
                    FaultAction::LostAcknowledgment,
                )]
            )
            .is_ok()
        );
    }

    #[tokio::test]
    async fn compact_prepare_before_and_after_faults_distinguish_no_call_from_committed_lost_ack() {
        let directory = tempfile::tempdir().unwrap();
        let raw = SqliteMetadataStore::open(directory.path().join("metadata.db")).unwrap();
        let backing = ConcurrentBackingId::from_bytes([0xc7; 16]).unwrap();
        if !cfg!(unix) {
            assert_eq!(
                raw.compact_inode_capability(),
                CompactInodeCapability::Unsupported
            );
            assert_eq!(
                raw.compact_inode_mode_state().await.unwrap_err().code,
                ErrorCode::Enotsup
            );
            assert_eq!(
                raw.prepare_compact_inode_mode(backing, 1)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Enotsup
            );
            return;
        }
        assert_eq!(raw.compact_inode_capability(), CompactInodeCapability::V1);
        raw.prepare_bound_concurrent_mode(backing).await.unwrap();
        raw.publish_bound_if_revision(backing, 0, namespace())
            .await
            .unwrap();
        let before = FaultMetadataStore::new(
            raw.clone(),
            injector(
                FaultPhase::Before,
                FaultOperation::Publish,
                FaultAction::Error(ErrorCode::Eio),
            ),
        );
        assert_eq!(
            before.compact_inode_capability(),
            CompactInodeCapability::V1
        );
        assert_eq!(
            before
                .prepare_compact_inode_mode(backing, 1)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        assert_eq!(
            raw.load_compact_snapshot(backing).await.unwrap_err().code,
            ErrorCode::Estale
        );
        assert_eq!(
            before.injector().trace().events[0].phase,
            FaultPhase::Before
        );

        let after = FaultMetadataStore::new(
            raw.clone(),
            injector(
                FaultPhase::After,
                FaultOperation::Publish,
                FaultAction::LostAcknowledgment,
            ),
        );
        let error = after
            .prepare_compact_inode_mode(backing, 1)
            .await
            .unwrap_err();
        assert!(is_commit_unknown(&error));
        let snapshot = raw.load_compact_snapshot(backing).await.unwrap();
        assert_eq!(snapshot.anchor.generation, 2);
        assert_eq!(
            after.injector().trace().events[0].outcome,
            FaultOutcome::CommitUnknown
        );
    }

    #[tokio::test]
    async fn compact_read_and_publication_faults_preserve_exact_backend_state() {
        let directory = tempfile::tempdir().unwrap();
        let raw = SqliteMetadataStore::open(directory.path().join("metadata.db")).unwrap();
        let backing = ConcurrentBackingId::from_bytes([0xc8; 16]).unwrap();
        if !cfg!(unix) {
            assert_eq!(
                raw.compact_inode_capability(),
                CompactInodeCapability::Unsupported
            );
            assert_eq!(
                raw.compact_inode_mode_state().await.unwrap_err().code,
                ErrorCode::Enotsup
            );
            assert_eq!(
                raw.prepare_compact_inode_mode(backing, 1)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Enotsup
            );
            return;
        }
        assert_eq!(raw.compact_inode_capability(), CompactInodeCapability::V1);
        raw.prepare_bound_concurrent_mode(backing).await.unwrap();
        raw.publish_bound_if_revision(backing, 0, namespace())
            .await
            .unwrap();
        raw.prepare_compact_inode_mode(backing, 1).await.unwrap();

        let load_before = FaultMetadataStore::new(
            raw.clone(),
            injector(
                FaultPhase::Before,
                FaultOperation::Load,
                FaultAction::Error(ErrorCode::Eio),
            ),
        );
        assert_eq!(
            load_before
                .load_compact_snapshot(backing)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        let load_after = FaultMetadataStore::new(
            raw.clone(),
            injector(
                FaultPhase::After,
                FaultOperation::Load,
                FaultAction::Error(ErrorCode::Eio),
            ),
        );
        assert_eq!(
            load_after
                .load_compact_inode(backing, 1)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        let wrapper = FaultMetadataStore::new(raw.clone(), FaultInjector::disabled(98));
        assert_eq!(
            wrapper.load_compact_snapshot(backing).await.unwrap(),
            raw.load_compact_snapshot(backing).await.unwrap()
        );

        let snapshot = wrapper.load_compact_snapshot(backing).await.unwrap();
        let mut candidate = snapshot.namespace().unwrap();
        let inode = candidate.next_inode;
        let mut stats = candidate.nodes[&1].stats.clone();
        stats.ino = inode;
        stats.mode = mount_rs_core::S_IFREG | 0o644;
        stats.nlink = 1;
        candidate.nodes.insert(
            inode,
            NodeMetadata {
                stats,
                data: NodeData::File(FileLayout {
                    chunker: candidate.default_chunker.clone(),
                    extents: vec![],
                }),
            },
        );
        let NodeData::Directory { entries } = &mut candidate.nodes.get_mut(&1).unwrap().data else {
            panic!()
        };
        entries.push(DirectoryEntry {
            name: "file".into(),
            inode,
        });
        candidate.next_inode += 1;
        let delta =
            CompactStructuralDelta::capture(&snapshot, &candidate, StructuralScope::FileCreate)
                .unwrap();
        let structure = FaultMetadataStore::new(
            raw.clone(),
            injector(
                FaultPhase::After,
                FaultOperation::Publish,
                FaultAction::LostAcknowledgment,
            ),
        );
        assert!(is_commit_unknown(
            &structure
                .publish_compact_structure(&delta)
                .await
                .unwrap_err()
        ));
        let current = raw.load_compact_snapshot(backing).await.unwrap();
        assert_eq!(current.anchor, *delta.next_anchor());
        let loaded = wrapper.load_compact_inode(backing, inode).await.unwrap();
        let mut updated = loaded.guard.node.clone();
        updated.stats.mtime_ms += 1;
        let selected = FaultMetadataStore::new(
            raw.clone(),
            injector(
                FaultPhase::After,
                FaultOperation::Publish,
                FaultAction::LostAcknowledgment,
            ),
        );
        assert!(is_commit_unknown(
            &selected
                .publish_compact_inode(
                    backing,
                    inode,
                    loaded.generation,
                    loaded.guard.identity,
                    updated.clone(),
                )
                .await
                .unwrap_err()
        ));
        let stored = raw.load_compact_inode(backing, inode).await.unwrap();
        assert_eq!(stored.guard.node, updated);
        assert_eq!(
            stored.guard.identity.revision,
            loaded.guard.identity.revision + 1
        );
        assert_eq!(
            selected.injector().trace().events[0].outcome,
            FaultOutcome::CommitUnknown
        );
        let incapable = FaultMetadataStore::new(
            mount_rs_memory::MemoryMetadataStore::new(),
            FaultInjector::disabled(101),
        );
        assert_eq!(
            incapable.compact_inode_capability(),
            CompactInodeCapability::Unsupported
        );
        assert_eq!(
            incapable
                .prepare_compact_inode_mode(backing, 1)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            incapable
                .load_compact_snapshot(backing)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            incapable
                .load_compact_inode(backing, inode)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            incapable
                .publish_compact_inode(
                    backing,
                    inode,
                    loaded.generation,
                    loaded.guard.identity,
                    updated
                )
                .await
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            incapable
                .publish_compact_structure(&delta)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
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
