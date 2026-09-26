//! Diagnostic-only allocator and process counters. Never enabled in production.
#[path = "device_io.rs"]
pub mod device_io;
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
    os_io: device_io::Snapshot,
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
    pub fn resident_bytes(&self) -> Option<u64> {
        self.resident_bytes
    }
    pub fn capture(clients: &[super::Client]) -> Result<Self, &'static str> {
        let network = clients
            .iter()
            .map(|client| Network::capture(&client.connection))
            .collect();
        Self::capture_network(network)
    }

    /// Boundary-only network capture; never used by the100ms process sampler.
    pub fn capture_connections(connections: &[quinn::Connection]) -> Result<Self, &'static str> {
        Self::capture_network(connections.iter().map(Network::capture).collect())
    }
    /// Explicit phase boundary only: the100ms sampler never selects or queries
    /// disk counters. MOUNT_RS_PROFILE_IO=1 enables these new OS observations.
    pub fn capture_connections_io_boundary(
        connections: &[quinn::Connection],
    ) -> Result<Self, &'static str> {
        let mut snapshot = Self::capture_connections(connections)?;
        snapshot.os_io = device_io::Snapshot::capture_from_env();
        Ok(snapshot)
    }
    pub fn connection_deltas(&self, before: &Self) -> Result<serde_json::Value, &'static str> {
        if self.network.len() != before.network.len() {
            return Err("resource profile client count changed");
        }
        let deltas: Vec<_> = self
            .network
            .iter()
            .zip(&before.network)
            .enumerate()
            .map(|(lane, (after, before))| {
                Ok(serde_json::json!({"lane":lane,"quic":after.checked_delta(*before)?}))
            })
            .collect::<Result<_, &'static str>>()?;
        Ok(serde_json::Value::Array(deltas))
    }
    /// Observe namespace/population/setup before any protocol clients exist.
    pub fn capture_process() -> Result<Self, &'static str> {
        Self::capture_network(Vec::new())
    }
    /// Collect own-process disk accounting and one explicitly selected host
    /// device at a phase boundary. Missing counters remain diagnostic gaps.
    pub fn capture_process_io_boundary() -> Result<Self, &'static str> {
        let mut snapshot = Self::capture_process()?;
        snapshot.os_io = device_io::Snapshot::capture_from_env();
        Ok(snapshot)
    }

    fn capture_network(network: Vec<Network>) -> Result<Self, &'static str> {
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
            os_io: device_io::Snapshot::disabled(),
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
            "os_io":self.os_io.delta(&before.os_io),
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

#[test]
fn process_resources_are_available_before_client_connections_exist() {
    let before = Snapshot::capture(&[]).expect("setup process snapshot unavailable");
    let after = Snapshot::capture_process().expect("setup process snapshot unavailable");
    let delta = after.delta(&before).expect("setup process delta invalid");
    assert!(delta["cpu_user_us"].is_number());
}

/// Fixed gauges only; cumulative CPU/network counters are phase boundaries.
#[derive(Clone, Copy, Default)]
pub struct ProcessSample {
    pub resident_bytes: Option<u64>,
    pub sqlite_heap_bytes: u64,
    pub rust_live_bytes: Option<u64>,
}
#[derive(Clone, Default, serde::Serialize)]
pub struct ProcessSummary {
    pub samples: u64,
    pub resident_peak_bytes: Option<u64>,
    pub resident_unavailable_samples: u64,
    pub sqlite_heap_peak_bytes: u64,
    pub rust_live_peak_bytes: Option<u64>,
    pub capture_errors: u64,
}
impl ProcessSummary {
    pub fn observe(&mut self, sample: ProcessSample) {
        self.samples += 1;
        match sample.resident_bytes {
            Some(bytes) => {
                self.resident_peak_bytes = Some(self.resident_peak_bytes.unwrap_or(0).max(bytes))
            }
            None => self.resident_unavailable_samples += 1,
        }
        self.sqlite_heap_peak_bytes = self.sqlite_heap_peak_bytes.max(sample.sqlite_heap_bytes);
        if let Some(bytes) = sample.rust_live_bytes {
            self.rust_live_peak_bytes = Some(self.rust_live_peak_bytes.unwrap_or(0).max(bytes));
        }
    }
}
type SamplerReadings = (bool, ProcessSummary, ProcessSummary);
type SamplerShared = (std::sync::Mutex<SamplerReadings>, std::sync::Condvar);
pub struct ProcessSampler {
    state: std::sync::Arc<SamplerShared>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl ProcessSampler {
    pub fn start(interval: std::time::Duration) -> Result<Self, &'static str> {
        Self::start_with(interval, || {
            let snapshot = Snapshot::capture_process()?;
            Ok(ProcessSample {
                resident_bytes: snapshot.resident_bytes,
                sqlite_heap_bytes: snapshot.sqlite_heap_bytes,
                rust_live_bytes: cfg!(feature = "allocation-profiling")
                    .then_some(snapshot.live_bytes),
            })
        })
    }
    pub fn start_with<F>(interval: std::time::Duration, capture: F) -> Result<Self, &'static str>
    where
        F: Fn() -> Result<ProcessSample, &'static str> + Send + 'static,
    {
        if interval.is_zero() {
            return Err("sampler interval must be positive");
        }
        let mut summary = ProcessSummary::default();
        match capture() {
            Ok(sample) => summary.observe(sample),
            Err(_) => summary.capture_errors += 1,
        }
        let state = std::sync::Arc::new((
            std::sync::Mutex::new((false, summary.clone(), summary)),
            std::sync::Condvar::new(),
        ));
        let thread_state = state.clone();
        let worker = std::thread::Builder::new()
            .name("qualification-process-sampler".into())
            .spawn(move || {
                let (lock, wake) = &*thread_state;
                loop {
                    let guard = lock.lock().unwrap();
                    let (guard, _) = wake
                        .wait_timeout_while(guard, interval, |state| !state.0)
                        .unwrap();
                    if guard.0 {
                        break;
                    }
                    drop(guard);
                    let sampled = capture();
                    let mut guard = lock.lock().unwrap();
                    match sampled {
                        Ok(sample) => {
                            guard.1.observe(sample);
                            guard.2.observe(sample);
                        }
                        Err(_) => {
                            guard.1.capture_errors += 1;
                            guard.2.capture_errors += 1;
                        }
                    }
                }
            })
            .map_err(|_| "sampler thread creation failed")?;
        Ok(Self {
            state,
            worker: Some(worker),
        })
    }
    pub fn checkpoint(&self) -> Result<ProcessSummary, &'static str> {
        let mut guard = self.state.0.lock().map_err(|_| "sampler state poisoned")?;
        Ok(std::mem::take(&mut guard.1))
    }
    fn stop(&mut self) -> Result<(), &'static str> {
        {
            let mut guard = self.state.0.lock().map_err(|_| "sampler state poisoned")?;
            guard.0 = true;
        }
        self.state.1.notify_all();
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "sampler worker panicked")?;
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<ProcessSummary, &'static str> {
        self.stop()?;
        let mut guard = self.state.0.lock().map_err(|_| "sampler state poisoned")?;
        Ok(std::mem::take(&mut guard.2))
    }
}
impl Drop for ProcessSampler {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

// Behavioral contracts for the bounded continuous sampler.
#[cfg(test)]
mod continuous_sampler_contracts {
    use super::{ProcessSample, ProcessSampler, ProcessSummary};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Duration;

