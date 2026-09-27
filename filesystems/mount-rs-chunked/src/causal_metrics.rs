//! Fixed, opt-in observations at the filesystem's actual operation boundaries.
//!
//! Observations live on the stack. Disabled observations create no span and
//! record no terminal event; enabled recording uses the existing profile bank.

use mount_rs_core::Result;
use mount_rs_core::diagnostics::profile::{self, Event, Span};
use std::marker::PhantomData;

/// Count this resolved fresh-create guard, including overlapping predicates.
/// A pass is not a mutation or publication result. Batch remapping is already
/// applied by the caller before this guard evaluates its candidate.
pub(super) fn observe_create_guard(
    revision_mismatch: bool,
    allocation_mismatch: bool,
    path_present: bool,
) {
    profile::add(Event::FilesystemMutationCreateGuardEvaluated, 1);
    profile::add(
        if revision_mismatch || allocation_mismatch || path_present {
            Event::FilesystemMutationCreateGuardConflict
        } else {
            Event::FilesystemMutationCreateGuardPassed
        },
        1,
    );
    if revision_mismatch {
        profile::add(Event::FilesystemMutationCreateGuardRevisionMismatch, 1);
    }
    if allocation_mismatch {
        profile::add(Event::FilesystemMutationCreateGuardAllocationMismatch, 1);
    }
    if path_present {
        profile::add(Event::FilesystemMutationCreateGuardPathPresent, 1);
    }
}

#[derive(Clone, Copy)]
pub(super) enum PutReason {
    Initial,
    Fallback,
    RetryRewrite,
    ChunkerReprepare,
}

impl PutReason {
    fn events(self) -> [Event; 4] {
        match self {
            Self::Initial => [
                Event::FilesystemBlockPutInitial,
                Event::FilesystemBlockPutInitialSuccess,
                Event::FilesystemBlockPutInitialError,
                Event::FilesystemBlockPutInitialCancelled,
            ],
            Self::Fallback => [
                Event::FilesystemBlockPutFallback,
                Event::FilesystemBlockPutFallbackSuccess,
                Event::FilesystemBlockPutFallbackError,
                Event::FilesystemBlockPutFallbackCancelled,
            ],
            Self::RetryRewrite => [
                Event::FilesystemBlockPutRetryRewrite,
                Event::FilesystemBlockPutRetryRewriteSuccess,
                Event::FilesystemBlockPutRetryRewriteError,
                Event::FilesystemBlockPutRetryRewriteCancelled,
            ],
            Self::ChunkerReprepare => [
                Event::FilesystemBlockPutChunkerReprepare,
                Event::FilesystemBlockPutChunkerReprepareSuccess,
                Event::FilesystemBlockPutChunkerReprepareError,
                Event::FilesystemBlockPutChunkerReprepareCancelled,
            ],
        }
    }
}

pub(super) struct PutObservation {
    span: Option<Span>,
    reason: PutReason,
    input_bytes: u64,
    enabled: bool,
    finished: bool,
}

impl PutObservation {
    pub(super) fn new(reason: PutReason, input_bytes: u64) -> Self {
        let enabled = profile::enabled();
        Self {
            span: enabled.then(|| Span::new(reason.events()[0]).units(input_bytes)),
            reason,
            input_bytes,
            enabled,
            finished: false,
        }
    }

    pub(super) fn finish<T>(&mut self, result: &Result<T>) {
        if self.finished {
            return;
        }
        self.finished = true;
        // Stop at the provider's result, before error context or layout work.
        drop(self.span.take());
        if self.enabled {
            let events = self.reason.events();
            let (event, units) = if result.is_ok() {
                (events[1], self.input_bytes)
            } else {
                (events[2], 0)
            };
            profile::add(event, units);
        }
    }
}

