//! Diagnostic-only allocator and process counters. Never enabled in production.
use serde::Serialize;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
#[cfg(feature = "allocation-profiling")]
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::AtomicUsize,
};

#[derive(Default)]
#[repr(align(128))]
struct Counters {
    allocs: AtomicU64,
    frees: AtomicU64,
    reallocs: AtomicU64,
    allocated_bytes: AtomicU64,
    freed_bytes: AtomicU64,
    live_bytes: AtomicI64,
}
impl Counters {
    const fn new() -> Self {
        Self {
            allocs: AtomicU64::new(0),
            frees: AtomicU64::new(0),
            reallocs: AtomicU64::new(0),
            allocated_bytes: AtomicU64::new(0),
            freed_bytes: AtomicU64::new(0),
            live_bytes: AtomicI64::new(0),
        }
    }
    fn allocated(&self, size: usize) {
        self.allocs.fetch_add(1, Ordering::Relaxed);
        self.allocated_bytes
            .fetch_add(size as u64, Ordering::Relaxed);
        self.live_bytes.fetch_add(size as i64, Ordering::Relaxed);
    }
    fn freed(&self, size: usize) {
        self.frees.fetch_add(1, Ordering::Relaxed);
        self.freed_bytes.fetch_add(size as u64, Ordering::Relaxed);
        self.live_bytes.fetch_sub(size as i64, Ordering::Relaxed);
    }
    fn reallocated(&self, old: usize, new: usize) {
        self.reallocs.fetch_add(1, Ordering::Relaxed);
        self.allocated_bytes
            .fetch_add(new as u64, Ordering::Relaxed);
        self.freed_bytes.fetch_add(old as u64, Ordering::Relaxed);
        if new >= old {
            self.live_bytes
                .fetch_add((new - old) as i64, Ordering::Relaxed);
        } else {
            self.live_bytes
                .fetch_sub((old - new) as i64, Ordering::Relaxed);
        }
    }
}

static COUNTERS: [Counters; 128] = [const { Counters::new() }; 128];
#[cfg(feature = "allocation-profiling")]
static NEXT_SHARD: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "allocation-profiling")]
thread_local! {
    // Constant, non-dropping TLS initialization allocates no heap memory.
    static SHARD: Cell<usize> = const { Cell::new(usize::MAX) };
}
#[cfg(feature = "allocation-profiling")]
fn thread_counters() -> &'static Counters {
    SHARD.with(|cell| {
        let mut index = cell.get();
        if index == usize::MAX {
            index = NEXT_SHARD.fetch_add(1, Ordering::Relaxed) % COUNTERS.len();
            cell.set(index);
        }
        &COUNTERS[index]
    })
}
#[cfg(feature = "allocation-profiling")]
struct ProfileAllocator;
#[cfg(feature = "allocation-profiling")]
#[global_allocator]
static ALLOCATOR: ProfileAllocator = ProfileAllocator;
// Only this opt-in integration-test binary uses the wrapper. Successful
// allocations retain the System allocator's exact pointer/layout contract.
#[cfg(feature = "allocation-profiling")]
unsafe impl GlobalAlloc for ProfileAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            thread_counters().allocated(layout.size());
        }
        pointer
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            thread_counters().allocated(layout.size());
        }
        pointer
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        thread_counters().freed(layout.size());
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, size) };
        if !result.is_null() {
            thread_counters().reallocated(layout.size(), size);
        }
        result
    }
}

#[derive(Clone, Copy, Default, Serialize)]
struct Network {
    tx_bytes: u64,
    rx_bytes: u64,
    tx_datagrams: u64,
    rx_datagrams: u64,
    tx_ios: u64,
    rx_ios: u64,
    lost_packets: u64,
    lost_bytes: u64,
    sent_packets: u64,
    congestion_events: u64,
}
impl Network {
    fn capture(connection: &quinn::Connection) -> Self {
        let stats = connection.stats();
        Self {
            tx_bytes: stats.udp_tx.bytes,
            rx_bytes: stats.udp_rx.bytes,
            tx_datagrams: stats.udp_tx.datagrams,
            rx_datagrams: stats.udp_rx.datagrams,
            tx_ios: stats.udp_tx.ios,
            rx_ios: stats.udp_rx.ios,
            lost_packets: stats.path.lost_packets,
            lost_bytes: stats.path.lost_bytes,
            sent_packets: stats.path.sent_packets,
            congestion_events: stats.path.congestion_events,
        }
    }
    fn checked_delta(self, before: Self) -> Result<Self, &'static str> {
        macro_rules! delta {
            ($name:ident) => {
                self.$name
                    .checked_sub(before.$name)
                    .ok_or("QUIC counter reset")?
            };
        }
        Ok(Self {
            tx_bytes: delta!(tx_bytes),
            rx_bytes: delta!(rx_bytes),
            tx_datagrams: delta!(tx_datagrams),
            rx_datagrams: delta!(rx_datagrams),
            tx_ios: delta!(tx_ios),
            rx_ios: delta!(rx_ios),
            lost_packets: delta!(lost_packets),
            lost_bytes: delta!(lost_bytes),
            sent_packets: delta!(sent_packets),
            congestion_events: delta!(congestion_events),
        })
    }
    fn add(&mut self, other: Self) {
        macro_rules! add { ($($name:ident),*) => { $(self.$name += other.$name;)* }; }
        add!(
            tx_bytes,
            rx_bytes,
            tx_datagrams,
            rx_datagrams,
            tx_ios,
            rx_ios,
            lost_packets,
            lost_bytes,
            sent_packets,
            congestion_events
        );
    }
}

