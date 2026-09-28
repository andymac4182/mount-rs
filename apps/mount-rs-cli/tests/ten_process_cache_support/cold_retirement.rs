//! Small causal qualification through ten actual CLI servers, not a scale benchmark.
//! Private fixture values are resolved only by the owned worker at execution.
use super::{
    config::{CLUSTER, CacheSettings, Fixture, sha256, write},
    contracts::*,
    process::Fleet,
    progress_trace::{self, Label},
    scenario,
};
use mount_rs_blob_cache::{
    CacheScope, LocalCache, LocalCacheConfig, PeerEndpoint, PeerId, PeerTransport, QuicPeerConfig,
    QuicPeerTransport, ScopeIdentity,
};
use mount_rs_core::{
    FileHandle, FsDriver, Loopback,
    storage::{BlockId, BlockStore, MetadataStore, NodeData},
};
use mount_rs_rustfs::{RustFsBlockStore, RustFsConfig};
use mount_rs_sdk::{ConstructionJournal, Filesystem, SplitOptions, StorageContext, StoreConfig};
use mount_rs_tidb::{TidbMetadataStore, TidbPoolContext, TidbStorageOptions};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    io::BufReader,
    net::SocketAddr,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::task::JoinHandle;

const READY_SHA: &str = "50f2c006d5a947d9523b1a6b89dc280005b27e1faa530a217d8283535a555fd0";

#[derive(Clone, Copy, serde::Serialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
enum OraclePhase {
    Initialize { drive: usize },
    Fresh { drive: usize, round: FreshRound },
    PeerPartitionDenial,
}
#[derive(Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum FreshRound {
    Initial,
    Final,
}
static WORKER_OBSERVATION_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
fn worker_observation() -> super::object_store_projection::Captured {
    let observer = mount_rs_core::diagnostics::object_store::Observer::enabled();
    if !observer.is_enabled() {
        return super::object_store_projection::capture_worker(
            false,
            &WORKER_OBSERVATION_SEQUENCE,
            |_| None,
            || None,
            &mut std::io::sink(),
        );
    }
    super::object_store_projection::capture_worker(
        true,
        &WORKER_OBSERVATION_SEQUENCE,
        |sequence| {
            Some(mount_rs_service::object_store_diagnostics::Capture {
                pid: std::process::id(),
                sequence,
                observed_unix_ms: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_millis()
                    .try_into()
                    .ok()?,
                context: mount_rs_service::object_store_diagnostics::CaptureContext::WorkerBoundary,
                generation: None,
            })
        },
        || observer.snapshot(),
        &mut std::io::stderr().lock(),
    )
}

const CONFIG_SHA: &str = "7c43a6f84ca662faf2224ee5d9a5658c7645e2260326b95d1f3d424f8b4cfbb3";

#[derive(Clone)]
struct Backing {
    connection: String,
    blocks: RustFsConfig,
    stem: String,
}
impl Backing {
    fn from_private_environment() -> Result<Self> {
        let read = |name: &str| {
            std::env::var(name).map_err(|_| format!("missing private fixture variable {name}"))
        };
        if read("MOUNT_RS_TEN_PROCESS_FIXTURE_READY_SHA256")? != READY_SHA
            || read("MOUNT_RS_TEN_PROCESS_FIXTURE_CONFIG_SHA256")? != CONFIG_SHA
            || read("MOUNT_RS_TIDB_DURABLE")? != "1"
            || read("MOUNT_RS_RUSTFS_DURABLE")? != "1"
        {
            return Err("exact original fixture/durable declarations required".into());
        }
        let mut nonce = [0_u8; 16];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| "owned namespace entropy unavailable")?;
        let suffix = nonce.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let backing = Self {
            connection: read("MOUNT_RS_TIDB_URL")?,
            blocks: RustFsConfig {
                endpoint: read("MOUNT_RS_RUSTFS_ENDPOINT")?,
                bucket: read("MOUNT_RS_RUSTFS_BUCKET")?,
                region: read("MOUNT_RS_RUSTFS_REGION")?,
                access_key_id: read("MOUNT_RS_RUSTFS_ACCESS_KEY_ID")?,
                secret_access_key: read("MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY")?,
            },
            stem: format!("mount-rs-ten-cold-{suffix}"),
        };
        backing
            .blocks
            .validate()
            .map_err(|_| "invalid private RustFS configuration")?;
        Ok(backing)
    }
    fn key(&self, n: usize) -> String {
        format!("{}-drive-{n}", self.stem)
    }
    fn prefix(&self, n: usize) -> String {
        format!("{}/drive-{n}", self.stem)
    }
    fn split(&self, n: usize, owner: &str) -> SplitOptions {
        let mut options = SplitOptions::memory(owner, BLOCK_BYTES).with_compact_inode_updates(true);
        options.metadata = StoreConfig::Tidb {
            connection: self.connection.clone(),
            volume_key: self.key(n),
            durable: true,
        };
        options.blocks = StoreConfig::RustFs {
            endpoint: self.blocks.endpoint.clone(),
            bucket: self.blocks.bucket.clone(),
            region: self.blocks.region.clone(),
            prefix: self.prefix(n),
            access_key_id: self.blocks.access_key_id.clone(),
            secret_access_key: self.blocks.secret_access_key.clone(),
            durable: true,
        };
        options
    }
    fn install_catalog(&self, fixture: &mut Fixture) -> Result<()> {
        for n in 0..NODES {
            let storage = fixture
                .document
                .pointer_mut(&format!(
                    "/partitions/{}/drives/{}/driver/storage",
                    partition(n),
                    drive(n)
                ))
                .ok_or("catalog storage route absent")?;
            *storage = json!({
                "metadata":{"kind":"tidb","connection":{"env":"MOUNT_RS_TIDB_URL"},"volume_key":self.key(n),"durable":true},
                "blocks":{"kind":"rustfs","endpoint":self.blocks.endpoint,"bucket":self.blocks.bucket,"region":self.blocks.region,
                    "prefix":self.prefix(n),"access_key_id":{"env":"MOUNT_RS_RUSTFS_ACCESS_KEY_ID"},
                    "secret_access_key":{"env":"MOUNT_RS_RUSTFS_SECRET_ACCESS_KEY"},"durable":true},
                "chunk_size_bytes":BLOCK_BYTES,"concurrent_writes":true,"inode_updates":true,"compact_inode_updates":true
            });
        }
        // Private ownership record contains generated names, never credential values.
        write(
            &fixture.root.join("owned-durable-scopes.json"),
            json!({
                "schema":1,"fixture_ready_sha256":READY_SHA,"fixture_config_sha256":CONFIG_SHA,
                "namespaces":(0..NODES).map(|n| self.key(n)).collect::<Vec<_>>(),
                "prefixes":(0..NODES).map(|n| self.prefix(n)).collect::<Vec<_>>(),
                "absence_is_observation_not_reservation":true,"retained_test_data":true
            })
            .to_string(),
        )
    }
}

