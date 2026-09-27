//! Opt-in, bounded process-local counters for integration I/O investigations.
//! Set MOUNT_RS_PROFILE_IO=1 before starting the process. No paths or payloads
//! are recorded. Durations are inclusive wall time, not exclusive CPU time.
//! MOUNT_RS_TRACE_STORAGE=1 additionally permits at most 16 fixed-field slow
//! profile records per process; recording allocations exclude that I/O.

use serde::Serialize;
use std::io::{self, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

const SLOW_THRESHOLD_NS: u64 = 100_000_000;
const MAX_SLOW_RECORDS: u64 = 16;

macro_rules! events {
    ($($event:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy)]
        #[repr(usize)]
        pub enum Event { $($event),+ }
        const NAMES: &[&str] = &[$($name),+];
    };
}
events! {
    WireEncode => "wire.json_encode_bytes",
    WireDecode => "wire.json_decode_bytes",
    CatalogLoad => "catalog.load",
    CatalogQueue => "catalog.queue_wait",
    CatalogPoolWait => "catalog.pool_wait",
    CatalogBackingVerify => "catalog.backing_verify",
    CatalogConnect => "catalog.connect_configure",
    CatalogQuery => "catalog.query_document_bytes",
    CatalogDecode => "catalog.decode_validate_bytes",
    CatalogClose => "catalog.close",
    CatalogPagerHits => "catalog.pager_hits",
    CatalogPagerMisses => "catalog.pager_misses",
    CatalogPagerWrites => "catalog.pager_writes",
    CatalogPagerUnavailable => "catalog.pager_unavailable",
    Dispatch => "service.dispatch",
    Authorization => "service.authorization",
    HandleWait => "service.handle_lock_wait",
    Audit => "service.audit",
    GateWait => "filesystem.gate_wait",
    MutationBatch => "filesystem.mutation_batch_attempted_requests",
    Snapshot => "filesystem.snapshot_nodes",
    Refresh => "filesystem.metadata_refresh",
    Changed => "filesystem.changed_namespace_nodes",
    Fallback => "filesystem.write_fallback",
    RewriteRead => "filesystem.old_chunk_read_bytes",
    MetadataLoad => "provider.metadata.load",
    MetadataConditional => "provider.metadata.load_if_changed",
    BlockGet => "provider.blocks.get_bytes",
    BlockPut => "provider.blocks.put_bytes",
    BlockFlush => "provider.blocks.flush",
    BackingVerify => "provider.blocks.verify_authority",
    Publication => "provider.metadata.publish_cas_nodes",
    PublishConflict => "provider.metadata.cas_conflict",
    NamespaceReturned => "provider.namespace_returned_bytes",
    NamespaceSerialized => "provider.namespace_serialized_bytes",
    InodeSnapshotConditional => "provider.inode.snapshot_if_changed",
    InodeSnapshotReturned => "provider.inode.snapshot_returned_nodes",
    InodeSnapshotHit => "provider.inode.snapshot_unchanged",
    InodePathGuard => "filesystem.inode_path_guard",
    InodeLoad => "provider.inode.load",
    InodeConditional => "provider.inode.load_if_changed",
    InodePublication => "provider.inode.publish_cas",
    InodeConflict => "provider.inode.cas_conflict",
    CompactAnchorReturned => "provider.compact_anchor_returned_bytes",
    CompactAnchorSerialized => "provider.compact_anchor_serialized_bytes",
    InodeReturned => "provider.inode_returned_bytes",
    InodeSerialized => "provider.inode_serialized_bytes",
    FilesystemBlockPutInitial => "filesystem.block_put.initial",
    FilesystemBlockPutInitialSuccess => "filesystem.block_put.initial.success",
    FilesystemBlockPutInitialError => "filesystem.block_put.initial.error",
    FilesystemBlockPutInitialCancelled => "filesystem.block_put.initial.cancelled",
    FilesystemBlockPutFallback => "filesystem.block_put.fallback",
    FilesystemBlockPutFallbackSuccess => "filesystem.block_put.fallback.success",
    FilesystemBlockPutFallbackError => "filesystem.block_put.fallback.error",
    FilesystemBlockPutFallbackCancelled => "filesystem.block_put.fallback.cancelled",
    FilesystemBlockPutRetryRewrite => "filesystem.block_put.retry_rewrite",
    FilesystemBlockPutRetryRewriteSuccess => "filesystem.block_put.retry_rewrite.success",
    FilesystemBlockPutRetryRewriteError => "filesystem.block_put.retry_rewrite.error",
    FilesystemBlockPutRetryRewriteCancelled => "filesystem.block_put.retry_rewrite.cancelled",
    FilesystemBlockPutChunkerReprepare => "filesystem.block_put.chunker_reprepare",
    FilesystemBlockPutChunkerReprepareSuccess => "filesystem.block_put.chunker_reprepare.success",
    FilesystemBlockPutChunkerReprepareError => "filesystem.block_put.chunker_reprepare.error",
    FilesystemBlockPutChunkerReprepareCancelled => "filesystem.block_put.chunker_reprepare.cancelled",
    FilesystemGateWaitRead => "filesystem.gate_wait.read",
    FilesystemGateHoldRead => "filesystem.gate_hold.read",
    FilesystemGateWaitReadCancelled => "filesystem.gate_wait.read.cancelled",
    FilesystemGateWaitMetadata => "filesystem.gate_wait.metadata",
    FilesystemGateHoldMetadata => "filesystem.gate_hold.metadata",
    FilesystemGateWaitMetadataCancelled => "filesystem.gate_wait.metadata.cancelled",
    FilesystemGateWaitWritePrepare => "filesystem.gate_wait.write_prepare",
    FilesystemGateHoldWritePrepare => "filesystem.gate_hold.write_prepare",
    FilesystemGateWaitWritePrepareCancelled => "filesystem.gate_wait.write_prepare.cancelled",
    FilesystemGateWaitWriteCommit => "filesystem.gate_wait.write_commit",
    FilesystemGateHoldWriteCommit => "filesystem.gate_hold.write_commit",
    FilesystemGateWaitWriteCommitCancelled => "filesystem.gate_wait.write_commit.cancelled",
    FilesystemGateWaitWriteFallback => "filesystem.gate_wait.write_fallback",
    FilesystemGateHoldWriteFallback => "filesystem.gate_hold.write_fallback",
    FilesystemGateWaitWriteFallbackCancelled => "filesystem.gate_wait.write_fallback.cancelled",
    FilesystemGateWaitWholeFileReplay => "filesystem.gate_wait.whole_file_replay",
    FilesystemGateHoldWholeFileReplay => "filesystem.gate_hold.whole_file_replay",
    FilesystemGateWaitWholeFileReplayCancelled => "filesystem.gate_wait.whole_file_replay.cancelled",
    FilesystemGateWaitMutationBatch => "filesystem.gate_wait.mutation_batch",
    FilesystemGateHoldMutationBatch => "filesystem.gate_hold.mutation_batch",
    FilesystemGateWaitMutationBatchCancelled => "filesystem.gate_wait.mutation_batch.cancelled",
    FilesystemGateWaitMaintenance => "filesystem.gate_wait.maintenance",
    FilesystemGateHoldMaintenance => "filesystem.gate_hold.maintenance",
    FilesystemGateWaitMaintenanceCancelled => "filesystem.gate_wait.maintenance.cancelled",
    FilesystemGatePhaseRefresh => "filesystem.gate_phase.refresh",
    FilesystemGatePhaseRecovery => "filesystem.gate_phase.recovery",
    FilesystemGatePhaseBlockRewrite => "filesystem.gate_phase.block_rewrite",
    FilesystemGatePhasePublication => "filesystem.gate_phase.publication",
    FilesystemGatePhaseCasBackoff => "filesystem.gate_phase.cas_backoff",
    FilesystemMutationEnqueueRequests => "filesystem.mutation.enqueue_requests",
    FilesystemMutationDequeueRequests => "filesystem.mutation.dequeue_requests",
    FilesystemMutationQueueWaitRequests => "filesystem.mutation.queue_wait_requests",
    FilesystemMutationCoalescingYields => "filesystem.mutation.coalescing_yields",
    FilesystemMutationAttemptRequests => "filesystem.mutation.attempt_requests",
    FilesystemMutationAttemptSuccess => "filesystem.mutation.attempt.success",
    FilesystemMutationAttemptConflict => "filesystem.mutation.attempt.conflict",
    FilesystemMutationAttemptNoPublication => "filesystem.mutation.attempt.no_publication",
    FilesystemMutationAttemptError => "filesystem.mutation.attempt.error",
    FilesystemMutationAttemptCancelled => "filesystem.mutation.attempt.cancelled",
    FilesystemMutationRequestCommitted => "filesystem.mutation.request.committed",
    FilesystemMutationRequestConflict => "filesystem.mutation.request.conflict",
    FilesystemMutationRequestCancelled => "filesystem.mutation.request.cancelled",
    FilesystemMutationRequestReceiverClosed => "filesystem.mutation.request.receiver_closed",
    FilesystemMutationRequestError => "filesystem.mutation.request.error",
    FilesystemMutationRequestReplySent => "filesystem.mutation.request.reply_sent",
    FilesystemMutationCreateGuardEvaluated => "filesystem.mutation.create_guard.evaluated",
    FilesystemMutationCreateGuardPassed => "filesystem.mutation.create_guard.passed",
    FilesystemMutationCreateGuardConflict => "filesystem.mutation.create_guard.conflict",
    FilesystemMutationCreateGuardRevisionMismatch => "filesystem.mutation.create_guard.revision_mismatch",
    FilesystemMutationCreateGuardAllocationMismatch => "filesystem.mutation.create_guard.allocation_mismatch",
    FilesystemMutationCreateGuardPathPresent => "filesystem.mutation.create_guard.path_present",
    CompactNamespaceMaterializeNodes => "compact.namespace.materialize_nodes",
    MutationCandidateCloneNodes => "filesystem.mutation.candidate_clone_nodes",
    CompactStructuralDeltaCaptureNodes => "compact.structure.delta_capture_nodes",
    CompactStructuralExpectedGuardNodes => "compact.structure.expected_guard_nodes",
    SqliteCompactAuthorityQuery => "sqlite.compact.authority_query",
    SqliteCompactAuthorityPath => "sqlite.compact.authority_path",
    SqliteCompactAnchorQueryBytes => "sqlite.compact.anchor_query_bytes",
    SqliteCompactAnchorDecodeBytes => "sqlite.compact.anchor_decode_bytes",
    SqliteCompactGuardSelectedRows => "sqlite.compact.guard_selected_rows",
    SqliteCompactGuardFullRows => "sqlite.compact.guard_full_rows",
    SqliteCompactGuardSelectedDecodeBytes => "sqlite.compact.guard_selected_decode_bytes",
    SqliteCompactGuardFullDecodeBytes => "sqlite.compact.guard_full_decode_bytes",
    SqliteCompactReadLockWait => "sqlite.compact.read_lock_wait",
    SqliteCompactReadBegin => "sqlite.compact.read_begin",
    FilesystemRefreshReplaceProbe => "filesystem.refresh.replace_probe",
    FilesystemRefreshCreateCapture => "filesystem.refresh.create_capture",
    FilesystemRefreshBatchCapture => "filesystem.refresh.batch_capture",
    FilesystemRefreshPathStructure => "filesystem.refresh.path_structure",
    FilesystemRefreshReadBefore => "filesystem.refresh.read_before",
    FilesystemRefreshReadAfter => "filesystem.refresh.read_after",
    BlobCacheRamHitBytes => "blob_cache.ram.hit_bytes",
    BlobCacheDiskHitBytes => "blob_cache.disk.hit_bytes",
}

