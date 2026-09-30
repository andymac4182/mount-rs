//! Bounded actual-TiDB diagnostics, not capacity or dataset-absence qualification.
//!
//! The read-only selector never changes settings or selects SQL text. The owned
//! write selector requires an explicitly supplied prefix and loopback metrics
//! endpoint. It retains its exact metadata scopes for the caller's independently
//! owned cleanup; successful pool closure does not establish dataset absence.

#[path = "support/guard_fixture.rs"]
mod guard_fixture;

use mount_rs_core::storage::compact::{
    CompactOptimisticCreateCapability, CompactRootFileCreate, CompactStructuralDelta,
    StructuralScope,
};
use mount_rs_core::storage::{
    ConcurrentBackingId, DirectoryEntry, MetadataStore, Namespace, NodeData,
};
use mount_rs_tidb::{TidbPoolContext, TidbStorageOptions};
use mysql_async::{Opts, Pool, Row, prelude::Queryable};
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::future::Future;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

type Diagnostic<T> = Result<T, &'static str>;
const SQL_TIMEOUT: Duration = Duration::from_secs(60);
const FIXTURE_TIMEOUT: Duration = Duration::from_secs(180);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HTTP_BYTES: usize = 8 * 1024 * 1024;
const METRIC: &str = "tidb_tikvclient_txn_write_kv_num";

async fn bounded<T, E>(
    limit: Duration,
    future: impl Future<Output = Result<T, E>>,
) -> Diagnostic<T> {
    timeout(limit, future)
        .await
        .map_err(|_| "bounded operation timed out")?
        .map_err(|_| "bounded operation failed; error detail redacted")
}

fn required(name: &str) -> Diagnostic<String> {
    std::env::var(name).map_err(|_| "explicit diagnostic environment input required")
}

fn bounded_number(name: &str, default: u64, maximum: u64) -> Diagnostic<u64> {
    let value = match std::env::var(name) {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| "invalid numeric diagnostic input")?,
        Err(std::env::VarError::NotPresent) => default,
        Err(_) => return Err("invalid numeric diagnostic input"),
    };
    if value == 0 || value > maximum {
        return Err("numeric diagnostic input outside bounded range");
    }
    Ok(value)
}

fn numeric(row: &Row, column: usize) -> Diagnostic<Option<u64>> {
    row.get_opt(column)
        .ok_or("missing numeric diagnostic column")?
        .map_err(|_| "invalid numeric diagnostic column")
}

fn seconds(row: &Row, column: usize) -> Diagnostic<Option<f64>> {
    let value: Option<f64> = row
        .get_opt(column)
        .ok_or("missing duration diagnostic column")?
        .map_err(|_| "invalid duration diagnostic column")?;
    if value.is_some_and(|value| !value.is_finite() || value < 0.0) {
        return Err("nonfinite or negative diagnostic duration");
    }
    Ok(value)
}

fn tidb_version_digits(version: &str) -> Diagnostic<[u64; 3]> {
    let suffix = version
        .split_once("TiDB-v")
        .ok_or("endpoint did not identify itself as TiDB")?
        .1;
    let mut components = suffix.split('.');
    let mut result = [0; 3];
    for component in &mut result {
        let digits: String = components
            .next()
            .ok_or("missing numeric TiDB version component")?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        *component = digits
            .parse()
            .map_err(|_| "invalid numeric TiDB version component")?;
    }
    Ok(result)
}

#[derive(Serialize)]
struct SlowCommit {
    timestamp_unix_seconds: Option<f64>,
    txn_start_ts: Option<u64>,
    connection_id: Option<u64>,
    query_seconds: Option<f64>,
    prewrite_seconds: Option<f64>,
    commit_seconds: Option<f64>,
    get_commit_ts_seconds: Option<f64>,
    commit_backoff_seconds: Option<f64>,
    resolve_lock_seconds: Option<f64>,
    local_latch_wait_seconds: Option<f64>,
    lock_keys_seconds: Option<f64>,
    write_keys: Option<u64>,
    write_bytes: Option<u64>,
    prewrite_regions: Option<u64>,
    txn_retries: Option<u64>,
    execution_retries: Option<u64>,
    success: Option<u64>,
    explicit_transaction: Option<u64>,
}

