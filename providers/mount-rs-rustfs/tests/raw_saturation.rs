//! Ignored real-fixture baseline for the blob adapter, without metadata or QUIC.
//!
//! The caller owns the fixture, environment, process deadline and paired backing
//! metrics. This test creates a fresh generated prefix, never reclaims a supplied
//! prefix, and quarantines writes whose outcome cannot be proved. Successful
//! calls measure the public BlockStore API, including its hashing, upload copy,
//! request allocation, download/digest and return copy. Payload generation and
//! worker construction precede admission. This is not an allocation-free test,
//! cold-server-cache proof, fsync/durability test or physical disk IOPS measurement.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures::stream::{FuturesUnordered, StreamExt};
use mount_rs_core::storage::{BlockId, BlockStore, ConcurrentBackingId};
use mount_rs_object_store_blocks::{LocalWorkSnapshot, RawApiSnapshot};
use mount_rs_rustfs::{RawBlockCacheBudget, RustFsBlockStore, RustFsConfig};
use object_store::aws::{AmazonS3Builder, S3ConditionalPut};
use object_store::{ClientOptions, ObjectStore, RetryConfig};
use serde_json::json;
use tokio::sync::{Barrier, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout_at};

const BLOCK_BYTES: usize = 4096;
const READ_SEEDS: usize = 256;
const WRITES_PER_PHASE: usize = 5120;
const PAYLOAD_COUNT: usize = READ_SEEDS + 3 * WRITES_PER_PHASE;
const MAX_WORKERS: usize = 100;
const ADMISSION: Duration = Duration::from_secs(5);
const REQUEST: Duration = Duration::from_secs(10);
const PHASE: Duration = Duration::from_secs(120);
const SETTLEMENT_RESERVE: Duration = Duration::from_secs(5);
const SUITE: Duration = Duration::from_secs(600);
const CLEANUP: Duration = Duration::from_secs(30);
const HISTOGRAM_BUCKETS: usize = 32;
static RUN_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug)]
enum Pattern {
    Read,
    Write { base: usize },
    Authority,
}

impl Pattern {
    fn label(self) -> &'static str {
        match self {
            Self::Read => "raw_get",
            Self::Write { .. } => "raw_unique_put",
            Self::Authority => "configured_authority_verify",
        }
    }
}

#[derive(Default)]
struct Counts {
    success: u64,
    failed: u64,
    uncertain: u64,
    success_bytes: u64,
    success_histogram: [u64; HISTOGRAM_BUCKETS],
    failed_histogram: [u64; HISTOGRAM_BUCKETS],
    uncertain_histogram: [u64; HISTOGRAM_BUCKETS],
    latency_ns_total: u128,
    latency_ns_max: u128,
    data_cap_reached: bool,
    write_outcome_unknown: bool,
    unacknowledged_payload_indices: Vec<usize>,
}

impl Counts {
    fn observe(&mut self, outcome: u8, elapsed: Duration, bytes: usize) {
        let index = latency_bucket(elapsed);
        let nanos = elapsed.as_nanos();
        self.latency_ns_total += nanos;
        self.latency_ns_max = self.latency_ns_max.max(nanos);
        match outcome {
            0 => {
                self.success += 1;
                self.success_bytes += bytes as u64;
                self.success_histogram[index] += 1;
            }
            1 => {
                self.failed += 1;
                self.failed_histogram[index] += 1;
            }
            _ => {
                self.uncertain += 1;
                self.uncertain_histogram[index] += 1;
            }
        }
    }

    fn merge(&mut self, other: Self) {
        self.success += other.success;
        self.failed += other.failed;
        self.uncertain += other.uncertain;
        self.success_bytes += other.success_bytes;
        self.latency_ns_total += other.latency_ns_total;
        self.latency_ns_max = self.latency_ns_max.max(other.latency_ns_max);
        self.data_cap_reached |= other.data_cap_reached;
        self.write_outcome_unknown |= other.write_outcome_unknown;
        self.unacknowledged_payload_indices
            .extend(other.unacknowledged_payload_indices);
        for index in 0..HISTOGRAM_BUCKETS {
            self.success_histogram[index] += other.success_histogram[index];
            self.failed_histogram[index] += other.failed_histogram[index];
            self.uncertain_histogram[index] += other.uncertain_histogram[index];
        }
    }
}

struct OwnedBlock {
    payload_index: usize,
    id: BlockId,
}

#[derive(Clone)]
struct ReadSeed {
    payload_index: usize,
    id: BlockId,
}

struct PhaseResult {
    counts: Counts,
    workers_settled: bool,
    all_workers_returned_metrics: bool,
    successful_work_observed: bool,
}

