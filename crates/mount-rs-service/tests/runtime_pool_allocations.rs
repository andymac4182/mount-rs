//! Allocation deltas for warmed lifecycle primitives, excluding cold opens,
//! filesystem work, RPC encoding, snapshots and close-task construction.

#![cfg(feature = "allocation-profiling")]

use async_trait::async_trait;
use mount_rs_core::{FileHandle, FsDriver, FsError, Result, Stats};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_service::runtime_diagnostics::RuntimeDiagnostics;
use mount_rs_service::runtime_pool::{ManagedDrive, RuntimeFactory, RuntimePool};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static CALLS: Cell<u64> = const { Cell::new(0) };
    static SIZES: Cell<[(usize,u64);16]> = const { Cell::new([(0,0);16]) };
}
struct Allocator;
fn allocation(size: usize) {
    let _ = COUNTING.try_with(|counting| {
        if counting.get() {
            let _ = CALLS.try_with(|calls| calls.set(calls.get().saturating_add(1)));
            let _ = SIZES.try_with(|sizes| {
                let mut values = sizes.get();
                if let Some(entry) = values
                    .iter_mut()
                    .find(|(seen, count)| *count == 0 || *seen == size)
                {
                    entry.0 = size;
                    entry.1 = entry.1.saturating_add(1);
                }
                sizes.set(values);
            });
        }
    });
}
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        allocation(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        allocation(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        allocation(size);
        unsafe { System.realloc(ptr, layout, size) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
struct Window;
impl Window {
    fn begin() -> Self {
        CALLS.with(|calls| calls.set(0));
        SIZES.with(|sizes| sizes.set([(0, 0); 16]));
        COUNTING.with(|v| v.set(true));
        Self
    }
    fn finish(self) -> u64 {
        drop(self);
        CALLS.with(Cell::get)
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        COUNTING.with(|v| v.set(false));
    }
}

struct Runtime(Arc<MemoryFs>);
#[async_trait]
impl ManagedDrive for Runtime {
    fn driver(&self) -> Arc<dyn FsDriver> {
        self.0.clone()
    }
    fn failed(&self) -> bool {
        false
    }
    async fn shutdown(&self) -> Result<()> {
        Ok(())
    }
}
struct Factory;
#[async_trait]
impl RuntimeFactory for Factory {
    async fn open(&self) -> Result<Arc<dyn ManagedDrive>> {
        Ok(Arc::new(Runtime(Arc::new(MemoryFs::new(
            MemoryOptions::default(),
        )))))
    }
}
#[derive(Default)]
struct OpCounts {
    reads: AtomicUsize,
    writes: AtomicUsize,
}
impl OpCounts {
    fn reset(&self) {
        self.reads.store(0, Ordering::Relaxed);
        self.writes.store(0, Ordering::Relaxed);
    }
    fn assert_io(&self) {
        assert_eq!(self.reads.load(Ordering::Relaxed), 128);
        assert_eq!(self.writes.load(Ordering::Relaxed), 128);
    }
}
struct ImmediateHandle(Arc<OpCounts>);
#[async_trait]
impl FileHandle for ImmediateHandle {
    async fn read(&self, _: &mut [u8], _: Option<u64>) -> Result<usize> {
        self.0.reads.fetch_add(1, Ordering::Relaxed);
        Ok(0)
    }
    async fn write(&self, data: &[u8], _: Option<u64>) -> Result<usize> {
        self.0.writes.fetch_add(1, Ordering::Relaxed);
        Ok(data.len())
    }
    async fn stat(&self) -> Result<Stats> {
        Err(FsError::enosys("fixture stat"))
    }
    async fn truncate(&self, _: u64) -> Result<()> {
        Ok(())
    }
    async fn close(&self) -> Result<()> {
        Ok(())
    }
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "explicit serialized allocation gate; not a whole-RPC/provider allocation claim"]
async fn warmed_runtime_leases_allocate_nothing_and_handle_holder_adds_no_io_boxes() {
    for observed in [false, true] {
        // Observer construction and all snapshots stay outside allocation windows.
        let pool = if observed {
            RuntimePool::with_diagnostics(1, RuntimeDiagnostics::new(false)).unwrap()
        } else {
            RuntimePool::new(1).unwrap()
        };
        assert_eq!(pool.diagnostics().is_some(), observed);
        let registration = pool.register(Arc::new(Factory)).unwrap();
        let first = registration.acquire().await.unwrap();
        let counts = Arc::new(OpCounts::default());
        let actual: Arc<dyn FileHandle> = Arc::new(ImmediateHandle(counts.clone()));
        let handle = first.wrap_handle(actual.clone());
        drop(first);
        let mut data = [0; 1];
        // Include normal cooperative yields in warmup. Eight pairs did not cross
        // Tokio's budget; the first measured yield then allocated a 64-byte buffer.
        // Keep cooperative scheduling enabled in both warmup and measured windows.
        for _ in 0..256 {
            drop(registration.acquire().await.unwrap());
            actual.read(&mut data, Some(0)).await.unwrap();
            actual.write(&data, Some(0)).await.unwrap();
            handle.read(&mut data, Some(0)).await.unwrap();
            handle.write(&data, Some(0)).await.unwrap();
        }
        let window = Window::begin();
        for _ in 0..1024 {
            let lease = registration.acquire().await.unwrap();
            assert!(!lease.waited());
            let cloned = lease.clone();
            drop(cloned);
            drop(lease);
        }
        let leases = window.finish();
        assert_eq!(
            leases, 0,
            "warmed lease acquisition/clone/drop added allocation calls"
        );
        counts.reset();
        let mut read_total = 0usize;
        let mut write_total = 0usize;
        let window = Window::begin();
        for _ in 0..128 {
            read_total += actual.read(&mut data, Some(0)).await.unwrap();
            write_total += actual.write(&data, Some(0)).await.unwrap();
        }
        let direct = window.finish();
        let direct_sizes = SIZES.with(Cell::get);
        counts.assert_io();
        assert_eq!(read_total, 0);
        assert_eq!(write_total, 128);
        counts.reset();
        read_total = 0;
        write_total = 0;
        let window = Window::begin();
        for _ in 0..128 {
            read_total += handle.read(&mut data, Some(0)).await.unwrap();
            write_total += handle.write(&data, Some(0)).await.unwrap();
        }
        let wrapped = window.finish();
        let wrapped_sizes = SIZES.with(Cell::get);
        println!(
            "runtime-pool-allocation-layouts: observed={observed} direct={direct_sizes:?} held={wrapped_sizes:?}"
        );
        counts.assert_io();
        assert_eq!(read_total, 0);
        assert_eq!(write_total, 128);
        assert_eq!(
            direct, 256,
            "control must observe the existing async_trait futures"
        );
        assert_eq!(
            wrapped, direct,
            "handle holder added boxed futures or allocations"
        );
        assert_eq!(
            wrapped, 256,
            "held I/O must retain the exact existing future-box count"
        );
        handle.close().await.unwrap();
        drop(handle);
        drop(actual);
        pool.shutdown().await.unwrap();
        assert_eq!(pool.snapshot().resident, 0);
        if observed {
            let snapshot = pool
                .diagnostics()
                .expect("explicit runtime observer unavailable");
            let stage = |name| {
                snapshot
                    .entries
                    .iter()
                    .find(|entry| entry.name == name)
                    .unwrap()
            };
            let acquire = stage("runtime.acquire");
            assert_eq!(
                (
                    acquire.calls,
                    acquire.success,
                    acquire.error,
                    acquire.cancelled,
                    acquire.in_flight
                ),
                (1281, 1281, 0, 0, 0)
            );
            for name in [
                "runtime.activation_wait",
                "runtime.open",
                "runtime.handle_close",
                "runtime.terminal_drain",
            ] {
                let row = stage(name);
                assert_eq!(
                    (
                        row.calls,
                        row.success,
                        row.error,
                        row.cancelled,
                        row.in_flight
                    ),
                    (1, 1, 0, 0, 0)
                );
            }
            assert!(stage("runtime.state_mutex_wait").success > 0);
            assert!(stage("runtime.state_mutex_hold").success > 0);
            assert!(!snapshot.counter_saturated);
            assert_eq!(snapshot.slow_record_attempts, 0);
        } else {
            assert!(pool.diagnostics().is_none());
        }
        println!(
            "runtime-pool-allocation-control: observed={observed} warm_leases={leases} direct_handle_calls={direct} held_handle_calls={wrapped}; cold/provider/RPC/metadata/snapshot/close allocations excluded"
        );
    }
}
