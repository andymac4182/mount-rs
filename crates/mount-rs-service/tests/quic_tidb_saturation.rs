#[test]
fn measurement_oracles() {
    assert!(timeout_message(false).contains("read request timeout"));
    assert!(!timeout_message(false).contains("uncertain"));
    assert!(!timeout_message(false).contains("write"));
    assert!(timeout_message(true).contains("write outcome uncertain"));
    assert!(timeout_message(true).contains("never replayed"));
    assert!(validate_depths(&[1, 2, 4, 8]).is_ok());
    assert!(validate_depths(&[0]).is_err());
    assert!(validate_depths(&[33]).is_err());
    assert!(validate_depths(&[1, 1]).is_err());
    assert!(valid_read(&serde_json::json!([1, 2])).is_err());
    assert!(valid_write(&serde_json::json!(4095)).is_err());
    assert_ne!(payload(0, 0, 1), payload(0, 1, 1));
    let mut h = Histogram::default();
    h.record(std::time::Duration::from_micros(100));
    h.record(std::time::Duration::from_micros(200));
    assert_eq!(h.count, 2);
    assert!(h.percentile(99) >= 200);
}

#[path = "support/tidb_wire.rs"]
mod wire;
use mount_rs_core::Loopback;
use mount_rs_remote_protocol::OperationName;
use mysql_async::{Pool, prelude::Queryable};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const CLIENTS: usize = 100;
const SERVERS: usize = 10;
const BYTES: usize = 4096;
fn validate_depths(depths: &[usize]) -> Result<(), String> {
    if depths.is_empty()
        || depths.len() > 6
        || depths.iter().any(|d| !(1..=32).contains(d))
        || depths.iter().copied().collect::<BTreeSet<_>>().len() != depths.len()
    {
        return Err("depths require 1..6 distinct integers in 1..32".into());
    }
    Ok(())
}
fn valid_read(v: &Value) -> Result<(), String> {
    if v.as_array()
        .is_some_and(|a| a.len() == BYTES && a.iter().all(|v| v.as_u64().is_some_and(|b| b <= 255)))
    {
        Ok(())
    } else {
        Err("partial or invalid read".into())
    }
}
fn valid_write(v: &Value) -> Result<(), String> {
    if v == &json!(BYTES) {
        Ok(())
    } else {
        Err("partial or invalid write".into())
    }
}
fn payload(client: usize, lane: usize, seq: u64) -> Vec<u8> {
    let mut bytes = vec![0; BYTES];
    let marker = format!("client={client};lane={lane};sequence={seq};");
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i as u64 * 31 + seq * 13 + client as u64 * 17 + lane as u64 * 19) as u8;
    }
    bytes[..marker.len()].copy_from_slice(marker.as_bytes());
    bytes
}
// Fixed 64-bucket logarithmic histogram, upper-bound microseconds; constant memory per worker.
#[derive(Clone)]
struct Histogram {
    buckets: [u64; 64],
    count: u64,
}
impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: [0; 64],
            count: 0,
        }
    }
}
impl Histogram {
    fn record(&mut self, d: Duration) {
        let us = d.as_micros().max(1).min(u64::MAX as u128) as u64;
        let bucket = (64 - (us - 1).leading_zeros()) as usize;
        self.buckets[bucket.min(63)] += 1;
        self.count += 1;
    }
    fn merge(&mut self, other: &Self) {
        for (a, b) in self.buckets.iter_mut().zip(other.buckets) {
            *a += b;
        }
        self.count += other.count;
    }
    fn percentile(&self, p: u64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let rank = (self.count * p).div_ceil(100);
        let mut n = 0;
        for (i, b) in self.buckets.iter().enumerate() {
            n += b;
            if n >= rank {
                return 1u64 << i;
            }
        }
        u64::MAX
    }
    fn json(&self) -> Value {
        json!({"completed":self.count,"p50_us_upper":self.percentile(50),"p95_us_upper":self.percentile(95),"p99_us_upper":self.percentile(99)})
    }
}
#[derive(Clone, Copy, Debug)]
enum Mode {
    Read,
    Write,
    Mixed,
}
#[derive(Default)]
struct LaneResult {
    read: Histogram,
    write: Histogram,
    last: Vec<(usize, u64)>,
    errors: Vec<String>,
}
struct Client {
    connection: quinn::Connection,
    handles: Vec<u64>,
}
fn env_num(name: &str, default: usize, min: usize, max: usize) -> usize {
    let n = std::env::var(name)
        .map(|v| v.parse().expect("integer setting required"))
        .unwrap_or(default);
    assert!(
        (min..=max).contains(&n),
        "{name} outside bounds {min}..={max}"
    );
    n
}
#[allow(clippy::too_many_arguments)] // Explicit worker inputs are test-only and immutable.
async fn lane(
    connection: quinn::Connection,
    handle: u64,
    client: usize,
    lane: usize,
    depth: usize,
    blocks: usize,
    mode: Mode,
    deadline: Instant,
    base: u64,
    timeout: Duration,
    mut expected: Vec<(usize, u64)>,
) -> LaneResult {
    let mut result = LaneResult::default();
    let mut seq = 0u64;
    let mut rng = (client as u64 + 1) * 7919 + (lane as u64 + 1) * 104729;
    let positions: Vec<usize> = (lane..blocks).step_by(depth).collect();
    while Instant::now() < deadline {
        seq += 1;
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let block = positions[rng as usize % positions.len()];
        let writing =
            matches!(mode, Mode::Write) || matches!(mode, Mode::Mixed) && seq.is_multiple_of(2);
        let name = if writing {
            OperationName::HandleWrite
        } else {
            OperationName::HandleRead
        };
        let body = if writing {
            json!({"handle":handle,"position":block*BYTES,"data":payload(client,lane,base+seq)})
        } else {
            json!({"handle":handle,"position":block*BYTES,"length":BYTES})
        };
        let start = Instant::now();
        let response = tokio::time::timeout(
            timeout,
            wire::success(&connection, base + seq, "left", name, body),
        )
        .await;
        match response {
            Ok(Ok(v)) => {
                let valid = if writing {
                    valid_write(&v)
                } else {
                    valid_read(&v)
                };
                if let Err(e) = valid {
                    result.errors.push(e);
                    break;
                }
                if writing {
                    result.write.record(start.elapsed());
                    expected[block] = (lane, base + seq);
                    if let Some(p) = result.last.iter_mut().find(|(b, _)| *b == block) {
                        p.1 = base + seq;
                    } else {
                        result.last.push((block, base + seq));
                    }
                } else {
                    if v != json!(payload(client, expected[block].0, expected[block].1)) {
                        result.errors.push("read content mismatch".into());
                        break;
                    }
                    result.read.record(start.elapsed());
                }
            }
            Ok(Err(e)) => {
                result.errors.push(e);
                break;
            }
            Err(_) => {
                result.errors.push(timeout_message(writing).into());
                break;
            }
        }
    }
    result
}
#[allow(clippy::too_many_arguments)]
async fn stage(
    clients: &[Client],
    depth: usize,
    blocks: usize,
    mode: Mode,
    seconds: usize,
    base: u64,
    timeout: Duration,
    expected: &[Vec<(usize, u64)>],
) -> (Value, Vec<(usize, usize, usize, u64)>, Vec<String>) {
    let start_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds as u64);
    let mut tasks = tokio::task::JoinSet::new();
    for (client, c) in clients.iter().enumerate() {
        for lane_id in 0..depth {
            let connection = c.connection.clone();
            let handle = c.handles[lane_id];
            let expected = expected[client].clone();
            tasks.spawn(async move {
                (
                    client,
                    lane_id,
                    lane(
                        connection,
                        handle,
                        client,
                        lane_id,
                        depth,
                        blocks,
                        mode,
                        deadline,
                        base + lane_id as u64 * 10_000_000,
                        timeout,
                        expected,
                    )
                    .await,
                )
            });
        }
    }
    let (mut read, mut write) = (Histogram::default(), Histogram::default());
    let mut errors = vec![];
    let mut ledger = vec![];
    while let Some(r) = tasks.join_next().await {
        match r {
            Ok((client, lane, r)) => {
                read.merge(&r.read);
                write.merge(&r.write);
                errors.extend(r.errors);
                ledger.extend(
                    r.last
                        .into_iter()
                        .map(|(block, seq)| (client, lane, block, seq)),
                );
            }
            Err(e) => errors.push(format!("worker failed: {e}")),
        }
    }
    if read.count + write.count == 0 {
        errors.push("stage completed zero operations".into());
    }
    let elapsed = start.elapsed().as_secs_f64();
    let iops = (read.count + write.count) as f64 / elapsed;
    (
        json!({"mode":format!("{mode:?}"),"nominal_seconds":seconds,"start_unix_ms":start_unix_ms,"finish_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),"clients":CLIENTS,"servers":SERVERS,"per_client_depth":depth,"total_queue_depth":CLIENTS*depth,"elapsed_seconds_including_drain":elapsed,"read":read.json(),"write":write.json(),"read_iops":read.count as f64/elapsed,"write_iops":write.count as f64/elapsed,"total_iops":iops,"payload_mib_per_second":iops*BYTES as f64/1048576.0,"reference_target_iops":100000,"target_attainment":iops/100000.0,"failures":errors.len(),"cache":"cache-warm randomized dataset; no cold-cache claim"}),
        ledger,
        errors,
    )
}
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
#[ignore = "requires actual disposable TiDB; 100 QUIC clients / 10 independent coordinators"]
async fn actual_tidb_100_clients_10_servers_saturation() {
    tokio::time::timeout(Duration::from_secs(1800), packet())
        .await
        .expect("overall saturation deadline exceeded")
        .expect("saturation failed");
}
async fn packet() -> Result<(), String> {
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("MOUNT_RS_TIDB_URL required");
    let depths: Vec<usize> = std::env::var("MOUNT_RS_REMOTE_TIDB_SATURATION_DEPTHS")
        .unwrap_or("1,2,4,8".into())
        .split(',')
        .map(|d| d.parse().expect("depth integer"))
        .collect();
    validate_depths(&depths)?;
    let mut modes = parse_modes(
        &std::env::var("MOUNT_RS_REMOTE_TIDB_SATURATION_MODES")
            .unwrap_or_else(|_| "read,write".into()),
    )?;
    if std::env::var("MOUNT_RS_REMOTE_TIDB_SATURATION_MIXED").as_deref() == Ok("1") {
        modes.push(Mode::Mixed);
    }
    let seconds = env_num("MOUNT_RS_REMOTE_TIDB_SATURATION_SECONDS", 5, 1, 30);
    let warmup = env_num("MOUNT_RS_REMOTE_TIDB_SATURATION_WARMUP_SECONDS", 1, 1, 10);
    let blocks = env_num("MOUNT_RS_REMOTE_TIDB_SATURATION_BLOCKS", 64, 32, 1024);
    assert!(blocks >= *depths.iter().max().unwrap());
    let timeout = Duration::from_secs(env_num(
        "MOUNT_RS_REMOTE_TIDB_SATURATION_REQUEST_TIMEOUT_SECONDS",
        30,
        1,
        60,
    ) as u64);
    let pool = Pool::from_url(&url).map_err(|_| "invalid TiDB URL (redacted)")?;
    let mut conn = pool
        .get_conn()
        .await
        .map_err(|_| "identity connection failed (redacted)")?;
    let (identity, version): (String, String) = conn
        .query_first("SELECT tidb_version(), VERSION()")
        .await
        .map_err(|_| "TiDB identity query failed (redacted)")?
        .ok_or("missing TiDB identity")?;
    if !identity.to_ascii_lowercase().contains("release version:")
        || !version.to_ascii_lowercase().contains("tidb")
    {
        return Err("actual TiDB required".into());
    }
    drop(conn);
    pool.disconnect()
        .await
        .map_err(|_| "identity disconnect failed")?;
    let key = format!(
        "remote-saturation-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let (mut servers, mut endpoints, mut dirs, mut providers) = (vec![], vec![], vec![], vec![]);
    let max_depth = *depths.iter().max().unwrap();
    let mut clients = vec![];
    let mut reports = vec![];
    let mut failed_phase = None;
    let mut namespace_bytes = None;
    let topology = std::env::var("MOUNT_RS_TIDB_TOPOLOGY").ok();
    let mut expected: Vec<Vec<(usize, u64)>> =
        vec![(0..blocks).map(|block| (0, block as u64 + 1)).collect(); CLIENTS];
    let work = tokio::time::timeout(Duration::from_secs(1500), async {
        for i in 0..SERVERS {
            let (fs, m, b) = wire::open(&url, &key, i).await?;
            let left = Arc::new(fs.clone());
            providers.push((fs, m, b));
            let (s, e, d) = wire::setup_with_left(left).await?;
            servers.push(s);
            endpoints.push(e);
            dirs.push(d);
        }

        let mut setup_tasks = tokio::task::JoinSet::new();
        for client in 0..CLIENTS {
            let endpoint = endpoints[client / 10].clone();
            let address = servers[client / 10].local_addr();
            setup_tasks.spawn(async move {
                let connection = wire::connect(&endpoint, address).await?;
                let handle = wire::success(
                    &connection,
                    1,
                    "left",
                    OperationName::Open,
                    json!({"path":format!("/saturation-{client}"),"flags":"w+","mode":420}),
                )
                .await?
                .as_u64()
                .ok_or("invalid handle")?;
                for first in (0..blocks).step_by(256) {
                    let count = (blocks - first).min(256);
                    let data: Vec<u8> = (first..first+count).flat_map(|block| seeded_block(client, block)).collect();
                    let written = wire::success(
                        &connection,
                        2 + first as u64,
                        "left",
                        OperationName::HandleWrite,
                        json!({"handle":handle,"position":first*BYTES,"data":data}),
                    )
                    .await?;
                    if written != json!(count * BYTES) {
                        return Err("short setup write".into());
                    }
                }
                let mut handles = vec![handle];
                for lane in 1..max_depth {
                    let extra = wire::success(
                        &connection,
                        10_000 + lane as u64,
                        "left",
                        OperationName::Open,
                        json!({"path":format!("/saturation-{client}"),"flags":"r+","mode":420}),
                    )
                    .await?
                    .as_u64()
                    .ok_or("invalid lane handle")?;
                    handles.push(extra);
                }
                Ok::<_, String>((
                    client,
                    Client {
                        connection,
                        handles,
                    },
                ))
            });
        }
        let mut prepared = vec![];
        let mut setup_errors = vec![];
        while let Some(result) = setup_tasks.join_next().await {
            match result {
                Ok(Ok(c)) => prepared.push(c),
                Ok(Err(e)) => setup_errors.push(e),
                Err(e) => setup_errors.push(format!("setup task failed: {e}")),
            }
        }
        if !setup_errors.is_empty() {
            return Err(format!("setup failures: count={} samples={:?}",setup_errors.len(),setup_errors.iter().take(8).collect::<Vec<_>>()));
        }
        prepared.sort_by_key(|(client, _)| *client);
        clients.extend(prepared.into_iter().map(|(_, c)| c));
        namespace_bytes = Some(
            tokio::time::timeout(Duration::from_secs(30), probe_namespace_bytes(&url, &key))
                .await
                .map_err(|_| "namespace size probe deadline exceeded")??,
        );
        let mut phase = 0u64;
        for depth in depths {
            for mode in &modes {
                for (measured, duration) in [(false, warmup), (true, seconds)] {
                    phase += 1;
                    let (report, ledger, errors) = stage(
                        &clients,
                        depth,
                        blocks,
                        *mode,
                        duration,
                        phase * 1_000_000_000,
                        timeout,
                        &expected,
                    )
                    .await;
                    for (c, l, b, s) in ledger {
                        expected[c][b] = (l, s);
                    }
                    if !errors.is_empty() {
                        let samples:Vec<&String>=errors.iter().take(8).collect();
                        failed_phase=Some(json!({"report":report,"measured":measured,"failure_count":errors.len(),"timeout_failures":errors.iter().filter(|e|e.contains("request timeout")).count(),"error_samples":samples}));
                        if measured {eprintln!("TIDB_REMOTE_SATURATION {report}");reports.push(report);}
                        return Err(format!("stage failures: count={} samples={samples:?}",errors.len()));
                    }
                    if measured {
                        eprintln!("TIDB_REMOTE_SATURATION {report}");
                        reports.push(report);
                    }
                }
            }
        }
        for c in &clients {
            for (lane, handle) in c.handles.iter().enumerate() {
                wire::success(
                    &c.connection,
                    u64::MAX - 1 - lane as u64,
                    "left",
                    OperationName::HandleClose,
                    json!({"handle":handle}),
                )
                .await?;
            }
        }
        Ok::<(), String>(())
    })
    .await
    .unwrap_or_else(|_| Err("setup/work deadline exceeded".into()));
    // Always shut down all created resources after work failure. No write is retried.
    for e in endpoints {
        e.close(0u32.into(), b"shutdown");
    }
    let cleanup = tokio::time::timeout(Duration::from_secs(60), async {
        let mut shutdown_tasks = tokio::task::JoinSet::new();
        for s in servers {
            shutdown_tasks.spawn(async move {
                s.close().await;
            });
        }
        let mut failures = vec![];
        while let Some(result) = shutdown_tasks.join_next().await {
            if result.is_err() {
                failures.push("server shutdown task failed");
            }
        }

        for (fs, m, b) in providers {
            if fs.shutdown().await.is_err() {
                failures.push("filesystem shutdown failed");
            }
            if m.close().await.is_err() {
                failures.push("metadata close failed");
            }
            if b.close().await.is_err() {
                failures.push("blocks close failed");
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!("shutdown failures: {failures:?}"))
        }
    })
    .await
    .unwrap_or_else(|_| Err("shutdown deadline exceeded".into()));
    drop(dirs);
    let verification_run = work.is_ok() && cleanup.is_ok();
    let verification = if verification_run {
        verify(&url, &key, &expected).await
    } else {
        Ok(())
    };
    let verification_status = if !verification_run {
        "skipped"
    } else if verification.is_ok() {
        "passed"
    } else {
        "failed"
    };
    let artifact = json!({"schema":"mount-rs-tidb-saturation-v1","tidb_identity":identity,"mysql_version":version,"volume_key":key,"dataset_bytes":CLIENTS*blocks*BYTES,"namespace_bytes":namespace_bytes,"topology":topology,"debug_assertions":cfg!(debug_assertions),"build_profile":if cfg!(debug_assertions){"debug"}else{"release"},"warmup_seconds":warmup,"nominal_stage_seconds":seconds,"configured_modes":modes.iter().map(|m|format!("{m:?}")).collect::<Vec<_>>(),"server_active_request_limit_per_connection":32,"audit_logging":"enabled; request audit cost included","latency_histogram":"power-of-two microsecond upper bounds","stages":reports,"failed_phase":failed_phase,"verification_status":verification_status,"verified_files":if verification_status=="passed"{CLIENTS}else{0},"work_error":work.as_ref().err(),"cleanup_error":cleanup.as_ref().err(),"verification_error":verification.as_ref().err()});
    if let Ok(path) = std::env::var("MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT") {
        let path = std::path::PathBuf::from(path);
        let bytes = serde_json::to_vec_pretty(&artifact).unwrap();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("saturation");
        let retained = path.with_file_name(format!("{stem}-{key}.json"));
        std::fs::write(&retained, &bytes)
            .map_err(|e| format!("retained artifact write failed: {e}"))?;
        std::fs::write(path, bytes).map_err(|e| format!("artifact write failed: {e}"))?;
    }
    work?;
    cleanup?;
    verification
}
async fn verify(url: &str, key: &str, expected: &[Vec<(usize, u64)>]) -> Result<(), String> {
    let (fs, m, b) = wire::open(url, key, SERVERS).await?;
    let view = Loopback::new(fs.clone());
    let verified = tokio::time::timeout(Duration::from_secs(120), async {
        let names: BTreeSet<String> = view
            .readdir("/")
            .await
            .map_err(|_| "fresh namespace listing failed")?
            .into_iter()
            .map(|e| e.name)
            .collect();
        let want: BTreeSet<String> = (0..CLIENTS).map(|c| format!("saturation-{c}")).collect();
        if names != want {
            return Err("fresh namespace mismatch".into());
        }
        for (client, blocks) in expected.iter().enumerate() {
            let actual = view
                .read_file(&format!("/saturation-{client}"))
                .await
                .map_err(|_| "fresh read failed")?;
            let want: Vec<u8> = blocks
                .iter()
                .flat_map(|(lane, seq)| payload(client, *lane, *seq))
                .collect();
            if actual != want {
                return Err(format!("fresh file mismatch client {client}"));
            }
        }
        Ok::<(), String>(())
    })
    .await
    .map_err(|_| "verification deadline exceeded");
    let closed = tokio::time::timeout(Duration::from_secs(30), async {
        let mut failures = vec![];
        if fs.shutdown().await.is_err() {
            failures.push("fresh shutdown failed");
        }
        if m.close().await.is_err() {
            failures.push("fresh metadata close failed");
        }
        if b.close().await.is_err() {
            failures.push("fresh blocks close failed");
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(format!("fresh close failures: {failures:?}"))
        }
    })
    .await
    .map_err(|_| "fresh close deadline exceeded")?;
    closed?;
    verified??;
    Ok(())
}

// This diagnostic query and its pool lifecycle complete before any warmup/timed work.
async fn probe_namespace_bytes(url: &str, key: &str) -> Result<u64, String> {
    let pool = Pool::from_url(url).map_err(|_| "invalid namespace probe URL (redacted)")?;
    let queried = async {
        let mut connection = pool
            .get_conn()
            .await
            .map_err(|_| "namespace probe connect failed (redacted)")?;
        let bytes: Option<Option<u64>> = connection
            .exec_first(
                "SELECT OCTET_LENGTH(namespace) FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (key,),
            )
            .await
            .map_err(|_| "namespace size query failed (redacted)")?;
        bytes
            .flatten()
            .ok_or_else(|| "namespace missing after setup".to_owned())
    }
    .await;
    let closed = pool
        .disconnect()
        .await
        .map_err(|_| "namespace probe disconnect failed (redacted)");
    let bytes = queried?;
    closed?;
    Ok(bytes)
}

fn seeded_block(client: usize, block: usize) -> Vec<u8> {
    payload(client, 0, block as u64 + 1)
}
#[test]
fn initial_dataset_has_100_times_64_distinct_blocks() {
    let unique: BTreeSet<Vec<u8>> = (0..CLIENTS)
        .flat_map(|client| (0..64).map(move |block| seeded_block(client, block)))
        .collect();
    assert_eq!(unique.len(), CLIENTS * 64);
}

#[test]
fn mode_selection_is_explicit_and_bounded() {
    assert!(matches!(
        parse_modes("read").unwrap().as_slice(),
        [Mode::Read]
    ));
    assert!(matches!(
        parse_modes("write").unwrap().as_slice(),
        [Mode::Write]
    ));
    assert!(matches!(
        parse_modes("read,write").unwrap().as_slice(),
        [Mode::Read, Mode::Write]
    ));
    for invalid in ["", "unknown", "read,read", "write,write", "read,", "mixed"] {
        assert!(parse_modes(invalid).is_err(), "accepted {invalid}");
    }
}

fn parse_modes(input: &str) -> Result<Vec<Mode>, String> {
    let mut seen = BTreeSet::new();
    let mut modes = vec![];
    for name in input.split(',') {
        if !seen.insert(name) {
            return Err("duplicate saturation mode".into());
        }
        modes.push(match name {
            "read" => Mode::Read,
            "write" => Mode::Write,
            _ => return Err("saturation modes require read or write".into()),
        });
    }
    Ok(modes)
}

fn timeout_message(writing: bool) -> &'static str {
    if writing {
        "write request timeout: write outcome uncertain; never replayed"
    } else {
        "read request timeout; never replayed"
    }
}