// These are the actual resources, installed in the owner before the next await.
// Reverse close order keeps writer authority/handles ahead of shared pools.
#[derive(Clone)]
enum Resource {
    Pool(TidbPoolContext),
    Metadata(Arc<TidbMetadataStore>),
    Blocks(Arc<RustFsBlockStore>),
    Context(StorageContext),
    Journal(ConstructionJournal),
    Filesystem(Arc<Filesystem>),
    Handle(Arc<dyn FileHandle>),
    Local(Arc<LocalCache>),
    Peer(Arc<QuicPeerTransport>),
}
impl Resource {
    async fn close(&self) -> Result<()> {
        let result = match self {
            Self::Pool(value) => value.close().await,
            Self::Metadata(value) => value.close().await,
            Self::Blocks(value) => value.flush().await,
            Self::Context(value) => value.close().await,
            Self::Journal(value) if value.snapshot().handed_off => return Ok(()),
            Self::Journal(value) => value.close().await,
            Self::Filesystem(value) => value.shutdown().await,
            Self::Handle(value) => value.close().await,
            Self::Local(value) => value.shutdown().await,
            Self::Peer(value) => value.shutdown().await,
        };
        result.map_err(|_| "actual private oracle resource close failed".into())
    }
}
#[derive(Default)]
struct Retained {
    resources: Vec<(Resource, bool)>,
    cleanup_complete: bool,
    cleanup_failure: Option<String>,
}
impl Retained {
    fn add(&mut self, resource: Resource) -> usize {
        let index = self.resources.len();
        self.resources.push((resource, false));
        index
    }
}
fn retain(owner: &Arc<Mutex<Retained>>, resource: Resource) -> Result<usize> {
    Ok(owner
        .lock()
        .map_err(|_| "private oracle owner poisoned")?
        .add(resource))
}
async fn close_retained(owner: &Arc<Mutex<Retained>>, deadline: Instant) -> Result<()> {
    let length = owner
        .lock()
        .map_err(|_| "private oracle owner poisoned")?
        .resources
        .len();
    for index in (0..length).rev() {
        let resource = {
            let state = owner.lock().map_err(|_| "private oracle owner poisoned")?;
            if state.resources[index].1 {
                continue;
            }
            state.resources[index].0.clone()
        };
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), resource.close())
            .await
            .map_err(|_| "private oracle close deadline exhausted; owner retained")??;
        if Instant::now() >= deadline {
            return Err("late private oracle resource close; owner retained".into());
        }
        owner
            .lock()
            .map_err(|_| "private oracle owner poisoned")?
            .resources[index]
            .1 = true;
    }
    owner
        .lock()
        .map_err(|_| "private oracle owner poisoned")?
        .cleanup_complete = true;
    Ok(())
}

