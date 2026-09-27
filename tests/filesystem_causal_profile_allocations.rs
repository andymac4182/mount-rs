//! Explicit opt-in allocation and stderr smoke gates for core profile spans.
//! Recording claims exclude first setup, snapshots, provider work and futures.
//! The allocation gate uses synthetic fixed units to exercise the primitives;
//! it does not qualify filesystem reason/terminal accounting by itself.

use mount_rs_core::diagnostics::profile::{self, Event, Span};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Duration;

thread_local! {
    static COUNT_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATION_CALLS: Cell<u64> = const { Cell::new(0) };
}

struct CountingAllocator;
fn allocation_call() {
    let _ = COUNT_ALLOCATIONS.try_with(|enabled| {
        if enabled.get() {
            let _ = ALLOCATION_CALLS.try_with(|calls| calls.set(calls.get() + 1));
        }
    });
}
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocation_call();
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocation_call();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocation_call();
        unsafe { System.realloc(pointer, layout, size) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const CAUSAL_EVENTS: [Event; 61] = [
    Event::FilesystemBlockPutInitial,
    Event::FilesystemBlockPutInitialSuccess,
    Event::FilesystemBlockPutInitialError,
    Event::FilesystemBlockPutInitialCancelled,
    Event::FilesystemBlockPutFallback,
    Event::FilesystemBlockPutFallbackSuccess,
    Event::FilesystemBlockPutFallbackError,
    Event::FilesystemBlockPutFallbackCancelled,
    Event::FilesystemBlockPutRetryRewrite,
    Event::FilesystemBlockPutRetryRewriteSuccess,
    Event::FilesystemBlockPutRetryRewriteError,
    Event::FilesystemBlockPutRetryRewriteCancelled,
    Event::FilesystemBlockPutChunkerReprepare,
    Event::FilesystemBlockPutChunkerReprepareSuccess,
    Event::FilesystemBlockPutChunkerReprepareError,
    Event::FilesystemBlockPutChunkerReprepareCancelled,
    Event::FilesystemGateWaitRead,
    Event::FilesystemGateHoldRead,
    Event::FilesystemGateWaitReadCancelled,
    Event::FilesystemGateWaitMetadata,
    Event::FilesystemGateHoldMetadata,
    Event::FilesystemGateWaitMetadataCancelled,
    Event::FilesystemGateWaitWritePrepare,
    Event::FilesystemGateHoldWritePrepare,
    Event::FilesystemGateWaitWritePrepareCancelled,
    Event::FilesystemGateWaitWriteCommit,
    Event::FilesystemGateHoldWriteCommit,
    Event::FilesystemGateWaitWriteCommitCancelled,
    Event::FilesystemGateWaitWriteFallback,
    Event::FilesystemGateHoldWriteFallback,
    Event::FilesystemGateWaitWriteFallbackCancelled,
    Event::FilesystemGateWaitWholeFileReplay,
    Event::FilesystemGateHoldWholeFileReplay,
    Event::FilesystemGateWaitWholeFileReplayCancelled,
    Event::FilesystemGateWaitMutationBatch,
    Event::FilesystemGateHoldMutationBatch,
    Event::FilesystemGateWaitMutationBatchCancelled,
    Event::FilesystemGateWaitMaintenance,
    Event::FilesystemGateHoldMaintenance,
    Event::FilesystemGateWaitMaintenanceCancelled,
    Event::FilesystemGatePhaseRefresh,
    Event::FilesystemGatePhaseRecovery,
    Event::FilesystemGatePhaseBlockRewrite,
    Event::FilesystemGatePhasePublication,
    Event::FilesystemGatePhaseCasBackoff,
    Event::FilesystemMutationEnqueueRequests,
    Event::FilesystemMutationDequeueRequests,
    Event::FilesystemMutationQueueWaitRequests,
    Event::FilesystemMutationCoalescingYields,
    Event::FilesystemMutationAttemptRequests,
    Event::FilesystemMutationAttemptSuccess,
    Event::FilesystemMutationAttemptConflict,
    Event::FilesystemMutationAttemptNoPublication,
    Event::FilesystemMutationAttemptError,
    Event::FilesystemMutationAttemptCancelled,
    Event::FilesystemMutationRequestCommitted,
    Event::FilesystemMutationRequestConflict,
    Event::FilesystemMutationRequestCancelled,
    Event::FilesystemMutationRequestReceiverClosed,
    Event::FilesystemMutationRequestError,
    Event::FilesystemMutationRequestReplySent,
];

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn warmed_causal_profile_rows_record_without_added_allocations() {
    assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MOUNT_RS_TRACE_STORAGE").as_deref(), Ok("0"));
    assert_eq!(std::env::var("MOUNT_RS_TRACE_REQUESTS").as_deref(), Ok("0"));
    assert!(profile::enabled());
    // Warm both the actual global bank and the slow-span trace flag through
    // public recording before entering the allocation count window.
    let warm = Span::new(Event::BlockGet);
    std::thread::sleep(Duration::from_millis(110));
    drop(warm);
    let before = profile::snapshot();
    ALLOCATION_CALLS.with(|calls| calls.set(0));
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
    for event in CAUSAL_EVENTS {
        let mut span = Span::new(event).units(4);
        span.set_units(8);
        drop(span);
        profile::add(event, 0);
        drop(Span::new(event));
    }
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
    assert_eq!(
        ALLOCATION_CALLS.with(Cell::get),
        0,
        "warmed actual profile Span/add/drop recording added an allocation"
    );
    let delta = profile::snapshot().delta(&before).unwrap();
    assert_eq!(before.entries.len(), 108);
    assert_eq!(delta.entries.len(), 61);
    for (event, row) in CAUSAL_EVENTS.into_iter().zip(delta.entries) {
        assert_eq!(row.name, before.entries[event as usize].name);
        assert_eq!((row.calls, row.units), (3, 8));
    }
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=1"]
fn causal_profile_slow_span_emits_fixed_stderr() {
    assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MOUNT_RS_TRACE_STORAGE").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MOUNT_RS_TRACE_REQUESTS").as_deref(), Ok("0"));
    assert!(profile::enabled());
    let before = profile::snapshot();
    let span = Span::new(Event::FilesystemBlockPutInitial).units(4);
    std::thread::sleep(Duration::from_millis(110));
    drop(span);
    let delta = profile::snapshot().delta(&before).unwrap();
    assert_eq!(delta.entries.len(), 1);
    assert_eq!(delta.entries[0].name, "filesystem.block_put.initial");
    assert_eq!((delta.entries[0].calls, delta.entries[0].units), (1, 4));
    assert!(delta.entries[0].elapsed_ns >= 100_000_000);
    // The owned bounded parent retains stderr and checks the actual fixed
    // marker. This binary does not create a child or redirect global stderr.
}
