//! Opt-in matched-source TiDB/RustFS metadata diagnostic. No capacity claim.
//! Copy this file byte-for-byte into both compared checkouts before building.
#![cfg(all(unix, feature = "allocation-profiling"))]

use futures_util::{Stream, StreamExt, TryStreamExt};
use mount_rs_core::diagnostics::{object_store, profile, storage};
use mount_rs_rustfs::RustFsConfig;
use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext, StoreConfig};
use mysql_async::{Pool, prelude::Queryable};
use ring::rand::{SecureRandom, SystemRandom};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

// The unchanged resource helper supports wire clients too. This target calls
// only its process capture: zero wire observations are explicitly unavailable.
#[allow(dead_code)]
struct Client {
    connection: quinn::Connection,
}
#[allow(dead_code)]
#[path = "support/production_target/command.rs"]
mod command;
#[allow(dead_code)]
#[path = "support/remote_blocks.rs"]
mod remote_blocks;
#[allow(dead_code)]
#[path = "support/resource_profile.rs"]
mod resource_profile;

const BYTES: usize = 4096;
const TABLES: [&str; 7] = [
    "mount_rs_tidb_metadata",
    "mount_rs_tidb_inodes",
    "mount_rs_tidb_compact_guards",
    "mount_rs_tidb_compact_members",
    "mount_rs_tidb_compact_dentries",
    "mount_rs_tidb_block_authority",
    "mount_rs_tidb_blocks",
];
const OPTIONAL_TABLES: [&str; 2] = [
    "mount_rs_tidb_compact_members",
    "mount_rs_tidb_compact_dentries",
];
const SELECTED_OPERATIONS: [&str; 16] = [
    "tidb.sql.metadata_read",
    "tidb.sql.metadata_write",
    "tidb.sql.inode_read",
    "tidb.sql.inode_write",
    "tidb.tx.begin.metadata",
    "tidb.tx.begin.inode",
    "tidb.tx.begin.compact_read",
    "tidb.tx.commit",
    "tidb.tx.rollback",
    "tidb.pool.checkout",
    "sdk.metadata.load_compact_inode",
    "sdk.metadata.publish_compact_inode",
    "sdk.blocks.get",
    "sdk.blocks.put",
    "object_store.backing_marker.data.get",
    "object_store.backing_marker.probe.get",
];
const SQL_OPERATIONS: [&str; 4] = [
    "tidb.sql.metadata_read",
    "tidb.sql.metadata_write",
    "tidb.sql.inode_read",
    "tidb.sql.inode_write",
];

type BenchResult<T> = Result<T, String>;
fn required(name: &str) -> BenchResult<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("explicit {name} required"))
}
fn bounded(name: &str, default: usize, low: usize, high: usize) -> BenchResult<usize> {
    let value = std::env::var(name).unwrap_or_else(|_| default.to_string());
    let number = value
        .parse::<usize>()
        .map_err(|_| format!("invalid {name}"))?;
    if !(low..=high).contains(&number) || number.to_string() != value {
        return Err(format!("out of bounds {name}"));
    }
    Ok(number)
}

