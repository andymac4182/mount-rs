//! Identical public-driver workloads with explicit sync boundaries.
use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions, OwnershipMode};
use mount_rs_core::storage::{
    BlockId, BlockStore, CheckoutRequest, ConcurrentBackingId, ConcurrentModeState,
    DelegatedCheckin, DelegatedPublish, DelegatedRecovery, DelegationState, DirectoryGrant,
    LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{ErrorCode, FsDriver, MkdirOptions, Result};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Counts {
    load: AtomicU64,
    publish: AtomicU64,
    publish_success: AtomicU64,
    renew: AtomicU64,
    metadata_flush: AtomicU64,
    put: AtomicU64,
    get: AtomicU64,
    block_flush: AtomicU64,
    load_if_changed: AtomicU64,
    cas_publish: AtomicU64,
    delegated_publish: AtomicU64,
    delegation_state: AtomicU64,
    checkout: AtomicU64,
    checkin: AtomicU64,
    backing_verify: AtomicU64,
}
impl Counts {
    fn reset(&self) {
        for c in [
            &self.load,
            &self.publish,
            &self.publish_success,
            &self.renew,
            &self.metadata_flush,
            &self.put,
            &self.get,
            &self.block_flush,
            &self.load_if_changed,
            &self.cas_publish,
            &self.delegated_publish,
            &self.delegation_state,
            &self.checkout,
            &self.checkin,
            &self.backing_verify,
        ] {
            c.store(0, Ordering::Relaxed);
        }
    }
    fn json(&self) -> Value {
        json!({"metadata_load":self.load.load(Ordering::Relaxed), "metadata_publish":self.publish.load(Ordering::Relaxed),"metadata_publish_success":self.publish_success.load(Ordering::Relaxed), "metadata_renew":self.renew.load(Ordering::Relaxed), "metadata_flush":self.metadata_flush.load(Ordering::Relaxed), "block_put":self.put.load(Ordering::Relaxed), "block_get":self.get.load(Ordering::Relaxed), "block_flush":self.block_flush.load(Ordering::Relaxed),"load_if_changed":self.load_if_changed.load(Ordering::Relaxed),"cas_publish":self.cas_publish.load(Ordering::Relaxed),"delegated_publish":self.delegated_publish.load(Ordering::Relaxed),"delegation_state":self.delegation_state.load(Ordering::Relaxed),"checkout":self.checkout.load(Ordering::Relaxed),"checkin":self.checkin.load(Ordering::Relaxed),"backing_verify":self.backing_verify.load(Ordering::Relaxed)})
    }
}
#[derive(Clone)]
struct Metadata<M>(M, Arc<Counts>);
#[async_trait]
impl<M: MetadataStore> MetadataStore for Metadata<M> {
    fn durable(&self) -> bool {
        self.0.durable()
    }
    fn publish_includes_flush_barrier(&self) -> bool {
        self.0.publish_includes_flush_barrier()
    }
    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        self.0.concurrent_mode_state().await
    }
    async fn load(&self) -> Result<LoadedMetadata> {
        self.1.load.fetch_add(1, Ordering::Relaxed);
        self.0.load().await
    }
    async fn load_if_changed(&self, revision: u64) -> Result<Option<LoadedMetadata>> {
        self.1.load_if_changed.fetch_add(1, Ordering::Relaxed);
        self.0.load_if_changed(revision).await
    }
    async fn preflight_new_bound_mode(&self) -> Result<()> {
        self.0.preflight_new_bound_mode().await
    }
    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        self.0.prepare_bound_concurrent_mode(backing).await
    }
    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        self.1.publish.fetch_add(1, Ordering::Relaxed);
        self.1.cas_publish.fetch_add(1, Ordering::Relaxed);
        let result = self
            .0
            .publish_bound_if_revision(backing, revision, namespace)
            .await;
        if result.is_ok() {
            self.1.publish_success.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
    async fn delegation_state(&self) -> Result<Option<DelegationState>> {
        self.1.delegation_state.fetch_add(1, Ordering::Relaxed);
        self.0.delegation_state().await
    }
    async fn prepare_delegated_mode(
        &self,
        backing: ConcurrentBackingId,
        revision: u64,
    ) -> Result<()> {
        self.0.prepare_delegated_mode(backing, revision).await
    }
    async fn checkout(&self, request: &CheckoutRequest) -> Result<DirectoryGrant> {
        self.1.checkout.fetch_add(1, Ordering::Relaxed);
        self.0.checkout(request).await
    }
    async fn publish_delegated(
        &self,
        request: &DelegatedPublish,
        namespace: Namespace,
    ) -> Result<u64> {
        self.1.publish.fetch_add(1, Ordering::Relaxed);
        self.1.delegated_publish.fetch_add(1, Ordering::Relaxed);
        let result = self.0.publish_delegated(request, namespace).await;
        if result.is_ok() {
            self.1.publish_success.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
    async fn checkin(&self, request: &DelegatedCheckin) -> Result<()> {
        self.1.checkin.fetch_add(1, Ordering::Relaxed);
        self.0.checkin(request).await
    }
    async fn recover(&self, request: &DelegatedRecovery) -> Result<()> {
        self.0.recover(request).await
    }
    async fn acquire_writer(&self, o: &str, t: Duration) -> Result<WriterLease> {
        self.0.acquire_writer(o, t).await
    }
    async fn renew_writer(&self, l: &WriterLease, t: Duration) -> Result<WriterLease> {
        self.1.renew.fetch_add(1, Ordering::Relaxed);
        self.0.renew_writer(l, t).await
    }
    async fn release_writer(&self, l: &WriterLease) -> Result<()> {
        self.0.release_writer(l).await
    }
    async fn publish(&self, r: u64, l: &WriterLease, n: Namespace) -> Result<u64> {
        self.1.publish.fetch_add(1, Ordering::Relaxed);
        let result = self.0.publish(r, l, n).await;
        if result.is_ok() {
            self.1.publish_success.fetch_add(1, Ordering::Relaxed);
        }
        result
    }
    async fn flush(&self) -> Result<()> {
        self.1.metadata_flush.fetch_add(1, Ordering::Relaxed);
        self.0.flush().await
    }
}
#[derive(Clone)]
struct Blocks<B>(B, Arc<Counts>);
#[async_trait]
impl<B: BlockStore> BlockStore for Blocks<B> {
    fn durable(&self) -> bool {
        self.0.durable()
    }
    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        self.0.prepare_concurrent_backing().await
    }
    async fn verify_concurrent_backing(&self, backing: ConcurrentBackingId) -> Result<()> {
        self.1.backing_verify.fetch_add(1, Ordering::Relaxed);
        self.0.verify_concurrent_backing(backing).await
    }
    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.0.get_for_migration(id).await
    }
    async fn put(&self, b: &[u8]) -> Result<BlockId> {
        self.1.put.fetch_add(1, Ordering::Relaxed);
        self.0.put(b).await
    }
    async fn get(&self, i: &BlockId) -> Result<Vec<u8>> {
        self.1.get.fetch_add(1, Ordering::Relaxed);
        self.0.get(i).await
    }
    async fn flush(&self) -> Result<()> {
        self.1.block_flush.fetch_add(1, Ordering::Relaxed);
        self.0.flush().await
    }
    async fn delete(&self, i: &BlockId) -> Result<()> {
        self.0.delete(i).await
    }
}
fn percentile(sorted: &[f64], p: f64) -> f64 {
    sorted[((sorted.len() as f64 * p).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1)]
}
fn distribution(values: &[f64]) -> Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({"samples":sorted.len(),"min_ms":sorted[0],"p50_ms":percentile(&sorted,0.5),"p95_ms":percentile(&sorted,0.95),"p99_ms":percentile(&sorted,0.99),"max_ms":sorted[sorted.len()-1]})
}
async fn trial<M: MetadataStore + Clone + 'static, B: BlockStore + Clone + 'static>(
    m: M,
    b: B,
    writeback: bool,
    workload: &str,
    batch: usize,
    operations: usize,
) -> Result<Value> {
    let counts = Arc::new(Counts::default());
    let m = Metadata(m, counts.clone());
    let b = Blocks(b, counts.clone());
    let options = ChunkedOptions::fixed("ownership-benchmark", 4096)?;
    let options = if writeback {
        options
            .with_ownership_mode(OwnershipMode::Exclusive)
            .with_writeback(true)
    } else {
        options // Exact legacy exclusive constructor, preserving write-through.
    };
    let fs = ChunkedFs::open(m.clone(), b.clone(), options.clone()).await?;
    let file = fs.open("/pages", "w+", 0o600).await?;
    file.write(&vec![0; 4096 * 64], Some(0)).await?;
    fs.syncfs().await?;
    counts.reset();
    let mut transactions = Vec::new();
    let started = Instant::now();
    let mut transaction = Instant::now();
    let mut expected = vec![0; 4096 * 64];
    let mut rng = 0x12345678_u64;
    for i in 0..operations {
        if workload == "random_pages" {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            let offset = (rng as usize % 64) * 4096;
            let page = vec![(i % 251 + 1) as u8; 4096];
            assert_eq!(file.write(&page, Some(offset as u64)).await?, page.len());
            expected[offset..offset + 4096].copy_from_slice(&page);
        } else {
            let from = format!("/entry-{i}");
            let to = format!("/renamed-{i}");
            let handle = fs.open(&from, "wx", 0o600).await?;
            handle.close().await?;
            fs.rename(&from, &to).await?;
            fs.unlink(&to).await?;
        }
        if (i + 1) % batch == 0 || i + 1 == operations {
            fs.syncfs().await?;
            transactions.push(transaction.elapsed().as_secs_f64() * 1000.0);
            transaction = Instant::now();
        }
    }
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let measured_counts = counts.json();
    file.close().await?;
    fs.shutdown().await?;
    drop(fs);
    let reopened = ChunkedFs::open(m, b, options).await?;
    let handle = reopened.open("/pages", "r", 0).await?;
    assert_eq!(handle.stat().await?.size, expected.len() as u64);
    let mut actual = vec![0; expected.len()];
    assert_eq!(handle.read(&mut actual, Some(0)).await?, actual.len());
    assert_eq!(actual, expected);
    assert_eq!(reopened.readdir("/").await?.len(), 1);
    handle.close().await?;
    reopened.shutdown().await?;
    Ok(
        json!({"elapsed_ms":elapsed,"transaction_ms":transactions,"transaction_distribution":distribution(&transactions),"counts":measured_counts,"reopen_verified":true}),
    )
}
fn shared_options(owner: &str, delegated: bool) -> Result<ChunkedOptions> {
    let options = ChunkedOptions::fixed(owner, 4096)?;
    Ok(if delegated {
        options.with_ownership_mode(OwnershipMode::Shared)
    } else {
        options.with_concurrent_writes(true)
    })
}
fn page_write(expected: &mut [u8], sequence: usize, client: usize) -> (usize, Vec<u8>) {
    let offset = ((sequence * 17 + client * 13) % 64) * 4096;
    let bytes = vec![((sequence + client * 79) % 251 + 1) as u8; 4096];
    expected[offset..offset + 4096].copy_from_slice(&bytes);
    (offset, bytes)
}
async fn verify_bytes(fs: &dyn FsDriver, path: &str, expected: &[u8]) -> Result<()> {
    let file = fs.open(path, "r", 0).await?;
    assert_eq!(file.stat().await?.size, expected.len() as u64);
    let mut actual = vec![0; expected.len()];
    assert_eq!(file.read(&mut actual, Some(0)).await?, expected.len());
    assert_eq!(actual, expected);
    file.close().await
}
async fn shared_trial(
    path: &std::path::Path,
    delegated: bool,
    workload: &str,
    operations: usize,
) -> Result<Value> {
    let counts = Arc::new(Counts::default());
    // Independent connections per client and a fresh provider pair for reopen.
    let stores = || -> Result<_> {
        Ok((
            Metadata(
                SqliteMetadataStore::open(path.join("metadata.db"))?,
                counts.clone(),
            ),
            Blocks(
                SqliteBlockStore::open(path.join("blocks.db"))?,
                counts.clone(),
            ),
        ))
    };
    let (metadata, blocks) = stores()?;
    let bootstrap =
        ChunkedFs::open(metadata, blocks, shared_options("bootstrap", delegated)?).await?;
    if delegated {
        bootstrap.checkout_scope("/").await?;
    }
    for directory in ["/a", "/b"] {
        bootstrap.mkdir(directory, MkdirOptions::default()).await?;
        let file = bootstrap
            .open(&format!("{directory}/pages"), "w+", 0o600)
            .await?;
        assert_eq!(file.write(&vec![0; 4096 * 64], Some(0)).await?, 4096 * 64);
        file.close().await?;
    }
    bootstrap.syncfs().await?;
    bootstrap.shutdown().await?;
    drop(bootstrap);
    let (metadata, blocks) = stores()?;
    let first = ChunkedFs::open(metadata, blocks, shared_options("client-a", delegated)?).await?;
    let (metadata, blocks) = stores()?;
    let second = ChunkedFs::open(metadata, blocks, shared_options("client-b", delegated)?).await?;
    if delegated {
        first.checkout_scope("/a").await?;
        if workload == "disjoint_clients" {
            second.checkout_scope("/b").await?;
        }
    }
    let mut expected_a = vec![0; 4096 * 64];
    let mut expected_b = expected_a.clone();
    let mut latencies = Vec::new();
    let mut admission_outcomes = Vec::new();
    counts.reset();
    let started = Instant::now();
    if workload == "disjoint_clients" {
        async fn writer(
            fs: &dyn FsDriver,
            path: &str,
            operations: usize,
            client: usize,
        ) -> Result<Vec<u8>> {
            let file = fs.open(path, "r+", 0).await?;
            let mut expected = vec![0; 4096 * 64];
            for i in 0..operations {
                let (offset, bytes) = page_write(&mut expected, i, client);
                assert_eq!(file.write(&bytes, Some(offset as u64)).await?, bytes.len());
                if (i + 1) % 16 == 0 || i + 1 == operations {
                    fs.syncfs().await?;
                }
            }
            file.close().await?;
            Ok(expected)
        }
        let barrier = std::sync::Barrier::new(2);
        let (a, b) = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                futures_lite::future::block_on(writer(&first, "/a/pages", operations, 0))
            });
            let b = scope.spawn(|| {
                barrier.wait();
                futures_lite::future::block_on(writer(&second, "/b/pages", operations, 1))
            });
            (
                a.join().expect("client a thread failed"),
                b.join().expect("client b thread failed"),
            )
        });
        expected_a = a?;
        expected_b = b?;
    } else if workload == "handoff" {
        for i in 0..operations {
            let (current, next) = if i % 2 == 0 {
                (&first, &second)
            } else {
                (&second, &first)
            };
            let file = current.open("/a/pages", "r+", 0).await?;
            let (offset, bytes) = page_write(&mut expected_a, i, 0);
            assert_eq!(file.write(&bytes, Some(offset as u64)).await?, bytes.len());
            file.close().await?;
            let transfer = Instant::now();
            if delegated {
                current.checkin_scope().await?;
                next.checkout_scope("/a").await?;
            } else {
                current.syncfs().await?;
            }
            // Both protocols verify the complete bytes in the receiving client.
            verify_bytes(next, "/a/pages", &expected_a).await?;
            latencies.push(transfer.elapsed().as_secs_f64() * 1000.0);
        }
    } else {
        for _ in 0..operations {
            let admission = Instant::now();
            if delegated {
                let error = second
                    .checkout_scope("/a")
                    .await
                    .expect_err("overlapping claim must fail");
                assert!(matches!(error.code, ErrorCode::Estale | ErrorCode::Ebusy));
                admission_outcomes.push(error.code.as_str());
            } else {
                verify_bytes(&second, "/a/pages", &expected_a).await?;
                admission_outcomes.push("accepted_without_ownership");
            }
            latencies.push(admission.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    let measured = counts.json();
    if workload == "disjoint_clients" {
        assert_eq!(
            measured["metadata_publish_success"].as_u64(),
            Some((operations * 2) as u64),
            "shared page writes must publish immediately"
        );
    }
    first.shutdown().await?;
    second.shutdown().await?;
    drop(first);
    drop(second);
    let (metadata, blocks) = stores()?;
    let verifier = ChunkedFs::open(
        metadata,
        blocks,
        shared_options("reopen-verifier", delegated)?,
    )
    .await?;
    if delegated {
        verifier.checkout_scope("/").await?;
    }
    verify_bytes(&verifier, "/a/pages", &expected_a).await?;
    verify_bytes(&verifier, "/b/pages", &expected_b).await?;
    verifier.shutdown().await?;
    Ok(
        json!({"elapsed_ms":elapsed,"operation_ms":latencies,"operation_distribution":if latencies.is_empty(){Value::Null}else{distribution(&latencies)},"counts":measured,"reopen_verified":true,"admission_outcomes":admission_outcomes,"timing_boundary":match workload {"disjoint_clients"=>"two independent barrier-started OS threads, each operations writes plus sync every16 and close; setup and reopen excluded", "handoff"=>"total includes write/close; operation_ms measures checkin+checkout (delegated) or syncfs (legacy), then receiver open/stat/full read/close", _=>"delegated denied checkout vs legacy accepted open/stat/full read/close; these admission outcomes have different semantics"}}),
    )
}

async fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let samples = std::env::var("BENCH_SAMPLES")
        .unwrap_or("7".into())
        .parse::<usize>()?;
    let operations = std::env::var("BENCH_OPERATIONS")
        .unwrap_or("128".into())
        .parse::<usize>()?;
    assert!(samples > 0 && operations > 0);
    let root = std::env::temp_dir().join(format!(
        "mount-rs-ownership-benchmark-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&root)?;
    let suite = std::env::var("BENCH_SUITE").unwrap_or("exclusive".into());
    assert!(matches!(suite.as_str(), "exclusive" | "delegated" | "all"));
    let mut cases = Vec::new();
    if suite != "delegated" {
        for backend in ["memory", "sqlite"] {
            for workload in ["random_pages", "namespace"] {
                for batch in [1, 16] {
                    for writeback in [false, true] {
                        let mut trials = Vec::new();
                        for sample in 0..=samples {
                            let result = if backend == "memory" {
                                trial(
                                    MemoryMetadataStore::new(),
                                    MemoryBlockStore::new(),
                                    writeback,
                                    workload,
                                    batch,
                                    operations,
                                )
                                .await?
                            } else {
                                let path =
                                    root.join(format!("{workload}-{batch}-{writeback}-{sample}"));
                                std::fs::create_dir_all(&path)?;
                                trial(
                                    SqliteMetadataStore::open(path.join("metadata.db"))?,
                                    SqliteBlockStore::open(path.join("blocks.db"))?,
                                    writeback,
                                    workload,
                                    batch,
                                    operations,
                                )
                                .await?
                            };
                            if sample > 0 {
                                trials.push(result);
                            }
                        }
                        let timings: Vec<f64> = trials
                            .iter()
                            .map(|v| v["elapsed_ms"].as_f64().unwrap())
                            .collect();
                        cases.push(json!({"backend":backend,"workload":workload,"sync_every_operations":batch,"writeback":writeback,"operations":operations,"trials":trials,"elapsed_distribution":distribution(&timings)}));
                    }
                }
            }
        }
    }
    if suite != "exclusive" {
        {
            let backend = "sqlite";
            for workload in ["disjoint_clients", "handoff", "same_directory_admission"] {
                for delegated in [false, true] {
                    let mut trials = Vec::new();
                    for sample in 0..=samples {
                        let path = root.join(format!("shared-{workload}-{delegated}-{sample}"));
                        std::fs::create_dir_all(&path)?;
                        let result = shared_trial(&path, delegated, workload, operations).await?;
                        if sample > 0 {
                            trials.push(result);
                        }
                    }
                    let timings: Vec<f64> = trials
                        .iter()
                        .map(|t| t["elapsed_ms"].as_f64().unwrap())
                        .collect();
                    let operation_timings: Vec<f64> = trials
                        .iter()
                        .flat_map(|t| {
                            t["operation_ms"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|v| v.as_f64().unwrap())
                        })
                        .collect();
                    cases.push(json!({"backend":backend,"workload":workload,"protocol":if delegated {"MRC3_delegated"} else {"MRC2_CAS"},"operations":operations,"trials":trials,"elapsed_distribution":distribution(&timings),"operation_distribution":if operation_timings.is_empty(){Value::Null}else{distribution(&operation_timings)}}));
                }
            }
        }
    }
    std::fs::remove_dir_all(&root)?;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"schema":2,"suite":suite,"profile":"release","page_bytes":4096,"file_pages":64,"warmup_trials_per_case":1,"samples_per_case":samples,"artificial_latency_ms":0,"timing_boundary":"workload operations plus every syncfs; setup, shutdown and verified reopen excluded", "cases":cases})
        )?
    );
    Ok(())
}
fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    futures_lite::future::block_on(run())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearest_rank_percentiles() {
        let v = [1., 2., 3., 4., 5.];
        assert_eq!(percentile(&v, 0.5), 3.);
        assert_eq!(percentile(&v, 0.95), 5.);
        assert_eq!(percentile(&v, 0.99), 5.);
    }
}
