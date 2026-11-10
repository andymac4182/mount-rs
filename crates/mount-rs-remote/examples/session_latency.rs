//! Closed-loop latency audit for the public session path:
//! `RemoteFs -> HTTP session -> owner session -> ChunkedFs -> blocks`.
//!
//! Everything runs in one process on loopback with no external services, so a
//! run takes seconds. `BENCH_SURFACE=direct` drives the owner driver without
//! HTTP, which separates transport cost from filesystem cost.
//!
//! ```sh
//! ./scripts/cargo-shared run --release -p mount-rs-remote --example session_latency
//! ```
//!
//! Environment (defaults in brackets):
//! `BENCH_SURFACE` remote|direct [remote], `BENCH_HTTP2` 0|1 [0, as the CLI],
//! `BENCH_HEADER_READS` 0|1 [0], `BENCH_CLIENTS` [16], `BENCH_SECONDS` [3],
//! `BENCH_FILES` namespace filler files [2000], `BENCH_DIR_ENTRIES` [64],
//! `BENCH_FILE_BYTES` [1048576], `BENCH_CHUNK_BYTES` [65536],
//! `BENCH_METADATA` memory|sqlite [memory], `BENCH_BLOCKS` memory|local|sqlite
//! [memory], `BENCH_BLOCK_CACHE` 0|1 [1], `BENCH_SHARED_HANDLE` 0|1 [0],
//! `BENCH_WRITER` 0|1 [0] (one background writer during every phase),
//! `BENCH_OPS` comma list of read,stat,readdir,open,mixed [all],
//! `BENCH_SERVER_THREADS` [4], `BENCH_DIR` scratch root for sqlite/local.
use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
use mount_rs_core::{FileHandle, FsDriver, MkdirOptions};
use mount_rs_remote::RemoteFs;
use mount_rs_sdk::{BlockCacheOptions, Filesystem, SplitOptions, SplitRuntimeOptions, StoreConfig};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

type Failure = Box<dyn std::error::Error + Send + Sync>;

fn number(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .map(|value| {
            value
                .parse()
                .unwrap_or_else(|_| panic!("{name} must be a number"))
        })
        .unwrap_or(default)
}

