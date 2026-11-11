//! Throughput and runtime-stall measurement for the SQLite catalog pool.
//!
//! Run with:
//! `./scripts/cargo-shared test --release --locked -p mount-rs-service --test catalog_bench -- --ignored --nocapture`

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use mount_rs_service::catalog::{
    CatalogError, CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition,
    Permission, SqliteCatalog,
};
use serde_json::json;

const RUN: Duration = Duration::from_secs(2);
const PROBE_SLEEP: Duration = Duration::from_millis(1);

fn seed() -> CatalogSnapshot {
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.issuer_policies.insert(
        "issuer".into(),
        json!({"issuer": "https://issuer.example.com", "audiences": ["mount-rs"]}),
    );
    for partition in 0..32 {
        let id = format!("partition-{partition}");
        snapshot.partitions.insert(
            id.clone(),
            PartitionDefinition {
                drives: BTreeMap::from([(
                    "data".into(),
                    DriveDefinition {
                        driver: json!({"kind": "memory"}),
                    },
                )]),
            },
        );
        snapshot.grants.insert(
            format!("grant-{partition}"),
            GrantDefinition {
                partition_id: id,
                policy_id: "issuer".into(),
                drives: BTreeMap::from([("data".into(), Permission::Read)]),
                claim_conditions: BTreeMap::from([("/sub".into(), format!("w-{partition}"))]),
            },
        );
    }
    snapshot
}

fn percentile(sorted: &[Duration], percent: usize) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    sorted[((sorted.len() - 1) * percent) / 100]
}

#[derive(Clone, Copy)]
enum Operation {
    Load,
    CompareAndSwap,
}

/// Runs `callers` tasks for [`RUN`] and prints throughput, per-call latency,
/// how late an unrelated 1 ms timer task on the same runtime woke up, and how
/// long an empty task spawned by it waited to run.
async fn measure(name: &str, catalog: &SqliteCatalog, operation: Operation, callers: usize) {
    let stop = Arc::new(AtomicBool::new(false));
    let probe_stop = Arc::clone(&stop);
    let probe = tokio::spawn(async move {
        let mut late = Vec::new();
        let mut hops = Vec::new();
        while !probe_stop.load(Ordering::Relaxed) {
            let started = Instant::now();
            tokio::time::sleep(PROBE_SLEEP).await;
            late.push(started.elapsed().saturating_sub(PROBE_SLEEP));
            // How long a new, empty task waits for a runtime worker.
            let started = Instant::now();
            tokio::spawn(async {}).await.unwrap();
            hops.push(started.elapsed());
        }
        (late, hops)
    });
    let started = Instant::now();
    let mut tasks = Vec::new();
    for _ in 0..callers {
        let catalog = catalog.clone();
        let stop = Arc::clone(&stop);
        tasks.push(tokio::spawn(async move {
            let mut latencies = Vec::new();
            let mut swapped = 0_u64;
            let document = seed();
            let mut expected = catalog.load_shared_current().await.unwrap().revision;
            while !stop.load(Ordering::Relaxed) {
                let call = Instant::now();
                match operation {
                    Operation::Load => {
                        std::hint::black_box(catalog.load_shared_current().await.unwrap());
                        latencies.push(call.elapsed());
                    }
                    Operation::CompareAndSwap => {
                        let outcome = catalog.compare_and_swap(expected, document.clone()).await;
                        latencies.push(call.elapsed());
                        match outcome {
                            Ok(revision) => {
                                expected = revision;
                                swapped += 1;
                            }
                            Err(CatalogError::Conflict) => {
                                expected = catalog.load_shared_current().await.unwrap().revision;
                            }
                            Err(error) => panic!("compare-and-swap failed: {error:?}"),
                        }
                    }
                }
            }
            (latencies, swapped)
        }));
    }
    tokio::time::sleep(RUN).await;
    stop.store(true, Ordering::Relaxed);
    let mut latencies = Vec::new();
    let mut swapped = 0;
    for task in tasks {
        let (mut task_latencies, task_swapped) = task.await.unwrap();
        latencies.append(&mut task_latencies);
        swapped += task_swapped;
    }
    let elapsed = started.elapsed();
    let (mut late, mut hops) = probe.await.unwrap();
    latencies.sort_unstable();
    late.sort_unstable();
    hops.sort_unstable();
    println!(
        "{name:<10} callers={callers:<2} ops/s={:>9.0} swapped/s={:>7.0} p50={:>9.1?} p99={:>9.1?} | \
         timer late p50={:>8.1?} p99={:>8.1?} max={:>8.1?} | \
         empty task p50={:>8.1?} p99={:>8.1?} max={:>8.1?}",
        latencies.len() as f64 / elapsed.as_secs_f64(),
        swapped as f64 / elapsed.as_secs_f64(),
        percentile(&latencies, 50),
        percentile(&latencies, 99),
        percentile(&late, 50),
        percentile(&late, 99),
        late.last().copied().unwrap_or_default(),
        percentile(&hops, 50),
        percentile(&hops, 99),
        hops.last().copied().unwrap_or_default(),
    );
}

#[test]
#[ignore = "benchmark: run with --release --ignored --nocapture"]
fn catalog_pool_throughput_and_runtime_stall() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let directory = tempfile::tempdir().unwrap();
        let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap();
        catalog.compare_and_swap(0, seed()).await.unwrap();
        for callers in [1, 16] {
            measure("load", &catalog, Operation::Load, callers).await;
        }
        for callers in [1, 16] {
            measure("cas", &catalog, Operation::CompareAndSwap, callers).await;
        }
    });
}
