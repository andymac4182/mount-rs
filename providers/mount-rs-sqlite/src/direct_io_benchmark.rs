//! Opt-in instrumentation: counters are SQLite pager work, not physical SSD I/O.
use super::*;
use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{BlockExtent, DirectoryEntry, FileLayout, NodeData, NodeMetadata};
use mount_rs_memfs::MemoryFs;
use rusqlite::ffi;
use std::sync::Mutex;
use std::{
    collections::BTreeMap,
    ffi::{CStr, c_void},
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Default)]
struct TraceCounts {
    statements: AtomicU64,
    sql: Mutex<BTreeMap<String, u64>>,
}
unsafe extern "C" fn trace(
    mask: u32,
    context: *mut c_void,
    statement: *mut c_void,
    _: *mut c_void,
) -> i32 {
    if mask == ffi::SQLITE_TRACE_STMT {
        // The Box stays alive until tracing is unregistered on the connection's thread.
        let counts = unsafe { &*(context.cast::<TraceCounts>()) };
        counts.statements.fetch_add(1, Ordering::Relaxed);
        let sql = unsafe { ffi::sqlite3_sql(statement.cast()) };
        if !sql.is_null() {
            let sql = unsafe { CStr::from_ptr(sql) }
                .to_string_lossy()
                .into_owned();
            if let Ok(mut map) = counts.sql.lock() {
                *map.entry(sql).or_default() += 1;
            }
        }
    }
    0
}
struct Observer {
    database: Database,
    counts: Box<TraceCounts>,
    page_size: u64,
}
impl Observer {
    fn new(database: &Database) -> Self {
        let mut result = Self {
            database: database.clone(),
            counts: Box::default(),
            page_size: 0,
        };
        let counts: &TraceCounts = &result.counts;
        let (page_size, rc) = database.with(|connection| {
            let page_size = connection
                .query_row("PRAGMA page_size", [], |row| row.get(0))
                .unwrap();
            let rc = unsafe {
                ffi::sqlite3_trace_v2(
                    connection.handle(),
                    ffi::SQLITE_TRACE_STMT,
                    Some(trace),
                    std::ptr::from_ref(counts).cast_mut().cast(),
                )
            };
            (page_size, rc)
        });
        assert_eq!(rc, ffi::SQLITE_OK);
        result.page_size = page_size;
        result
    }
    fn pager(&self, reset: bool) -> BTreeMap<&'static str, u64> {
        self.database.with(|connection| {
            [
                ("cache_hits", ffi::SQLITE_DBSTATUS_CACHE_HIT),
                ("cache_misses", ffi::SQLITE_DBSTATUS_CACHE_MISS),
                ("page_writes", ffi::SQLITE_DBSTATUS_CACHE_WRITE),
                ("cache_spills", ffi::SQLITE_DBSTATUS_CACHE_SPILL),
            ]
            .into_iter()
            .map(|(label, operation)| {
                let (mut current, mut high) = (0, 0);
                let rc = unsafe {
                    ffi::sqlite3_db_status(
                        connection.handle(),
                        operation,
                        &mut current,
                        &mut high,
                        i32::from(reset),
                    )
                };
                assert_eq!(rc, ffi::SQLITE_OK);
                (label, u64::try_from(current).unwrap())
            })
            .collect()
        })
    }
    fn reset(&self) {
        self.pager(true);
        self.counts.statements.store(0, Ordering::Relaxed);
        self.counts.sql.lock().unwrap().clear();
    }
    fn snapshot(&self) -> serde_json::Value {
        let pager = self.pager(false);
        serde_json::json!({ "sql_statements": self.counts.statements.load(Ordering::Relaxed),
            "sql": *self.counts.sql.lock().unwrap(), "pager": pager,
            "page_size": self.page_size,
            "pager_read_bytes_estimate": pager["cache_misses"] * self.page_size,
            "pager_write_bytes_estimate": pager["page_writes"] * self.page_size })
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        // On the connection's thread, so no statement is being traced into
        // the counters while they are detached.
        let _ = self.database.owner.run_blocking(|state| unsafe {
            ffi::sqlite3_trace_v2(state.connection.handle(), 0, None, std::ptr::null_mut());
        });
    }
}
fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    futures_lite::future::block_on(future)
}
fn phase(
    label: &str,
    ops: usize,
    logical_bytes: usize,
    observers: &[Observer],
    action: impl FnOnce(),
) {
    for observer in observers {
        observer.reset();
    }
    let started = Instant::now();
    let epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    eprintln!("SQLITE_IO_PHASE_START {label} epoch_ms={epoch_ms}");
    action();
    let elapsed = started.elapsed().as_secs_f64();
    let counts: Vec<_> = observers.iter().map(Observer::snapshot).collect();
    let sql: u64 = counts
        .iter()
        .map(|v| v["sql_statements"].as_u64().unwrap())
        .sum();
    let misses: u64 = counts
        .iter()
        .map(|v| v["pager"]["cache_misses"].as_u64().unwrap())
        .sum();
    let writes: u64 = counts
        .iter()
        .map(|v| v["pager"]["page_writes"].as_u64().unwrap())
        .sum();
    println!(
        "SQLITE_IO {}",
        serde_json::json!({"phase":label, "epoch_ms":epoch_ms, "elapsed_seconds":elapsed,
        "logical_ops":ops,"logical_bytes":logical_bytes,"logical_ops_per_second":ops as f64/elapsed,
        "sql_statements":sql,"sql_per_logical_op":sql as f64/ops as f64,"pager_misses":misses,"pager_writes":writes,
        "pager_misses_per_logical_op":misses as f64/ops as f64,"pager_writes_per_logical_op":writes as f64/ops as f64,
        "connections":counts})
    );
}
fn payload(index: usize) -> Vec<u8> {
    let mut state = (index as u64).wrapping_add(1);
    (0..4096)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}