impl PhaseResult {
    fn qualified(&self) -> bool {
        self.successful_work_observed
            && self.workers_settled
            && self.all_workers_returned_metrics
            && self.counts.failed == 0
            && self.counts.uncertain == 0
    }
}

fn latency_bucket(duration: Duration) -> usize {
    let micros = duration.as_nanos().div_ceil(1000);
    if micros <= 1 {
        return 0;
    }
    (128 - (micros - 1).leading_zeros()).min((HISTOGRAM_BUCKETS - 1) as u32) as usize
}

fn wall_ns() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch")
        .as_nanos()
        .to_string()
}

fn run_identity() -> ([u8; 32], String) {
    let clock = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must follow the Unix epoch")
        .as_nanos();
    let counter = RUN_COUNTER
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
            value.checked_add(1)
        })
        .expect("run counter must not wrap");
    let pid = u64::from(std::process::id());
    let mut nonce = [0; 32];
    nonce[..16].copy_from_slice(&clock.to_le_bytes());
    nonce[16..24].copy_from_slice(&pid.to_le_bytes());
    nonce[24..].copy_from_slice(&counter.to_le_bytes());
    (
        nonce,
        format!("mount-rs-owned-raw-saturation/{pid}-{clock}-{counter}"),
    )
}

fn payload(nonce: &[u8; 32], index: usize) -> [u8; BLOCK_BYTES] {
    let mut bytes = [0; BLOCK_BYTES];
    bytes[..32].copy_from_slice(nonce);
    bytes[32..40].copy_from_slice(&(index as u64).to_le_bytes());
    let mut state = (index as u64).wrapping_add(0x9e37_79b9_7f4a_7c15);
    for chunk in nonce.chunks_exact(8) {
        state ^= u64::from_le_bytes(chunk.try_into().expect("nonce chunks are eight bytes"));
        state = state.rotate_left(17).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    }
    for byte in &mut bytes[40..] {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        *byte = state as u8;
    }
    bytes
}

fn local_config() -> RustFsConfig {
    let required = |name: &str| {
        std::env::var(name)
            .unwrap_or_else(|_| panic!("{name} is required by the owned RustFS fixture"))
    };
    let config = RustFsConfig {
        endpoint: required("RUSTFS_ENDPOINT"),
        bucket: required("RUSTFS_BUCKET"),
        access_key_id: required("RUSTFS_ACCESS_KEY_ID"),
        secret_access_key: required("RUSTFS_SECRET_ACCESS_KEY"),
        region: required("RUSTFS_REGION"),
    };
    config
        .validate()
        .unwrap_or_else(|_| panic!("invalid owned RustFS configuration"));
    assert!(
        config.endpoint.starts_with("http://127.0.0.1:")
            || config.endpoint.starts_with("http://localhost:"),
        "raw saturation requires the owned loopback RustFS fixture"
    );
    assert!(
        std::env::var_os("RUSTFS_TEST_PREFIX").is_some(),
        "the RustFS fixture must identify its owning run"
    );
    // The caller's prefix is never selected for writes, listing or deletion.
    config
}

fn raw_blocks(
    config: &RustFsConfig,
    prefix: &str,
    budget: RawBlockCacheBudget,
) -> Arc<RustFsBlockStore> {
    // Preserve the production signed path-style conditional-create convention,
    // while removing SDK retries specifically from the measured raw data path.
    let client: Arc<dyn ObjectStore> = Arc::new(
        AmazonS3Builder::new()
            .with_endpoint(config.endpoint.trim_end_matches('/'))
            .with_bucket_name(&config.bucket)
            .with_access_key_id(&config.access_key_id)
            .with_secret_access_key(&config.secret_access_key)
            .with_region(&config.region)
            .with_virtual_hosted_style_request(false)
            .with_conditional_put(S3ConditionalPut::ETagMatch)
            .with_client_options(
                ClientOptions::new()
                    .with_connect_timeout(Duration::from_secs(3))
                    .with_timeout(REQUEST),
            )
            .with_allow_http(true)
            .with_retry(RetryConfig {
                max_retries: 0,
                retry_timeout: REQUEST,
                ..RetryConfig::default()
            })
            .build()
            .unwrap_or_else(|_| panic!("validated raw RustFS client construction failed")),
    );
    Arc::new(
        RustFsBlockStore::new_with_cache_budget(client, prefix, true, budget)
            .unwrap_or_else(|_| panic!("raw RustFS block scope construction failed")),
    )
}

fn zero_cache(budget: &RawBlockCacheBudget) -> bool {
    budget.snapshot().is_some_and(|snapshot| {
        snapshot.charged_bytes == 0
            && snapshot.payload_bytes == 0
            && snapshot.entries == 0
            && snapshot.high_water_charged_bytes == 0
            && snapshot.high_water_entries == 0
    })
}

