//! Direct provider baseline for the same 100 x 32 unique 4 KiB blocks as the QUIC ramp.
//! The measured phases bypass QUIC, dispatch, and ChunkedFs operations.

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::diagnostics::profile;
use mount_rs_core::storage::{BlockId, BlockStore, MetadataStore};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
use mysql_async::{Pool, prelude::Queryable};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::Read,
    path::Path,
    process::Command,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::task::JoinSet;

const CLIENTS: usize = 100;
const BLOCKS_PER_CLIENT: usize = 32;
const BLOCK_BYTES: usize = 4096;
const PHASE_TIMEOUT: Duration = Duration::from_secs(300);

fn payload(seed: &[u8; 32], client: usize, block: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(BLOCK_BYTES);
    for segment in 0..BLOCK_BYTES / 32 {
        let mut hasher = Sha256::new();
        hasher.update(seed);
        hasher.update((client as u64).to_le_bytes());
        hasher.update((block as u64).to_le_bytes());
        hasher.update((segment as u64).to_le_bytes());
        bytes.extend_from_slice(&hasher.finalize());
    }
    bytes
}

fn observe(action: &str, mode: &str, stage: &str, successes: usize, failures: usize) {
    let Ok(observer) = std::env::var("MOUNT_RS_DATASTORE_STAGE_OBSERVER") else {
        return;
    };
    let status = Command::new(observer)
        .args([
            action,
            mode,
            &CLIENTS.to_string(),
            stage,
            &successes.to_string(),
            &failures.to_string(),
        ])
        .status()
        .expect("start TiDB stage observer");
    assert!(
        status.success(),
        "TiDB stage observer failed at {action} {stage}"
    );
}

fn stage_result(
    name: &str,
    successes: usize,
    bytes: usize,
    elapsed: Duration,
    mut latencies: Vec<Duration>,
) -> serde_json::Value {
    latencies.sort_unstable();
    let upper = |p: usize| {
        latencies
            .get((latencies.len().saturating_sub(1) * p) / 100)
            .copied()
            .unwrap_or_default()
            .as_micros()
    };
    json!({
        "stage":name, "clients":CLIENTS, "blocks_per_client":BLOCKS_PER_CLIENT,
        "payload_bytes":BLOCK_BYTES, "successes":successes, "errors":0, "counted_bytes":bytes,
        "elapsed_seconds":elapsed.as_secs_f64(), "ops_per_second": successes as f64 / elapsed.as_secs_f64(),
        "p50_us":upper(50), "p95_us":upper(95), "p99_us":upper(99)
    })
}

async fn collect<T: Send + 'static>(mut jobs: JoinSet<Result<T, String>>) -> (Vec<T>, Vec<String>) {
    let mut values = Vec::new();
    let mut errors = Vec::new();
    let outcome = tokio::time::timeout(PHASE_TIMEOUT, async {
        while let Some(result) = jobs.join_next().await {
            match result {
                Ok(Ok(value)) => values.push(value),
                Ok(Err(error)) => errors.push(error),
                Err(error) => errors.push(error.to_string()),
            }
        }
    })
    .await;
    if outcome.is_err() {
        jobs.abort_all();
        while jobs.join_next().await.is_some() {}
        errors.push("phase exceeded 300 seconds".into());
    }
    (values, errors)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 8)]