fn namespace(ids: &[BlockId]) -> Namespace {
    use mount_rs_core::FsDriver;
    let stats = run(MemoryFs::empty().stat("/")).unwrap();
    let chunker = FixedSizeChunker::new(4096).unwrap().config();
    let mut nodes = BTreeMap::new();
    let mut entries = Vec::new();
    for file in 0..100 {
        let ino = stats.ino + 1 + file;
        let mut file_stats = stats.clone();
        file_stats.ino = ino;
        file_stats.mode = 0o100644;
        file_stats.nlink = 1;
        file_stats.size = 32 * 4096;
        file_stats.blocks = file_stats.size / 512;
        entries.push(DirectoryEntry {
            name: format!("file-{file}"),
            inode: ino,
        });
        let extents = (0..32)
            .map(|block| BlockExtent {
                file_offset: block as u64 * 4096,
                block: ids[file as usize * 32 + block].clone(),
                block_offset: 0,
                length: 4096,
            })
            .collect();
        nodes.insert(
            ino,
            NodeMetadata {
                stats: file_stats,
                data: NodeData::File(FileLayout {
                    chunker: chunker.clone(),
                    extents,
                }),
            },
        );
    }
    nodes.insert(
        stats.ino,
        NodeMetadata {
            stats: stats.clone(),
            data: NodeData::Directory { entries },
        },
    );
    Namespace {
        format_version: 1,
        root: stats.ino,
        next_inode: stats.ino + 101,
        default_uid: 0,
        default_gid: 0,
        umask: 0o022,
        default_chunker: chunker,
        nodes: nodes.into(),
    }
}