struct Oracle {
    scopes: Vec<CacheScope>,
    ids: Vec<Vec<BlockId>>,
    report: Value,
}
impl Oracle {
    fn empty() -> Self {
        Self {
            scopes: Vec::new(),
            ids: Vec::new(),
            report: json!({}),
        }
    }
}
struct Job {
    task: Option<JoinHandle<Result<Oracle>>>,
    resources: Arc<Mutex<Retained>>,
    observation: Arc<Mutex<Value>>,
    settled: bool,
}
#[derive(Default)]
pub struct OracleOwner {
    jobs: Vec<Job>,
    failure: Option<String>,
}
impl OracleOwner {
    fn start<F, Fut>(&mut self, parent: Instant, phase: OraclePhase, action: F) -> Result<usize>
    where
        F: FnOnce(Arc<Mutex<Retained>>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Oracle>> + Send + 'static,
    {
        let deadline = clipped(Instant::now(), parent, REQUEST_SECONDS)?;
        let resources = Arc::new(Mutex::new(Retained::default()));
        let index = self.jobs.len();
        let observation = Arc::new(Mutex::new(
            json!({"status":"unavailable","reason":"not_captured"}),
        ));
        self.jobs.push(Job {
            task: None,
            resources: resources.clone(),
            observation: observation.clone(),
            settled: false,
        });
        let task = tokio::spawn(async move {
            let before = {
                let _capture = progress_trace::span(Label::OracleCaptureBefore);
                worker_observation()
            };
            if let Ok(mut recorded) = observation.lock() {
                *recorded = json!({"status":"unavailable","reason":"missing_after",
                    "owner":{"pid":std::process::id(),"job":index,"phase":phase,"stderr":"worker.stderr","server_generation":Value::Null},
                    "before":before.evidence});
            }
            let result = {
                let action = action(resources.clone());
                tokio::pin!(action);
                match tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    std::future::poll_fn(|context| {
                        progress_trace::poll(action.as_mut(), context, Label::OracleActionPoll)
                    }),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err("private oracle operation deadline exhausted; no replay".into()),
                }
            };
            // Cancellation of a waiter leaves this actual task and every partial owner retained.
            let cleanup_deadline = clipped(Instant::now(), parent, SHUTDOWN_SECONDS)?;
            let cleanup = {
                let cleanup = close_retained(&resources, cleanup_deadline);
                tokio::pin!(cleanup);
                std::future::poll_fn(|context| {
                    progress_trace::poll(cleanup.as_mut(), context, Label::OracleCleanupPoll)
                })
                .await
            };
            if let Err(error) = &cleanup
                && let Ok(mut state) = resources.lock()
            {
                state.cleanup_failure = Some(error.clone());
            }
            let after = {
                let _capture = progress_trace::span(Label::OracleCaptureAfter);
                worker_observation()
            };
            let projected = super::object_store_projection::project_worker(
                &super::object_store_projection::WorkerBinding {
                    pid: std::process::id(),
                    job: index as u64,
                    phase: json!(phase),
                },
                &before,
                &after,
            );
            if let Ok(mut recorded) = observation.lock() {
                *recorded = projected;
            }
            match (result, cleanup) {
                (Ok(value), Ok(())) => Ok(value),
                (Err(error), _) => Err(error),
                (_, Err(error)) => Err(error),
            }
        });
        self.jobs[index].task = Some(task);
        Ok(index)
    }
    async fn wait(&mut self, fleet: &mut Fleet, index: usize, deadline: Instant) -> Result<Oracle> {
        scenario::checked(fleet, deadline, async {
            let job = &mut self.jobs[index];
            let result = job
                .task
                .as_mut()
                .ok_or("private oracle task owner missing")?
                .await;
            job.settled = true;
            result.map_err(|_| "private oracle task join failed; owner retained".to_owned())?
        })
        .await
    }
    pub async fn settle(&mut self, deadline: Instant) -> Result<()> {
        for job in &mut self.jobs {
            if !job.settled {
                let joined = tokio::time::timeout_at(
                    tokio::time::Instant::from_std(deadline),
                    job.task
                        .as_mut()
                        .ok_or("private oracle task owner missing")?,
                )
                .await;
                match joined {
                    Ok(Ok(_)) => job.settled = true,
                    Ok(Err(_)) => {
                        job.settled = true;
                        self.failure
                            .get_or_insert("private oracle join failed".into());
                    }
                    Err(_) => {
                        self.failure.get_or_insert(
                            "private oracle join deadline exhausted; actual owner retained".into(),
                        );
                    }
                }
            }
            let mut state = job
                .resources
                .lock()
                .map_err(|_| "private oracle owner poisoned")?;
            if !job.settled || !state.cleanup_complete || state.cleanup_failure.is_some() {
                self.failure.get_or_insert(
                    "private oracle resource cleanup unproven; exact owners retained".into(),
                );
            } else {
                state.resources.clear();
            }
        }
        if self.jobs.is_empty() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            self.failure
                .get_or_insert("late private oracle settlement".into());
        }
        self.failure.clone().map_or(Ok(()), Err)
    }
    pub fn snapshot(&self) -> Value {
        let rows = self.jobs.iter().map(|job| {
            let mut row = match job.resources.lock() {
                Ok(state) => json!({"task_settled":job.settled,"cleanup_complete":state.cleanup_complete,
                    "retained_resources":state.resources.len(),"cleanup_failure":state.cleanup_failure}),
                Err(_) => json!({"task_settled":job.settled,"cleanup_complete":false,"owner_poisoned":true}),
            };
            row["object_store_observation"] = job.observation.lock().map(|value| value.clone())
                .unwrap_or_else(|_| json!({"status":"unavailable","reason":"observation_owner_poisoned"}));
            row
        }).collect::<Vec<_>>();
        json!({"jobs":rows,"failure":self.failure,"scope":"actual private provider/SDK tasks and retained resources; process reap is separate"})
    }
}

impl Drop for OracleOwner {
    fn drop(&mut self) {
        // This also runs on receipt-write/assertion unwinding, before the worker
        // runtime drops. Process containment may cancel runtime tasks, but must
        // not discard their actual uncertain provider/journal owner objects.
        for job in self.jobs.drain(..) {
            let proven = job.settled
                && match job.resources.lock() {
                    Ok(state) => {
                        state.cleanup_complete
                            && state.cleanup_failure.is_none()
                            && state.resources.iter().all(|(_, closed)| *closed)
                    }
                    Err(_) => false,
                };
            if !proven {
                // Deliberate retention until this independently owned worker
                // process exits. No replacement/retry or cleanup ACK is implied.
                std::mem::forget(job);
            }
        }
    }
}