async fn slow_capture(pool: &Pool) -> Diagnostic<serde_json::Value> {
    let lookback = bounded_number("MOUNT_RS_TIDB_DIAGNOSTIC_LOOKBACK_SECONDS", 21_600, 86_400)?;
    let row_limit = bounded_number("MOUNT_RS_TIDB_DIAGNOSTIC_ROW_LIMIT", 24, 128)?;
    let minimum_keys = bounded_number("MOUNT_RS_TIDB_DIAGNOSTIC_MIN_WRITE_KEYS", 100, 10_000_000)?;
    let mut connection = bounded(SQL_TIMEOUT, pool.get_conn()).await?;
    let settings: Option<(String, u64, u64, u64, u64, u64)> = bounded(
        SQL_TIMEOUT,
        connection.query_first(
            "SELECT VERSION(), CAST(@@GLOBAL.tidb_enable_slow_log AS UNSIGNED), \
             CAST(@@GLOBAL.tidb_slow_log_threshold AS UNSIGNED), \
             CAST(@@GLOBAL.tidb_record_plan_in_slow_log AS UNSIGNED), \
             CAST(@@GLOBAL.tidb_enable_collect_execution_info AS UNSIGNED), \
             CAST((@@SESSION.tidb_slow_log_rules <> '') AS UNSIGNED)",
        ),
    )
    .await?;
    let (version, enabled, threshold, plan, execution_info, rules_present) =
        settings.ok_or("slow-log configuration returned no row")?;
    let version_digits = tidb_version_digits(&version)?;
    if [enabled, plan, execution_info, rules_present]
        .iter()
        .any(|value| *value > 1)
    {
        return Err("nonboolean slow-log configuration witness");
    }
    // Restrict schema output to these fixed, known numeric fields and QUERY's
    // availability. QUERY is used only in a predicate and is never returned.
    let columns: Vec<(String, String)> = bounded(
        SQL_TIMEOUT,
        connection.query(
            "SELECT COLUMN_NAME, DATA_TYPE FROM INFORMATION_SCHEMA.COLUMNS \
             WHERE TABLE_SCHEMA='INFORMATION_SCHEMA' AND TABLE_NAME='SLOW_QUERY' \
             AND COLUMN_NAME IN ('TIME','TXN_START_TS','CONN_ID','QUERY_TIME', \
             'PREWRITE_TIME','COMMIT_TIME','GET_COMMIT_TS_TIME','COMMIT_BACKOFF_TIME', \
             'RESOLVE_LOCK_TIME','LOCAL_LATCH_WAIT_TIME','WRITE_KEYS', \
             'WRITE_SIZE','PREWRITE_REGION','TXN_RETRY','EXEC_RETRY_COUNT','SUCC', \
             'IS_INTERNAL','QUERY') ORDER BY COLUMN_NAME LIMIT 18",
        ),
    )
    .await?;
    let schema: Vec<_> = columns
        .iter()
        .map(|(name, data_type)| {
            json!({"column": name, "numeric": matches!(data_type.as_str(),
                "bigint" | "int" | "tinyint" | "double" | "float" | "decimal")})
        })
        .collect();
    if columns.len() != 18 {
        return Ok(json!({
            "schema": "mount-rs.compact-commit-slow-diagnostic.v1",
            "tidb_version_digits": version_digits,
            "available": false,
            "reason": "required slow-query columns unavailable",
            "numeric_column_witnesses": schema,
        }));
    }
    let rows: Vec<Row> = bounded(
        SQL_TIMEOUT,
        connection.exec(
            "SELECT CAST(UNIX_TIMESTAMP(TIME) AS DOUBLE), TXN_START_TS, CONN_ID, \
             QUERY_TIME, PREWRITE_TIME, COMMIT_TIME, GET_COMMIT_TS_TIME, \
             COMMIT_BACKOFF_TIME, RESOLVE_LOCK_TIME, LOCAL_LATCH_WAIT_TIME, \
             CAST(NULL AS DOUBLE), WRITE_KEYS, WRITE_SIZE, PREWRITE_REGION, TXN_RETRY, \
             EXEC_RETRY_COUNT, SUCC, CAST(NULL AS UNSIGNED) \
             FROM INFORMATION_SCHEMA.SLOW_QUERY \
             WHERE TIME >= DATE_SUB(NOW(), INTERVAL ? SECOND) \
             AND UPPER(TRIM(QUERY)) IN ('COMMIT','COMMIT;') \
             AND WRITE_KEYS >= ? AND IS_INTERNAL=0 ORDER BY TIME DESC LIMIT ?",
            (lookback, minimum_keys, row_limit),
        ),
    )
    .await?;
    let mut commits = Vec::with_capacity(rows.len());
    for row in rows {
        commits.push(SlowCommit {
            timestamp_unix_seconds: seconds(&row, 0)?,
            txn_start_ts: numeric(&row, 1)?,
            connection_id: numeric(&row, 2)?,
            query_seconds: seconds(&row, 3)?,
            prewrite_seconds: seconds(&row, 4)?,
            commit_seconds: seconds(&row, 5)?,
            get_commit_ts_seconds: seconds(&row, 6)?,
            commit_backoff_seconds: seconds(&row, 7)?,
            resolve_lock_seconds: seconds(&row, 8)?,
            local_latch_wait_seconds: seconds(&row, 9)?,
            lock_keys_seconds: seconds(&row, 10)?,
            write_keys: numeric(&row, 11)?,
            write_bytes: numeric(&row, 12)?,
            prewrite_regions: numeric(&row, 13)?,
            txn_retries: numeric(&row, 14)?,
            execution_retries: numeric(&row, 15)?,
            success: numeric(&row, 16)?,
            explicit_transaction: numeric(&row, 17)?,
        });
    }
    Ok(json!({
        "schema": "mount-rs.compact-commit-slow-diagnostic.v1",
        "tidb_version_digits": version_digits,
        "slow_log_enabled": enabled == 1,
        "slow_log_threshold_ms": threshold,
        "slow_log_plan_recording": plan == 1,
        "execution_info_collection": execution_info == 1,
        "session_slow_log_rules_present": rules_present == 1,
        "numeric_column_witnesses": schema,
        "optional_fields_unavailable": ["lock_keys_seconds", "explicit_transaction"],
        "lookback_seconds": lookback,
        "row_limit": row_limit,
        "minimum_write_keys": minimum_keys,
        "available": !commits.is_empty(),
        "rows": commits,
        "attribution": "retained general COMMIT rows; no owned-transaction attribution",
        "absence_means_zero": false,
    }))
}