impl Drop for PutObservation {
    fn drop(&mut self) {
        if self.enabled && !self.finished {
            profile::add(self.reason.events()[3], 0);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum GateKind {
    Read,
    Metadata,
    WritePrepare,
    WriteCommit,
    WriteFallback,
    WholeFileReplay,
    MutationBatch,
    Maintenance,
}

impl GateKind {
    fn events(self) -> [Event; 3] {
        match self {
            Self::Read => [
                Event::FilesystemGateWaitRead,
                Event::FilesystemGateHoldRead,
                Event::FilesystemGateWaitReadCancelled,
            ],
            Self::Metadata => [
                Event::FilesystemGateWaitMetadata,
                Event::FilesystemGateHoldMetadata,
                Event::FilesystemGateWaitMetadataCancelled,
            ],
            Self::WritePrepare => [
                Event::FilesystemGateWaitWritePrepare,
                Event::FilesystemGateHoldWritePrepare,
                Event::FilesystemGateWaitWritePrepareCancelled,
            ],
            Self::WriteCommit => [
                Event::FilesystemGateWaitWriteCommit,
                Event::FilesystemGateHoldWriteCommit,
                Event::FilesystemGateWaitWriteCommitCancelled,
            ],
            Self::WriteFallback => [
                Event::FilesystemGateWaitWriteFallback,
                Event::FilesystemGateHoldWriteFallback,
                Event::FilesystemGateWaitWriteFallbackCancelled,
            ],
            Self::WholeFileReplay => [
                Event::FilesystemGateWaitWholeFileReplay,
                Event::FilesystemGateHoldWholeFileReplay,
                Event::FilesystemGateWaitWholeFileReplayCancelled,
            ],
            Self::MutationBatch => [
                Event::FilesystemGateWaitMutationBatch,
                Event::FilesystemGateHoldMutationBatch,
                Event::FilesystemGateWaitMutationBatchCancelled,
            ],
            Self::Maintenance => [
                Event::FilesystemGateWaitMaintenance,
                Event::FilesystemGateHoldMaintenance,
                Event::FilesystemGateWaitMaintenanceCancelled,
            ],
        }
    }
}

pub(super) struct GateWaitObservation {
    _span: Option<Span>,
    kind: GateKind,
    enabled: bool,
    acquired: bool,
}

impl GateWaitObservation {
    pub(super) fn new(kind: GateKind) -> Self {
        let enabled = profile::enabled();
        Self {
            _span: enabled.then(|| Span::new(kind.events()[0])),
            kind,
            enabled,
            acquired: false,
        }
    }

    pub(super) fn acquired(&mut self) {
        self.acquired = true;
    }
}

impl Drop for GateWaitObservation {
    fn drop(&mut self) {
        if self.enabled && !self.acquired {
            profile::add(self.kind.events()[2], 0);
        }
    }
}

pub(super) fn gate_hold(kind: GateKind) -> Span {
    Span::new(kind.events()[1])
}

#[derive(Clone, Copy)]
pub(super) enum GatePhase {
    Refresh,
    Recovery,
    BlockRewrite,
    Publication,
    CasBackoff,
}

impl GatePhase {
    fn event(self) -> Event {
        match self {
            Self::Refresh => Event::FilesystemGatePhaseRefresh,
            Self::Recovery => Event::FilesystemGatePhaseRecovery,
            Self::BlockRewrite => Event::FilesystemGatePhaseBlockRewrite,
            Self::Publication => Event::FilesystemGatePhasePublication,
            Self::CasBackoff => Event::FilesystemGatePhaseCasBackoff,
        }
    }
}

/// A capability borrowed from an acquired gate, without retaining its mutex.
#[derive(Clone, Copy)]
pub(super) struct GatePhasePermit<'gate> {
    _gate: PhantomData<&'gate ()>,
}

impl<'gate> GatePhasePermit<'gate> {
    pub(super) fn new(_guard: &'gate super::OperationGateGuard<'_>) -> Self {
        Self { _gate: PhantomData }
    }

    pub(super) fn phase(self, phase: GatePhase) -> GatePhaseObservation<'gate> {
        GatePhaseObservation {
            span: profile::enabled().then(|| Span::new(phase.event())),
            _gate: PhantomData,
        }
    }
}

/// Retains the gate borrow until this phase's span ends.
pub(super) struct GatePhaseObservation<'gate> {
    span: Option<Span>,
    _gate: PhantomData<&'gate ()>,
}