fn flag(name: &str, default: bool) -> bool {
    match std::env::var(name).as_deref() {
        Ok("1") => true,
        Ok("0") => false,
        Err(_) => default,
        _ => panic!("{name} must be 0 or 1"),
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Op {
    Read,
    Stat,
    Readdir,
    Open,
}

const OPS: [Op; 4] = [Op::Read, Op::Stat, Op::Readdir, Op::Open];

struct Worker {
    driver: Arc<dyn FsDriver>,
    handle: Arc<dyn FileHandle>,
    index: usize,
    file_bytes: u64,
    files: usize,
    state: u64,
}

impl Worker {
    fn random(&mut self) -> u64 {
        // xorshift64*: deterministic per worker, no shared state.
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        self.state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    async fn run(&mut self, op: Op) -> Result<(), Failure> {
        match op {
            Op::Read => {
                let mut buffer = [0u8; 4096];
                let blocks = (self.file_bytes / 4096).max(1);
                let position = (self.random() % blocks) * 4096;
                let count = self.handle.read(&mut buffer, Some(position)).await?;
                if count != 4096 {
                    return Err(format!("short read {count} at {position}").into());
                }
            }
            Op::Stat => {
                let target = self.random() as usize % self.files.max(1);
                self.driver.stat(&format!("/fill/f{target:06}")).await?;
            }
            Op::Readdir => {
                let entries = self.driver.readdir("/listing").await?;
                if entries.is_empty() {
                    return Err("empty listing".into());
                }
            }
            Op::Open => {
                let handle = self
                    .driver
                    .open(&format!("/data/d{:03}", self.index), "r", 0)
                    .await?;
                handle.close().await?;
            }
        }
        Ok(())
    }
}

fn percentile(sorted: &[u64], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let rank = ((sorted.len() as f64) * fraction).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1] as f64 / 1e6
}

fn report(phase: &str, op: Op, mut samples: Vec<u64>, seconds: f64) {
    samples.sort_unstable();
    println!(
        "{phase:<8} {:<8} n={:<8} ops/s={:<9.0} p50={:<8.3} p99={:<8.3} p99.9={:<8.3} max={:.3} ms",
        format!("{op:?}").to_lowercase(),
        samples.len(),
        samples.len() as f64 / seconds,
        percentile(&samples, 0.50),
        percentile(&samples, 0.99),
        percentile(&samples, 0.999),
        samples.last().copied().unwrap_or(0) as f64 / 1e6,
    );
}

async fn phase(
    name: &str,
    ops: &[Op],
    workers: Vec<Worker>,
    duration: Duration,
) -> Result<Vec<Worker>, Failure> {
    let mut tasks = Vec::new();
    let started = Instant::now();
    // Workers watch the clock themselves: on the direct surface an operation
    // may never yield, so a timer on this runtime could not stop them.
    let deadline = started + duration;
    for mut worker in workers {
        let ops = ops.to_vec();
        tasks.push(tokio::spawn(async move {
            let mut samples: Vec<Vec<u64>> = vec![Vec::new(); OPS.len()];
            let mut turn = worker.index;
            while Instant::now() < deadline {
                let op = ops[turn % ops.len()];
                turn += 1;
                let begin = Instant::now();
                worker.run(op).await?;
                let slot = OPS.iter().position(|candidate| *candidate == op).unwrap();
                samples[slot].push(begin.elapsed().as_nanos() as u64);
            }
            Ok::<_, Failure>((worker, samples))
        }));
    }
    let mut merged: Vec<Vec<u64>> = vec![Vec::new(); OPS.len()];
    let mut workers = Vec::new();
    for task in tasks {
        let (worker, samples) = task.await??;
        workers.push(worker);
        for (slot, values) in samples.into_iter().enumerate() {
            merged[slot].extend(values);
        }
    }
    let seconds = started.elapsed().as_secs_f64();
    for (slot, samples) in merged.into_iter().enumerate() {
        if !samples.is_empty() {
            report(name, OPS[slot], samples, seconds);
        }
    }
    Ok(workers)
}

fn main() -> Result<(), Failure> {
    let client_threads = number("BENCH_CLIENT_THREADS", 4);
    let server_threads = number("BENCH_SERVER_THREADS", 4);
    // The owner runs on its own runtime so client scheduling cannot hide or
    // inflate server-side latency.
    let server = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(server_threads)
        .thread_name("owner")
        .enable_all()
        .build()?;
    let client = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(client_threads)
        .thread_name("client")
        .enable_all()
        .build()?;
    let clients = number("BENCH_CLIENTS", 16);
    let seconds = number("BENCH_SECONDS", 3);
    let files = number("BENCH_FILES", 2000);
    let directory_entries = number("BENCH_DIR_ENTRIES", 64);
    let file_bytes = number("BENCH_FILE_BYTES", 1 << 20);
    let chunk_bytes = number("BENCH_CHUNK_BYTES", 65536);
    let http2 = flag("BENCH_HTTP2", false);
    let header_reads = flag("BENCH_HEADER_READS", false);
    let shared_handle = flag("BENCH_SHARED_HANDLE", false);
    let writer = flag("BENCH_WRITER", false);
    let surface = std::env::var("BENCH_SURFACE").unwrap_or_else(|_| "remote".into());
    let metadata = std::env::var("BENCH_METADATA").unwrap_or_else(|_| "memory".into());
    let blocks = std::env::var("BENCH_BLOCKS").unwrap_or_else(|_| "memory".into());
    let selected =
        std::env::var("BENCH_OPS").unwrap_or_else(|_| "read,stat,readdir,open,mixed".into());
    let root = std::env::var("BENCH_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
        .join(format!("session-latency-{}", std::process::id()));
    std::fs::create_dir_all(&root)?;

    let mut options = SplitOptions::memory("session-latency", chunk_bytes);
    options.metadata = match metadata.as_str() {
        "memory" => StoreConfig::Memory,
        "sqlite" => StoreConfig::Sqlite {
            path: root.join("metadata.sqlite"),
        },
        _ => return Err("BENCH_METADATA must be memory or sqlite".into()),
    };
    options.blocks = match blocks.as_str() {
        "memory" => StoreConfig::Memory,
        "local" => {
            let path = root.join("blocks");
            std::fs::create_dir_all(&path)?;
            StoreConfig::Local { path }
        }
        "sqlite" => StoreConfig::Sqlite {
            path: root.join("blocks.sqlite"),
        },
        _ => return Err("BENCH_BLOCKS must be memory, local or sqlite".into()),
    };
    let runtime = SplitRuntimeOptions {
        block_cache: flag("BENCH_BLOCK_CACHE", true).then_some(BlockCacheOptions {
            max_bytes: 4 * 1024 * 1024,
            max_entries: 128,
        }),
        ..Default::default()
    };
    println!(
        "surface={surface} http2={http2} header_reads={header_reads} clients={clients} files={files} dir_entries={directory_entries} file_bytes={file_bytes} chunk_bytes={chunk_bytes} metadata={metadata} blocks={blocks} block_cache={} shared_handle={shared_handle} writer={writer} server_threads={server_threads} profile={}",
        runtime.block_cache.is_some(),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );

    let secret = "s".repeat(32);
    let (owner, registry, http, url) = server.block_on(async {
        let owner = Arc::new(Filesystem::split_with_runtime(options, runtime).await?);
        let driver = owner.driver();
        let populate = Instant::now();
        for directory in ["/fill", "/listing", "/data"] {
            driver.mkdir(directory, MkdirOptions::default()).await?;
        }
        for index in 0..files {
            driver
                .write_file(&format!("/fill/f{index:06}"), b"x")
                .await?;
        }
        for index in 0..directory_entries {
            driver
                .write_file(&format!("/listing/e{index:05}"), b"x")
                .await?;
        }
        let payload: Vec<u8> = (0..file_bytes).map(|index| (index % 251) as u8).collect();
        for index in 0..clients.max(1) {
            driver
                .write_file(&format!("/data/d{index:03}"), &payload)
                .await?;
        }
        driver.syncfs().await?;
        println!(
            "populate: {} whole-file writes in {:.2}s",
            files + directory_entries + clients.max(1),
            populate.elapsed().as_secs_f64()
        );
        let registry = Arc::new(VolumeSessions::new(
            owner.clone(),
            64,
            4096,
            Duration::from_secs(600),
        )?);
        let handler = SessionHttpHandler::new(
            "volume",
            SessionCredentials::new(vec![("client".into(), secret.clone())])?,
            registry.clone(),
        )?;
        let http = mount_rs_http::HttpServer::start_with_handler(
            Arc::new(handler),
            mount_rs_http::HttpServerOptions {
                http2_prior_knowledge: http2,
                ..Default::default()
            },
        )
        .await?;
        let url = http.url();
        Ok::<_, Failure>((owner, registry, http, url))
    })?;

    client.block_on(async {
        let remote = if surface == "remote" {
            let remote = RemoteFs::connect_with_http2(
                &url,
                "volume",
                &secret,
                Duration::from_secs(30),
                http2,
            )
            .await?;
            if header_reads {
                remote.enable_header_reads().await?;
            }
            Some(Arc::new(remote))
        } else if surface == "direct" {
            None
        } else {
            return Err("BENCH_SURFACE must be remote or direct".into());
        };
        let driver: Arc<dyn FsDriver> = match &remote {
            Some(remote) => remote.clone(),
            None => owner.driver(),
        };
        let mut workers = Vec::new();
        let shared = driver.open("/data/d000", "r", 0).await?;
        for index in 0..clients {
            let handle = if shared_handle {
                shared.clone()
            } else {
                driver.open(&format!("/data/d{index:03}"), "r", 0).await?
            };
            workers.push(Worker {
                driver: driver.clone(),
                handle,
                index,
                file_bytes: file_bytes as u64,
                files,
                state: 0x9e37_79b9_7f4a_7c15 ^ ((index as u64 + 1) << 32),
            });
        }
        let stop_writer = Arc::new(AtomicBool::new(false));
        let background = writer.then(|| {
            let driver = driver.clone();
            let stop = stop_writer.clone();
            tokio::spawn(async move {
                let mut turn = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    turn += 1;
                    driver
                        .write_file(&format!("/fill/w{:04}", turn % 64), &turn.to_le_bytes())
                        .await?;
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                Ok::<_, Failure>(turn)
            })
        });
        let duration = Duration::from_secs(seconds as u64);
        // Warm connections, caches and allocator before any measured phase.
        workers = phase_quiet(workers).await?;
        for name in selected.split(',') {
            let ops: &[Op] = match name {
                "read" => &[Op::Read],
                "stat" => &[Op::Stat],
                "readdir" => &[Op::Readdir],
                "open" => &[Op::Open],
                "mixed" => &OPS,
                _ => return Err(format!("unknown BENCH_OPS entry {name}").into()),
            };
            workers = phase(name, ops, workers, duration).await?;
        }
        stop_writer.store(true, Ordering::Relaxed);
        if let Some(task) = background {
            println!("background writes={}", task.await??);
        }
        for worker in workers {
            if !shared_handle {
                worker.handle.close().await?;
            }
        }
        shared.close().await?;
        if let Some(remote) = remote {
            remote.shutdown().await?;
        }
        Ok::<_, Failure>(())
    })?;
    server.block_on(async {
        http.close().await?;
        registry.close().await?;
        owner.shutdown().await?;
        Ok::<_, Failure>(())
    })?;
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

async fn phase_quiet(mut workers: Vec<Worker>) -> Result<Vec<Worker>, Failure> {
    for worker in &mut workers {
        for op in OPS {
            for _ in 0..20 {
                worker.run(op).await?;
            }
        }
    }
    Ok(workers)
}