#[tokio::test]
#[ignore = "read-only actual TiDB diagnostic; explicit URL required"]
async fn actual_compact_commit_slow_rows_read_only() {
    let url = required("MOUNT_RS_TIDB_URL").expect("explicit actual TiDB URL required");
    let opts = Opts::from_url(&url)
        .map_err(|_| "invalid actual TiDB URL; details redacted")
        .expect("valid actual TiDB URL required");
    let pool = Pool::new(opts);
    let captured = slow_capture(&pool).await;
    let closed = bounded(SQL_TIMEOUT, pool.disconnect()).await;
    println!(
        "STRUCTURAL_DIAGNOSTIC {}",
        json!({
            "capture": captured.as_ref().ok(),
            "failure": captured.as_ref().err(),
            "pool_closed": closed.is_ok(),
            "close_failure": closed.as_ref().err(),
        })
    );
    assert!(closed.is_ok(), "read-only diagnostic pool did not close");
    assert!(
        captured.is_ok(),
        "read-only diagnostic unavailable; details redacted"
    );
}

#[derive(Clone, Copy, Debug, Serialize)]
struct GeneralHistogram {
    sum: u64,
    count: u64,
}

fn exact_counter(value: &str) -> Diagnostic<u64> {
    let number = value
        .parse::<f64>()
        .map_err(|_| "invalid histogram number")?;
    if !number.is_finite()
        || number < 0.0
        || number.fract() != 0.0
        || number > 9_007_199_254_740_991.0
    {
        return Err("histogram number outside exact integer range");
    }
    Ok(number as u64)
}

