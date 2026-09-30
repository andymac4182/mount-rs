//! Fixture-only independent-process controller. No production runtime policy changes.
#[cfg(test)]
mod artifact_size_tests;
pub(crate) mod artifacts;
mod backend;
mod checkpoints;
mod command;
mod config;
#[allow(dead_code)]
#[path = "../production_fixture.rs"]
mod fixture;
mod lazy_runtime;
#[allow(dead_code)]
mod metrics;
mod oracle;
mod preflight;
mod process;
mod progress;
mod quic_artifacts;
#[allow(dead_code)]
#[path = "../remote_blocks.rs"]
mod remote_blocks;
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
use workload::{CrossnodeCoverage, Lane};
type Client = Lane;
#[cfg(test)]
pub use checkpoints::retained_checkpoint;
use fixture::{FileProfile, SignedTokens, target_catalog};
pub use process::worker;
#[cfg(test)]
pub use quic_artifacts::verify_stages as verify_controller_quic_artifacts;
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
    let span = metrics::observer().begin("file_hash");
    let result = std::fs::read(path).map_err(|_| "identity file unavailable".to_string());
    let bytes = result.as_ref().map_or(0, |bytes| bytes.len() as u64);
    let result = result.map(|bytes| digest(&bytes));
    span.finish(result.is_ok(), bytes);
    result
}
pub fn read_json(path: &Path) -> Result<Value, String> {
    if path.extension().is_some_and(|extension| extension == "gz") {
        return metrics::read_compressed(path);
    }
    serde_json::from_slice(&std::fs::read(path).map_err(|_| "receipt unavailable")?)
        .map_err(|_| "receipt invalid".into())
}
pub fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let span = metrics::observer().begin("receipt_publication");
    let result: Result<u64, String> = (|| {
        let pending = path.with_extension("pending");
        let bytes = serde_json::to_vec_pretty(value).map_err(|_| "receipt encoding failed")?;
        std::fs::write(&pending, &bytes).map_err(|_| "receipt write failed")?;
        std::fs::rename(pending, path).map_err(|_| "receipt publication failed")?;
        Ok(bytes.len() as u64)
    })();
    span.finish(result.is_ok(), result.as_ref().copied().unwrap_or(0));
    result.map(|_| ())
}
pub async fn source_identity(commands: &mut command::Commands) -> Result<Value, String> {
    let sources = [
        (
            "artifact_size_tests.rs",
            include_bytes!("artifact_size_tests.rs").as_slice(),
        ),
        ("command.rs", include_bytes!("command.rs").as_slice()),
        ("timing.rs", include_bytes!("timing.rs").as_slice()),
        ("metrics.rs", include_bytes!("metrics.rs").as_slice()),
        (
            "quic_artifacts.rs",
            include_bytes!("quic_artifacts.rs").as_slice(),
        ),
        ("artifacts.rs", include_bytes!("artifacts.rs").as_slice()),
        ("progress.rs", include_bytes!("progress.rs").as_slice()),
        (
            "../../../src/startup.rs",
            include_bytes!("../../../src/startup.rs").as_slice(),
        ),
        (
            "../../../src/server.rs",
            include_bytes!("../../../src/server.rs").as_slice(),
        ),
        (
            "../../../src/server/diagnostics.rs",
            include_bytes!("../../../src/server/diagnostics.rs").as_slice(),
        ),
        ("mod.rs", include_bytes!("mod.rs").as_slice()),
        ("config.rs", include_bytes!("config.rs").as_slice()),
        ("state.rs", include_bytes!("state.rs").as_slice()),
        ("resources.rs", include_bytes!("resources.rs").as_slice()),
        (
            "checkpoints.rs",
            include_bytes!("checkpoints.rs").as_slice(),
        ),
        ("backend.rs", include_bytes!("backend.rs").as_slice()),
        (
            "lazy_runtime.rs",
            include_bytes!("lazy_runtime.rs").as_slice(),
        ),
        (
            "../../../src/filesystem_runtime.rs",
            include_bytes!("../../../src/filesystem_runtime.rs").as_slice(),
        ),
        (
            "../../../src/runtime_pool.rs",
            include_bytes!("../../../src/runtime_pool.rs").as_slice(),
        ),
        (
            "../../../src/runtime_diagnostics.rs",
            include_bytes!("../../../src/runtime_diagnostics.rs").as_slice(),
        ),
        (
            "../../../src/dispatch.rs",
            include_bytes!("../../../src/dispatch.rs").as_slice(),
        ),
        (
            "../../../../mount-rs-sdk/src/filesystem.rs",
            include_bytes!("../../../../mount-rs-sdk/src/filesystem.rs").as_slice(),
        ),
        (
            "../../../../mount-rs-sdk/src/providers.rs",
            include_bytes!("../../../../mount-rs-sdk/src/providers.rs").as_slice(),
        ),
        (
            "../../../../mount-rs-sdk/src/construction.rs",
            include_bytes!("../../../../mount-rs-sdk/src/construction.rs").as_slice(),
        ),
        (
            "../../../../mount-rs-sdk/src/options.rs",
            include_bytes!("../../../../mount-rs-sdk/src/options.rs").as_slice(),
        ),
        (
            "../../../../../src/construction.rs",
            include_bytes!("../../../../../src/construction.rs").as_slice(),
        ),
        (
            "../remote_blocks.rs",
            include_bytes!("../remote_blocks.rs").as_slice(),
        ),
        (
            "../filesystem_preflight.rs",
            include_bytes!("../filesystem_preflight.rs").as_slice(),
        ),
        (
            "../filesystem_preflight_tests.rs",
            include_bytes!("../filesystem_preflight_tests.rs").as_slice(),
        ),
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
            "../resource_profile/sqlite_heap.rs",
            include_bytes!("../resource_profile/sqlite_heap.rs").as_slice(),
        ),
        (
            "../device_io.rs",
            include_bytes!("../device_io.rs").as_slice(),
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
            "sdk_runtime":cfg!(feature="sdk-runtime"),
            "debug_assertions":cfg!(debug_assertions),
            "scope":"current checkout native fixture; dirty source disclosed; not clean committed CI"}
    ))
}
struct Journal {
    value: Value,
    output: std::path::PathBuf,
    counts: Vec<Arc<Mutex<state::Counts>>>,
    enclosing_deadline: Instant,
    phase_deadline: Instant,
    progress: progress::Progress,
    oracle_progress: Option<oracle::ProgressView>,
}
fn project_resource_progress(
    fleet: &mut Fleet,
    resources: &resources::Resources,
    progress: &mut progress::Progress,
    boundary: bool,
) {
    if boundary {
        progress.resource_boundary();
    }
    if !progress.resources_due() {
        return;
    }
    progress.resource(
        progress::ResourceOwner {
            pid: std::process::id(),
            worker: None,
            generation_context: None,
        },
        || Ok(Some(resources.snapshot())),
    );
    fleet.project_resources(progress);
    progress.resources_done();
}
impl Journal {
    fn publish_quic_boundary(
        &self,
        base: &Value,
        mode: &str,
        pattern: &str,
        network: &Value,
        lanes: usize,
    ) -> Result<Value, String> {
        quic_artifacts::publish_boundary(
            &self.output,
            base,
            mode,
            pattern,
            network,
            lanes,
            self.phase_deadline.min(self.enclosing_deadline),
        )
    }
    fn flush(&mut self) -> Result<(), String> {
        let span = metrics::observer().begin("journal_publication");
        if let Some(oracle) = &self.oracle_progress {
            self.value["fresh_oracle_progress"] =
                progress::public_oracle_snapshot(&oracle.snapshot());
        }
        self.value["lanes"] = serde_json::to_value(
            self.counts
                .iter()
                .map(|c| c.lock().unwrap().clone())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        self.value["updated_unix_ms"] = json!(utc_ms());
        self.progress.observe(&self.value);
        let result = write_json(&self.output.join("terminal.json"), &self.value);
        span.finish(result.is_ok(), 0);
        result
    }
    fn phase(&mut self, name: &str) -> Result<(), String> {
        self.phase_at(name, Instant::now(), utc_ms())
    }
    fn phase_at(&mut self, name: &str, now: Instant, now_unix_ms: u64) -> Result<(), String> {
        self.phase_with_budget_at(name, PHASE_SECONDS, now, now_unix_ms)
    }
    fn phase_with_budget(&mut self, name: &str, seconds: u64) -> Result<(), String> {
        self.phase_with_budget_at(name, seconds, Instant::now(), utc_ms())
    }
    fn phase_with_budget_at(
        &mut self,
        name: &str,
        seconds: u64,
        now: Instant,
        now_unix_ms: u64,
    ) -> Result<(), String> {
        if now >= self.phase_deadline {
            return Err("previous phase exhausted its inherited deadline".into());
        }
        // A phase allowance never renews the enclosing work deadline.
        self.phase_deadline = self
            .enclosing_deadline
            .min(now + Duration::from_secs(seconds));
        let now = now_unix_ms;
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
    let deadline = journal
        .phase_deadline
        .min(Instant::now() + Duration::from_secs(seconds));
    if Instant::now() >= deadline {
        return Err("inherited workload phase deadline".into());
    }
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tokio::pin!(future);
    loop {
        tokio::select! {
                biased;
                _=tokio::time::sleep_until(deadline.into())=>return Err("inherited workload phase deadline; partial work incomplete".into()),
                result=&mut future=>return if Instant::now() < deadline { result } else { Err("work completed after inherited phase deadline".into()) },
        _=tick.tick()=>{
                resources.check()?;
                fleet.check()?;
                journal.value["controller_resources"]=resources.snapshot();
                journal.flush()?;
                project_resource_progress(fleet, resources, &mut journal.progress, false);
                }
                }
    }
}
trait OracleBoundary {
    fn accounting(&self) -> &metrics::Accounting;
    fn settled(&self) -> bool;
}
impl OracleBoundary for oracle::Pool {
    fn accounting(&self) -> &metrics::Accounting {
        &self.accounting
    }
    fn settled(&self) -> bool {
        oracle::Pool::settled(self)
    }
}
impl OracleBoundary for oracle::Owner {
    fn accounting(&self) -> &metrics::Accounting {
        &self.accounting
    }
    fn settled(&self) -> bool {
        oracle::Owner::settled(self)
    }
}
async fn metric_boundary(
    collector: &mut metrics::Collector,
    fleet: &mut Fleet,
    private: &PrivateConfig,
    resources: &resources::Resources,
    journal: &mut Journal,
    boundary: (&str, &str),
    oracle: &impl OracleBoundary,
) -> Result<Duration, String> {
    if !oracle.settled() {
        collector.complete = false;
        return Err("metric boundary has unsettled fresh oracle slots".into());
    }
    if journal.counts.iter().any(|counts| {
        let counts = counts.lock().unwrap();
        counts.pending.is_some() || counts.uncertain != 0
    }) {
        collector.complete = false;
        return Err("metric boundary has pending or uncertain owned work".into());
    }
    collector.complete &= fleet.startup_accounting_complete() && journal.progress.complete();
    let deadline = journal.phase_deadline;
    let result = tokio::time::timeout_at(
        deadline.into(),
        collector.boundary(
            fleet,
            private,
            resources,
            boundary,
            oracle.accounting().snapshot(),
            deadline,
        ),
    )
    .await
    .map_err(|_| "metric inherited enclosing phase deadline".to_string())
    .and_then(|result| result);
    if result.is_err() {
        collector.complete = false;
    }
    journal.value["phase_metrics"] = collector.summary();
    journal.flush()?;
    project_resource_progress(fleet, resources, &mut journal.progress, true);
    if !journal.progress.complete() || !oracle.settled() {
        collector.complete = false;
        return Err("metric boundary publication or fresh oracle settlement incomplete".into());
    }
    if Instant::now() >= deadline {
        collector.complete = false;
        return Err("metric publication exceeded inherited phase deadline".into());
    }
    result
}
async fn connect(
    endpoint: &quinn::Endpoint,
    fleet: &Fleet,
    tokens: &SignedTokens,
    drive: usize,
    server: usize,
) -> Result<quinn::Connection, String> {
    let address = fleet.children[server]
        .ready
        .as_ref()
        .ok_or("worker readiness missing")?
        .address;
    connect_address(endpoint, address, tokens, drive).await
}
fn checked_oracle_pass(
    snapshot: &Value,
    pass: &str,
    expected: &[&state::Expected],
    verified: &oracle::Verified,
) -> Result<Value, String> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let mut drives = std::collections::BTreeSet::new();
    for ledger in expected {
        if !drives.insert(ledger.drive as u64) {
            return Err("fresh oracle expected Drive repeated".into());
        }
        files = files
            .checked_add(ledger.files.len() as u64)
            .ok_or("fresh oracle file total overflow")?;
        for file in ledger.files.values() {
            if !file.length.is_multiple_of(4096) {
                return Err("fresh oracle expected length is not block aligned".into());
            }
            bytes = bytes
                .checked_add(file.length as u64)
                .ok_or("fresh oracle byte total overflow")?;
        }
    }
    let public = progress::public_oracle_snapshot(snapshot);
    let completed = snapshot["completed_drive_ids"]
        .as_array()
        .ok_or("fresh oracle completion roster missing")?;
    let completed: Vec<_> = completed
        .iter()
        .map(|drive| {
            drive
                .as_u64()
                .ok_or("fresh oracle completion identity invalid")
        })
        .collect::<Result<_, _>>()?;
    if !matches!(pass, "initial" | "final")
        || public["pass"] != pass
        || public["slot_limit"] != oracle::FRESH_ORACLE_SLOTS
        || public["expected_drives"] != expected.len()
        || public["complete"] != true
        || public["settled"] != true
        || public["expected_files"] != files
        || public["expected_bytes"] != bytes
        || verified.files != files
        || verified.bytes != bytes
        || completed != drives.into_iter().collect::<Vec<_>>()
    {
        return Err("fresh oracle complete corpus proof mismatch".into());
    }
    Ok(public)
}
fn record_oracle_pass(
    journal: &mut Journal,
    pool: &oracle::Pool,
    pass: &str,
    expected: &[&state::Expected],
    verified: &oracle::Verified,
) -> Result<(), String> {
    if !pool.settled() || Instant::now() >= journal.phase_deadline {
        return Err("fresh oracle receipt settlement or inherited deadline incomplete".into());
    }
    let snapshot = pool.progress().snapshot();
    let mut summary = checked_oracle_pass(&snapshot, pass, expected, verified)?;
    let passes = journal.value["fresh_oracle_passes"]
        .as_array()
        .ok_or("fresh oracle pass receipts missing")?;
    if passes.len() != usize::from(pass == "final") {
        return Err("fresh oracle pass receipt order invalid".into());
    }
    let directory = journal.output.join("oracle-receipts");
    std::fs::create_dir_all(&directory)
        .map_err(|_| "fresh oracle private receipt directory unavailable")?;
    let path = directory.join(format!("{pass}.json"));
    if path.exists() {
        return Err("fresh oracle refuses existing pass receipt".into());
    }
    write_json(&path, &snapshot)?;
    let sha256 = file_digest(&path)?;
    if Instant::now() >= journal.phase_deadline {
        return Err("fresh oracle receipt exceeded inherited deadline".into());
    }
    summary["after_boundary_complete"] = json!(true);
    summary["receipt"] = json!({"file":format!("oracle-receipts/{pass}.json"),"sha256":sha256});
    journal.value["fresh_oracle_passes"]
        .as_array_mut()
        .unwrap()
        .push(summary);
    Ok(())
}
fn oracle_passes_complete(journal: &Value) -> bool {
    let Some(passes) = journal["fresh_oracle_passes"].as_array() else {
        return false;
    };
    if passes.len() != 2 || journal["verified_passes"] != 2 {
        return false;
    }
    for (pass, label) in passes.iter().zip(["initial", "final"]) {
        let public = progress::public_oracle_snapshot(pass);
        if public["pass"] != label
            || public["complete"] != true
            || public["settled"] != true
            || public["slot_limit"] != oracle::FRESH_ORACLE_SLOTS
            || public["expected_drives"] != journal["configuration"]["drives"]
            || pass["after_boundary_complete"] != true
        {
            return false;
        }
    }
    passes[1]["completed_files"] == journal["verified_files"]
        && passes[1]["completed_bytes"] == journal["verified_bytes"]
}

#[cfg(test)]
mod oracle_receipt_tests {
    use super::*;