#[derive(Default)]
struct Metric {
    calls: AtomicU64,
    elapsed_ns: AtomicU64,
    units: AtomicU64,
}
struct Recorder {
    metrics: Vec<Metric>,
}
impl Recorder {
    fn new() -> Self {
        Self {
            metrics: NAMES.iter().map(|_| Metric::default()).collect(),
        }
    }
    fn record(&self, event: Event, elapsed_ns: u64, units: u64) {
        let metric = &self.metrics[event as usize];
        metric.calls.fetch_add(1, Ordering::Relaxed);
        metric.elapsed_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        metric.units.fetch_add(units, Ordering::Relaxed);
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            entries: self
                .metrics
                .iter()
                .enumerate()
                .map(|(index, m)| Entry {
                    name: NAMES[index],
                    calls: m.calls.load(Ordering::Relaxed),
                    elapsed_ns: m.elapsed_ns.load(Ordering::Relaxed),
                    units: m.units.load(Ordering::Relaxed),
                })
                .collect(),
        }
    }
}
static ENABLED: OnceLock<bool> = OnceLock::new();
static TRACE_ENABLED: OnceLock<bool> = OnceLock::new();
static RECORDER: OnceLock<Recorder> = OnceLock::new();
static SLOW_RECORDS: AtomicU64 = AtomicU64::new(0);
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("MOUNT_RS_PROFILE_IO").is_some_and(|v| v == "1"))
}
fn trace_enabled() -> bool {
    *TRACE_ENABLED
        .get_or_init(|| std::env::var_os("MOUNT_RS_TRACE_STORAGE").is_some_and(|v| v == "1"))
}
pub fn add(event: Event, units: u64) {
    if enabled() {
        RECORDER.get_or_init(Recorder::new).record(event, 0, units);
    }
}
pub fn snapshot() -> Snapshot {
    RECORDER.get_or_init(Recorder::new).snapshot()
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub name: &'static str,
    pub calls: u64,
    pub elapsed_ns: u64,
    pub units: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
}
impl Snapshot {
    /// Call at quiescent stage boundaries; concurrent snapshots are not atomic.
    pub fn delta(&self, before: &Self) -> Result<Self, &'static str> {
        if self.entries.len() != before.entries.len() {
            return Err("profile shape changed");
        }
        let mut entries = Vec::new();
        for (now, old) in self.entries.iter().zip(&before.entries) {
            if now.name != old.name {
                return Err("profile shape changed");
            }
            let entry = Entry {
                name: now.name,
                calls: now
                    .calls
                    .checked_sub(old.calls)
                    .ok_or("profile counter reset")?,
                elapsed_ns: now
                    .elapsed_ns
                    .checked_sub(old.elapsed_ns)
                    .ok_or("profile counter reset")?,
                units: now
                    .units
                    .checked_sub(old.units)
                    .ok_or("profile counter reset")?,
            };
            if entry.calls != 0 || entry.units != 0 {
                entries.push(entry);
            }
        }
        Ok(Self { entries })
    }
}
pub struct Span {
    event: Event,
    started: Option<Instant>,
    units: u64,
}
impl Span {
    pub fn new(event: Event) -> Self {
        Self::from_clock(event, enabled(), Instant::now)
    }
    // The same lazy factory is used by production and deterministic disabled
    // controls. No diagnostic future, callback or clock is boxed or retained.
    fn from_clock(event: Event, active: bool, now: impl FnOnce() -> Instant) -> Self {
        Self {
            event,
            started: active.then(now),
            units: 0,
        }
    }
    pub fn units(mut self, units: u64) -> Self {
        self.units = units;
        self
    }
    pub fn set_units(&mut self, units: u64) {
        self.units = units;
    }
    fn finish_with(
        &mut self,
        elapsed: impl FnOnce(Instant) -> u64,
        record: impl FnOnce(Event, u64, u64),
    ) {
        if let Some(started) = self.started.take() {
            record(self.event, elapsed(started), self.units);
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        self.finish_with(
            |started| started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
            |event, elapsed_ns, units| {
                RECORDER
                    .get_or_init(Recorder::new)
                    .record(event, elapsed_ns, units);
                if slow_record_enabled(elapsed_ns, trace_enabled) {
                    let _ = write_slow_record(
                        &mut io::stderr().lock(),
                        &SLOW_RECORDS,
                        event,
                        elapsed_ns,
                        units,
                    );
                }
            },
        );
    }
}
fn slow_record_enabled(elapsed_ns: u64, trace: impl FnOnce() -> bool) -> bool {
    elapsed_ns >= SLOW_THRESHOLD_NS && trace()
}
fn write_slow_record(
    output: &mut impl Write,
    budget: &AtomicU64,
    event: Event,
    elapsed_ns: u64,
    units: u64,
) -> io::Result<()> {
    if budget
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            (value < MAX_SLOW_RECORDS).then_some(value + 1)
        })
        .is_ok()
    {
        writeln!(
            output,
            "MOUNT_RS_PROFILE_SLOW event={} elapsed_us={} units={}",
            NAMES[event as usize],
            elapsed_ns / 1_000,
            units,
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_counters_and_quiescent_deltas_reconcile() {
        let recorder = Recorder::new();
        let before = recorder.snapshot();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let recorder = &recorder;
                scope.spawn(move || {
                    for _ in 0..250 {
                        recorder.record(Event::BlockGet, 2, 4096);
                    }
                });
            }
        });
        let after = recorder.snapshot();
        let delta = after.delta(&before).unwrap();
        let entry = delta
            .entries
            .iter()
            .find(|e| e.name == "provider.blocks.get_bytes")
            .unwrap();
        assert_eq!(entry.calls, 1000);
        assert_eq!(entry.elapsed_ns, 2000);
        assert_eq!(entry.units, 4096000);
        assert!(before.delta(&after).is_err());
    }

    #[test]
    fn legacy_event_prefix_and_ordinals_remain_exact() {
        let expected = [
            (Event::WireEncode, "wire.json_encode_bytes"),
            (Event::WireDecode, "wire.json_decode_bytes"),
            (Event::CatalogLoad, "catalog.load"),
            (Event::CatalogQueue, "catalog.queue_wait"),
            (Event::CatalogPoolWait, "catalog.pool_wait"),
            (Event::CatalogBackingVerify, "catalog.backing_verify"),
            (Event::CatalogConnect, "catalog.connect_configure"),
            (Event::CatalogQuery, "catalog.query_document_bytes"),
            (Event::CatalogDecode, "catalog.decode_validate_bytes"),
            (Event::CatalogClose, "catalog.close"),
            (Event::CatalogPagerHits, "catalog.pager_hits"),
            (Event::CatalogPagerMisses, "catalog.pager_misses"),
            (Event::CatalogPagerWrites, "catalog.pager_writes"),
            (Event::CatalogPagerUnavailable, "catalog.pager_unavailable"),
            (Event::Dispatch, "service.dispatch"),
            (Event::Authorization, "service.authorization"),
            (Event::HandleWait, "service.handle_lock_wait"),
            (Event::Audit, "service.audit"),
            (Event::GateWait, "filesystem.gate_wait"),
            (
                Event::MutationBatch,
                "filesystem.mutation_batch_attempted_requests",
            ),
            (Event::Snapshot, "filesystem.snapshot_nodes"),
            (Event::Refresh, "filesystem.metadata_refresh"),
            (Event::Changed, "filesystem.changed_namespace_nodes"),
            (Event::Fallback, "filesystem.write_fallback"),
            (Event::RewriteRead, "filesystem.old_chunk_read_bytes"),
            (Event::MetadataLoad, "provider.metadata.load"),
            (
                Event::MetadataConditional,
                "provider.metadata.load_if_changed",
            ),
            (Event::BlockGet, "provider.blocks.get_bytes"),
            (Event::BlockPut, "provider.blocks.put_bytes"),
            (Event::BlockFlush, "provider.blocks.flush"),
            (Event::BackingVerify, "provider.blocks.verify_authority"),
            (Event::Publication, "provider.metadata.publish_cas_nodes"),
            (Event::PublishConflict, "provider.metadata.cas_conflict"),
            (
                Event::NamespaceReturned,
                "provider.namespace_returned_bytes",
            ),
            (
                Event::NamespaceSerialized,
                "provider.namespace_serialized_bytes",
            ),
            (
                Event::InodeSnapshotConditional,
                "provider.inode.snapshot_if_changed",
            ),
            (
                Event::InodeSnapshotReturned,
                "provider.inode.snapshot_returned_nodes",
            ),
            (Event::InodeSnapshotHit, "provider.inode.snapshot_unchanged"),
            (Event::InodePathGuard, "filesystem.inode_path_guard"),
            (Event::InodeLoad, "provider.inode.load"),
            (Event::InodeConditional, "provider.inode.load_if_changed"),
            (Event::InodePublication, "provider.inode.publish_cas"),
            (Event::InodeConflict, "provider.inode.cas_conflict"),
            (
                Event::CompactAnchorReturned,
                "provider.compact_anchor_returned_bytes",
            ),
            (
                Event::CompactAnchorSerialized,
                "provider.compact_anchor_serialized_bytes",
            ),
            (Event::InodeReturned, "provider.inode_returned_bytes"),
            (Event::InodeSerialized, "provider.inode_serialized_bytes"),
        ];
        for (index, (event, name)) in expected.into_iter().enumerate() {
            assert_eq!(event as usize, index);
            assert_eq!(NAMES[index], name);
        }
    }

    #[test]
    fn causal_event_append_order_is_fixed_and_has_no_dynamic_labels() {
        let expected = [
            (
                Event::FilesystemBlockPutInitial,
                "filesystem.block_put.initial",
            ),
            (
                Event::FilesystemBlockPutInitialSuccess,
                "filesystem.block_put.initial.success",
            ),
            (
                Event::FilesystemBlockPutInitialError,
                "filesystem.block_put.initial.error",
            ),
            (
                Event::FilesystemBlockPutInitialCancelled,
                "filesystem.block_put.initial.cancelled",
            ),
            (
                Event::FilesystemBlockPutFallback,
                "filesystem.block_put.fallback",
            ),
            (
                Event::FilesystemBlockPutFallbackSuccess,
                "filesystem.block_put.fallback.success",
            ),
            (
                Event::FilesystemBlockPutFallbackError,
                "filesystem.block_put.fallback.error",
            ),
            (
                Event::FilesystemBlockPutFallbackCancelled,
                "filesystem.block_put.fallback.cancelled",
            ),
            (
                Event::FilesystemBlockPutRetryRewrite,
                "filesystem.block_put.retry_rewrite",
            ),
            (
                Event::FilesystemBlockPutRetryRewriteSuccess,
                "filesystem.block_put.retry_rewrite.success",
            ),
            (
                Event::FilesystemBlockPutRetryRewriteError,
                "filesystem.block_put.retry_rewrite.error",
            ),
            (
                Event::FilesystemBlockPutRetryRewriteCancelled,
                "filesystem.block_put.retry_rewrite.cancelled",
            ),
            (
                Event::FilesystemBlockPutChunkerReprepare,
                "filesystem.block_put.chunker_reprepare",
            ),
            (
                Event::FilesystemBlockPutChunkerReprepareSuccess,
                "filesystem.block_put.chunker_reprepare.success",
            ),
            (
                Event::FilesystemBlockPutChunkerReprepareError,
                "filesystem.block_put.chunker_reprepare.error",
            ),
            (
                Event::FilesystemBlockPutChunkerReprepareCancelled,
                "filesystem.block_put.chunker_reprepare.cancelled",
            ),
            (Event::FilesystemGateWaitRead, "filesystem.gate_wait.read"),
            (Event::FilesystemGateHoldRead, "filesystem.gate_hold.read"),
            (
                Event::FilesystemGateWaitReadCancelled,
                "filesystem.gate_wait.read.cancelled",
            ),
            (
                Event::FilesystemGateWaitMetadata,
                "filesystem.gate_wait.metadata",
            ),
            (
                Event::FilesystemGateHoldMetadata,
                "filesystem.gate_hold.metadata",
            ),
            (
                Event::FilesystemGateWaitMetadataCancelled,
                "filesystem.gate_wait.metadata.cancelled",
            ),
            (
                Event::FilesystemGateWaitWritePrepare,
                "filesystem.gate_wait.write_prepare",
            ),
            (
                Event::FilesystemGateHoldWritePrepare,
                "filesystem.gate_hold.write_prepare",
            ),
            (
                Event::FilesystemGateWaitWritePrepareCancelled,
                "filesystem.gate_wait.write_prepare.cancelled",
            ),
            (
                Event::FilesystemGateWaitWriteCommit,
                "filesystem.gate_wait.write_commit",
            ),
            (
                Event::FilesystemGateHoldWriteCommit,
                "filesystem.gate_hold.write_commit",
            ),
            (
                Event::FilesystemGateWaitWriteCommitCancelled,
                "filesystem.gate_wait.write_commit.cancelled",
            ),
            (
                Event::FilesystemGateWaitWriteFallback,
                "filesystem.gate_wait.write_fallback",
            ),
            (
                Event::FilesystemGateHoldWriteFallback,
                "filesystem.gate_hold.write_fallback",
            ),
            (
                Event::FilesystemGateWaitWriteFallbackCancelled,
                "filesystem.gate_wait.write_fallback.cancelled",
            ),
            (
                Event::FilesystemGateWaitWholeFileReplay,
                "filesystem.gate_wait.whole_file_replay",
            ),
            (
                Event::FilesystemGateHoldWholeFileReplay,
                "filesystem.gate_hold.whole_file_replay",
            ),
            (
                Event::FilesystemGateWaitWholeFileReplayCancelled,
                "filesystem.gate_wait.whole_file_replay.cancelled",
            ),
            (
                Event::FilesystemGateWaitMutationBatch,
                "filesystem.gate_wait.mutation_batch",
            ),
            (
                Event::FilesystemGateHoldMutationBatch,
                "filesystem.gate_hold.mutation_batch",
            ),
            (
                Event::FilesystemGateWaitMutationBatchCancelled,
                "filesystem.gate_wait.mutation_batch.cancelled",
            ),
            (
                Event::FilesystemGateWaitMaintenance,
                "filesystem.gate_wait.maintenance",
            ),
            (
                Event::FilesystemGateHoldMaintenance,
                "filesystem.gate_hold.maintenance",
            ),
            (
                Event::FilesystemGateWaitMaintenanceCancelled,
                "filesystem.gate_wait.maintenance.cancelled",
            ),
            (
                Event::FilesystemGatePhaseRefresh,
                "filesystem.gate_phase.refresh",
            ),
            (
                Event::FilesystemGatePhaseRecovery,
                "filesystem.gate_phase.recovery",
            ),
            (
                Event::FilesystemGatePhaseBlockRewrite,
                "filesystem.gate_phase.block_rewrite",
            ),
            (
                Event::FilesystemGatePhasePublication,
                "filesystem.gate_phase.publication",
            ),
            (
                Event::FilesystemGatePhaseCasBackoff,
                "filesystem.gate_phase.cas_backoff",
            ),
            (
                Event::FilesystemMutationEnqueueRequests,
                "filesystem.mutation.enqueue_requests",
            ),
            (
                Event::FilesystemMutationDequeueRequests,
                "filesystem.mutation.dequeue_requests",
            ),
            (
                Event::FilesystemMutationQueueWaitRequests,
                "filesystem.mutation.queue_wait_requests",
            ),
            (
                Event::FilesystemMutationCoalescingYields,
                "filesystem.mutation.coalescing_yields",
            ),
            (
                Event::FilesystemMutationAttemptRequests,
                "filesystem.mutation.attempt_requests",
            ),
            (
                Event::FilesystemMutationAttemptSuccess,
                "filesystem.mutation.attempt.success",
            ),
            (
                Event::FilesystemMutationAttemptConflict,
                "filesystem.mutation.attempt.conflict",
            ),
            (
                Event::FilesystemMutationAttemptNoPublication,
                "filesystem.mutation.attempt.no_publication",
            ),
            (
                Event::FilesystemMutationAttemptError,
                "filesystem.mutation.attempt.error",
            ),
            (
                Event::FilesystemMutationAttemptCancelled,
                "filesystem.mutation.attempt.cancelled",
            ),
            (
                Event::FilesystemMutationRequestCommitted,
                "filesystem.mutation.request.committed",
            ),
            (
                Event::FilesystemMutationRequestConflict,
                "filesystem.mutation.request.conflict",
            ),
            (
                Event::FilesystemMutationRequestCancelled,
                "filesystem.mutation.request.cancelled",
            ),
            (
                Event::FilesystemMutationRequestReceiverClosed,
                "filesystem.mutation.request.receiver_closed",
            ),
            (
                Event::FilesystemMutationRequestError,
                "filesystem.mutation.request.error",
            ),
            (
                Event::FilesystemMutationRequestReplySent,
                "filesystem.mutation.request.reply_sent",
            ),
            (
                Event::FilesystemMutationCreateGuardEvaluated,
                "filesystem.mutation.create_guard.evaluated",
            ),
            (
                Event::FilesystemMutationCreateGuardPassed,
                "filesystem.mutation.create_guard.passed",
            ),
            (
                Event::FilesystemMutationCreateGuardConflict,
                "filesystem.mutation.create_guard.conflict",
            ),
            (
                Event::FilesystemMutationCreateGuardRevisionMismatch,
                "filesystem.mutation.create_guard.revision_mismatch",
            ),
            (
                Event::FilesystemMutationCreateGuardAllocationMismatch,
                "filesystem.mutation.create_guard.allocation_mismatch",
            ),
            (
                Event::FilesystemMutationCreateGuardPathPresent,
                "filesystem.mutation.create_guard.path_present",
            ),
            (
                Event::CompactNamespaceMaterializeNodes,
                "compact.namespace.materialize_nodes",
            ),
            (
                Event::MutationCandidateCloneNodes,
                "filesystem.mutation.candidate_clone_nodes",
            ),
            (
                Event::CompactStructuralDeltaCaptureNodes,
                "compact.structure.delta_capture_nodes",
            ),
            (
                Event::CompactStructuralExpectedGuardNodes,
                "compact.structure.expected_guard_nodes",
            ),
            (
                Event::SqliteCompactAuthorityQuery,
                "sqlite.compact.authority_query",
            ),
            (
                Event::SqliteCompactAuthorityPath,
                "sqlite.compact.authority_path",
            ),
            (
                Event::SqliteCompactAnchorQueryBytes,
                "sqlite.compact.anchor_query_bytes",
            ),
            (
                Event::SqliteCompactAnchorDecodeBytes,
                "sqlite.compact.anchor_decode_bytes",
            ),
            (
                Event::SqliteCompactGuardSelectedRows,
                "sqlite.compact.guard_selected_rows",
            ),
            (
                Event::SqliteCompactGuardFullRows,
                "sqlite.compact.guard_full_rows",
            ),
            (
                Event::SqliteCompactGuardSelectedDecodeBytes,
                "sqlite.compact.guard_selected_decode_bytes",
            ),
            (
                Event::SqliteCompactGuardFullDecodeBytes,
                "sqlite.compact.guard_full_decode_bytes",
            ),
            (
                Event::SqliteCompactReadLockWait,
                "sqlite.compact.read_lock_wait",
            ),
            (Event::SqliteCompactReadBegin, "sqlite.compact.read_begin"),
            (
                Event::FilesystemRefreshReplaceProbe,
                "filesystem.refresh.replace_probe",
            ),
            (
                Event::FilesystemRefreshCreateCapture,
                "filesystem.refresh.create_capture",
            ),
            (
                Event::FilesystemRefreshBatchCapture,
                "filesystem.refresh.batch_capture",
            ),
            (
                Event::FilesystemRefreshPathStructure,
                "filesystem.refresh.path_structure",
            ),
            (
                Event::FilesystemRefreshReadBefore,
                "filesystem.refresh.read_before",
            ),
            (
                Event::FilesystemRefreshReadAfter,
                "filesystem.refresh.read_after",
            ),
        ];
        assert_eq!(NAMES.len(), 136);
        for (offset, (event, name)) in expected.into_iter().enumerate() {
            assert_eq!(event as usize, 47 + offset);
            assert_eq!(NAMES[47 + offset], name);
        }
        assert_eq!(Event::BlobCacheRamHitBytes as usize, 134);
        assert_eq!(Event::BlobCacheDiskHitBytes as usize, 135);
        assert_eq!(
            &NAMES[134..136],
            &["blob_cache.ram.hit_bytes", "blob_cache.disk.hit_bytes",]
        );
    }

    #[test]
    fn disabled_span_evaluates_neither_clock_nor_recording_callbacks() {
        let mut span = Span::from_clock(Event::FilesystemBlockPutInitial, false, || {
            panic!("disabled observation evaluated a clock")
        })
        .units(4);
        span.set_units(8);
        span.finish_with(
            |_| panic!("disabled observation evaluated elapsed time"),
            |_, _, _| panic!("disabled observation recorded or logged"),
        );
        drop(span);
    }

    #[test]
    fn actual_span_finisher_uses_final_units_and_records_only_once() {
        let recorder = Recorder::new();
        let started = Instant::now();
        let clock_calls = std::cell::Cell::new(0);
        let mut span = Span::from_clock(Event::FilesystemBlockPutInitial, true, || {
            clock_calls.set(clock_calls.get() + 1);
            started
        })
        .units(4);
        span.set_units(8);
        span.finish_with(
            |observed| {
                assert_eq!(observed, started);
                17
            },
            |event, elapsed, units| recorder.record(event, elapsed, units),
        );
        span.finish_with(
            |_| panic!("finished span evaluated elapsed time twice"),
            |_, _, _| panic!("finished span recorded twice"),
        );
        drop(span);
        let snapshot = recorder.snapshot();
        let row = &snapshot.entries[Event::FilesystemBlockPutInitial as usize];
        assert_eq!(clock_calls.get(), 1);
        assert_eq!((row.calls, row.elapsed_ns, row.units), (1, 17, 8));
    }

    #[test]
    fn slow_predicate_preserves_threshold_and_lazy_disabled_trace() {
        assert!(!slow_record_enabled(SLOW_THRESHOLD_NS - 1, || {
            panic!("short observation evaluated the trace flag")
        }));
        assert!(!slow_record_enabled(SLOW_THRESHOLD_NS, || false));
        assert!(slow_record_enabled(SLOW_THRESHOLD_NS, || true));
        assert!(slow_record_enabled(u64::MAX, || true));
    }

    #[test]
    fn slow_records_are_fixed_and_process_budget_is_finite() {
        let budget = AtomicU64::new(0);
        let mut output = Vec::new();
        for _ in 0..32 {
            write_slow_record(
                &mut output,
                &budget,
                Event::FilesystemBlockPutInitial,
                123_456_789,
                4096,
            )
            .unwrap();
        }
        let log = String::from_utf8(output).unwrap();
        assert_eq!(log.lines().count(), 16);
        assert_eq!(budget.load(Ordering::Relaxed), 16);
        assert!(log.lines().all(|line| line ==
            "MOUNT_RS_PROFILE_SLOW event=filesystem.block_put.initial elapsed_us=123456 units=4096"));
    }

    #[test]
    fn disabled_trace_does_not_write_or_consume_slow_budget() {
        let budget = AtomicU64::new(0);
        let mut output = Vec::new();
        if slow_record_enabled(SLOW_THRESHOLD_NS, || false) {
            write_slow_record(
                &mut output,
                &budget,
                Event::FilesystemGateWaitMetadata,
                SLOW_THRESHOLD_NS,
                0,
            )
            .unwrap();
        }
        assert!(output.is_empty());
        assert_eq!(budget.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn failed_slow_writer_consumes_one_bounded_attempt_without_retry() {
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("fixed test write failure"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let budget = AtomicU64::new(0);
        for attempt in 0..32 {
            let result = write_slow_record(
                &mut FailedWriter,
                &budget,
                Event::FilesystemGatePhasePublication,
                SLOW_THRESHOLD_NS,
                0,
            );
            assert_eq!(result.is_err(), attempt < 16);
        }
        assert_eq!(budget.load(Ordering::Relaxed), 16);
    }
}