impl Drop for GatePhaseObservation<'_> {
    fn drop(&mut self) {
        // Explicit Drop keeps the capability borrow live until the span ends.
        drop(self.span.take());
    }
}

#[derive(Clone, Copy)]
pub(super) enum RequestOutcome {
    Committed,
    Conflict,
    Cancelled,
    Error,
}

impl RequestOutcome {
    fn event(self) -> Event {
        match self {
            Self::Committed => Event::FilesystemMutationRequestCommitted,
            Self::Conflict => Event::FilesystemMutationRequestConflict,
            Self::Cancelled => Event::FilesystemMutationRequestCancelled,
            Self::Error => Event::FilesystemMutationRequestError,
        }
    }
}

pub(super) struct MutationObservation {
    queue_span: Option<Span>,
    enabled: bool,
    active: bool,
    dequeued: bool,
    terminal: bool,
    delivered: bool,
}

impl MutationObservation {
    pub(super) fn new() -> Self {
        Self {
            queue_span: None,
            enabled: profile::enabled(),
            active: false,
            dequeued: false,
            terminal: false,
            delivered: false,
        }
    }

    pub(super) fn enqueued(&mut self) {
        if self.active {
            return;
        }
        self.active = true;
        if self.enabled {
            self.queue_span = Some(Span::new(Event::FilesystemMutationQueueWaitRequests).units(1));
            profile::add(Event::FilesystemMutationEnqueueRequests, 1);
        }
    }

    pub(super) fn dequeued(&mut self) {
        if !self.active || self.dequeued {
            return;
        }
        self.dequeued = true;
        drop(self.queue_span.take());
        if self.enabled {
            profile::add(Event::FilesystemMutationDequeueRequests, 1);
        }
    }

    pub(super) fn outcome(&mut self, outcome: RequestOutcome) {
        if !self.active || self.terminal {
            return;
        }
        self.terminal = true;
        if self.enabled {
            profile::add(outcome.event(), 1);
        }
    }

    pub(super) fn delivery(&mut self, sent: bool) {
        if !self.active || self.delivered {
            return;
        }
        self.delivered = true;
        if self.enabled {
            profile::add(
                if sent {
                    Event::FilesystemMutationRequestReplySent
                } else {
                    Event::FilesystemMutationRequestReceiverClosed
                },
                1,
            );
        }
    }

    pub(super) fn cancelled_receiver(&mut self) {
        self.outcome(RequestOutcome::Cancelled);
        self.delivery(false);
    }
}