#[test]
#[ignore = "opt-in direct provider I/O benchmark; coordinate CPU and disk exclusivity"]
fn direct_provider_io_amplification() {
    let root = std::env::var_os("MOUNT_RS_SQLITE_IO_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("mount-rs-sqlite-io-{}", std::process::id()))
        });
    assert!(!root.exists(), "benchmark root must be fresh");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("blocks.db");
    let blocks = SqliteBlockStore::open(&path).unwrap();
    let observer = Observer::new(&blocks.0);
    let mut ids = Vec::with_capacity(3200);
    phase(
        "provider_put_single",
        3200,
        3200 * 4096,
        std::slice::from_ref(&observer),
        || {
            for i in 0..3200 {
                ids.push(run(blocks.put(&payload(i))).unwrap());
            }
        },
    );
    let readers: Vec<_> = (0..10)
        .map(|_| SqliteBlockStore::open(&path).unwrap())
        .collect();
    let observers: Vec<_> = readers
        .iter()
        .map(|reader| Observer::new(&reader.0))
        .collect();
    let rounds: usize = std::env::var("MOUNT_RS_SQLITE_IO_ROUNDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    // Each of 100 logical clients reads one 32-block file each round, spread over 10 connections.
    for pass in [
        "provider_get_100_clients_10_connections_first",
        "provider_get_100_clients_10_connections_repeat",
    ] {
        phase(
            pass,
            3200 * rounds,
            3200 * 4096 * rounds,
            &observers,
            || {
                for round in 0..rounds {
                    for client in 0..100 {
                        for block in 0..32 {
                            let index = ((client + round) % 100) * 32 + block;
                            let bytes = run(readers[client % 10].get(&ids[index])).unwrap();
                            assert_eq!(bytes.len(), 4096);
                            std::hint::black_box(bytes);
                        }
                    }
                }
            },
        );
    }
    phase(
        "provider_get_single",
        3200,
        3200 * 4096,
        std::slice::from_ref(&observer),
        || {
            for id in &ids {
                std::hint::black_box(run(blocks.get(id)).unwrap());
            }
        },
    );
    phase(
        "raw_sql_get_single_prepared",
        3200,
        3200 * 4096,
        std::slice::from_ref(&observer),
        || {
            blocks.0.with(|connection| {
                let mut statement = connection
                    .prepare("SELECT bytes FROM mount_rs_blocks WHERE id=?1")
                    .unwrap();
                for id in &ids {
                    let bytes: Vec<u8> = statement
                        .query_row(params![id.0], |row| row.get(0))
                        .unwrap();
                    std::hint::black_box(bytes);
                }
            });
        },
    );
    phase(
        "raw_sql_put_single_prepared_caller_id_autocommit",
        3200,
        3200 * 4096,
        std::slice::from_ref(&observer),
        || {
            blocks.0.with(|connection| {
                let mut statement = connection
                    .prepare("INSERT INTO mount_rs_blocks(id,bytes) VALUES(?1,?2)")
                    .unwrap();
                for index in 0..3200 {
                    statement
                        .execute(params![format!("raw-{index:060}"), payload(index + 3200)])
                        .unwrap();
                }
            });
        },
    );
    let metadata = SqliteMetadataStore::open(root.join("metadata.db")).unwrap();
    let ns = namespace(&ids);
    ns.validate().unwrap();
    let lease = run(metadata.acquire_writer("io-baseline", Duration::from_secs(3600))).unwrap();
    let meta_observer = Observer::new(&metadata.0);
    phase(
        "metadata_publish_initial",
        1,
        0,
        std::slice::from_ref(&meta_observer),
        || {
            run(metadata.publish(0, &lease, ns.clone())).unwrap();
        },
    );
    phase(
        "metadata_conditional_unchanged",
        3200,
        0,
        std::slice::from_ref(&meta_observer),
        || {
            for _ in 0..3200 {
                assert!(run(metadata.load_if_changed(1)).unwrap().is_none());
            }
        },
    );
    phase(
        "metadata_load_full",
        100,
        0,
        std::slice::from_ref(&meta_observer),
        || {
            for _ in 0..100 {
                std::hint::black_box(run(metadata.load()).unwrap());
            }
        },
    );
    phase(
        "metadata_publish_existing",
        100,
        0,
        std::slice::from_ref(&meta_observer),
        || {
            for revision in 1..101 {
                run(metadata.publish(revision, &lease, ns.clone())).unwrap();
            }
        },
    );
    println!("SQLITE_IO_ROOT {}", root.display());
}