fn raw_api_value(snapshot: &Option<RawApiSnapshot>) -> serde_json::Value {
    snapshot.as_ref().map_or(serde_json::Value::Null, |value| {
        json!({"schema":value.schema,"scope":value.scope,"saturated":value.saturated,
            "in_flight":value.in_flight,"pending_claims":value.pending_claims,
            "claims":{"leader_claims":value.claims.leader_claims,"leader_success":value.claims.leader_success,
                "leader_error":value.claims.leader_error,"leader_cancelled":value.claims.leader_cancelled,
                "follower_claims":value.claims.follower_claims,"follower_success":value.claims.follower_success,
                "follower_error":value.claims.follower_error,"follower_cancelled":value.claims.follower_cancelled},
            "entries":value.entries.iter().map(|row|json!({"name":row.name,"calls":row.calls,
                "success":row.success,"error":row.error,"cancelled":row.cancelled,
                "elapsed_ns":row.elapsed_ns,"latency_max_ns":row.latency_max_ns,
                "attempted_bytes":row.attempted_bytes,"confirmed_bytes":row.confirmed_bytes,
                "returned_bytes":row.returned_bytes,"latency_log2_us":row.latency_log2_us})).collect::<Vec<_>>()})
    })
}

fn local_work_value(snapshot: &Option<LocalWorkSnapshot>) -> serde_json::Value {
    snapshot.as_ref().map_or(serde_json::Value::Null, |value| {
        json!({"schema":value.schema,"scope":value.scope,"saturated":value.saturated,"in_flight":value.in_flight,
            "entries":value.entries.iter().map(|row|json!({"name":row.name,"calls":row.calls,
                "success":row.success,"error":row.error,"cancelled":row.cancelled,
                "elapsed_ns":row.elapsed_ns,"latency_max_ns":row.latency_max_ns,
                "input_bytes":row.input_bytes,"output_bytes":row.output_bytes,
                "latency_log2_us":row.latency_log2_us})).collect::<Vec<_>>()})
    })
}