#[derive(Clone, Copy)]
struct Config {
    drives: usize,
    lanes: usize,
    files: usize,
    iterations: usize,
    setup_seconds: usize,
    stage_seconds: usize,
    oracle_seconds: usize,
}
impl Config {
    fn environment() -> BenchResult<Self> {
        let config = Self {
            drives: bounded("MOUNT_RS_INDEXED_BENCH_DRIVES", 2, 1, 10)?,
            lanes: bounded("MOUNT_RS_INDEXED_BENCH_LANES_PER_DRIVE", 5, 1, 10)?,
            files: bounded("MOUNT_RS_INDEXED_BENCH_FILES", 100, 10, 1000)?,
            iterations: bounded("MOUNT_RS_INDEXED_BENCH_ITERATIONS", 20, 2, 128)?,
            setup_seconds: bounded("MOUNT_RS_INDEXED_BENCH_SETUP_SECONDS", 600, 1, 1200)?,
            stage_seconds: bounded("MOUNT_RS_INDEXED_BENCH_STAGE_SECONDS", 120, 1, 600)?,
            oracle_seconds: bounded("MOUNT_RS_INDEXED_BENCH_ORACLE_SECONDS", 600, 1, 1200)?,
        };
        if config.drives * config.lanes > 10 || config.files < config.lanes {
            return Err("maximum ten clients; each lane needs a distinct file cohort".into());
        }
        Ok(config)
    }
    fn json(self) -> Value {
        json!({"drives":self.drives,"lanes_per_drive":self.lanes,
            "clients":self.drives*self.lanes,"files_per_drive":self.files,
            "iterations_per_lane_per_stage":self.iterations,"payload_bytes":BYTES,
            "chunk_bytes":BYTES,"setup_deadline_seconds":self.setup_seconds,
            "stage_deadline_seconds":self.stage_seconds,"oracle_deadline_seconds":self.oracle_seconds,
            "shutdown_deadline_seconds":60,"purge_deadline_seconds":120})
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pattern {
    SequentialRead,
    RandomRead,
    SequentialWrite,
    RandomWrite,
    Mixed,
}
impl Pattern {
    fn label(self) -> &'static str {
        match self {
            Self::SequentialRead => "sequential_read",
            Self::RandomRead => "random_read",
            Self::SequentialWrite => "sequential_overwrite",
            Self::RandomWrite => "random_overwrite",
            Self::Mixed => "mixed",
        }
    }
    fn writing(self, cycle: usize) -> bool {
        matches!(self, Self::SequentialWrite | Self::RandomWrite)
            || self == Self::Mixed && cycle % 2 == 1
    }
    fn random(self) -> bool {
        matches!(self, Self::RandomRead | Self::RandomWrite | Self::Mixed)
    }
}
const PATTERNS: [Pattern; 5] = [
    Pattern::SequentialRead,
    Pattern::RandomRead,
    Pattern::SequentialWrite,
    Pattern::RandomWrite,
    Pattern::Mixed,
];

fn payload_into(bytes: &mut [u8], drive: usize, file: usize, generation: u64) {
    let mut state = generation
        ^ (drive as u64 + 1).wrapping_mul(7919)
        ^ (file as u64 + 1).wrapping_mul(104_729);
    for byte in bytes.iter_mut() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    bytes[..8].copy_from_slice(&(drive as u64).to_le_bytes());
    bytes[8..16].copy_from_slice(&(file as u64).to_le_bytes());
    bytes[16..24].copy_from_slice(&generation.to_le_bytes());
}
fn validate_read(actual: &[u8], count: usize, wanted: &[u8], eof: usize) -> BenchResult<()> {
    if count != BYTES
        || actual.len() != BYTES
        || wanted.len() != BYTES
        || actual != wanted
        || eof != 0
    {
        return Err("full-byte, length or EOF oracle failed".into());
    }
    Ok(())
}

#[derive(Clone, Default)]
struct Counts {
    cycles: u64,
    opens: u64,
    closes: u64,
    reads: u64,
    writes: u64,
    verified_reads: u64,
    eof_checks: u64,
    read_bytes: u64,
    written_bytes: u64,
}
impl Counts {
    fn record(&mut self, writing: bool) {
        self.cycles += 1;
        self.opens += 1;
        self.closes += 1;
        if writing {
            self.writes += 1;
            self.written_bytes += BYTES as u64;
        } else {
            self.reads += 1;
            self.verified_reads += 1;
            self.eof_checks += 1;
            self.read_bytes += BYTES as u64;
        }
    }
    fn merge(&mut self, other: &Self) {
        self.cycles += other.cycles;
        self.opens += other.opens;
        self.closes += other.closes;
        self.reads += other.reads;
        self.writes += other.writes;
        self.verified_reads += other.verified_reads;
        self.eof_checks += other.eof_checks;
        self.read_bytes += other.read_bytes;
        self.written_bytes += other.written_bytes;
    }
    fn validate(&self, expected_cycles: u64, expected_writes: u64) -> BenchResult<()> {
        let expected_reads = expected_cycles
            .checked_sub(expected_writes)
            .ok_or("invalid expected counts")?;
        if self.cycles != expected_cycles
            || self.opens != expected_cycles
            || self.closes != expected_cycles
            || self.writes != expected_writes
            || self.reads != expected_reads
            || self.verified_reads != expected_reads
            || self.eof_checks != expected_reads
            || self.read_bytes != expected_reads * BYTES as u64
            || self.written_bytes != expected_writes * BYTES as u64
        {
            return Err("acknowledged operation or verified-byte counts incomplete".into());
        }
        Ok(())
    }
    fn json(&self) -> Value {
        json!({"cycles":self.cycles,"opens":self.opens,"closes":self.closes,
            "payload_reads":self.reads,"payload_writes":self.writes,"verified_reads":self.verified_reads,
            "eof_checks":self.eof_checks,"read_bytes":self.read_bytes,"written_bytes":self.written_bytes})
    }
}

fn validate_storage(snapshot: &storage::Snapshot) -> BenchResult<()> {
    if snapshot.in_flight != 0 || snapshot.entries.len() != storage::operation_names().len() {
        return Err("storage boundary is nonquiescent or bank shape changed".into());
    }
    for (row, name) in snapshot.entries.iter().zip(storage::operation_names()) {
        if row.name != *name
            || row.in_flight != 0
            || row
                .success
                .checked_add(row.error)
                .and_then(|n| n.checked_add(row.cancelled))
                != Some(row.calls)
            || row
                .latency_log2_us
                .iter()
                .try_fold(0u64, |n, value| n.checked_add(*value))
                != Some(row.calls)
        {
            return Err("storage bank identity, outcome or histogram invalid".into());
        }
    }
    Ok(())
}
fn selected_rows(delta: &storage::Snapshot) -> BenchResult<Value> {
    let mut selected = BTreeMap::new();
    for name in SELECTED_OPERATIONS {
        let row = delta
            .entries
            .iter()
            .find(|row| row.name == name)
            .ok_or("required named operation absent")?;
        if row.error != 0 || row.cancelled != 0 {
            return Err("selected storage errors or cancellations observed".into());
        }
        selected.insert(
            name,
            serde_json::to_value(row).map_err(|_| "storage row serialization failed")?,
        );
    }
    // Optional additive labels are joined by name on both builds, never index.
    for name in [
        "sdk.metadata.compact_root_file_capability",
        "sdk.metadata.load_compact_root_file",
    ] {
        selected.insert(
            name,
            delta
                .entries
                .iter()
                .find(|row| row.name == name)
                .map(serde_json::to_value)
                .transpose()
                .map_err(|_| "optional row serialization failed")?
                .unwrap_or(Value::Null),
        );
    }
    Ok(json!(selected))
}

fn validate_http(snapshot: &object_store::Snapshot) -> BenchResult<()> {
    if snapshot.saturated
        || snapshot.concurrent_activity
        || snapshot.bundles.builds_inflight != 0
        || snapshot.clients.iter().any(|client| {
            client.build.inflight != 0
                || client
                    .http
                    .iter()
                    .any(|http| http.attempts_inflight != 0 || http.bodies_inflight != 0)
        })
    {
        return Err("HTTP observer boundary incomplete or saturated".into());
    }
    Ok(())
}
fn cache_budgets(clients: &[OwnedClient]) -> Value {
    json!(clients.iter().map(|client| {
        let observed = client.context.raw_cache_budget_snapshot();
        let (bytes,entries) = client.context.raw_cache_limits();
        json!({"drive":client.drive,"lane":client.lane,"max_charged_bytes":bytes,"max_entries":entries,
            "observed":observed.map(|value|json!({"charged_bytes":value.charged_bytes,"payload_bytes":value.payload_bytes,
                "entries":value.entries,"high_water_charged_bytes":value.high_water_charged_bytes,
                "high_water_entries":value.high_water_entries,"rejected_reservations":value.rejected_reservations,
                "contention_rejections":value.contention_rejections,"admission_skips":value.admission_skips})),
            "scope":"capacity and admission gauges; not cache hit or miss counts"})
    }).collect::<Vec<_>>())
}

struct Scope {
    key: String,
    prefix: String,
}
fn scope_name() -> BenchResult<String> {
    let mut random = [0u8; 12];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| "scope entropy unavailable")?;
    let random = hex(&random);
    Ok(format!("sdk-indexed-bench-{}-{random}", std::process::id()))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn digest(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}
fn source_receipt() -> BenchResult<Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sdk_tidb_metadata_benchmark.rs");
    let compiled = include_bytes!("sdk_tidb_metadata_benchmark.rs");
    let current = std::fs::read(path).map_err(|_| "runner source read failed")?;
    if current != compiled {
        return Err("runner bytes changed after build".into());
    }
    let mut support = BTreeMap::new();
    for (name, compiled) in [
        (
            "support/resource_profile.rs",
            include_bytes!("support/resource_profile.rs").as_slice(),
        ),
        (
            "support/resource_profile/sqlite_heap.rs",
            include_bytes!("support/resource_profile/sqlite_heap.rs").as_slice(),
        ),
        (
            "support/device_io.rs",
            include_bytes!("support/device_io.rs").as_slice(),
        ),
        (
            "support/remote_blocks.rs",
            include_bytes!("support/remote_blocks.rs").as_slice(),
        ),
        (
            "support/production_target/command.rs",
            include_bytes!("support/production_target/command.rs").as_slice(),
        ),
    ] {
        let current = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests")
                .join(name),
        )
        .map_err(|_| "support source read failed")?;
        if current != compiled {
            return Err("support source changed after build".into());
        }
        support.insert(name, digest(compiled));
    }
    let mut input =
        std::fs::File::open(std::env::current_exe().map_err(|_| "executable path unavailable")?)
            .map_err(|_| "executable open failed")?;
    let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|_| "executable digest read failed")?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    let git = |arguments: &[&str]| -> BenchResult<String> {
        let result = std::process::Command::new("git")
            .arg("-C")
            .arg(env!("CARGO_MANIFEST_DIR"))
            .args(arguments)
            .output()
            .map_err(|_| "source identity probe failed")?;
        if !result.status.success() {
            return Err("source identity probe failed".into());
        }
        String::from_utf8(result.stdout).map_err(|_| "source identity encoding invalid".into())
    };
    let revision = git(&["rev-parse", "HEAD"])?;
    let revision = revision.trim();
    let compiled_revision = option_env!("MOUNT_RS_INDEXED_BENCH_SOURCE_REVISION")
        .ok_or("compile-time source revision required")?;
    if revision.len() != 40
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        || revision != compiled_revision
    {
        return Err("compile-time revision does not match source checkout".into());
    }
    Ok(json!({"checkout_revision_at_run":revision,
        "checkout_dirty_at_run":!git(&["status","--porcelain"] )?.is_empty(),
        "compiled_revision_env":compiled_revision,
        "compiled_runner_sha256":digest(compiled),"support_source_sha256":support,"executable_sha256":hex(hasher.finish().as_ref()),
        "resource_profiling":true,"allocation_profiling":true,"debug_assertions":cfg!(debug_assertions)}))
}

async fn table_inventory(sql: &mut mysql_async::Conn) -> BenchResult<Vec<&'static str>> {
    let mut present = vec![];
    for table in TABLES {
        let count: Option<u64> = sql.exec_first(
            "SELECT COUNT(*) FROM information_schema.tables WHERE table_schema=DATABASE() AND table_name=?", (table,))
            .await.map_err(|_| "owned table inventory query failed")?;
        match count {
            Some(1) => present.push(table),
            Some(0) if OPTIONAL_TABLES.contains(&table) => {}
            _ => return Err("owned table inventory missing or ambiguous".into()),
        }
    }
    Ok(present)
}
async fn assert_absent(connection: &str, scopes: &[Scope]) -> BenchResult<Vec<&'static str>> {
    let pool = Pool::from_url(connection).map_err(|_| "invalid private TiDB connection")?;
    let result: BenchResult<Vec<&'static str>> = async {
        let mut sql = pool
            .get_conn()
            .await
            .map_err(|_| "TiDB freshness connection failed")?;
        let tables = table_inventory(&mut sql).await?;
        for scope in scopes {
            for table in &tables {
                let count: Option<u64> = sql
                    .exec_first(
                        format!("SELECT COUNT(*) FROM {table} WHERE volume_key=?"),
                        (scope.key.as_bytes(),),
                    )
                    .await
                    .map_err(|_| "owned freshness query failed")?;
                if count != Some(0) {
                    return Err("owned metadata scope was not absent".into());
                }
            }
        }
        Ok(tables)
    }
    .await;
    let closed = pool
        .disconnect()
        .await
        .map_err(|_| "freshness connection shutdown failed");
    let tables = result?;
    closed?;
    Ok(tables)
}
async fn mode_receipts(connection: &str, scopes: &[Scope]) -> BenchResult<Value> {
    let pool = Pool::from_url(connection).map_err(|_| "invalid persisted mode connection")?;
    let read: BenchResult<Value> = async {
        let mut sql = pool
            .get_conn()
            .await
            .map_err(|_| "persisted mode connection failed")?;
        let mut receipts = vec![];
        for scope in scopes {
            let observed: Option<(Vec<u8>, Option<Vec<u8>>)> = sql
                .exec_first(
                    "SELECT write_mode,backing_id FROM mount_rs_tidb_metadata WHERE volume_key=?",
                    (scope.key.as_bytes(),),
                )
                .await
                .map_err(|_| "persisted mode query failed")?;
            let (mode, backing) = observed.ok_or("persisted metadata row missing")?;
            let backing = backing.ok_or("persisted backing missing")?;
            let backing =
                std::str::from_utf8(&backing).map_err(|_| "persisted backing encoding invalid")?;
            if mode != b"MRC5"
                || mount_rs_core::storage::ConcurrentBackingId::from_hex(backing).is_err()
            {
                return Err("persisted compact mode or backing invalid".into());
            }
            receipts.push(json!({"metadata_key":scope.key,"mode":"MRC5","backing_id":backing}));
        }
        Ok(json!(receipts))
    }
    .await;
    let closed = pool
        .disconnect()
        .await
        .map_err(|_| "persisted mode connection shutdown failed");
    let receipts = read?;
    closed?;
    Ok(receipts)
}

