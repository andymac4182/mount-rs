//! Opt-in instrumentation: counters are SQLite pager work, not physical SSD I/O.
use super::*;
use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{BlockExtent, DirectoryEntry, FileLayout, NodeData, NodeMetadata};
use mount_rs_memfs::MemoryFs;
use rusqlite::ffi;
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
        // The Box stays alive until tracing is unregistered under the connection mutex.
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
        let connection = database.lock().unwrap();
        result.page_size = connection
            .query_row("PRAGMA page_size", [], |row| row.get(0))
            .unwrap();
        let rc = unsafe {
            ffi::sqlite3_trace_v2(
                connection.handle(),
                ffi::SQLITE_TRACE_STMT,
                Some(trace),
                (&mut *result.counts as *mut TraceCounts).cast(),
            )
        };
        assert_eq!(rc, ffi::SQLITE_OK);
        result
    }
    fn pager(&self, reset: bool) -> BTreeMap<&'static str, u64> {
        let connection = self.database.lock().unwrap();
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
        if let Ok(connection) = self.database.lock() {
            unsafe {
                ffi::sqlite3_trace_v2(connection.handle(), 0, None, std::ptr::null_mut());
            }
        }
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
        nodes,
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
            let connection = blocks.0.lock().unwrap();
            let mut statement = connection
                .prepare("SELECT bytes FROM mount_rs_blocks WHERE id=?1")
                .unwrap();
            for id in &ids {
                let bytes: Vec<u8> = statement
                    .query_row(params![id.0], |row| row.get(0))
                    .unwrap();
                std::hint::black_box(bytes);
            }
        },
    );
    phase(
        "raw_sql_put_single_prepared_caller_id_autocommit",
        3200,
        3200 * 4096,
        std::slice::from_ref(&observer),
        || {
            let connection = blocks.0.lock().unwrap();
            let mut statement = connection
                .prepare("INSERT INTO mount_rs_blocks(id,bytes) VALUES(?1,?2)")
                .unwrap();
            for index in 0..3200 {
                statement
                    .execute(params![format!("raw-{index:060}"), payload(index + 3200)])
                    .unwrap();
            }
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