#[ignore = "requires disposable actual TiDB and MOUNT_RS_TIDB_URL"]
async fn direct_tidb_put_get_and_fresh_revision_baseline() {
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("actual TiDB URL required");
    let pool = Pool::from_url(&url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    let version: String = connection
        .query_first("SELECT VERSION()")
        .await
        .unwrap()
        .unwrap();
    assert!(
        version.to_ascii_lowercase().contains("tidb"),
        "actual TiDB required"
    );
    drop(connection);
    pool.disconnect().await.unwrap();

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let key = format!("tidb-direct-baseline-{}-{timestamp}", std::process::id());
    let stage_prefix = format!("direct-{}-{timestamp}", std::process::id());
    let options = TidbStorageOptions::new(key);
    let metadata = TidbMetadataStore::connect_with_options(&url, options.clone())
        .await
        .unwrap();
    let blocks = TidbBlockStore::connect_with_options(&url, options)
        .await
        .unwrap();
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("direct-baseline", BLOCK_BYTES).unwrap(),
    )
    .await
    .unwrap();
    let revision = metadata.load().await.unwrap().revision;
    assert!(revision > 0, "metadata baseline needs a published revision");
    let mut seed = [0_u8; 32];
    File::open("/dev/urandom")
        .unwrap()
        .read_exact(&mut seed)
        .unwrap();
    let payloads: Vec<_> = (0..CLIENTS)
        .map(|client| {
            Arc::new(
                (0..BLOCKS_PER_CLIENT)
                    .map(|block| payload(&seed, client, block))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let blocks = Arc::new(blocks);
    let mut stages = Vec::new();

    let stage = format!("{stage_prefix}-put");
    observe("begin", "write", &stage, 0, 0);
    let profile_before = profile::snapshot();
    let started = Instant::now();
    let mut jobs = JoinSet::new();
    for (client, values) in payloads.iter().cloned().enumerate() {
        let store = blocks.clone();
        jobs.spawn(async move {
            let mut ids = Vec::with_capacity(BLOCKS_PER_CLIENT);
            let mut latencies = Vec::with_capacity(BLOCKS_PER_CLIENT);
            for value in values.iter() {
                let one = Instant::now();
                ids.push(
                    store
                        .put(value)
                        .await
                        .map_err(|e| format!("client {client} put: {e:?}"))?,
                );
                latencies.push(one.elapsed());
            }
            Ok((client, ids, latencies))
        });
    }
    let (written, errors) = collect(jobs).await;
    let elapsed = started.elapsed();
    let io_profile = profile::snapshot().delta(&profile_before).unwrap();
    let successes: usize = written.iter().map(|(_, ids, _)| ids.len()).sum();
    observe("end", "write", &stage, successes, errors.len());
    assert!(errors.is_empty(), "direct put failures: {errors:?}");
    assert_eq!(successes, CLIENTS * BLOCKS_PER_CLIENT);
    let mut ids_by_client = vec![Vec::<BlockId>::new(); CLIENTS];
    let mut latencies = Vec::new();
    for (client, ids, measured) in written {
        ids_by_client[client] = ids;
        latencies.extend(measured);
    }
    let mut report = stage_result(
        "direct_block_put",
        successes,
        successes * BLOCK_BYTES,
        elapsed,
        latencies,
    );
    report["io_profile"] = serde_json::to_value(io_profile).unwrap();
    stages.push(report);

    let stage = format!("{stage_prefix}-get");
    observe("begin", "read", &stage, 0, 0);
    let profile_before = profile::snapshot();
    let started = Instant::now();
    let mut jobs = JoinSet::new();
    for (client, (values, ids)) in payloads
        .iter()
        .cloned()
        .zip(ids_by_client.into_iter())
        .enumerate()
    {
        let store = blocks.clone();
        jobs.spawn(async move {
            let mut latencies = Vec::with_capacity(BLOCKS_PER_CLIENT);
            for (id, expected) in ids.iter().zip(values.iter()) {
                let one = Instant::now();
                let actual = store
                    .get(id)
                    .await
                    .map_err(|e| format!("client {client} get: {e:?}"))?;
                if actual != *expected {
                    return Err(format!("client {client} direct block mismatch"));
                }
                latencies.push(one.elapsed());
            }
            Ok(latencies)
        });
    }
    let (read, errors) = collect(jobs).await;
    let elapsed = started.elapsed();
    let io_profile = profile::snapshot().delta(&profile_before).unwrap();
    let successes: usize = read.iter().map(Vec::len).sum();
    observe("end", "read", &stage, successes, errors.len());
    assert!(errors.is_empty(), "direct get failures: {errors:?}");
    assert_eq!(successes, CLIENTS * BLOCKS_PER_CLIENT);
    let mut report = stage_result(
        "direct_block_get",
        successes,
        successes * BLOCK_BYTES,
        elapsed,
        read.into_iter().flatten().collect(),
    );
    report["io_profile"] = serde_json::to_value(io_profile).unwrap();
    stages.push(report);

    let stage = format!("{stage_prefix}-revision");
    observe("begin", "read", &stage, 0, 0);
    let profile_before = profile::snapshot();
    let started = Instant::now();
    let mut jobs = JoinSet::new();
    for client in 0..CLIENTS {
        let store = metadata.clone();
        jobs.spawn(async move {
            let mut latencies = Vec::with_capacity(BLOCKS_PER_CLIENT);
            for _ in 0..BLOCKS_PER_CLIENT {
                let one = Instant::now();
                let result = store
                    .load_if_changed(revision)
                    .await
                    .map_err(|e| format!("client {client} metadata: {e:?}"))?;
                if result.is_some() {
                    return Err(format!("client {client} metadata unexpectedly changed"));
                }
                latencies.push(one.elapsed());
            }
            Ok(latencies)
        });
    }
    let (loaded, errors) = collect(jobs).await;
    let elapsed = started.elapsed();
    let io_profile = profile::snapshot().delta(&profile_before).unwrap();
    let successes: usize = loaded.iter().map(Vec::len).sum();
    observe("end", "read", &stage, successes, errors.len());
    assert!(errors.is_empty(), "revision baseline failures: {errors:?}");
    assert_eq!(successes, CLIENTS * BLOCKS_PER_CLIENT);
    let mut report = stage_result(
        "direct_fresh_revision_load",
        successes,
        0,
        elapsed,
        loaded.into_iter().flatten().collect(),
    );
    report["io_profile"] = serde_json::to_value(io_profile).unwrap();
    stages.push(report);

    filesystem.shutdown().await.unwrap();
    metadata.close().await.unwrap();
    blocks.close().await.unwrap();
    let artifact = json!({"kind":"actual_tidb_direct_baseline", "stages":stages});
    if let Ok(path) = std::env::var("MOUNT_RS_TIDB_DIRECT_BASELINE_OUTPUT") {
        let base = Path::new(&path);
        let unique = base.with_file_name(format!(
            "{}-{stage_prefix}.json",
            base.file_stem().unwrap().to_string_lossy()
        ));
        let encoded = serde_json::to_vec_pretty(&artifact).unwrap();
        std::fs::write(base, &encoded).unwrap();
        std::fs::write(&unique, encoded).unwrap();
        println!(
            "TIDB_DIRECT_BASELINE_OUTPUT latest={path} unique={}",
            unique.display()
        );
    }
    println!("TIDB_DIRECT_BASELINE_PASS {artifact}");
}
