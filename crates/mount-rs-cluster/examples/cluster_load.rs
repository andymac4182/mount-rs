//! Configurable server-process, unique-volume-per-client load audit. Requires disposable Redis.
#[path = "support/direct_ingress.rs"]
mod direct_ingress;
#[path = "support/forward_samples.rs"]
mod forward_samples;
#[path = "support/ingress_admission.rs"]
mod ingress_admission;
#[path = "support/request_detail.rs"]
mod request_detail;
#[path = "support/retry_events.rs"]
mod retry_events;

use mount_rs_cluster::{transport::*, *};
use mount_rs_core::FsDriver;
use mount_rs_http::{HttpServer, HttpServerOptions};
use mount_rs_sdk::{
    AtimePolicy, BlockCacheOptions, Filesystem, SplitOptions, SplitRuntimeOptions, StoreConfig,
    TidbStorePool,
};
use rcgen::{
    BasicConstraints, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair, KeyUsagePurpose,
    SanType,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap, io::Write, net::SocketAddr, path::Path, process::Stdio, sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::{Instant, timeout},
};
use tokio_util::sync::CancellationToken;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

// Diagnostic only: timer wake lateness includes timer granularity and OS scheduling.
// It cannot be subtracted from request latency or identify a particular task.
async fn scheduler_probe(stop: CancellationToken) -> Value {
    let period = Duration::from_millis(2);
    let mut samples = 0u64;
    let mut over_1ms = 0u64;
    let mut over_5ms = 0u64;
    let mut over_10ms = 0u64;
    let mut maximum = Duration::ZERO;
    loop {
        let deadline = Instant::now() + period;
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tokio::time::sleep_until(deadline) => {}
        }
        let late = Instant::now().saturating_duration_since(deadline);
        samples += 1;
        over_1ms += u64::from(late >= Duration::from_millis(1));
        over_5ms += u64::from(late >= Duration::from_millis(5));
        over_10ms += u64::from(late >= Duration::from_millis(10));
        maximum = maximum.max(late);
    }
    json!({"period_ms":2,"samples":samples,"lateness_at_least_1ms":over_1ms,
        "lateness_at_least_5ms":over_5ms,"lateness_at_least_10ms":over_10ms,
        "max_lateness_ms":maximum.as_secs_f64()*1000.0})
}

fn audit_disk_cache() -> Result<bool> {
    match std::env::var("AUDIT_DISK_CACHE").as_deref() {
        Ok("1") => Ok(true),
        Ok("0") | Err(_) => Ok(false),
        _ => Err("AUDIT_DISK_CACHE must be 0 or 1".into()),
    }
}

fn audit_disk_workers() -> Result<usize> {
    let value = std::env::var("AUDIT_DISK_WORKERS").unwrap_or_else(|_| "4".into());
    parse_disk_workers(&value)
}

fn parse_disk_workers(value: &str) -> Result<usize> {
    let workers = value.parse::<usize>()?;
    if !(1..=64).contains(&workers) {
        return Err("AUDIT_DISK_WORKERS must be 1..64".into());
    }
    Ok(workers)
}

fn audit_tidb_pool_limit() -> Result<Option<usize>> {
    match std::env::var("AUDIT_TIDB_SHARED_POOL") {
        Err(_) => Ok(None),
        Ok(value) => {
            let limit = value.parse::<usize>()?;
            if !(1..=100).contains(&limit) {
                return Err("AUDIT_TIDB_SHARED_POOL must be 1..100".into());
            }
            Ok(Some(limit))
        }
    }
}

fn audit_read_parallelism() -> Result<usize> {
    let value = std::env::var("AUDIT_READ_PARALLELISM")
        .unwrap_or_else(|_| "1".into())
        .parse()?;
    if !(1..=4).contains(&value) {
        return Err("AUDIT_READ_PARALLELISM must be 1..4".into());
    }
    Ok(value)
}

fn audit_http2() -> Result<bool> {
    match std::env::var("AUDIT_HTTP2").as_deref() {
        Ok("1") => Ok(true),
        Ok("0") | Err(_) => Ok(false),
        Ok(_) => Err("AUDIT_HTTP2 must be 0 or 1".into()),
    }
}
fn audit_http_client() -> Result<reqwest::Client> {
    let builder = reqwest::Client::builder().timeout(Duration::from_secs(30));
    Ok(if audit_http2()? {
        builder.http2_prior_knowledge()
    } else {
        builder
    }
    .build()?)
}

fn audit_tcp_nodelay() -> Result<bool> {
    match std::env::var("AUDIT_TCP_NODELAY").as_deref() {
        Ok("1") => Ok(true),
        Ok("0") => Ok(false),
        Err(_) => Ok(HttpServerOptions::default().tcp_nodelay),
        _ => Err("AUDIT_TCP_NODELAY must be 0 or 1".into()),
    }
}

fn audit_shared_r2_client() -> Result<bool> {
    match std::env::var("AUDIT_SHARED_R2_CLIENT").as_deref() {
        Err(_) | Ok("0") => Ok(false),
        Ok("1") if std::env::var("AUDIT_BLOCKS").as_deref() == Ok("rustfs") => Ok(true),
        _ => Err("AUDIT_SHARED_R2_CLIENT must be 0, or 1 with RustFS blocks".into()),
    }
}

fn audit_directory_entries() -> Result<usize> {
    let entries = std::env::var("AUDIT_DIRECTORY_ENTRIES")
        .unwrap_or_else(|_| "0".into())
        .parse()?;
    if entries > 10000 {
        return Err("AUDIT_DIRECTORY_ENTRIES must be 0..10000".into());
    }
    Ok(entries)
}

fn audit_storage_chunk_bytes() -> Result<usize> {
    let bytes = std::env::var("AUDIT_STORAGE_CHUNK_BYTES")
        .unwrap_or_else(|_| "65536".into())
        .parse()?;
    if !(1024..=1048576).contains(&bytes) {
        return Err("AUDIT_STORAGE_CHUNK_BYTES must be 1024..1048576".into());
    }
    Ok(bytes)
}

fn audit_read_chunk_bytes() -> Result<usize> {
    let size = std::env::var("AUDIT_READ_CHUNK_BYTES")
        .unwrap_or_else(|_| "65536".into())
        .parse()?;
    if !(1024..=1048576).contains(&size) {
        return Err("AUDIT_READ_CHUNK_BYTES must be 1024..1048576".into());
    }
    Ok(size)
}

fn transport_error(error: reqwest::Error) -> &'static str {
    // Classify standard transport causes without exporting URLs, headers or
    // backend-provided error strings. These labels never change retry policy.
    let mut hyper_category = None;
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            match io.kind() {
                std::io::ErrorKind::UnexpectedEof => return "transport unexpected EOF",
                std::io::ErrorKind::ConnectionReset => return "transport connection reset",
                std::io::ErrorKind::BrokenPipe => return "transport broken pipe",
                _ => {}
            }
        }
        if let Some(http) = cause.downcast_ref::<hyper::Error>() {
            hyper_category = Some(if http.is_incomplete_message() {
                "transport HTTP incomplete message"
            } else if http.is_closed() {
                "transport HTTP closed"
            } else if http.is_canceled() {
                "transport HTTP canceled"
            } else if http.is_parse() {
                "transport HTTP parse"
            } else if http.is_body_write_aborted() {
                "transport HTTP body write aborted"
            } else {
                "transport HTTP other"
            });
        }
        source = cause.source();
    }
    if error.is_timeout() {
        "transport timeout"
    } else if let Some(category) = hyper_category {
        category
    } else if error.is_connect() {
        "transport connect"
    } else if error.is_body() {
        "transport body"
    } else if error.is_decode() {
        "transport decode"
    } else {
        "transport request"
    }
}

fn audit_local_write_limit() -> Result<Option<usize>> {
    match std::env::var("AUDIT_LOCAL_WRITE_LIMIT") {
        Ok(value) if !value.is_empty() => {
            let limit: usize = value.parse()?;
            if !(1..=1024).contains(&limit) {
                return Err("AUDIT_LOCAL_WRITE_LIMIT must be 1..1024".into());
            }
            if std::env::var("AUDIT_BLOCKS").as_deref() != Ok("local-fsync") {
                return Err("AUDIT_LOCAL_WRITE_LIMIT requires local-fsync blocks".into());
            }
            Ok(Some(limit))
        }
        _ => Ok(None),
    }
}

#[derive(Clone, Copy)]
enum ReadOperation {
    File,
    Range { bytes: usize, offset: usize },
    Stat,
    StatJson,
    StatFs,
    SymlinkStat,
    Lstat,
    Readlink,
    Readdir { extra_entries: usize, gzip: bool },
}
fn validate_directory_entries(
    bytes: &[u8],
    extra_entries: usize,
) -> std::result::Result<(), &'static str> {
    let entries: Vec<mount_rs_sdk::DirEntry> =
        serde_json::from_slice(bytes).map_err(|_| "directory JSON")?;
    let names: BTreeMap<_, _> = entries
        .iter()
        .map(|entry| (entry.name.as_str(), entry))
        .collect();
    if names.len() != entries.len() || entries.iter().any(|entry| entry.parent_path != "/") {
        return Err("directory identity mismatch");
    }
    // Unique canonical indices prove every expected fixture is present without
    // formatting and allocating an expected String for every entry/read.
    let matching_fixtures = names
        .iter()
        .filter(|(name, entry)| {
            let Some(digits) = name.strip_prefix("fixture-") else {
                return false;
            };
            if digits.len() != 5 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
                return false;
            }
            let index = digits.bytes().fold(0usize, |value, digit| {
                value * 10 + usize::from(digit - b'0')
            });
            index < extra_entries && entry.is_directory()
        })
        .count();
    if !names.get("data").is_some_and(|entry| entry.is_file()) || matching_fixtures != extra_entries
    {
        return Err("directory entry missing or wrong type");
    }
    Ok(())
}

struct ReadMeasurement {
    session_transport_ms: Option<(f64, f64)>,
    session_ingress_ms: Option<f64>,
    wire_bytes: usize,
    body_complete_ms: f64,
    decode_ms: f64,
    validation_ms: f64,
}

impl ReadOperation {
    fn name(self) -> &'static str {
        match self {
            Self::File => "file-read",
            Self::Range {
                bytes: 4096,
                offset: 0,
            } => "file-range-first-4096",
            Self::Range { offset: 0, .. } => "file-range-prefix",
            Self::Range { .. } => "file-range-window",
            Self::Stat => "metadata-stat",
            Self::StatJson => "metadata-stat-json",
            Self::StatFs => "metadata-statfs",
            Self::SymlinkStat => "metadata-stat-symlink",
            Self::Lstat => "metadata-lstat-symlink",
            Self::Readlink => "metadata-readlink",
            Self::Readdir { .. } => "metadata-readdir",
        }
    }
    async fn execute(
        self,
        client: &reqwest::Client,
        file_url: &str,
        bearer: &str,
        expected: &[u8],
    ) -> std::result::Result<usize, &'static str> {
        self.execute_detailed(client, file_url, bearer, expected)
            .await
            .map(|measurement| measurement.wire_bytes)
    }

    async fn execute_detailed(
        self,
        client: &reqwest::Client,
        file_url: &str,
        bearer: &str,
        expected: &[u8],
    ) -> std::result::Result<ReadMeasurement, &'static str> {
        let started = Instant::now();
        let request = match self {
            Self::File => client.get(file_url),
            Self::Range { bytes, offset } => client.get(file_url).header(
                reqwest::header::RANGE,
                format!("bytes={offset}-{}", offset + bytes - 1),
            ),
            Self::Stat => client.head(file_url),
            Self::StatJson | Self::StatFs => client.get(format!(
                "{}/{}/data",
                file_url
                    .strip_suffix("/fs/data")
                    .ok_or("invalid fixture URL")?,
                if matches!(self, Self::StatJson) {
                    "stat"
                } else {
                    "statfs"
                }
            )),
            Self::SymlinkStat | Self::Lstat | Self::Readlink => client.get(format!(
                "{}/{}/audit-link",
                file_url
                    .strip_suffix("/fs/data")
                    .ok_or("invalid fixture URL")?,
                match self {
                    Self::SymlinkStat => "stat",
                    Self::Lstat => "lstat",
                    _ => "readlink",
                }
            )),
            Self::Readdir { gzip, .. } => client
                .get(
                    file_url
                        .strip_suffix("/fs/data")
                        .ok_or("invalid fixture URL")?
                        .to_owned()
                        + "/entries/",
                )
                .header(
                    reqwest::header::ACCEPT_ENCODING,
                    if gzip { "gzip" } else { "identity" },
                ),
        };
        let response = request
            .bearer_auth(bearer)
            .send()
            .await
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err(match response.status().as_u16() {
                400 => "read HTTP 400",
                401 => "read HTTP 401",
                403 => "read HTTP 403",
                404 => "read HTTP 404",
                409 => "read HTTP 409",
                500 => "read HTTP 500",
                502 => "read HTTP 502",
                503 => "read HTTP 503",
                _ => "read HTTP error",
            });
        }
        if matches!(self, Self::Stat)
            && response
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<usize>().ok())
                != Some(expected.len())
        {
            return Err("stat length mismatch");
        }
        if let Self::Range { bytes, offset } = self {
            let length = expected.len().saturating_sub(offset).min(bytes);
            let content_range = format!(
                "bytes {offset}-{}/{}",
                offset + length.saturating_sub(1),
                expected.len()
            );
            if response.status() != reqwest::StatusCode::PARTIAL_CONTENT
                || response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|value| value.to_str().ok())
                    != Some(content_range.as_str())
            {
                return Err("range status or bounds mismatch");
            }
        }
        let compressed = response
            .headers()
            .get(reqwest::header::CONTENT_ENCODING)
            .is_some_and(|value| value == "gzip");
        let bytes = response.bytes().await.map_err(transport_error)?;
        let body_complete_ms = started.elapsed().as_secs_f64() * 1000.0;
        let wire_bytes = bytes.len();
        let decode_started = Instant::now();
        let bytes = if compressed {
            use std::io::Read;
            if !matches!(self, Self::Readdir { gzip: true, .. }) {
                return Err("unexpected compressed response");
            }
            let mut decoded = Vec::new();
            flate2::read::GzDecoder::new(bytes.as_ref())
                .take(16 * 1024 * 1024 + 1)
                .read_to_end(&mut decoded)
                .map_err(|_| "gzip response decode")?;
            if decoded.len() > 16 * 1024 * 1024 {
                return Err("decoded response too large");
            }
            bytes::Bytes::from(decoded)
        } else {
            bytes
        };
        let decode_ms = decode_started.elapsed().as_secs_f64() * 1000.0;
        let validation_started = Instant::now();
        let result = match self {
            Self::File if bytes.as_ref() != expected => Err("read content mismatch"),
            Self::Range {
                bytes: count,
                offset,
            } if bytes.as_ref() != &expected[offset..expected.len().min(offset + count)] => {
                Err("range content mismatch")
            }
            Self::Stat if !bytes.is_empty() => Err("stat unexpectedly returned file bytes"),
            Self::StatJson | Self::SymlinkStat => {
                let stats: mount_rs_sdk::Stats =
                    serde_json::from_slice(&bytes).map_err(|_| "stat JSON")?;
                if stats.is_file() && stats.size == expected.len() as u64 {
                    Ok(bytes.len())
                } else {
                    Err("stat metadata mismatch")
                }
            }
            Self::Lstat => {
                let stats: mount_rs_sdk::Stats =
                    serde_json::from_slice(&bytes).map_err(|_| "lstat JSON")?;
                if stats.is_symbolic_link() {
                    Ok(bytes.len())
                } else {
                    Err("lstat type mismatch")
                }
            }
            Self::Readlink => {
                let target: String = serde_json::from_slice(&bytes).map_err(|_| "readlink JSON")?;
                if target == "/data" {
                    Ok(bytes.len())
                } else {
                    Err("readlink target mismatch")
                }
            }
            Self::StatFs => {
                let stats: mount_rs_sdk::StatsFs =
                    serde_json::from_slice(&bytes).map_err(|_| "statfs JSON")?;
                if stats.block_size > 0 && stats.files > 0 {
                    Ok(bytes.len())
                } else {
                    Err("statfs metadata mismatch")
                }
            }
            Self::Readdir { extra_entries, .. } => {
                validate_directory_entries(&bytes, extra_entries)?;
                Ok(wire_bytes)
            }
            _ => Ok(bytes.len()),
        };
        result.map(|wire_bytes| ReadMeasurement {
            session_transport_ms: None,
            session_ingress_ms: None,
            wire_bytes,
            body_complete_ms,
            decode_ms,
            validation_ms: validation_started.elapsed().as_secs_f64() * 1000.0,
        })
    }
}