pub struct Snapshot {
    cpu_user_us: u64,
    cpu_system_us: u64,
    minor_faults: u64,
    major_faults: u64,
    block_inputs: u64,
    block_outputs: u64,
    voluntary_switches: u64,
    involuntary_switches: u64,
    peak_rss_bytes: u64,
    resident_bytes: Option<u64>,
    sqlite_heap_bytes: u64,
    sqlite_heap_peak_bytes: u64,
    allocations: u64,
    deallocations: u64,
    reallocations: u64,
    allocated_bytes: u64,
    freed_bytes: u64,
    live_bytes: u64,
    network: Vec<Network>,
}

fn sqlite_heap() -> Result<(u64, u64), &'static str> {
    let mut current = 0;
    let mut peak = 0;
    // SQLite owns the counters and synchronizes access; reset=0 leaves its
    // lifetime high-water mark unchanged. Both output pointers are valid.
    let status = unsafe {
        rusqlite::ffi::sqlite3_status64(
            rusqlite::ffi::SQLITE_STATUS_MEMORY_USED,
            &mut current,
            &mut peak,
            0,
        )
    };
    if status != rusqlite::ffi::SQLITE_OK {
        return Err("SQLite memory counters unavailable");
    }
    Ok((
        u64::try_from(current).map_err(|_| "negative SQLite heap usage")?,
        u64::try_from(peak).map_err(|_| "negative SQLite heap peak")?,
    ))
}

#[test]
fn sqlite_foreign_heap_is_observed_separately_from_rust_allocations() {
    let before = sqlite_heap().unwrap().0;
    let connection = rusqlite::Connection::open_in_memory().unwrap();
    connection.execute_batch("CREATE TABLE heap_probe (payload BLOB); INSERT INTO heap_probe VALUES (zeroblob(4096));").unwrap();
    let during = sqlite_heap().unwrap();
    assert!(during.0 >= before + 4096);
    assert!(during.1 >= during.0);
    drop(connection);
    assert!(sqlite_heap().unwrap().0 < during.0);
}

#[cfg(target_os = "macos")]
#[allow(deprecated)] // libc retains the Darwin ABI; only this diagnostic uses it.
fn resident_bytes() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::mach_task_basic_info>::uninit();
    let mut count = libc::MACH_TASK_BASIC_INFO_COUNT;
    let result = unsafe {
        libc::task_info(
            libc::mach_task_self(),
            libc::MACH_TASK_BASIC_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if result != libc::KERN_SUCCESS || count != libc::MACH_TASK_BASIC_INFO_COUNT {
        return None;
    }
    Some(unsafe { info.assume_init() }.resident_size)
}
#[cfg(target_os = "linux")]
fn resident_bytes() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: u64 = text.split_whitespace().nth(1)?.parse().ok()?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    pages.checked_mul(u64::try_from(page_size).ok()?)
}
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn resident_bytes() -> Option<u64> {
    None
}