    fn corpus() -> (state::Expected, state::Expected, Value) {
        let mut first = state::Expected::empty(4, 2);
        first.create("payload".into(), 0);
        first.write("payload", 0, 0);
        first.create("empty".into(), 1);
        let mut second = state::Expected::empty(9, 1);
        second.create("other".into(), 0);
        second.write("other", 0, 0);
        let snapshot = json!({"pass":"initial","slot_limit":8,
            "expected_drives":2,"started_drives":2,"completed_drives":2,"live_slots":0,
            "expected_files":3,"completed_files":3,"checked_files":3,
            "expected_bytes":8192,"completed_bytes":8192,"compared_bytes":8192,
            "complete":true,"settled":true,"completed_drive_ids":[4,9]});
        (first, second, snapshot)
    }

    #[test]
    fn oracle_receipt_requires_exact_unique_corpus_beyond_matching_counts() {
        let (first, second, snapshot) = corpus();
        let expected = [&first, &second];
        let verified = oracle::Verified {
            files: 3,
            bytes: 8192,
        };
        let public = checked_oracle_pass(&snapshot, "initial", &expected, &verified).unwrap();
        assert_eq!(public.as_object().unwrap().len(), 14);
        assert!(public.get("completed_drive_ids").is_none());
        for roster in [
            json!([4, 4]),
            json!([4, 8]),
            json!([9, 4]),
            json!([4]),
            json!([4, "9"]),
        ] {
            let mut wrong = snapshot.clone();
            wrong["completed_drive_ids"] = roster;
            assert!(checked_oracle_pass(&wrong, "initial", &expected, &verified).is_err());
        }
        assert!(checked_oracle_pass(&snapshot, "initial", &[&first, &first], &verified).is_err());
        let wrong = oracle::Verified {
            files: 3,
            bytes: 4096,
        };
        assert!(checked_oracle_pass(&snapshot, "initial", &expected, &wrong).is_err());
    }

