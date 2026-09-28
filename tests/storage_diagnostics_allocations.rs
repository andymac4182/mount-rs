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
            | Operation::BlobCachePeerRequestSend
            | Operation::BlobCachePeerResponseReceive
            | Operation::BlobCachePeerGet
            | Operation::ObjectStoreBackingMarkerProbeBodyRead
            | Operation::ObjectStoreBackingMarkerDataBodyRead
            | Operation::ObjectStoreBackingMarkerProbeCreate
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
        Operation::RemoteClientQuicOpenBi,
        Operation::RemoteClientQuicRequestSend,
        Operation::RemoteClientQuicResponseReceive,
        Operation::BlobCachePeerRequestByteAdmissionWait,
        Operation::BlobCachePeerOpenBi,
        Operation::BlobCachePeerRequestSend,
        Operation::BlobCachePeerResponseReceive,
        Operation::BlobCachePeerGet,
        Operation::BlobCachePeerGetMiss,
        Operation::RemoteClientWebSocketTcpConnect,
        Operation::RemoteClientWebSocketTlsHandshake,
        Operation::RemoteClientWebSocketUpgrade,
        Operation::RemoteClientWebSocketSocketLockWait,
        Operation::RemoteClientWebSocketRequestEncode,
        Operation::RemoteClientWebSocketRequestSend,
        Operation::RemoteClientWebSocketResponseReceive,
        Operation::RemoteClientWebSocketResponseDecode,
        Operation::RemoteClientQuicConnectionSetup,
        Operation::BlobCacheDiscoveryLocate,
        Operation::ObjectStoreBackingMarkerProbeGet,
        Operation::ObjectStoreBackingMarkerProbeBodyRead,
        Operation::ObjectStoreBackingMarkerDataGet,
        Operation::ObjectStoreBackingMarkerDataBodyRead,
        Operation::ObjectStoreBackingMarkerProbeCreate,
        Operation::ObjectStoreBackingMarkerRetryBackoff,
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
    assert_eq!(before.entries.len(), 116);
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

#[test]
fn warmed_object_store_guards_and_fixed_snapshots_do_not_add_allocations() {
    use mount_rs_core::diagnostics::object_store::{ClientRole, HttpMethod, Observer};
    // Isolated bank and owner token construction are deliberately outside the window.
    let observer = Observer::isolated();
    let disabled = Observer::disabled();
    let cache = observer.cache_residency();
    let bundle = observer.bundle_build().finish_success().unwrap();
    let bundle_clone = std::sync::Arc::clone(&bundle);
    let client = observer.client(ClientRole::PrimaryDataMixed);
    let inert_client = disabled.client(ClientRole::StandaloneData);
    let _ = observer.snapshot();
    ALLOCATION_CALLS.with(|calls| calls.set(0));
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
    let positive = std::hint::black_box(Box::new([7u8; 1024]));
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
    let positive_calls = ALLOCATION_CALLS.with(Cell::get);
    drop(positive);
    assert!(positive_calls > 0, "allocator positive control failed");
    ALLOCATION_CALLS.with(|calls| calls.set(0));
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(true));
    for _ in 0..64 {
        observer.known_extra_future_box(ClientRole::PrimaryDataMixed, HttpMethod::Get);
        observer.known_extra_response_body_box(ClientRole::PrimaryDataMixed, HttpMethod::Get);
        let mut body = client.attempt(HttpMethod::Get, Some(3)).headers(200);
        body.data(3);
        body.eof();
        client.attempt(HttpMethod::Get, None).transport_error();
        drop(client.attempt(HttpMethod::Get, None));
        drop(client.attempt(HttpMethod::Get, None).headers(404));
        cache.publish(2, 3);
        cache.publish(1, 2);
        observer.bundle_build().finish_error();
        drop(observer.bundle_build());
        inert_client
            .attempt(HttpMethod::Get, None)
            .headers(200)
            .eof();
        drop(disabled.bundle_build().finish_success());
        let _ = std::hint::black_box(observer.snapshot());
    }
    drop(bundle);
    drop(bundle_clone);
    drop(client);
    drop(inert_client);
    drop(cache);
    COUNT_ALLOCATIONS.with(|enabled| enabled.set(false));
    let recorded_calls = ALLOCATION_CALLS.with(Cell::get);
    assert_eq!(
        recorded_calls, 0,
        "warmed bank-only guard updates added allocations"
    );
    let final_state = observer.snapshot().unwrap();
    assert_eq!(final_state.cache.live, 0);
    assert_eq!(final_state.bundles.live, 0);
    assert_eq!(
        final_state.clients[ClientRole::PrimaryDataMixed.index()].live,
        0
    );
    assert!(!final_state.saturated);
    assert_eq!(storage::snapshot().entries.len(), 116);
    // Excludes serialization, startup Arc/token construction, wrapper Box sites,
    // backing clients and the existing allocating storage::snapshot() above.
}
