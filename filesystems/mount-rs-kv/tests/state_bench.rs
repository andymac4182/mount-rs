//! Throughput and latency of the paths that go through the adapter's
//! process-local tables (attributes, empty directories, open files).
//!
//! Run with:
//! `./scripts/cargo-shared test --release --locked -p mount-rs-kv --test state_bench -- --ignored --nocapture`

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mount_rs_core::MkdirOptions;
use mount_rs_core::driver::FsDriver;
use mount_rs_kv::{KeyValueFs, KeyValueStore, UnstorageOptions, create_unstorage_driver};

const SHARDS: usize = 64;
/// Paths the tables hold before a measurement starts, so a cost that grows
/// with the table shows.
const POPULATION: usize = 5000;
/// Files held open during the open-table measurement.
const OPEN_FILES: usize = 1000;
const RUN: Duration = Duration::from_millis(600);

/// A store that keeps out of the way: keys spread over many small maps, so
/// callers on different keys rarely meet.
type Shard = Mutex<HashMap<String, Vec<u8>>>;

#[derive(Clone)]
struct ShardedStore {
    shards: Arc<Vec<Shard>>,
}

impl ShardedStore {
    fn new() -> Self {
        Self {
            shards: Arc::new((0..SHARDS).map(|_| Mutex::new(HashMap::new())).collect()),
        }
    }

    fn shard(&self, key: &str) -> &Shard {
        let hash = key.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
        });
        &self.shards[(hash % SHARDS as u64) as usize]
    }
}

#[derive(Debug)]
struct Never;

impl std::fmt::Display for Never {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("unreachable")
    }
}

#[async_trait]
impl KeyValueStore for ShardedStore {
    type Error = Never;

    async fn has_item(&self, key: &str) -> Result<bool, Never> {
        Ok(self.shard(key).lock().expect("shard").contains_key(key))
    }

    async fn get_item_raw(&self, key: &str) -> Result<Option<Vec<u8>>, Never> {
        Ok(self.shard(key).lock().expect("shard").get(key).cloned())
    }

    async fn set_item_raw(&self, key: &str, value: Vec<u8>) -> Result<(), Never> {
        self.shard(key)
            .lock()
            .expect("shard")
            .insert(key.to_owned(), value);
        Ok(())
    }

    async fn remove_item(&self, key: &str) -> Result<(), Never> {
        self.shard(key).lock().expect("shard").remove(key);
        Ok(())
    }

    async fn get_keys(&self, prefix: &str) -> Result<Vec<String>, Never> {
        let below = format!("{prefix}:");
        let mut keys = Vec::new();
        for shard in self.shards.iter() {
            let shard = shard.lock().expect("shard");
            keys.extend(
                shard
                    .keys()
                    .filter(|key| prefix.is_empty() || key.starts_with(&below))
                    .cloned(),
            );
        }
        Ok(keys)
    }
}

type Fs = KeyValueFs<ShardedStore>;

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime")
        .block_on(future)
}

/// A driver whose tables already hold `POPULATION` directories.
fn populated() -> Fs {
    let fs = create_unstorage_driver(ShardedStore::new(), UnstorageOptions::default());
    block_on(async {
        for index in 0..POPULATION {
            fs.mkdir(&format!("/d{index}"), MkdirOptions::default())
                .await
                .expect("mkdir");
        }
    });
    fs
}

struct Report {
    operations: usize,
    elapsed: Duration,
    latencies: Vec<u32>,
}

/// Runs `work(thread)` on `threads` threads until the time is up. Each
/// thread gets its own state from `prepare` and times every operation.
fn measure<P, W, T>(name: &str, threads: usize, fs: &Fs, prepare: P, work: W)
where
    P: Fn(&Fs, usize) -> T + Sync,
    W: AsyncFn(&Fs, &mut T, u64) + Sync,
    T: Send,
{
    let start = Barrier::new(threads + 1);
    let stop = AtomicBool::new(false);
    let report = std::thread::scope(|scope| {
        let workers = (0..threads)
            .map(|thread| {
                let (start, stop, prepare, work) = (&start, &stop, &prepare, &work);
                scope.spawn(move || {
                    let mut state = prepare(fs, thread);
                    let mut latencies = Vec::with_capacity(1 << 20);
                    start.wait();
                    block_on(async {
                        let mut round = 0;
                        while !stop.load(Ordering::Relaxed) {
                            let began = Instant::now();
                            work(fs, &mut state, round).await;
                            latencies
                                .push(began.elapsed().as_nanos().min(u128::from(u32::MAX)) as u32);
                            round += 1;
                        }
                    });
                    latencies
                })
            })
            .collect::<Vec<_>>();
        start.wait();
        let began = Instant::now();
        std::thread::sleep(RUN);
        stop.store(true, Ordering::Relaxed);
        let mut latencies = Vec::new();
        for worker in workers {
            latencies.extend(worker.join().expect("worker"));
        }
        Report {
            operations: latencies.len(),
            elapsed: began.elapsed(),
            latencies,
        }
    });
    let mut latencies = report.latencies;
    latencies.sort_unstable();
    let at = |fraction: f64| latencies[((latencies.len() - 1) as f64 * fraction) as usize];
    println!(
        "{name:<30} threads={threads} ops/s={:>10.0} p50={:>6}ns p99={:>7}ns",
        report.operations as f64 / report.elapsed.as_secs_f64(),
        at(0.50),
        at(0.99),
    );
}