    #[test]
    fn summary_retains_transient_resident_and_sqlite_peaks_in_constant_space() {
        let mut summary = ProcessSummary::default();
        let storage_size = std::mem::size_of_val(&summary);
        summary.observe(ProcessSample {
            resident_bytes: Some(10),
            sqlite_heap_bytes: 5,
            rust_live_bytes: None,
        });
        summary.observe(ProcessSample {
            resident_bytes: Some(900),
            sqlite_heap_bytes: 700,
            rust_live_bytes: None,
        });
        for _ in 0..100_000 {
            summary.observe(ProcessSample {
                resident_bytes: Some(20),
                sqlite_heap_bytes: 8,
                rust_live_bytes: None,
            });
        }
        assert_eq!(summary.samples, 100_002);
        assert_eq!(summary.resident_peak_bytes, Some(900));
        assert_eq!(summary.sqlite_heap_peak_bytes, 700);
        assert_eq!(std::mem::size_of_val(&summary), storage_size);
        // The summary exposes no sample history: its retained storage is fixed.
        assert!(storage_size <= 128);
    }

    #[test]
    fn unavailable_resident_sample_is_not_reported_as_zero() {
        let mut summary = ProcessSummary::default();
        summary.observe(ProcessSample {
            resident_bytes: None,
            sqlite_heap_bytes: 3,
            rust_live_bytes: None,
        });
        assert_eq!(summary.resident_peak_bytes, None);
        assert_eq!(summary.resident_unavailable_samples, 1);
    }

    #[test]
    fn early_drop_joins_worker_and_stops_sampling() {
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        let sampler = ProcessSampler::start_with(Duration::from_millis(1), move || {
            observed.fetch_add(1, Ordering::SeqCst);
            let _ = sender.send(());
            Ok(ProcessSample {
                resident_bytes: Some(12),
                sqlite_heap_bytes: 6,
                rust_live_bytes: None,
            })
        })
        .unwrap();
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        drop(sampler);
        let count = calls.load(Ordering::SeqCst);
        // Joined worker owns the only sender; disconnect proves termination
        // without sleeping and hoping no new sample appears.
        while receiver.try_recv().is_ok() {}
        assert!(matches!(
            receiver.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ));
        assert_eq!(calls.load(Ordering::SeqCst), count);
    }

    #[test]
    fn transient_sqlite_allocation_is_observed_before_free() {
        let (sender, receiver) = std::sync::mpsc::channel();
        let sampler = ProcessSampler::start_with(Duration::from_millis(2), move || {
            let (heap, _) = super::sqlite_heap()?;
            let _ = sender.send(heap);
            Ok(ProcessSample {
                resident_bytes: super::resident_bytes(),
                sqlite_heap_bytes: heap,
                rust_live_bytes: None,
            })
        })
        .unwrap();
        let baseline = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        let allocation = unsafe { rusqlite::ffi::sqlite3_malloc64(8 * 1024 * 1024) };
        assert!(!allocation.is_null());
        let observed = loop {
            let heap = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
            if heap >= baseline + 8 * 1024 * 1024 {
                break heap;
            }
        };
        unsafe {
            rusqlite::ffi::sqlite3_free(allocation);
        }
        let (final_heap, _) = super::sqlite_heap().unwrap();
        assert!(final_heap < observed);
        let summary = sampler.finish().unwrap();
        assert!(summary.sqlite_heap_peak_bytes >= observed);
    }

    #[test]
    fn capture_failure_is_retained_and_finish_is_bounded() {
        let sampler = ProcessSampler::start_with(Duration::from_millis(1), || {
            Err("process counters unavailable")
        })
        .unwrap();
        let summary = sampler.finish().unwrap();
        assert!(summary.capture_errors >= 1);
        assert_eq!(summary.samples, 0);
    }
}