fn general_histogram(body: &str) -> Diagnostic<GeneralHistogram> {
    let mut histogram_type = false;
    let mut sum = None;
    let mut count = None;
    for line in body.lines() {
        if line == format!("# TYPE {METRIC} histogram") {
            histogram_type = true;
        }
        let mut fields = line.split_whitespace();
        let Some(key) = fields.next() else { continue };
        if key.starts_with('#') {
            continue;
        }
        for (suffix, slot) in [("sum", &mut sum), ("count", &mut count)] {
            let expected = format!("{METRIC}_{suffix}{{scope=\"general\"}}");
            if key == expected {
                let value = exact_counter(fields.next().ok_or("histogram sample lacks value")?)?;
                if fields.next().is_some() || slot.replace(value).is_some() {
                    return Err("duplicate or timestamped general histogram sample");
                }
            } else if key.starts_with(&format!("{METRIC}_{suffix}"))
                && key.contains("scope=\"general\"")
            {
                return Err("general histogram series labels differ from required stable key");
            }
        }
    }
    if !histogram_type {
        return Err("required transaction metric is not a histogram");
    }
    Ok(GeneralHistogram {
        sum: sum.ok_or("general histogram sum unavailable")?,
        count: count.ok_or("general histogram count unavailable")?,
    })
}

fn mutation_keys(before: GeneralHistogram, after: GeneralHistogram) -> Diagnostic<u64> {
    if after.count.checked_sub(before.count) != Some(1) {
        return Err("unavailable: general transaction count delta is not exactly one");
    }
    let keys = after
        .sum
        .checked_sub(before.sum)
        .ok_or("unavailable: general transaction sum reset")?;
    if keys == 0 {
        return Err("unavailable: observed transaction has no mutation keys");
    }
    Ok(keys)
}

fn metrics_url() -> Diagnostic<url::Url> {
    let url = url::Url::parse(&required("MOUNT_RS_TIDB_STATUS_METRICS")?)
        .map_err(|_| "invalid metrics URL; details redacted")?;
    if url.scheme() != "http"
        || !matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "::1"))
        || url.port().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/metrics"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("metrics URL must be explicit uncredentialed loopback HTTP /metrics");
    }
    Ok(url)
}

fn http_body(response: &[u8]) -> Diagnostic<String> {
    let boundary = response
        .windows(4)
        .position(|bytes| bytes == b"\r\n\r\n")
        .ok_or("metrics response lacks header boundary")?;
    if boundary > 65_536 {
        return Err("metrics response headers exceed bound");
    }
    let headers = std::str::from_utf8(&response[..boundary])
        .map_err(|_| "invalid metrics response headers")?;
    let mut lines = headers.split("\r\n");
    if !matches!(lines.next(), Some("HTTP/1.1 200 OK" | "HTTP/1.0 200 OK")) {
        return Err("metrics response status is not 200");
    }
    let mut chunked = false;
    let mut length = None;
    for line in lines {
        let (name, value) = line.split_once(':').ok_or("invalid metrics header")?;
        match name.to_ascii_lowercase().as_str() {
            "transfer-encoding" if value.trim().eq_ignore_ascii_case("chunked") => chunked = true,
            "transfer-encoding" => return Err("unsupported metrics transfer encoding"),
            "content-encoding" if !value.trim().eq_ignore_ascii_case("identity") => {
                return Err("compressed metrics response refused");
            }
            "content-length" => {
                let value = value
                    .trim()
                    .parse::<usize>()
                    .map_err(|_| "invalid metrics content length")?;
                if length.replace(value).is_some() {
                    return Err("duplicate metrics content length");
                }
            }
            _ => {}
        }
    }
    let encoded = &response[boundary + 4..];
    let body = if chunked {
        if length.is_some() {
            return Err("ambiguous metrics framing");
        }
        let mut remaining = encoded;
        let mut decoded = Vec::new();
        loop {
            let end = remaining
                .windows(2)
                .position(|bytes| bytes == b"\r\n")
                .ok_or("truncated metrics chunk header")?;
            let size = std::str::from_utf8(&remaining[..end])
                .map_err(|_| "invalid metrics chunk header")?;
            // Extensions/trailers are unnecessary for the fixed Go /metrics endpoint.
            let size =
                usize::from_str_radix(size, 16).map_err(|_| "invalid metrics chunk length")?;
            remaining = &remaining[end + 2..];
            if size == 0 {
                if remaining != b"\r\n" {
                    return Err("unsupported metrics trailer");
                }
                break;
            }
            if size > MAX_HTTP_BYTES.saturating_sub(decoded.len())
                || size
                    .checked_add(2)
                    .is_none_or(|framed| framed > remaining.len())
                || &remaining[size..size + 2] != b"\r\n"
            {
                return Err("invalid or oversized metrics chunk");
            }
            decoded.extend_from_slice(&remaining[..size]);
            remaining = &remaining[size + 2..];
        }
        decoded
    } else {
        if length.is_some_and(|length| length != encoded.len()) {
            return Err("metrics content length mismatch");
        }
        encoded.to_vec()
    };
    String::from_utf8(body).map_err(|_| "metrics body is not UTF-8")
}

