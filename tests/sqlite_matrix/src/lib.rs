use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::chunking::ChunkerConfig;
use mount_rs_core::storage::{BlockStore, MetadataStore, Namespace, NodeData, NodeMetadata};
use mount_rs_core::types::{S_IFDIR, Stats};
use mount_rs_core::{ErrorCode, Loopback};
use mount_rs_fault_injection::{
    COMMIT_UNKNOWN_MESSAGE, FaultAction, FaultBlockStore, FaultBoundary, FaultInjector,
    FaultMetadataStore, FaultOccurrence, FaultOperation, FaultOutcome, FaultPhase, FaultPlan,
    FaultRule,
};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use rusqlite::Connection;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fmt::Display;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;
use tempfile::TempDir;

pub const FAULT_SEED: u64 = 0x5a17_2026;
pub const CONCURRENCY_ROUNDS: usize = 8;

const BASELINE: &[u8] = b"sqlite-matrix-baseline-v1";
const UPDATED: &[u8] = b"sqlite-matrix-updatedx-v1";

type SqliteFs = ChunkedFs<SqliteMetadataStore, SqliteBlockStore>;
type FaultedFs =
    ChunkedFs<FaultMetadataStore<SqliteMetadataStore>, FaultBlockStore<SqliteBlockStore>>;

#[derive(Clone, Copy)]
enum JournalMode {
    Delete,
    Wal,
}

impl JournalMode {
    const fn label(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Wal => "wal",
        }
    }

    const fn pragma(self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Wal => "WAL",
        }
    }
}

#[derive(Clone, Copy)]
enum FaultCase {
    BlockPutBefore,
    PublishAfterUnknown,
}

impl FaultCase {
    const fn label(self) -> &'static str {
        match self {
            Self::BlockPutBefore => "block_put_before",
            Self::PublishAfterUnknown => "metadata_publish_after_unknown",
        }
    }
}