async fn open_sdk(
    backing: &Backing,
    n: usize,
    owner: &Arc<Mutex<Retained>>,
) -> Result<Arc<Filesystem>> {
    let context = StorageContext::new(16).map_err(|_| "private SDK context creation failed")?;
    retain(owner, Resource::Context(context.clone()))?;
    let journal = ConstructionJournal::new();
    retain(owner, Resource::Journal(journal.clone()))?;
    let attempt = journal
        .begin()
        .map_err(|_| "private SDK construction journal unavailable")?;
    let opened = {
        let opened = Filesystem::split_with_context_and_construction_observer(
            backing.split(n, "owned-ten-cli-fresh-oracle"),
            &context,
            &journal,
        );
        tokio::pin!(opened);
        std::future::poll_fn(|context| {
            progress_trace::poll(opened.as_mut(), context, Label::OracleSdkOpenPoll)
        })
        .await
    };
    match opened {
        Ok(filesystem) => {
            let filesystem = Arc::new(filesystem);
            retain(owner, Resource::Filesystem(filesystem.clone()))?;
            attempt
                .handoff()
                .map_err(|_| "private SDK construction handoff failed; owner retained")?;
            Ok(filesystem)
        }
        Err(_) => {
            attempt.fail();
            Err("actual private SDK construction failed; owner retained".into())
        }
    }
}
async fn initialize(backing: Backing, n: usize, owner: Arc<Mutex<Retained>>) -> Result<Oracle> {
    let pool = TidbPoolContext::new(&backing.connection, 16)
        .map_err(|_| "private TiDB context creation failed")?;
    retain(&owner, Resource::Pool(pool.clone()))?;
    let namespace_absent = {
        let key = backing.key(n);
        let observation = pool.inspect_namespace_presence(&key);
        tokio::pin!(observation);
        std::future::poll_fn(|context| {
            progress_trace::poll(observation.as_mut(), context, Label::OracleNamespacePoll)
        })
        .await
        .map_err(|_| "private namespace absence observation failed")?
        .is_absent()
    };
    if !namespace_absent
        || !{
            let prefix = backing.prefix(n);
            let observation = backing.blocks.observe_owned_prefix_absence(&prefix);
            tokio::pin!(observation);
            std::future::poll_fn(|context| {
                progress_trace::poll(observation.as_mut(), context, Label::OraclePrefixPoll)
            })
            .await
            .map_err(|_| "private block prefix absence observation failed")?
        }
    {
        return Err("generated owned namespace/prefix is not absent; no overwrite".into());
    }
    open_sdk(&backing, n, &owner).await?;
    Ok(Oracle::empty())
}
async fn fresh(backing: Backing, n: usize, owner: Arc<Mutex<Retained>>) -> Result<Oracle> {
    let pool = TidbPoolContext::new(&backing.connection, 16)
        .map_err(|_| "fresh TiDB pool creation failed")?;
    retain(&owner, Resource::Pool(pool.clone()))?;
    let metadata = Arc::new(
        pool.metadata(TidbStorageOptions::new(backing.key(n)).with_durable(true))
            .await
            .map_err(|_| "fresh TiDB metadata open failed")?,
    );
    retain(&owner, Resource::Metadata(metadata.clone()))?;
    let mode = metadata
        .compact_inode_mode_state()
        .await
        .map_err(|_| "fresh TiDB compact mode read failed")?
        .ok_or("fresh TiDB has no actual MRC5 compact mode")?;
    let snapshot = metadata
        .load_compact_snapshot(mode.backing)
        .await
        .map_err(|_| "fresh TiDB compact snapshot read failed")?;
    let namespace = snapshot
        .namespace()
        .map_err(|_| "fresh compact namespace invalid")?;
    if namespace.nodes.len() != FILES + 1 || snapshot.anchor.members.len() != FILES + 1 {
        return Err("fresh TiDB namespace membership mismatch".into());
    }
    let root = namespace
        .nodes
        .get(&namespace.root)
        .ok_or("fresh root missing")?;
    let NodeData::Directory { entries } = &root.data else {
        return Err("fresh root data is not a directory".into());
    };
    if !root.stats.is_directory() || entries.len() != FILES {
        return Err("fresh root type/entries mismatch".into());
    }
    let blocks = Arc::new(
        RustFsBlockStore::from_config(&backing.blocks, backing.prefix(n), true)
            .map_err(|_| "fresh signed RustFS block provider construction failed")?,
    );
    retain(&owner, Resource::Blocks(blocks.clone()))?;
    blocks
        .verify_concurrent_backing(mode.backing)
        .await
        .map_err(|_| "fresh RustFS authority mismatch")?;
    let mut ids = Vec::new();
    let mut digests = Vec::new();
    for file in 0..FILES {
        let item = entries
            .iter()
            .find(|item| item.name == format!("file-{file}"))
            .ok_or("fresh expected file missing")?;
        let node = namespace
            .nodes
            .get(&item.inode)
            .ok_or("fresh file guard missing")?;
        let NodeData::File(layout) = &node.data else {
            return Err("fresh expected file data type mismatch".into());
        };
        if !node.stats.is_file()
            || node.stats.size != BLOCK_BYTES as u64
            || layout.extents.len() != 1
        {
            return Err("fresh file type/length/extent count mismatch".into());
        }
        let extent = &layout.extents[0];
        if extent.file_offset != 0
            || extent.block_offset != 0
            || extent.length != BLOCK_BYTES as u64
        {
            return Err("fresh exact extent bounds mismatch".into());
        }
        let bytes = blocks
            .get(&extent.block)
            .await
            .map_err(|_| "fresh undecorated RustFS block read failed")?;
        if bytes != payload(n, file) {
            return Err("fresh raw provider payload mismatch".into());
        }
        ids.push(extent.block.clone());
        digests.push(sha256(&bytes));
    }
    // Independent undecorated public SDK path, including explicit EOF and handle close.
    let filesystem = open_sdk(&backing, n, &owner).await?;
    let driver = filesystem.driver();
    for file in 0..FILES {
        let handle = driver
            .open(&format!("/file-{file}"), "r", 0)
            .await
            .map_err(|_| "fresh SDK file open failed")?;
        let slot = retain(&owner, Resource::Handle(handle.clone()))?;
        let mut bytes = vec![0_u8; BLOCK_BYTES];
        let mut offset = 0;
        while offset < BLOCK_BYTES {
            let count = handle
                .read(&mut bytes[offset..], Some(offset as u64))
                .await
                .map_err(|_| "fresh SDK read failed")?;
            if count == 0 || count > BLOCK_BYTES - offset {
                return Err("fresh SDK read framing mismatch".into());
            }
            offset += count;
        }
        let mut tail = [0_u8; 1];
        if bytes != payload(n, file)
            || handle
                .read(&mut tail, Some(BLOCK_BYTES as u64))
                .await
                .map_err(|_| "fresh SDK EOF failed")?
                != 0
        {
            return Err("fresh SDK bytes/EOF mismatch".into());
        }
        handle
            .close()
            .await
            .map_err(|_| "fresh SDK handle close failed; owner retained")?;
        owner
            .lock()
            .map_err(|_| "private oracle owner poisoned")?
            .resources[slot]
            .1 = true;
    }
    let scope = CacheScope {
        identity: ScopeIdentity {
            cluster: CLUSTER.into(),
            partition: partition(n),
            drive: drive(n),
        },
        backing: mode.backing,
    };
    Ok(Oracle {
        scopes: vec![scope],
        ids: vec![ids],
        report: json!({
            "partition":partition(n),"drive":drive(n),"write_mode":"MRC5",
            "mode_evidence":"actual TiDB compact_inode_mode_state plus complete compact snapshot and exact RustFS backing verification",
            "backing":mode.backing.to_hex(),"anchor_generation":snapshot.anchor.generation,
            "members":snapshot.anchor.members.len(),"nodes":namespace.nodes.len(),"files":FILES,
            "bytes_each":BLOCK_BYTES,"byte_sha256":digests,"explicit_sdk_eof":true,
            "oracle":"fresh typed TiDB metadata + undecorated signed RustFS raw provider bytes + independent undecorated SDK",
            "raw_http_attempts":"unavailable; provider get is a logical call, not a wire attempt"
        }),
    })
}
async fn fresh_all(
    owner: &mut OracleOwner,
    fleet: &mut Fleet,
    backing: &Backing,
    deadline: Instant,
    round: FreshRound,
) -> Result<Oracle> {
    let mut result = Oracle::empty();
    let mut rows = Vec::new();
    for n in 0..NODES {
        let backing = backing.clone();
        let index = owner.start(
            deadline,
            OraclePhase::Fresh { drive: n, round },
            move |resources| fresh(backing, n, resources),
        )?;
        let value = owner.wait(fleet, index, deadline).await?;
        result.scopes.extend(value.scopes);
        result.ids.extend(value.ids);
        rows.push(value.report);
    }
    result.report = json!(rows);
    Ok(result)
}