async fn capture_histogram(url: &url::Url) -> Diagnostic<GeneralHistogram> {
    let host = url.host_str().ok_or("metrics host unavailable")?;
    let host = host.trim_matches(['[', ']']);
    let port = url.port().ok_or("metrics port unavailable")?;
    timeout(HTTP_TIMEOUT, async {
        let mut socket = TcpStream::connect((host, port)).await
            .map_err(|_| "metrics connection failed; details redacted")?;
        let request = format!("GET /metrics HTTP/1.1\r\nHost: {}:{port}\r\nAccept: text/plain\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n", url.host_str().unwrap_or(""));
        socket.write_all(request.as_bytes()).await.map_err(|_| "metrics request failed")?;
        let mut response = Vec::new();
        let mut bounded_reader = socket.take((MAX_HTTP_BYTES + 1) as u64);
        bounded_reader.read_to_end(&mut response).await.map_err(|_| "metrics response failed")?;
        if response.len() > MAX_HTTP_BYTES { return Err("metrics response exceeds bound"); }
        general_histogram(&http_body(&response)?)
    }).await.map_err(|_| "metrics capture timed out")?
}

fn namespace_with_files(files: u64) -> Namespace {
    let template = guard_fixture::namespace();
    let mut namespace = template.clone();
    let mut file = template.nodes[&2].clone();
    namespace.nodes.retain(|inode, _| *inode == namespace.root);
    let mut entries = Vec::with_capacity(files as usize);
    for inode in 2..files + 2 {
        file.stats.ino = inode;
        namespace.nodes.insert(inode, file.clone());
        entries.push(DirectoryEntry {
            name: format!("f{inode}"),
            inode,
        });
    }
    namespace.nodes.get_mut(&namespace.root).unwrap().data = NodeData::Directory { entries };
    namespace.next_inode = files + 2;
    namespace
}

#[derive(Serialize)]
struct CreateObservation {
    existing_files: u64,
    owned_key_sha256: String,
    before: GeneralHistogram,
    after: GeneralHistogram,
    publication_acknowledged: bool,
    provider_context_closed: bool,
    fresh_complete_namespace_equal: bool,
    fresh_context_closed: bool,
    observed_mutation_keys: Option<u64>,
    measurement_unavailable: Option<&'static str>,
}