struct OwnedClient {
    drive: usize,
    lane: usize,
    filesystem: Arc<Filesystem>,
    context: StorageContext,
}
fn options(connection: &str, config: &RustFsConfig, scope: &Scope, lane: usize) -> SplitOptions {
    let mut options = SplitOptions::memory(format!("indexed-diagnostic-{lane}"), BYTES)
        .with_compact_inode_updates(true);
    options.metadata = StoreConfig::Tidb {
        connection: connection.into(),
        volume_key: scope.key.clone(),
        durable: true,
    };
    options.blocks = StoreConfig::RustFs {
        endpoint: config.endpoint.clone(),
        bucket: config.bucket.clone(),
        region: config.region.clone(),
        prefix: scope.prefix.clone(),
        access_key_id: config.access_key_id.clone(),
        secret_access_key: config.secret_access_key.clone(),
        durable: true,
    };
    options
}
async fn setup(
    config: Config,
    connection: &str,
    blocks: &RustFsConfig,
    scopes: &[Scope],
    clients: &mut Vec<OwnedClient>,
) -> BenchResult<()> {
    for (drive, scope) in scopes.iter().enumerate() {
        for lane in 0..config.lanes {
            let context = StorageContext::new(4).map_err(|_| "invalid context capacity")?;
            let filesystem =
                Filesystem::split_with_context(options(connection, blocks, scope, lane), &context)
                    .await
                    .map_err(|_| "owned filesystem construction failed")?;
            clients.push(OwnedClient {
                drive,
                lane,
                filesystem: Arc::new(filesystem),
                context,
            });
            if lane == 0 {
                let driver = clients.last().unwrap().filesystem.driver();
                let mut payload = vec![0u8; BYTES];
                for file in 0..config.files {
                    payload_into(&mut payload, drive, file, 1);
                    driver
                        .write_file(&format!("/file-{file}"), &payload)
                        .await
                        .map_err(|_| "population write failed")?;
                }
            }
        }
    }
    Ok(())
}

struct LaneResult {
    drive: usize,
    lane: usize,
    counts: Counts,
    generations: Vec<(usize, u64)>,
    cycle_latencies_us: Vec<u64>,
}
async fn lane(
    config: Config,
    pattern: Pattern,
    stage: usize,
    client: &OwnedClient,
    ledger: &[u64],
) -> BenchResult<LaneResult> {
    let cohort: Vec<usize> = (client.lane..config.files).step_by(config.lanes).collect();
    let mut generations = ledger.to_vec();
    let driver = client.filesystem.driver();
    let mut actual = vec![0; BYTES];
    let mut expected = vec![0; BYTES];
    let mut eof = [0u8; 1];
    let mut counts = Counts::default();
    let mut latencies = Vec::with_capacity(config.iterations);
    for cycle in 0..config.iterations {
        let index = if pattern.random() {
            cycle
                .wrapping_mul(2_654_435_761)
                .wrapping_add(client.drive * 7919)
                % cohort.len()
        } else {
            cycle % cohort.len()
        };
        let file = cohort[index];
        let writing = pattern.writing(cycle);
        let generation = if writing {
            (stage as u64 + 2) * 1_000_000 + cycle as u64 + 1
        } else {
            generations[file]
        };
        payload_into(&mut expected, client.drive, file, generation);
        let path = format!("/file-{file}");
        let started = Instant::now();
        let handle = driver
            .open(&path, if writing { "r+" } else { "r" }, 0)
            .await
            .map_err(|_| "existing root-file open failed")?;
        let applied: BenchResult<()> = async {
            if writing {
                let count = handle
                    .write(&expected, Some(0))
                    .await
                    .map_err(|_| "overwrite outcome unacknowledged; never replayed")?;
                if count != BYTES {
                    return Err("short overwrite; never replayed".into());
                }
            } else {
                let count = handle
                    .read(&mut actual, Some(0))
                    .await
                    .map_err(|_| "payload read failed")?;
                let tail = handle
                    .read(&mut eof, Some(BYTES as u64))
                    .await
                    .map_err(|_| "EOF read failed")?;
                validate_read(&actual, count, &expected, tail)?;
            }
            Ok(())
        }
        .await;
        let closed = handle
            .close()
            .await
            .map_err(|_| "handle close unacknowledged");
        applied?;
        closed?;
        latencies
            .push(u64::try_from(started.elapsed().as_micros()).map_err(|_| "latency overflow")?);
        counts.record(writing);
        if writing {
            generations[file] = generation;
        }
    }
    Ok(LaneResult {
        drive: client.drive,
        lane: client.lane,
        counts,
        generations: cohort
            .into_iter()
            .map(|file| (file, generations[file]))
            .collect(),
        cycle_latencies_us: latencies,
    })
}
fn apply_lanes(config: Config, results: &[LaneResult], ledger: &mut [Vec<u64>]) -> BenchResult<()> {
    let mut owners = BTreeSet::new();
    let mut files = BTreeSet::new();
    if results.len() != config.drives * config.lanes || ledger.len() != config.drives {
        return Err("lane or ledger coverage incomplete".into());
    }
    for result in results {
        if result.drive >= config.drives
            || result.lane >= config.lanes
            || !owners.insert((result.drive, result.lane))
        {
            return Err("lane owner duplicate or outside geometry".into());
        }
        for &(file, generation) in &result.generations {
            if file >= config.files
                || file % config.lanes != result.lane
                || generation == 0
                || !files.insert((result.drive, file))
            {
                return Err("file generation duplicate or outside owned lane".into());
            }
        }
    }
    if files.len() != config.drives * config.files {
        return Err("file generation coverage incomplete".into());
    }
    for result in results {
        for &(file, generation) in &result.generations {
            ledger[result.drive][file] = generation;
        }
    }
    Ok(())
}
fn latencies(mut values: Vec<u64>) -> Value {
    values.sort_unstable();
    let pick = |percent: usize| values[(values.len() * percent).div_ceil(100).saturating_sub(1)];
    json!({"samples":values.len(),"p50_us":pick(50),"p95_us":pick(95),"p99_us":pick(99),
        "scope":"open, one acknowledged payload operation, read byte and EOF verification where applicable, close; payload generation excluded"})
}
async fn stage(
    config: Config,
    pattern: Pattern,
    stage: usize,
    clients: &[OwnedClient],
    ledger: &mut [Vec<u64>],
) -> BenchResult<Value> {
    let before_storage = storage::snapshot();
    validate_storage(&before_storage)?;
    let before_core = profile::snapshot();
    let observer = object_store::Observer::enabled();
    let before_http = observer.snapshot().ok_or("HTTP observations disabled")?;
    validate_http(&before_http)?;
    let before_cache = cache_budgets(clients);
    let before_resources = resource_profile::Snapshot::capture_process().map_err(str::to_owned)?;
    let started = Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for client in clients {
        let client = OwnedClient {
            drive: client.drive,
            lane: client.lane,
            filesystem: client.filesystem.clone(),
            context: client.context.clone(),
        };
        let generations = ledger[client.drive].clone();
        tasks.spawn(async move { lane(config, pattern, stage, &client, &generations).await });
    }
    let completed: Result<BenchResult<Vec<LaneResult>>, tokio::time::error::Elapsed> =
        tokio::time::timeout(Duration::from_secs(config.stage_seconds as u64), async {
            let mut results = vec![];
            let mut failure = None;
            while let Some(result) = tasks.join_next().await {
                match result {
                    Ok(Ok(result)) => results.push(result),
                    Ok(Err(error)) => {
                        failure.get_or_insert(error);
                    }
                    Err(_) => {
                        failure.get_or_insert("lane task failed".into());
                    }
                }
            }
            failure.map_or(Ok(results), Err)
        })
        .await;
    let results = match completed {
        Ok(results) => results?,
        Err(_) => {
            tasks.abort_all();
            let drained = tokio::time::timeout(Duration::from_secs(30), async {
                while tasks.join_next().await.is_some() {}
            })
            .await;
            return Err(if drained.is_ok() {
                "stage deadline; aborted lanes joined; writes uncertain and scope retained"
            } else {
                "stage deadline; lane drain unconfirmed; scope retained"
            }
            .into());
        }
    };
    let elapsed = started.elapsed().as_secs_f64();
    let resources = resource_profile::Snapshot::capture_process()
        .map_err(str::to_owned)?
        .delta(&before_resources)
        .map_err(str::to_owned)?;
    let after_http = observer.snapshot().ok_or("HTTP observations disabled")?;
    validate_http(&after_http)?;
    let after_cache = cache_budgets(clients);
    let after_storage = storage::snapshot();
    validate_storage(&after_storage)?;
    let delta = after_storage
        .delta(&before_storage)
        .map_err(str::to_owned)?;
    let selected = selected_rows(&delta)?;
    let core = profile::snapshot()
        .delta(&before_core)
        .map_err(str::to_owned)?;
    let mut counts = Counts::default();
    let mut samples = vec![];
    for result in &results {
        counts.merge(&result.counts);
        samples.extend_from_slice(&result.cycle_latencies_us);
    }
    let cycles = (config.drives * config.lanes * config.iterations) as u64;
    let writes = if matches!(pattern, Pattern::SequentialWrite | Pattern::RandomWrite) {
        cycles
    } else if pattern == Pattern::Mixed {
        (config.drives * config.lanes * (config.iterations / 2)) as u64
    } else {
        0
    };
    counts.validate(cycles, writes)?;
    if samples.len() as u64 != cycles {
        return Err("cycle latency coverage incomplete".into());
    }
    apply_lanes(config, &results, ledger)?;
    let sql_calls: u64 = SQL_OPERATIONS
        .iter()
        .map(|name| selected[*name]["calls"].as_u64().unwrap())
        .sum();
    Ok(
        json!({"pattern":pattern.label(),"elapsed_seconds_including_drain":elapsed,
        "counts":counts.json(),"cycles_per_second":cycles as f64/elapsed,
        "logical_payload_operations_per_second":(counts.reads+counts.writes) as f64/elapsed,
        "cycle_latency":latencies(samples),"resources":resources,"core_profile":core,
        "object_store_http":{"before":before_http,"after":after_http,"complete":true,
            "scope":"actual RustFS connector attempts and response body bytes by closed data/probe role and HTTP method; retries included; no physical device IOPS"},
        "cache_budgets":{"before":before_cache,"after":after_cache,"hit_miss_counts_available":false},
        "storage_profile":{"complete":true,"before":before_storage,"after":after_storage,"delta":delta,"selected":selected,
            "selected_sql_calls":sql_calls,"sql_calls_per_payload_operation":sql_calls as f64/(counts.reads+counts.writes) as f64,
            "scope":"named completed provider calls; no physical IOPS or SQL server lock wait claim"}}),
    )
}