/// Polls every future on the calling thread until all have finished.
fn run_all<T>(
    futures: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = T> + '_>>>,
) -> Vec<T> {
    use std::task::{Context, Poll, Wake, Waker};
    struct Unpark(std::thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(Unpark(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut futures: Vec<_> = futures.into_iter().map(Some).collect();
    let mut results: Vec<Option<T>> = futures.iter().map(|_| None).collect();
    let mut left = futures.len();
    while left > 0 {
        for (slot, result) in futures.iter_mut().zip(&mut results) {
            if let Some(future) = slot
                && let Poll::Ready(value) = future.as_mut().poll(&mut context)
            {
                *result = Some(value);
                *slot = None;
                left -= 1;
            }
        }
        if left > 0 {
            std::thread::park();
        }
    }
    results.into_iter().map(Option::unwrap).collect()
}

fn percentile(sorted: &[u64], fraction: f64) -> u64 {
    sorted[((sorted.len() - 1) as f64 * fraction) as usize]
}

/// Durable puts from several callers at once, and gets with and without
/// puts in progress. Uses only `put` and `get`, so it measures whatever
/// arranges the group commit.
///
/// ```sh
/// ./scripts/cargo-shared test --release --locked -p mount-rs-sqlite \
///     concurrent_put_and_get_throughput -- --ignored --nocapture
/// ```
#[test]
#[ignore = "opt-in concurrent put/get benchmark; coordinate CPU and disk exclusivity"]
fn concurrent_put_and_get_throughput() {
    let root = std::env::var_os("MOUNT_RS_SQLITE_IO_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!("mount-rs-sqlite-concurrent-{}", std::process::id()))
        });
    assert!(!root.exists(), "benchmark root must be fresh");
    std::fs::create_dir_all(&root).unwrap();
    let blocks = SqliteBlockStore::open(root.join("blocks.db")).unwrap();
    let observer = Observer::new(&blocks.0);
    // How the puts of a phase were grouped: transactions, and how long
    // each took on average, queueing aside.
    let grouping = |puts: usize, elapsed: f64| {
        let commits: u64 = observer
            .counts
            .sql
            .lock()
            .unwrap()
            .iter()
            .filter(|(sql, _)| sql.starts_with("BEGIN"))
            .map(|(_, count)| count)
            .sum();
        serde_json::json!({"commits": commits,
            "puts_per_commit": puts as f64 / commits as f64,
            "microseconds_per_commit": elapsed * 1e6 / commits as f64})
    };
    let report = |phase: &str, ops: usize, elapsed: f64, extra: serde_json::Value| {
        println!(
            "SQLITE_CONCURRENT {}",
            serde_json::json!({"phase": phase, "ops": ops, "elapsed_seconds": elapsed,
                "ops_per_second": ops as f64 / elapsed,
                "microseconds_per_op": elapsed * 1e6 / ops as f64, "detail": extra})
        );
    };
    let bytes = payload(1);
    let known: Vec<BlockId> = (0..256)
        .map(|i| run(blocks.put(&payload(i))).unwrap())
        .collect();

    // Durable puts, one thread per caller.
    for putters in [1_usize, 4, 16, 64] {
        let each = 2048 / putters;
        observer.reset();
        let started = Instant::now();
        std::thread::scope(|scope| {
            for _ in 0..putters {
                let (blocks, bytes) = (blocks.clone(), &bytes);
                scope.spawn(move || {
                    for _ in 0..each {
                        run(blocks.put(bytes)).unwrap();
                    }
                });
            }
        });
        let elapsed = started.elapsed().as_secs_f64();
        report(
            &format!("put_{putters}_threads"),
            putters * each,
            elapsed,
            grouping(putters * each, elapsed),
        );
    }
    // Durable puts, many callers that are tasks of one thread.
    for tasks in [16_usize, 64] {
        let each = 2048 / tasks;
        observer.reset();
        let started = Instant::now();
        run_all(
            (0..tasks)
                .map(|_| {
                    let (blocks, bytes) = (&blocks, &bytes);
                    Box::pin(async move {
                        for _ in 0..each {
                            blocks.put(bytes).await.unwrap();
                        }
                    })
                        as std::pin::Pin<Box<dyn std::future::Future<Output = ()>>>
                })
                .collect(),
        );
        let elapsed = started.elapsed().as_secs_f64();
        report(
            &format!("put_{tasks}_tasks_one_thread"),
            tasks * each,
            elapsed,
            grouping(tasks * each, elapsed),
        );
    }

    // Cached gets with nothing else going on.
    for id in &known {
        run(blocks.get(id)).unwrap();
    }
    let started = Instant::now();
    for step in 0..20_000 {
        std::hint::black_box(run(blocks.get(&known[step % known.len()])).unwrap());
    }
    report(
        "get_cached_1_thread",
        20_000,
        started.elapsed().as_secs_f64(),
        serde_json::Value::Null,
    );
    let started = Instant::now();
    std::thread::scope(|scope| {
        for reader in 0..4_usize {
            let (blocks, known) = (blocks.clone(), &known);
            scope.spawn(move || {
                for step in 0..5_000 {
                    let id = &known[(reader * 61 + step) % known.len()];
                    std::hint::black_box(run(blocks.get(id)).unwrap());
                }
            });
        }
    });
    report(
        "get_cached_4_threads",
        20_000,
        started.elapsed().as_secs_f64(),
        serde_json::Value::Null,
    );

    // Gets while eight callers keep putting.
    let stop = std::sync::atomic::AtomicBool::new(false);
    let put_count = AtomicU64::new(0);
    let started = Instant::now();
    let mut latencies: Vec<u64> = std::thread::scope(|scope| {
        for _ in 0..8 {
            let (blocks, bytes, stop, put_count) = (blocks.clone(), &bytes, &stop, &put_count);
            scope.spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    run(blocks.put(bytes)).unwrap();
                    put_count.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
        let readers: Vec<_> = (0..4_usize)
            .map(|reader| {
                let (blocks, known) = (blocks.clone(), &known);
                scope.spawn(move || {
                    (0..1_000)
                        .map(|step| {
                            let id = &known[(reader * 61 + step) % known.len()];
                            let asked = Instant::now();
                            std::hint::black_box(run(blocks.get(id)).unwrap());
                            asked.elapsed().as_micros() as u64
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        let latencies = readers
            .into_iter()
            .flat_map(|reader| reader.join().unwrap())
            .collect();
        stop.store(true, Ordering::Relaxed);
        latencies
    });
    let elapsed = started.elapsed().as_secs_f64();
    latencies.sort_unstable();
    report(
        "get_4_threads_during_8_putters",
        latencies.len(),
        elapsed,
        serde_json::json!({
            "get_mean_us": latencies.iter().sum::<u64>() / latencies.len() as u64,
            "get_p50_us": percentile(&latencies, 0.5),
            "get_p99_us": percentile(&latencies, 0.99),
            "get_max_us": latencies.last(),
            "puts_per_second": put_count.load(Ordering::Relaxed) as f64 / elapsed,
        }),
    );
    drop(observer);
    drop(blocks);
    std::fs::remove_dir_all(root).unwrap();
}