async fn create_observation(
    url: &str,
    metrics: &url::Url,
    key: &str,
    files: u64,
) -> Diagnostic<CreateObservation> {
    let context =
        TidbPoolContext::new(url, 1).map_err(|_| "provider context construction failed")?;
    let captured = timeout(FIXTURE_TIMEOUT, async {
        if !context
            .inspect_namespace_presence(key)
            .await
            .map_err(|_| "owned scope inspection failed")?
            .is_absent()
        {
            return Err("owned diagnostic scope was not empty");
        }
        let store = context
            .metadata(TidbStorageOptions::new(key))
            .await
            .map_err(|_| "owned metadata store open failed")?;
        if store.compact_optimistic_create_capability()
            != CompactOptimisticCreateCapability::Supported
        {
            return Err("atomic audited-create capability unavailable");
        }
        let backing = ConcurrentBackingId::from_bytes([0x74; 16])
            .map_err(|_| "invalid diagnostic backing identity")?;
        let mut expected = namespace_with_files(files);
        store
            .prepare_bound_concurrent_mode(backing)
            .await
            .map_err(|_| "owned bound enrollment failed")?;
        store
            .publish_bound_if_revision(backing, 0, namespace_with_files(0))
            .await
            .map_err(|_| "owned empty namespace publication failed; no replay")?;
        store
            .prepare_compact_inode_mode(backing, 1)
            .await
            .map_err(|_| "owned compact enrollment failed")?;
        let empty = store
            .load_compact_snapshot(backing)
            .await
            .map_err(|_| "owned empty compact snapshot failed")?;
        let population = CompactStructuralDelta::capture(&empty, &expected, StructuralScope::Full)
            .map_err(|_| "owned full population capture failed")?;
        store
            .publish_compact_structure(&population)
            .await
            .map_err(|_| "owned full population publication failed; no replay")?;
        let snapshot = store
            .load_compact_snapshot(backing)
            .await
            .map_err(|_| "owned compact snapshot failed")?;
        let (_, _, audited) = snapshot
            .clone()
            .into_validated_namespace()
            .map_err(|_| "owned complete audit failed")?;
        let root = &snapshot.guards[&snapshot.anchor.root];
        let mut created = expected.nodes[&2].clone();
        created.stats.ino = expected.next_inode;
        let name = "diagnostic-created".to_owned();
        let mtime = root
            .node
            .stats
            .mtime_ms
            .checked_add(1)
            .ok_or("diagnostic time overflow")?;
        let ctime = root
            .node
            .stats
            .ctime_ms
            .checked_add(1)
            .ok_or("diagnostic time overflow")?;
        let proposal = CompactRootFileCreate::capture_audited(
            &audited,
            root.identity,
            name.clone(),
            created.clone(),
            mtime,
            ctime,
        )
        .map_err(|_| "owned audited create capture failed")?;
        let before = capture_histogram(metrics).await?;
        // Exactly one provider publication, with no oracle query inside this window.
        let publication = store
            .publish_compact_structure(proposal.delta())
            .await
            .map_err(|_| "owned structural publication failed; outcome not replayed")?;
        let after = capture_histogram(metrics).await?;
        proposal
            .validate_publication(&publication)
            .map_err(|_| "owned publication receipt invalid")?;
        let inode = expected.next_inode;
        expected.nodes.insert(inode, created);
        expected.next_inode += 1;
        let parent = expected
            .nodes
            .get_mut(&expected.root)
            .ok_or("owned parent absent")?;
        let NodeData::Directory { entries } = &mut parent.data else {
            return Err("owned parent is not a directory");
        };
        entries.push(DirectoryEntry { name, inode });
        parent.stats.mtime_ms = mtime;
        parent.stats.ctime_ms = ctime;
        store
            .close()
            .await
            .map_err(|_| "owned metadata store close failed")?;
        Ok((backing, expected, before, after))
    })
    .await
    .map_err(|_| "owned fixture operation timed out; scope retained")
    .and_then(|result| result);
    let closed = bounded(SQL_TIMEOUT, context.close()).await;
    if closed.is_err() {
        return Err("owned provider context close unproven; scope retained");
    }
    let (backing, expected, before, after) = captured?;
    // Reconnect after complete pool shutdown; validate all guards/members/dentries.
    let fresh = TidbPoolContext::new(url, 1).map_err(|_| "fresh context construction failed")?;
    let validated = timeout(FIXTURE_TIMEOUT, async {
        let store = fresh
            .metadata(TidbStorageOptions::new(key))
            .await
            .map_err(|_| "fresh metadata open failed")?;
        let snapshot = store
            .load_compact_snapshot(backing)
            .await
            .map_err(|_| "fresh complete snapshot failed")?;
        let (actual, _, _) = snapshot
            .into_validated_namespace()
            .map_err(|_| "fresh complete namespace audit failed")?;
        let actual = serde_json::to_vec(&actual).map_err(|_| "fresh namespace encoding failed")?;
        let expected =
            serde_json::to_vec(&expected).map_err(|_| "expected namespace encoding failed")?;
        if actual != expected {
            return Err("fresh complete namespace differs from acknowledged create");
        }
        store
            .close()
            .await
            .map_err(|_| "fresh metadata close failed")?;
        Ok(())
    })
    .await
    .map_err(|_| "fresh complete oracle timed out; scope retained")
    .and_then(|result| result);
    let fresh_closed = bounded(SQL_TIMEOUT, fresh.close()).await;
    if fresh_closed.is_err() {
        return Err("fresh context close unproven; scope retained");
    }
    validated?;
    let measured = mutation_keys(before, after);
    Ok(CreateObservation {
        existing_files: files,
        owned_key_sha256: format!("{:x}", Sha256::digest(key.as_bytes())),
        before,
        after,
        publication_acknowledged: true,
        provider_context_closed: true,
        fresh_complete_namespace_equal: true,
        fresh_context_closed: true,
        observed_mutation_keys: measured.as_ref().ok().copied(),
        measurement_unavailable: measured.err(),
    })
}