async fn shutdown(clients: &[OwnedClient]) -> BenchResult<()> {
    let mut failed = false;
    for client in clients {
        failed |= client.filesystem.shutdown().await.is_err();
        failed |= client.context.close().await.is_err();
    }
    if failed {
        Err("provider or context shutdown unconfirmed".into())
    } else {
        Ok(())
    }
}
// Keep the positional file oracle identical to the source-bound measured runs.
#[allow(clippy::needless_range_loop)]
async fn oracle(
    config: Config,
    connection: &str,
    blocks: &RustFsConfig,
    scopes: &[Scope],
    ledger: &[Vec<u64>],
) -> BenchResult<Value> {
    let mut files = 0usize;
    for (drive, scope) in scopes.iter().enumerate() {
        let context = StorageContext::new(2).map_err(|_| "invalid oracle context capacity")?;
        let filesystem =
            Filesystem::split_with_context(options(connection, blocks, scope, 0), &context)
                .await
                .map_err(|_| "fresh filesystem construction failed")?;
        let driver = filesystem.driver();
        let checked: BenchResult<()> = async {
            let listed = driver
                .readdir("/")
                .await
                .map_err(|_| "fresh listing failed")?;
            let listed_count = listed.len();
            let names: BTreeSet<_> = listed.into_iter().map(|entry| entry.name).collect();
            let wanted: BTreeSet<_> = (0..config.files)
                .map(|file| format!("file-{file}"))
                .collect();
            if listed_count != config.files || names.len() != config.files || names != wanted {
                return Err("fresh exact membership oracle failed".into());
            }
            let mut actual = vec![0; BYTES];
            let mut expected = vec![0; BYTES];
            let mut eof = [0u8; 1];
            for file in 0..config.files {
                payload_into(&mut expected, drive, file, ledger[drive][file]);
                let handle = driver
                    .open(&format!("/file-{file}"), "r", 0)
                    .await
                    .map_err(|_| "fresh open failed")?;
                let read = async {
                    let count = handle
                        .read(&mut actual, Some(0))
                        .await
                        .map_err(|_| "fresh payload read failed")?;
                    let tail = handle
                        .read(&mut eof, Some(BYTES as u64))
                        .await
                        .map_err(|_| "fresh EOF read failed")?;
                    validate_read(&actual, count, &expected, tail)
                }
                .await;
                let closed = handle
                    .close()
                    .await
                    .map_err(|_| "fresh handle close failed");
                read?;
                closed?;
                files += 1;
            }
            Ok(())
        }
        .await;
        let closed = filesystem
            .shutdown()
            .await
            .map_err(|_| "fresh provider shutdown failed");
        let context_closed = context
            .close()
            .await
            .map_err(|_| "fresh context shutdown failed");
        checked?;
        closed?;
        context_closed?;
    }
    Ok(
        json!({"passed":true,"files":files,"bytes":files*BYTES,"full_bytes":true,"eof":true,"exact_root_membership":true}),
    )
}

const CLEANUP_OBJECT_LIMIT: usize = 32768;
const CLEANUP_BOUND_ERROR: &str = "cleanup object bound exceeded; no deletion admitted";

fn validate_owned_location(exact_prefix: &str, location: &str) -> BenchResult<()> {
    if exact_prefix.is_empty()
        || !exact_prefix.ends_with('/')
        || location
            .strip_prefix(exact_prefix)
            .is_none_or(|suffix| suffix.is_empty())
    {
        return Err("cleanup object outside exact prefix or empty suffix".into());
    }
    Ok(())
}

// ObjectStore::list is streaming; its internal page sizes are not exposed.
// Retain each original location for deletion and stop at the first overflow
// sentinel, without retaining or consuming subsequent objects from that stream.
async fn collect_cleanup_inventory<T, S>(
    exact_prefixes: &[String],
    streams: &mut [S],
    limit: usize,
) -> BenchResult<Vec<Vec<T>>>
where
    T: AsRef<str>,
    S: Stream<Item = BenchResult<T>> + Unpin,
{
    if exact_prefixes.len() != streams.len() {
        return Err("cleanup inventory scope coverage incomplete".into());
    }
    let mut seen_prefixes = BTreeSet::new();
    for exact in exact_prefixes {
        if exact.is_empty() || !exact.ends_with('/') || !seen_prefixes.insert(exact.as_str()) {
            return Err("cleanup exact prefix invalid or repeated".into());
        }
    }
    let mut seen_locations = BTreeSet::new();
    let mut retained = 0usize;
    let mut inventories = Vec::with_capacity(streams.len());
    for (exact, stream) in exact_prefixes.iter().zip(streams) {
        let mut inventory = vec![];
        while let Some(row) = stream.next().await {
            let location = row?;
            if retained == limit {
                return Err(CLEANUP_BOUND_ERROR.into());
            }
            validate_owned_location(exact, location.as_ref())?;
            if !seen_locations.insert(location.as_ref().to_owned()) {
                return Err("cleanup object location repeated".into());
            }
            inventory.push(location);
            retained += 1;
        }
        inventories.push(inventory);
    }
    Ok(inventories)
}

// This gate is shared by the actual purge and negative controls: a failure in
// any later inventory cannot poll the future that deletes SQL rows or blobs.
async fn inventory_then<T, S, F, Fut, R>(
    exact_prefixes: &[String],
    streams: &mut [S],
    limit: usize,
    mutate: F,
) -> BenchResult<R>
where
    T: AsRef<str>,
    S: Stream<Item = BenchResult<T>> + Unpin,
    F: FnOnce(Vec<Vec<T>>) -> Fut,
    Fut: std::future::Future<Output = BenchResult<R>>,
{
    let inventories = collect_cleanup_inventory(exact_prefixes, streams, limit).await?;
    mutate(inventories).await
}

// One completed row is sufficient to reject absence. An errored, malformed or
// present observation cannot become a successful post-cleanup receipt.
async fn require_stream_absence<T, S>(exact_prefix: &str, stream: &mut S) -> BenchResult<()>
where
    T: AsRef<str>,
    S: Stream<Item = BenchResult<T>> + Unpin,
{
    match stream.next().await {
        None => Ok(()),
        Some(Err(error)) => Err(error),
        Some(Ok(location)) => {
            validate_owned_location(exact_prefix, location.as_ref())?;
            Err("owned blob prefix remains after cleanup".into())
        }
    }
}

fn require_cleanup_tables(tables: &[&str]) -> BenchResult<()> {
    let expected: Vec<_> = TABLES
        .iter()
        .copied()
        .filter(|table| tables.contains(table))
        .collect();
    if tables != expected
        || TABLES
            .iter()
            .any(|table| !OPTIONAL_TABLES.contains(table) && !tables.contains(table))
    {
        return Err("cleanup SQL family inventory invalid or required table missing".into());
    }
    Ok(())
}

fn cleanup_sql_absence_receipt(present: &[&str]) -> BenchResult<Value> {
    require_cleanup_tables(present)?;
    Ok(json!(
        TABLES
            .iter()
            .map(|table| json!({"table":table,
        "table_present":present.contains(table),"absent":true,
        "evidence":if present.contains(table) {"zero_rows_for_every_owned_key"}
            else {"optional_table_not_present_information_schema"}}))
            .collect::<Vec<_>>()
    ))
}