#[derive(Clone, Copy)]
enum WriteOperation {
    Chmod,
    File,
    RenameRoundtrip,
}
impl WriteOperation {
    fn name(self) -> &'static str {
        match self {
            Self::Chmod => "metadata-chmod-one-inode-publication",
            Self::File => "file-put-4096-bytes-including-metadata",
            Self::RenameRoundtrip => "metadata-rename-roundtrip-two-publications",
        }
    }
    async fn execute(
        self,
        client: &reqwest::Client,
        volume_url: &str,
        bearer: &str,
        iteration: usize,
        session: Option<&mount_rs_remote::RemoteFs>,
    ) -> std::result::Result<(), &'static str> {
        match self {
            Self::Chmod => {
                session
                    .ok_or("chmod requires session RPC")?
                    .chmod(
                        "/data",
                        if iteration.is_multiple_of(2) {
                            0o600
                        } else {
                            0o640
                        },
                    )
                    .await
                    .map_err(|_| "chmod RPC failed")?;
            }
            Self::File => {
                let response = client
                    .put(format!("{volume_url}/fs/scratch"))
                    .bearer_auth(bearer)
                    .body(vec![(iteration % 251) as u8; 4096])
                    .send()
                    .await
                    .map_err(transport_error)?;
                if !response.status().is_success() {
                    return Err(ingress_admission::response_error(
                        response.status(),
                        response.headers(),
                        "write status",
                    ));
                }
                response.bytes().await.map_err(transport_error)?;
            }
            Self::RenameRoundtrip => {
                for (from, to) in [("/rename-a", "/rename-b"), ("/rename-b", "/rename-a")] {
                    let response = client
                        .post(format!("{volume_url}/ops/rename"))
                        .bearer_auth(bearer)
                        .header(reqwest::header::CONTENT_TYPE, "application/json")
                        .body(json!({"from":from,"to":to}).to_string())
                        .send()
                        .await
                        .map_err(transport_error)?;
                    if !response.status().is_success() {
                        return Err(ingress_admission::response_error(
                            response.status(),
                            response.headers(),
                            "rename status",
                        ));
                    }
                    let body = response.bytes().await.map_err(transport_error)?;
                    let value: Value = serde_json::from_slice(&body).map_err(|_| "rename JSON")?;
                    if value["ok"] != true {
                        return Err("rename result");
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Default, Serialize, Deserialize)]
struct CacheTotals {
    hits: u64,
    misses: u64,
    evictions: u64,
    resident_bytes: u64,
    resident_entries: u64,
    backing_gets_completed: u64,
    backing_gets_failed: u64,
    backing_read_bytes: u64,
    backing_get_nanoseconds: u64,
    disk_lock_sections: u64,
    disk_lock_wait_nanoseconds: u64,
    disk_lock_hold_nanoseconds: u64,
    disk_lock_wait_max_nanoseconds: u64,
    disk_lock_hold_max_nanoseconds: u64,
    disk_read_probes: u64,
    disk_read_queue_nanoseconds: u64,
    disk_read_service_nanoseconds: u64,
    disk_fill_attempts: u64,
    disk_fill_wait_and_copy_nanoseconds: u64,
    disk_fill_service_nanoseconds: u64,
    disk_backing_get_nanoseconds: u64,
    disk_hits: u64,
    disk_misses: u64,
    disk_corruptions: u64,
    disk_io_failures: u64,
    disk_evictions: u64,
    disk_rejections: u64,
    disk_resident_bytes: u64,
    disk_disabled: u64,
}
impl CacheTotals {
    fn add(&mut self, other: &Self) {
        self.disk_lock_sections = self
            .disk_lock_sections
            .saturating_add(other.disk_lock_sections);
        self.disk_lock_wait_nanoseconds = self
            .disk_lock_wait_nanoseconds
            .saturating_add(other.disk_lock_wait_nanoseconds);
        self.disk_lock_hold_nanoseconds = self
            .disk_lock_hold_nanoseconds
            .saturating_add(other.disk_lock_hold_nanoseconds);
        self.disk_lock_wait_max_nanoseconds = self
            .disk_lock_wait_max_nanoseconds
            .max(other.disk_lock_wait_max_nanoseconds);
        self.disk_lock_hold_max_nanoseconds = self
            .disk_lock_hold_max_nanoseconds
            .max(other.disk_lock_hold_max_nanoseconds);

        self.disk_read_probes = self.disk_read_probes.saturating_add(other.disk_read_probes);
        self.disk_read_queue_nanoseconds = self
            .disk_read_queue_nanoseconds
            .saturating_add(other.disk_read_queue_nanoseconds);
        self.disk_read_service_nanoseconds = self
            .disk_read_service_nanoseconds
            .saturating_add(other.disk_read_service_nanoseconds);
        self.disk_fill_attempts = self
            .disk_fill_attempts
            .saturating_add(other.disk_fill_attempts);
        self.disk_fill_wait_and_copy_nanoseconds = self
            .disk_fill_wait_and_copy_nanoseconds
            .saturating_add(other.disk_fill_wait_and_copy_nanoseconds);
        self.disk_backing_get_nanoseconds = self
            .disk_backing_get_nanoseconds
            .saturating_add(other.disk_backing_get_nanoseconds);
        self.disk_fill_service_nanoseconds = self
            .disk_fill_service_nanoseconds
            .saturating_add(other.disk_fill_service_nanoseconds);
        self.disk_hits = self.disk_hits.saturating_add(other.disk_hits);
        self.disk_misses = self.disk_misses.saturating_add(other.disk_misses);
        self.disk_corruptions = self.disk_corruptions.saturating_add(other.disk_corruptions);
        self.disk_io_failures = self.disk_io_failures.saturating_add(other.disk_io_failures);
        self.disk_evictions = self.disk_evictions.saturating_add(other.disk_evictions);
        self.disk_rejections = self.disk_rejections.saturating_add(other.disk_rejections);
        self.disk_resident_bytes = self
            .disk_resident_bytes
            .saturating_add(other.disk_resident_bytes);
        self.disk_disabled = self.disk_disabled.saturating_add(other.disk_disabled);
        self.hits = self.hits.saturating_add(other.hits);
        self.misses = self.misses.saturating_add(other.misses);
        self.evictions = self.evictions.saturating_add(other.evictions);
        self.backing_gets_completed = self
            .backing_gets_completed
            .saturating_add(other.backing_gets_completed);
        self.backing_gets_failed = self
            .backing_gets_failed
            .saturating_add(other.backing_gets_failed);
        self.backing_read_bytes = self
            .backing_read_bytes
            .saturating_add(other.backing_read_bytes);
        self.backing_get_nanoseconds = self
            .backing_get_nanoseconds
            .saturating_add(other.backing_get_nanoseconds);

        self.resident_bytes = self.resident_bytes.saturating_add(other.resident_bytes);
        self.resident_entries = self.resident_entries.saturating_add(other.resident_entries);
    }
    fn disk_change_since(&self, before: &Self) -> Value {
        json!({"lock_sections":self.disk_lock_sections.saturating_sub(before.disk_lock_sections),"lock_wait_nanoseconds":self.disk_lock_wait_nanoseconds.saturating_sub(before.disk_lock_wait_nanoseconds),"lock_hold_nanoseconds":self.disk_lock_hold_nanoseconds.saturating_sub(before.disk_lock_hold_nanoseconds),"lock_wait_max_nanoseconds_lifetime":self.disk_lock_wait_max_nanoseconds,"lock_hold_max_nanoseconds_lifetime":self.disk_lock_hold_max_nanoseconds,"fill_attempts":self.disk_fill_attempts.saturating_sub(before.disk_fill_attempts),"fill_wait_and_copy_nanoseconds":self.disk_fill_wait_and_copy_nanoseconds.saturating_sub(before.disk_fill_wait_and_copy_nanoseconds),"backing_get_nanoseconds":self.disk_backing_get_nanoseconds.saturating_sub(before.disk_backing_get_nanoseconds),"fill_service_nanoseconds":self.disk_fill_service_nanoseconds.saturating_sub(before.disk_fill_service_nanoseconds),"read_probes":self.disk_read_probes.saturating_sub(before.disk_read_probes),"read_queue_nanoseconds":self.disk_read_queue_nanoseconds.saturating_sub(before.disk_read_queue_nanoseconds),"read_service_nanoseconds":self.disk_read_service_nanoseconds.saturating_sub(before.disk_read_service_nanoseconds),"hits":self.disk_hits.saturating_sub(before.disk_hits),"misses":self.disk_misses.saturating_sub(before.disk_misses),
            "corruptions":self.disk_corruptions.saturating_sub(before.disk_corruptions),"io_failures":self.disk_io_failures.saturating_sub(before.disk_io_failures),
            "evictions":self.disk_evictions.saturating_sub(before.disk_evictions),"admission_rejections":self.disk_rejections.saturating_sub(before.disk_rejections),
            "resident_encoded_bytes_after":self.disk_resident_bytes,"disabled_caches_after":self.disk_disabled})
    }
    fn change_since(&self, before: &Self) -> Value {
        let hits = self.hits.saturating_sub(before.hits);
        let misses = self.misses.saturating_sub(before.misses);
        let total = hits.saturating_add(misses);
        json!({"backing_gets_completed":self.backing_gets_completed.saturating_sub(before.backing_gets_completed),"backing_gets_failed":self.backing_gets_failed.saturating_sub(before.backing_gets_failed),"backing_read_bytes":self.backing_read_bytes.saturating_sub(before.backing_read_bytes),"backing_get_nanoseconds":self.backing_get_nanoseconds.saturating_sub(before.backing_get_nanoseconds),"hits":hits,"misses":misses,"evictions":self.evictions.saturating_sub(before.evictions),
            "hit_ratio":if total == 0 {None} else {Some(hits as f64 / total as f64)},
            "resident_bytes_after":self.resident_bytes,"resident_entries_after":self.resident_entries})
    }
}

#[derive(Serialize, Deserialize)]
struct Config {
    node: usize,
    clients: usize,
    run: String,
    ca: String,
    cert: String,
    key: String,
    certificates: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct PeerInfo {
    node: usize,
    address: SocketAddr,
}
fn node_id(i: usize) -> String {
    format!("node-{i}")
}
fn boot(run: &str, i: usize) -> String {
    format!("{run}-boot-{i}")
}
fn volume(i: usize) -> String {
    format!("volume-{i}")
}
fn session_rpc_enabled() -> Result<bool> {
    match std::env::var("AUDIT_SESSION_RPC").as_deref() {
        Ok("1") => Ok(true),
        Ok("0") | Err(_) => Ok(false),
        _ => Err("AUDIT_SESSION_RPC must be 0 or 1".into()),
    }
}
fn session_token(run: &str, i: usize) -> String {
    format!("session-benchmark-client-token-{}", token(run, i))
}
#[derive(Clone)]
struct SessionRead {
    filesystem: mount_rs_remote::RemoteFs,
    handle: Arc<dyn mount_rs_core::FileHandle>,
}
impl SessionRead {
    async fn execute(
        &self,
        operation: ReadOperation,
        expected: &[u8],
    ) -> std::result::Result<ReadMeasurement, &'static str> {
        let before = self.filesystem.diagnostics();
        let start = Instant::now();
        let bytes = match operation {
            ReadOperation::StatJson => {
                let stats = self
                    .filesystem
                    .stat("/data")
                    .await
                    .map_err(|_| "session stat failed")?;
                if stats.size != expected.len() as u64 {
                    return Err("session stat size mismatch");
                }
                0 // Wire metadata size is not exposed by the adapter.
            }
            ReadOperation::Range { bytes, offset } => {
                let mut buffer = vec![0; bytes];
                let actual = self
                    .handle
                    .read(&mut buffer, Some(offset as u64))
                    .await
                    .map_err(|_| "session read failed")?;
                let expected = &expected[offset..expected.len().min(offset + bytes)];
                if &buffer[..actual] != expected {
                    return Err("session content mismatch");
                }
                actual
            }
            _ => return Err("unsupported session benchmark operation"),
        };
        let after = self.filesystem.diagnostics();
        if after.response_headers.checked_sub(before.response_headers) != Some(1)
            || after
                .successful_bodies
                .checked_sub(before.successful_bodies)
                != Some(1)
        {
            return Err("overlapping session diagnostics");
        }
        let headers = after
            .response_header_nanoseconds
            .checked_sub(before.response_header_nanoseconds)
            .ok_or("session header counter regressed")?;
        let body = after
            .successful_body_nanoseconds
            .checked_sub(before.successful_body_nanoseconds)
            .ok_or("session body counter regressed")?;
        let ingress = if after
            .audit_ingress_samples
            .checked_sub(before.audit_ingress_samples)
            == Some(1)
        {
            Some(
                after
                    .audit_ingress_nanoseconds
                    .checked_sub(before.audit_ingress_nanoseconds)
                    .ok_or("ingress counter regressed")? as f64
                    / 1e6,
            )
        } else {
            None
        };
        Ok(ReadMeasurement {
            session_ingress_ms: ingress,
            session_transport_ms: Some((headers as f64 / 1e6, body as f64 / 1e6)),
            wire_bytes: bytes,
            body_complete_ms: start.elapsed().as_secs_f64() * 1000.0,
            decode_ms: 0.0,
            validation_ms: 0.0,
        })
    }
    async fn close(self) -> Result<()> {
        let handle = self.handle.close().await;
        let session = self.filesystem.shutdown().await;
        handle.map_err(|error| format!("close reader handle: {error}"))?;
        session.map_err(|error| format!("shutdown reader session: {error}"))?;
        Ok(())
    }
}
fn token(run: &str, i: usize) -> String {
    format!("{run}-token-{i}")
}
fn emit(value: Value) {
    println!("{value}");
    std::io::stdout().flush().unwrap();
}

fn audit_server_threads() -> Result<usize> {
    let threads = match std::env::var("AUDIT_SERVER_THREADS") {
        Ok(value) => value.parse()?,
        Err(_) => std::thread::available_parallelism()?.get(),
    };
    if !(1..=256).contains(&threads) {
        return Err("AUDIT_SERVER_THREADS must be 1..256".into());
    }
    Ok(threads)
}

fn audit_shared_disk_budget() -> Result<bool> {
    match std::env::var("AUDIT_SHARED_DISK_BUDGET").as_deref() {
        Ok("1") => Ok(true),
        Ok("0") | Err(_) => Ok(false),
        _ => Err("AUDIT_SHARED_DISK_BUDGET must be 0 or 1".into()),
    }
}

fn audit_atime() -> Result<AtimePolicy> {
    match std::env::var("AUDIT_ATIME").as_deref() {
        Ok("strict") => Ok(AtimePolicy::Strict),
        Ok("noatime") | Err(_) => Ok(AtimePolicy::NoAtime),
        _ => Err("AUDIT_ATIME must be strict or noatime".into()),
    }
}

fn audit_blocking_threads() -> Result<Option<usize>> {
    match std::env::var("AUDIT_BLOCKING_THREADS") {
        Err(_) => Ok(None),
        Ok(value) if value.is_empty() => Ok(None),
        Ok(value) => {
            let limit = value.parse::<usize>()?;
            if !(1..=512).contains(&limit) {
                return Err("AUDIT_BLOCKING_THREADS must be 1..512".into());
            }
            Ok(Some(limit))
        }
    }
}

fn parse_client_threads(value: &str) -> Result<usize> {
    let threads = value.parse::<usize>()?;
    if !(1..=256).contains(&threads) {
        return Err("AUDIT_CLIENT_THREADS must be 1..256".into());
    }
    Ok(threads)
}

fn direct_client_threads() -> Result<Option<usize>> {
    let Ok(value) = std::env::var("AUDIT_DIRECT_CLIENT_THREADS") else {
        return Ok(None);
    };
    if std::env::var("AUDIT_DIRECT_PUBLIC").as_deref() != Ok("1")
        || std::env::var("AUDIT_DIRECT_INGRESS").as_deref() != Ok("1")
    {
        return Err(
            "AUDIT_DIRECT_CLIENT_THREADS requires the direct public client workload".into(),
        );
    }
    let threads = value.parse::<usize>()?;
    if !(1..=256).contains(&threads) {
        return Err("AUDIT_DIRECT_CLIENT_THREADS must be 1..256".into());
    }
    Ok(Some(threads))
}

// The Linux benchmark can constrain threads created by each runtime independently.
fn set_runtime_affinity(builder: &mut tokio::runtime::Builder, variable: &str) -> Result<()> {
    let Some(mask) = std::env::var_os(variable) else {
        return Ok(());
    };
    let mask = mask.into_string().map_err(|_| "invalid CPU list")?;
    if !cfg!(target_os = "linux") || mask.is_empty() {
        return Err("runtime CPU lists require Linux and a nonempty canonical CPU list".into());
    }
    builder.on_thread_start(move || {
        let thread = std::fs::read_link("/proc/thread-self").expect("read Linux thread id");
        let tid = thread.file_name().expect("Linux thread id");
        let status = std::process::Command::new("taskset")
            .args(["-pc", &mask])
            .arg(tid)
            .output()
            .expect("execute taskset");
        assert!(
            status.status.success(),
            "taskset: {}",
            String::from_utf8_lossy(&status.stderr)
        );
        let status = std::fs::read_to_string("/proc/thread-self/status").expect("read thread mask");
        let actual = status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .expect("effective CPU list")
            .trim();
        assert_eq!(actual, mask, "effective runtime CPU mask");
        eprintln!(
            "runtime affinity pid={} tid={} cpus={actual}",
            std::process::id(),
            tid.to_string_lossy()
        );
    });
    Ok(())
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let is_worker = args.get(1).map(String::as_str) == Some("--worker");
    if is_worker {
        retry_events::install()?;
    }
    let mut runtime = tokio::runtime::Builder::new_multi_thread();
    runtime.enable_all();
    if is_worker {
        runtime.worker_threads(audit_server_threads()?);
        if let Ok(raw) = std::env::var("AUDIT_SERVER_EVENT_INTERVAL") {
            let interval: std::num::NonZeroU32 = raw.parse()?;
            runtime.event_interval(interval.get());
            eprintln!("server runtime event interval={interval}");
        }
        set_runtime_affinity(&mut runtime, "AUDIT_SERVER_CPU_LIST")?;
        if let Some(limit) = audit_blocking_threads()? {
            runtime.max_blocking_threads(limit);
        }
    } else if let Ok(value) = std::env::var("AUDIT_CLIENT_THREADS") {
        runtime.worker_threads(parse_client_threads(&value)?);
    }
    // Own both runtimes in synchronous main so shutdown happens outside a worker.
    let client_runtime = if is_worker {
        if let Some(threads) = direct_client_threads()? {
            let mut builder = tokio::runtime::Builder::new_multi_thread();
            builder
                .worker_threads(threads)
                .thread_name("audit-http-client")
                .enable_all();
            set_runtime_affinity(&mut builder, "AUDIT_CLIENT_CPU_LIST")?;
            Some(builder.build()?)
        } else {
            None
        }
    } else {
        None
    };
    runtime.build()?.block_on(async {
        if is_worker {
            worker(
                Path::new(args.get(2).ok_or("missing worker config")?),
                client_runtime.as_ref().map(|rt| rt.handle()),
            )
            .await
        } else {
            let seconds = std::env::var("AUDIT_CASE_TIMEOUT_SECONDS")
                .unwrap_or_else(|_| "1800".into())
                .parse::<u64>()?;
            if !(1..=21600).contains(&seconds) {
                return Err("AUDIT_CASE_TIMEOUT_SECONDS must be 1..21600".into());
            }
            timeout(Duration::from_secs(seconds), controller())
                .await
                .map_err(|_| format!("load case exceeded {seconds}s controller deadline"))?
        }
    })
}

// Diagnostic distribution only: all traffic retains the same authoritative owner.
#[derive(Clone, Copy)]
struct PeerPoolRouting {
    count: usize,
    separate_reads: bool,
}
impl PeerPoolRouting {
    fn from_env() -> Result<Self> {
        let count = std::env::var("AUDIT_PEER_POOLS")
            .unwrap_or_else(|_| "1".into())
            .parse()?;
        let separate_reads = match std::env::var("AUDIT_SEPARATE_READ_PEERS").as_deref() {
            Ok("1") => true,
            Ok("0" | "") | Err(_) => false,
            _ => return Err("AUDIT_SEPARATE_READ_PEERS must be 0 or 1".into()),
        };
        Self::new(count, separate_reads)
    }
    fn new(count: usize, separate_reads: bool) -> Result<Self> {
        if !(1..=8).contains(&count) {
            return Err("AUDIT_PEER_POOLS must be 1..8".into());
        }
        if separate_reads && count != 2 {
            return Err("AUDIT_SEPARATE_READ_PEERS requires AUDIT_PEER_POOLS=2".into());
        }
        Ok(Self {
            count,
            separate_reads,
        })
    }
    fn index(self, method: &http::Method, path: &str) -> usize {
        if self.separate_reads {
            return usize::from(*method != http::Method::GET && *method != http::Method::HEAD);
        }
        let volume = path.split('/').nth(3).unwrap_or("");
        volume
            .bytes()
            .fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize))
            % self.count
    }
}
fn ingress_admission_config() -> Result<Option<(usize, usize)>> {
    let Some(value) = std::env::var("AUDIT_INGRESS_WRITE_LIMIT")
        .ok()
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    let active = value.parse()?;
    let queued = std::env::var("AUDIT_INGRESS_WRITE_QUEUE")
        .unwrap_or_else(|_| "100".into())
        .parse()?;
    ingress_admission::Admission::new(active, queued)?;
    Ok(Some((active, queued)))
}
struct BenchmarkIngress {
    capture: arc_swap::ArcSwapOption<forward_samples::Capture>,
    ingresses: Vec<ClusterIngress>,
    routing: PeerPoolRouting,
    admission: Option<ingress_admission::Admission>,
}
impl mount_rs_http::HttpRequestHandler for BenchmarkIngress {
    fn on_http_service_call(&self, request: &mut http::Request<mount_rs_http::HttpBody>) {
        if request.headers().contains_key(forward_samples::HEADER) {
            request
                .extensions_mut()
                .insert(forward_samples::ServiceStart(Instant::now()));
        }
    }
    fn handle(
        &self,
        mut request: http::Request<mount_rs_http::HttpBody>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = http::Response<mount_rs_http::HttpBody>> + Send + '_>,
    > {
        let capture = self.capture.load_full();
        let index = self.routing.index(request.method(), request.uri().path());
        Box::pin(async move {
            let dispatched = Instant::now();
            let response_sample =
                capture.and_then(|capture| capture.attach(&mut request, dispatched));
            let _permit = match &self.admission {
                Some(admission) => match admission.acquire(request.method()).await {
                    Ok(permit) => permit,
                    Err(reason) => {
                        use http_body_util::BodyExt;
                        return http::Response::builder()
                            .status(503)
                            .header("connection", "close")
                            .header(ingress_admission::ERROR_HEADER, reason)
                            .body(
                                http_body_util::Full::new(bytes::Bytes::new())
                                    .map_err(|never| -> mount_rs_http::BoxError { match never {} })
                                    .boxed_unsync(),
                            )
                            .expect("static diagnostic response");
                    }
                },
                None => None,
            };
            let pool = request
                .extensions()
                .get::<Arc<request_detail::Pool>>()
                .cloned();
            let handle = mount_rs_http::HttpRequestHandler::handle(&self.ingresses[index], request);
            let mut response = if let Some(pool) = pool {
                request_detail::POOL.scope(pool, handle).await
            } else {
                handle.await
            };
            if let Some(sample) = response_sample {
                response
                    .headers_mut()
                    .insert(forward_samples::HEADER, (sample as u64).into());
            }
            response.headers_mut().insert(
                "x-mount-audit-ingress-ns",
                u64::try_from(dispatched.elapsed().as_nanos())
                    .unwrap_or(u64::MAX)
                    .into(),
            );
            response
        })
    }
}

#[cfg(target_os = "linux")]
fn force_socket_buffers(socket: &std::net::UdpSocket, bytes: usize) -> Result<()> {
    use std::os::fd::AsRawFd;
    let value = libc::c_int::try_from(bytes)?;
    for option in [libc::SO_RCVBUFFORCE, libc::SO_SNDBUFFORCE] {
        // SAFETY: socket owns a live descriptor; value is an initialized c_int
        // and its exact size is supplied. Only this socket is modified.
        let result = unsafe {
            libc::setsockopt(
                socket.as_raw_fd(),
                libc::SOL_SOCKET,
                option,
                (&value as *const libc::c_int).cast(),
                std::mem::size_of_val(&value) as libc::socklen_t,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
    }
    Ok(())
}

fn audit_endpoint(server: Option<quinn::ServerConfig>) -> Result<(quinn::Endpoint, Value)> {
    let requested: usize = std::env::var("AUDIT_UDP_BUFFER_BYTES")
        .unwrap_or_else(|_| "0".into())
        .parse()?;
    let receive: usize = std::env::var("AUDIT_UDP_RECV_BUFFER_BYTES")
        .unwrap_or_else(|_| requested.to_string())
        .parse()?;
    let send: usize = std::env::var("AUDIT_UDP_SEND_BUFFER_BYTES")
        .unwrap_or_else(|_| requested.to_string())
        .parse()?;
    if requested.max(receive).max(send) > 16 * 1024 * 1024 {
        return Err("UDP buffer exceeds 16 MiB".into());
    }
    let socket = std::net::UdpSocket::bind("127.0.0.1:0")?;
    let reference = socket2::SockRef::from(&socket);
    if receive != 0 {
        reference.set_recv_buffer_size(receive)?;
    }
    if send != 0 {
        reference.set_send_buffer_size(send)?;
    }
    let forced = std::env::var("AUDIT_FORCE_UDP_BUFFERS").as_deref() == Ok("1");
    if forced {
        if receive != requested || send != requested {
            return Err("forced buffers require equal shared settings".into());
        }
        if requested == 0 {
            return Err("forced buffers require a positive size".into());
        }
        #[cfg(target_os = "linux")]
        force_socket_buffers(&socket, requested)?;
        #[cfg(not(target_os = "linux"))]
        return Err("forced socket buffers are Linux-only".into());
    }
    let actual = json!({"port":socket.local_addr()?.port(),"forced":forced,"requested_bytes":requested,
        "requested_receive_bytes":receive,"requested_send_bytes":send,
        "receive_bytes":reference.recv_buffer_size()?,"send_bytes":reference.send_buffer_size()?});
    let endpoint = quinn::Endpoint::new(
        Default::default(),
        server,
        socket,
        Arc::new(quinn::TokioRuntime),
    )?;
    Ok((endpoint, actual))
}

async fn worker(path: &Path, client_runtime: Option<&tokio::runtime::Handle>) -> Result<()> {
    let config: Config = serde_json::from_slice(&std::fs::read(path)?)?;
    let server_tls = load_peer_server_config_from_pem(
        config.cert.as_bytes(),
        config.key.as_bytes(),
        config.ca.as_bytes(),
    )?;
    let (endpoint, inbound_socket) = audit_endpoint(Some(server_tls))?;
    let own_address = endpoint.local_addr()?;
    emit(json!({"peer": {"node":config.node,"address":own_address},"pid":std::process::id()}));
    let mut commands = BufReader::new(tokio::io::stdin()).lines();
    let peers: Vec<PeerInfo> = serde_json::from_str(
        &commands
            .next_line()
            .await?
            .ok_or("missing peer directory")?,
    )?;
    let mut trusted = Vec::new();
    for peer in peers {
        let cert = &config.certificates[peer.node];
        let der = pem_der(cert)?;
        let pin = spki_sha256_from_der(&der)?;
        trusted.push(TrustedPeer {
            node: node_id(peer.node),
            boot: boot(&config.run, peer.node),
            address: peer.address,
            tls_server_name: "localhost".into(),
            pin,
        });
    }
    let redis = redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL")?)?;
    let registry = RedisRouteRegistry::new(
        redis.get_multiplexed_async_connection().await?,
        config.run.clone(),
        Duration::from_secs(2),
    )?;
    let mut volumes = Vec::new();
    let mut handlers = Vec::new();
    let session_budget = SessionBudget::new(4096)?;
    let mut registrations = Vec::new();
    let block_backend = std::env::var("AUDIT_BLOCKS").unwrap_or_else(|_| "memory".into());
    let pool_limit = audit_tidb_pool_limit()?;
    let mut shared_tidb_pool = None;
    let share_r2 = audit_shared_r2_client()?;
    let mut shared_r2_client = None;
    let block_root = tempfile::tempdir()?;
    let disk_root = tempfile::tempdir()?;
    let local_write_budget = audit_local_write_limit()?
        .map(mount_rs_sdk::LocalWriteBudget::new)
        .transpose()?;
    let background_read_fills = match std::env::var("AUDIT_BACKGROUND_READ_FILLS").as_deref() {
        Ok("1") => true,
        Ok("0") | Err(_) => false,
        _ => return Err("AUDIT_BACKGROUND_READ_FILLS must be 0 or 1".into()),
    };
    let disk_budget = mount_rs_sdk::DiskCacheBudget::new(audit_disk_workers()?, 4)?
        .with_background_read_fills(background_read_fills);
    for i in (config.node..config.clients).step_by(config.certificates.len()) {
        let mut options =
            SplitOptions::memory(format!("{}-{i}", config.run), audit_storage_chunk_bytes()?);
        match std::env::var("AUDIT_METADATA")
            .as_deref()
            .unwrap_or("memory")
        {
            "memory" => {}
            "tidb" => {
                let connection = std::env::var("MOUNT_RS_TIDB_URL")?;
                let parsed = reqwest::Url::parse(&connection)?;
                if parsed.scheme() != "mysql"
                    || !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1"))
                {
                    return Err("TiDB benchmark requires loopback harness".into());
                }
                let scope = std::env::var("MOUNT_RS_TIDB_TEST_VOLUME_KEY")?;
                let volume_key = format!("{scope}-{}-{i}", config.run);
                options.metadata = if let Some(limit) = pool_limit {
                    if shared_tidb_pool.is_none() {
                        shared_tidb_pool = Some(TidbStorePool::new(&connection, limit)?);
                    }
                    StoreConfig::TidbShared {
                        pool: shared_tidb_pool.as_ref().unwrap().clone(),
                        volume_key,
                        durable: true,
                    }
                } else {
                    StoreConfig::Tidb {
                        connection,
                        volume_key,
                        durable: true,
                    }
                };
            }
            _ => return Err("AUDIT_METADATA must be memory or tidb".into()),
        }

        options.blocks = match block_backend.as_str() {
            "memory" => StoreConfig::Memory,
            "local-fsync" => {
                let path = block_root.path().join(volume(i));
                std::fs::create_dir(&path)?;
                match &local_write_budget {
                    Some(budget) => StoreConfig::LocalWithWriteBudget {
                        path,
                        budget: budget.clone(),
                    },
                    None => StoreConfig::Local { path },
                }
            }
            "rustfs" => {
                let harness = std::env::var("RUSTFS_RUN_DIR")?;
                if !Path::new(&harness).is_dir() {
                    return Err("missing isolated RustFS harness".into());
                }
                let endpoint = std::env::var("R2_ENDPOINT")?;
                let parsed = reqwest::Url::parse(&endpoint)?;
                if parsed.scheme() != "http"
                    || !matches!(parsed.host_str(), Some("localhost" | "127.0.0.1"))
                {
                    return Err("RustFS benchmark requires loopback harness".into());
                }
                let bucket = std::env::var("R2_BUCKET")?;
                let prefix = format!(
                    "{}/{}/{}",
                    std::env::var("RUSTFS_COMBO_PREFIX")?,
                    config.run,
                    volume(i)
                );
                let access_key_id = std::env::var("R2_ACCESS_KEY_ID")?;
                let secret_access_key = std::env::var("R2_SECRET_ACCESS_KEY")?;
                if share_r2 {
                    if shared_r2_client.is_none() {
                        shared_r2_client = Some(mount_rs_sdk::SharedR2Client::from_config(
                            &mount_rs_r2::R2Config {
                                endpoint,
                                bucket,
                                access_key_id,
                                secret_access_key,
                                state_key: prefix.clone(),
                            },
                        )?);
                    }
                    StoreConfig::R2Shared {
                        client: shared_r2_client.as_ref().unwrap().clone(),
                        prefix,
                        durable: true,
                    }
                } else {
                    StoreConfig::R2 {
                        endpoint,
                        bucket,
                        prefix,
                        access_key_id,
                        secret_access_key,
                        durable: true,
                    }
                }
            }
            _ => return Err("AUDIT_BLOCKS must be memory, local-fsync or rustfs".into()),
        };
        let policies = SplitRuntimeOptions {
            read_parallelism: audit_read_parallelism()?,
            lease_ttl: Duration::from_secs(240),
            atime_policy: audit_atime()?,
            read_lease_cache_window: Duration::from_secs(60),
            block_cache: Some(BlockCacheOptions {
                max_bytes: 4 * 1024 * 1024,
                max_entries: 128,
            }),
        };
        let fs = Arc::new(if audit_disk_cache()? {
            Filesystem::split_with_disk_cache_budget(
                options,
                policies,
                mount_rs_sdk::DiskCacheOptions {
                    root: disk_root.path().to_owned(),
                    scope: format!("{}-{}-{i}", config.run, config.node),
                    max_bytes: 16 * 1024 * 1024,
                    max_entries: 512,
                },
                if audit_shared_disk_budget()? {
                    disk_budget.clone()
                } else {
                    mount_rs_sdk::DiskCacheBudget::new(audit_disk_workers()?, 4)?
                        .with_background_read_fills(background_read_fills)
                },
            )
            .await?
        } else {
            Filesystem::split_with_runtime(options, policies).await?
        });
        for entry in 0..audit_directory_entries()? {
            fs.driver()
                .mkdir(
                    &format!("/fixture-{entry:05}"),
                    mount_rs_sdk::MkdirOptions::default(),
                )
                .await?;
        }
        if matches!(
            std::env::var("AUDIT_READ_OPERATION").as_deref(),
            Ok("stat-symlink" | "lstat" | "readlink")
        ) {
            fs.driver().symlink("/data", "/audit-link").await?;
        }
        let route = VolumeRoute {
            volume: volume(i),
            node: node_id(config.node),
            boot: boot(&config.run, config.node),
            registration: format!("{}-{i}", config.run),
            fence: fs.checked_owner_fence().await?,
            address: own_address,
            tls_server_name: "localhost".into(),
        };
        let registration = Arc::new(
            OwnedVolumeRegistration::new(
                fs.clone(),
                registry.clone(),
                route.clone(),
                Duration::from_secs(15),
            )
            .await
            .map_err(|e| format!("registration {e:?}"))?,
        );
        let handler = OwnerHttpHandler::new(
            fs.clone(),
            route,
            token(&config.run, i),
            HttpServerOptions {
                read_chunk_bytes: audit_read_chunk_bytes()?,
                ..Default::default()
            },
        )
        .await?;
        let handler = match std::env::var("AUDIT_OWNER_OPERATION_TIMINGS").as_deref() {
            Ok("1") => handler.with_session_operation_timings(),
            Ok("0") | Err(std::env::VarError::NotPresent) => handler,
            _ => return Err("AUDIT_OWNER_OPERATION_TIMINGS must be 0 or 1".into()),
        };
        handlers.push(if session_rpc_enabled()? {
            handler.with_sessions(
                SessionCredentials::new(vec![("benchmark".into(), session_token(&config.run, i))])?,
                128,
                4,
                Duration::from_secs(3600),
                session_budget.clone(),
            )?
        } else {
            handler
        });
        volumes.push(fs);
        registrations.push(registration);
    }
    let session_registries = handlers
        .iter()
        .filter_map(OwnerHttpHandler::session_registry)
        .collect::<Vec<_>>();
    let session_supervisor = if session_registries.is_empty() {
        None
    } else {
        Some(SessionSupervisor::start(session_registries)?)
    };
    let router = OwnerHttpRouter::new(handlers)?;
    let directory = Arc::new(PeerDirectory::new(trusted)?);
    let pins = directory.approved_pins()?;
    let inbound = directory.clone();
    let policy = move |identity: &TlsIdentity, _: &http::Request<()>| inbound.accepts(identity);
    let stop = CancellationToken::new();
    let peer_server = PeerServer::new(
        endpoint,
        pins.clone(),
        PeerTransportLimits::default(),
        policy,
        router.clone(),
    )?;
    let peer_task = tokio::spawn(peer_server.serve(stop.clone()));
    let peer_routing = PeerPoolRouting::from_env()?;
    let mut clients = Vec::new();
    let mut ingresses = Vec::new();
    let mut diagnostic_forwarders = Vec::new();
    let mut outbound_sockets = Vec::new();
    for _ in 0..peer_routing.count {
        let (mut outgoing, socket) = audit_endpoint(None)?;
        outbound_sockets.push(socket);
        outgoing.set_default_client_config(load_peer_client_config_from_pem(
            config.cert.as_bytes(),
            config.key.as_bytes(),
            config.ca.as_bytes(),
        )?);
        let client = Arc::new(PeerClient::new(
            outgoing,
            pins.clone(),
            PeerTransportLimits::default(),
        )?);
        let forwarder = IngressForwarder::new(
            registry.clone(),
            directory.clone(),
            client.clone(),
            ForwardOptions {
                route_cache_ttl: audit_route_cache_ttl()?,
                renew_route_on_success: audit_route_renewal()?,
                refresh_active_routes: audit_proactive_refresh()?,
                ..ForwardOptions::default()
            },
        )?;
        diagnostic_forwarders.push(forwarder.clone());
        ingresses.push(ClusterIngress::new(router.clone(), forwarder));
        clients.push(client);
    }
    let benchmark_ingress = Arc::new(BenchmarkIngress {
        capture: arc_swap::ArcSwapOption::empty(),
        ingresses,
        routing: peer_routing,
        admission: ingress_admission_config()?
            .map(|(active, queued)| ingress_admission::Admission::new(active, queued))
            .transpose()?
            .map(|gate| {
                if std::env::var("AUDIT_INGRESS_PUT_ONLY").as_deref() == Ok("1") {
                    gate.put_only()
                } else {
                    gate
                }
            }),
    });
    let public = HttpServer::start_with_handler(
        benchmark_ingress.clone(),
        HttpServerOptions {
            tcp_nodelay: audit_tcp_nodelay()?,
            http2_prior_knowledge: audit_http2()?,
            ..HttpServerOptions::default()
        },
    )
    .await?;
    let registration_supervisor = if registrations.is_empty() {
        None
    } else {
        let supervisor = RegistrationSupervisor::start_ready(
            registrations,
            RegistrationRecovery {
                initial_delay: Duration::from_millis(100),
                max_delay: Duration::from_secs(2),
                max_attempts: 10,
            },
            Duration::from_secs(10),
        )
        .await?;
        Some(supervisor)
    };
    emit(
        json!({"ready":public.url(),"volumes":volumes.len(),"pid":std::process::id(),
        "udp_sockets":{"inbound":inbound_socket,"outbound":outbound_sockets[0],"outbound_pools":outbound_sockets}}),
    );
    let mut probe: Option<(CancellationToken, tokio::task::JoinHandle<Value>)> = None;
    while let Some(command) = commands.next_line().await? {
        if !matches!(command.as_str(), "stop" | "health")
            && (!registration_supervisor
                .as_ref()
                .is_none_or(RegistrationSupervisor::ready)
                || session_supervisor
                    .as_ref()
                    .is_some_and(SessionSupervisor::failed))
        {
            let registrations = registration_supervisor
                .as_ref()
                .map(RegistrationSupervisor::statuses);
            let sessions_failed = session_supervisor
                .as_ref()
                .is_some_and(SessionSupervisor::failed);
            return Err(format!(
                "worker {} refused {command}: registrations={registrations:?} sessions_failed={sessions_failed}",
                std::process::id()
            ).into());
        }
        match command.as_str() {
            "probe-start" => {
                if probe.is_some() {
                    return Err("probe already running".into());
                }
                let stop = CancellationToken::new();
                let task = tokio::spawn(scheduler_probe(stop.clone()));
                probe = Some((stop, task));
                emit(json!({"probe_started":true}));
            }
            "probe-stop" => {
                let (stop, task) = probe.take().ok_or("probe not running")?;
                stop.cancel();
                emit(task.await?);
            }
            "clear" | "clear-all" => {
                for fs in &volumes {
                    fs.clear_block_cache().await;
                    if command == "clear-all" {
                        fs.clear_disk_cache().await?;
                    }
                }
                emit(json!({"cleared":true}));
            }
            "owner-stats" => {
                emit(serde_json::to_value(router.timing_snapshots())?);
            }
            "retry-stats" => {
                emit(json!(retry_events::count()));
            }
            "admission-stats" => {
                emit(
                    benchmark_ingress
                        .admission
                        .as_ref()
                        .map(|gate| gate.take_stats())
                        .unwrap_or(Value::Null),
                );
            }
            "forward-stats" => {
                emit(serde_json::to_value(
                    diagnostic_forwarders
                        .iter()
                        .map(IngressForwarder::diagnostics)
                        .collect::<Vec<_>>(),
                )?);
            }
            "volume-read-stats" => {
                let mut rows = Vec::with_capacity(volumes.len());
                for (index, fs) in volumes.iter().enumerate() {
                    let ram = fs.block_cache_stats().await.ok_or("missing block cache")?;
                    let disk = fs.disk_cache_stats().await?;
                    rows.push(json!({
                        "volume": config.node + index * config.certificates.len(),
                        "reads": ram.backing_gets_completed, "provider_ns": ram.backing_get_nanoseconds,
                        "backing_ns": disk.map(|s| s.backing_get_nanoseconds),
                        "read_fill_ns": disk.map(|s| s.read_fill_nanoseconds)
                    }));
                }
                emit(json!(rows));
            }
            "cache-stats" => {
                let mut total = CacheTotals::default();
                for fs in &volumes {
                    let stats = fs.block_cache_stats().await.ok_or("missing block cache")?;
                    total.add(&CacheTotals {
                        hits: stats.hits,
                        misses: stats.misses,
                        evictions: stats.evictions,
                        resident_bytes: stats.resident_bytes as u64,
                        resident_entries: stats.resident_entries as u64,
                        backing_gets_completed: stats.backing_gets_completed,
                        backing_gets_failed: stats.backing_gets_failed,
                        backing_read_bytes: stats.backing_read_bytes,
                        backing_get_nanoseconds: stats.backing_get_nanoseconds,
                        ..Default::default()
                    });
                    if let Some(disk) = fs.disk_cache_stats().await? {
                        total.add(&CacheTotals {
                            disk_lock_sections: disk.lock_sections,
                            disk_lock_wait_nanoseconds: disk.lock_wait_nanoseconds,
                            disk_lock_hold_nanoseconds: disk.lock_hold_nanoseconds,
                            disk_lock_wait_max_nanoseconds: disk.lock_wait_max_nanoseconds,
                            disk_lock_hold_max_nanoseconds: disk.lock_hold_max_nanoseconds,
                            disk_read_probes: disk.read_probes,
                            disk_read_queue_nanoseconds: disk.read_queue_nanoseconds,
                            disk_read_service_nanoseconds: disk.read_service_nanoseconds,
                            disk_fill_attempts: disk.fill_attempts,
                            disk_fill_wait_and_copy_nanoseconds: disk
                                .fill_wait_and_copy_nanoseconds,
                            disk_fill_service_nanoseconds: disk.fill_service_nanoseconds,
                            disk_backing_get_nanoseconds: disk.backing_get_nanoseconds,
                            disk_hits: disk.hits,
                            disk_misses: disk.misses,
                            disk_corruptions: disk.corruptions,
                            disk_io_failures: disk.io_failures,
                            disk_evictions: disk.evictions,
                            disk_rejections: disk.admission_rejections,
                            disk_resident_bytes: disk.resident_bytes,
                            disk_disabled: u64::from(disk.disabled),
                            ..Default::default()
                        });
                    }
                }
                emit(serde_json::to_value(total)?);
            }
            "health" => {
                let mut health = Vec::new();
                for (index, fs) in volumes.iter().enumerate() {
                    let (state, failure_stage, publication_reason) =
                        match fs.checked_owner_fence().await {
                            Ok(_) => ("ready".to_string(), None, None),
                            Err(error) => {
                                // Export only known operation labels, never backend messages,
                                // paths, connection strings or other provider-controlled text.
                                let stage = match error.syscall.as_deref() {
                                    Some(
                                        stage @ ("lease-renew-timeout"
                                        | "lease-renew"
                                        | "lease-acquire"
                                        | "metadata-load"
                                        | "metadata-validate"
                                        | "metadata-publish"
                                        | "metadata-flush"),
                                    ) => Some(stage.to_owned()),
                                    _ => Some("other".to_owned()),
                                };
                                let reason = if stage.as_deref() == Some("metadata-publish") {
                                    let detail = error.to_string();
                                    [
                                        "owner mismatch",
                                        "fence mismatch",
                                        "expiry token mismatch",
                                        "lease expired",
                                        "publication predicate failed",
                                    ]
                                    .into_iter()
                                    .find(|label| detail == *label)
                                } else {
                                    None
                                };
                                (format!("{:?}", error.code), stage, reason)
                            }
                        };
                    health.push(json!({"volume":volume(config.node + index * config.certificates.len()),"state":state,"failure_stage":failure_stage,"publication_reason":publication_reason}));
                }
                let forwarding: Vec<_> = diagnostic_forwarders
                    .iter()
                    .map(IngressForwarder::diagnostics)
                    .collect();
                let session_expiry_failed = session_supervisor
                    .as_ref()
                    .is_some_and(SessionSupervisor::failed);
                let registration_health: Vec<_> = registration_supervisor
                    .as_ref()
                    .map(RegistrationSupervisor::statuses)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|status| json!({"status":format!("{status:?}")}))
                    .collect();
                let registration_ready = registration_supervisor
                    .as_ref()
                    .is_none_or(RegistrationSupervisor::ready);
                emit(
                    json!({"owner_health":health,"forwarding_diagnostics":forwarding,
                    "registration_health":registration_health,"registration_ready":registration_ready,
                    "session_expiry_failed":session_expiry_failed}),
                );
            }
            command if command.starts_with("direct-ingress ") => {
                let batch = serde_json::from_str(command.strip_prefix("direct-ingress ").unwrap())?;
                let run = direct_ingress::run(
                    benchmark_ingress.clone(),
                    batch,
                    (std::env::var("AUDIT_DIRECT_PUBLIC").as_deref() == Ok("1"))
                        .then(|| public.url()),
                );
                let result = match client_runtime {
                    Some(runtime) => runtime.spawn(run).await?,
                    None => run.await,
                };
                match result {
                    Ok(report) => emit(report),
                    Err(error) => {
                        eprintln!("direct workload failed: {error}");
                        emit(json!({"direct_batch_failed":true}));
                    }
                }
            }
            "stop" => break,
            _ => return Err("invalid worker command".into()),
        }
    }
    public.close().await?;
    if let Some(supervisor) = registration_supervisor {
        supervisor.shutdown().await?;
    }
    stop.cancel();
    peer_task.await??;
    if let Some(supervisor) = session_supervisor {
        supervisor.shutdown().await?;
    }
    router.close().await?;
    for client in clients {
        client.shutdown().await?;
    }
    for fs in volumes {
        fs.shutdown().await?;
    }
    if let Some(pool) = shared_tidb_pool {
        pool.close().await?;
    }
    emit(json!({"stopped":true}));
    Ok(())
}

fn pem_der(pem: &str) -> Result<Vec<u8>> {
    use base64::Engine;
    let encoded: String = pem
        .lines()
        .filter(|line| !line.starts_with("-----"))
        .collect();
    Ok(base64::engine::general_purpose::STANDARD.decode(encoded)?)
}

struct Worker {
    process: Child,
    input: ChildStdin,
    output: tokio::io::Lines<BufReader<ChildStdout>>,
    url: String,
    pid: u32,
    udp_sockets: Value,
}
impl Worker {
    async fn read(&mut self) -> Result<Value> {
        self.read_with_timeout(120).await
    }
    async fn read_with_timeout(&mut self, seconds: u64) -> Result<Value> {
        let line = timeout(Duration::from_secs(seconds), self.output.next_line())
            .await
            .map_err(|_| format!("worker {} response exceeded {seconds}s", self.pid))??
            .ok_or("worker exited before response")?;
        Ok(serde_json::from_str(&line)?)
    }
    async fn send(&mut self, line: &str) -> Result<()> {
        self.input.write_all(format!("{line}\n").as_bytes()).await?;
        self.input.flush().await?;
        Ok(())
    }
}

fn configurations(
    root: &Path,
    clients: usize,
    run: &str,
    servers: usize,
) -> Result<Vec<std::path::PathBuf>> {
    let ca_key = KeyPair::generate()?;
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    let ca = params.self_signed(&ca_key)?;
    let mut leaves = Vec::new();
    for i in 0..servers {
        let key = KeyPair::generate()?;
        let mut params = CertificateParams::new(vec!["localhost".into()])?;
        params.subject_alt_names.push(SanType::URI(
            format!("urn:agent-tunnel:peer:{}", node_id(i)).try_into()?,
        ));
        params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ClientAuth,
            ExtendedKeyUsagePurpose::ServerAuth,
        ];
        leaves.push((
            params.signed_by(&key, &ca, &ca_key)?.pem(),
            key.serialize_pem(),
        ));
    }
    let certificates: Vec<String> = leaves.iter().map(|(cert, _)| cert.clone()).collect();
    let mut paths = Vec::new();
    for (node, (cert, key)) in leaves.into_iter().enumerate() {
        let path = root.join(format!("{run}-{node}.json"));
        std::fs::write(
            &path,
            serde_json::to_vec(&Config {
                node,
                clients,
                run: run.into(),
                ca: ca.pem(),
                cert,
                key,
                certificates: certificates.clone(),
            })?,
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        paths.push(path);
    }
    Ok(paths)
}

async fn start_workers(
    root: &Path,
    clients: usize,
    run: &str,
    servers: usize,
) -> Result<Vec<Worker>> {
    let paths = configurations(root, clients, run, servers)?;
    let mut workers = Vec::new();
    let mut peers = Vec::new();
    for path in paths {
        let mut process = Command::new(std::env::current_exe()?)
            .arg("--worker")
            .arg(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let pid = process.id().ok_or("worker has no PID")?;
        let input = process.stdin.take().unwrap();
        let output = BufReader::new(process.stdout.take().unwrap()).lines();
        let mut worker = Worker {
            process,
            input,
            output,
            url: String::new(),
            udp_sockets: Value::Null,
            pid,
        };
        let ready = worker.read().await?;
        peers.push(serde_json::from_value::<PeerInfo>(ready["peer"].clone())?);
        workers.push(worker);
    }
    let peers = serde_json::to_string(&peers)?;
    for worker in &mut workers {
        worker.send(&peers).await?;
    }
    for worker in &mut workers {
        let ready = worker.read().await?;
        worker.udp_sockets = ready["udp_sockets"].clone();
        worker.url = ready["ready"]
            .as_str()
            .ok_or("missing public listener")?
            .to_owned();
    }
    Ok(workers)
}

fn payload(client: usize, bytes: usize) -> Vec<u8> {
    (0..bytes)
        .map(|i| ((i * 31 + client * 17) % 251) as u8)
        .collect()
}
fn summary(mut values: Vec<f64>) -> Value {
    let raw = values.clone();
    if values.is_empty() {
        return json!({"samples":0});
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    json!({"samples":n,"p50_ms":values[(n*50).div_ceil(100)-1],"p95_ms":values[(n*95).div_ceil(100)-1],"p99_ms":values[(n*99).div_ceil(100)-1],"max_ms":values[n-1],"over_10ms":values.iter().filter(|x|**x>=10.0).count(),"samples_ms":raw,"sample_order":"successful completion order within sequential bursts"})
}
async fn resource_sample(pids: &[u32]) -> Value {
    let ids = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    match Command::new("ps")
        .args(["-p", &ids, "-o", "pid=,%cpu=,rss="])
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let rows: String = String::from_utf8_lossy(&output.stdout).into_owned();
            json!({"ps_pid_cpu_percent_rss_kib":rows})
        }
        _ => json!({"unavailable":true}),
    }
}
// Separate correctness probe: all three ingress processes write concurrently
// to distinct paths on one volume, then each ingress observes every commit.
async fn shared_volume_probe(workers: &[Worker], run: &str) -> Result<()> {
    let mut writes = tokio::task::JoinSet::new();
    for (writer, worker) in workers.iter().enumerate() {
        let url = format!(
            "{}/v1/drives/{}/fs/consistency-{writer}",
            worker.url,
            volume(0)
        );
        let bearer = token(run, 0);
        writes.spawn(async move {
            let response = audit_http_client()?
                .put(url)
                .bearer_auth(bearer)
                .body(format!("writer-{writer}"))
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "shared-volume write headers writer={writer}: {}",
                        transport_error(e)
                    )
                })?;
            if !response.status().is_success() {
                return Err(format!(
                    "shared-volume write status writer={writer}: {}",
                    response.status().as_u16()
                )
                .into());
            }
            response.bytes().await.map_err(|e| {
                format!(
                    "shared-volume write body writer={writer}: {}",
                    transport_error(e)
                )
            })?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        });
    }
    while let Some(result) = writes.join_next().await {
        result??;
    }
    let client = audit_http_client()?;
    for (ingress, worker) in workers.iter().enumerate() {
        for writer in 0..workers.len() {
            let url = format!(
                "{}/v1/drives/{}/fs/consistency-{writer}",
                worker.url,
                volume(0)
            );
            let response = client
                .get(url)
                .bearer_auth(token(run, 0))
                .send()
                .await
                .map_err(|e| {
                    format!(
                        "shared-volume read headers ingress={ingress} writer={writer}: {}",
                        transport_error(e)
                    )
                })?;
            if !response.status().is_success()
                || response
                    .bytes()
                    .await
                    .map_err(|e| {
                        format!(
                            "shared-volume read body ingress={ingress} writer={writer}: {}",
                            transport_error(e)
                        )
                    })?
                    .as_ref()
                    != format!("writer-{writer}").as_bytes()
            {
                return Err("shared-volume visibility check failed".into());
            }
        }
    }
    // Each acknowledged replacement must be visible through every ingress,
    // including a zero-length replacement after the content caches are warm.
    for round in 0..=workers.len() {
        let writer = round % workers.len();
        let expected = if round == workers.len() {
            Vec::new()
        } else {
            vec![(round + 1) as u8; 4096 + round * 17]
        };
        let path = format!("/v1/drives/{}/fs/consistency-shared", volume(0));
        let response = client
            .put(format!("{}{path}", workers[writer].url))
            .bearer_auth(token(run, 0))
            .body(expected.clone())
            .send()
            .await
            .map_err(transport_error)?;
        if !response.status().is_success() {
            return Err("shared overwrite failed".into());
        }
        response.bytes().await.map_err(transport_error)?;
        for worker in workers {
            let response = client
                .get(format!("{}{path}", worker.url))
                .bearer_auth(token(run, 0))
                .send()
                .await
                .map_err(transport_error)?;
            if !response.status().is_success()
                || response.bytes().await.map_err(transport_error)?.as_ref() != expected
            {
                return Err("shared overwrite content visibility failed".into());
            }
            let response = client
                .get(format!(
                    "{}/v1/drives/{}/stat/consistency-shared",
                    worker.url,
                    volume(0)
                ))
                .bearer_auth(token(run, 0))
                .send()
                .await
                .map_err(transport_error)?;
            if !response.status().is_success() {
                return Err("shared overwrite stat failed".into());
            }
            let bytes = response.bytes().await.map_err(transport_error)?;
            let stats: mount_rs_sdk::Stats = serde_json::from_slice(&bytes)?;
            if !stats.is_file() || stats.size != expected.len() as u64 {
                return Err("shared overwrite metadata visibility failed".into());
            }
        }
    }
    Ok(())
}