fn launch(
    fleet: &mut Fleet,
    fixture: &Fixture,
    node: usize,
    directory: &Path,
    peers: &[usize],
    restrict_receiver: bool,
    deadline: Instant,
) -> Result<u64> {
    let generation = fleet.generation();
    let config = fixture.service_config(
        node,
        generation,
        CacheSettings {
            mode: "peer-query",
            ram_bytes: RAM_BYTES,
            disk_bytes: DISK_BYTES,
            peers,
            directory,
        },
    )?;
    let mut document: Value = serde_json::from_slice(
        &std::fs::read(&config).map_err(|_| "private service config read failed")?,
    )
    .map_err(|_| "private service config invalid")?;
    document["max_active_drives"] = json!(1);
    // Only the holder receiver trusts node-1 for partition0. The outbound
    // address remains the actual requester endpoint; no routing mock is used.
    if restrict_receiver {
        let peers = document
            .pointer_mut("/cache/peers")
            .and_then(Value::as_array_mut)
            .ok_or("peer array absent")?;
        if peers.len() != 1 || peers[0]["id"] != "node-1" {
            return Err("isolated holder peer identity mismatch".into());
        }
        peers[0]["partitions"] = json!([partition(0)]);
    }
    write(&config, document.to_string())?;
    fleet.launch(
        node,
        generation,
        &config,
        fixture.peer_addresses[node],
        directory,
        deadline,
    )?;
    Ok(generation)
}
async fn read_drive(
    fleet: &mut Fleet,
    fixture: &Fixture,
    node: usize,
    n: usize,
    deadline: Instant,
) -> Result<()> {
    let (connection, driver) = scenario::connect(fleet, fixture, node, n, deadline).await?;
    let view = Loopback::from_arc(driver);
    let result = scenario::checked(fleet, deadline, async {
        view.read_file("/file-0")
            .await
            .map_err(|_| "signed public CLI read failed".to_owned())
    })
    .await;
    connection.close();
    if result? != payload(n, 0) {
        return Err("signed public CLI exact drive payload mismatch".into());
    }
    Ok(())
}