async fn purge(connection: &str, config: &RustFsConfig, scopes: &[Scope]) -> BenchResult<Value> {
    purge_with_limit(connection, config, scopes, CLEANUP_OBJECT_LIMIT).await
}

async fn purge_with_limit(
    connection: &str,
    config: &RustFsConfig,
    scopes: &[Scope],
    object_limit: usize,
) -> BenchResult<Value> {
    let store = config
        .build_store()
        .map_err(|_| "cleanup blob client failed")?;
    let exact_prefixes: Vec<_> = scopes
        .iter()
        .map(|scope| format!("{}/", scope.prefix))
        .collect();
    let mut streams: Vec<_> = exact_prefixes
        .iter()
        .map(|exact| {
            store
                .list(Some(&exact.clone().into()))
                .map_ok(|metadata| metadata.location)
                .map_err(|_| "bounded cleanup listing failed".to_owned())
        })
        .collect();
    inventory_then(
        &exact_prefixes,
        &mut streams,
        object_limit,
        |inventories| async {
            // Every blob scope has now completed a bounded, validated inventory.
            // SQL construction, DML and blob deletion are all after that gate.
            let pool = Pool::from_url(connection).map_err(|_| "invalid cleanup connection")?;
            let removed: BenchResult<Vec<&'static str>> = async {
                let mut sql = pool
                    .get_conn()
                    .await
                    .map_err(|_| "cleanup SQL connection failed")?;
                let tables = table_inventory(&mut sql).await?;
                require_cleanup_tables(&tables)?;
                for scope in scopes {
                    for table in &tables {
                        sql.exec_drop(
                            format!("DELETE FROM {table} WHERE volume_key=?"),
                            (scope.key.as_bytes(),),
                        )
                        .await
                        .map_err(|_| "exact owned SQL cleanup failed")?;
                    }
                }
                Ok(tables)
            }
            .await;
            let closed = pool
                .disconnect()
                .await
                .map_err(|_| "cleanup SQL shutdown failed");
            let tables = removed?;
            closed?;
            let mut removed_objects = 0usize;
            for inventory in inventories {
                for location in inventory {
                    store
                        .delete(&location)
                        .await
                        .map_err(|_| "owned blob deletion failed")?;
                    removed_objects += 1;
                }
            }
            let sql_absence = cleanup_sql_absence_receipt(&assert_absent(connection, scopes).await?)?;
            for exact in &exact_prefixes {
                let mut observed = store
                    .list(Some(&exact.clone().into()))
                    .map_ok(|metadata| metadata.location)
                    .map_err(|_| "post-cleanup streaming observation failed".to_owned());
                require_stream_absence(exact, &mut observed).await?;
            }
            Ok(json!({"passed":true,"sql_tables":tables,"owned_scopes":scopes.len(),
                "removed_blob_objects":removed_objects,"inventory_object_limit":object_limit,
                "inventory_scope":"all scopes completed before any deletion; streaming rows, underlying pages unavailable",
                "sql_family_absence":sql_absence,
                "post_cleanup_metadata_absent":true,"post_cleanup_prefixes_absent":true}))
        },
    )
    .await
}

