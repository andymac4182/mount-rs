//! Reproducible Rust-side dispatch baseline for the core async-trait contract.
//!
//! This example intentionally measures the current contract, including its
//! boxed async-trait futures and Arc<dyn FileHandle> result from open.
//! It is a baseline, not an adapter optimization or an alternative trait
//! design.

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::{FileHandle, FsDriver, MemoryFs};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use serde_json::{Value, json};
use std::alloc::{GlobalAlloc, Layout, System};
use std::env;
use std::fmt::Display;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

struct CountingAllocator;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static REALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);
static DEALLOCATED_BYTES: AtomicU64 = AtomicU64::new(0);

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        DEALLOCATED_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let result = unsafe { System.realloc(pointer, layout, new_size) };
        if !result.is_null() {
            REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            if new_size > layout.size() {
                ALLOCATED_BYTES.fetch_add((new_size - layout.size()) as u64, Ordering::Relaxed);
            } else {
                DEALLOCATED_BYTES.fetch_add((layout.size() - new_size) as u64, Ordering::Relaxed);
            }
        }
        result
    }
}

#[derive(Clone, Copy)]
struct CounterSnapshot {
    allocations: u64,
    deallocations: u64,
    reallocations: u64,
    allocated_bytes: u64,
    deallocated_bytes: u64,
}

fn counters() -> CounterSnapshot {
    CounterSnapshot {
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
        reallocations: REALLOCATIONS.load(Ordering::Relaxed),
        allocated_bytes: ALLOCATED_BYTES.load(Ordering::Relaxed),
        deallocated_bytes: DEALLOCATED_BYTES.load(Ordering::Relaxed),
    }
}

fn reset_counters() {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    DEALLOCATIONS.store(0, Ordering::Relaxed);
    REALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATED_BYTES.store(0, Ordering::Relaxed);
    DEALLOCATED_BYTES.store(0, Ordering::Relaxed);
}

#[derive(Clone)]
struct Config {
    warmup: u64,
    iterations: u64,
    samples: u32,
    concurrency: Vec<usize>,
    payload_sizes: Vec<usize>,
    chunk_sizes: Vec<usize>,
    operations: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            warmup: 100,
            iterations: 500,
            samples: 5,
            concurrency: vec![1],
            payload_sizes: vec![32, 4096],
            chunk_sizes: vec![4096],
            operations: vec![
                "stat".to_owned(),
                "open_close".to_owned(),
                "read".to_owned(),
                "write".to_owned(),
            ],
        }
    }
}

#[derive(Clone, Copy)]
struct TimedBatch {
    duration_ns: u64,
    counters: CounterSnapshot,
}

type BenchResult<T> = std::result::Result<T, String>;

fn parse_positive<T>(value: &str, name: &str) -> BenchResult<T>
where
    T: std::str::FromStr,
    T::Err: Display,
{
    value
        .parse::<T>()
        .map_err(|error| format!("invalid {name} value {value:?}: {error}"))
}

fn parse_list<T>(value: &str, name: &str) -> BenchResult<Vec<T>>
where
    T: std::str::FromStr,
    T::Err: Display,
{
    let values = value
        .split(',')
        .map(|entry| parse_positive(entry.trim(), name))
        .collect::<BenchResult<Vec<T>>>()?;
    if values.is_empty() {
        return Err(format!("{name} must not be empty"));
    }
    Ok(values)
}

fn parse_operations(value: &str) -> BenchResult<Vec<String>> {
    let operations = value
        .split(',')
        .map(str::trim)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if operations.is_empty()
        || operations.iter().any(|operation| {
            !matches!(operation.as_str(), "stat" | "open_close" | "read" | "write")
        })
    {
        return Err(format!(
            "operations must contain only stat, open_close, read, write: {value:?}"
        ));
    }
    Ok(operations)
}