struct PeerPartitionProbe {
    root: std::path::PathBuf,
    holder_address: SocketAddr,
    roots: rustls::RootCertStore,
    pin: String,
    allowed: CacheScope,
    allowed_id: BlockId,
    denied: CacheScope,
    denied_id: BlockId,
}
async fn peer_partition_denial(
    probe: PeerPartitionProbe,
    owner: Arc<Mutex<Retained>>,
) -> Result<Oracle> {
    let PeerPartitionProbe {
        root,
        holder_address,
        roots,
        pin,
        allowed,
        allowed_id,
        denied,
        denied_id,
    } = probe;
    let certificates = rustls_pemfile::certs(&mut BufReader::new(
        std::fs::File::open(root.join("cert-1.pem"))
            .map_err(|_| "owned peer probe certificate absent")?,
    ))
    .collect::<std::result::Result<Vec<_>, _>>()
    .map_err(|_| "owned peer probe certificate invalid")?;
    let private_key = rustls_pemfile::private_key(&mut BufReader::new(
        std::fs::File::open(root.join("key-1.pem")).map_err(|_| "owned peer probe key absent")?,
    ))
    .map_err(|_| "owned peer probe key invalid")?
    .ok_or("owned peer probe key missing")?;
    let mut certificate_sha256 = [0_u8; 32];
    if pin.len() != 64 {
        return Err("owned holder pin invalid".into());
    }
    for (index, byte) in certificate_sha256.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&pin[index * 2..index * 2 + 2], 16)
            .map_err(|_| "owned holder pin invalid")?;
    }
    let local = LocalCache::new(LocalCacheConfig {
        directory: root.join("cache-tidb-peer-negative"),
        memory_bytes: RAM_BYTES,
        disk_bytes: DISK_BYTES,
        max_entries: 4096,
        max_blob_bytes: 64 * 1024,
    })
    .map_err(|_| "owned peer probe cache creation failed")?;
    retain(&owner, Resource::Local(local.clone()))?;
    let peer = QuicPeerTransport::bind(
        QuicPeerConfig {
            local: PeerId("node-1".into()),
            bind: "127.0.0.1:0"
                .parse()
                .map_err(|_| "fixed peer probe bind invalid")?,
            certificates,
            private_key,
            roots,
            // The caller deliberately permits the denied partition: this must not
            // succeed merely because an outbound caller whitelist rejected it.
            trusted: BTreeMap::from([(
                PeerId("node-0".into()),
                PeerEndpoint {
                    address: holder_address,
                    server_name: "localhost".into(),
                    certificate_sha256,
                    partitions: BTreeSet::from([
                        allowed.identity.partition.clone(),
                        denied.identity.partition.clone(),
                    ]),
                },
            )]),
            max_blob_bytes: 64 * 1024,
            max_inflight: 128,
            transfer_bytes: 8 * 1024 * 1024,
            deadline: Duration::from_millis(500),
        },
        local,
    )
    .map_err(|_| "owned peer probe bind failed")?;
    retain(&owner, Resource::Peer(peer.clone()))?;
    for after in [false, true] {
        let bytes = peer
            .get_shared(&PeerId("node-0".into()), &allowed, &allowed_id)
            .await
            .map_err(|_| "real allowed peer probe failed")?
            .ok_or("real allowed peer probe omitted cached bytes")?;
        if bytes.as_ref() != payload(0, 0) {
            return Err("real allowed peer probe payload mismatch".into());
        }
        if !after
            && peer
                .get_shared(&PeerId("node-0".into()), &denied, &denied_id)
                .await
                .is_ok()
        {
            return Err("actual peer receiver accepted cross-partition scope".into());
        }
    }
    Ok(Oracle {
        report: json!({"allowed_real_get_before_and_after":true,
        "caller_outbound_allowlist_permits_denied_partition":true,"actual_holder_receiver_allowlist":[partition(0)],
        "denied_scope_is_exact_known_warm_backing_and_block":true,
        "denied_runtime_is_verified_and_resident_under_capacity1":true,"denied_result":"transport error; no bytes",
        "receiver_event_count":"unavailable; source receiver whitelist precedes cold admission; positive wire controls bracket denial"}),
        ..Oracle::empty()
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Holder {
    configured_routes: usize,
    retained_proof_groups: usize,
    admitted_proofs: usize,
    retained_inspectors: usize,
    sealed: bool,
    inspection_starts: u64,
    inspection_successes: u64,
    authority_refusals: u64,
    proof_reuses: u64,
}
fn parse_holder(bytes: &[u8]) -> Result<Holder> {
    if bytes.len() as u64 > FILE_CAP || bytes.last() != Some(&b'\n') {
        return Err("holder log incomplete/over cap".into());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "holder log invalid UTF8")?;
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.len() > LINE_CAP {
            return Err("holder log line exceeds bound".into());
        }
        if line.starts_with("cache_holder") && !line.starts_with("cache_holder ") {
            return Err("malformed holder frame prefix".into());
        }
        if let Some(value) = line.strip_prefix("cache_holder ") {
            rows.push(
                serde_json::from_str::<Holder>(value)
                    .map_err(|_| "holder frame invalid/unknown/duplicate field")?,
            );
        }
    }
    if rows.len() != 1 {
        return Err("exactly one complete holder shutdown frame required".into());
    }
    let row = rows.remove(0);
    if row.configured_routes != NODES
        || !row.sealed
        || row.retained_proof_groups != 0
        || row.admitted_proofs != 0
        || row.retained_inspectors != 0
        || row.inspection_starts == 0
        || row.inspection_successes == 0
        || row.inspection_successes > row.inspection_starts
        || row.authority_refusals != 0
    {
        return Err("actual holder inspection/shutdown contract mismatch".into());
    }
    Ok(row)
}
fn holder(fixture: &Fixture, node: usize, generation: u64) -> Result<Holder> {
    let path = fixture
        .root
        .join(format!("server-node-{node}-{generation}.stderr"));
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| "owned holder log absent")?;
    if !metadata.file_type().is_file() || metadata.len() > FILE_CAP {
        return Err("owned holder log not a bounded regular file".into());
    }
    parse_holder(&std::fs::read(path).map_err(|_| "owned holder log read failed")?)
}