#[test]
#[ignore = "benchmark: run with --release -- --ignored --nocapture"]
fn state_paths_benchmark() {
    for threads in [1, 8] {
        // Small positioned writes, every thread through its own handle on
        // the same open file.
        let fs = populated();
        block_on(async {
            fs.open("/w", "w", 0o644)
                .await
                .expect("create")
                .close()
                .await
                .expect("close");
        });
        measure(
            "write one file",
            threads,
            &fs,
            |fs, _| block_on(fs.open("/w", "r+", 0)).expect("open"),
            async |_, handle, round| {
                handle
                    .write(&[7; 16], Some((round % 64) * 16))
                    .await
                    .expect("write");
            },
        );

        // The same, each thread on its own open file.
        let fs = populated();
        measure(
            "write distinct files",
            threads,
            &fs,
            |fs, thread| block_on(fs.open(&format!("/w{thread}"), "w+", 0o644)).expect("open"),
            async |_, handle, round| {
                handle
                    .write(&[7; 16], Some((round % 64) * 16))
                    .await
                    .expect("write");
            },
        );

        // Stat of a closed file, each thread on its own.
        let fs = populated();
        measure(
            "stat",
            threads,
            &fs,
            |fs, thread| {
                let path = format!("/s{thread}");
                block_on(async {
                    let handle = fs.open(&path, "w", 0o644).await.expect("create");
                    handle.write(b"contents", None).await.expect("write");
                    handle.close().await.expect("close");
                });
                path
            },
            async |fs, path, _| {
                fs.stat(path).await.expect("stat");
            },
        );

        // Open and close, each thread on its own file.
        let fs = populated();
        measure(
            "open+close distinct",
            threads,
            &fs,
            |fs, thread| {
                let path = format!("/o{thread}");
                block_on(async {
                    let handle = fs.open(&path, "w", 0o644).await.expect("create");
                    handle.close().await.expect("close");
                });
                path
            },
            async |fs, path, _| {
                let handle = fs.open(path, "r", 0).await.expect("open");
                handle.close().await.expect("close");
            },
        );

        // The same while many other files are open, so the open-file table
        // is not small.
        let fs = populated();
        let held = block_on(async {
            let mut held = Vec::new();
            for index in 0..OPEN_FILES {
                held.push(
                    fs.open(&format!("/held{index}"), "w", 0o644)
                        .await
                        .expect("open held"),
                );
            }
            held
        });
        measure(
            "open+close, 1000 open",
            threads,
            &fs,
            |fs, thread| {
                let path = format!("/o{thread}");
                block_on(async {
                    let handle = fs.open(&path, "w", 0o644).await.expect("create");
                    handle.close().await.expect("close");
                });
                path
            },
            async |fs, path, _| {
                let handle = fs.open(path, "r", 0).await.expect("open");
                handle.close().await.expect("close");
            },
        );
        drop(held);

        // The first stat of a path: it is given an inode number, so the
        // attribute table grows.
        let fs = populated();
        measure(
            "first stat (mkdir+stat+rmdir)",
            threads,
            &fs,
            |_, thread| format!("/n{thread}"),
            async |fs, path, _| {
                fs.mkdir(path, MkdirOptions::default())
                    .await
                    .expect("mkdir");
                fs.stat(path).await.expect("stat");
                fs.rmdir(path).await.expect("rmdir");
            },
        );

        // Rename back and forth, each thread on its own file.
        let fs = populated();
        measure(
            "rename",
            threads,
            &fs,
            |fs, thread| {
                let names = (format!("/r{thread}a"), format!("/r{thread}b"));
                block_on(async {
                    let handle = fs.open(&names.0, "w", 0o644).await.expect("create");
                    handle.close().await.expect("close");
                });
                names
            },
            async |fs, names, _| {
                fs.rename(&names.0, &names.1).await.expect("rename");
                std::mem::swap(&mut names.0, &mut names.1);
            },
        );
    }
}