fn parse_args() -> BenchResult<Config> {
    let mut config = Config::default();
    let mut args = env::args().skip(1);
    while let Some(argument) = args.next() {
        let value = |args: &mut std::iter::Skip<std::env::Args>, name: &str| {
            args.next()
                .ok_or_else(|| format!("missing value for {name}"))
        };
        match argument.as_str() {
            "--help" => {
                eprintln!(
                    "usage: dispatch_baseline [--warmup N] [--iterations N] [--samples N] \
                     [--concurrency N[,N...]] [--payload-sizes N[,N...]] \
                     [--chunk-sizes N[,N...]] [--operations stat,open_close,read,write]"
                );
                std::process::exit(0);
            }
            "--warmup" => config.warmup = parse_positive(&value(&mut args, "--warmup")?, "warmup")?,
            "--iterations" => {
                config.iterations =
                    parse_positive(&value(&mut args, "--iterations")?, "iterations")?
            }
            "--samples" => {
                config.samples = parse_positive(&value(&mut args, "--samples")?, "samples")?
            }
            "--concurrency" => {
                config.concurrency = parse_list(&value(&mut args, "--concurrency")?, "concurrency")?
            }
            "--payload-sizes" => {
                config.payload_sizes =
                    parse_list(&value(&mut args, "--payload-sizes")?, "payload-sizes")?
            }
            "--chunk-sizes" => {
                config.chunk_sizes = parse_list(&value(&mut args, "--chunk-sizes")?, "chunk-sizes")?
            }
            "--operations" => {
                config.operations = parse_operations(&value(&mut args, "--operations")?)?
            }
            other => return Err(format!("unknown argument {other:?}; use --help")),
        }
    }
    if config.iterations == 0 || config.samples == 0 {
        return Err("iterations and samples must be greater than zero".to_owned());
    }
    if config.concurrency.contains(&0)
        || config.payload_sizes.contains(&0)
        || config.chunk_sizes.contains(&0)
    {
        return Err(
            "concurrency, payload-sizes, and chunk-sizes must be greater than zero".to_owned(),
        );
    }
    Ok(config)
}

async fn seed_file<D>(driver: &D, path: &str, payload: &[u8]) -> BenchResult<()>
where
    D: FsDriver + ?Sized,
{
    let handle = driver
        .open(path, "w", 0o666)
        .await
        .map_err(|error| error.to_string())?;
    let written = handle
        .write(payload, Some(0))
        .await
        .map_err(|error| error.to_string())?;
    handle.close().await.map_err(|error| error.to_string())?;
    if written != payload.len() {
        return Err(format!(
            "seed write returned {written}, expected {}",
            payload.len()
        ));
    }
    Ok(())
}

async fn validate_file<D>(driver: &D, path: &str, payload: &[u8]) -> BenchResult<()>
where
    D: FsDriver + ?Sized,
{
    let handle = driver
        .open(path, "r", 0)
        .await
        .map_err(|error| error.to_string())?;
    let mut contents = vec![0; payload.len()];
    let read = handle
        .read(&mut contents, Some(0))
        .await
        .map_err(|error| error.to_string())?;
    handle.close().await.map_err(|error| error.to_string())?;
    if read != payload.len() || contents != payload {
        return Err(format!(
            "validation read returned {read} bytes or mismatched payload; expected {}",
            payload.len()
        ));
    }
    Ok(())
}

async fn close_handles(handles: Vec<Arc<dyn FileHandle>>) -> BenchResult<()> {
    for handle in handles {
        handle.close().await.map_err(|error| error.to_string())?;
    }
    Ok(())
}

async fn worker<D>(
    driver: Arc<D>,
    operation: &str,
    path: &str,
    payload: &[u8],
    handle: Option<Arc<dyn FileHandle>>,
    buffer: &mut [u8],
    iterations: u64,
) -> BenchResult<()>
where
    D: FsDriver + ?Sized + 'static,
{
    for _ in 0..iterations {
        match operation {
            "stat" => {
                driver.stat(path).await.map_err(|error| error.to_string())?;
            }
            "open_close" => {
                let opened = driver
                    .open(path, "r", 0)
                    .await
                    .map_err(|error| error.to_string())?;
                opened.close().await.map_err(|error| error.to_string())?;
            }
            "read" => {
                let file = handle
                    .as_ref()
                    .ok_or_else(|| "read worker missing file handle".to_owned())?;
                let read = file
                    .read(buffer, Some(0))
                    .await
                    .map_err(|error| error.to_string())?;
                if read != payload.len() {
                    return Err(format!("read returned {read}, expected {}", payload.len()));
                }
            }
            "write" => {
                let file = handle
                    .as_ref()
                    .ok_or_else(|| "write worker missing file handle".to_owned())?;
                let written = file
                    .write(payload, Some(0))
                    .await
                    .map_err(|error| error.to_string())?;
                if written != payload.len() {
                    return Err(format!(
                        "write returned {written}, expected {}",
                        payload.len()
                    ));
                }
            }
            other => return Err(format!("unsupported operation {other:?}")),
        }
    }
    Ok(())
}

