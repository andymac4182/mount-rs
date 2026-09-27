//! Explicit warmed recording gate; ordinary suites retain this as ignored.
//! It measures added Rust allocations in actual core Span lifecycles, excluding
//! initial environment/global-bank setup and all native client futures.

use mount_rs_core::diagnostics::storage::{self, Operation, Span};
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

fn selected_bytes(operation: Operation) -> u64 {
    if matches!(
        operation,
        Operation::FoundationDbReadGet
            | Operation::FoundationDbReadGetKey
            | Operation::FoundationDbReadGetRangePage
            | Operation::BlobCacheRamLookup
            | Operation::BlobCacheDiskLookup
    ) {
        3
    } else {
        0
    }
}

#[test]
#[ignore = "requires MOUNT_RS_PROFILE_IO=1 and MOUNT_RS_TRACE_STORAGE=0"]
fn warmed_core_spans_record_without_added_allocations() {
    assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(), Ok("1"));
    assert_eq!(std::env::var("MOUNT_RS_TRACE_STORAGE").as_deref(), Ok("0"));
    assert!(storage::enabled());
    let mut warm = Span::new(Operation::BlockGet);
    // The current core slow threshold is 100ms. Trigger its public completion
    // path outside the window to warm the private trace flag as well as the
    // global recorder, with tracing explicitly disabled by the gate.
    std::thread::sleep(Duration::from_millis(110));
    warm.finish_success(0);
    let before = storage::snapshot();
    let operations = [
        Operation::FoundationDbTransactionCreate,
        Operation::FoundationDbTransactionClosureAttempt,
        Operation::FoundationDbReadGet,
        Operation::FoundationDbReadGetKey,
        Operation::FoundationDbReadGetRangePage,
        Operation::FoundationDbTransactionCommit,
        Operation::FoundationDbTransactionOnError,
        Operation::BlobCacheMissAdmissionWait,
        Operation::BlobCacheMissSingleflightWait,
        Operation::BlobCacheRamLookup,
        Operation::BlobCacheDiskLookup,
        Operation::BlobCachePeerConnectionLockWait,
        Operation::BlobCachePeerConnectionEstablish,
    ];
    ALLOCATION_CALLS.with(|calls| calls.set(0));
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
    for operation in operations {
        let mut success = Span::new(operation);
        success.finish_success(selected_bytes(operation));
        let mut error = Span::new(operation);
        error.finish_error();
        drop(Span::new(operation));
    }
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
    let allocation_calls = ALLOCATION_CALLS.with(Cell::get);
    println!(
        "MOUNT_RS_CACHE_RECORDER_ALLOCATION bank=storage selected_rows={} declared_rows={} allocation_calls={allocation_calls}",
        operations.len(),
        before.entries.len()
    );
    assert_eq!(
        allocation_calls, 0,
        "warmed actual core Span added an allocation"
    );
    let delta = storage::snapshot().delta(&before).unwrap();
    assert_eq!(before.entries.len(), 91);
    assert_eq!(delta.in_flight, 0);
    for operation in operations {
        let row = &delta.entries[operation as usize];
        assert_eq!(
            (
                row.calls,
                row.success,
                row.error,
                row.cancelled,
                row.in_flight
            ),
            (3, 1, 1, 1, 0)
        );
        assert_eq!(row.latency_log2_us.iter().sum::<u64>(), 3);
        assert_eq!(row.bytes, selected_bytes(operation));
        assert_eq!((row.returned_rows, row.returned_row_observations), (0, 0));
    }
}