async fn probe_command(workers: &mut [Worker], command: &str) -> Result<Vec<Value>> {
    for worker in workers.iter_mut() {
        worker.send(command).await?;
    }
    let mut results = Vec::new();
    for worker in workers {
        results.push(worker.read().await?);
    }
    Ok(results)
}

async fn cache_totals(workers: &mut [Worker]) -> Result<CacheTotals> {
    for worker in workers.iter_mut() {
        worker.send("cache-stats").await?;
    }
    let mut total = CacheTotals::default();
    for worker in workers {
        total.add(&serde_json::from_value(worker.read().await?)?);
    }
    Ok(total)
}

fn save_checkpoint(
    metadata: &str,
    blocks: &str,
    payload_bytes: usize,
    results: &[Value],
    shared_volume_checks: &[Value],
) -> Result<()> {
    if let Ok(path) = std::env::var("AUDIT_CHECKPOINT_PATH") {
        #[derive(Serialize)]
        struct Checkpoint<'a> {
            complete: bool,
            metadata: &'a str,
            blocks: &'a str,
            payload_bytes: usize,
            results: &'a [Value],
            shared_volume_checks: &'a [Value],
        }
        let checkpoint = Checkpoint {
            complete: false,
            metadata,
            blocks,
            payload_bytes,
            results,
            shared_volume_checks,
        };
        let destination = Path::new(&path);
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut writer = std::io::BufWriter::new(temporary.as_file_mut());
            serde_json::to_writer(&mut writer, &checkpoint)?;
            writer.flush()?;
        }
        temporary.persist(destination)?;
    }
    Ok(())
}