#[test]
#[ignore = "requires explicit owned actual TiDB/RustFS, allocation profiling and retained output"]
// This runtime release gate still lets the pure controls compile in debug mode.
#[allow(clippy::assertions_on_constants)]
fn actual_sdk_tidb_rustfs_metadata_benchmark() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(16)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        assert_eq!(std::env::var("MOUNT_RS_PROFILE_IO").as_deref(),Ok("1"));
        assert!(storage::enabled() && profile::enabled());
        assert!(!cfg!(debug_assertions),"matched benchmark requires release binaries");
        let config = Config::environment().unwrap();
        let output = required("MOUNT_RS_INDEXED_BENCH_OUTPUT").unwrap();
        assert!(Path::new(&output).is_absolute(),"absolute retained output required");
        let mut output = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(output).expect("new private output reservation");
        let key = scope_name().unwrap();
        let scopes: Vec<_> = (0..config.drives).map(|drive|Scope {
            key:format!("{key}-drive-{drive}"),prefix:format!("{key}/blocks-drive-{drive}") }).collect();
        let mut artifact = json!({"schema":"mount-rs-indexed-sdk-benchmark-v1","configuration":config.json(),
            "source_binary_binding":source_receipt().unwrap(),"scopes":scopes.iter().map(|scope|json!({"metadata_key":scope.key,"blob_prefix":scope.prefix})).collect::<Vec<_>>(),
            "requested_layout":"MRC5","setup":null,"stages":[],"oracle":null,"shutdown":null,"cleanup":null,"error":null,"qualified":false,
            "scope":"direct SDK diagnostic; independent pool and filesystem per lane; no QUIC, external OIDC, cross-host, physical IOPS, power-loss or production capacity qualification",
            "cache":"SDK default cache; no all-file warm or cold-cache claim; peer cache not configured; cache hit counts unavailable",
            "timing":"fixed acknowledged cycle count; stage spawn, bookkeeping and drain included; observer snapshots outside reported elapsed; allocator instrumentation affects throughput"});
        // Retain exact ownership before provider mutation, including when an
        // enclosing supervisor terminates this process before terminal output.
        output.write_all(&serde_json::to_vec_pretty(&artifact).unwrap()).expect("initial owned scope receipt write");
        output.sync_all().expect("initial owned scope receipt sync");
        let connection = required("MOUNT_RS_TIDB_URL").unwrap();
        let blocks = RustFsConfig {endpoint:required("MOUNT_RS_RUSTFS_ENDPOINT").unwrap(),bucket:required("MOUNT_RS_RUSTFS_BUCKET").unwrap(),
            region:required("MOUNT_RS_RUSTFS_REGION").unwrap(),access_key_id:required("MOUNT_RS_RUSTFS_ACCESS_KEY_ID").unwrap(),
            secret_access_key:required("MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY").unwrap()};
        let mut clients = vec![];
        let mut ledger = vec![vec![1u64;config.files];config.drives];
        let work: BenchResult<()> = async {
            let store = remote_blocks::resolve_blocks("tidb",&scopes[0].prefix,Some("rustfs"),|name|std::env::var(name).ok())?
                .ok_or("explicit RustFS selection missing")?;
            let mut commands = command::Commands::default();
            let checked = remote_blocks::preflight(&mut commands,&store).await;
            let commands_closed = commands.cleanup().await;
            artifact["rustfs_preflight"] = checked?;
            commands_closed?;
            let pool = Pool::from_url(&connection).map_err(|_| "invalid private TiDB endpoint")?;
            let observed: BenchResult<Value> = async {
                let mut sql = pool.get_conn().await.map_err(|_| "actual TiDB identity connection failed")?;
                let (identity,version): (String,String) = sql.query_first("SELECT tidb_version(), VERSION()")
                    .await.map_err(|_| "actual TiDB identity query failed")?.ok_or("missing TiDB identity")?;
                if !identity.to_lowercase().contains("release version:") || !version.to_lowercase().contains("tidb") {return Err("actual TiDB required".into());}
                Ok(json!({"identity":identity,"version":version}))
            }.await;
            let closed = pool.disconnect().await.map_err(|_| "TiDB identity shutdown failed");
            artifact["tidb"] = observed?;
            closed?;
            artifact["initial_table_inventory"] = json!(assert_absent(&connection,&scopes).await?);
            for scope in &scopes {
                if !blocks.observe_owned_prefix_absence(&scope.prefix).await.map_err(|_| "owned blob freshness observation failed")? {
                    return Err("blob scope was not absent".into());
                }
            }
            let started = Instant::now();
            tokio::time::timeout(Duration::from_secs(config.setup_seconds as u64),setup(config,&connection,&blocks,&scopes,&mut clients))
                .await.map_err(|_| "setup deadline; scope retained")??;
            artifact["setup"] = json!({"passed":true,"elapsed_seconds":started.elapsed().as_secs_f64(),"files":config.drives*config.files,"payload_bytes":config.drives*config.files*BYTES,
                "clients":clients.len(),"preexisting_metadata_absent":true,"preexisting_blob_prefixes_absent":true});
            artifact["persisted_mode_receipts"] = mode_receipts(&connection,&scopes).await?;
            for (index,pattern) in PATTERNS.iter().enumerate() {
                let measured = stage(config,*pattern,index,&clients,&mut ledger).await?;
                eprintln!("INDEXED_SDK_STAGE pattern={} cycles={} elapsed_seconds={}",pattern.label(),measured["counts"]["cycles"],measured["elapsed_seconds_including_drain"]);
                artifact["stages"].as_array_mut().unwrap().push(measured);
            }
            Ok(())
        }.await;
        // Every returned provider is closed even after a workload failure. An
        // interrupted/unacknowledged operation never authorizes namespace purge.
        let closed = tokio::time::timeout(Duration::from_secs(60),shutdown(&clients)).await
            .map_err(|_| "shutdown deadline".to_owned()).and_then(|value|value);
        artifact["shutdown"] = json!({"passed":closed.is_ok(),"error":closed.as_ref().err()});
        drop(clients);
        let result = async {
            work?;
            closed?;
            artifact["oracle"] = tokio::time::timeout(Duration::from_secs(config.oracle_seconds as u64),oracle(config,&connection,&blocks,&scopes,&ledger))
                .await.map_err(|_| "fresh oracle deadline; scope retained")??;
            artifact["cleanup"] = tokio::time::timeout(Duration::from_secs(120),purge(&connection,&blocks,&scopes))
                .await.map_err(|_| "purge deadline; cleanup incomplete")??;
            Ok::<(),String>(())
        }.await;
        artifact["qualified"] = json!(result.is_ok());
        artifact["error"] = json!(result.as_ref().err());
        let encoded = serde_json::to_vec_pretty(&artifact).unwrap();
        output.set_len(0).expect("terminal output truncate");
        output.seek(SeekFrom::Start(0)).expect("terminal output seek");
        output.write_all(&encoded).expect("retained artifact write");
        output.sync_all().expect("retained artifact sync");
        eprintln!("INDEXED_SDK_BENCHMARK qualified={} files_per_drive={} clients={}",result.is_ok(),config.files,config.drives*config.lanes);
        result.expect("SDK metadata benchmark failed; inspect private artifact; failed scopes retained");
    });
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetiredWireManifest {
    schema: String,
    root_key: String,
    drive_count: usize,
    metadata_keys: Vec<String>,
    blob_prefixes: Vec<String>,
    retired: bool,
    tidb_url_sha256: String,
    rustfs_endpoint: String,
    rustfs_bucket: String,
    rustfs_region: String,
    rustfs_cid: String,
    rustfs_owner: String,
}
fn retired_scopes(manifest: &RetiredWireManifest) -> BenchResult<Vec<Scope>> {
    let suffix = manifest
        .root_key
        .strip_prefix("remote-saturation-")
        .ok_or("invalid retired wire root")?;
    let mut components = suffix.split('-');
    let canonical = |component: &str| {
        !component.is_empty()
            && component.bytes().all(|byte| byte.is_ascii_digit())
            && component
                .parse::<u128>()
                .is_ok_and(|number| number > 0 && number.to_string() == component)
    };
    if !components.next().is_some_and(canonical)
        || !components.next().is_some_and(canonical)
        || components.next().is_some()
        || manifest.schema != "mount-rs-retired-wire-scope-v1"
        || !manifest.retired
        || !(1..=1000).contains(&manifest.drive_count)
        || manifest.root_key.len() > 120
        || manifest.metadata_keys.len() != manifest.drive_count
        || manifest.blob_prefixes.len() != manifest.drive_count
    {
        return Err("invalid retired wire manifest identity or geometry".into());
    }
    let mut scopes = vec![];
    for index in 0..manifest.drive_count {
        let key = format!("{}-sandbox-{index}", manifest.root_key);
        let prefix = format!("{}/blocks-sandbox-{index}", manifest.root_key);
        if manifest.metadata_keys[index] != key || manifest.blob_prefixes[index] != prefix {
            return Err("retired scope order, index or exact derivation invalid".into());
        }
        scopes.push(Scope { key, prefix });
    }
    Ok(scopes)
}
fn read_retired_manifest(path: &str) -> BenchResult<RetiredWireManifest> {
    if !Path::new(path).is_absolute() {
        return Err("absolute private manifest required".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| "private manifest open failed")?;
    let before = file
        .metadata()
        .map_err(|_| "private manifest metadata failed")?;
    if !before.is_file()
        || before.uid() != unsafe { libc::geteuid() }
        || before.nlink() != 1
        || before.mode() & 0o077 != 0
        || before.len() == 0
        || before.len() > 65536
    {
        return Err("private manifest type, ownership, mode or bound invalid".into());
    }
    let mut bytes = Vec::with_capacity(before.len() as usize);
    std::io::Read::by_ref(&mut file)
        .take(65537)
        .read_to_end(&mut bytes)
        .map_err(|_| "manifest bounded read failed")?;
    let after = file
        .metadata()
        .map_err(|_| "private manifest recheck failed")?;
    let named =
        std::fs::symlink_metadata(path).map_err(|_| "private manifest name recheck failed")?;
    if bytes.len() as u64 != before.len()
        || after.dev() != before.dev()
        || after.ino() != before.ino()
        || after.len() != before.len()
        || after.mtime() != before.mtime()
        || after.mtime_nsec() != before.mtime_nsec()
        || named.dev() != before.dev()
        || named.ino() != before.ino()
        || !named.is_file()
    {
        return Err("private manifest changed during read".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "strict retired manifest parse failed".into())
}
fn validate_retired_binding(
    manifest: &RetiredWireManifest,
    connection: &str,
    config: &RustFsConfig,
) -> BenchResult<()> {
    if manifest.tidb_url_sha256 != digest(connection.as_bytes())
        || manifest.rustfs_endpoint != config.endpoint
        || manifest.rustfs_bucket != config.bucket
        || manifest.rustfs_region != config.region
        || manifest.rustfs_cid != required("MOUNT_RS_REMOTE_RUSTFS_CID")?
        || manifest.rustfs_owner != required("MOUNT_RS_BACKING_RUSTFS_OWNER")?
    {
        return Err("retired fixture binding changed".into());
    }
    Ok(())
}

#[test]
#[ignore = "root-owned manifest authorizes cleanup of successful retired separate-drive wire run only"]
fn actual_retired_wire_scope_cleanup() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let manifest = read_retired_manifest(&required("MOUNT_RS_INDEXED_BENCH_CLEANUP_MANIFEST").unwrap()).unwrap();
        let scopes = retired_scopes(&manifest).unwrap();
        let output = required("MOUNT_RS_INDEXED_BENCH_CLEANUP_RECEIPT").unwrap();
        assert!(Path::new(&output).is_absolute(),"absolute cleanup receipt required");
        let mut receipt = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(output).expect("new private cleanup receipt");
        let mut artifact = json!({"schema":"mount-rs-retired-wire-cleanup-v1","root_key":manifest.root_key,
            "drive_count":scopes.len(),"retired":true,"cleanup":null,"error":null,
            "scope":"explicit root-owned retired manifest; exact derived separate-drive keys and slash-delimited prefixes; no operation replay"});
        receipt.write_all(&serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();receipt.sync_all().unwrap();
        let config = RustFsConfig {endpoint:required("MOUNT_RS_RUSTFS_ENDPOINT").unwrap(),bucket:required("MOUNT_RS_RUSTFS_BUCKET").unwrap(),
            region:required("MOUNT_RS_RUSTFS_REGION").unwrap(),access_key_id:required("MOUNT_RS_RUSTFS_ACCESS_KEY_ID").unwrap(),
            secret_access_key:required("MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY").unwrap()};
        let connection = required("MOUNT_RS_TIDB_URL").unwrap();
        validate_retired_binding(&manifest,&connection,&config).expect("retired fixture binding");
        let store = remote_blocks::resolve_blocks("tidb",&scopes[0].prefix,Some("rustfs"),|name|std::env::var(name).ok()).unwrap().unwrap();
        let mut commands = command::Commands::default();
        let checked = remote_blocks::preflight(&mut commands,&store).await;
        let commands_closed = commands.cleanup().await;
        artifact["rustfs_preflight"] = checked.expect("unchanged retired fixture preflight");
        commands_closed.expect("retired fixture observer cleanup");
        let result = tokio::time::timeout(Duration::from_secs(120),purge(&connection,&config,&scopes)).await
            .map_err(|_| "retired cleanup deadline; cleanup incomplete".to_owned()).and_then(|value|value);
        artifact["cleanup"]=json!(result.as_ref().ok());artifact["error"]=json!(result.as_ref().err());
        receipt.set_len(0).unwrap();receipt.seek(SeekFrom::Start(0)).unwrap();
        receipt.write_all(&serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();receipt.sync_all().unwrap();
        result.expect("retired exact-scope cleanup incomplete; inspect private receipt");
        eprintln!("INDEXED_RETIRED_WIRE_CLEANUP passed=true drives={}",scopes.len());
    });
}

async fn cleanup_metadata_witness(connection: &str, scopes: &[Scope]) -> BenchResult<Value> {
    let pool = Pool::from_url(connection).map_err(|_| "invalid cleanup witness connection")?;
    let observed: BenchResult<Value> = async {
        let mut sql = pool
            .get_conn()
            .await
            .map_err(|_| "cleanup witness connection failed")?;
        let present = table_inventory(&mut sql).await?;
        require_cleanup_tables(&present)?;
        let mut rows = vec![];
        for scope in scopes {
            let mut families = vec![];
            for table in TABLES {
                let count = if present.contains(&table) {
                    Some(
                        sql.exec_first::<u64, _, _>(
                            format!("SELECT COUNT(*) FROM {table} WHERE volume_key=?"),
                            (scope.key.as_bytes(),),
                        )
                        .await
                        .map_err(|_| "cleanup witness query failed")?
                        .ok_or("cleanup witness count missing")?,
                    )
                } else {
                    None
                };
                families.push(
                    json!({"table":table,"table_present":present.contains(&table),"rows":count}),
                );
            }
            rows.push(json!({"metadata_key":scope.key,"families":families}));
        }
        Ok(json!(rows))
    }
    .await;
    let closed = pool
        .disconnect()
        .await
        .map_err(|_| "cleanup witness SQL shutdown failed");
    let observed = observed?;
    closed?;
    Ok(observed)
}

async fn cleanup_blob_witness(config: &RustFsConfig, scopes: &[Scope]) -> BenchResult<Value> {
    let store = config
        .build_store()
        .map_err(|_| "cleanup witness blob client failed")?;
    let prefixes: Vec<_> = scopes
        .iter()
        .map(|scope| format!("{}/", scope.prefix))
        .collect();
    let mut streams: Vec<_> = prefixes
        .iter()
        .map(|prefix| {
            store
                .list(Some(&prefix.clone().into()))
                .map_ok(|metadata| metadata.location)
                .map_err(|_| "cleanup witness streaming listing failed".to_owned())
        })
        .collect();
    let inventories =
        collect_cleanup_inventory(&prefixes, &mut streams, CLEANUP_OBJECT_LIMIT).await?;
    Ok(json!(
        inventories
            .into_iter()
            .zip(prefixes)
            .map(|(inventory, prefix)| {
                let mut locations: Vec<_> = inventory
                    .into_iter()
                    .map(|path| path.as_ref().to_owned())
                    .collect();
                locations.sort_unstable();
                json!({"exact_prefix":prefix,"locations":locations})
            })
            .collect::<Vec<_>>()
    ))
}

#[test]
#[ignore = "root-owned one-drive retired manifest; proves a two-object cleanup bound admits no deletion"]
fn actual_retired_wire_cleanup_bound_rejects_before_mutation() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let manifest = read_retired_manifest(&required("MOUNT_RS_INDEXED_BENCH_CLEANUP_MANIFEST").unwrap()).unwrap();
        let scopes = retired_scopes(&manifest).unwrap();
        assert_eq!(scopes.len(),1,"small-cap control requires one explicit retired drive");
        let connection = required("MOUNT_RS_TIDB_URL").unwrap();
        let config = RustFsConfig {endpoint:required("MOUNT_RS_RUSTFS_ENDPOINT").unwrap(),bucket:required("MOUNT_RS_RUSTFS_BUCKET").unwrap(),
            region:required("MOUNT_RS_RUSTFS_REGION").unwrap(),access_key_id:required("MOUNT_RS_RUSTFS_ACCESS_KEY_ID").unwrap(),
            secret_access_key:required("MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY").unwrap()};
        validate_retired_binding(&manifest,&connection,&config).expect("retired fixture binding");
        let store = remote_blocks::resolve_blocks("tidb",&scopes[0].prefix,Some("rustfs"),|name|std::env::var(name).ok()).unwrap().unwrap();
        let mut commands = command::Commands::default();
        let checked = remote_blocks::preflight(&mut commands,&store).await;
        let commands_closed = commands.cleanup().await;
        let preflight = checked.expect("unchanged retired fixture preflight");
        commands_closed.expect("retired fixture observer cleanup");
        let output = required("MOUNT_RS_INDEXED_BENCH_CLEANUP_RECEIPT").unwrap();
        assert!(Path::new(&output).is_absolute(),"absolute bound-control receipt required");
        let mut receipt = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
            .custom_flags(libc::O_NOFOLLOW).open(output).expect("new private bound-control receipt");
        let mut artifact = json!({"schema":"mount-rs-retired-wire-cleanup-bound-v1","root_key":manifest.root_key,
            "retired":true,"rustfs_preflight":preflight,"proof":null,"error":null,
            "scope":"explicit retired one-drive manifest; unchanged seven-family count witnesses and complete bounded native-location blob inventories; no deletion"});
        receipt.write_all(&serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();receipt.sync_all().unwrap();
        let observed: Result<BenchResult<Value>,tokio::time::error::Elapsed> = tokio::time::timeout(Duration::from_secs(120),async {
            let before_sql = cleanup_metadata_witness(&connection,&scopes).await?;
            let families = before_sql[0]["families"].as_array()
                .ok_or("small-cap SQL witness families missing")?;
            if !families.iter().any(|family| family["table"] == "mount_rs_tidb_metadata"
                && family["rows"].as_u64().is_some_and(|rows| rows > 0)) {
                return Err("small-cap control requires populated metadata namespace".into());
            }
            let before_blobs = cleanup_blob_witness(&config,&scopes).await?;
            let objects = before_blobs.as_array().ok_or("blob witness shape invalid")?
                .iter().map(|scope|scope["locations"].as_array().map(Vec::len).ok_or("blob location witness invalid"))
                .collect::<Result<Vec<_>,_>>()?.into_iter().sum::<usize>();
            if objects <= 2 {return Err("small-cap control requires more than two witnessed objects".into());}
            let rejected = purge_with_limit(&connection,&config,&scopes,2).await;
            if rejected.as_ref().err().map(String::as_str) != Some(CLEANUP_BOUND_ERROR) {
                return Err("small-cap cleanup did not reject at exact object bound".into());
            }
            let after_sql = cleanup_metadata_witness(&connection,&scopes).await?;
            let after_blobs = cleanup_blob_witness(&config,&scopes).await?;
            if before_sql != after_sql || before_blobs != after_blobs {
                return Err("small-cap cleanup changed exact SQL count or blob inventory witnesses".into());
            }
            Ok(json!({"passed":true,"requested_object_limit":2,"witnessed_objects":objects,
                "limit_error":CLEANUP_BOUND_ERROR,"sql_families":TABLES,
                "sql_witness_sha256":digest(&serde_json::to_vec(&before_sql).map_err(|_| "SQL witness encoding failed")?),
                "blob_witness_sha256":digest(&serde_json::to_vec(&before_blobs).map_err(|_| "blob witness encoding failed")?),
                "sql_counts_unchanged":true,"complete_blob_inventory_unchanged":true}))
        }).await;
        let observed = observed.map_err(|_| "small-cap control deadline; proof incomplete".to_owned()).and_then(|value|value);
        artifact["proof"]=json!(observed.as_ref().ok());artifact["error"]=json!(observed.as_ref().err());
        receipt.set_len(0).unwrap();receipt.seek(SeekFrom::Start(0)).unwrap();
        receipt.write_all(&serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();receipt.sync_all().unwrap();
        observed.expect("actual small-cap cleanup control failed; inspect private receipt");
        eprintln!("INDEXED_RETIRED_WIRE_CLEANUP_BOUND passed=true requested_object_limit=2 deletion_admitted=false");
    });
}

