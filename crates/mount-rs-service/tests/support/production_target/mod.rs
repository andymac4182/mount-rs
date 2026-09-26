//! Fixture-only independent-process controller. No production runtime policy changes.
mod backend;
mod command;
mod config;
#[allow(dead_code)]
#[path = "../production_fixture.rs"]
mod fixture;
mod oracle;
mod preflight;
mod process;
#[allow(dead_code)]
#[path = "../resource_profile.rs"]
mod resource_profile;
mod resources;
mod state;
mod timing;
#[allow(dead_code)]
#[path = "../tidb_wire.rs"]
mod wire;
mod workload;
use config::{Config, PATTERNS, PHASE_SECONDS, REQUEST_SECONDS, SERVERS, WORK_SECONDS};
use process::{Fleet, PrivateConfig};
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use workload::Lane;
type Client = Lane;
use fixture::{FileProfile, SignedTokens, target_catalog};
pub use process::worker;
pub fn utc_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
pub fn digest(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub fn file_digest(path: &Path) -> Result<String, String> {
    Ok(digest(
        &std::fs::read(path).map_err(|_| "identity file unavailable")?,
    ))
}
pub fn read_json(path: &Path) -> Result<Value, String> {
    serde_json::from_slice(&std::fs::read(path).map_err(|_| "receipt unavailable")?)
        .map_err(|_| "receipt invalid".into())
}
pub fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let pending = path.with_extension("pending");
    std::fs::write(
        &pending,
        serde_json::to_vec_pretty(value).map_err(|_| "receipt encoding failed")?,
    )
    .map_err(|_| "receipt write failed")?;
    std::fs::rename(pending, path).map_err(|_| "receipt publication failed".into())
}
pub async fn source_identity(commands: &mut command::Commands) -> Result<Value, String> {
    let sources = [
        ("command.rs", include_bytes!("command.rs").as_slice()),
        ("timing.rs", include_bytes!("timing.rs").as_slice()),
        ("mod.rs", include_bytes!("mod.rs").as_slice()),
        ("config.rs", include_bytes!("config.rs").as_slice()),
        ("state.rs", include_bytes!("state.rs").as_slice()),
        ("resources.rs", include_bytes!("resources.rs").as_slice()),
        ("backend.rs", include_bytes!("backend.rs").as_slice()),
        ("process.rs", include_bytes!("process.rs").as_slice()),
        ("workload.rs", include_bytes!("workload.rs").as_slice()),
        ("oracle.rs", include_bytes!("oracle.rs").as_slice()),
        ("preflight.rs", include_bytes!("preflight.rs").as_slice()),
        (
            "../production_fixture.rs",
            include_bytes!("../production_fixture.rs").as_slice(),
        ),
        (
            "../tidb_wire.rs",
            include_bytes!("../tidb_wire.rs").as_slice(),
        ),
        (
            "../resource_profile.rs",
            include_bytes!("../resource_profile.rs").as_slice(),
        ),
        (
            "../../quic_production_target.rs",
            include_bytes!("../../quic_production_target.rs").as_slice(),
        ),
    ];
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/production_target");
    let mut rows = std::collections::BTreeMap::new();
    for (name, compiled) in sources {
        let actual = std::fs::read(root.join(name)).map_err(|_| "runner source missing")?;
        if actual != compiled {
            return Err(format!(
                "runner source differs from compiled binary: {name}"
            ));
        }
        rows.insert(name, digest(compiled));
    }
    let script = include_bytes!("../../../../../scripts/bench-remote-production-target.sh");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    if std::fs::read(repo.join("scripts/bench-remote-production-target.sh"))
        .map_err(|_| "runner script missing")?
        != script
    {
        return Err("runner script differs from compiled binary".into());
    }
    rows.insert("scripts/bench-remote-production-target.sh", digest(script));
    let dirty = commands
        .capture(
            "git",
            &["diff", "HEAD", "--binary"],
            Some(&repo),
            Duration::from_secs(10),
        )
        .await?;
    let protected_paths = [
        "crates/mount-rs-sdk/tests/compact_runtime.rs",
        "filesystems/mount-rs-chunked/src/lib.rs",
        "filesystems/mount-rs-chunked/tests/compact_inodes.rs",
        "filesystems/mount-rs-chunked/tests/concurrent_inodes.rs",
        "providers/mount-rs-foundationdb/src/compact_tests.rs",
        "providers/mount-rs-pglite/src/compact_tests.rs",
        "providers/mount-rs-sqlite/src/compact_tests.rs",
        "providers/mount-rs-tidb/tests/compact.rs",
        "src/diagnostics/profile.rs",
        "src/storage/compact.rs",
        "src/storage/compact/tests.rs",
    ];
    let protected: std::collections::BTreeMap<_, _> = protected_paths
        .into_iter()
        .map(|p| Ok((p, file_digest(&repo.join(p))?)))
        .collect::<Result<_, String>>()?;
    let revision = commands
        .capture(
            "git",
            &["rev-parse", "HEAD"],
            Some(&repo),
            Duration::from_secs(10),
        )
        .await?;
    let status = commands
        .capture(
            "git",
            &["status", "--porcelain"],
            Some(&repo),
            Duration::from_secs(10),
        )
        .await?;
    Ok(json!({
            "digest":digest(&serde_json::to_vec(&rows).unwrap()),
            "sources":rows,
            "revision":revision.trim(),
            "checkout_status":status,
            "tracked_dirty_patch_sha256":digest(dirty.as_bytes()),
            "protected_sha256":protected,
            "binary_sha256":file_digest(&std::env::current_exe().map_err(|_|"binary path unavailable")?)?,
            "resource_profiling":cfg!(feature="resource-profiling"),
            "allocation_profiling":cfg!(feature="allocation-profiling"),
            "debug_assertions":cfg!(debug_assertions),
            "scope":"current checkout native fixture; dirty source disclosed; not clean committed CI"}
    ))
}
struct Journal {
    value: Value,
    output: std::path::PathBuf,
    counts: Vec<Arc<Mutex<state::Counts>>>,
}
impl Journal {
    fn flush(&mut self) -> Result<(), String> {
        self.value["lanes"] = serde_json::to_value(
            self.counts
                .iter()
                .map(|c| c.lock().unwrap().clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        self.value["updated_unix_ms"] = json!(utc_ms());
        write_json(&self.output.join("terminal.json"), &self.value)
    }
    fn phase(&mut self, name: &str) -> Result<(), String> {
        let now = utc_ms();
        let previous = self.value["phase"].clone();
        let began = self.value["phase_started_unix_ms"]
            .as_u64()
            .or_else(|| self.value["created_unix_ms"].as_u64())
            .unwrap_or(now);
        if self.value["phase_history"].is_null() {
            self.value["phase_history"] = json!([]);
        }
        self.value["phase_history"].as_array_mut().unwrap().push(
            json!({"phase":previous,"elapsed_ms":now.saturating_sub(began),"ended_unix_ms":now}),
        );
        self.value["phase_started_unix_ms"] = json!(now);
        self.value["phase"] = json!(name);
        self.flush()
    }
}
async fn supervised<T>(
    fleet: &mut Fleet,
    resources: &resources::Resources,
    journal: &mut Journal,
    seconds: u64,
    future: impl std::future::Future<Output = Result<T, String>>,
) -> Result<T, String> {
    let begin = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tokio::pin!(future);
    loop {
        tokio::select! {
                result=&mut future=>return result,
        _=tick.tick()=>{
                resources.check()?;
                fleet.check()?;
                journal.value["controller_resources"]=resources.snapshot();
                journal.flush()?;
                if begin.elapsed()>=Duration::from_secs(seconds){
                return Err(format!("{} phase deadline; partial work incomplete",
        journal.value["phase"]));
                }
                }
                }
    }
}
async fn connect(
    endpoint: &quinn::Endpoint,
    fleet: &Fleet,
    tokens: &SignedTokens,
    drive: usize,
    server: usize,
) -> Result<quinn::Connection, String> {
    tokio::time::timeout(
        Duration::from_secs(REQUEST_SECONDS),
        wire::connect_token(
            endpoint,
            fleet.children[server]
                .ready
                .as_ref()
                .ok_or("worker readiness missing")?
                .address,
            &format!("partition-{}", drive / 2),
            &tokens.token(drive, 3000),
        ),
    )
    .await
    .map_err(|_| "authentication deadline")?
    .map_err(|_| "signed authentication failed".into())
}
pub async fn controller() -> Result<(), String> {
    let output = std::path::PathBuf::from(
        std::env::var_os("MOUNT_RS_TARGET_OUTPUT").ok_or("retained output required")?,
    );
    std::fs::create_dir_all(&output).map_err(|_| "output directory unavailable")?;
    if output.join("terminal.json").exists() {
        return Err("output already contains a run; refusing overwrite".into());
    }
    let mut journal = Journal {
        value: json!({
                "schema":"mount-rs-production-target-v1",
                "outcome":"incomplete",
                "phase":"preflight",
                "created_unix_ms":utc_ms(),
                "full_target":false,
                "workers":[],
                "namespace_files":0,
                "population_bytes":0,
                "routes":0,
                "verified_passes":0,
                "stages":[],
                "scope":"local loopback, signed local ES256 fixture authentication; not external issuer or cross-host capacity",
                "budgets":{
                    "phase_seconds":PHASE_SECONDS,
                    "work_seconds":WORK_SECONDS,
                    "request_seconds":REQUEST_SECONDS,
                    "setup_seconds":600,
                    "child_cleanup_seconds":95,"client_cleanup_seconds":30,"oracle_cleanup_seconds":30,"expected_receipt_seconds":30,
                    "rss_cap_per_owned_process":config::RSS_CAP,
                    "host_free_floor":config::DISK_FLOOR}
                ,
                "journal_cadence_seconds":1,
                "observer_cost":"journal/resource sampling excluded from RPC latency, included in elapsed phase/work; no physical IOPS attribution",
                "observer_budgets_seconds":{"expected_state":30,"audit":30,"sampler_stop":1,"subprocess_reap":1,"preflight_sql_disconnect":10}}
        ),
        output: output.clone(),
        counts: vec![],
    };
    journal.flush()?;
    let mut fleet = Fleet::new();
    let mut endpoints = Vec::new();
    let mut lanes = Vec::new();
    let mut private_path = None;
    let mut resources = None;
    let mut oracle_owner = oracle::Owner::default();
    let mut initializer_owner = oracle::Owner::default();
    let mut initialization_receipts = Vec::new();
    let mut initialization_start = None;
    let mut commands = command::Commands::default();
    let mut provider_preflight = preflight::Owner::default();
    let result: Result<(), String> = async {
        let setup = async {
            let config = Config::environment()?;
            journal.value["full_target"] = json!(config.full_target);
            journal.value["fault_injection"] = json!(std::env::var("MOUNT_RS_TARGET_INJECT").ok());
            if config.full_target && std::env::var_os("MOUNT_RS_TARGET_INJECT").is_some() {
                return Err("full target refuses fault injection".into());
            }
            journal.value["configuration"] = serde_json::to_value(&config).unwrap();
            journal.value["initial_profile"] = json!({
                    "files_per_drive":config.files,
                    "initial_total_bytes":(0..config.files).map(|f|FileProfile::Mixed.size(f)as u64).sum::<u64>()*config.drives as u64,
                    "required_patterns":PATTERNS,
                    "initial_sizes":"990x4096+9x131072+1x1048576 per1000 files",
                    "mode":"MRC5"}
            );
            journal.value["source"] = source_identity(&mut commands).await?;
            journal.flush()?;
            resources = Some(resources::Resources::start(
                output.join("controller-resources.json"),
            )?);
            if resources::disk_available()? < config::DISK_FLOOR {
                return Err("preflight refusal: host free disk below64GiB".into());
            }
            if config.provider == "tidb" {
                journal.value["provider_preflight"] =
                    preflight::tidb(&output, &mut commands, &mut provider_preflight).await?;
            } else {
                journal.value["provider_preflight"] = json!({
                        "provider":"sqlite",
                        "version":rusqlite::version(),
                        "scope":"owned local SQLite diagnostic; no TiDB capacity claim"}
                );
            }
            let directory = output.join("private");
            std::fs::create_dir(&directory).map_err(|_| "private fixture directory unavailable")?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "private directory permissions failed")?;
            let backend = backend::Backend {
                provider: config.provider.clone(),
                root: directory.clone(),
                prefix: format!("production-target-{}-{}", std::process::id(), utc_ms()),
            };
            journal.phase("empty_drive_initialization")?;
            initialization_start = Some(Instant::now());
            journal.value["initialization"] = json!({"scope":"sequential empty MRC5 roots/backings before steady-state workers; no namespace/payload preseed; parallel virgin-start unqualified","complete":false,"initialized_drives":0,"cleanup_confirmed":false});
            let mut last_progress = Instant::now();
            for drive in 0..config.drives {
                resources.as_ref().unwrap().check()?;
                let receipt = initializer_owner.initialize_empty(&backend, drive).await?;
                initialization_receipts.push(receipt);
                journal.value["initialization"]["initialized_drives"] = json!(initialization_receipts.len());
                journal.value["initialization"]["elapsed_seconds"] = json!(initialization_start.unwrap().elapsed().as_secs_f64());
                if last_progress.elapsed() >= Duration::from_secs(1) {
                    journal.flush()?;
                    last_progress = Instant::now();
                }
            }
            initializer_owner.close().await?;
            journal.value["initialization"]["cleanup_confirmed"] = json!(true);
            journal.value["initialization"]["complete"] = json!(true);
            journal.value["initialization"]["elapsed_seconds"] = json!(initialization_start.take().unwrap().elapsed().as_secs_f64());
            fleet.expect_initialized_backings(
                initialization_receipts
                    .iter()
                    .enumerate()
                    .map(|(drive, r)| process::validate_backing_receipt(r, drive))
                    .collect::<Result<_, _>>()?,
            );
            journal.flush()?;
            let catalog_path = directory.join("catalog.sqlite");
            let catalog = mount_rs_service::catalog::SqliteCatalog::open(&catalog_path)
                .await
                .map_err(|_| "catalog open failed")?;
            let snapshot = target_catalog(config.drives);
            catalog
                .compare_and_swap(0, snapshot)
                .await
                .map_err(|_| "catalog publication failed")?;
            let snapshot = catalog
                .load_shared_current()
                .await
                .map_err(|_| "catalog receipt read failed")?;
            let catalog_digest = digest(&serde_json::to_vec(snapshot.as_ref()).unwrap());
            let tokens = SignedTokens::new();
            let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()])
                .map_err(|_| "TLS generation failed")?;
            let private = PrivateConfig {
                config: config.clone(),
                backend: backend.clone(),
                catalog: catalog_path,
                catalog_digest,
                cert: certificate.cert.der().to_vec(),
                key: certificate.signing_key.serialize_der(),
                jwk: tokens.jwk.clone(),
                source_digest: journal.value["source"]["digest"].as_str().unwrap().into(),
                binary_digest: journal.value["source"]["binary_sha256"]
                    .as_str()
                    .unwrap()
                    .into(),
                output: output.clone(),
                parent_pid: std::process::id(),
            };
            let path = directory.join("worker-config.json");
            write_json(&path, &serde_json::to_value(&private).unwrap())?;
            private_path = Some(path.clone());
            journal.phase("worker_setup")?;
            for index in 0..SERVERS {
                fleet.launch(&path, &output, index)?;
                if std::env::var("MOUNT_RS_TARGET_INJECT").as_deref() == Ok("partial_start")
                    && index == 2
                {
                    return Err("injected partial startup failure".into());
                }
            }
            fleet.ready(&private, 0).await?;
            journal.value["workers"] = json!(fleet.receipts());
            journal.flush()?;
            if std::env::var("MOUNT_RS_TARGET_INJECT").as_deref() == Ok("child_loss") {
                fleet.children[3]
                    .child
                    .kill()
                    .map_err(|_| "injected kill failed")?;
                tokio::time::sleep(Duration::from_millis(100)).await;
                fleet.check()?;
                return Err("child loss injection unexpectedly survived".into());
            }
            for _ in 0..SERVERS {
                endpoints.push(workload::endpoint(&private.cert)?);
            }
            Ok::<_, String>((config, private, tokens, catalog, backend))
        };
        let (config, private, tokens, catalog, backend) =
            tokio::time::timeout(Duration::from_secs(600), setup)
                .await
                .map_err(|_| "complete setup deadline; retained owners require cleanup")??;
        let resource = resources.as_ref().ok_or("setup resource owner missing")?;
        let work_started = Instant::now();
        let work = async {
            if std::env::var("MOUNT_RS_TARGET_INJECT").as_deref() == Ok("work_timeout") {
                journal.phase("injected_timeout")?;
                supervised(
                    &mut fleet,
                    resource,
                    &mut journal,
                    1,
                    std::future::pending::<Result<(), String>>(),
                )
                .await?;
            }
            journal.phase("signed_connections")?;
            for drive in 0..config.drives {
                let connection = connect(
                    &endpoints[drive % SERVERS],
                    &fleet,
                    &tokens,
                    drive,
                    drive % SERVERS,
                )
                .await?;
                let lane = Lane::new(connection, drive, config.files);
                journal.counts.push(lane.counts.clone());
                lanes.push(lane);
            }
            journal.value["connected_clients"] = json!(lanes.len());
            journal.phase("online_namespace")?;
            supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS, async {
                futures_util::future::try_join_all(
                    lanes.iter_mut().map(|l| l.populate(config.files, false)),
                )
                .await?;
                Ok(())
            })
            .await?;
            journal.value["namespace_files"] = json!(lanes.iter().map(|l|l.expected.files.len()as u64).sum::<u64>());
            journal.phase("online_payload")?;
            supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS, async {
                futures_util::future::try_join_all(
                    lanes.iter_mut().map(|l| l.populate(config.files, true)),
                )
                .await?;
                Ok(())
            })
            .await?;
            journal.value["population_bytes"] = json!(lanes.iter().flat_map(|l|l.expected.files.values()).map(|f|f.length as u64).sum::<u64>());
            journal.phase("initial_fresh_oracle")?;
            supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS, async {
                for lane in &lanes {
                    oracle::verify(&mut oracle_owner, &backend, &lane.expected).await?;
                }
                Ok(())
            })
            .await?;
            journal.value["verified_passes"] = json!(1);
            // End old connections and all replicas before reopening each worker.
            journal.phase("refresh_replicas")?;
            for lane in &lanes {
                lane.connection.close(0u32.into(), b"replica refresh");
            }
            fleet.command("reopen", 1)?;
            fleet.ready(&private, 1).await?;
            for (drive, lane) in lanes.iter_mut().enumerate() {
                lane.connection = connect(
                    &endpoints[drive % SERVERS],
                    &fleet,
                    &tokens,
                    drive,
                    drive % SERVERS,
                )
                .await?;
            }
            journal.phase("routes_and_scope")?;
            let mut routes = 0;
            let mut sibling = 0;
            let mut partition = 0;
            for (server, endpoint) in endpoints.iter().enumerate() {
                for drive in 0..config.drives {
                    let connection = connect(endpoint, &fleet, &tokens, drive, server).await?;
                    tokio::time::timeout(
                        Duration::from_secs(REQUEST_SECONDS),
                        wire::success(
                            &connection,
                            1,
                            &format!("sandbox-{drive}"),
                            mount_rs_remote_protocol::OperationName::Stat,
                            json!({
                                    "path":"/mixed-0"}
                            ),
                        ),
                    )
                    .await
                    .map_err(|_| "route request deadline")??;
                    routes += 1;
                    connection.close(0u32.into(), b"route complete");
                }
            }
            for (drive, lane) in lanes.iter().enumerate() {
                let denied = tokio::time::timeout(
                    Duration::from_secs(REQUEST_SECONDS),
                    wire::request(
                        &lane.connection,
                        u64::MAX - 1,
                        &format!("sandbox-{}", drive ^ 1),
                        mount_rs_remote_protocol::OperationName::Stat,
                        json!({
                                "path":"/"}
                        ),
                    ),
                )
                .await
                .map_err(|_| "scope request deadline")??;
                if denied != Err("EACCES".into()) {
                    return Err("sibling Drive denial missing".into());
                }
                sibling += 1;
                let other = ((drive + 2) % config.drives) / 2;
                tokio::time::timeout(
                    Duration::from_secs(REQUEST_SECONDS),
                    fixture::expect_authentication_denial(
                        &endpoints[drive % SERVERS],
                        fleet.children[drive % SERVERS]
                            .ready
                            .as_ref()
                            .unwrap()
                            .address,
                        &format!("partition-{other}"),
                        &tokens.token(drive, 3000),
                    ),
                )
                .await
                .map_err(|_| "cross Partition deadline")??;
                partition += 1;
            }
            journal.value["routes"] = json!(routes);
            journal.value["scope_denials"] = json!({
                    "sibling":sibling,
                    "partition":partition}
            );
            for mostly_idle in [true, false] {
                for pattern in PATTERNS {
                    let mode = if mostly_idle {
                        "mostly_idle"
                    } else {
                        "all_active"
                    };
                    journal.phase(&format!("{mode}/{pattern}"))?;
                    let phase_begin = Instant::now();
                    let active = config.active(mostly_idle);
                    let connections: Vec<_> =
                        lanes.iter().map(|lane| lane.connection.clone()).collect();
                    let network_before =
                        resource_profile::Snapshot::capture_connections(&connections)
                            .map_err(|_| "boundary network baseline unavailable")?;
                    let before: Vec<_> = journal
                        .counts
                        .iter()
                        .map(|c| c.lock().unwrap().clone())
                        .collect();
                    let begin = Instant::now();
                    let mut clock = timing::WorkloadClock::start();
                    let end = begin + Duration::from_secs(config.seconds);
                    let cycles =
                        supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS, async {
                            futures_util::future::try_join_all(lanes[..active].iter_mut().map(
                                |lane| async move {
                                    let mut cycle = 0;
                                    while cycle == 0 || Instant::now() < end {
                                        lane.cycle(pattern, cycle, config.files).await?;
                                        cycle += 1;
                                    }
                                    Ok::<_, String>(cycle)
                                },
                            ))
                            .await
                        })
                        .await?;
                    clock.active_finished();
                    let mut idle_live = 0;
                    supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS, async {
                        for lane in &mut lanes[active..] {
                            if lane.connection.close_reason().is_some() {
                                return Err("idle retained client disconnected".into());
                            }
                            lane.request(
                                mount_rs_remote_protocol::OperationName::Stat,
                                json!({
                                        "path":"/"}
                                ),
                            )
                            .await?;
                            idle_live += 1;
                        }
                        Ok::<_, String>(())
                    })
                    .await?;
                    let timing = clock.finish(cycles.iter().sum());
                    let network_after =
                        resource_profile::Snapshot::capture_connections(&connections)
                            .map_err(|_| "boundary network terminal unavailable")?;
                    let network = network_after
                        .connection_deltas(&network_before)
                        .map_err(|_| "boundary network delta unavailable")?;
                    let after: Vec<_> = journal
                        .counts
                        .iter()
                        .map(|c| c.lock().unwrap().clone())
                        .collect();
                    let latency_histogram: Vec<u64> = (0..32)
                        .map(|bucket| {
                            after[..active]
                                .iter()
                                .zip(&before)
                                .map(|(a, b)| {
                                    a.latency_histogram_log2_us[bucket]
                                        - b.latency_histogram_log2_us[bucket]
                                })
                                .sum()
                        })
                        .collect();
                    journal.value["stages"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({
                                "mode":mode,
                                "pattern":pattern,
                                "rpc_latency_histogram_log2_microseconds":latency_histogram,
                                "timing":timing,"controller_quic_boundary":{"scope":"actual retained client connections; active workload plus idle liveness; snapshot observer outside active throughput interval; server transport unavailable","connections":network},
                                "configured_active_clients":active,
                                "clients_with_completed_cycles":cycles.iter().filter(|n|**n>0).count(),
                                "connected_clients":lanes.len(),
                                "idle_liveness_acknowledgments":idle_live,
                                "cycles":cycles.iter().sum::<usize>(),
                                "elapsed_seconds":phase_begin.elapsed().as_secs_f64(),"elapsed_scope":"overall phase including boundary observers, active work and idle liveness",
                                "requested_seconds":config.seconds,
                                "acknowledged_requests":after[..active].iter().zip(&before).map(|(a,
                                        b)|a.acknowledged-b.acknowledged).sum::<u64>(),
                                "scope":"RPC acknowledgements include open/close; cycles are workload operations, not physical IOPS"}
                        ));
                    journal.flush()?;
                }
            }
            journal.phase("final_fresh_oracle")?;
            let mut verified_files = 0;
            let mut verified_bytes = 0;
            supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS, async {
                for lane in &lanes {
                    let (files, bytes) =
                        oracle::verify(&mut oracle_owner, &backend, &lane.expected).await?;
                    verified_files += files;
                    verified_bytes += bytes;
                }
                Ok(())
            })
            .await?;
            journal.value["verified_passes"] = json!(2);
            journal.value["verified_files"] = json!(verified_files);
            journal.value["verified_bytes"] = json!(verified_bytes);
            journal.phase("revocation")?;
            let mut revoked = target_catalog(config.drives);
            revoked.grants.clear();
            catalog
                .compare_and_swap(1, revoked)
                .await
                .map_err(|_| "revocation catalog update failed")?;
            for lane in &lanes {
                let response = tokio::time::timeout(
                    Duration::from_secs(REQUEST_SECONDS),
                    wire::request(
                        &lane.connection,
                        u64::MAX,
                        &format!("sandbox-{}", lane.expected.drive),
                        mount_rs_remote_protocol::OperationName::Stat,
                        json!({
                                "path":"/"}
                        ),
                    ),
                )
                .await
                .map_err(|_| "revocation deadline")??;
                if response != Err("EACCES".into()) {
                    return Err("revocation not enforced".into());
                }
            }
            journal.value["revocation_denials"] = json!(lanes.len());
            Ok(())
        };
        let result = tokio::time::timeout(Duration::from_secs(WORK_SECONDS), work)
            .await
            .map_err(|_| "enclosing work deadline; partial work incomplete".to_string())
            .and_then(|r| r);
        journal.value["work_elapsed_seconds"] = json!(work_started.elapsed().as_secs_f64());
        result
    }
    .await;
    for counts in &journal.counts {
        counts.lock().unwrap().uncertain();
    }
    for lane in &lanes {
        lane.connection.close(0u32.into(), b"controller cleanup");
    }
    if let Some(start) = initialization_start {
        journal.value["initialization"]["elapsed_seconds"] = json!(start.elapsed().as_secs_f64());
    }
    let mut cleanup = fleet.cleanup().await;
    match initializer_owner.close().await {
        Ok(()) => {
            if journal.value["initialization"].is_object() {
                journal.value["initialization"]["cleanup_confirmed"] = json!(true);
            }
        }
        Err(error) => cleanup.push(json!({"error":error})),
    }
    if let Err(error) = commands.cleanup().await {
        cleanup.push(json!({"error":error}));
    }
    if let Err(error) = provider_preflight.close().await {
        cleanup.push(json!({"error":error}));
    }
    journal.value["observer_processes"] = commands.receipts();
    if let Err(error) = oracle_owner.close().await {
        cleanup.push(json!({
                "error":error}
        ));
    }
    for endpoint in &endpoints {
        endpoint.close(0u32.into(), b"controller cleanup");
    }
    if tokio::time::timeout(
        Duration::from_secs(30),
        futures_util::future::join_all(endpoints.iter().map(|e| e.wait_idle())),
    )
    .await
    .is_err()
    {
        cleanup.push(json!({
                "error":"client endpoint drain unproven"}
        ));
    }
    if let Some(path) = private_path
        && std::fs::remove_file(path).is_err()
    {
        cleanup.push(json!({
                "error":"private key config removal failed"}
        ));
    }
    let expected_start = Instant::now();
    let initialization_path = output.join("initialization-receipts.json");
    match write_json(&initialization_path, &json!(initialization_receipts))
        .and_then(|()| file_digest(&initialization_path))
    {
        Ok(hash) => {
            journal.value["initialization_receipt_file"] = json!({"path":"initialization-receipts.json","sha256":hash,"drives":initialization_receipts.len()})
        }
        Err(error) => cleanup.push(json!({"error":error})),
    }
    let expected_dir = output.join("expected");
    if std::fs::create_dir(&expected_dir).is_err() {
        cleanup.push(json!({
                "error":"expected-state receipt directory failed"}
        ));
    }
    let mut expected_receipts = Vec::new();
    for lane in &lanes {
        if expected_start.elapsed() > Duration::from_secs(30) {
            cleanup.push(json!({
                    "error":"expected-state terminal receipt deadline"}
            ));
            break;
        }
        let name = format!("drive-{}.json", lane.expected.drive);
        let value = lane.expected.snapshot();
        let recorded = write_json(&expected_dir.join(&name), &value)
            .and_then(|()| file_digest(&expected_dir.join(&name)));
        match recorded {
            Ok(hash) => expected_receipts.push(json!({"drive":lane.expected.drive,"file":format!("expected/{name}"),"sha256":hash})),
            Err(error) => cleanup.push(json!({"error":error})),
        }
        if expected_start.elapsed() > Duration::from_secs(30) {
            cleanup.push(json!({"error":"expected-state receipt exceeded30s after write/hash"}));
            break;
        }
    }
    let expected_complete = expected_receipts.len() == lanes.len()
        && expected_start.elapsed() <= Duration::from_secs(30);
    if !expected_complete {
        cleanup.push(json!({"error":"expected-state observation incomplete"}));
    }
    journal.value["expected_state_observation"] = json!({"complete":expected_complete,"elapsed_seconds":expected_start.elapsed().as_secs_f64(),"deadline_seconds":30,"bound":"cooperative post-write/hash checks; blocked OS I/O cannot be interrupted"});
    journal.value["expected_state_receipts"] = json!(expected_receipts);
    let audit_start = Instant::now();
    let audit_deadline = audit_start + Duration::from_secs(30);
    let audit: Vec<_> = fleet
        .children
        .iter()
        .map(|child| {
            let mut value = observe_audit(&child.root.join("worker.log"), audit_deadline);
            value["server"] = json!(child.server);
            value
        })
        .collect();
    let audit_complete =
        audit.iter().all(|value| value["complete"] == true) && Instant::now() <= audit_deadline;
    if !audit_complete {
        cleanup.push(json!({"error":"audit observation incomplete"}));
    }
    journal.value["worker_audit_logs"] = json!(audit);
    journal.value["audit_observation"] = json!({"complete":audit_complete,"elapsed_seconds":audit_start.elapsed().as_secs_f64(),"deadline_seconds":30,"bound":"cooperative checks before/after each OS read; cannot interrupt blocked filesystem calls"});
    let worker_receipts = fleet.receipts();
    journal.value["workers"] = json!(worker_receipts);
    journal.value["cleanup_errors"] = json!(cleanup);
    journal.value["error"] = json!(result.as_ref().err());
    if let Some(r) = &mut resources
        && let Err(error) = r.finish().await
    {
        cleanup.push(json!({"error":error}));
    }
    if let Some(r) = &resources {
        journal.value["controller_resources"] = r.snapshot();
    }
    journal.value["aggregate_owned_resources"] = json!({
            "sum_individual_peak_rss_bytes":worker_receipts.iter().filter_map(|w|w["resources"]["peak_rss_bytes"].as_u64()).sum::<u64>()+resources.as_ref().and_then(|r|r.snapshot()["peak_rss_bytes"].as_u64()).unwrap_or(0),
            "scope":"sum of separately sampled process peaks; not a simultaneous aggregate peak; no host or Docker attribution"}
    );
    journal.value["cleanup_errors"] = json!(cleanup);
    journal.value["last_work_phase"] = journal.value["phase"].clone();
    let success = result.is_ok() && cleanup.is_empty();
    journal.value["outcome"] = json!(if success { "success" } else { "incomplete" });
    journal.value["phase"] = json!("terminal");
    journal.flush()?;
    if success {
        Ok(())
    } else {
        Err("production target incomplete; see retained terminal.json".into())
    }
}
fn observe_audit(path: &Path, deadline: Instant) -> Value {
    use std::io::BufRead;
    let bytes = std::fs::metadata(path).map(|m| m.len()).ok();
    let result = (|| {
        if Instant::now() >= deadline {
            return Err("audit observation deadline");
        }
        let file = std::fs::File::open(path).map_err(|_| "audit log unavailable")?;
        let mut count = 0usize;
        for line in std::io::BufReader::new(file).lines() {
            let line = line.map_err(|_| "audit log read failed")?;
            if Instant::now() >= deadline {
                return Err("audit observation deadline");
            }
            if line.contains("\"event\":\"remote_access\"") {
                count += 1;
            }
        }
        if Instant::now() >= deadline {
            return Err("audit observation deadline");
        }
        Ok(count)
    })();
    json!({"bytes":bytes,"remote_access_events":result.as_ref().ok(),"complete":result.is_ok(),"error":result.err(),"scope":"unchanged dispatcher security audit; logging I/O and CPU included in phase work; event duration unavailable"})
}
#[test]
fn audit_missing_or_expired_is_unknown_not_zero() {
    let root = tempfile::tempdir().unwrap();
    let missing = observe_audit(
        &root.path().join("missing"),
        Instant::now() + Duration::from_secs(1),
    );
    assert_eq!(missing["complete"], false);
    assert!(missing["remote_access_events"].is_null());
    let path = root.path().join("log");
    std::fs::write(&path, b"{\"event\":\"remote_access\"}\n").unwrap();
    assert_eq!(observe_audit(&path, Instant::now())["complete"], false);
    assert_eq!(
        observe_audit(&path, Instant::now() + Duration::from_secs(1))["remote_access_events"],
        1
    );
}