    #[test]
    fn oracle_receipt_rejects_partial_wrong_pass_slots_and_unaligned_tail() {
        let (mut first, second, snapshot) = corpus();
        let verified = oracle::Verified {
            files: 3,
            bytes: 8192,
        };
        for (field, value) in [
            ("complete", json!(false)),
            ("settled", json!(false)),
            ("live_slots", json!(1)),
            ("slot_limit", json!(1)),
            ("pass", json!("final")),
            ("completed_files", json!(2)),
            ("compared_bytes", json!(4096)),
        ] {
            let mut wrong = snapshot.clone();
            wrong[field] = value;
            assert!(
                checked_oracle_pass(&wrong, "initial", &[&first, &second], &verified).is_err(),
                "{field}"
            );
        }
        first.truncate("payload", 4097);
        assert!(checked_oracle_pass(&snapshot, "initial", &[&first, &second], &verified).is_err());
    }

    #[test]
    fn oracle_receipt_terminal_requires_both_after_boundaries_and_final_totals() {
        let (_, _, mut initial) = corpus();
        initial["after_boundary_complete"] = json!(true);
        let mut final_pass = initial.clone();
        final_pass["pass"] = json!("final");
        // Final acknowledged append changes the proof; population totals cannot substitute.
        for field in ["expected_bytes", "completed_bytes", "compared_bytes"] {
            final_pass[field] = json!(12288);
        }
        let journal = json!({"configuration":{"drives":2},"verified_passes":2,
            "verified_files":3,"verified_bytes":12288,"fresh_oracle_passes":[initial,final_pass]});
        assert!(oracle_passes_complete(&journal));
        let mut wrong = journal.clone();
        wrong["fresh_oracle_passes"][0]["after_boundary_complete"] = json!(false);
        assert!(!oracle_passes_complete(&wrong));
        let mut wrong = journal.clone();
        wrong["verified_bytes"] = json!(8192);
        assert!(!oracle_passes_complete(&wrong));
        let mut wrong = journal.clone();
        wrong["fresh_oracle_passes"][1]["pass"] = json!("initial");
        assert!(!oracle_passes_complete(&wrong));
    }
}
async fn connect_address(
    endpoint: &quinn::Endpoint,
    address: std::net::SocketAddr,
    tokens: &SignedTokens,
    drive: usize,
) -> Result<quinn::Connection, String> {
    tokio::time::timeout(
        Duration::from_secs(REQUEST_SECONDS),
        wire::connect_token(
            endpoint,
            address,
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
    let setup_deadline = Instant::now() + Duration::from_secs(600);
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
                    "population_seconds":null,
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
        enclosing_deadline: setup_deadline,
        phase_deadline: setup_deadline,
        progress: progress::Progress::disabled(),
        oracle_progress: None,
    };
    journal.flush()?;
    let mut fleet = Fleet::new();
    let mut endpoints = Vec::new();
    let mut lanes = Vec::new();
    let mut private_path = None;
    let mut resources = None;
    let mut oracle_owner = oracle::Pool::new(oracle::FRESH_ORACLE_SLOTS)?;
    journal.oracle_progress = Some(oracle_owner.progress());
    journal.value["fresh_oracle_passes"] = json!([]);
    let mut initializer_owner = oracle::Owner::default();
    let mut initialization_receipts = Vec::new();
    let mut initialization_start = None;
    let mut commands = command::Commands::default();
    let mut provider_preflight = preflight::Owner::default();
    let mut phase_metrics = None;
    let result: Result<(), String> = async {
        let setup = async {
            phase_metrics=Some(metrics::Collector::new()?);
            let config = Config::environment()?;
            journal.value["budgets"]["population_seconds"] = json!(config.population_seconds);
            let block_provider = remote_blocks::selector_from_environment("MOUNT_RS_TARGET_BLOCK_PROVIDER")?
                .unwrap_or_else(|| "metadata".into());
            // Validate roles and all explicit settings before identity/open/provisioning.
            let selected_blocks = remote_blocks::resolve_blocks(&config.provider, "target-preflight/blocks",
                Some(&block_provider), |name| std::env::var(name).ok())?;
            journal.value["metadata_provider"] = json!(config.provider);
            journal.value["block_provider"] = json!(match &selected_blocks {
                Some(mount_rs_sdk::StoreConfig::RustFs { .. }) => "rustfs",
                Some(mount_rs_sdk::StoreConfig::Filesystem { .. }) => "filesystem",
                Some(_) => return Err("target block provider shape invalid".into()),
                None => config.provider.as_str(),
            });
            journal.value["block_provider_selection"] = json!({"selector":"MOUNT_RS_TARGET_BLOCK_PROVIDER","requested":block_provider});
            journal.progress = progress::Progress::new(mount_rs_core::diagnostics::profile::enabled(), &config);
            journal.progress.start();
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
            journal.progress.source(&journal.value["source"]);
            journal.flush()?;
            resources = Some(resources::Resources::start(
                output.join("controller-resources.json"),
            )?);
            project_resource_progress(&mut fleet, resources.as_ref().unwrap(), &mut journal.progress, true);
            let host_free_bytes = resources::disk_available()?;
            journal.progress.capacity(host_free_bytes);
            if host_free_bytes < config::DISK_FLOOR {
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
            if let Some(blocks) = &selected_blocks {
                let receipt = remote_blocks::preflight(&mut commands, blocks).await?;
                match blocks {
                    mount_rs_sdk::StoreConfig::RustFs { .. } => {
                        write_json(&output.join("rustfs-preflight.json"), &receipt)?;
                        journal.value["rustfs_preflight"] = receipt;
                    }
                    mount_rs_sdk::StoreConfig::Filesystem { .. } => {
                        write_json(&output.join("filesystem-preflight.json"), &receipt)?;
                        journal.value["filesystem_preflight"] = receipt;
                    }
                    _ => return Err("target preflight block provider shape invalid".into()),
                }
            }
            let directory = output.join("private");
            std::fs::create_dir(&directory).map_err(|_| "private fixture directory unavailable")?;
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "private directory permissions failed")?;
            let backend = backend::Backend {
                provider: config.provider.clone(),
                block_provider,
                root: directory.clone(),
                prefix: format!("production-target-{}-{}", std::process::id(), utc_ms()),
            };
            journal.phase("empty_drive_initialization")?;
            project_resource_progress(&mut fleet, resources.as_ref().unwrap(), &mut journal.progress, false);
            initialization_start = Some(Instant::now());
            journal.value["initialization"] = json!({"scope":"sequential empty MRC5 roots/backings before steady-state workers; no namespace/payload preseed; parallel virgin-start unqualified","complete":false,"initialized_drives":0,"cleanup_confirmed":false});
            let mut last_progress = Instant::now();
            for drive in 0..config.drives {
                resources.as_ref().unwrap().check()?;
                backend.prepare_blocks(drive)?;
                let receipt = initializer_owner.initialize_empty(&backend, drive).await?;
                initialization_receipts.push(receipt);
                journal.value["initialization"]["initialized_drives"] = json!(initialization_receipts.len());
                journal.value["initialization"]["elapsed_seconds"] = json!(initialization_start.unwrap().elapsed().as_secs_f64());
                if last_progress.elapsed() >= Duration::from_secs(1) {
                    journal.flush()?;
                    project_resource_progress(&mut fleet, resources.as_ref().unwrap(), &mut journal.progress, false);
                    last_progress = Instant::now();
                }
            }
            initializer_owner.close().await?;
            journal.value["initialization"]["cleanup_confirmed"] = json!(true);
            journal.value["initialization"]["complete"] = json!(true);
            journal.value["initialization"]["elapsed_seconds"] = json!(initialization_start.take().unwrap().elapsed().as_secs_f64());
            let expected_backings = initialization_receipts.iter().enumerate()
                .map(|(drive, receipt)| process::validate_backing_receipt(receipt, drive))
                .collect::<Result<Vec<_>, _>>()?;
            let expected_backings_sha256 = digest(&serde_json::to_vec(&expected_backings)
                .map_err(|_| "initializer manifest encoding failed")?);
            fleet.expect_initialized_backings(expected_backings.clone());
            journal.value["initialization"]["expected_backings_sha256"] = json!(expected_backings_sha256);
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
                expected_backings,
                expected_backings_sha256,
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
            fleet.ready(&private, 0, &mut journal.progress, resources.as_ref().unwrap()).await?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resources.as_ref().unwrap(),&mut journal,("worker_setup","after_ready"),&initializer_owner).await?;
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
            tokio::time::timeout_at(setup_deadline.into(), setup)
                .await
                .map_err(|_| "complete setup deadline; retained owners require cleanup")??;
        let resource = resources.as_ref().ok_or("setup resource owner missing")?;
        let work_started = Instant::now();
        let work_deadline = work_started + Duration::from_secs(WORK_SECONDS);
        journal.enclosing_deadline = work_deadline;
        journal.phase_deadline = work_deadline;
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
            tokio::time::timeout_at(journal.phase_deadline.into(), async {
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
                journal.value["connected_clients"] = json!(lanes.len());
                if journal.progress.observe(&journal.value) {
                    journal.flush()?;
                    project_resource_progress(&mut fleet, resource, &mut journal.progress, false);
                }
            }
                Ok::<_, String>(())
            }).await.map_err(|_| "signed connections inherited phase deadline")??;
            journal.value["connected_clients"] = json!(lanes.len());
            journal.phase_with_budget("online_namespace", config.population_seconds)?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("online_namespace","before"),&oracle_owner).await?;
            supervised(&mut fleet, resource, &mut journal, config.population_seconds, async {
                futures_util::future::try_join_all(
                    lanes.iter_mut().map(|l| l.populate(config.files, false)),
                )
                .await?;
                Ok(())
            })
            .await?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("online_namespace","after"),&oracle_owner).await?;
            journal.value["namespace_files"] = json!(lanes.iter().map(|l|l.expected.files.len()as u64).sum::<u64>());
            journal.phase_with_budget("online_payload", config.population_seconds)?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("online_payload","before"),&oracle_owner).await?;
            supervised(&mut fleet, resource, &mut journal, config.population_seconds, async {
                futures_util::future::try_join_all(
                    lanes.iter_mut().map(|l| l.populate(config.files, true)),
                )
                .await?;
                Ok(())
            })
            .await?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("online_payload","after"),&oracle_owner).await?;
            journal.value["population_bytes"] = json!(lanes.iter().flat_map(|l|l.expected.files.values()).map(|f|f.length as u64).sum::<u64>());
            journal.phase("initial_fresh_oracle")?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("initial_fresh_oracle","before"),&oracle_owner).await?;
            let expected: Vec<_> = lanes.iter().map(|lane| &lane.expected).collect();
            let oracle_deadline = journal.phase_deadline;
            let verification = supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS,
                oracle_owner.verify_pass(oracle::Pass::Initial, &backend, &expected, oracle_deadline)).await;
            // Publish even an immediate failure before switching to cleanup context.
            journal.flush()?;
            let verified = verification?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("initial_fresh_oracle","after"),&oracle_owner).await?;
            record_oracle_pass(&mut journal, &oracle_owner, "initial", &expected, &verified)?;
            journal.value["verified_passes"] = json!(1);
            // End old connections and all replicas before reopening each worker.
            journal.phase("refresh_replicas")?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("refresh_replicas","before"),&oracle_owner).await?;
            for lane in &lanes {
                lane.connection.close(0u32.into(), b"replica refresh");
            }
            fleet.command("reopen", 1)?;
            tokio::time::timeout_at(journal.phase_deadline.into(), fleet.ready(&private, 1, &mut journal.progress, resource))
                .await.map_err(|_| "replica refresh inherited phase deadline")??;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("refresh_replicas","after_ready"),&oracle_owner).await?;

            tokio::time::timeout_at(journal.phase_deadline.into(), async {
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
                Ok::<_, String>(())
            }).await.map_err(|_| "replica reconnect inherited phase deadline")??;
            journal.phase("routes_and_scope")?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("routes_and_scope","before"),&oracle_owner).await?;
            let (route_probes, sibling, partition) = tokio::time::timeout_at(journal.phase_deadline.into(), async {
            let mut routes = 0;
            let mut sibling = 0;
            let mut partition = 0;
            for (server, endpoint) in endpoints.iter().enumerate() {
                for drive in 0..config.drives {
                    let connection = connect(endpoint, &fleet, &tokens, drive, server).await?;
                    // A fresh session has no handle0. Its typed EBADF follows
                    // current catalog authorization, definition and route checks,
                    // without acquiring a filesystem runtime. Backing access is
                    // proved by assigned warmup and the post-profile Stat batches.
                    let response = tokio::time::timeout(
                        Duration::from_secs(REQUEST_SECONDS),
                        wire::request(
                            &connection,
                            1,
                            &format!("sandbox-{drive}"),
                            mount_rs_remote_protocol::OperationName::HandleClose,
                            json!({"handle":0}),
                        ),
                    )
                    .await
                    .map_err(|_| "route request deadline")??;
                    if response != Err("EBADF".into()) {
                        return Err("nonactivating route binding proof missing".into());
                    }
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
                Ok::<_, String>((routes, sibling, partition))
            }).await.map_err(|_| "routes inherited phase deadline")??;
            journal.value["nonactivating_routes"] = json!(route_probes);
            journal.value["nonactivating_route_scope"] = json!("fresh authenticated handle0 EBADF after current catalog and Drive binding checks; no backing or runtime health proof");
            journal.value["scope_denials"] = json!({
                    "sibling":sibling,
                    "partition":partition}
            );
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("routes_and_scope","after"),&oracle_owner).await?;
            journal.phase("assigned_warmup")?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("assigned_warmup","before"),&oracle_owner).await?;
            let mut warmed = 0;
            let warmup = supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS, async {
                for lane in &mut lanes {
                    lane.request(
                        mount_rs_remote_protocol::OperationName::Stat,
                        json!({"path":"/mixed-0"}),
                    ).await?;
                    warmed += 1;
                }
                Ok(())
            }).await;
            journal.value["assigned_warmup"] = json!({"acknowledged_stats":warmed,"expected_stats":config.drives,"complete":warmup.is_ok(),"scope":"actual I/O for each sandbox on its assigned server; caches may warm before timed modes"});
            journal.flush()?;
            warmup?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("assigned_warmup","after"),&oracle_owner).await?;
            for mostly_idle in [true, false] {
                for pattern in PATTERNS {
                    let mode = if mostly_idle {
                        "mostly_idle"
                    } else {
                        "all_active"
                    };
                    journal.phase(&format!("{mode}/{pattern}"))?;
                    let phase_begin = Instant::now();
                    let metrics_first_sequence=phase_metrics.as_ref().unwrap().sequence+1;
                    metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,(&format!("{mode}/{pattern}"),"before_active"),&oracle_owner).await?;
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
                    let observer_start=tokio::time::Instant::now();
                    metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,(&format!("{mode}/{pattern}"),"after_active"),&oracle_owner).await?;
                    clock.add_observer_elapsed(observer_start.elapsed());
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
                    let observer_start=tokio::time::Instant::now();
                    metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,(&format!("{mode}/{pattern}"),"after_idle"),&oracle_owner).await?;
                    clock.add_observer_elapsed(observer_start.elapsed());
                    let observer_start=tokio::time::Instant::now();
                    let network_after =
                        resource_profile::Snapshot::capture_connections(&connections)
                            .map_err(|_| "boundary network terminal unavailable")?;
                    let network = network_after
                        .connection_deltas(&network_before)
                        .map_err(|_| "boundary network delta unavailable")?;
                    let last_boundary = phase_metrics.as_ref().unwrap().records.last()
                        .ok_or("QUIC after-idle metric boundary unavailable")?;
                    let generation = last_boundary["generation"].as_u64()
                        .ok_or("QUIC after-idle metric generation unavailable")?;
                    if last_boundary["phase"] != format!("{mode}/{pattern}")
                        || last_boundary["boundary"] != "after_idle"
                        || last_boundary["sequence"].as_u64() != Some(metrics_first_sequence+2)
                    {
                        return Err("QUIC after-idle metric binding changed".into());
                    }
                    let identity = metrics::identity(
                        &private, std::process::id(), None, generation,
                        metrics_first_sequence+2, &format!("{mode}/{pattern}"), "after_idle",
                    );
                    let quic_boundary = journal.publish_quic_boundary(
                        &identity, mode, pattern, &network, config.drives,
                    )?;
                    clock.add_observer_elapsed(observer_start.elapsed());
                    let timing = clock.finish(cycles.iter().sum());
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
                                "metric_sequences":[metrics_first_sequence,metrics_first_sequence+1,metrics_first_sequence+2],
                                "rpc_latency_histogram_log2_microseconds":latency_histogram,
                                "timing":timing,"controller_quic_boundary":quic_boundary,
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
            // Keep the complete actual cross-server storage proof after the
            // balanced timed modes. Every rotation starts only after the old
            // generation's acknowledged drain; no batch receives a fresh phase.
            journal.phase("crossnode_routes")?;
            journal.value["crossnode_route_batches"] = json!([]);
            let mut payload_coverage = CrossnodeCoverage::new(config.drives)?;
            journal.value["crossnode_payload"] = json!({"verified_reads":0,"verified_bytes":0,"completed_pairs":0,"expected_pairs":payload_coverage.expected_pairs(),"complete":false,"scope":"one existing acknowledged 4096-byte block per Drive/server pair over rotated QUIC sessions; no payload writes; outside timed modes"});
            let mut routes = 0;
            for rotation in 1..=SERVERS {
                let generation = rotation as u64 + 1;
                let offset = rotation % SERVERS;
                for lane in &lanes {
                    lane.connection.close(0u32.into(), b"post-profile rotation");
                }
                fleet.command("reopen", generation)?;
                tokio::time::timeout_at(journal.phase_deadline.into(), fleet.ready(&private, generation, &mut journal.progress, resource))
                    .await.map_err(|_| "crossnode refresh inherited phase deadline")??;
                let ready_sequence = phase_metrics.as_ref().unwrap().sequence + 1;
                metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("crossnode_routes","after_ready"),&oracle_owner).await?;
                // Retain the just-validated addresses separately so the owned
                // supervisor can keep checking the mutable Fleet while I/O runs.
                let addresses = fleet.children.iter().map(|child| {
                    child.ready.as_ref().map(|ready| ready.address).ok_or("worker readiness missing")
                }).collect::<Result<Vec<_>, _>>()?;
                if addresses.len() != SERVERS {
                    return Err("crossnode worker geometry missing".into());
                }
                let mut batch_routes = 0;
                let mut batch_reads = 0;
                let mut batch_bytes = 0;
                let batch = supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS, async {
                    for (drive, lane) in lanes.iter_mut().enumerate() {
                        let server = (drive + offset) % SERVERS;
                        lane.connection = connect_address(&endpoints[server], addresses[server], &tokens, drive).await?;
                        lane.request(
                            mount_rs_remote_protocol::OperationName::Stat,
                            json!({"path":"/mixed-0"}),
                        ).await?;
                        batch_routes += 1;
                        let bytes = lane.crossnode_sentinel().await?;
                        payload_coverage.record(drive, server)?;
                        batch_reads += 1;
                        batch_bytes += bytes;
                        lane.connection.close(0u32.into(), b"post-profile payload verified");
                    }
                    Ok(())
                }).await;
                routes += batch_routes;
                journal.value["routes"] = json!(routes);
                let completed_pairs = payload_coverage.completed_pairs();
                journal.value["crossnode_payload"]["verified_reads"] = json!(completed_pairs);
                journal.value["crossnode_payload"]["verified_bytes"] = json!(completed_pairs * 4096);
                journal.value["crossnode_payload"]["completed_pairs"] = json!(completed_pairs);
                journal.value["crossnode_route_batches"].as_array_mut().unwrap().push(json!({"generation":generation,"offset":offset,"acknowledged_stats":batch_routes,"expected_stats":config.drives,"verified_reads":batch_reads,"verified_bytes":batch_bytes,"expected_reads":config.drives,"expected_bytes":config.drives*4096,"ready_sequence":ready_sequence,"rpc_complete":batch.is_ok(),"validation_complete":false}));
                journal.flush()?;
                batch?;
                if batch_routes != config.drives || batch_reads != config.drives || batch_bytes != config.drives * 4096 {
                    return Err("crossnode payload batch incomplete".into());
                }
                let batch_sequence = phase_metrics.as_ref().unwrap().sequence + 1;
                metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("crossnode_routes","after_batch"),&oracle_owner).await?;
                let receipt = journal.value["crossnode_route_batches"].as_array_mut().unwrap().last_mut().unwrap();
                receipt["batch_sequence"] = json!(batch_sequence);
                receipt["validation_complete"] = json!(true);
                journal.flush()?;
            }
            if routes != config.drives * SERVERS {
                return Err("complete crossnode route coverage missing".into());
            }
            payload_coverage.verify_complete()?;
            journal.value["crossnode_payload"]["complete"] = json!(true);
            journal.flush()?;
            // The last rotation has offset0. Retain fresh original-assignment
            // sessions for the existing final durability and revocation checks.
            tokio::time::timeout_at(journal.phase_deadline.into(), async {
                for (drive, lane) in lanes.iter_mut().enumerate() {
                    lane.connection = connect(&endpoints[drive % SERVERS], &fleet, &tokens, drive, drive % SERVERS).await?;
                }
                Ok::<_, String>(())
            }).await.map_err(|_| "post-profile reconnect inherited phase deadline")??;
            journal.phase("final_fresh_oracle")?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("final_fresh_oracle","before"),&oracle_owner).await?;
            let expected: Vec<_> = lanes.iter().map(|lane| &lane.expected).collect();
            let oracle_deadline = journal.phase_deadline;
            let verification = supervised(&mut fleet, resource, &mut journal, PHASE_SECONDS,
                oracle_owner.verify_pass(oracle::Pass::Final, &backend, &expected, oracle_deadline)).await;
            journal.flush()?;
            let verified = verification?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("final_fresh_oracle","after"),&oracle_owner).await?;
            record_oracle_pass(&mut journal, &oracle_owner, "final", &expected, &verified)?;
            journal.value["verified_passes"] = json!(2);
            journal.value["verified_files"] = json!(verified.files);
            journal.value["verified_bytes"] = json!(verified.bytes);
            journal.phase("revocation")?;
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("revocation","before"),&oracle_owner).await?;
            tokio::time::timeout_at(journal.phase_deadline.into(), async {
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
                Ok::<_, String>(())
            }).await.map_err(|_| "revocation inherited phase deadline")??;
            journal.value["revocation_denials"] = json!(lanes.len());
            metric_boundary(phase_metrics.as_mut().unwrap(),&mut fleet,&private,resource,&mut journal,("revocation","after"),&oracle_owner).await?;

            Ok(())
        };
        let result = tokio::time::timeout_at(work_deadline.into(), work)
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
    journal.progress.cleanup_context();
    let mut cleanup = fleet
        .cleanup(resources.as_ref(), &mut journal.progress)
        .await;
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
    if let Err(error) = oracle_owner.close_once().await {
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
    let expected_span = metrics::observer().begin("expected_state_observation");
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
    let mut expected_drives = 0usize;
    let declared_drives = journal.value["configuration"]["drives"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0);
    let expected_deadline = expected_start + Duration::from_secs(30);
    for (pack, group) in lanes.chunks(artifacts::PACK_DRIVES).enumerate() {
        if expected_start.elapsed() > Duration::from_secs(30) {
            cleanup.push(json!({
                    "error":"expected-state terminal receipt deadline"}
            ));
            break;
        }
        let borrowed = group.iter().map(|lane| &lane.expected).collect::<Vec<_>>();
        match artifacts::publish_pack(&output, pack, &borrowed, expected_deadline) {
            Ok(receipt) => {
                expected_drives += receipt.count;
                expected_receipts.push(receipt);
            }
            Err(error) => {
                cleanup.push(json!({"error":error}));
                break;
            }
        }
        if expected_start.elapsed() > Duration::from_secs(30) {
            cleanup.push(json!({"error":"expected-state receipt exceeded30s after write/hash"}));
            break;
        }
    }
    let expected_complete = declared_drives > 0
        && expected_drives == declared_drives
        && expected_drives == lanes.len()
        && expected_start.elapsed() <= Duration::from_secs(30);
    if !expected_complete {
        cleanup.push(json!({"error":"expected-state observation incomplete"}));
    }
    journal.value["expected_state_observation"] = json!({"complete":expected_complete,"elapsed_seconds":expected_start.elapsed().as_secs_f64(),"deadline_seconds":30,"bound":"cooperative post-write/hash checks; blocked OS I/O cannot be interrupted"});
    journal.value["expected_state_receipts"] = json!({
        "schema":artifacts::INVENTORY_SCHEMA,"drive_count":declared_drives,
        "pack_size":artifacts::PACK_DRIVES,"packs":expected_receipts});
    expected_span.finish(expected_complete, 0);
    let audit_span = metrics::observer().begin("audit_observation");
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
    audit_span.finish(
        audit_complete,
        audit.iter().filter_map(|v| v["bytes"].as_u64()).sum(),
    );
    journal.value["worker_audit_logs"] = json!(audit);
    journal.value["audit_observation"] = json!({"complete":audit_complete,"elapsed_seconds":audit_start.elapsed().as_secs_f64(),"deadline_seconds":30,"bound":"cooperative checks before/after each OS read; cannot interrupt blocked filesystem calls"});
    let worker_receipts = fleet.receipts();
    journal.value["workers"] = json!(worker_receipts);
    journal.value["cleanup_errors"] = json!(cleanup);
    journal.value["error"] = json!(result.as_ref().err());
    if let Some(collector) = &mut phase_metrics {
        collector.complete &= oracle_owner.settled();
        if let Err(error) = collector.terminal_workers(&fleet, audit_deadline) {
            collector.complete = false;
            journal.value["worker_metrics_terminal_error"] = json!(error);
        }
        if let Err(error) =
            collector.terminal(&output, oracle_owner.accounting.snapshot(), audit_deadline)
        {
            collector.complete = false;
            journal.value["metrics_terminal_error"] = json!(error);
        }
        journal.value["phase_metrics"] = collector.summary();
    }
    if let Some(r) = &mut resources
        && let Err(error) = r.finish().await
    {
        cleanup.push(json!({"error":error}));
    }
    if let Some(r) = &resources {
        journal.value["controller_resources"] = r.snapshot();
    }
    journal.value["last_work_phase"] = journal.value["phase"].clone();
    journal.value["phase"] = json!("terminal");
    journal.progress.observe(&journal.value);
    if let Some(r) = &resources {
        project_resource_progress(&mut fleet, r, &mut journal.progress, true);
    }
    journal.value["aggregate_owned_resources"] = json!({
            "sum_individual_peak_rss_bytes":worker_receipts.iter().filter_map(|w|w["resources"]["peak_rss_bytes"].as_u64()).sum::<u64>()+resources.as_ref().and_then(|r|r.snapshot()["peak_rss_bytes"].as_u64()).unwrap_or(0),
            "scope":"sum of separately sampled process peaks; not a simultaneous aggregate peak; no host or Docker attribution"}
    );
    journal.value["oracle_accounting"] = oracle_owner.accounting.snapshot();
    journal.value["initialization_accounting"] = initializer_owner.accounting.snapshot();
    journal.value["workload_complete"] = json!(result.is_ok());
    if let Some(collector) = &mut phase_metrics {
        collector.complete &= fleet.startup_accounting_complete() && journal.progress.complete();
        journal.value["phase_metrics"] = collector.summary();
    }
    journal.value["metrics_complete"] = json!(
        phase_metrics
            .as_ref()
            .is_some_and(metrics::Collector::qualified)
            && oracle_owner.accounting.complete()
            && oracle_owner.settled()
            && initializer_owner.accounting.complete()
    );
    journal.value["cleanup_errors"] = json!(cleanup);
    let oracle_complete = oracle_owner.settled() && oracle_passes_complete(&journal.value);
    journal.value["fresh_oracle_settled"] = json!(oracle_owner.settled());
    journal.value["fresh_oracle_complete"] = json!(oracle_complete);
    journal.value["workload_complete"] = json!(result.is_ok() && oracle_complete);
    let workload_success = result.is_ok() && cleanup.is_empty() && oracle_complete;
    let metrics_required = mount_rs_core::diagnostics::profile::enabled();
    let success =
        workload_success && (!metrics_required || journal.value["metrics_complete"] == true);
    journal.value["metrics_required_for_outcome"] = json!(metrics_required);
    journal.value["outcome"] = json!(if success { "success" } else { "incomplete" });
    journal.value["phase"] = json!("terminal");
    journal.value["observer_accounting"] = metrics::observer().snapshot();
    journal.value["observer_accounting_scope"] = json!(
        "includes completed final metric publication and resource sampler; excludes this final journal encode/write; overlapping observer wall is not exclusive CPU"
    );
    journal.flush()?;
    journal
        .progress
        .finish(success, journal.value["metrics_complete"] == true);
    if success && metrics_required && !journal.progress.complete() {
        journal.value["metrics_complete"] = json!(false);
        journal.value["outcome"] = json!("incomplete");
        journal.value["error"] = json!("controller diagnostic final publication incomplete");
        // Preserve a failed qualification even if a second private receipt write also fails.
        let retained = write_json(&output.join("terminal.json"), &journal.value);
        return Err(if retained.is_ok() {
            "controller diagnostic final publication incomplete"
        } else {
            "controller diagnostic final publication and retained receipt incomplete"
        }
        .into());
    }
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

#[cfg(test)]
mod population_budget_tests {
    use super::*;

    fn journal_at(now: Instant, remaining: u64) -> (tempfile::TempDir, Journal) {
        let directory = tempfile::tempdir().unwrap();
        let enclosing_deadline = now + Duration::from_secs(remaining);
        let journal = Journal {
            value: json!({
                "phase":"signed_connections","created_unix_ms":1000,
                "phase_started_unix_ms":2000
            }),
            output: directory.path().to_owned(),
            counts: vec![],
            enclosing_deadline,
            phase_deadline: enclosing_deadline,
            progress: progress::Progress::disabled(),
            oracle_progress: None,
        };
        (directory, journal)
    }

    fn assert_published_phase(directory: &tempfile::TempDir, journal: &Journal, name: &str) {
        let published = read_json(&directory.path().join("terminal.json")).unwrap();
        assert_eq!(published, journal.value);
        assert_eq!(published["phase"], name);
        assert_eq!(published["phase_started_unix_ms"], 5000);
        assert_eq!(
            published["phase_history"],
            json!([
                {"phase":"signed_connections","elapsed_ms":3000,"ended_unix_ms":5000}
            ])
        );
    }

    #[test]
    fn production_population_budget_extends_actual_journal_beyond_ordinary_600() {
        let now = Instant::now();
        let (directory, mut journal) = journal_at(now, WORK_SECONDS);
        let enclosing = journal.enclosing_deadline;
        journal
            .phase_with_budget_at("online_payload", 900, now, 5000)
            .unwrap();
        assert_published_phase(&directory, &journal, "online_payload");
        assert_eq!(journal.enclosing_deadline, enclosing);
        assert_eq!(
            journal.phase_deadline,
            now + Duration::from_secs(900),
            "population allowance must change the actual inherited deadline"
        );
    }

    #[test]
    fn production_population_budget_shrinks_actual_journal_to_60() {
        let now = Instant::now();
        let (directory, mut journal) = journal_at(now, WORK_SECONDS);
        journal
            .phase_with_budget_at("online_namespace", 60, now, 5000)
            .unwrap();
        assert_published_phase(&directory, &journal, "online_namespace");
        assert_eq!(
            journal.phase_deadline,
            now + Duration::from_secs(60),
            "population allowance must also enforce a smaller configured cap"
        );
    }

    #[test]
    fn production_population_budget_clips_actual_journal_to_120_remaining() {
        let now = Instant::now();
        let (directory, mut journal) = journal_at(now, 120);
        let enclosing = journal.enclosing_deadline;
        journal
            .phase_with_budget_at("online_payload", 900, now, 5000)
            .unwrap();
        assert_published_phase(&directory, &journal, "online_payload");
        assert_eq!(journal.phase_deadline, enclosing);
        assert_eq!(journal.enclosing_deadline, enclosing);
    }

    #[test]
    fn production_population_budget_preserves_ordinary_phase_600() {
        let now = Instant::now();
        let (directory, mut journal) = journal_at(now, WORK_SECONDS);
        journal.phase_at("initial_fresh_oracle", now, 5000).unwrap();
        assert_published_phase(&directory, &journal, "initial_fresh_oracle");
        assert_eq!(
            journal.phase_deadline,
            now + Duration::from_secs(PHASE_SECONDS)
        );
    }

    #[test]
    fn production_population_budget_refuses_expired_predecessor_without_mutating_history() {
        let now = Instant::now();
        let (directory, mut journal) = journal_at(now, WORK_SECONDS);
        journal.phase_deadline = now;
        journal.flush().unwrap();
        let previous_value = journal.value.clone();
        let previous_receipt = std::fs::read(directory.path().join("terminal.json")).unwrap();
        let enclosing = journal.enclosing_deadline;
        let error = journal
            .phase_with_budget_at("online_payload", 900, now, 5000)
            .unwrap_err();
        assert_eq!(error, "previous phase exhausted its inherited deadline");
        assert_eq!(journal.phase_deadline, now);
        assert_eq!(journal.enclosing_deadline, enclosing);
        assert_eq!(journal.value, previous_value);
        assert_eq!(
            std::fs::read(directory.path().join("terminal.json")).unwrap(),
            previous_receipt
        );
    }
}