impl Snapshot {
    pub fn capture(clients: &[super::Client]) -> Result<Self, &'static str> {
        if clients.is_empty() {
            return Err("resource profile requires clients");
        }
        let network = clients
            .iter()
            .map(|client| Network::capture(&client.connection))
            .collect();
        let resident_bytes = resident_bytes();
        let (sqlite_heap_bytes, sqlite_heap_peak_bytes) = sqlite_heap()?;
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
            return Err("getrusage unavailable");
        }
        let usage = unsafe { usage.assume_init() };
        let number = |value| u64::try_from(value).map_err(|_| "negative process counter");
        let time = |value: libc::timeval| {
            number(value.tv_sec)?
                .checked_mul(1_000_000)
                .and_then(|n| n.checked_add(u64::try_from(value.tv_usec).ok()?))
                .ok_or("invalid CPU counter")
        };
        #[cfg(target_os = "macos")]
        let peak_rss_bytes = number(usage.ru_maxrss)?;
        #[cfg(not(target_os = "macos"))]
        let peak_rss_bytes = number(usage.ru_maxrss)?
            .checked_mul(1024)
            .ok_or("RSS overflow")?;
        let total = |field: fn(&Counters) -> &AtomicU64| {
            COUNTERS
                .iter()
                .map(|c| field(c).load(Ordering::Relaxed))
                .sum()
        };
        let live: i64 = COUNTERS
            .iter()
            .map(|c| c.live_bytes.load(Ordering::Relaxed))
            .sum();
        Ok(Self {
            cpu_user_us: time(usage.ru_utime)?,
            cpu_system_us: time(usage.ru_stime)?,
            minor_faults: number(usage.ru_minflt)?,
            major_faults: number(usage.ru_majflt)?,
            block_inputs: number(usage.ru_inblock)?,
            block_outputs: number(usage.ru_oublock)?,
            voluntary_switches: number(usage.ru_nvcsw)?,
            involuntary_switches: number(usage.ru_nivcsw)?,
            peak_rss_bytes,
            resident_bytes,
            sqlite_heap_bytes,
            sqlite_heap_peak_bytes,
            allocations: total(|c| &c.allocs),
            deallocations: total(|c| &c.frees),
            reallocations: total(|c| &c.reallocs),
            allocated_bytes: total(|c| &c.allocated_bytes),
            freed_bytes: total(|c| &c.freed_bytes),
            live_bytes: u64::try_from(live).map_err(|_| "inconsistent live allocation snapshot")?,
            network,
        })
    }
    pub fn delta(&self, before: &Self) -> Result<serde_json::Value, &'static str> {
        macro_rules! delta {
            ($name:ident) => {
                self.$name
                    .checked_sub(before.$name)
                    .ok_or("process counter reset")?
            };
        }
        if self.network.len() != before.network.len() {
            return Err("resource profile client count changed");
        }
        let mut network = Network::default();
        for (after, before) in self.network.iter().zip(before.network.iter().copied()) {
            network.add(after.checked_delta(before)?);
        }
        let allocation_profile = cfg!(feature = "allocation-profiling");
        Ok(
            serde_json::json!({"schema":"mount-rs-process-resource-v1", "rust_allocator_instrumented":allocation_profile,
            "cpu_user_us":delta!(cpu_user_us), "cpu_system_us":delta!(cpu_system_us),
            "minor_faults":delta!(minor_faults), "major_faults":delta!(major_faults),
            "block_inputs":delta!(block_inputs), "block_outputs":delta!(block_outputs),
            "voluntary_context_switches":delta!(voluntary_switches), "involuntary_context_switches":delta!(involuntary_switches),
            "rss_start_bytes":before.resident_bytes, "rss_end_bytes":self.resident_bytes,
            "lifetime_peak_rss_bytes":self.peak_rss_bytes,
            "sqlite_heap_start_bytes":before.sqlite_heap_bytes, "sqlite_heap_end_bytes":self.sqlite_heap_bytes,
            "sqlite_heap_lifetime_peak_bytes":self.sqlite_heap_peak_bytes,
            "rust_allocations":allocation_profile.then_some(delta!(allocations)), "rust_deallocations":allocation_profile.then_some(delta!(deallocations)), "rust_reallocations":allocation_profile.then_some(delta!(reallocations)),
            "rust_allocated_bytes":allocation_profile.then_some(delta!(allocated_bytes)), "rust_freed_bytes":allocation_profile.then_some(delta!(freed_bytes)),
            "rust_live_start_bytes":allocation_profile.then_some(before.live_bytes), "rust_live_end_bytes":allocation_profile.then_some(self.live_bytes),
            "quic_client_side":network,
            "scope":"one process includes clients, servers, verification and background; System Rust allocations exclude foreign C allocators; QUIC UDP bytes exclude IP/UDP headers; atomic allocator instrumentation affects throughput"}),
        )
    }
}

#[test]
fn quic_counter_reset_invalidates_resource_measurement() {
    let before = Network {
        rx_bytes: 10,
        lost_packets: 2,
        ..Network::default()
    };
    let after = Network {
        rx_bytes: 20,
        lost_packets: 4,
        ..Network::default()
    };
    let delta = after.checked_delta(before).unwrap();
    assert_eq!(delta.rx_bytes, 10);
    assert_eq!(delta.lost_packets, 2);
    assert!(before.checked_delta(after).is_err());
}

#[test]
fn allocation_churn_reconciles_live_bytes_through_shrink_and_growth() {
    let counters = Counters::default();
    counters.allocated(4096);
    counters.reallocated(4096, 8192);
    counters.reallocated(8192, 1024);
    assert_eq!(counters.live_bytes.load(Ordering::Relaxed), 1024);
    assert_eq!(counters.allocated_bytes.load(Ordering::Relaxed), 13312);
    assert_eq!(counters.freed_bytes.load(Ordering::Relaxed), 12288);
    counters.freed(1024);
    assert_eq!(counters.live_bytes.load(Ordering::Relaxed), 0);
    assert_eq!(counters.allocs.load(Ordering::Relaxed), 1);
    assert_eq!(counters.frees.load(Ordering::Relaxed), 1);
    assert_eq!(counters.reallocs.load(Ordering::Relaxed), 2);
}