fn cleanup_counted_stream<T: Unpin>(
    rows: Vec<BenchResult<T>>,
    consumed: Arc<std::sync::atomic::AtomicUsize>,
) -> impl Stream<Item = BenchResult<T>> + Unpin {
    futures_util::stream::iter(rows).inspect(move |_| {
        consumed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    })
}

#[tokio::test]
async fn benchmark_cleanup_stream_allows_n_and_rejects_n_plus_one_at_sentinel() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let prefixes = vec!["owned/".to_owned()];
    let consumed = Arc::new(AtomicUsize::new(0));
    let mut allowed = vec![cleanup_counted_stream(
        vec![
            Ok("owned/a".to_owned()),
            Ok("owned/b".to_owned()),
            Ok("owned/c".to_owned()),
        ],
        consumed.clone(),
    )];
    let retained = collect_cleanup_inventory(&prefixes, &mut allowed, 3)
        .await
        .unwrap();
    assert_eq!(retained[0].len(), 3);
    assert_eq!(consumed.load(Ordering::SeqCst), 3);
    let consumed = Arc::new(AtomicUsize::new(0));
    let mut overflow = vec![cleanup_counted_stream(
        (0..6).map(|index| Ok(format!("owned/{index}"))).collect(),
        consumed.clone(),
    )];
    assert_eq!(
        collect_cleanup_inventory(&prefixes, &mut overflow, 3)
            .await
            .unwrap_err(),
        CLEANUP_BOUND_ERROR
    );
    assert_eq!(
        consumed.load(Ordering::SeqCst),
        4,
        "must not consume beyond the first overflow sentinel"
    );
}

#[tokio::test]
async fn benchmark_cleanup_stream_rejects_outside_empty_and_duplicate_locations() {
    let prefixes = vec!["owned/".to_owned()];
    for rows in [
        vec![Ok("owned-sibling/a".to_owned())],
        vec![Ok("owned/".to_owned())],
        vec![Ok("owned/a".to_owned()), Ok("owned/a".to_owned())],
        vec![Err("listing error".to_owned())],
    ] {
        let mut streams = vec![futures_util::stream::iter(rows)];
        assert!(
            collect_cleanup_inventory(&prefixes, &mut streams, 3)
                .await
                .is_err()
        );
    }
    struct OriginalLocation {
        name: String,
        identity: usize,
    }
    impl AsRef<str> for OriginalLocation {
        fn as_ref(&self) -> &str {
            &self.name
        }
    }
    let mut streams = vec![futures_util::stream::iter(vec![Ok(OriginalLocation {
        name: "owned/original".into(),
        identity: 42,
    })])];
    let retained = collect_cleanup_inventory(&prefixes, &mut streams, 3)
        .await
        .unwrap();
    assert_eq!(
        retained[0][0].identity, 42,
        "retain the original location object, not a reparsed string"
    );
}