fn audit_route_renewal() -> Result<bool> {
    match std::env::var("AUDIT_ROUTE_RENEWAL").as_deref() {
        Ok("1") => Ok(true),
        Ok("0") | Err(_) => Ok(false),
        _ => Err("AUDIT_ROUTE_RENEWAL must be 0 or 1".into()),
    }
}

fn audit_proactive_refresh() -> Result<bool> {
    match std::env::var("AUDIT_PROACTIVE_REFRESH").as_deref() {
        Ok("1") => Ok(true),
        Ok("0") | Err(_) => Ok(false),
        _ => Err("AUDIT_PROACTIVE_REFRESH must be 0 or 1".into()),
    }
}

fn audit_route_cache_ttl() -> Result<Duration> {
    let millis: u64 = std::env::var("AUDIT_ROUTE_CACHE_TTL_MS")
        .unwrap_or_else(|_| "1000".into())
        .parse()?;
    if !(1..=5000).contains(&millis) {
        return Err("AUDIT_ROUTE_CACHE_TTL_MS must be 1..5000".into());
    }
    Ok(Duration::from_millis(millis))
}

async fn controller() -> Result<()> {
    let session_rpc = session_rpc_enabled()?;
    let direct = std::env::var("AUDIT_DIRECT_INGRESS").as_deref() == Ok("1");
    let disk_enabled = audit_disk_cache()?;
    let shared_reads = match std::env::var("AUDIT_SHARED_READ_VOLUME").as_deref() {
        Ok("1") => true,
        Ok("0") | Err(_) => false,
        _ => return Err("AUDIT_SHARED_READ_VOLUME must be 0 or 1".into()),
    };
    let write_operation = match std::env::var("AUDIT_WRITE_OPERATION")
        .as_deref()
        .unwrap_or("file")
    {
        "file" => WriteOperation::File,
        "rename" => WriteOperation::RenameRoundtrip,
        "chmod" if session_rpc => WriteOperation::Chmod,
        _ => return Err(
            "AUDIT_WRITE_OPERATION must be file, rename, or chmod (requires AUDIT_SESSION_RPC=1)"
                .into(),
        ),
    };
    if direct && !matches!(write_operation, WriteOperation::File) {
        return Err("direct ingress supports only AUDIT_WRITE_OPERATION=file".into());
    }
    let read_operation = match std::env::var("AUDIT_READ_OPERATION")
        .as_deref()
        .unwrap_or("file")
    {
        "file" => ReadOperation::File,
        "range" => {
            let bytes = std::env::var("AUDIT_RANGE_BYTES")
                .unwrap_or_else(|_| "4096".into())
                .parse::<usize>()?;
            if !(1..=1048576).contains(&bytes) {
                return Err("AUDIT_RANGE_BYTES must be 1..1048576".into());
            }
            let offset = std::env::var("AUDIT_RANGE_OFFSET")
                .unwrap_or_else(|_| "0".into())
                .parse::<usize>()?;
            if offset >= 1048576 {
                return Err("AUDIT_RANGE_OFFSET must be 0..1048575".into());
            }
            ReadOperation::Range { bytes, offset }
        }
        "stat" => ReadOperation::Stat,
        "stat-json" => ReadOperation::StatJson,
        "statfs" => ReadOperation::StatFs,
        "stat-symlink" => ReadOperation::SymlinkStat,
        "lstat" => ReadOperation::Lstat,
        "readlink" => ReadOperation::Readlink,
        "readdir" => ReadOperation::Readdir {
            extra_entries: audit_directory_entries()?,
            gzip: match std::env::var("AUDIT_LISTING_GZIP").as_deref() {
                Ok("1") => true,
                Ok("0" | "") | Err(_) => false,
                _ => return Err("AUDIT_LISTING_GZIP must be 0 or 1".into()),
            },
        },
        _ => {
            return Err(
                "AUDIT_READ_OPERATION must be file, range, stat, stat-json, statfs, stat-symlink, lstat, readlink or readdir"
                    .into(),
            );
        }
    };

    if session_rpc
        && !matches!(
            read_operation,
            ReadOperation::StatJson
                | ReadOperation::Range {
                    bytes: 1..=65536,
                    ..
                }
        )
    {
        return Err("session RPC supports stat-json and range reads up to 64KiB".into());
    }

    let servers: usize = std::env::var("AUDIT_SERVERS")
        .unwrap_or_else(|_| "3".into())
        .parse()?;
    if !(1..=3).contains(&servers) {
        return Err("AUDIT_SERVERS must be 1..3".into());
    }

    let peer_routing = PeerPoolRouting::from_env()?;
    let ingress_admission = ingress_admission_config()?;
    let metadata_backend = std::env::var("AUDIT_METADATA").unwrap_or_else(|_| "memory".into());
    if !matches!(metadata_backend.as_str(), "memory" | "tidb") {
        return Err("AUDIT_METADATA must be memory or tidb".into());
    }

    let directory_entries = audit_directory_entries()?;
    let storage_chunk_bytes = audit_storage_chunk_bytes()?;
    let local_write_limit = audit_local_write_limit()?;
    let block_backend = std::env::var("AUDIT_BLOCKS").unwrap_or_else(|_| "memory".into());
    if !matches!(block_backend.as_str(), "memory" | "local-fsync" | "rustfs") {
        return Err("AUDIT_BLOCKS must be memory, local-fsync or rustfs".into());
    }
    std::env::var("MOUNT_RS_REDIS_TEST_URL")
        .map_err(|_| "requires disposable MOUNT_RS_REDIS_TEST_URL")?;
    let samples: usize = std::env::var("AUDIT_SAMPLES")
        .unwrap_or_else(|_| "20".into())
        .parse()?;
    if samples == 0 || samples > 1000 {
        return Err("AUDIT_SAMPLES must be 1..1000".into());
    }
    let write_concurrency: usize = std::env::var("AUDIT_WRITE_CONCURRENCY")
        .unwrap_or_else(|_| "100".into())
        .parse()?;
    if !(1..=100).contains(&write_concurrency) {
        return Err("AUDIT_WRITE_CONCURRENCY must be 1..100".into());
    }
    let payload_bytes: usize = std::env::var("AUDIT_PAYLOAD_BYTES")
        .unwrap_or_else(|_| "131123".into())
        .parse()?;
    if payload_bytes == 0 || payload_bytes > 1024 * 1024 {
        return Err("AUDIT_PAYLOAD_BYTES must be 1..1048576".into());
    }
    if let ReadOperation::Range { offset, .. } = read_operation
        && offset >= payload_bytes
    {
        return Err("AUDIT_RANGE_OFFSET must be below AUDIT_PAYLOAD_BYTES".into());
    }
    let levels: Vec<usize> = std::env::var("AUDIT_CLIENT_LEVELS")
        .unwrap_or_else(|_| "1,5,10,25,50,100".into())
        .split(',')
        .map(str::parse)
        .collect::<std::result::Result<_, _>>()?;
    if levels.is_empty() || levels.iter().any(|n| *n == 0 || *n > 100) {
        return Err("client levels must be 1..100".into());
    }
    let root = tempfile::tempdir()?;
    let mut results = Vec::new();
    let mut shared_volume_checks = Vec::new();
    for clients in levels {
        let run = format!(
            "load-{}-{clients}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        );
        let volumes = if shared_reads { 1 } else { clients };
        let mut workers = start_workers(root.path(), volumes, &run, servers).await?;
        let pids: Vec<u32> = workers.iter().map(|worker| worker.pid).collect();
        let http: Vec<_> = (0..clients)
            .map(|_| audit_http_client())
            .collect::<std::result::Result<_, _>>()?;
        let data: Vec<_> = (0..clients)
            .map(|i| Arc::new(payload(if shared_reads { 0 } else { i }, payload_bytes)))
            .collect();
        let seed_started = Instant::now();
        let seed_progress_step = volumes.div_ceil(10).max(1);
        eprintln!(
            "cluster seed start volumes={volumes} payload_bytes={payload_bytes} storage_chunk_bytes={storage_chunk_bytes}"
        );
        for i in 0..volumes {
            let url = format!(
                "{}/v1/drives/{}/fs/data",
                workers[i % servers].url,
                volume(i)
            );
            let response = http[i]
                .put(url)
                .bearer_auth(token(&run, i))
                .body(data[i].as_ref().clone())
                .send()
                .await;
            let failure = match response {
                Ok(reply) if reply.status().is_success() => {
                    reply.bytes().await?;
                    None
                }
                Ok(reply) => {
                    let status = reply.status().as_u16();
                    let not_dispatched = reply
                        .headers()
                        .get("x-mount-dispatch")
                        .is_some_and(|value| value == "not-dispatched");
                    // Only report known API codes. Provider messages can include paths.
                    let code = if reply.content_length().is_some_and(|size| size <= 4096) {
                        reply
                            .bytes()
                            .await
                            .ok()
                            .and_then(|bytes| {
                                serde_json::from_slice::<serde_json::Value>(&bytes).ok()
                            })
                            .and_then(|body| match body.pointer("/error/code")?.as_str()? {
                                "ENOENT" => Some("ENOENT"),
                                "ESTALE" => Some("ESTALE"),
                                "EIO" => Some("EIO"),
                                "EAGAIN" => Some("EAGAIN"),
                                _ => None,
                            })
                    } else {
                        None
                    };
                    Some(format!(
                        "status={status} not_dispatched={not_dispatched} api_code={}",
                        code.unwrap_or("unclassified")
                    ))
                }
                Err(error) => Some(format!(
                    "transport timeout={} connect={}",
                    error.is_timeout(),
                    error.is_connect()
                )),
            };
            if let Some(failure) = failure {
                let owner = &mut workers[i % servers];
                if owner.send("health").await.is_ok()
                    && let Ok(Ok(health)) =
                        tokio::time::timeout(Duration::from_secs(5), owner.read()).await
                {
                    eprintln!("seed failure owner health: {health}");
                }
                return Err(
                    format!("seed write failed: clients={clients} client={i} {failure}").into(),
                );
            }
            if matches!(write_operation, WriteOperation::RenameRoundtrip) {
                let response = http[i]
                    .post(format!(
                        "{}/v1/drives/{}/ops/mkdir",
                        workers[i % servers].url,
                        volume(i)
                    ))
                    .bearer_auth(token(&run, i))
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .body(json!({"path":"/rename-a","recursive":false,"mode":448}).to_string())
                    .send()
                    .await?;
                if !response.status().is_success() {
                    return Err("rename fixture mkdir failed".into());
                }
                response.bytes().await?;
            }
            if (i + 1) % seed_progress_step == 0 || i + 1 == volumes {
                eprintln!(
                    "cluster seed progress completed={} volumes={volumes} elapsed_ms={:.3}",
                    i + 1,
                    seed_started.elapsed().as_secs_f64() * 1000.0
                );
            }
        }
        let seed_elapsed_ms = seed_started.elapsed().as_secs_f64() * 1000.0;
        if direct {
            let repetitions: usize = std::env::var("AUDIT_DIRECT_REPETITIONS")
                .unwrap_or_else(|_| "1".to_owned())
                .parse()?;
            if !(1..=100).contains(&repetitions) {
                return Err("AUDIT_DIRECT_REPETITIONS must be 1..100".into());
            }
            for repetition in 1..=repetitions {
                results.extend(
                    direct_ingress::controller(&mut workers, &run, clients, samples).await?,
                );
                if repetition % 10 == 0 {
                    save_checkpoint(
                        &metadata_backend,
                        &block_backend,
                        payload_bytes,
                        &results,
                        &shared_volume_checks,
                    )?;
                }
            }
        }

        for forwarded in [false, true]
            .into_iter()
            .filter(|forwarded| !direct && (!forwarded || servers > 1))
        {
            for cache_mode in 0..if disk_enabled { 3 } else { 2 } {
                let cold = cache_mode != 0;
                let cache_label = match (disk_enabled, cache_mode) {
                    (_, 0) => "warm",
                    (true, 1) => "cleared-ram-warm-disk",
                    (true, _) => "cleared-ram-and-disk-before-each-burst",
                    (false, _) => "cleared-before-each-burst",
                };
                for mixed in [false, true]
                    .into_iter()
                    .filter(|mixed| !shared_reads || !mixed)
                {
                    // A RAM hit can mask a disk entry evicted by the preceding write phase.
                    // Prime through the disk layer before measuring disk-warm reads.
                    if disk_enabled && cache_mode == 1 {
                        for worker in &mut workers {
                            worker.send("clear").await?;
                            if worker.read().await?["cleared"] != true {
                                return Err("could not clear RAM before disk priming".into());
                            }
                        }
                    }
                    let mut session_reads = Vec::new();
                    let mut session_writes = Vec::new();
                    if session_rpc {
                        for (i, expected) in data.iter().enumerate().take(clients) {
                            let volume_index = if shared_reads { 0 } else { i };
                            let ingress =
                                (volume_index % servers + usize::from(forwarded)) % servers;
                            let setup_error = |operation: &str, error: &dyn std::fmt::Display| {
                                format!(
                                    "session setup failed: operation={operation} client={i} volume={volume_index} ingress={ingress} forwarded={forwarded} cold={cold} mixed={mixed} cause={error}"
                                )
                            };
                            let filesystem = mount_rs_remote::RemoteFs::connect_with_http2(
                                &workers[ingress].url,
                                &volume(volume_index),
                                &session_token(&run, volume_index),
                                Duration::from_secs(30),
                                audit_http2()?,
                            )
                            .await
                            .map_err(|e| setup_error("connect reader", &e))?;
                            if std::env::var("AUDIT_HEADER_READS").as_deref() == Ok("1") {
                                filesystem
                                    .enable_header_reads()
                                    .await
                                    .map_err(|e| setup_error("enable header reads", &e))?;
                            }
                            if matches!(write_operation, WriteOperation::Chmod) {
                                session_writes.push(
                                    mount_rs_remote::RemoteFs::connect_with_http2(
                                        &workers[ingress].url,
                                        &volume(volume_index),
                                        &session_token(&run, volume_index),
                                        Duration::from_secs(30),
                                        audit_http2()?,
                                    )
                                    .await
                                    .map_err(|e| setup_error("connect writer", &e))?,
                                );
                            }
                            let handle = filesystem
                                .open("/data", "r", 0)
                                .await
                                .map_err(|e| setup_error("open reader", &e))?;
                            let reader = SessionRead { filesystem, handle };
                            reader
                                .execute(read_operation, expected)
                                .await
                                .map_err(|e| setup_error("prime reader", &e))?;
                            session_reads.push(reader);
                        }
                    }
                    // Establish pools/routes and validate each path outside measured bursts.
                    // Cold phases clear the application cache again before every burst.
                    for i in 0..clients {
                        let volume_index = if shared_reads { 0 } else { i };
                        let ingress = (volume_index % servers + usize::from(forwarded)) % servers;
                        let url = format!(
                            "{}/v1/drives/{}/fs/data",
                            workers[ingress].url,
                            volume(volume_index)
                        );
                        if let Err(error) = read_operation
                            .execute(&http[i], &url, &token(&run, volume_index), &data[i])
                            .await
                        {
                            eprintln!(
                                "warmup failure operation={} cause={error}",
                                read_operation.name()
                            );
                            for worker in &mut workers {
                                worker.send("health").await?;
                            }
                            for worker in &mut workers {
                                eprintln!("worker health: {}", worker.read().await?);
                            }
                            return Err(format!("phase warmup failed: clients={clients} client={i} forwarded={forwarded} cold={cold} mixed={mixed} cause={error}").into());
                        }
                    }
                    let cache_before = cache_totals(&mut workers).await?;
                    let retries_before = probe_command(&mut workers, "retry-stats").await?;
                    let forward_before = probe_command(&mut workers, "forward-stats").await?;
                    let owner_before = probe_command(&mut workers, "owner-stats").await?;
                    let before = resource_sample(&pids).await;
                    let probe_enabled = match std::env::var("AUDIT_SCHEDULER_PROBE").as_deref() {
                        Ok("1") => true,
                        Ok("0") | Err(_) => false,
                        _ => return Err("AUDIT_SCHEDULER_PROBE must be 0 or 1".into()),
                    };
                    if probe_enabled {
                        probe_command(&mut workers, "probe-start").await?;
                    }
                    let phase_start = Instant::now();
                    let mut active_seconds = 0.0;
                    let mut reads = Vec::new();
                    let mut session_ingress = Vec::new();
                    let mut session_headers = Vec::new();
                    let mut session_bodies = Vec::new();
                    let mut body_completion = Vec::new();
                    let mut decoding = Vec::new();
                    let mut validation = Vec::new();
                    let mut response_bytes_total = 0u64;
                    let mut response_bytes_min: Option<usize> = None;
                    let mut response_bytes_max = 0usize;
                    let mut writes = Vec::new();
                    let mut errors = BTreeMap::<String, usize>::new();
                    let write_slots =
                        Arc::new(tokio::sync::Semaphore::new(write_concurrency.min(clients)));
                    for iteration in 0..samples {
                        if cold {
                            for worker in &mut workers {
                                worker
                                    .send(if disk_enabled && cache_mode == 2 {
                                        "clear-all"
                                    } else {
                                        "clear"
                                    })
                                    .await?;
                            }
                            for worker in &mut workers {
                                if worker.read().await?["cleared"] != true {
                                    return Err("cache clear failed".into());
                                }
                            }
                        }
                        let started = Instant::now();
                        let mut tasks = tokio::task::JoinSet::new();
                        let barrier = Arc::new(tokio::sync::Barrier::new(clients));
                        for i in 0..clients {
                            let http = http[i].clone();
                            let expected = data[i].clone();
                            let barrier = barrier.clone();
                            let volume_index = if shared_reads { 0 } else { i };
                            let ingress =
                                (volume_index % servers + usize::from(forwarded)) % servers;
                            let url = format!(
                                "{}/v1/drives/{}/fs/data",
                                workers[ingress].url,
                                volume(volume_index)
                            );
                            let write_url = format!(
                                "{}/v1/drives/{}",
                                workers[ingress].url,
                                volume(volume_index)
                            );
                            let token = token(&run, volume_index);
                            let write_slots = write_slots.clone();
                            let session_read = session_reads.get(i).cloned();
                            let session_write = session_writes.get(i).cloned();
                            tasks.spawn(async move {
                                barrier.wait().await;
                                let read = async {
                                    let start = Instant::now();
                                    let result = if let Some(session) = session_read {
                                        session.execute(read_operation, &expected).await
                                    } else {
                                        read_operation
                                            .execute_detailed(&http, &url, &token, &expected)
                                            .await
                                    };
                                    (start.elapsed().as_secs_f64() * 1000.0, result)
                                };
                                let write = async {
                                    if !mixed {
                                        return None;
                                    }
                                    let start = Instant::now();
                                    let result = async {
                                        // Queueing is part of measured write latency. Reads
                                        // remain unconstrained by this diagnostic control.
                                        let _slot = write_slots
                                            .acquire()
                                            .await
                                            .map_err(|_| "write admission closed")?;
                                        write_operation
                                            .execute(
                                                &http,
                                                &write_url,
                                                &token,
                                                iteration,
                                                session_write.as_ref(),
                                            )
                                            .await
                                    }
                                    .await;
                                    Some((start.elapsed().as_secs_f64() * 1000.0, result))
                                };
                                tokio::join!(read, write)
                            });
                        }
                        while let Some(result) = tasks.join_next().await {
                            let (read, write) = result?;
                            match read.1 {
                                Ok(measurement) => {
                                    if let Some(ingress) = measurement.session_ingress_ms {
                                        session_ingress.push(ingress);
                                    }
                                    if let Some((headers, body)) = measurement.session_transport_ms
                                    {
                                        session_headers.push(headers);
                                        session_bodies.push(body);
                                    }
                                    let bytes = measurement.wire_bytes;
                                    body_completion.push(measurement.body_complete_ms);
                                    decoding.push(measurement.decode_ms);
                                    validation.push(measurement.validation_ms);
                                    reads.push(read.0);
                                    response_bytes_total += bytes as u64;
                                    response_bytes_min = Some(
                                        response_bytes_min.map_or(bytes, |old| old.min(bytes)),
                                    );
                                    response_bytes_max = response_bytes_max.max(bytes);
                                }
                                Err(error) => *errors.entry(error.into()).or_default() += 1,
                            }
                            if let Some(write) = write {
                                match write.1 {
                                    Ok(()) => writes.push(write.0),
                                    Err(error) => *errors.entry(error.into()).or_default() += 1,
                                }
                            }
                        }
                        active_seconds += started.elapsed().as_secs_f64();
                    }
                    let scheduler = if probe_enabled {
                        probe_command(&mut workers, "probe-stop").await?
                    } else {
                        Vec::new()
                    };
                    let teardown_error = |client: usize, error: &dyn std::fmt::Display| {
                        format!(
                            "session teardown failed: client={client} path_forwarded={forwarded} cache={cache_label} mixed={mixed} successful_reads={} successful_writes={} operation_errors={errors:?} cause={error}",
                            reads.len(),
                            writes.len()
                        )
                    };
                    for (client, session) in session_writes.into_iter().enumerate() {
                        session
                            .shutdown()
                            .await
                            .map_err(|error| teardown_error(client, &error))?;
                    }
                    for (client, session) in session_reads.into_iter().enumerate() {
                        session
                            .close()
                            .await
                            .map_err(|error| teardown_error(client, &error))?;
                    }
                    let cache_after = cache_totals(&mut workers).await?;
                    let retries_after = probe_command(&mut workers, "retry-stats").await?;
                    let forward_after = probe_command(&mut workers, "forward-stats").await?;
                    let owner_after = probe_command(&mut workers, "owner-stats").await?;
                    let successful = reads.len() + writes.len();
                    let mut result = json!({"clients":clients,"unique_volumes":volumes,"servers":servers,"server_pids":pids,"udp_sockets":workers.iter().map(|w| &w.udp_sockets).collect::<Vec<_>>(),
                "path":if forwarded{"forwarded"}else{"local-owner"},"cache":cache_label,
                "workload":if mixed{"one-read-and-one-write-workload-concurrently-per-client"}else{"read-only"},
                "read_operation":read_operation.name(),"file_size_bytes":payload_bytes,"http_read_chunk_bytes":audit_read_chunk_bytes()?,"server_runtime_threads":audit_server_threads()?,
                "tcp_nodelay":audit_tcp_nodelay()?,
                "read_parallelism":audit_read_parallelism()?,"tidb_shared_pool_max_connections":audit_tidb_pool_limit()?,"scheduler_probe":scheduler,
                "read_payload_bytes":match read_operation {ReadOperation::File=>Some(payload_bytes), ReadOperation::Range { bytes, offset }=>Some((payload_bytes-offset).min(bytes)), ReadOperation::Stat=>Some(0), ReadOperation::Readdir { .. } | ReadOperation::StatJson | ReadOperation::StatFs | ReadOperation::SymlinkStat | ReadOperation::Lstat | ReadOperation::Readlink=>None},
                "write_operation":if mixed{Some(write_operation.name())}else{None},
                "forwarding_before":forward_before,"forwarding_after":forward_after,
                "owner_before":owner_before,"owner_after":owner_after,
                "metadata_cache":"resident-owner-namespace","block_cache":cache_after.change_since(&cache_before),
                "block_cache_backing":if disk_enabled{"disk-cache"}else{"authoritative-provider"},
                "disk_cache":if disk_enabled{Some(cache_after.disk_change_since(&cache_before))}else{None},
                "disk_cache_max_encoded_bytes_per_volume":if disk_enabled{Some(16*1024*1024)}else{None},
                "write_concurrency":if mixed{Some(write_concurrency.min(clients))}else{None},
                "owner_lease_ttl_ms":240000,"owner_serving_window_ms":60000,"owner_renewal_interval_ms":30000,
                "write_admission":"client-side diagnostic; write latency includes queue; reads unrestricted",
                "iterations":samples,"read":summary(reads),"write":summary(writes),"errors":errors,
                "active_seconds":active_seconds,"phase_wall_seconds":phase_start.elapsed().as_secs_f64(),
                "successful_ops_per_active_second":successful as f64/active_seconds,
                "resources_before":before,"resources_after":resource_sample(&pids).await});
                    result["http2_prior_knowledge"] = json!(audit_http2()?);
                    result["client_runtime_threads"] =
                        json!(tokio::runtime::Handle::current().metrics().num_workers());
                    result["header_reads"] =
                        json!(std::env::var("AUDIT_HEADER_READS").as_deref() == Ok("1"));
                    result["read_protocol"] = json!(if session_rpc {
                        "session-rpc"
                    } else {
                        "file-http"
                    });
                    if session_rpc {
                        result["session_measurement_note"] = json!(
                            "adapter time includes decode and validation; separate breakdown unavailable; metadata wire bytes unavailable and reported as zero; setup excluded; mixed chmod writes use separate session RPC clients; other mixed writes use file HTTP"
                        );
                    }

                    result["object_store_retry_events"] = json!({"scope":"all worker requests including writes; pinned object_store 0.12.5 INFO retry events, not HTTP attempt counts","before_per_server":retries_before,"after_per_server":retries_after});
                    if let ReadOperation::Range { bytes, offset } = read_operation {
                        result["requested_range_bytes"] = json!(bytes);
                        result["range_offset"] = json!(offset);
                    }
                    result["directory_validator"] = json!("typed-direntry-canonical-indices-v2");
                    result["client_read_stages"] = json!({
                        "session_ingress_to_response": summary(session_ingress),
                        "session_dispatch_to_headers": summary(session_headers),
                        "session_headers_to_body": summary(session_bodies),
                        "body_completion": summary(body_completion),
                        "decompression": summary(decoding),
                        "validation": summary(validation),
                        "scope": "successful requests only; body completion includes scheduling and transport; stages are not independent and percentiles must not be added or subtracted"
                    });
                    result["read_response_bytes_total"] = json!(response_bytes_total);
                    result["read_response_bytes_min"] = json!(response_bytes_min);
                    result["read_response_bytes_max"] = json!(if response_bytes_min.is_none() {
                        None
                    } else {
                        Some(response_bytes_max)
                    });
                    result["fixture_directory_entries"] = json!(directory_entries);
                    result["shared_r2_client_per_server"] = json!(audit_shared_r2_client()?);
                    result["seed_elapsed_ms"] = json!(seed_elapsed_ms);
                    result["seed_volumes"] = json!(volumes);
                    result["storage_chunk_bytes"] = json!(storage_chunk_bytes);
                    result["local_write_limit"] = json!(local_write_limit);
                    result["ingress_write_admission"] = json!(ingress_admission.map(|(active, queued)| json!({"active":active,"queued":queued,"timeout_seconds":5,"held_until":"response headers"})));
                    result["peer_pool_count"] = json!(peer_routing.count);
                    result["separate_read_peers"] = json!(peer_routing.separate_reads);
                    result["listing_gzip"] = json!(matches!(
                        read_operation,
                        ReadOperation::Readdir { gzip: true, .. }
                    ));
                    result["route_cache_ttl_ms"] = json!(audit_route_cache_ttl()?.as_millis());
                    result["shared_read_volume"] = json!(shared_reads);
                    result["shared_disk_budget"] = json!(audit_shared_disk_budget()?);
                    result["disk_cache_workers_per_budget"] = json!(audit_disk_workers()?);
                    result["server_max_blocking_threads"] = json!(audit_blocking_threads()?);
                    result["atime_policy"] = json!(match audit_atime()? {
                        AtimePolicy::Strict => "strict",
                        AtimePolicy::NoAtime => "noatime",
                    });
                    eprintln!(
                        "cluster load clients={clients} forwarded={forwarded} cold={cold} mixed={mixed} complete read_p99_ms={} write_p99_ms={} errors={}",
                        result["read"]["p99_ms"], result["write"]["p99_ms"], result["errors"]
                    );
                    results.push(result);
                    save_checkpoint(
                        &metadata_backend,
                        &block_backend,
                        payload_bytes,
                        &results,
                        &shared_volume_checks,
                    )?;
                    if !errors.is_empty() {
                        // Save samples first: diagnostics may fail if an owner exits.
                        let health = async {
                            for worker in &mut workers {
                                worker.send("health").await?;
                            }
                            let mut health = Vec::new();
                            for worker in &mut workers {
                                health.push(worker.read().await?);
                            }
                            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(health)
                        }
                        .await;
                        let health = match health {
                            Ok(health) => json!({"status":"captured","workers":health}),
                            Err(_) => json!({"status":"unavailable"}),
                        };
                        eprintln!("failed phase owner health: {health}");
                        results.last_mut().expect("phase just inserted")["owner_health_after_failure"] =
                            health;
                        save_checkpoint(
                            &metadata_backend,
                            &block_backend,
                            payload_bytes,
                            &results,
                            &shared_volume_checks,
                        )?;
                        return Err(
                            "measured requests failed; phase checkpoint saved when configured"
                                .into(),
                        );
                    }
                }
            }
        }
        let probe = match timeout(
            Duration::from_secs(120),
            shared_volume_probe(&workers, &run),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err("shared-volume probe deadline".into()),
        };
        shared_volume_checks.push(match &probe {
            Ok(()) => json!({"clients_in_load_phase":clients,"servers":servers,
                "concurrent_writers":servers,"verified_reads":servers * servers + (servers + 1) * servers,
                "verified_metadata_reads":(servers + 1) * servers,
                "sequential_shared_replacements":servers + 1,
                "probe_version":"cross-ingress-overwrite-v2","status":"pass"}),
            Err(error) => json!({"clients_in_load_phase":clients,"servers":servers,
                "status":"failed","failure":error.to_string()}),
        });
        save_checkpoint(
            &metadata_backend,
            &block_backend,
            payload_bytes,
            &results,
            &shared_volume_checks,
        )?;
        if probe.is_err() {
            let diagnostics = timeout(Duration::from_secs(5), async {
                for worker in &mut workers {
                    worker.send("health").await?;
                }
                let mut snapshots = Vec::new();
                for worker in &mut workers {
                    snapshots.push(worker.read().await?);
                }
                Ok::<_, Box<dyn std::error::Error + Send + Sync>>(snapshots)
            })
            .await;
            shared_volume_checks.last_mut().expect("probe inserted")["diagnostics"] =
                match diagnostics {
                    Ok(Ok(snapshots)) => json!({"status":"captured","workers":snapshots}),
                    _ => json!({"status":"unavailable"}),
                };
            save_checkpoint(
                &metadata_backend,
                &block_backend,
                payload_bytes,
                &results,
                &shared_volume_checks,
            )?;
        }
        probe?;
        for worker in &mut workers {
            worker.send("stop").await?;
        }
        for worker in &mut workers {
            if worker.read().await?["stopped"] != true {
                return Err("worker shutdown failed".into());
            }
            if !timeout(Duration::from_secs(20), worker.process.wait())
                .await??
                .success()
            {
                return Err("worker failed".into());
            }
        }
    }
    let mut report = json!({"complete":true,"surface":if std::env::var("AUDIT_DIRECT_INGRESS").as_deref() == Ok("1") {"in-process-ingress-with-http3-forwarding"} else {"server-process-public-http-with-optional-http3-forwarding"},"servers":servers,"metadata":metadata_backend,"blocks":block_backend,"profile":if cfg!(debug_assertions){"debug"}else{"release"},
        "read_operation":read_operation.name(),"payload_bytes":payload_bytes,"resource_scope":"server-process ps snapshots; CPU percentage is ps accounting, not instantaneous CPU; not peak RSS",
        "load_model":if shared_reads {"barrier-aligned read-only bursts; one volume shared by all client pools; cold clearing outside timing"} else {"barrier-aligned closed-loop bursts; cold clearing outside active timing; unique reqwest pool and volume per client"},
        "results":results,"shared_volume_checks":shared_volume_checks});
    report["configuration"] = json!({
        "route_cache_ttl_ms": audit_route_cache_ttl()?.as_millis(),
        "renew_route_on_success": audit_route_renewal()?,
        "refresh_active_routes": audit_proactive_refresh()?,
        "http2_prior_knowledge": audit_http2()?,
        "session_rpc": session_rpc,
        "server_threads": audit_server_threads()?,
        "server_cpu_list": std::env::var("AUDIT_SERVER_CPU_LIST").ok(),
        "client_cpu_list": std::env::var("AUDIT_CLIENT_CPU_LIST").ok(),
        "direct_client_runtime_threads": direct_client_threads()?,
        "storage_chunk_bytes": audit_storage_chunk_bytes()?,
        "tidb_shared_pool_max_connections": audit_tidb_pool_limit()?
    });
    if std::env::var("AUDIT_DIRECT_INGRESS").as_deref() == Ok("1") {
        report["load_model"] = json!(
            "per-server barriers; caller tasks inside server processes; client runtime placement specified in configuration; unique volume per caller; public socket inclusion and mixed workload specified per result row; warmup excluded from latency samples"
        );
    }
    emit(report);
    Ok(())
}

#[cfg(test)]
mod transport_diagnostic_tests {
    #[test]
    fn client_runtime_threads_are_bounded() {
        for value in ["1", "4", "256"] {
            assert_eq!(
                super::parse_client_threads(value).unwrap().to_string(),
                value
            );
        }
        for value in ["", "0", "257", "-1", "many"] {
            assert!(super::parse_client_threads(value).is_err());
        }
    }
    #[test]
    fn disk_worker_budget_is_bounded() {
        for value in ["1", "4", "16", "64"] {
            assert_eq!(super::parse_disk_workers(value).unwrap().to_string(), value);
        }
        for value in ["0", "65", "-1", "", "many"] {
            assert!(super::parse_disk_workers(value).is_err());
        }
    }
    use super::*;

    #[test]
    fn diagnostic_peer_routing_separates_only_transport_pools() {
        assert!(PeerPoolRouting::new(0, false).is_err());
        assert!(PeerPoolRouting::new(9, false).is_err());
        assert!(PeerPoolRouting::new(1, true).is_err());
        assert!(PeerPoolRouting::new(4, true).is_err());
        let separate = PeerPoolRouting::new(2, true).unwrap();
        for path in ["/v1/drives/a/fs/data", "/v1/drives/b/stat/data"] {
            for method in [http::Method::GET, http::Method::HEAD] {
                assert_eq!(separate.index(&method, path), 0);
            }
            for method in [
                http::Method::PUT,
                http::Method::POST,
                http::Method::DELETE,
                http::Method::PATCH,
            ] {
                assert_eq!(separate.index(&method, path), 1);
            }
        }
        let by_volume = PeerPoolRouting::new(4, false).unwrap();
        assert_eq!(
            by_volume.index(&http::Method::GET, "/v1/drives/a/fs/data"),
            by_volume.index(&http::Method::PUT, "/v1/drives/a/fs/other")
        );
    }

    #[test]
    fn typed_directory_validation_requires_complete_correct_entries() {
        let valid = json!([
            {"name":"data","parent_path":"/","file_type":"File"},
            {"name":"fixture-00000","parent_path":"/","file_type":"Directory"}
        ]);
        assert!(validate_directory_entries(&serde_json::to_vec(&valid).unwrap(), 1).is_ok());
        for name in [
            "fixture-0000",
            "fixture-000000",
            "fixture-00001",
            "fixture-0000x",
        ] {
            let invalid = json!([
                {"name":"data","parent_path":"/","file_type":"File"},
                {"name":name,"parent_path":"/","file_type":"Directory"}
            ]);
            assert!(validate_directory_entries(&serde_json::to_vec(&invalid).unwrap(), 1).is_err());
        }
        for invalid in [
            json!([{"name":"data","parent_path":"/","file_type":"File"}]),
            json!([
                {"name":"data","parent_path":"/","file_type":"File"},
                {"name":"fixture-00000","parent_path":"/","file_type":"File"}
            ]),
            json!([
                {"name":"data","parent_path":"/","file_type":"File"},
                {"name":"fixture-00000","parent_path":"/wrong","file_type":"Directory"}
            ]),
            json!([
                {"name":"data","parent_path":"/","file_type":"File"},
                {"name":"data","parent_path":"/","file_type":"File"},
                {"name":"fixture-00000","parent_path":"/","file_type":"Directory"}
            ]),
            json!([{"name":"data"},{"name":"fixture-00000"}]),
        ] {
            assert!(validate_directory_entries(&serde_json::to_vec(&invalid).unwrap(), 1).is_err());
        }
    }

    #[tokio::test]
    async fn truncated_http_headers_are_classified_without_request_details() {
        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                stream.read(&mut request).await.unwrap();
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length:")
                    .await
                    .unwrap();
                stream.shutdown().await.unwrap();
            });
            let error = reqwest::Client::new()
                .get(format!("http://{address}/private-path"))
                .bearer_auth("private-token")
                .send()
                .await
                .unwrap_err();
            assert_eq!(transport_error(error), "transport HTTP incomplete message");
            server.await.unwrap();
        })
        .await
        .unwrap();
    }
}