#[allow(clippy::too_many_arguments)]
async fn phase(
    pattern: Pattern,
    concurrency: usize,
    blocks: Arc<RustFsBlockStore>,
    authority: Arc<RustFsBlockStore>,
    expected: ConcurrentBackingId,
    corpus: Arc<Vec<[u8; BLOCK_BYTES]>>,
    seeds: Arc<Vec<ReadSeed>>,
    owned: &mut Vec<OwnedBlock>,
    budget: &RawBlockCacheBudget,
    work_end: Instant,
    admission: Duration,
) -> PhaseResult {
    assert!((1..=MAX_WORKERS).contains(&concurrency));
    assert!(admission <= ADMISSION);
    let hard_end = (Instant::now() + PHASE).min(work_end);
    let operation_end = hard_end - SETTLEMENT_RESERVE;
    let ready = Arc::new(Barrier::new(concurrency + 1));
    let release = Arc::new(Barrier::new(concurrency + 1));
    let (clock_tx, clock_rx) = watch::channel::<Option<Instant>>(None);
    let mut started = None;
    let stop = Arc::new(AtomicBool::new(false));
    let mut workers = JoinSet::new();
    let mut ledgers = Vec::with_capacity(concurrency);
    let before = blocks.stats();
    let authority_before = authority.http_observer().snapshot();
    for worker in 0..concurrency {
        let blocks = Arc::clone(&blocks);
        let authority = Arc::clone(&authority);
        let corpus = Arc::clone(&corpus);
        let seeds = Arc::clone(&seeds);
        let ready = Arc::clone(&ready);
        let release = Arc::clone(&release);
        let mut clock_rx = clock_rx.clone();
        let stop = Arc::clone(&stop);
        let capacity = if matches!(pattern, Pattern::Write { .. }) {
            WRITES_PER_PHASE.div_ceil(concurrency)
        } else {
            0
        };
        // One writer per ledger. The parent retains it across task errors, and
        // only reads it after retirement; no global completion-ledger lock.
        let ledger = Arc::new(Mutex::new(Vec::<OwnedBlock>::with_capacity(capacity)));
        ledgers.push(Arc::clone(&ledger));
        workers.spawn(async move {
            let mut result = Counts {
                unacknowledged_payload_indices: Vec::with_capacity(1),
                ..Counts::default()
            };
            ready.wait().await;
            release.wait().await;
            // Publication follows the completed release barrier. A stored
            // watch value cannot be missed by a worker scheduled afterwards.
            if clock_rx.changed().await.is_err() {
                return result;
            }
            let began = clock_rx
                .borrow()
                .expect("released phase clock is published");
            let admission_end = (began + admission).min(operation_end);
            let mut ordinal = 0_usize;
            while Instant::now() < admission_end && !stop.load(Ordering::Acquire) {
                let request_end = (Instant::now() + REQUEST).min(operation_end);
                let request_started = Instant::now();
                match pattern {
                    Pattern::Write { base } => {
                        let offset = worker + ordinal * concurrency;
                        if offset >= WRITES_PER_PHASE {
                            result.data_cap_reached = true;
                            break;
                        }
                        let index = base + offset;
                        match timeout_at(request_end, blocks.put(&corpus[index])).await {
                            Ok(Ok(id)) => {
                                result.observe(0, request_started.elapsed(), BLOCK_BYTES);
                                ledger.lock().expect("single-writer owned ledger").push(
                                    OwnedBlock {
                                        payload_index: index,
                                        id,
                                    },
                                );
                            }
                            Ok(Err(_)) => {
                                result.observe(1, request_started.elapsed(), 0);
                                // A returned transport/provider error cannot
                                // establish that no object was committed.
                                result.write_outcome_unknown = true;
                                result.unacknowledged_payload_indices.push(index);
                                stop.store(true, Ordering::Release);
                            }
                            Err(_) => {
                                result.observe(2, request_started.elapsed(), 0);
                                result.write_outcome_unknown = true;
                                result.unacknowledged_payload_indices.push(index);
                                stop.store(true, Ordering::Release);
                            }
                        }
                    }
                    Pattern::Read => {
                        let seed = &seeds[(worker + ordinal * concurrency) % seeds.len()];
                        match timeout_at(request_end, blocks.get(&seed.id)).await {
                            Ok(Ok(bytes)) => {
                                let elapsed = request_started.elapsed();
                                if bytes.as_slice() == corpus[seed.payload_index].as_slice() {
                                    result.observe(0, elapsed, bytes.len());
                                } else {
                                    result.observe(1, elapsed, 0);
                                    stop.store(true, Ordering::Release);
                                }
                            }
                            Ok(Err(_)) => {
                                result.observe(1, request_started.elapsed(), 0);
                                stop.store(true, Ordering::Release);
                            }
                            Err(_) => {
                                result.observe(2, request_started.elapsed(), 0);
                                stop.store(true, Ordering::Release);
                            }
                        }
                    }
                    Pattern::Authority => {
                        match timeout_at(request_end, authority.verify_concurrent_backing(expected))
                            .await
                        {
                            Ok(Ok(())) => result.observe(0, request_started.elapsed(), 0),
                            Ok(Err(_)) => {
                                result.observe(1, request_started.elapsed(), 0);
                                stop.store(true, Ordering::Release);
                            }
                            Err(_) => {
                                result.observe(2, request_started.elapsed(), 0);
                                stop.store(true, Ordering::Release);
                            }
                        }
                    }
                }
                ordinal += 1;
            }
            result
        });
    }
    let mut result = PhaseResult {
        counts: Counts {
            unacknowledged_payload_indices: Vec::with_capacity(concurrency),
            ..Counts::default()
        },
        workers_settled: true,
        all_workers_returned_metrics: true,
        successful_work_observed: false,
    };
    drop(clock_rx);
    if timeout_at(operation_end, ready.wait()).await.is_err() {
        result.all_workers_returned_metrics = false;
        stop.store(true, Ordering::Release);
        workers.abort_all();
    } else {
        // Startup formatting and output are outside admission. Workers remain
        // behind the release barrier until this event has finished printing.
        println!(
            "{}",
            json!({"event":"raw_saturation_phase_startup","pattern":pattern.label(),"concurrency":concurrency,"wall_unix_ns":wall_ns(),"admission_ns":admission.as_nanos().to_string(),"phase_limit_seconds":120,"scope":"startup event precedes release and timed admission"})
        );
        if timeout_at(operation_end, release.wait()).await.is_err() {
            result.all_workers_returned_metrics = false;
            stop.store(true, Ordering::Release);
            workers.abort_all();
        } else {
            // No formatting, output or allocation separates this clock from
            // the completed release barrier. Workers only admit I/O after the
            // publication; subsequent publication/scheduling delay is counted.
            let began = Instant::now();
            started = Some(began);
            if clock_tx.send(Some(began)).is_err() {
                result.all_workers_returned_metrics = false;
                stop.store(true, Ordering::Release);
                workers.abort_all();
            }
        }
    }
    while !workers.is_empty() {
        match timeout_at(operation_end, workers.join_next()).await {
            Ok(Some(Ok(counts))) => result.counts.merge(counts),
            Ok(Some(Err(_))) => {
                result.all_workers_returned_metrics = false;
                stop.store(true, Ordering::Release);
            }
            Ok(None) => break,
            Err(_) => {
                result.all_workers_returned_metrics = false;
                stop.store(true, Ordering::Release);
                workers.abort_all();
                break;
            }
        }
    }
    while !workers.is_empty() {
        match timeout_at(hard_end, workers.join_next()).await {
            Ok(Some(Ok(counts))) => result.counts.merge(counts),
            Ok(Some(Err(_))) => result.all_workers_returned_metrics = false,
            Ok(None) => break,
            Err(_) => {
                result.workers_settled = false;
                break;
            }
        }
    }
    if result.workers_settled {
        for ledger in ledgers {
            match ledger.lock() {
                Ok(mut ledger) => owned.append(&mut ledger),
                Err(_) => result.all_workers_returned_metrics = false,
            }
        }
    }
    let elapsed = started.map_or(Duration::ZERO, |began| began.elapsed());
    let after = blocks.stats();
    let cache_empty = zero_cache(budget);
    let no_cache_hits = after.cache_hits == before.cache_hits;
    let no_duplicate_put = after.conditional_conflicts == before.conditional_conflicts;
    if !cache_empty || !no_cache_hits || !no_duplicate_put {
        result.all_workers_returned_metrics = false;
    }
    result.successful_work_observed = result.counts.success > 0;
    println!(
        "{}",
        json!({
            "event":"raw_saturation_phase_end","pattern":pattern.label(),"concurrency":concurrency,
            "wall_unix_ns":wall_ns(),"elapsed_ns":elapsed.as_nanos().to_string(),
            "successful_logical_operations":result.counts.success,"successful_logical_bytes":result.counts.success_bytes,
            "successful_work_observed":result.successful_work_observed,"phase_qualified":result.qualified(),
            "zero_work_failure":!result.successful_work_observed,"clock_scope":"published immediately after release barrier; startup excluded",
            "returned_failed_operations":result.counts.failed,"timed_out_uncertain_operations":result.counts.uncertain,
            "latency_bucket_unit":"microseconds_inclusive_upper_bound",
            "latency_bucket_le":std::array::from_fn::<_,HISTOGRAM_BUCKETS,_>(|index|1_u64<<index),
            "success_latency_histogram":result.counts.success_histogram,
            "failed_latency_histogram":result.counts.failed_histogram,
            "uncertain_latency_histogram":result.counts.uncertain_histogram,
            "latency_ns_total":result.counts.latency_ns_total.to_string(),"latency_ns_max":result.counts.latency_ns_max.to_string(),
            "capacity_limited":result.counts.data_cap_reached,"write_outcome_unknown":result.counts.write_outcome_unknown,
            "unacknowledged_payload_indices":result.counts.unacknowledged_payload_indices,
            "workers_settled":result.workers_settled,"all_workers_returned_metrics":result.all_workers_returned_metrics,
            "raw_cache_zero":cache_empty,"cache_hits_delta":after.cache_hits.checked_sub(before.cache_hits),
            "adapter_puts_delta":after.puts.checked_sub(before.puts),"adapter_gets_delta":after.gets.checked_sub(before.gets),
            "adapter_successes_delta":after.successes.checked_sub(before.successes),"adapter_errors_delta":after.errors.checked_sub(before.errors),
            "adapter_raw_api_before":raw_api_value(&before.raw_api),"adapter_raw_api_after":raw_api_value(&after.raw_api),
            "adapter_local_work_before":local_work_value(&before.local_work),"adapter_local_work_after":local_work_value(&after.local_work),
            "authority_http_before":authority_before,"authority_http_after":authority.http_observer().snapshot(),
            "raw_data_sdk_max_retries":0,"harness_retries":0,"physical_disk_iops_claim":false,
            "scope":"public BlockStore logical calls; server/OS caches and durability unqualified; framework bookkeeping remains in elapsed time"
        })
    );
    result
}