#[tokio::test]
async fn benchmark_cleanup_later_scope_overflow_never_admits_mutation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let prefixes = vec!["first/".to_owned(), "later/".to_owned()];
    let first = Arc::new(AtomicUsize::new(0));
    let later = Arc::new(AtomicUsize::new(0));
    let mut streams = vec![
        cleanup_counted_stream(
            vec![Ok("first/a".to_owned()), Ok("first/b".to_owned())],
            first.clone(),
        ),
        cleanup_counted_stream(
            vec![
                Ok("later/a".to_owned()),
                Ok("later/b".to_owned()),
                Ok("later/c".to_owned()),
            ],
            later.clone(),
        ),
    ];
    let mutations = AtomicUsize::new(0);
    let observed = inventory_then(&prefixes, &mut streams, 3, |_| async {
        mutations.fetch_add(1, Ordering::SeqCst);
        Ok(())
    })
    .await;
    assert_eq!(observed.unwrap_err(), CLEANUP_BOUND_ERROR);
    assert_eq!(first.load(Ordering::SeqCst), 2);
    assert_eq!(later.load(Ordering::SeqCst), 2);
    assert_eq!(mutations.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn benchmark_cleanup_absence_errors_or_present_rows_cannot_pass_receipt() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    for first in [
        Ok("owned/present".to_owned()),
        Ok("owned/".to_owned()),
        Err("observation failed".to_owned()),
    ] {
        let consumed = Arc::new(AtomicUsize::new(0));
        let mut stream = cleanup_counted_stream(
            vec![first, Ok("owned/unconsumed".to_owned())],
            consumed.clone(),
        );
        let receipt = require_stream_absence("owned/", &mut stream)
            .await
            .map(|()| json!({"passed":true}));
        assert!(receipt.is_err());
        assert_eq!(
            consumed.load(Ordering::SeqCst),
            1,
            "absence requires only the first observed row"
        );
    }
    let mut empty = futures_util::stream::iter(Vec::<BenchResult<String>>::new());
    assert!(require_stream_absence("owned/", &mut empty).await.is_ok());
}

#[test]
fn benchmark_cleanup_absence_reports_all_seven_families_in_older_schemas() {
    let older: Vec<_> = TABLES
        .iter()
        .copied()
        .filter(|table| !OPTIONAL_TABLES.contains(table))
        .collect();
    let report = cleanup_sql_absence_receipt(&older).unwrap();
    assert_eq!(report.as_array().unwrap().len(), 7);
    for (row, table) in report.as_array().unwrap().iter().zip(TABLES) {
        assert_eq!(row["table"], table);
        assert_eq!(row["absent"], true);
        assert_eq!(row["table_present"], !OPTIONAL_TABLES.contains(&table));
    }
    let mut incomplete = older.clone();
    incomplete.remove(0);
    assert!(cleanup_sql_absence_receipt(&incomplete).is_err());
    let mut repeated = older.clone();
    repeated.push(older[0]);
    assert!(cleanup_sql_absence_receipt(&repeated).is_err());
}

#[test]
fn benchmark_full_byte_and_eof_oracle_rejects_corruption() {
    let mut wanted = vec![0; BYTES];
    payload_into(&mut wanted, 1, 99, 12);
    assert!(validate_read(&wanted, BYTES, &wanted, 0).is_ok());
    for index in [0, 17, BYTES / 2, BYTES - 1] {
        let mut corrupt = wanted.clone();
        corrupt[index] ^= 1;
        assert!(validate_read(&corrupt, BYTES, &wanted, 0).is_err());
    }
    assert!(validate_read(&wanted, BYTES - 1, &wanted, 0).is_err());
    assert!(validate_read(&wanted, BYTES, &wanted, 1).is_err());
    let mut other = vec![0; BYTES];
    payload_into(&mut other, 1, 100, 12);
    assert!(validate_read(&other, BYTES, &wanted, 0).is_err());
}
#[test]
fn benchmark_counts_require_actual_complete_payload_and_close_receipts() {
    let valid = Counts {
        cycles: 3,
        opens: 3,
        closes: 3,
        reads: 2,
        writes: 1,
        verified_reads: 2,
        eof_checks: 2,
        read_bytes: 8192,
        written_bytes: 4096,
    };
    assert!(valid.validate(3, 1).is_ok());
    let mut changed = valid.clone();
    changed.closes = 2;
    assert!(changed.validate(3, 1).is_err());
    let mut changed = valid.clone();
    changed.verified_reads = 1;
    assert!(changed.validate(3, 1).is_err());
    let mut changed = valid.clone();
    changed.eof_checks = 1;
    assert!(changed.validate(3, 1).is_err());
    let mut changed = valid.clone();
    changed.written_bytes = 4095;
    assert!(changed.validate(3, 1).is_err());
    assert!(Counts::default().validate(3, 1).is_err());
}
#[test]
fn benchmark_ledger_rejects_duplicate_cross_lane_and_missing_files_before_mutation() {
    let config = Config {
        drives: 1,
        lanes: 2,
        files: 10,
        iterations: 2,
        setup_seconds: 1,
        stage_seconds: 1,
        oracle_seconds: 1,
    };
    let result = |lane| LaneResult {
        drive: 0,
        lane,
        counts: Counts::default(),
        generations: (lane..10).step_by(2).map(|file| (file, 7)).collect(),
        cycle_latencies_us: vec![],
    };
    let mut ledger = vec![vec![1; 10]];
    assert!(apply_lanes(config, &[result(0), result(1)], &mut ledger).is_ok());
    assert_eq!(ledger, vec![vec![7; 10]]);
    let mut ledger = vec![vec![1; 10]];
    assert!(apply_lanes(config, &[result(0), result(0)], &mut ledger).is_err());
    assert_eq!(ledger, vec![vec![1; 10]]);
    let mut missing = result(1);
    missing.generations.pop();
    assert!(apply_lanes(config, &[result(0), missing], &mut ledger).is_err());
    assert_eq!(ledger, vec![vec![1; 10]]);
    let mut cross = result(1);
    cross.generations[0].0 = 0;
    assert!(apply_lanes(config, &[result(0), cross], &mut ledger).is_err());
    assert_eq!(ledger, vec![vec![1; 10]]);
}
#[test]
fn benchmark_storage_boundary_rejects_pending_and_relabelled_rows() {
    let mut snapshot = storage::snapshot();
    assert!(validate_storage(&snapshot).is_ok());
    snapshot.in_flight = 1;
    assert!(validate_storage(&snapshot).is_err());
    snapshot.in_flight = 0;
    snapshot.entries[0].name = "invented";
    assert!(validate_storage(&snapshot).is_err());
}
#[test]
fn benchmark_retired_manifest_rejects_unowned_misindexed_duplicate_and_unknown_scopes() {
    let valid = json!({"schema":"mount-rs-retired-wire-scope-v1","root_key":"remote-saturation-123-456",
        "drive_count":2,"metadata_keys":["remote-saturation-123-456-sandbox-0","remote-saturation-123-456-sandbox-1"],
        "blob_prefixes":["remote-saturation-123-456/blocks-sandbox-0","remote-saturation-123-456/blocks-sandbox-1"],"retired":true,
        "tidb_url_sha256":"a".repeat(64),"rustfs_endpoint":"http://127.0.0.1:1234/","rustfs_bucket":"bucket",
        "rustfs_region":"us-east-1","rustfs_cid":"b".repeat(64),"rustfs_owner":"mount-rs-rustfs-test"});
    let parsed: RetiredWireManifest = serde_json::from_value(valid.clone()).unwrap();
    assert_eq!(retired_scopes(&parsed).unwrap().len(), 2);
    for (field, value) in [
        ("retired", json!(false)),
        ("drive_count", json!(3)),
        ("root_key", json!("remote-saturation-0123-456")),
        (
            "metadata_keys",
            json!([
                "remote-saturation-123-456-sandbox-0",
                "remote-saturation-123-456-sandbox-0"
            ]),
        ),
        (
            "blob_prefixes",
            json!([
                "remote-saturation-123-456/blocks-sandbox-0",
                "remote-saturation-123-456/blocks-sandbox-10"
            ]),
        ),
    ] {
        let mut corrupt = valid.clone();
        corrupt[field] = value;
        assert!(retired_scopes(&serde_json::from_value(corrupt).unwrap()).is_err());
    }
    let mut unknown = valid;
    unknown["bypass"] = json!(true);
    assert!(serde_json::from_value::<RetiredWireManifest>(unknown).is_err());
}
