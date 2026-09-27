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
    assert!(valid_read(2).is_err());
    assert!(valid_write(4095).is_err());
    assert_ne!(payload(0, 0, 1), payload(0, 1, 1));
    let mut h = Histogram::default();
    h.record(std::time::Duration::from_micros(100));
    h.record(std::time::Duration::from_micros(200));
    assert_eq!(h.count, 2);
    assert!(h.percentile(99) >= 200);
}

#[test]
fn read_content_accepts_exact_payload() {
    let expected = payload(2, 3, 7);
    assert!(valid_read_content(&expected, &expected).is_ok());
}
#[test]
fn read_content_rejects_invalid_lengths() {
    let expected = payload(2, 3, 7);
    for received in [&[][..], &expected[..BYTES - 1], &vec![0; BYTES + 1][..]] {
        assert!(valid_read_content(received, &expected).is_err());
    }
    assert!(valid_read_content(&expected[..BYTES - 1], &expected[..BYTES - 1]).is_err());
    let oversized = vec![0; BYTES + 1];
    assert!(valid_read_content(&oversized, &oversized).is_err());
}
#[test]
fn read_content_compares_every_byte_and_payload_marker() {
    let expected = payload(2, 3, 7);
    for index in 0..BYTES {
        let mut received = expected.clone();
        received[index] ^= 1;
        assert!(valid_read_content(&received, &expected).is_err());
    }
    for received in [payload(4, 3, 7), payload(2, 4, 7), payload(2, 3, 8)] {
        assert!(valid_read_content(&received, &expected).is_err());
    }
}

#[test]
fn reused_payload_buffer_preserves_original_pattern_and_marker() {
    let mut bytes = vec![0; BYTES];
    for (client, lane, sequence) in [(0, 0, 0), (2, 3, 7), (99, 31, 1_310_000_001)] {
        payload_into(&mut bytes, client, lane, sequence);
        assert_eq!(bytes, payload(client, lane, sequence));
    }
}

#[test]
fn payload_generation_finishes_before_operation_latency_starts() {
    let mut bytes = vec![0; BYTES];
    let mut prepared_at = None;
    let start = start_after_payload(|| {
        payload_into(&mut bytes, 2, 5, 7);
        prepared_at = Some(Instant::now());
    });
    assert_eq!(bytes, payload(2, 5, 7));
    assert!(start >= prepared_at.unwrap());
}

#[cfg(all(feature = "resource-profiling", unix))]
#[path = "support/resource_profile.rs"]
mod resource_profile;
#[path = "support/tidb_wire.rs"]
mod wire;
use mount_rs_core::Loopback;
use mount_rs_remote_protocol::{OperationName, binary::IoRequest};
#[path = "support/saturation_backend.rs"]
mod backend;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
const DEFAULT_CLIENTS: usize = 100;
const DEFAULT_SERVERS: usize = 10;
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
fn valid_read(count: usize) -> Result<(), String> {
    if count == BYTES {
        Ok(())
    } else {
        Err("partial or invalid read".into())
    }
}
fn valid_read_content(received: &[u8], expected: &[u8]) -> Result<(), String> {
    valid_read(received.len())?;
    if expected.len() != BYTES {
        return Err("partial or invalid expected payload".into());
    }
    if received != expected {
        return Err("read content mismatch".into());
    }
    Ok(())
}
fn valid_write(count: usize) -> Result<(), String> {
    if count == BYTES {
        Ok(())
    } else {
        Err("partial or invalid write".into())
    }
}
// Numeric arrays are a diagnostic lane on the current v2 control envelope.
// This mode is test-only and does not add production codec compatibility.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Codec {
    Binary,
    NumericJson,
}
impl Codec {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "binary" => Ok(Self::Binary),
            "numeric-json" => Ok(Self::NumericJson),
            _ => Err("codec requires binary or numeric-json".into()),
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Binary => "binary",
            Self::NumericJson => "numeric-json",
        }
    }
}
enum IoReply {
    Count(usize),
    Numeric(Value),
}
fn valid_numeric_read(received: &Value, expected: &[u8]) -> Result<(), String> {
    let values = received
        .as_array()
        .filter(|v| v.len() == BYTES && expected.len() == BYTES)
        .ok_or("partial or invalid read")?;
    if values
        .iter()
        .zip(expected)
        .all(|(value, expected)| value.as_u64() == Some(u64::from(*expected)))
    {
        Ok(())
    } else {
        Err("read content mismatch".into())
    }
}
fn valid_numeric_write(received: &Value) -> Result<(), String> {
    if received.as_u64() == Some(BYTES as u64) {
        Ok(())
    } else {
        Err("partial or invalid write".into())
    }
}

const RUNNER_SOURCE_PATH: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/quic_tidb_saturation.rs");
const BACKEND_SOURCE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/support/saturation_backend.rs"
);