async fn run_batch<D>(
    driver: Arc<D>,
    operation: &str,
    path: Arc<String>,
    payload: Arc<Vec<u8>>,
    handles: Arc<Vec<Arc<dyn FileHandle>>>,
    iterations: u64,
    concurrency: usize,
) -> BenchResult<TimedBatch>
where
    D: FsDriver + ?Sized + 'static,
{
    let mut buffers = (0..concurrency)
        .map(|_| vec![0_u8; payload.len()])
        .collect::<Vec<_>>();
    if matches!(operation, "read" | "write") && handles.len() != concurrency {
        return Err(format!(
            "{operation} prepared {} handles for concurrency {concurrency}",
            handles.len()
        ));
    }

    // Buffer setup is deliberately outside the timed region. Resetting here
    // makes allocation counts cover the async operation/control path instead
    // of per-sample fixture setup.
    reset_counters();
    let started = Instant::now();
    let operation = operation.to_owned();
    if concurrency == 1 {
        worker(
            driver,
            operation.as_str(),
            path.as_str(),
            payload.as_slice(),
            handles.first().cloned(),
            buffers
                .first_mut()
                .ok_or_else(|| "single-worker buffer missing".to_owned())?,
            iterations,
        )
        .await?;
    } else {
        let mut tasks = Vec::with_capacity(concurrency);
        for (index, buffer) in buffers.into_iter().enumerate() {
            let driver = Arc::clone(&driver);
            let path = Arc::clone(&path);
            let payload = Arc::clone(&payload);
            let handles = Arc::clone(&handles);
            let operation = operation.clone();
            tasks.push(tokio::spawn(async move {
                let mut buffer = buffer;
                worker(
                    driver,
                    operation.as_str(),
                    path.as_str(),
                    payload.as_slice(),
                    handles.get(index).cloned(),
                    &mut buffer,
                    iterations,
                )
                .await
            }));
        }
        for task in tasks {
            task.await
                .map_err(|error| format!("benchmark worker panicked: {error}"))??;
        }
    }
    let duration_ns = started.elapsed().as_nanos() as u64;
    Ok(TimedBatch {
        duration_ns,
        counters: counters(),
    })
}

async fn prepare_handles<D>(
    driver: &D,
    operation: &str,
    path: &str,
    concurrency: usize,
) -> BenchResult<Vec<Arc<dyn FileHandle>>>
where
    D: FsDriver + ?Sized,
{
    let Some(flags) = (match operation {
        "read" => Some("r"),
        "write" => Some("r+"),
        _ => None,
    }) else {
        return Ok(Vec::new());
    };
    let mut handles = Vec::with_capacity(concurrency);
    for _ in 0..concurrency {
        handles.push(
            driver
                .open(path, flags, 0)
                .await
                .map_err(|error| error.to_string())?,
        );
    }
    Ok(handles)
}

fn sample_json(
    operation: &str,
    payload_bytes: usize,
    concurrency: usize,
    sample_index: u32,
    iterations: u64,
    batch: TimedBatch,
) -> Value {
    let operations = iterations.saturating_mul(concurrency as u64);
    let counters = batch.counters;
    let ns_per_operation = batch.duration_ns as f64 / operations as f64;
    let operations_per_second = if batch.duration_ns == 0 {
        0.0
    } else {
        operations as f64 * 1_000_000_000.0 / batch.duration_ns as f64
    };
    let bytes_per_second = if batch.duration_ns == 0 || !matches!(operation, "read" | "write") {
        None
    } else {
        Some(payload_bytes as f64 * operations as f64 * 1_000_000_000.0 / batch.duration_ns as f64)
    };
    json!({
        "operation": operation,
        "payloadBytes": payload_bytes,
        "concurrency": concurrency,
        "sampleIndex": sample_index,
        "iterationsPerWorker": iterations,
        "operations": operations,
        "durationNs": batch.duration_ns,
        "nsPerOperation": ns_per_operation,
        "operationsPerSecond": operations_per_second,
        "bytesPerSecond": bytes_per_second,
        "allocationCount": counters.allocations,
        "deallocationCount": counters.deallocations,
        "reallocationCount": counters.reallocations,
        "allocatedBytes": counters.allocated_bytes,
        "deallocatedBytes": counters.deallocated_bytes,
        "allocationMeasurement": "global-allocator-deltas"
    })
}