#[derive(Default)]
struct CleanupResult {
    deleted: usize,
    failed: usize,
    uncertain: usize,
    unattempted: usize,
}

async fn delete_exact(
    blocks: &RustFsBlockStore,
    owned: &[OwnedBlock],
    deadline: Instant,
) -> CleanupResult {
    let mut result = CleanupResult::default();
    let mut pending = FuturesUnordered::new();
    let mut next = 0;
    loop {
        while next < owned.len() && pending.len() < MAX_WORKERS && Instant::now() < deadline {
            let id = &owned[next].id;
            pending.push(async move { timeout_at(deadline, blocks.delete(id)).await });
            next += 1;
        }
        match pending.next().await {
            Some(Ok(Ok(()))) => result.deleted += 1,
            Some(Ok(Err(_))) => result.failed += 1,
            Some(Err(_)) => result.uncertain += 1,
            None => break,
        }
    }
    result.unattempted = owned.len() - next;
    result
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the owned loopback RustFS fixture, MOUNT_RS_PROFILE_IO=1 and a 600 second owner"]
async fn raw_block_store_saturation_zero_cache() {
    let suite_started = Instant::now();
    let suite_end = suite_started + SUITE;
    let work_end = suite_end - CLEANUP;
    let config = local_config();
    assert_eq!(
        std::env::var("MOUNT_RS_PROFILE_IO").as_deref(),
        Ok("1"),
        "raw saturation requires isolated adapter diagnostics"
    );
    assert!(mount_rs_core::diagnostics::storage::enabled());
    let (nonce, prefix) = run_identity();
    let prepared_payload_bytes = PAYLOAD_COUNT
        .checked_mul(BLOCK_BYTES)
        .expect("prepared payload byte count must not overflow");
    assert!(
        prepared_payload_bytes <= 64 * 1024 * 1024,
        "prepared payload corpus must fit the 64 MiB data cap"
    );
    let corpus = Arc::new(
        (0..PAYLOAD_COUNT)
            .map(|index| payload(&nonce, index))
            .collect::<Vec<_>>(),
    );
    let budget = RawBlockCacheBudget::new(0, 0);
    let authority = Arc::new(
        RustFsBlockStore::from_config_with_cache_budget(&config, &prefix, true, budget.clone())
            .unwrap_or_else(|_| panic!("signed authority construction failed")),
    );
    let blocks = raw_blocks(&config, &prefix, budget.clone());
    let mut owned = Vec::<OwnedBlock>::with_capacity(PAYLOAD_COUNT);
    let mut seeds = Vec::with_capacity(READ_SEEDS);
    let mut quarantine = false;
    let mut workers_settled = true;
    let mut established_authority = None;
    let mut unacknowledged_payload_indices = Vec::with_capacity(MAX_WORKERS + 1);
    println!(
        "{}",
        json!({
            "event":"raw_saturation_owned_prefix","prefix":prefix,"block_bytes":BLOCK_BYTES,
            "payload_count":PAYLOAD_COUNT,"prepared_payload_bytes":prepared_payload_bytes,
            "workers_cap":MAX_WORKERS,"suite_limit_seconds":600,"cleanup_limit_seconds":30,
            "caller_prefix_used":false,"raw_cache_max_charged_bytes":0,"raw_cache_max_entries":0,
            "payload_generation_outside_timed_phases":true,"provider_request_and_copy_allocations_included":true,
            "authority_scope":"separate signed configured facade on the same generated prefix; existing bounded probe retry policy retained",
            "durability_scope":"caller declaration only; server disk cache, fsync and crash durability not qualified"
        })
    );
    let run: Result<(), &'static str> = async {
        match timeout_at((Instant::now() + CLEANUP).min(work_end), config.observe_owned_prefix_absence(&prefix)).await {
            Ok(Ok(true)) => {}
            _ => return Err("fresh_owned_prefix_absence_unproven"),
        }
        // Preflight can create marker/probe objects. A failed outcome retains the
        // generated diagnostic prefix; it never triggers range reconciliation.
        quarantine = true;
        let preflight_started = Instant::now();
        let expected = match timeout_at((Instant::now() + PHASE).min(work_end), authority.prepare_concurrent_backing()).await {
            Ok(Ok(expected)) => expected,
            _ => return Err("signed_backing_preparation_unproven"),
        };
        established_authority = Some(expected);
        println!("{}", json!({"event":"raw_saturation_backing_prepared","elapsed_ns":preflight_started.elapsed().as_nanos().to_string(),"authority_id":expected.to_hex(),"scope":"marker and independent-client probes; excluded from timed raw GET/PUT"}));
        match timeout_at((Instant::now() + REQUEST).min(work_end), authority.verify_concurrent_backing(expected)).await {
            Ok(Ok(())) => quarantine = false,
            _ => return Err("initial_backing_authority_unproven"),
        }
        let seed_end = (Instant::now() + PHASE).min(work_end);
        for (index, bytes) in corpus.iter().take(READ_SEEDS).enumerate() {
            if Instant::now() >= seed_end {
                quarantine = true;
                return Err("read_seed_budget_exhausted");
            }
            let request_end = (Instant::now() + REQUEST).min(seed_end);
            match timeout_at(request_end, blocks.put(bytes)).await {
                Ok(Ok(id)) => {
                    seeds.push(ReadSeed { payload_index: index, id: id.clone() });
                    owned.push(OwnedBlock { payload_index: index, id });
                }
                _ => {
                    quarantine = true;
                    unacknowledged_payload_indices.push(index);
                    return Err("seed_put_outcome_unproven");
                }
            }
        }
        let seeds = Arc::new(std::mem::take(&mut seeds));
        for (group, concurrency) in [1, 10, 100].into_iter().enumerate() {
            for pattern in [Pattern::Read, Pattern::Write { base: READ_SEEDS + group * WRITES_PER_PHASE }, Pattern::Authority] {
                if work_end.saturating_duration_since(Instant::now()) <= SETTLEMENT_RESERVE + Duration::from_secs(1) {
                    quarantine = true;
                    return Err("suite_work_budget_exhausted");
                }
                let result = phase(pattern, concurrency, Arc::clone(&blocks), Arc::clone(&authority), expected,
                                   Arc::clone(&corpus), Arc::clone(&seeds), &mut owned, &budget, work_end, ADMISSION).await;
                workers_settled &= result.workers_settled;
                unacknowledged_payload_indices.extend(result.counts.unacknowledged_payload_indices.iter().copied());
                quarantine |= !result.workers_settled || !result.all_workers_returned_metrics || !result.successful_work_observed || result.counts.write_outcome_unknown;
                if !result.qualified() {
                    // No later workload or retry follows an unsuccessful phase.
                    if matches!(pattern, Pattern::Authority) {
                        quarantine = true;
                    }
                    return Err("raw_saturation_phase_unqualified");
                }
            }
        }
        if !zero_cache(&budget) {
            quarantine = true;
            return Err("zero_raw_cache_unproven");
        }
        Ok(())
    }.await;
    // A returned read error also reaches this gate. Exact data cleanup always
    // requires a fresh successful authority check, not merely a past preflight.
    let mut cleanup_authority_verified = false;
    if !quarantine && workers_settled {
        if let Some(expected) = established_authority {
            cleanup_authority_verified = matches!(
                timeout_at(
                    (Instant::now() + REQUEST).min(work_end),
                    authority.verify_concurrent_backing(expected)
                )
                .await,
                Ok(Ok(()))
            );
            quarantine |= !cleanup_authority_verified;
        } else if !owned.is_empty() {
            quarantine = true;
        }
    }
    let cleanup = if !quarantine && workers_settled {
        delete_exact(&blocks, &owned, (Instant::now() + CLEANUP).min(suite_end)).await
    } else {
        CleanupResult {
            unattempted: owned.len(),
            ..CleanupResult::default()
        }
    };
    let clean = !quarantine
        && workers_settled
        && cleanup.failed == 0
        && cleanup.uncertain == 0
        && cleanup.unattempted == 0;
    println!(
        "{}",
        json!({
            "event":"raw_saturation_cleanup","prefix":prefix,"quarantined":quarantine,"workers_settled":workers_settled,
            "known_acknowledged_blocks":owned.len(),"deleted_exact_blocks":cleanup.deleted,"failed_deletes":cleanup.failed,
            "uncertain_deletes":cleanup.uncertain,"unattempted_deletes":cleanup.unattempted,
            "payload_cleanup_complete":clean,"backing_marker_retained":true,"range_deletion_or_reconcile_used":false,
            "cleanup_authority_verified":cleanup_authority_verified,
            "suite_elapsed_ns":suite_started.elapsed().as_nanos().to_string(),"run_failure":run.err(),
            "raw_cache_zero":zero_cache(&budget),"physical_disk_iops_claim":false
        })
    );
    if !clean {
        // Exact acknowledged identities survive in the private owner log. A
        // failed/timed-out PUT can additionally exist without an acknowledged
        // BlockId: retain the whole generated prefix for operator diagnosis.
        println!(
            "{}",
            json!({"event":"raw_saturation_quarantine_manifest","prefix":prefix,
            "acknowledged_blocks":owned.iter().map(|block|json!({"payload_index":block.payload_index,"block_id":block.id.0.as_str()})).collect::<Vec<_>>(),
            "unacknowledged_payload_indices":unacknowledged_payload_indices,
            "inventory_complete_if_worker_retirement_unproven":false,
            "unacknowledged_write_ids_available":false,"automatic_retry_or_prefix_reuse":false})
        );
    }
    assert!(run.is_ok(), "raw RustFS saturation phase did not qualify");
    assert!(clean, "exact owned payload cleanup did not qualify");
    assert!(
        suite_started.elapsed() < SUITE,
        "raw saturation exceeded suite budget"
    );
}