fn runner_source_digest(runner: &[u8], backend: &[u8]) -> String {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    digest.update(b"mount-rs-saturation-runner-sources-v1\0");
    for (name, bytes) in [
        (b"quic_tidb_saturation.rs".as_slice(), runner),
        (b"support/saturation_backend.rs".as_slice(), backend),
    ] {
        digest.update(&(name.len() as u64).to_be_bytes());
        digest.update(name);
        digest.update(&(bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn require_matching_runner_source(compiled: &str, current: &str) -> Result<(), String> {
    if compiled == current {
        Ok(())
    } else {
        Err("runner source bytes changed since executable compilation".into())
    }
}

fn source_binary_receipt() -> Result<Value, String> {
    use std::io::Read;
    let compiled_source_sha256 = runner_source_digest(
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/quic_tidb_saturation.rs"
        )),
        include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/saturation_backend.rs"
        )),
    );
    let current_source_sha256 = runner_source_digest(
        &std::fs::read(RUNNER_SOURCE_PATH).map_err(|_| "runner source read failed")?,
        &std::fs::read(BACKEND_SOURCE_PATH).map_err(|_| "backend source read failed")?,
    );
    require_matching_runner_source(&compiled_source_sha256, &current_source_sha256)?;
    let executable = std::env::current_exe().map_err(|_| "executable path unavailable")?;
    let mut input = std::fs::File::open(&executable).map_err(|_| "executable read failed")?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|_| "executable digest read failed")?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    let executable_sha256: String = digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let source = std::process::Command::new("git")
        .args(["-C", env!("CARGO_MANIFEST_DIR"), "rev-parse", "HEAD"])
        .output()
        .map_err(|_| "source revision probe failed")?;
    if !source.status.success() {
        return Err("source revision probe failed".into());
    }
    let checkout_revision = String::from_utf8(source.stdout)
        .map_err(|_| "source revision invalid")?
        .trim()
        .to_owned();
    if checkout_revision.len() != 40 {
        return Err("source revision invalid".into());
    }
    let status = std::process::Command::new("git")
        .args(["-C", env!("CARGO_MANIFEST_DIR"), "status", "--porcelain"])
        .output()
        .map_err(|_| "source status probe failed")?;
    if !status.status.success() {
        return Err("source status probe failed".into());
    }
    Ok(json!({
        "executable":executable,
        "executable_sha256":executable_sha256,
        "checkout_revision_at_run":checkout_revision,
        "checkout_dirty_at_run":!status.stdout.is_empty(),
        "compiled_revision_env":option_env!("MOUNT_RS_SATURATION_SOURCE_REVISION"),
        "compiled_runner_source_sha256":compiled_source_sha256,
        "current_runner_source_sha256":current_source_sha256,
        "features":{"resource_profiling":cfg!(feature="resource-profiling"),"allocation_profiling":cfg!(feature="allocation-profiling"),"foundationdb":cfg!(feature="saturation-foundationdb")},
    }))
}

#[test]
fn source_receipt_binds_compiled_and_current_runner_bytes() {
    let receipt = source_binary_receipt().unwrap();
    let compiled = receipt["compiled_runner_source_sha256"].as_str().unwrap();
    let current = receipt["current_runner_source_sha256"].as_str().unwrap();
    assert_eq!(compiled.len(), 64);
    assert_eq!(compiled, current);
    assert!(require_matching_runner_source(compiled, "different").is_err());
}

#[test]
fn artifact_inode_options_match_effective_backend_mode() {
    for (mode, inode, compact) in [
        (backend::InodeMode::Legacy, false, false),
        (backend::InodeMode::Inode, true, false),
        (backend::InodeMode::Compact, true, true),
    ] {
        let fields = json!({
            "requested_inode_mode":mode.label(),
            "inode_updates":mode.inode_updates(),
            "compact_inode_updates":mode.compact_inode_updates(),
        });
        assert_eq!(fields["inode_updates"], inode);
        assert_eq!(fields["compact_inode_updates"], compact);
    }
}

#[test]
fn rejected_mode_receipt_or_owned_count_writes_failed_artifact() {
    let directory = tempfile::tempdir().unwrap();
    for (case, mode_error, count_error) in [
        ("mode", Some("persisted marker mismatch"), None),
        ("count", None, Some("owned SQL count failed")),
    ] {
        let path = directory.path().join(format!("{case}.json"));
        let mut artifact = json!({"schema":"mount-rs-provider-saturation-v2"});
        finalize_artifact_qualification(&mut artifact, 4, "passed", mode_error, count_error);
        write_saturation_artifact(&artifact, Some(&path), case).unwrap();
        for written in [&path, &directory.path().join(format!("{case}-{case}.json"))] {
            let observed: Value = serde_json::from_slice(&std::fs::read(written).unwrap()).unwrap();
            assert_eq!(observed["verification_status"], "failed", "{case}");
            assert_eq!(observed["verified_files"], 0, "{case}");
            assert_eq!(observed["file_verification_status"], "passed", "{case}");
            assert_eq!(observed["persisted_mode_receipt_error"], json!(mode_error));
            assert_eq!(
                observed["owned_logical_sql_counts_error"],
                json!(count_error)
            );
        }
    }
}
#[test]
fn diagnostic_codec_selection_and_numeric_oracles_are_explicit() {
    assert_eq!(Codec::parse("binary").unwrap(), Codec::Binary);
    assert_eq!(Codec::parse("numeric-json").unwrap(), Codec::NumericJson);
    for invalid in ["", "v1", "json", "fallback"] {
        assert!(Codec::parse(invalid).is_err());
    }
    let expected = payload(2, 3, 7);
    assert!(valid_numeric_read(&json!(&expected), &expected).is_ok());
    assert!(valid_numeric_read(&json!(&expected[..BYTES - 1]), &expected).is_err());
    for invalid in [json!(-1), json!(256), json!(1.5), json!("1"), Value::Null] {
        let mut received = json!(&expected);
        received[BYTES - 1] = invalid;
        assert!(valid_numeric_read(&received, &expected).is_err());
    }
    assert!(valid_numeric_write(&json!(BYTES)).is_ok());
    assert!(valid_numeric_write(&json!(BYTES - 1)).is_err());
}