async fn run_lane<D>(
    driver: Arc<D>,
    backend: &str,
    dispatch: &str,
    chunk_size: Option<usize>,
    config: &Config,
) -> BenchResult<Value>
where
    D: FsDriver + ?Sized + 'static,
{
    let mut samples = Vec::new();
    for &payload_size in &config.payload_sizes {
        let path = format!("/dispatch-baseline-{payload_size}");
        let payload = vec![0x5a; payload_size];
        seed_file(driver.as_ref(), &path, &payload).await?;
        let path = Arc::new(path);
        let payload = Arc::new(payload);

        for operation in &config.operations {
            for &concurrency in &config.concurrency {
                let handles =
                    prepare_handles(driver.as_ref(), operation, path.as_str(), concurrency).await?;
                let handles = Arc::new(handles);

                if config.warmup != 0 {
                    run_batch(
                        Arc::clone(&driver),
                        operation,
                        Arc::clone(&path),
                        Arc::clone(&payload),
                        Arc::clone(&handles),
                        config.warmup,
                        concurrency,
                    )
                    .await?;
                }
                for sample_index in 0..config.samples {
                    let batch = run_batch(
                        Arc::clone(&driver),
                        operation,
                        Arc::clone(&path),
                        Arc::clone(&payload),
                        Arc::clone(&handles),
                        config.iterations,
                        concurrency,
                    )
                    .await?;
                    samples.push(sample_json(
                        operation,
                        payload.len(),
                        concurrency,
                        sample_index,
                        config.iterations,
                        batch,
                    ));
                }
                let handles = Arc::try_unwrap(handles).unwrap_or_else(|handles| (*handles).clone());
                close_handles(handles).await?;
            }
        }
        validate_file(driver.as_ref(), path.as_str(), payload.as_slice()).await?;
    }

    Ok(json!({
        "surface": "rust",
        "backend": backend,
        "dispatch": dispatch,
        "futureRepresentation": "Pin<Box<dyn Future + Send>> (async-trait)",
        "genericReceiverStatus": if dispatch == "generic" {
            "generic receiver still boxed by async-trait"
        } else {
            "erased receiver"
        },
        "handleErasure": "Arc<dyn FileHandle> (core FsDriver::open contract)",
        "chunkSizeBytes": chunk_size,
        "allocationMeasurement": "global-allocator-deltas",
        "samples": samples
    }))
}

async fn run_chunked_pair(config: &Config, chunk_size: usize) -> BenchResult<Vec<Value>> {
    let sequence = format!("{}-{chunk_size}", std::process::id());
    let direct_fs = ChunkedFs::open(
        MemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedOptions::fixed(format!("dispatch-generic-{sequence}"), chunk_size)
            .map_err(|error| error.to_string())?,
    )
    .await
    .map_err(|error| error.to_string())?;
    let direct_output = run_lane(
        Arc::new(direct_fs.clone()),
        "chunked-memory",
        "generic",
        Some(chunk_size),
        config,
    )
    .await;
    let direct_shutdown = direct_fs
        .shutdown()
        .await
        .map_err(|error| error.to_string());
    let direct_output = direct_output?;
    direct_shutdown?;

    let erased_fs = ChunkedFs::open(
        MemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedOptions::fixed(format!("dispatch-erased-{sequence}"), chunk_size)
            .map_err(|error| error.to_string())?,
    )
    .await
    .map_err(|error| error.to_string())?;
    let erased_driver: Arc<dyn FsDriver> = Arc::new(erased_fs.clone());
    let erased_output = run_lane(
        erased_driver,
        "chunked-memory",
        "erased",
        Some(chunk_size),
        config,
    )
    .await;
    let erased_shutdown = erased_fs
        .shutdown()
        .await
        .map_err(|error| error.to_string());
    let erased_output = erased_output?;
    erased_shutdown?;
    Ok(vec![direct_output, erased_output])
}

async fn run() -> BenchResult<Value> {
    let config = parse_args()?;
    let mut lanes = Vec::new();

    let direct_memory = Arc::new(MemoryFs::empty());
    lanes.push(run_lane(direct_memory, "memory", "generic", None, &config).await?);
    let erased_memory: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    lanes.push(run_lane(erased_memory, "memory", "erased", None, &config).await?);

    for chunk_size in &config.chunk_sizes {
        lanes.extend(run_chunked_pair(&config, *chunk_size).await?);
    }

    Ok(json!({
        "schemaVersion": 1,
        "producer": "examples/dispatch_baseline.rs",
        "surface": "rust",
        "environment": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "rustDebugAssertions": cfg!(debug_assertions),
            "allocatorCounting": true
        },
        "config": {
            "warmup": config.warmup,
            "iterations": config.iterations,
            "samples": config.samples,
            "concurrency": config.concurrency,
            "payloadSizes": config.payload_sizes,
            "chunkSizes": config.chunk_sizes,
            "operations": config.operations
        },
        "lanes": lanes
    }))
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    match run().await {
        Ok(output) => match serde_json::to_string(&output) {
            Ok(serialized) => println!("{serialized}"),
            Err(error) => {
                eprintln!("failed to serialize dispatch baseline: {error}");
                std::process::exit(1);
            }
        },
        Err(error) => {
            eprintln!("dispatch baseline failed: {error}");
            std::process::exit(1);
        }
    }
}