impl Drop for MutationObservation {
    fn drop(&mut self) {
        if self.active && !self.terminal {
            self.outcome(RequestOutcome::Cancelled);
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum AttemptOutcome {
    Success,
    Conflict,
    NoPublication,
    Error,
}

impl AttemptOutcome {
    fn event(self) -> Event {
        match self {
            Self::Success => Event::FilesystemMutationAttemptSuccess,
            Self::Conflict => Event::FilesystemMutationAttemptConflict,
            Self::NoPublication => Event::FilesystemMutationAttemptNoPublication,
            Self::Error => Event::FilesystemMutationAttemptError,
        }
    }
}

pub(super) struct BatchAttemptObservation {
    span: Option<Span>,
    considered: u64,
    enabled: bool,
    terminal: bool,
}

impl BatchAttemptObservation {
    pub(super) fn new() -> Self {
        let enabled = profile::enabled();
        Self {
            span: enabled.then(|| Span::new(Event::FilesystemMutationAttemptRequests)),
            considered: 0,
            enabled,
            terminal: false,
        }
    }

    pub(super) fn considered(&mut self) {
        if self.terminal {
            return;
        }
        self.considered = self.considered.saturating_add(1);
        if let Some(span) = &mut self.span {
            span.set_units(self.considered);
        }
    }

    pub(super) fn finish(&mut self, outcome: AttemptOutcome) {
        if self.terminal {
            return;
        }
        self.terminal = true;
        drop(self.span.take());
        if self.enabled {
            profile::add(outcome.event(), self.considered);
        }
    }
}

impl Drop for BatchAttemptObservation {
    fn drop(&mut self) {
        if self.enabled && !self.terminal {
            profile::add(Event::FilesystemMutationAttemptCancelled, self.considered);
        }
    }
}

pub(super) struct CoalescingObservation {
    span: Option<Span>,
    yields: u64,
}

impl CoalescingObservation {
    pub(super) fn new() -> Self {
        Self {
            span: profile::enabled().then(|| Span::new(Event::FilesystemMutationCoalescingYields)),
            yields: 0,
        }
    }

    pub(super) fn yielded(&mut self) {
        self.yields = self.yields.saturating_add(1);
        if let Some(span) = &mut self.span {
            span.set_units(self.yields);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::{ErrorCode, FsError};

    #[test]
    fn phase_capabilities_do_not_add_mutex_guard_send_or_sync_bounds() {
        fn send_sync<T: Send + Sync>() {}
        send_sync::<GatePhasePermit<'static>>();
        send_sync::<GatePhaseObservation<'static>>();
        send_sync::<PutObservation>();
        send_sync::<GateWaitObservation>();
        send_sync::<MutationObservation>();
        send_sync::<BatchAttemptObservation>();
        send_sync::<CoalescingObservation>();
    }

    fn counter_delta(
        before: &profile::Snapshot,
        after: &profile::Snapshot,
        name: &str,
    ) -> (u64, u64) {
        let entry = |snapshot: &profile::Snapshot| {
            let entry = snapshot
                .entries
                .iter()
                .find(|entry| entry.name == name)
                .expect("fixed causal row exists");
            (entry.calls, entry.units)
        };
        let old = entry(before);
        let now = entry(after);
        (
            now.0.checked_sub(old.0).expect("monotonic calls"),
            now.1.checked_sub(old.1).expect("monotonic units"),
        )
    }

    #[test]
    #[ignore = "run alone with MOUNT_RS_PROFILE_IO=1 and --test-threads=1"]
    fn helper_terminal_outcomes_and_drop_partition_once() {
        assert!(profile::enabled(), "start with MOUNT_RS_PROFILE_IO=1");
        let ok: Result<()> = Ok(());
        let error: Result<()> = Err(FsError::new(ErrorCode::Eio));
        let before = profile::snapshot();
        for (reason, label) in [
            (PutReason::Initial, "initial"),
            (PutReason::Fallback, "fallback"),
            (PutReason::RetryRewrite, "retry_rewrite"),
            (PutReason::ChunkerReprepare, "chunker_reprepare"),
        ] {
            let mut success = PutObservation::new(reason, 7);
            success.finish(&ok);
            success.finish(&error);
            drop(success);
            let mut failed = PutObservation::new(reason, 5);
            failed.finish(&error);
            drop(failed);
            drop(PutObservation::new(reason, 11));
            let after = profile::snapshot();
            let base = format!("filesystem.block_put.{label}");
            assert_eq!(counter_delta(&before, &after, &base), (3, 23));
            assert_eq!(
                counter_delta(&before, &after, &format!("{base}.success")),
                (1, 7)
            );
            assert_eq!(
                counter_delta(&before, &after, &format!("{base}.error")),
                (1, 0)
            );
            assert_eq!(
                counter_delta(&before, &after, &format!("{base}.cancelled")),
                (1, 0)
            );
        }

        let before = profile::snapshot();
        for (kind, label) in [
            (GateKind::Read, "read"),
            (GateKind::Metadata, "metadata"),
            (GateKind::WritePrepare, "write_prepare"),
            (GateKind::WriteCommit, "write_commit"),
            (GateKind::WriteFallback, "write_fallback"),
            (GateKind::WholeFileReplay, "whole_file_replay"),
            (GateKind::MutationBatch, "mutation_batch"),
            (GateKind::Maintenance, "maintenance"),
        ] {
            let mut acquired = GateWaitObservation::new(kind);
            acquired.acquired();
            drop(acquired);
            drop(gate_hold(kind));
            drop(GateWaitObservation::new(kind));
            let after = profile::snapshot();
            assert_eq!(
                counter_delta(&before, &after, &format!("filesystem.gate_wait.{label}")),
                (2, 0)
            );
            assert_eq!(
                counter_delta(&before, &after, &format!("filesystem.gate_hold.{label}")),
                (1, 0)
            );
            assert_eq!(
                counter_delta(
                    &before,
                    &after,
                    &format!("filesystem.gate_wait.{label}.cancelled")
                ),
                (1, 0)
            );
        }

        let before = profile::snapshot();
        drop(MutationObservation::new());
        let mut committed = MutationObservation::new();
        committed.enqueued();
        committed.enqueued();
        committed.dequeued();
        committed.dequeued();
        committed.outcome(RequestOutcome::Committed);
        committed.outcome(RequestOutcome::Error);
        committed.delivery(true);
        committed.delivery(false);
        drop(committed);
        let mut cancelled = MutationObservation::new();
        cancelled.enqueued();
        drop(cancelled);
        let mut closed = MutationObservation::new();
        closed.enqueued();
        closed.dequeued();
        closed.cancelled_receiver();
        closed.cancelled_receiver();
        drop(closed);
        let after = profile::snapshot();
        for (label, expected) in [
            ("enqueue_requests", (3, 3)),
            ("dequeue_requests", (2, 2)),
            ("queue_wait_requests", (3, 3)),
            ("request.committed", (1, 1)),
            ("request.cancelled", (2, 2)),
            ("request.receiver_closed", (1, 1)),
            ("request.reply_sent", (1, 1)),
            ("request.error", (0, 0)),
        ] {
            assert_eq!(
                counter_delta(&before, &after, &format!("filesystem.mutation.{label}")),
                expected
            );
        }

        let before = profile::snapshot();
        for outcome in [
            AttemptOutcome::Success,
            AttemptOutcome::Conflict,
            AttemptOutcome::NoPublication,
            AttemptOutcome::Error,
        ] {
            let mut attempt = BatchAttemptObservation::new();
            for _ in 0..3 {
                attempt.considered();
            }
            attempt.finish(outcome);
            attempt.finish(AttemptOutcome::Error);
            drop(attempt);
        }
        let mut cancelled = BatchAttemptObservation::new();
        cancelled.considered();
        cancelled.considered();
        drop(cancelled);
        let after = profile::snapshot();
        assert_eq!(
            counter_delta(&before, &after, "filesystem.mutation.attempt_requests"),
            (5, 14)
        );
        for label in ["success", "conflict", "no_publication", "error"] {
            assert_eq!(
                counter_delta(
                    &before,
                    &after,
                    &format!("filesystem.mutation.attempt.{label}")
                ),
                (1, 3)
            );
        }
        assert_eq!(
            counter_delta(&before, &after, "filesystem.mutation.attempt.cancelled"),
            (1, 2)
        );

        let before = profile::snapshot();
        let mut coalescing = CoalescingObservation::new();
        coalescing.yielded();
        coalescing.yielded();
        drop(coalescing);
        drop(CoalescingObservation::new());
        let after = profile::snapshot();
        assert_eq!(
            counter_delta(&before, &after, "filesystem.mutation.coalescing_yields"),
            (2, 2)
        );
    }
}