#[tokio::test]
#[ignore = "owned actual TiDB F4/F1000 mutation-key diagnostic; explicit URL/prefix/metrics required"]
async fn actual_compact_create_commit_key_fanout_f4_f1000() {
    let inputs = (|| -> Diagnostic<_> {
        let url = required("MOUNT_RS_TIDB_URL")?;
        Opts::from_url(&url).map_err(|_| "invalid actual TiDB URL; details redacted")?;
        let prefix = required("MOUNT_RS_TIDB_DIAGNOSTIC_PREFIX")?;
        if prefix.is_empty()
            || prefix.len() > 96
            || !prefix
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("owned prefix must contain 1..=96 ASCII alphanumeric or hyphen bytes");
        }
        let metrics = metrics_url()?;
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "diagnostic clock precedes epoch")?
            .as_nanos();
        Ok((url, prefix, metrics, nonce))
    })()
    .expect("explicit validated diagnostic inputs required; details redacted");
    let (url, prefix, metrics, nonce) = inputs;
    let pid = std::process::id();
    let mut observations = Vec::new();
    let mut failures = Vec::new();
    // Both owned cases settle independently, even if the first window is polluted.
    for files in [4, 1_000] {
        let key = format!("{prefix}-commit-f{files}-{pid}-{nonce}");
        match create_observation(&url, &metrics, &key, files).await {
            Ok(observation) => observations.push(observation),
            Err(reason) => failures.push(json!({"existing_files": files, "reason": reason})),
        }
    }
    println!(
        "STRUCTURAL_DIAGNOSTIC {}",
        json!({
            "schema": "mount-rs.compact-create-mutation-key-diagnostic.v1",
            "pid": pid,
            "nonce_unix_nanoseconds": nonce.to_string(),
            "owned_key_derivation": "explicit-prefix + '-commit-f' + files + '-' + pid + '-' + nonce",
            "metric_sum": "tidb_tikvclient_txn_write_kv_num_sum{scope=\"general\"}",
            "metric_count": "tidb_tikvclient_txn_write_kv_num_count{scope=\"general\"}",
            "observations": observations,
            "failures": failures,
            "metric_scope": "shared TiDB process; exactly one general observation required per window",
            "keys_meaning": "assembled transaction mutation keys, including lock-only keys, before commit ACK",
            "transaction_identity_proven": false,
            "dataset_retained_for_owned_cleanup": true,
            "dataset_absence_proven": false,
        })
    );
    assert!(
        failures.is_empty(),
        "owned fixture or fresh oracle failed; no replay; retained scope"
    );
    assert_eq!(observations.len(), 2, "both bounded cases must be observed");
    let small = observations[0]
        .observed_mutation_keys
        .expect("F4 metric window unavailable");
    let large = observations[1]
        .observed_mutation_keys
        .expect("F1000 metric window unavailable");
    assert_eq!(
        large, small,
        "structural create mutation-key fanout should be independent of untouched siblings"
    );
}

#[test]
fn histogram_diagnostic_refuses_contamination_resets_and_missing_samples() {
    let body = "# TYPE tidb_tikvclient_txn_write_kv_num histogram\n\
                tidb_tikvclient_txn_write_kv_num_sum{scope=\"general\"} 100\n\
                tidb_tikvclient_txn_write_kv_num_count{scope=\"general\"} 10\n\
                tidb_tikvclient_txn_write_kv_num_sum{scope=\"internal\"} 999\n\
                tidb_tikvclient_txn_write_kv_num_count{scope=\"internal\"} 999\n";
    let before = general_histogram(body).unwrap();
    assert_eq!(
        mutation_keys(
            before,
            GeneralHistogram {
                sum: 112,
                count: 11
            }
        ),
        Ok(12)
    );
    for after in [
        GeneralHistogram {
            sum: 112,
            count: 10,
        },
        GeneralHistogram {
            sum: 112,
            count: 12,
        },
        GeneralHistogram { sum: 99, count: 11 },
        GeneralHistogram {
            sum: 100,
            count: 11,
        },
    ] {
        assert!(mutation_keys(before, after).is_err());
    }
    assert!(general_histogram("").is_err());
    assert!(general_histogram(&format!("{body}{body}")).is_err());
    assert!(
        general_histogram(&body.replace("scope=\"general\"", "scope=\"general\",extra=\"x\""))
            .is_err()
    );
    assert!(exact_counter("9007199254740992").is_err());
    assert!(exact_counter("NaN").is_err());
}
