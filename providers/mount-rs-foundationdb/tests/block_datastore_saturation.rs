#![cfg(feature = "foundationdb")]

use mount_rs_core::storage::BlockStore;
use mount_rs_foundationdb::{
    FoundationDbStorage, FoundationDbStorageOptions, shutdown_client_network,
};
use serde_json::json;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn observer(phase: &str, mode: &str, stage: &str, successes: u64, failures: u64) {
    if let Ok(path) = std::env::var("MOUNT_RS_DATASTORE_STAGE_OBSERVER") {
        if !path.is_empty() {
            assert!(
                std::process::Command::new(path)
                    .args([
                        phase,
                        mode,
                        "100",
                        stage,
                        &successes.to_string(),
                        &failures.to_string()
                    ])
                    .status()
                    .expect("observer process")
                    .success(),
                "observer failed"
            );
        }
    }
}

fn payload(worker: usize, sequence: u64) -> Vec<u8> {
    let mut state = sequence ^ ((worker as u64 + 1) << 32);
    (0..4096)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

#[test]
#[ignore = "requires an owned real SSD FoundationDB cluster and serial performance slot"]
fn direct_4k_block_store_100_clients() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(10)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let cluster = std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").unwrap();
        let output = std::env::var("MOUNT_RS_FOUNDATIONDB_DIRECT_OUTPUT").unwrap();
        let run = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let prefix = format!("mount-rs/direct-block-datastore/{run}");
        let seconds: u64 = std::env::var("MOUNT_RS_FOUNDATIONDB_DIRECT_SECONDS")
            .unwrap_or_else(|_| "15".into()).parse().unwrap();
        assert!((1..=30).contains(&seconds));
        let mut stores = Vec::new();
        for _ in 0..10 {
            stores.push(FoundationDbStorage::connect(&cluster,
                FoundationDbStorageOptions::new(&prefix).without_lease_oracle()).unwrap());
        }
        let mut seed_tasks = Vec::new();
        for worker in 0..100 {
            let blocks = stores[worker % 10].blocks();
            seed_tasks.push(tokio::spawn(async move {
                let mut ids = Vec::new();
                for block in 0..32 { ids.push(blocks.put(&payload(worker, block + 1)).await.unwrap()); }
                ids
            }));
        }
        let mut seed_ids = Vec::new();
        for task in seed_tasks { seed_ids.push(task.await.unwrap()); }
        let mut reports = Vec::new();
        for mode in ["read", "write"] {
            // Warm each read dataset before its timed stage; no cold-cache claim.
            if mode == "read" {
                for (worker, ids) in seed_ids.iter().enumerate() {
                    let blocks = stores[worker % 10].blocks();
                    for (index, id) in ids.iter().enumerate() {
                        assert_eq!(blocks.get(id).await.unwrap(), payload(worker, index as u64 + 1));
                    }
                }
            }
            let stage = format!("fdb-direct-{run}-{mode}-q100");
            observer("begin", mode, &stage, 0, 0);
            let start = Instant::now();
            let deadline = start + Duration::from_secs(seconds);
            let mut tasks = Vec::new();
            for worker in 0..100 {
                let blocks = stores[worker % 10].blocks();
                let ids = seed_ids[worker].clone();
                tasks.push(tokio::spawn(async move {
                    let mut count = 0u64;
                    let mut failures = Vec::new();
                    let mut histogram = [0u64; 64];
                    let mut written = Vec::new();
                    while Instant::now() < deadline {
                        let operation_start = Instant::now();
                        let result = if mode == "read" {
                            let index = (count as usize * 17 + worker) % ids.len();
                            blocks.get(&ids[index]).await.map(|bytes| {
                                assert_eq!(bytes, payload(worker, index as u64 + 1));
                            })
                        } else {
                            let bytes = payload(worker + 100, count + 1);
                            blocks.put(&bytes).await.map(|id| written.push((id, count + 1)))
                        };
                        match result {
                            Ok(()) => {
                                count += 1;
                                let us = operation_start.elapsed().as_micros().max(1) as u64;
                                let bin = (64 - (us - 1).leading_zeros()) as usize;
                                histogram[bin.min(63)] += 1;
                            }
                            Err(error) => { failures.push(error.to_string()); break; }
                        }
                    }
                    (worker, count, failures, histogram, written)
                }));
            }
            let mut results = Vec::new();
            for task in tasks { results.push(task.await.unwrap()); }
            let elapsed = start.elapsed().as_secs_f64();
            let successes = results.iter().map(|r| r.1).sum::<u64>();
            let failures = results.iter().map(|r| r.2.len() as u64).sum::<u64>();
            observer("end", mode, &stage, successes, failures);
            let mut histogram = [0u64; 64];
            for result in &results {
                for (bin, count) in result.3.iter().enumerate() { histogram[bin] += count; }
            }
            let mut cumulative = 0;
            let mut p99 = 0;
            for (bin, count) in histogram.iter().enumerate() {
                cumulative += count;
                if successes > 0 && cumulative >= (successes * 99).div_ceil(100) { p99 = 1u64 << bin; break; }
            }
            reports.push(json!({"mode":mode,"stage_id":stage,"successes":successes,
                "failures":failures,"elapsed_seconds_including_drain":elapsed,
                "block_operations_per_second":successes as f64/elapsed,"p99_us_upper":p99,
                "failure_samples":results.iter().flat_map(|r|r.2.iter()).take(8).collect::<Vec<_>>() }));
            // Validate every newly stored block and delete owned keys outside timing.
            let mut cleanup = Vec::new();
            for (worker, _, _, _, written) in results {
                let blocks = stores[worker % 10].blocks();
                cleanup.push(tokio::spawn(async move {
                    for (id, sequence) in written {
                        assert_eq!(blocks.get(&id).await.unwrap(), payload(worker + 100, sequence));
                        blocks.delete(&id).await.unwrap();
                    }
                }));
            }
            for task in cleanup { task.await.unwrap(); }
        }
        let mut cleanup = Vec::new();
        for (worker, ids) in seed_ids.into_iter().enumerate() {
            let blocks = stores[worker % 10].blocks();
            cleanup.push(tokio::spawn(async move {
                for id in ids { blocks.delete(&id).await.unwrap(); }
            }));
        }
        for task in cleanup { task.await.unwrap(); }
        let any_failure = reports.iter().any(|report| report["failures"].as_u64().unwrap() != 0);
        std::fs::write(output, serde_json::to_vec_pretty(&json!({
            "schema":"mount-rs-foundationdb-direct-block-v1","clients":100,"provider_handles":10,
            "payload_bytes":4096,"seed_dataset_bytes":100*32*4096,"queue_depth":100,
            "scope":"native content-addressed BlockStore; SDK/filesystem/protocol/audit omitted",
            "cache":"warm read dataset","all_blocks_verified_and_deleted":true,"stages":reports,
            "write_semantics":"new immutable 4KiB block each put; metadata pointer publication omitted"
        })).unwrap()).unwrap();
        assert!(!any_failure, "direct block stage failed; retained diagnostic artifact");
    });
    drop(runtime);
    shutdown_client_network().unwrap();
}