pub async fn run(
    fleet: &mut Fleet,
    fixture: &mut Fixture,
    owner: &mut OracleOwner,
    phases: &mut Vec<Value>,
    setup: Instant,
) -> Result<()> {
    let backing = Backing::from_private_environment()?;
    backing.install_catalog(fixture)?;
    let config = fixture.apply_config(0)?;
    fleet.apply(&config, clipped(Instant::now(), setup, REQUEST_SECONDS)?)?;
    // Observe exact generated absence before sequential virgin MRC5 enrollment.
    for n in 0..NODES {
        let backing = backing.clone();
        let index = owner.start(
            setup,
            OraclePhase::Initialize { drive: n },
            move |resources| initialize(backing, n, resources),
        )?;
        owner.wait(fleet, index, setup).await?;
    }
    fixture.release_reservations();
    for n in 0..NODES {
        launch(
            fleet,
            fixture,
            n,
            &scenario::cache_path(fixture, &format!("tidb-seed-{n}")),
            &[],
            false,
            setup,
        )?;
    }
    let seed_pids = fleet
        .processes
        .iter()
        .map(|process| process.receipt.pid)
        .collect::<BTreeSet<_>>();
    if seed_pids.len() != NODES {
        return Err("ten simultaneous signed seed CLI PIDs absent".into());
    }
    for n in 0..NODES {
        let (connection, driver) = scenario::connect(fleet, fixture, n, n, setup).await?;
        let write_result = async {
            for file in 0..FILES {
                scenario::checked(fleet, setup, async {
                    driver
                        .write_file(&format!("/file-{file}"), &payload(n, file))
                        .await
                        .map_err(|_| "signed seed write failed".to_owned())
                })
                .await?;
            }
            scenario::checked(fleet, setup, async {
                driver
                    .syncfs()
                    .await
                    .map_err(|_| "signed seed syncfs failed".to_owned())
            })
            .await
        }
        .await;
        connection.close();
        write_result?;
    }
    fleet.stop_all(clipped(Instant::now(), setup, SHUTDOWN_SECONDS)?)?;
    let initial = fresh_all(owner, fleet, &backing, setup, FreshRound::Initial).await?;
    phases.push(json!({"phase":"tidb-rustfs-signed-seed-fresh-oracle","pids":seed_pids,"oracle":initial.report,
        "transport":"signed OIDC QUIC public CLI","ten_cli_processes_simultaneously_live":true,
        "write_ack_scope":"signed RPC completion + syncfs + fresh TiDB/RustFS metadata/raw bytes/SDK EOF",
        "fixture_identity":"original private ready/config hashes declared; root live eight-role refresh is a separate required gate"}));
    let work = Instant::now() + Duration::from_secs(WORK_SECONDS);
    let phase = clipped(Instant::now(), work, PHASE_SECONDS)?;
    let holder_cache = scenario::cache_path(fixture, "tidb-cold-holder");
    // Positively close every actual maintenance/provider owner in an earlier
    // no-peer generation. The next holder reads only these complete local entries:
    // source local hits never enqueue placement leases in CachedBlockStore::get.
    let prewarm_generation = launch(fleet, fixture, 0, &holder_cache, &[], false, phase)?;
    for n in [2, 0, 1] {
        read_drive(fleet, fixture, 0, n, phase).await?;
        scenario::observe_entry(
            fleet,
            &scenario::entry(&holder_cache, &initial.scopes[n], &initial.ids[n][0]),
            &payload(n, 0),
            phase,
        )
        .await?;
    }
    let prewarm_bank = scenario::stop(fleet, 0, phase)?;
    prewarm_bank.expect(0, 0, 1, 0)?;
    if prewarm_bank.cache_errors != 0 {
        return Err("prewarm cache errors prohibit causal cell".into());
    }
    let holder_generation = launch(fleet, fixture, 0, &holder_cache, &[1], true, phase)?;
    // Ten simultaneous CLI PIDs also remain live at the ordered A -> B retirement.
    for n in 1..NODES {
        launch(
            fleet,
            fixture,
            n,
            &scenario::cache_path(fixture, &format!("tidb-idle-{n}")),
            &[],
            false,
            phase,
        )?;
    }
    let cold_pids = fleet
        .processes
        .iter()
        .map(|process| process.receipt.pid)
        .collect::<BTreeSet<_>>();
    if cold_pids.len() != NODES {
        return Err("cold retirement did not retain ten simultaneous CLI PIDs".into());
    }
    // Warm an exact real forbidden tenant block before the ordered A -> B cell.
    read_drive(fleet, fixture, 0, 2, phase).await?;
    scenario::observe_entry(
        fleet,
        &scenario::entry(&holder_cache, &initial.scopes[2], &initial.ids[2][0]),
        &payload(2, 0),
        phase,
    )
    .await?;
    read_drive(fleet, fixture, 0, 0, phase).await?;
    scenario::observe_entry(
        fleet,
        &scenario::entry(&holder_cache, &initial.scopes[0], &initial.ids[0][0]),
        &payload(0, 0),
        phase,
    )
    .await?;
    read_drive(fleet, fixture, 0, 1, phase).await?;
    // Both signed views closed. Capacity1 and distinct B bytes require A's real eviction shutdown.
    // Remove idle requester only after retirement; each measurement gets a fresh empty directory.
    scenario::stop(fleet, 1, phase)?.expect(0, 0, 0, 0)?;
    let peer_cache = scenario::cache_path(fixture, "tidb-cold-requester-empty");
    launch(fleet, fixture, 1, &peer_cache, &[0], false, phase)?;
    read_drive(fleet, fixture, 1, 0, phase).await?;
    let peer = scenario::stop(fleet, 1, phase)?;
    peer.expect(0, 1, 0, BLOCK_BYTES as u64)?;
    // A second actual retirement retains the same healthy cold proof across another peer read.
    read_drive(fleet, fixture, 0, 0, phase).await?;
    read_drive(fleet, fixture, 0, 1, phase).await?;
    launch(
        fleet,
        fixture,
        1,
        &scenario::cache_path(fixture, "tidb-cold-proof-reuse-empty"),
        &[0],
        false,
        phase,
    )?;
    read_drive(fleet, fixture, 1, 0, phase).await?;
    let reuse = scenario::stop(fleet, 1, phase)?;
    reuse.expect(0, 1, 0, BLOCK_BYTES as u64)?;
    let baseline_cache = scenario::cache_path(fixture, "tidb-cold-no-peer-empty");
    launch(fleet, fixture, 1, &baseline_cache, &[], false, phase)?;
    read_drive(fleet, fixture, 1, 0, phase).await?;
    let baseline = scenario::stop(fleet, 1, phase)?;
    baseline.expect(0, 0, 1, 0)?;
    // Leave the exact forbidden Drive2 runtime resident and verified. A broken
    // receiver whitelist would now serve its warm block through the current
    // real scope lease, rather than accidentally fail a cold500ms inspection.
    read_drive(fleet, fixture, 0, 2, phase).await?;
    scenario::observe_entry(
        fleet,
        &scenario::entry(&holder_cache, &initial.scopes[2], &initial.ids[2][0]),
        &payload(2, 0),
        phase,
    )
    .await?;
    let probe_root = fixture.root.clone();
    let probe_address = fixture.peer_addresses[0];
    let probe_roots = fixture.roots.clone();
    let probe_pin = fixture.pins[0].clone();
    let allowed = initial.scopes[0].clone();
    let allowed_id = initial.ids[0][0].clone();
    let denied = initial.scopes[2].clone();
    let denied_id = initial.ids[2][0].clone();
    let probe = owner.start(phase, OraclePhase::PeerPartitionDenial, move |resources| {
        peer_partition_denial(
            PeerPartitionProbe {
                root: probe_root,
                holder_address: probe_address,
                roots: probe_roots,
                pin: probe_pin,
                allowed,
                allowed_id,
                denied,
                denied_id,
            },
            resources,
        )
    })?;
    let denial = owner.wait(fleet, probe, phase).await?;
    phases.push(
        json!({"phase":"actual-peer-receiver-cross-partition-denial","contract":denial.report}),
    );
    let holder_bank = scenario::stop(fleet, 0, phase)?;
    holder_bank.expect(2, 0, 0, 2 * BLOCK_BYTES as u64)?;
    if [
        peer.cache_errors,
        reuse.cache_errors,
        baseline.cache_errors,
        holder_bank.cache_errors,
    ]
    .iter()
    .any(|count| *count != 0)
    {
        return Err("cache errors prohibit exact causal savings assertion".into());
    }
    let holder = holder(fixture, 0, holder_generation)?;
    if holder.proof_reuses == 0 {
        return Err("retained holder proof was not actually reused".into());
    }
    fleet.stop_all(clipped(Instant::now(), phase, SHUTDOWN_SECONDS)?)?;
    phases.push(json!({"phase":"actual-capacity-one-cold-retirement-peer-read","pids":cold_pids,
        "max_active_drives":1,"ordered_signed_drives":[drive(0),drive(1)],"retirement_evidence":"B distinct bytes after closed A view under actual RuntimePool capacity1",
        "retained_exact_disk_entry_observed":true,"only_eligible_requester_peer":"node-0",
        "prewarm_generation":prewarm_generation,"prewarm_bank":prewarm_bank,"holder_local_only_bank":holder_bank,
        "placement_lease_isolation":"earlier no-peer prewarm positively shut down; actual capacity1 holder A reads are exactly two local hits and zero backing/peer misses",
        "holder_and_requester_addresses":"actual retained loopback CLI peer endpoints",
        "peer_bank":peer,"proof_reuse_bank":reuse,"no_peer_bank":baseline,
        "inspection_starts":holder.inspection_starts,"inspection_successes":holder.inspection_successes,
        "proof_reuses":holder.proof_reuses,"authority_refusals":holder.authority_refusals,
        "holder_actual_shutdown_sealed_joined_released":true,
        "savings_scope":"one logical backing.get avoided per exact isolated peer read; no HTTP/physical IOPS claim"}));
    // Existing signed invalid-signature/cross-partition/sibling/read-only/revocation oracles.
    // Catalog storage remains actual TiDB/RustFS throughout; only grants change.
    scenario::security(
        fleet,
        fixture,
        clipped(Instant::now(), work, PHASE_SECONDS)?,
        phases,
    )
    .await?;
    let final_oracle = fresh_all(
        owner,
        fleet,
        &backing,
        clipped(Instant::now(), work, PHASE_SECONDS)?,
        FreshRound::Final,
    )
    .await?;
    if final_oracle.scopes != initial.scopes || final_oracle.ids != initial.ids {
        return Err(
            "fresh durable backing/extent identities changed during read/retirement/denial cells"
                .into(),
        );
    }
    phases.push(json!({"phase":"tidb-rustfs-final-fresh-oracle","oracle":final_oracle.report,
        "same_exact_backings_and_blocks":true,"small_causal_geometry":{"servers":NODES,"drives":NODES,"partitions":PARTITIONS,"files_each":FILES},
        "final_production_target":{"clients":10000,"drives":10000,"partitions":5000,"files_each":1000},
        "scale_claim":false,"peer_receiver_cross_partition_negative":"executed with real pinned peer GET positive controls before and after",
        "physical_io":"not measured","http_attempts":"not counted","lost_reply":"separate lineage"}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame() -> String {
        format!(
            "cache_holder {{\"configured_routes\":{NODES},\"retained_proof_groups\":0,\"admitted_proofs\":0,\"retained_inspectors\":0,\"sealed\":true,\"inspection_starts\":1,\"inspection_successes\":1,\"authority_refusals\":0,\"proof_reuses\":1}}\n"
        )
    }
    #[test]
    fn cold_holder_shutdown_frame_requires_complete_unique_real_contract() {
        let valid = frame();
        assert!(parse_holder(valid.as_bytes()).is_ok());
        assert!(parse_holder(valid.trim_end().as_bytes()).is_err());
        assert!(parse_holder(format!("{valid}cache_holder{{}}\n").as_bytes()).is_err());
        assert!(parse_holder(format!("{valid}{valid}").as_bytes()).is_err());
        assert!(
            parse_holder(
                valid
                    .replace("\"retained_inspectors\":0", "\"retained_inspectors\":1")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(
            parse_holder(
                valid
                    .replace("\"inspection_successes\":1", "\"inspection_successes\":0")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(
            parse_holder(
                valid
                    .replace("\"sealed\":true", "\"sealed\":false")
                    .as_bytes()
            )
            .is_err()
        );
        assert!(
            parse_holder(
                valid
                    .replace("\"proof_reuses\":1", "\"proof_reuses\":1,\"unknown\":0")
                    .as_bytes()
            )
            .is_err()
        );
    }
    #[test]
    fn cold_oracle_owner_unwind_retains_unproven_actual_resource_group() {
        let state = Arc::new(Mutex::new(Retained::default()));
        state
            .lock()
            .unwrap()
            .add(Resource::Context(StorageContext::new(16).unwrap()));
        let weak = Arc::downgrade(&state);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let mut owner = OracleOwner::default();
            owner.jobs.push(Job {
                task: None,
                resources: state,
                observation: Arc::new(Mutex::new(Value::Null)),
                settled: false,
            });
            panic!("controlled worker receipt failure before positive oracle close");
        }));
        assert!(panic.is_err());
        assert!(
            weak.upgrade().is_some(),
            "unknown real context owner was discarded during unwind"
        );
    }
    #[test]
    fn cold_oracle_owner_releases_only_positive_groups_and_retains_poison() {
        let proven = Arc::new(Mutex::new(Retained {
            cleanup_complete: true,
            ..Retained::default()
        }));
        let proven_weak = Arc::downgrade(&proven);
        let poison = Arc::new(Mutex::new(Retained::default()));
        let poison_weak = Arc::downgrade(&poison);
        let actual = poison.clone();
        assert!(
            std::thread::spawn(move || {
                let _held = actual.lock().unwrap();
                panic!("controlled actual oracle owner poison");
            })
            .join()
            .is_err()
        );
        let mut owner = OracleOwner::default();
        owner.jobs.push(Job {
            task: None,
            resources: proven,
            observation: Arc::new(Mutex::new(Value::Null)),
            settled: true,
        });
        owner.jobs.push(Job {
            task: None,
            resources: poison,
            observation: Arc::new(Mutex::new(Value::Null)),
            settled: true,
        });
        drop(owner);
        assert!(
            proven_weak.upgrade().is_none(),
            "positive empty close group was unnecessarily retained"
        );
        assert!(
            poison_weak.upgrade().is_some(),
            "poisoned owner was discarded"
        );
    }
}