fn payload(client: usize, lane: usize, seq: u64) -> Vec<u8> {
    let mut bytes = vec![0; BYTES];
    let marker = format!("client={client};lane={lane};sequence={seq};");
    fill_payload_pattern(&mut bytes, client, lane, seq);
    bytes[..marker.len()].copy_from_slice(marker.as_bytes());
    bytes
}
fn fill_payload_pattern(bytes: &mut [u8], client: usize, lane: usize, seq: u64) {
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (i as u64 * 31 + seq * 13 + client as u64 * 17 + lane as u64 * 19) as u8;
    }
}
fn payload_into(bytes: &mut [u8], client: usize, lane: usize, seq: u64) {
    use std::io::Write;
    assert_eq!(bytes.len(), BYTES);
    fill_payload_pattern(bytes, client, lane, seq);
    // Three decimal u64-sized identifiers and their labels fit in 128 bytes.
    let mut marker = [0_u8; 128];
    let mut cursor = std::io::Cursor::new(marker.as_mut_slice());
    write!(cursor, "client={client};lane={lane};sequence={seq};")
        .expect("payload marker fits in stack buffer");
    let marker_len = cursor.position() as usize;
    bytes[..marker_len].copy_from_slice(&marker[..marker_len]);
}

fn start_after_payload(prepare: impl FnOnce()) -> Instant {
    prepare();
    Instant::now()
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
    drive: String,
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
    drive: String,
    handle: u64,
    client: usize,
    lane: usize,
    depth: usize,
    blocks: usize,
    mode: Mode,
    codec: Codec,
    deadline: Instant,
    base: u64,
    timeout: Duration,
    mut expected: Vec<(usize, u64)>,
) -> LaneResult {
    let mut result = LaneResult::default();
    let mut seq = 0u64;
    let mut rng = (client as u64 + 1) * 7919 + (lane as u64 + 1) * 104729;
    let positions: Vec<usize> = (lane..blocks).step_by(depth).collect();
    let mut expected_bytes = vec![0; BYTES];
    let mut read_buffer = vec![0; BYTES];
    let mut request = IoRequest {
        drive_id: drive,
        handle,
        position: None,
    };
    while Instant::now() < deadline {
        seq += 1;
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let block = positions[rng as usize % positions.len()];
        let writing =
            matches!(mode, Mode::Write) || matches!(mode, Mode::Mixed) && seq.is_multiple_of(2);
        request.position = Some((block * BYTES) as u64);
        let start = start_after_payload(|| {
            let (payload_lane, payload_sequence) = if writing {
                (lane, base + seq)
            } else {
                expected[block]
            };
            payload_into(&mut expected_bytes, client, payload_lane, payload_sequence);
        });
        let response=tokio::time::timeout(timeout,async {
            match codec {
                Codec::Binary=>{
                    let count=if writing { wire::handle_write(&connection,base+seq,&request,&expected_bytes).await? }
                    else { wire::handle_read(&connection,base+seq,&request,&mut read_buffer).await? };
                    Ok(IoReply::Count(count))
                }
                Codec::NumericJson=>{
                    let (name,body)=if writing { (OperationName::HandleWrite,json!({"handle":handle,"position":request.position,"data":&expected_bytes})) }
                    else { (OperationName::HandleRead,json!({"handle":handle,"position":request.position,"length":BYTES})) };
                    wire::success(&connection,base+seq,&request.drive_id,name,body).await.map(IoReply::Numeric)
                }
            }
        }).await;
        match response {
            Ok(Ok(reply)) => {
                let valid = match (reply, writing) {
                    (IoReply::Count(count), true) => valid_write(count),
                    (IoReply::Count(count), false) => valid_read(count)
                        .and_then(|()| valid_read_content(&read_buffer, &expected_bytes)),
                    (IoReply::Numeric(value), true) => valid_numeric_write(&value),
                    (IoReply::Numeric(value), false) => valid_numeric_read(&value, &expected_bytes),
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
                    // Read content validation remains inside the latency scope.
                    result.read.record(start.elapsed());
                }
            }
            Ok(Err(e)) => {
                connection.close(1_u32.into(), b"I/O failed; never replayed");
                result.errors.push(e);
                break;
            }
            Err(_) => {
                connection.close(1_u32.into(), b"request timeout; never replayed");
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
    server_count: usize,
    active_clients: usize,
    depth: usize,
    blocks: usize,
    mode: Mode,
    codec: Codec,
    seconds: usize,
    base: u64,
    timeout: Duration,
    expected: &[Vec<(usize, u64)>],
) -> (Value, Vec<(usize, usize, usize, u64)>, Vec<String>) {
    #[cfg(all(feature = "resource-profiling", unix))]
    let resources_before =
        resource_profile::Snapshot::capture(clients).expect("process resource profile unavailable");
    let start_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds as u64);
    let mut tasks = tokio::task::JoinSet::new();
    for (client, c) in clients.iter().take(active_clients).enumerate() {
        for lane_id in 0..depth {
            let connection = c.connection.clone();
            let drive = c.drive.clone();
            let handle = c.handles[lane_id];
            let expected = expected[client].clone();
            tasks.spawn(async move {
                (
                    client,
                    lane_id,
                    lane(
                        connection,
                        drive,
                        handle,
                        client,
                        lane_id,
                        depth,
                        blocks,
                        mode,
                        codec,
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
    #[cfg(all(feature = "resource-profiling", unix))]
    let resources = resource_profile::Snapshot::capture(clients)
        .expect("process resource profile unavailable")
        .delta(&resources_before)
        .expect("process resource counters invalid");
    let report = json!({"mode":format!("{mode:?}"),"codec":codec.label(),"nominal_seconds":seconds,"start_unix_ms":start_unix_ms,"finish_unix_ms":SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis(),"clients":clients.len(),"active_clients":active_clients,"servers":server_count,"per_client_depth":depth,"total_queue_depth":active_clients*depth,"elapsed_seconds_including_drain":elapsed,"read":read.json(),"write":write.json(),"read_iops":read.count as f64/elapsed,"write_iops":write.count as f64/elapsed,"total_iops":iops,"payload_mib_per_second":iops*BYTES as f64/1048576.0,"reference_target_iops":100000,"target_attainment":iops/100000.0,"failures":errors.len(),"cache":"cache-warm randomized dataset; no cold-cache claim"});
    #[cfg(all(feature = "resource-profiling", unix))]
    let report = {
        let mut report = report;
        report["resources"] = resources;
        report
    };
    (report, ledger, errors)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 16)]
#[ignore = "requires disposable provider; 100 QUIC clients / 10 independent coordinators"]
async fn actual_tidb_100_clients_10_servers_saturation() {
    tokio::time::timeout(Duration::from_secs(1800), packet())
        .await
        .expect("overall saturation deadline exceeded")
        .expect("saturation failed");
}
async fn stage_observer(
    phase: &str,
    mode: Mode,
    depth: usize,
    id: &str,
    successes: u64,
    failures: usize,
    client_count: usize,
) -> Result<(), String> {
    let Ok(executable) = std::env::var("MOUNT_RS_DATASTORE_STAGE_OBSERVER") else {
        return Ok(());
    };
    if executable.is_empty() {
        return Ok(());
    }
    let args = vec![
        phase.to_owned(),
        format!("{mode:?}").to_lowercase(),
        (client_count * depth).to_string(),
        id.to_owned(),
        successes.to_string(),
        failures.to_string(),
    ];
    tokio::task::spawn_blocking(move || {
        let mut child = std::process::Command::new(executable)
            .args(args)
            .spawn()
            .map_err(|_| "datastore observer start failed")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
        loop {
            if let Some(status) = child
                .try_wait()
                .map_err(|_| "datastore observer wait failed")?
            {
                return if status.success() {
                    Ok(())
                } else {
                    Err("datastore observer failed".to_owned())
                };
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err("datastore observer timeout".to_owned());
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    })
    .await
    .map_err(|_| "datastore observer task failed".to_owned())?
}
async fn packet() -> Result<(), String> {
    let binding = source_binary_receipt()?;
    let client_count = env_num(
        "MOUNT_RS_REMOTE_SATURATION_CLIENTS",
        DEFAULT_CLIENTS,
        1,
        10_000,
    );
    let active_clients = env_num(
        "MOUNT_RS_REMOTE_SATURATION_ACTIVE_CLIENTS",
        client_count,
        1,
        client_count,
    );
    let setup_concurrency = env_num("MOUNT_RS_REMOTE_SATURATION_SETUP_CONCURRENCY", 100, 1, 100);
    let server_count = env_num(
        "MOUNT_RS_REMOTE_SATURATION_SERVERS",
        DEFAULT_SERVERS,
        1,
        client_count,
    );
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
    let codec = Codec::parse(
        &std::env::var("MOUNT_RS_REMOTE_SATURATION_CODEC").unwrap_or_else(|_| "binary".into()),
    )?;
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
    let key = format!(
        "remote-saturation-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let backend = backend::Backend::from_environment(&key).await?;
    let preseed = std::env::var("MOUNT_RS_REMOTE_SATURATION_PRESEED").as_deref() == Ok("1");
    let separate =
        std::env::var("MOUNT_RS_REMOTE_SATURATION_SEPARATE_DRIVES").as_deref() == Ok("1");
    let tidb_pool_max_connections =
        env_num("MOUNT_RS_REMOTE_SATURATION_TIDB_POOL_MAX", 16, 1, 1024);
    let storage_contexts: Vec<_> = (0..server_count)
        .map(|_| {
            mount_rs_sdk::StorageContext::new(tidb_pool_max_connections)
                .map_err(|_| "invalid TiDB pool maximum")
        })
        .collect::<Result<_, _>>()?;
    let mut drive_backends = vec![];
    if separate {
        for i in 0..client_count {
            drive_backends.push(backend.child(i)?);
        }
    } else if preseed {
        backend.preseed_empty_files(active_clients).await?;
    }
    let drive_backends = std::sync::Arc::new(drive_backends);
    let (mut servers, mut endpoints, mut dirs, mut providers) = (vec![], vec![], vec![], vec![]);
    let max_depth = *depths.iter().max().unwrap();
    let mut clients = vec![];
    let mut reports = vec![];
    let mut failed_phase = None;
    let mut namespace_bytes = None;
    let mut setup_seconds = None;
    let mut driver_setup_seconds = None;
    let mut provisioning_seconds = None;
    let provision = separate
        && std::env::var("MOUNT_RS_REMOTE_SATURATION_PROVISION_DRIVES").as_deref() == Ok("1");
    let driver_setup_started = Instant::now();
    let topology = &backend.topology;
    let mut expected: Vec<Vec<(usize, u64)>> =
        vec![(0..blocks).map(|block| (0, block as u64 + 1)).collect(); client_count];
    let work = tokio::time::timeout(Duration::from_secs(1500), async {
        if provision {
            let started = Instant::now();
            for b in drive_backends.iter() {
                let fs = b.open(0).await?;
                fs.shutdown().await.map_err(|_| "drive provisioning shutdown failed")?;
            }
            provisioning_seconds = Some(started.elapsed().as_secs_f64());
        }
        if separate {
            let mut starts = tokio::task::JoinSet::new();
            for (i, context) in storage_contexts.iter().enumerate() {
                let backends = drive_backends.clone();
                let context = context.clone();
                starts.spawn(async move {
                    let mut opened = vec![];
                    let result = async {
                        let mut drivers = vec![];
                        for b in backends.iter() {
                            let fs = b.open_in_context(i, Some(&context)).await?;
                            drivers.push(fs.driver()); opened.push(fs);
                        }
                        wire::setup_with_drives(drivers).await
                    }.await;
                    (i,opened,result)
                });
            }
            let mut prepared = vec![];
            let mut errors = vec![];
            while let Some(result) = starts.join_next().await {
                match result {
                    Ok((i,opened,result)) => {
                        providers.extend(opened);
                        match result {Ok(wire)=>prepared.push((i,wire)),Err(e)=>errors.push(e)}
                    }
                    Err(e)=>errors.push(format!("server setup task failed: {e}")),
                }
            }
            prepared.sort_by_key(|(i,_)|*i);
            for (_, (s,e,d)) in prepared {servers.push(s);endpoints.push(e);dirs.push(d);}
            if !errors.is_empty() {return Err(format!("server setup failures: {errors:?}"));}
        } else {
            for (i, context) in storage_contexts.iter().enumerate() {
                let fs = backend.open_in_context(i, Some(context)).await?;
                let left = fs.driver(); providers.push(fs);
                let (s,e,d) = wire::setup_with_left(left).await?;
                servers.push(s);endpoints.push(e);dirs.push(d);
            }
        }

        driver_setup_seconds = Some(driver_setup_started.elapsed().as_secs_f64());
        let mut setup_tasks = tokio::task::JoinSet::new();
        let setup_slots = std::sync::Arc::new(tokio::sync::Semaphore::new(setup_concurrency));
        let setup_started = Instant::now();
        for client in 0..client_count {
            let endpoint = endpoints[client % server_count].clone();
            let address = servers[client % server_count].local_addr();
            let setup_slots = setup_slots.clone();
            setup_tasks.spawn(async move {
                let _permit = setup_slots.acquire_owned().await.map_err(|_| "setup semaphore closed")?;
                let drive = if separate {format!("sandbox-{client}")} else {"left".into()};
                let connection = if separate {wire::connect_sandbox(&endpoint,address,client).await?} else {wire::connect(&endpoint, address).await?};
                if separate && client_count > 1 {
                    let denied = wire::request(&connection, 900_000, &format!("sandbox-{}", (client+1)%client_count), OperationName::Stat,json!({"path":"/"})).await?;
                    if denied != Err("EACCES".into()) {return Err("cross-sandbox access was not denied".into());}
                }
                if client >= active_clients {
                    if separate { wire::success(&connection, 1, &drive, OperationName::Stat, json!({"path":"/"})).await?; }
                    return Ok((client, Client { drive, connection, handles: vec![] }));
                }
                let handle = wire::success(
                    &connection,
                    1,
                    &drive,
                    OperationName::Open,
                    json!({"path":format!("/saturation-{client}"),"flags":if preseed && !separate {"r+"} else {"w+"},"mode":420}),
                )
                .await?
                .as_u64()
                .ok_or("invalid handle")?;
                for first in (0..blocks).step_by(256) {
                    let count = (blocks - first).min(256);
                    let data: Vec<u8> = (first..first+count).flat_map(|block| seeded_block(client, block)).collect();
                    let written=wire::handle_write(&connection,2+first as u64,&IoRequest{drive_id:drive.clone(),handle,position:Some((first*BYTES) as u64)},&data).await?;
                    if written != count * BYTES {
                        return Err("short setup write".into());
                    }
                }
                let mut handles = vec![handle];
                for lane in 1..max_depth {
                    let extra = wire::success(
                        &connection,
                        10_000 + lane as u64,
                        &drive,
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
                        drive,
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
        setup_seconds = Some(setup_started.elapsed().as_secs_f64());
        namespace_bytes =
            tokio::time::timeout(Duration::from_secs(30), if separate {drive_backends[0].namespace_bytes()} else {backend.namespace_bytes()})
                .await
                .map_err(|_| "namespace size probe deadline exceeded")??;
        let mut phase = 0u64;
        for depth in depths {
            for mode in &modes {
                for (measured, duration) in [(false, warmup), (true, seconds)] {
                    phase += 1;
                    let stage_id = format!("{key}-{phase}");
                    if measured { stage_observer("begin", *mode, depth, &stage_id, 0, 0, active_clients).await?; }
                    let profile_before = mount_rs_core::diagnostics::profile::snapshot();
                    let sqlite_before = if measured && backend.name == "sqlite" && mount_rs_core::diagnostics::profile::enabled() {
                        Some(mount_rs_sqlite::sqlite_io_diagnostics(true))
                    } else { None };
                    let (mut report, ledger, errors) = stage(
                        &clients,
                        server_count,
                        active_clients,
                        depth,
                        blocks,
                        *mode,
                        codec,
                        duration,
                        phase * 1_000_000_000,
                        timeout,
                        &expected,
                    )
                    .await;
                    if measured {
                        let profile_after = mount_rs_core::diagnostics::profile::snapshot();
                        report["io_profile"] = serde_json::to_value(profile_after.delta(&profile_before)?).map_err(|_| "profile encode failed")?;
                        if let Some(before) = sqlite_before {
                            report["sqlite_io_begin"] = before;
                            report["sqlite_io_end"] = mount_rs_sqlite::sqlite_io_diagnostics(false);
                        }
                        stage_observer("end", *mode, depth, &stage_id,
                            report["read"]["completed"].as_u64().unwrap_or(0) + report["write"]["completed"].as_u64().unwrap_or(0), errors.len(), active_clients).await?;
                    }
                    for (c, l, b, s) in ledger {
                        expected[c][b] = (l, s);
                    }
                    if !errors.is_empty() {
                        let samples:Vec<&String>=errors.iter().take(8).collect();
                        failed_phase=Some(json!({"report":report,"measured":measured,"failure_count":errors.len(),"timeout_failures":errors.iter().filter(|e|e.contains("request timeout")).count(),"error_samples":samples}));
                        if measured {eprintln!("REMOTE_PROVIDER_SATURATION {report}");reports.push(report);}
                        return Err(format!("stage failures: count={} samples={samples:?}",errors.len()));
                    }
                    if measured {
                        eprintln!("REMOTE_PROVIDER_SATURATION {report}");
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
                    &c.drive,
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

        for fs in providers {
            if fs.shutdown().await.is_err() {
                failures.push("filesystem shutdown failed");
            }
        }
        for context in &storage_contexts {
            if context.close().await.is_err() {
                failures.push("storage context shutdown failed");
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
        if separate {
            async {
                for (client, b) in drive_backends.iter().take(active_clients).enumerate() {
                    verify_one_separate_drive_with_shutdown(
                        b,
                        server_count,
                        client,
                        &expected[client],
                        |fs| async move {
                            fs.shutdown()
                                .await
                                .map_err(|_| "separate fresh shutdown failed".into())
                        },
                    )
                    .await?;
                }
                Ok(())
            }
            .await
        } else {
            verify(&backend, server_count, &expected[..active_clients]).await
        }
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
    let snapshot_verification =
        std::env::var("MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY").as_deref() == Ok("1");
    let mut artifact = json!({"separate_drives":separate,"drive_count":if separate {client_count} else {1},"driver_replicas":if separate {client_count*server_count} else {server_count},"verification_method":if separate && snapshot_verification {"all stored and fresh driver files"} else if separate {"all fresh driver files"} else if snapshot_verification {"all stored files plus fresh driver sample"} else {"all fresh driver files"},"fresh_driver_sample_limit":if separate {active_clients} else if snapshot_verification {64} else {active_clients},"schema":"mount-rs-provider-saturation-v2","inode_updates":backend.inode_mode.inode_updates(),"compact_inode_updates":backend.inode_mode.compact_inode_updates(),"provider":backend.name,"provider_identity":backend.identity,"provider_version":backend.version,"volume_key":key,"clients":client_count,"active_clients":active_clients,"servers":server_count,"offline_empty_file_preseed":preseed,"setup_concurrency":setup_concurrency,"setup_seconds":setup_seconds,"driver_setup_seconds":driver_setup_seconds,"parallel_server_startup":separate,"drives_provisioned_before_startup":provision,"provisioning_seconds":provisioning_seconds,"dataset_bytes":active_clients*blocks*BYTES,"namespace_bytes":namespace_bytes,"topology":topology,"debug_assertions":cfg!(debug_assertions),"build_profile":if cfg!(debug_assertions){"debug"}else{"release"},"warmup_seconds":warmup,"nominal_stage_seconds":seconds,"configured_modes":modes.iter().map(|m|format!("{m:?}")).collect::<Vec<_>>(),"audit_logging":"enabled; request audit cost included","latency_histogram":"power-of-two microsecond upper bounds","stages":reports,"failed_phase":failed_phase,"verification_status":verification_status,"verified_files":if verification_status=="passed"{active_clients}else{0},"work_error":work.as_ref().err(),"cleanup_error":cleanup.as_ref().err(),"verification_error":verification.as_ref().err()});
    artifact["requested_inode_mode"] = json!(backend.inode_mode.label());
    artifact["inode_mode_selector_env"] = json!({
        "MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES":std::env::var("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES").ok(),
        "MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES":std::env::var("MOUNT_RS_REMOTE_SATURATION_COMPACT_INODE_UPDATES").ok(),
    });
    artifact["source_binary_binding"] = binding;
    let mut mode_receipt_error = None;
    if verification_run && (backend.name == "tidb" || backend.name == "sqlite") {
        let mut receipts = vec![];
        if separate {
            for (index, owned) in drive_backends.iter().enumerate() {
                match owned.persisted_mode_receipt().await {
                    Ok(receipt) => receipts.push(json!({"drive_index":index,"mode":receipt})),
                    Err(error) => {
                        mode_receipt_error = Some(error);
                        break;
                    }
                }
            }
        } else {
            match backend.persisted_mode_receipt().await {
                Ok(receipt) => receipts.push(json!({"drive_index":0,"mode":receipt})),
                Err(error) => mode_receipt_error = Some(error),
            }
        }
        artifact["persisted_mode_receipts"] = json!(receipts);
    }
    if let Some(error) = &mode_receipt_error {
        artifact["persisted_mode_receipt_error"] = json!(error);
    }
    #[cfg(all(feature = "resource-profiling", unix))]
    let mut owned_count_error = None;
    #[cfg(all(feature = "resource-profiling", unix))]
    if verification_run && backend.name == "tidb" {
        let mut counts = vec![];
        if separate {
            for (index, owned) in drive_backends.iter().enumerate() {
                match owned.owned_counts().await {
                    Ok(value) => counts.push(json!({"drive_index":index,"counts":value})),
                    Err(error) => {
                        owned_count_error = Some(error);
                        break;
                    }
                }
            }
        } else {
            match backend.owned_counts().await {
                Ok(value) => counts.push(json!({"drive_index":0,"counts":value})),
                Err(error) => owned_count_error = Some(error),
            }
        }
        artifact["owned_logical_sql_counts"] = json!(counts);
        if let Some(error) = &owned_count_error {
            artifact["owned_logical_sql_counts_error"] = json!(error);
        }
    }
    artifact["wire_protocol_version"] = json!(2);
    artifact["wire_io"] = json!(codec.label());
    artifact["read_buffers"] = json!("binary lane reuses per-lane buffer");
    artifact["codec_selection_env"] = json!("MOUNT_RS_REMOTE_SATURATION_CODEC");
    artifact["numeric_diagnostic_scope"] =
        json!("current v2 control envelope; no compatibility fallback");
    artifact["encoding_timing"] = json!(
        "wire request construction and serialization, response validation, and read content comparison included; payload generation excluded before both read and write latency timestamps"
    );
    artifact["tidb_pool"] = json!({"scope":"per server per exact connection identity", "max_connections":tidb_pool_max_connections,"schema_initialization":"once per context and role"});
    let limits = mount_rs_service::server::RemoteTransferLimits::default();
    artifact["server_admission"] = json!({"scope":"per server, shared across all connections","active_data_operations":limits.active_data_operations,"active_control_operations":limits.active_control_operations,"data_bytes_each_direction":limits.data_bytes,"reserved_control_bytes_each_direction":limits.control_bytes,"quic_bidi_streams_per_connection":40});
    #[cfg(all(feature = "resource-profiling", unix))]
    let owned_count_error_ref = owned_count_error.as_deref();
    #[cfg(not(all(feature = "resource-profiling", unix)))]
    let owned_count_error_ref = None;
    finalize_artifact_qualification(
        &mut artifact,
        active_clients,
        verification_status,
        mode_receipt_error.as_deref(),
        owned_count_error_ref,
    );
    let output =
        std::env::var_os("MOUNT_RS_REMOTE_TIDB_SATURATION_OUTPUT").map(std::path::PathBuf::from);
    write_saturation_artifact(&artifact, output.as_deref(), &key)?;
    work?;
    cleanup?;
    if let Some(error) = mode_receipt_error {
        return Err(error);
    }
    #[cfg(all(feature = "resource-profiling", unix))]
    if let Some(error) = owned_count_error {
        return Err(error);
    }
    verification
}

async fn verify_one_separate_drive_with_shutdown<F, Fut>(
    backend: &backend::Backend,
    server_count: usize,
    client: usize,
    expected: &[(usize, u64)],
    shutdown: F,
) -> Result<(), String>
where
    F: FnOnce(mount_rs_sdk::Filesystem) -> Fut,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let fs = backend.open(server_count).await?;
    let view = Loopback::from_arc(fs.driver());
    let verified = async {
        let actual = view
            .read_file(&format!("/saturation-{client}"))
            .await
            .map_err(|_| "separate fresh read failed")?;
        let want: Vec<u8> = expected
            .iter()
            .flat_map(|(lane, seq)| payload(client, *lane, *seq))
            .collect();
        if actual != want {
            return Err("separate drive content mismatch".into());
        }
        if std::env::var("MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY").as_deref() == Ok("1") {
            backend
                .verify_stored_files_from(client, &[expected.to_vec()])
                .await?;
        }
        Ok(())
    }
    .await;
    let closed = shutdown(fs).await;
    closed?;
    verified
}

fn finalize_artifact_qualification(
    artifact: &mut Value,
    active_clients: usize,
    file_verification_status: &str,
    mode_receipt_error: Option<&str>,
    owned_count_error: Option<&str>,
) {
    artifact["file_verification_status"] = json!(file_verification_status);
    let qualification_status = if mode_receipt_error.is_some()
        || owned_count_error.is_some()
        || !artifact["work_error"].is_null()
        || !artifact["cleanup_error"].is_null()
        || !artifact["verification_error"].is_null()
    {
        "failed"
    } else {
        file_verification_status
    };
    artifact["verification_status"] = json!(qualification_status);
    artifact["verified_files"] = json!(if qualification_status == "passed" {
        active_clients
    } else {
        0
    });
    if let Some(error) = mode_receipt_error {
        artifact["persisted_mode_receipt_error"] = json!(error);
    }
    if let Some(error) = owned_count_error {
        artifact["owned_logical_sql_counts_error"] = json!(error);
    }
}

fn write_saturation_artifact(
    artifact: &Value,
    path: Option<&std::path::Path>,
    key: &str,
) -> Result<(), String> {
    if let Some(path) = path {
        let bytes = serde_json::to_vec_pretty(artifact).unwrap();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("saturation");
        let retained = path.with_file_name(format!("{stem}-{key}.json"));
        std::fs::write(&retained, &bytes)
            .map_err(|e| format!("retained artifact write failed: {e}"))?;
        std::fs::write(path, bytes).map_err(|e| format!("artifact write failed: {e}"))?;
    }
    Ok(())
}

async fn verify(
    backend: &backend::Backend,
    server_count: usize,
    expected: &[Vec<(usize, u64)>],
) -> Result<(), String> {
    let fs = backend.open(server_count).await?;
    let view = Loopback::from_arc(fs.driver());
    let snapshot_verification =
        std::env::var("MOUNT_RS_REMOTE_SATURATION_SNAPSHOT_VERIFY").as_deref() == Ok("1");
    let verify_seconds = env_num("MOUNT_RS_REMOTE_SATURATION_VERIFY_SECONDS", 120, 1, 1200);
    let verified = tokio::time::timeout(Duration::from_secs(verify_seconds as u64), async {
        let names: BTreeSet<String> = view
            .readdir("/")
            .await
            .map_err(|_| "fresh namespace listing failed")?
            .into_iter()
            .map(|e| e.name)
            .collect();
        let want: BTreeSet<String> = (0..expected.len())
            .map(|c| format!("saturation-{c}"))
            .collect();
        if names != want {
            return Err("fresh namespace mismatch".into());
        }
        if snapshot_verification {
            backend.verify_stored_files(expected).await?;
        }
        for (client, blocks) in expected.iter().enumerate() {
            if snapshot_verification && client % expected.len().div_ceil(64) != 0 {
                continue;
            }
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

fn seeded_block(client: usize, block: usize) -> Vec<u8> {
    payload(client, 0, block as u64 + 1)
}
#[test]
fn initial_dataset_has_100_times_64_distinct_blocks() {
    let unique: BTreeSet<Vec<u8>> = (0..DEFAULT_CLIENTS)
        .flat_map(|client| (0..64).map(move |block| seeded_block(client, block)))
        .collect();
    assert_eq!(unique.len(), DEFAULT_CLIENTS * 64);
}

#[tokio::test]
#[ignore = "requires owned TiDB and inode mode"]
async fn snapshot_oracle_rejects_incorrect_byte_ledger() {
    assert_eq!(
        std::env::var("MOUNT_RS_REMOTE_SATURATION_INODE_UPDATES").as_deref(),
        Ok("1")
    );
    let key = format!(
        "snapshot-oracle-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let backend = backend::Backend::from_environment(&key).await.unwrap();
    backend.preseed_empty_files(1).await.unwrap();
    let fs = backend.open(0).await.unwrap();
    Loopback::from_arc(fs.driver())
        .write_file("/saturation-0", &payload(0, 0, 1))
        .await
        .unwrap();
    fs.shutdown().await.unwrap();
    backend.verify_stored_files(&[vec![(0, 1)]]).await.unwrap();
    let error = backend
        .verify_stored_files(&[vec![(0, 2)]])
        .await
        .unwrap_err();
    assert!(error.contains("stored file mismatch"));
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

#[path = "support/production_scale.rs"]
mod production_scale;