#[derive(Debug, Serialize)]
pub struct CaseReport {
    pub kind: &'static str,
    pub case: String,
    pub status: String,
    pub classification: String,
    pub journal: String,
    pub checks: BTreeMap<String, bool>,
    pub observed: BTreeMap<String, String>,
    pub fault_events: Vec<FaultEventReport>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FaultEventReport {
    pub sequence: u64,
    pub seed: u64,
    pub boundary: String,
    pub operation: String,
    pub phase: String,
    pub occurrence: u64,
    pub rule_index: usize,
    pub action: String,
    pub outcome: String,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub kind: &'static str,
    pub status: String,
    pub total: usize,
    pub passed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub fault_seed: u64,
    pub concurrency_rounds: usize,
    pub scope: &'static str,
    pub gaps: Vec<&'static str>,
}

struct Paths {
    _directory: TempDir,
    metadata: PathBuf,
    blocks: PathBuf,
}

struct DbInfo {
    sqlite_version: String,
    journal: String,
    synchronous: i64,
    integrity: String,
}

pub fn run_matrix() -> Vec<CaseReport> {
    let mut reports = Vec::with_capacity(8);
    for mode in [JournalMode::Delete, JournalMode::Wal] {
        reports.push(execute_case(
            format!("{}_persistence_reopen", mode.label()),
            mode,
            "provider_file_backed_persistence",
            || run_persistence(mode),
        ));
        reports.push(execute_case(
            format!("{}_concurrent_reader_writer", mode.label()),
            mode,
            "provider_single_writer_concurrency",
            || run_concurrency(mode),
        ));
        reports.push(execute_case(
            format!("{}_fault_block_put_before", mode.label()),
            mode,
            "provider_fault_injection",
            || run_fault(mode, FaultCase::BlockPutBefore),
        ));
        reports.push(execute_case(
            format!("{}_fault_publish_after_unknown", mode.label()),
            mode,
            "provider_fault_injection",
            || run_fault(mode, FaultCase::PublishAfterUnknown),
        ));
    }
    reports
}

pub fn summarize(reports: &[CaseReport]) -> Summary {
    let passed = reports
        .iter()
        .filter(|report| report.status == "pass")
        .count();
    let failed = reports
        .iter()
        .filter(|report| report.status == "fail")
        .count();
    let skipped = reports
        .iter()
        .filter(|report| report.status == "skip")
        .count();
    Summary {
        kind: "summary",
        status: if failed == 0 { "pass" } else { "fail" }.to_owned(),
        total: reports.len(),
        passed,
        failed,
        skipped,
        fault_seed: FAULT_SEED,
        concurrency_rounds: CONCURRENCY_ROUNDS,
        scope: "direct mount-rs SQLite metadata/block providers",
        gaps: vec![
            "native FUSE/NFS mount hosting is outside this direct-provider packet",
            "mount-rs SQLite VFS behavior is outside this direct-provider packet",
            "process kill, power loss, and distributed multi-host locking are not simulated",
            "the concurrency cells exercise the supported single-writer lease, not multi-writer success",
        ],
    }
}

fn execute_case<F>(name: String, mode: JournalMode, classification: &str, run: F) -> CaseReport
where
    F: FnOnce() -> Result<CaseReport, String>,
{
    match run() {
        Ok(mut report) => {
            report.status = "pass".to_owned();
            report
        }
        Err(error) => {
            let mut report = new_report(&name, mode, classification);
            report.status = "fail".to_owned();
            report.error = Some(error);
            report
        }
    }
}

fn new_report(name: &str, mode: JournalMode, classification: &str) -> CaseReport {
    CaseReport {
        kind: "case",
        case: name.to_owned(),
        status: "fail".to_owned(),
        classification: classification.to_owned(),
        journal: mode.label().to_owned(),
        checks: BTreeMap::new(),
        observed: BTreeMap::new(),
        fault_events: Vec::new(),
        error: None,
    }
}

fn record_check(report: &mut CaseReport, name: &str, value: bool) -> Result<(), String> {
    report.checks.insert(name.to_owned(), value);
    if value {
        Ok(())
    } else {
        Err(format!("check failed: {name}"))
    }
}

fn display_error(error: impl Display) -> String {
    error.to_string()
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = std::task::Waker::noop();
    let mut context = Context::from_waker(waker);
    let mut future = Box::pin(future);
    loop {
        match Future::poll(future.as_mut(), &mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => thread::yield_now(),
        }
    }
}

fn new_paths(mode: JournalMode) -> Result<Paths, String> {
    let directory = TempDir::new().map_err(display_error)?;
    let metadata = directory.path().join("metadata.sqlite");
    let blocks = directory.path().join("blocks.sqlite");
    configure_database(&metadata, mode)?;
    configure_database(&blocks, mode)?;
    Ok(Paths {
        _directory: directory,
        metadata,
        blocks,
    })
}

fn configure_database(path: &Path, mode: JournalMode) -> Result<DbInfo, String> {
    let connection = Connection::open(path).map_err(display_error)?;
    let journal_sql = format!("PRAGMA journal_mode={};", mode.pragma());
    let actual_journal: String = connection
        .query_row(&journal_sql, [], |row| row.get(0))
        .map_err(display_error)?;
    connection
        .execute_batch("PRAGMA synchronous=FULL;")
        .map_err(display_error)?;
    let info = read_db_info_from_connection(&connection)?;
    if actual_journal.eq_ignore_ascii_case(mode.pragma()) {
        Ok(info)
    } else {
        Err(format!(
            "requested SQLite journal {} but observed {}",
            mode.pragma(),
            actual_journal
        ))
    }
}

fn inspect_database(path: &Path) -> Result<DbInfo, String> {
    let connection = Connection::open(path).map_err(display_error)?;
    read_db_info_from_connection(&connection)
}

fn read_db_info_from_connection(connection: &Connection) -> Result<DbInfo, String> {
    let sqlite_version: String = connection
        .query_row("SELECT sqlite_version()", [], |row| row.get(0))
        .map_err(display_error)?;
    let journal: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(display_error)?;
    let synchronous: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .map_err(display_error)?;
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(display_error)?;
    Ok(DbInfo {
        sqlite_version,
        journal: journal.to_ascii_uppercase(),
        synchronous,
        integrity,
    })
}

fn options(owner: &str) -> Result<ChunkedOptions, String> {
    ChunkedOptions::fixed(owner, 64)
        .map(|options| options.with_lease_ttl(Duration::from_secs(30)))
        .map_err(display_error)
}

async fn open_plain(paths: &Paths, owner: &str) -> Result<SqliteFs, String> {
    let metadata = SqliteMetadataStore::open(&paths.metadata).map_err(display_error)?;
    let blocks = SqliteBlockStore::open(&paths.blocks).map_err(display_error)?;
    ChunkedFs::open(metadata, blocks, options(owner)?)
        .await
        .map_err(display_error)
}

async fn open_faulted(
    paths: &Paths,
    plan: FaultPlan,
    owner: &str,
) -> Result<(FaultedFs, FaultInjector), String> {
    let injector = FaultInjector::new(plan).map_err(display_error)?;
    let metadata = FaultMetadataStore::new(
        SqliteMetadataStore::open(&paths.metadata).map_err(display_error)?,
        injector.clone(),
    );
    let blocks = FaultBlockStore::new(
        SqliteBlockStore::open(&paths.blocks).map_err(display_error)?,
        injector.clone(),
    );
    let filesystem = ChunkedFs::open(metadata, blocks, options(owner)?)
        .await
        .map_err(display_error)?;
    Ok((filesystem, injector))
}

fn test_namespace() -> Namespace {
    let mut nodes = BTreeMap::new();
    nodes.insert(
        1,
        NodeMetadata {
            stats: Stats {
                dev: 1,
                ino: 1,
                mode: S_IFDIR | 0o755,
                nlink: 2,
                uid: 0,
                gid: 0,
                rdev: 0,
                size: 0,
                blksize: 4096,
                blocks: 0,
                atime_ms: 1,
                mtime_ms: 1,
                ctime_ms: 1,
                birthtime_ms: 1,
            },
            data: NodeData::Directory {
                entries: Vec::new(),
            },
        },
    );
    Namespace {
        format_version: 1,
        root: 1,
        next_inode: 2,
        default_uid: 0,
        default_gid: 0,
        umask: 0,
        default_chunker: ChunkerConfig {
            algorithm: "fixed-size".to_owned(),
            version: 1,
            parameters: BTreeMap::from([(String::from("chunk_size"), 64)]),
        },
        nodes,
    }
}

fn run_persistence(mode: JournalMode) -> Result<CaseReport, String> {
    let mut report = new_report(
        &format!("{}_persistence_reopen", mode.label()),
        mode,
        "provider_file_backed_persistence",
    );
    let paths = new_paths(mode)?;

    let filesystem = block_on(open_plain(&paths, "matrix-persistence-first"))?;
    let metadata_durable = filesystem.metadata_store().durable();
    let blocks_durable = filesystem.block_store().durable();
    let loopback = Loopback::new(filesystem.clone());
    block_on(loopback.write_file("/payload", BASELINE)).map_err(display_error)?;
    drop(loopback);
    block_on(filesystem.shutdown()).map_err(display_error)?;
    drop(filesystem);

    let reopened = block_on(open_plain(&paths, "matrix-persistence-reopen"))?;
    let reopened_loopback = Loopback::new(reopened.clone());
    let payload = block_on(reopened_loopback.read_file("/payload")).map_err(display_error)?;
    drop(reopened_loopback);
    block_on(reopened.shutdown()).map_err(display_error)?;
    drop(reopened);

    let metadata = inspect_database(&paths.metadata)?;
    let blocks = inspect_database(&paths.blocks)?;
    report.observed.insert(
        "sqlite_version_metadata".to_owned(),
        metadata.sqlite_version.clone(),
    );
    report.observed.insert(
        "sqlite_version_blocks".to_owned(),
        blocks.sqlite_version.clone(),
    );
    report
        .observed
        .insert("metadata_journal".to_owned(), metadata.journal.clone());
    report
        .observed
        .insert("blocks_journal".to_owned(), blocks.journal.clone());
    report.observed.insert(
        "metadata_synchronous".to_owned(),
        metadata.synchronous.to_string(),
    );
    report.observed.insert(
        "blocks_synchronous".to_owned(),
        blocks.synchronous.to_string(),
    );
    report.observed.insert(
        "reopened_payload_bytes".to_owned(),
        payload.len().to_string(),
    );

    record_check(&mut report, "metadata_durable", metadata_durable)?;
    record_check(&mut report, "blocks_durable", blocks_durable)?;
    record_check(
        &mut report,
        "payload_survives_reopen",
        payload.as_slice() == BASELINE,
    )?;
    record_check(
        &mut report,
        "metadata_journal_matches_request",
        metadata.journal == mode.pragma(),
    )?;
    record_check(
        &mut report,
        "blocks_journal_matches_request",
        blocks.journal == mode.pragma(),
    )?;
    record_check(
        &mut report,
        "metadata_full_synchronous",
        metadata.synchronous == 2,
    )?;
    record_check(
        &mut report,
        "blocks_full_synchronous",
        blocks.synchronous == 2,
    )?;
    record_check(
        &mut report,
        "metadata_integrity_ok",
        metadata.integrity == "ok",
    )?;
    record_check(&mut report, "blocks_integrity_ok", blocks.integrity == "ok")?;
    Ok(report)
}

fn run_concurrency(mode: JournalMode) -> Result<CaseReport, String> {
    let mut report = new_report(
        &format!("{}_concurrent_reader_writer", mode.label()),
        mode,
        "provider_single_writer_concurrency",
    );
    let paths = new_paths(mode)?;
    let writer = SqliteMetadataStore::open(&paths.metadata).map_err(display_error)?;
    let reader = SqliteMetadataStore::open(&paths.metadata).map_err(display_error)?;
    let challenger = SqliteMetadataStore::open(&paths.metadata).map_err(display_error)?;
    let initial = block_on(writer.load()).map_err(display_error)?;
    initial.validate().map_err(display_error)?;
    let initial_revision = initial.revision;
    let lease =
        block_on(writer.acquire_writer("matrix-concurrent-writer", Duration::from_secs(30)))
            .map_err(display_error)?;

    let competing_writer_error = match block_on(
        challenger.acquire_writer("matrix-competing-writer", Duration::from_secs(30)),
    ) {
        Ok(competing_lease) => {
            let _ = block_on(challenger.release_writer(&competing_lease));
            return Err("a competing writer acquired the active writer lease".to_owned());
        }
        Err(error) => error,
    };

    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = Arc::clone(&barrier);
    let reader_barrier = Arc::clone(&barrier);
    let (writer_result, reader_result) = thread::scope(|scope| {
        let writer_thread = scope.spawn(move || -> Result<u64, String> {
            let mut expected_revision = initial_revision;
            let mut first_error = None;
            for _ in 0..CONCURRENCY_ROUNDS {
                writer_barrier.wait();
                if first_error.is_none() {
                    match block_on(writer.publish(expected_revision, &lease, test_namespace())) {
                        Ok(next_revision) => expected_revision = next_revision,
                        Err(error) => first_error = Some(display_error(error)),
                    }
                }
                writer_barrier.wait();
            }
            let release_result = block_on(writer.release_writer(&lease));
            if let Some(error) = first_error {
                Err(error)
            } else {
                release_result
                    .map(|()| expected_revision)
                    .map_err(display_error)
            }
        });

        let reader_thread = scope.spawn(move || -> Result<Vec<u64>, String> {
            let mut revisions = Vec::with_capacity(CONCURRENCY_ROUNDS);
            let mut first_error = None;
            for _ in 0..CONCURRENCY_ROUNDS {
                reader_barrier.wait();
                if first_error.is_none() {
                    match block_on(reader.load()) {
                        Ok(loaded) => {
                            if let Err(error) = loaded.validate() {
                                first_error = Some(display_error(error));
                            } else {
                                revisions.push(loaded.revision);
                            }
                        }
                        Err(error) => first_error = Some(display_error(error)),
                    }
                }
                reader_barrier.wait();
            }
            first_error.map_or(Ok(revisions), Err)
        });

        (writer_thread.join(), reader_thread.join())
    });

    let writer_final = writer_result.map_err(|_| "SQLite writer thread panicked".to_owned())??;
    let reader_revisions =
        reader_result.map_err(|_| "SQLite reader thread panicked".to_owned())??;
    let metadata = inspect_database(&paths.metadata)?;

    report
        .observed
        .insert("sqlite_version".to_owned(), metadata.sqlite_version);
    report
        .observed
        .insert("journal".to_owned(), metadata.journal.clone());
    report
        .observed
        .insert("synchronous".to_owned(), metadata.synchronous.to_string());
    report.observed.insert(
        "reader_rounds".to_owned(),
        reader_revisions.len().to_string(),
    );
    report
        .observed
        .insert("writer_final_revision".to_owned(), writer_final.to_string());
    report.observed.insert(
        "competing_writer_error".to_owned(),
        format!("{:?}", competing_writer_error.code),
    );

    let nondecreasing = reader_revisions
        .windows(2)
        .all(|window| window[0] <= window[1]);
    let revisions_in_range = reader_revisions
        .iter()
        .all(|revision| *revision >= initial_revision && *revision <= writer_final);
    record_check(
        &mut report,
        "competing_writer_rejected_eagain",
        competing_writer_error.code == ErrorCode::Eagain,
    )?;
    record_check(
        &mut report,
        "reader_completed_all_rounds",
        reader_revisions.len() == CONCURRENCY_ROUNDS,
    )?;
    record_check(&mut report, "reader_revisions_nondecreasing", nondecreasing)?;
    record_check(&mut report, "reader_revisions_in_range", revisions_in_range)?;
    record_check(
        &mut report,
        "writer_completed_all_rounds",
        writer_final == initial_revision + CONCURRENCY_ROUNDS as u64,
    )?;
    record_check(
        &mut report,
        "journal_matches_request",
        metadata.journal == mode.pragma(),
    )?;
    record_check(&mut report, "full_synchronous", metadata.synchronous == 2)?;
    record_check(&mut report, "integrity_ok", metadata.integrity == "ok")?;
    Ok(report)
}

fn run_fault(mode: JournalMode, fault_case: FaultCase) -> Result<CaseReport, String> {
    let mut report = new_report(
        &format!("{}_fault_{}", mode.label(), fault_case.label()),
        mode,
        "provider_fault_injection",
    );
    let paths = new_paths(mode)?;

    let baseline_fs = block_on(open_plain(&paths, "matrix-fault-baseline"))?;
    let baseline_loopback = Loopback::new(baseline_fs.clone());
    block_on(baseline_loopback.write_file("/payload", BASELINE)).map_err(display_error)?;
    drop(baseline_loopback);
    block_on(baseline_fs.shutdown()).map_err(display_error)?;
    drop(baseline_fs);

    let plan = fault_plan(fault_case)?;
    let (faulted, injector) = block_on(open_faulted(
        &paths,
        plan,
        &format!("matrix-fault-{}", fault_case.label()),
    ))?;
    let faulted_loopback = Loopback::new(faulted.clone());
    let handle = block_on(faulted_loopback.open("/payload", "r+", 0)).map_err(display_error)?;
    let write_error = match block_on(handle.write(UPDATED, Some(0))) {
        Ok(count) => {
            return Err(format!(
                "faulted write unexpectedly succeeded with {count} bytes"
            ));
        }
        Err(error) => error,
    };
    let _ = block_on(handle.close());
    drop(faulted_loopback);
    block_on(faulted.shutdown()).map_err(display_error)?;
    drop(faulted);

    let trace = injector.trace();
    report.fault_events = trace
        .events
        .iter()
        .map(|event| FaultEventReport {
            sequence: event.sequence,
            seed: event.seed,
            boundary: event.boundary.to_string(),
            operation: event.operation.to_string(),
            phase: event.phase.to_string(),
            occurrence: event.occurrence,
            rule_index: event.rule_index,
            action: action_label(event.action),
            outcome: outcome_label(event.outcome),
        })
        .collect();

    let reopened = block_on(open_plain(&paths, "matrix-fault-reopen"))?;
    let reopened_loopback = Loopback::new(reopened.clone());
    let payload = block_on(reopened_loopback.read_file("/payload")).map_err(display_error)?;
    drop(reopened_loopback);
    block_on(reopened.shutdown()).map_err(display_error)?;
    drop(reopened);

    let metadata = inspect_database(&paths.metadata)?;
    let blocks = inspect_database(&paths.blocks)?;
    let payload_label = if payload.as_slice() == BASELINE {
        "baseline"
    } else if payload.as_slice() == UPDATED {
        "updated"
    } else {
        "unexpected"
    };
    report
        .observed
        .insert("fault_case".to_owned(), fault_case.label().to_owned());
    report.observed.insert(
        "write_error_code".to_owned(),
        format!("{:?}", write_error.code),
    );
    report
        .observed
        .insert("reopen_payload".to_owned(), payload_label.to_owned());
    report
        .observed
        .insert("metadata_journal".to_owned(), metadata.journal.clone());
    report
        .observed
        .insert("blocks_journal".to_owned(), blocks.journal.clone());
    report.observed.insert(
        "metadata_synchronous".to_owned(),
        metadata.synchronous.to_string(),
    );
    report.observed.insert(
        "blocks_synchronous".to_owned(),
        blocks.synchronous.to_string(),
    );

    record_check(
        &mut report,
        "one_fault_event_recorded",
        trace.events.len() == 1,
    )?;
    record_check(
        &mut report,
        "fault_trace_seed_matches",
        trace.seed == FAULT_SEED,
    )?;
    match fault_case {
        FaultCase::BlockPutBefore => {
            record_check(
                &mut report,
                "error_is_enospc",
                write_error.code == ErrorCode::Enospc,
            )?;
            record_check(
                &mut report,
                "outcome_is_injected_error",
                trace.events.first().is_some_and(|event| {
                    matches!(
                        event.outcome,
                        FaultOutcome::InjectedError {
                            code: ErrorCode::Enospc
                        }
                    )
                }),
            )?;
            record_check(
                &mut report,
                "reopen_retains_baseline",
                payload_label == "baseline",
            )?;
        }
        FaultCase::PublishAfterUnknown => {
            record_check(
                &mut report,
                "error_is_commit_unknown",
                write_error.code == ErrorCode::Eio
                    && write_error.to_string().contains(COMMIT_UNKNOWN_MESSAGE),
            )?;
            record_check(
                &mut report,
                "outcome_is_commit_unknown",
                trace
                    .events
                    .first()
                    .is_some_and(|event| matches!(event.outcome, FaultOutcome::CommitUnknown)),
            )?;
            record_check(
                &mut report,
                "reopen_is_whole_allowed_state",
                payload_label == "baseline" || payload_label == "updated",
            )?;
        }
    }
    record_check(
        &mut report,
        "metadata_journal_matches_request",
        metadata.journal == mode.pragma(),
    )?;
    record_check(
        &mut report,
        "blocks_journal_matches_request",
        blocks.journal == mode.pragma(),
    )?;
    record_check(
        &mut report,
        "metadata_full_synchronous",
        metadata.synchronous == 2,
    )?;
    record_check(
        &mut report,
        "blocks_full_synchronous",
        blocks.synchronous == 2,
    )?;
    record_check(
        &mut report,
        "metadata_integrity_ok",
        metadata.integrity == "ok",
    )?;
    record_check(&mut report, "blocks_integrity_ok", blocks.integrity == "ok")?;
    Ok(report)
}

fn fault_plan(fault_case: FaultCase) -> Result<FaultPlan, String> {
    let rule = match fault_case {
        FaultCase::BlockPutBefore => FaultRule::new(
            FaultBoundary::Blocks,
            FaultOperation::Put,
            FaultPhase::Before,
            FaultOccurrence::Once,
            FaultAction::Error(ErrorCode::Enospc),
        ),
        FaultCase::PublishAfterUnknown => FaultRule::new(
            FaultBoundary::Metadata,
            FaultOperation::Publish,
            FaultPhase::After,
            FaultOccurrence::Once,
            FaultAction::LostAcknowledgment,
        ),
    };
    FaultPlan::new(FAULT_SEED, 1, vec![rule]).map_err(display_error)
}

fn action_label(action: FaultAction) -> String {
    match action {
        FaultAction::Error(code) => format!("error:{code:?}"),
        FaultAction::LeaseFailure => "lease_failure".to_owned(),
        FaultAction::CasConflict => "cas_conflict".to_owned(),
        FaultAction::LostAcknowledgment => "lost_acknowledgment".to_owned(),
        FaultAction::DelayMs(milliseconds) => format!("delay_ms:{milliseconds}"),
    }
}

fn outcome_label(outcome: FaultOutcome) -> String {
    match outcome {
        FaultOutcome::Pending => "pending".to_owned(),
        FaultOutcome::Cancelled => "cancelled".to_owned(),
        FaultOutcome::InjectedError { code } => format!("injected_error:{code:?}"),
        FaultOutcome::LeaseFailure => "lease_failure".to_owned(),
        FaultOutcome::CasConflict => "cas_conflict".to_owned(),
        FaultOutcome::CommitUnknown => "commit_unknown".to_owned(),
        FaultOutcome::Delayed => "delayed".to_owned(),
        FaultOutcome::DelegateError { code } => format!("delegate_error:{code:?}"),
        FaultOutcome::InvalidPlan => "invalid_plan".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_is_bounded_and_machine_readable() {
        let reports = run_matrix();
        assert_eq!(reports.len(), 8);
        assert!(reports.iter().all(|report| report.status == "pass"));
        let summary = summarize(&reports);
        assert_eq!(summary.total, 8);
        assert_eq!(summary.passed, 8);
        assert_eq!(summary.failed, 0);
        assert_eq!(summary.skipped, 0);
        let json = serde_json::to_string(&reports[0]).expect("case report serializes");
        assert!(json.contains("\"kind\":\"case\""));
        assert!(json.contains("\"checks\""));
    }
}