#[test]
fn prepared_payload_tuple_prevents_duplicate_put_inputs() {
    let nonce = [7; 32];
    for first in [0, 1, READ_SEEDS, PAYLOAD_COUNT - 1] {
        for second in [0, 1, READ_SEEDS, PAYLOAD_COUNT - 1] {
            assert_eq!(
                payload(&nonce, first) == payload(&nonce, second),
                first == second
            );
        }
    }
    assert_ne!(payload(&nonce, 0), payload(&[8; 32], 0));
}

#[test]
fn latency_histogram_uses_inclusive_power_of_two_microsecond_bounds() {
    assert_eq!(latency_bucket(Duration::ZERO), 0);
    assert_eq!(latency_bucket(Duration::from_nanos(1001)), 1);
    assert_eq!(latency_bucket(Duration::from_micros(2)), 1);
    assert_eq!(latency_bucket(Duration::from_micros(3)), 2);
    assert_eq!(latency_bucket(Duration::from_micros(4)), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn zero_admission_and_expired_deadline_do_not_qualify_or_dispatch_io() {
    // Real signed handles are only constructed. No fixture, server, mock I/O,
    // preparation, marker read or caller environment is involved: both paths
    // prohibit admission before any BlockStore operation can be dispatched.
    let config = RustFsConfig {
        endpoint: "http://127.0.0.1:1".to_owned(),
        bucket: "mount-rs-deadline-control".to_owned(),
        access_key_id: "unused-control-key".to_owned(),
        secret_access_key: "unused-control-secret".to_owned(),
        region: "us-east-1".to_owned(),
    };
    let (_, prefix) = run_identity();
    let budget = RawBlockCacheBudget::new(0, 0);
    let blocks = raw_blocks(&config, &prefix, budget.clone());
    let authority = Arc::new(
        RustFsBlockStore::from_config_with_cache_budget(&config, &prefix, true, budget.clone())
            .expect("constructor-only signed authority handle"),
    );
    let expected = ConcurrentBackingId::from_bytes([1; 16]).expect("nonzero control identity");
    let corpus = Arc::new(Vec::new());
    let seeds = Arc::new(Vec::new());
    let mut owned = Vec::new();
    let before = blocks.stats();
    let authority_before = authority.stats();
    let authority_http_before = serde_json::to_value(authority.http_observer().snapshot())
        .expect("serialize control HTTP counters");
    for concurrency in [1, 10, 100] {
        for pattern in [
            Pattern::Read,
            Pattern::Write { base: 0 },
            Pattern::Authority,
        ] {
            let result = timeout_at(
                Instant::now() + Duration::from_secs(5),
                phase(
                    pattern,
                    concurrency,
                    Arc::clone(&blocks),
                    Arc::clone(&authority),
                    expected,
                    Arc::clone(&corpus),
                    Arc::clone(&seeds),
                    &mut owned,
                    &budget,
                    Instant::now() + PHASE,
                    Duration::ZERO,
                ),
            )
            .await
            .expect("zero-admission workers must settle within the control deadline");
            // All workers crossed the actual barriers and returned metrics;
            // the previous success gate could qualify this empty workload.
            assert!(result.workers_settled);
            assert!(result.all_workers_returned_metrics);
            assert_eq!(result.counts.success, 0);
            assert_eq!(result.counts.failed, 0);
            assert_eq!(result.counts.uncertain, 0);
            assert!(!result.successful_work_observed);
            assert!(!result.qualified());
        }
    }
    for pattern in [
        Pattern::Read,
        Pattern::Write { base: 0 },
        Pattern::Authority,
    ] {
        let result = timeout_at(
            Instant::now() + Duration::from_secs(5),
            phase(
                pattern,
                10,
                Arc::clone(&blocks),
                Arc::clone(&authority),
                expected,
                Arc::clone(&corpus),
                Arc::clone(&seeds),
                &mut owned,
                &budget,
                Instant::now() + SETTLEMENT_RESERVE,
                ADMISSION,
            ),
        )
        .await
        .expect("expired-operation-deadline workers must retire within the reserve");
        assert!(result.workers_settled);
        assert_eq!(result.counts.success, 0);
        assert_eq!(result.counts.failed, 0);
        assert_eq!(result.counts.uncertain, 0);
        assert!(!result.successful_work_observed);
        assert!(!result.qualified());
    }
    assert!(owned.is_empty());
    assert!(zero_cache(&budget));
    assert_eq!(blocks.stats(), before);
    assert_eq!(authority.stats(), authority_before);
    assert_eq!(
        serde_json::to_value(authority.http_observer().snapshot())
            .expect("serialize settled control HTTP counters"),
        authority_http_before
    );
}
