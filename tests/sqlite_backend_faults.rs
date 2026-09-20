//! Fault-injection acceptance for the split SQLite metadata/block backend.
//!
//! The ordinary test exercises the real SQLite providers without a kernel
//! mount. The ignored test runs the same providers behind a Linux FUSE mount
//! and drives a real Python SQLite process. Faults are armed only after the
//! initial schema and baseline row have committed.
//!
//! A block put, block flush, or metadata publish fault happens before the
//! namespace publication boundary, so reopening must show the baseline row.
//! A metadata flush fault happens after `publish` has committed the metadata
//! transaction; the caller must still receive an error, but the post-error
//! commit is uncertain and reopening may show either valid baseline or
//! attempted data. No case treats an error as a successful commit.

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{ErrorCode, FsError, Loopback, Result as FsResult};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::sync::{
    Arc,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum FaultPoint {
    BlockPut = 1,
    BlockFlush = 2,
    MetadataPublish = 3,
    MetadataFlush = 4,
}

impl FaultPoint {
    const ALL: [Self; 4] = [
        Self::BlockPut,
        Self::BlockFlush,
        Self::MetadataPublish,
        Self::MetadataFlush,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::BlockPut => "block-put",
            Self::BlockFlush => "block-flush",
            Self::MetadataPublish => "metadata-publish",
            Self::MetadataFlush => "metadata-flush",
        }
    }

    const fn code(self) -> u8 {
        self as u8
    }

    const fn is_uncertain_commit(self) -> bool {
        matches!(self, Self::MetadataFlush)
    }
}

#[derive(Clone)]
struct FaultPlan {
    active: Arc<AtomicU8>,
    hits: Arc<AtomicUsize>,
}

impl FaultPlan {
    fn new() -> Self {
        Self {
            active: Arc::new(AtomicU8::new(0)),
            hits: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn arm(&self, point: FaultPoint) {
        self.hits.store(0, Ordering::SeqCst);
        self.active.store(point.code(), Ordering::SeqCst);
    }

    fn clear(&self) {
        self.active.store(0, Ordering::SeqCst);
    }

    fn should_fail(&self, point: FaultPoint) -> bool {
        if self.active.load(Ordering::SeqCst) == point.code() {
            self.hits.fetch_add(1, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    fn hit_count(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn injected(point: FaultPoint) -> FsError {
    FsError::new(ErrorCode::Eio)
        .with_syscall(point.label())
        .with_message(format!("injected {} failure", point.label()))
}

#[derive(Clone)]
struct FaultMetadataStore {
    inner: SqliteMetadataStore,
    faults: FaultPlan,
}

impl FaultMetadataStore {
    fn open(path: impl AsRef<std::path::Path>, faults: FaultPlan) -> FsResult<Self> {
        Ok(Self {
            inner: SqliteMetadataStore::open(path)?,
            faults,
        })
    }
}

#[async_trait]
impl MetadataStore for FaultMetadataStore {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn load(&self) -> FsResult<LoadedMetadata> {
        self.inner.load().await
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> FsResult<WriterLease> {
        self.inner.acquire_writer(owner, ttl).await
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> FsResult<WriterLease> {
        self.inner.renew_writer(lease, ttl).await
    }

    async fn release_writer(&self, lease: &WriterLease) -> FsResult<()> {
        self.inner.release_writer(lease).await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> FsResult<u64> {
        if self.faults.should_fail(FaultPoint::MetadataPublish) {
            return Err(injected(FaultPoint::MetadataPublish));
        }
        self.inner
            .publish(expected_revision, lease, namespace)
            .await
    }

    async fn flush(&self) -> FsResult<()> {
        if self.faults.should_fail(FaultPoint::MetadataFlush) {
            return Err(injected(FaultPoint::MetadataFlush));
        }
        self.inner.flush().await
    }
}

#[derive(Clone)]
struct FaultBlockStore {
    inner: SqliteBlockStore,
    faults: FaultPlan,
}

impl FaultBlockStore {
    fn open(path: impl AsRef<std::path::Path>, faults: FaultPlan) -> FsResult<Self> {
        Ok(Self {
            inner: SqliteBlockStore::open(path)?,
            faults,
        })
    }
}

#[async_trait]
impl BlockStore for FaultBlockStore {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> FsResult<BlockId> {
        if self.faults.should_fail(FaultPoint::BlockPut) {
            return Err(injected(FaultPoint::BlockPut));
        }
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> FsResult<Vec<u8>> {
        self.inner.get(id).await
    }

    async fn flush(&self) -> FsResult<()> {
        if self.faults.should_fail(FaultPoint::BlockFlush) {
            return Err(injected(FaultPoint::BlockFlush));
        }
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> FsResult<()> {
        self.inner.delete(id).await
    }
}

type FaultedFs = ChunkedFs<FaultMetadataStore, FaultBlockStore>;
type PlainFs = ChunkedFs<SqliteMetadataStore, SqliteBlockStore>;

const BASELINE_DATA: &[u8] = b"committed-baseline";
const ATTEMPTED_DATA: &[u8] = b"faulted-transaction";

fn options(owner: impl Into<String>) -> ChunkedOptions {
    ChunkedOptions::fixed(owner, 4096)
        .expect("fixed chunk configuration")
        .with_lease_ttl(Duration::from_secs(10))
}

async fn open_faulted(
    metadata_path: &std::path::Path,
    blocks_path: &std::path::Path,
    faults: FaultPlan,
    owner: &str,
) -> FsResult<FaultedFs> {
    ChunkedFs::open(
        FaultMetadataStore::open(metadata_path, faults.clone())?,
        FaultBlockStore::open(blocks_path, faults)?,
        options(owner),
    )
    .await
}

async fn open_plain(
    metadata_path: &std::path::Path,
    blocks_path: &std::path::Path,
    owner: &str,
) -> FsResult<PlainFs> {
    ChunkedFs::open(
        SqliteMetadataStore::open(metadata_path)?,
        SqliteBlockStore::open(blocks_path)?,
        options(owner),
    )
    .await
}

async fn direct_fault_case(point: FaultPoint) {
    let directory = tempfile::tempdir().expect("create ordinary fault-test directory");
    let metadata_path = directory.path().join("metadata.sqlite");
    let blocks_path = directory.path().join("blocks.sqlite");
    let faults = FaultPlan::new();
    let filesystem = open_faulted(
        &metadata_path,
        &blocks_path,
        faults.clone(),
        &format!("ordinary-{}", point.label()),
    )
    .await
    .expect("open faulted SQLite stores");
    let loopback = Loopback::new(filesystem.clone());
    loopback
        .write_file("/records", BASELINE_DATA)
        .await
        .expect("commit baseline file before arming fault");

    faults.arm(point);
    let handle = loopback
        .open("/records", "r+", 0)
        .await
        .expect("open baseline file for faulted transaction");
    let error = handle
        .write(ATTEMPTED_DATA, Some(0))
        .await
        .expect_err("faulted write must not acknowledge success");
    assert_eq!(error.code, ErrorCode::Eio, "{}", point.label());
    assert!(
        faults.hit_count() > 0,
        "{} was not exercised",
        point.label()
    );
    drop(handle);
    drop(loopback);
    faults.clear();
    filesystem
        .shutdown()
        .await
        .expect("release writer after faulted ordinary operation");
    drop(filesystem);

    let reopened = open_plain(
        &metadata_path,
        &blocks_path,
        &format!("ordinary-reopen-{}", point.label()),
    )
    .await
    .expect("reopen SQLite stores after fault");
    let data = Loopback::new(reopened.clone())
        .read_file("/records")
        .await
        .expect("read reopened baseline file");
    if point.is_uncertain_commit() {
        assert!(
            data == BASELINE_DATA || data == ATTEMPTED_DATA,
            "metadata flush uncertainty produced invalid data: {data:?}"
        );
    } else {
        assert_eq!(
            data,
            BASELINE_DATA,
            "{} published data unexpectedly",
            point.label()
        );
    }
    reopened
        .shutdown()
        .await
        .expect("release reopened ordinary filesystem");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sqlite_backend_fault_wrappers_preserve_reopen_integrity() {
    for point in FaultPoint::ALL {
        direct_fault_case(point).await;
    }
}

#[cfg(target_os = "linux")]
use mount_rs_fuse::mount::{FuseMount, MountMode, MountOptions, mount};
#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::FileTypeExt;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Output, Stdio};
#[cfg(target_os = "linux")]
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
#[cfg(target_os = "linux")]
use tokio::process::{Child, Command};

#[cfg(target_os = "linux")]
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(target_os = "linux")]
const PYTHON_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(target_os = "linux")]
const PYTHON_KILL_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "linux")]
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(target_os = "linux")]
const MOUNTPOINT_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(target_os = "linux")]
const PYTHON_SQLITE: &str = r#"
import sqlite3
import sys

path, journal, phase = sys.argv[1:4]
baseline = b"baseline:" + bytes(range(256)) * 257
attempted = b"attempted:" + bytes(reversed(range(256))) * 257
db = None
try:
    db = sqlite3.connect(path, timeout=2.0)
    if phase == "seed":
        actual = db.execute("PRAGMA journal_mode=" + journal).fetchone()[0].upper()
    else:
        actual = db.execute("PRAGMA journal_mode").fetchone()[0].upper()
    assert actual == journal, (journal, actual)
    db.execute("PRAGMA busy_timeout=2000")
    db.execute("PRAGMA synchronous=FULL")
    assert db.execute("PRAGMA synchronous").fetchone()[0] == 2

    if phase == "seed":
        db.execute("CREATE TABLE records(id INTEGER PRIMARY KEY, payload BLOB NOT NULL)")
        db.execute("INSERT INTO records VALUES(1, ?)", (baseline,))
        db.commit()
        assert db.execute("SELECT payload FROM records WHERE id=1").fetchone()[0] == baseline
        print("SEED_COMMITTED", flush=True)
    elif phase == "attempt":
        print("READY", flush=True)
        if sys.stdin.readline().strip() != "GO":
            raise RuntimeError("expected GO after READY")
        try:
            db.execute("BEGIN IMMEDIATE")
            db.execute("UPDATE records SET payload=? WHERE id=1", (attempted,))
            db.commit()
        except Exception as error:
            print("COMMIT_ERROR:" + repr(error), flush=True)
            try:
                db.rollback()
            except Exception:
                pass
            raise SystemExit(3)
        print("COMMIT_OK", flush=True)
    elif phase == "verify":
        payload = db.execute("SELECT payload FROM records WHERE id=1").fetchone()[0]
        integrity = db.execute("PRAGMA integrity_check").fetchone()[0]
        assert integrity == "ok", integrity
        print("INTEGRITY_OK", flush=True)
        if payload == baseline:
            print("PAYLOAD=baseline", flush=True)
        elif payload == attempted:
            print("PAYLOAD=attempted", flush=True)
        else:
            print("PAYLOAD=unexpected", flush=True)
            raise SystemExit(4)
    else:
        raise AssertionError("unknown phase: " + phase)
except Exception as error:
    if phase == "attempt":
        print("PREFLIGHT_ERROR:" + repr(error), flush=True)
    raise
finally:
    if db is not None:
        db.close()
"#;

#[cfg(target_os = "linux")]
struct NativeTools {
    fusermount: String,
}

#[cfg(target_os = "linux")]
struct CasePaths {
    root: PathBuf,
    mountpoint: PathBuf,
    metadata: PathBuf,
    blocks: PathBuf,
}

#[cfg(target_os = "linux")]
fn new_case(journal: &str, point: FaultPoint) -> CasePaths {
    let root = tempfile::tempdir()
        .expect("create native fault-test directory")
        .keep();
    let mountpoint = root.join(format!(
        "mount-{}-{}",
        journal.to_ascii_lowercase(),
        point.label()
    ));
    fs::create_dir(&mountpoint).expect("create unmounted native mountpoint");
    CasePaths {
        metadata: root.join("metadata.sqlite"),
        blocks: root.join("blocks.sqlite"),
        mountpoint,
        root,
    }
}

#[cfg(target_os = "linux")]
async fn command_available(command: &str) -> bool {
    tokio::time::timeout(
        MOUNTPOINT_TIMEOUT,
        Command::new(command)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status(),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .is_some_and(|status| status.success())
}

#[cfg(target_os = "linux")]
async fn native_tools() -> NativeTools {
    assert_eq!(
        env::var("MOUNT_RS_RUN_NATIVE_FUSE").as_deref(),
        Ok("1"),
        "set MOUNT_RS_RUN_NATIVE_FUSE=1 to opt into the Linux kernel harness"
    );
    let device = fs::metadata("/dev/fuse").expect("Linux FUSE device /dev/fuse is required");
    assert!(
        device.file_type().is_char_device(),
        "/dev/fuse must be a character device"
    );
    assert!(
        command_available("python3").await,
        "python3 is required to drive real SQLite"
    );
    assert!(
        command_available("mountpoint").await,
        "mountpoint is required for explicit cleanup verification"
    );
    let mut fusermount = None;
    for candidate in ["fusermount3", "fusermount"] {
        if command_available(candidate).await {
            fusermount = Some(candidate);
            break;
        }
    }
    let fusermount = fusermount
        .unwrap_or_else(|| panic!("fusermount3 or fusermount is required for bounded cleanup"));
    NativeTools {
        fusermount: fusermount.to_owned(),
    }
}

#[cfg(target_os = "linux")]
async fn is_mounted(path: &Path) -> Result<bool, String> {
    let status = tokio::time::timeout(
        MOUNTPOINT_TIMEOUT,
        Command::new("mountpoint").arg("-q").arg(path).status(),
    )
    .await
    .map_err(|_| format!("mountpoint query timed out for {}", path.display()))?
    .map_err(|error| format!("run mountpoint for {}: {error}", path.display()))?;
    Ok(status.success())
}

#[cfg(target_os = "linux")]
async fn wait_unmounted(path: &Path) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + UNMOUNT_TIMEOUT;
    loop {
        if !is_mounted(path).await? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("{} remained mounted past deadline", path.display()));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[cfg(target_os = "linux")]
async fn force_unmount(tools: &NativeTools, path: &Path) -> Result<(), String> {
    let status = tokio::time::timeout(
        UNMOUNT_TIMEOUT,
        Command::new(&tools.fusermount)
            .arg("-u")
            .arg("-z")
            .arg("--")
            .arg(path)
            .status(),
    )
    .await
    .map_err(|_| format!("fusermount timed out for {}", path.display()))?
    .map_err(|error| format!("start fusermount for {}: {error}", path.display()))?;
    if !status.success() && is_mounted(path).await? {
        return Err(format!(
            "fusermount failed for {}: {status}",
            path.display()
        ));
    }
    wait_unmounted(path).await
}

#[cfg(target_os = "linux")]
async fn stop_filesystem<M, B>(
    mounted: FuseMount,
    filesystem: &ChunkedFs<M, B>,
    tools: &NativeTools,
    mountpoint: &Path,
) -> Result<(), String>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    let mut errors = Vec::new();
    match tokio::time::timeout(UNMOUNT_TIMEOUT, mounted.unmount()).await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => errors.push(format!("unmount failed: {error}")),
        Err(error) => errors.push(format!("unmount timeout: {error}")),
    }
    match is_mounted(mountpoint).await {
        Ok(true) => {
            if let Err(error) = force_unmount(tools, mountpoint).await {
                errors.push(error);
            }
        }
        Ok(false) => {}
        Err(error) => errors.push(error),
    }
    if let Err(error) = wait_unmounted(mountpoint).await {
        errors.push(error);
    }
    if let Err(error) = filesystem.shutdown().await {
        errors.push(format!("filesystem shutdown failed: {error}"));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

#[cfg(target_os = "linux")]
async fn python_output(path: &Path, journal: &str, phase: &str) -> Result<Output, String> {
    tokio::time::timeout(
        PYTHON_TIMEOUT,
        Command::new("python3")
            .arg("-c")
            .arg(PYTHON_SQLITE)
            .arg(path)
            .arg(journal)
            .arg(phase)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("Python SQLite {phase} timed out"))?
    .map_err(|error| format!("start Python SQLite {phase}: {error}"))
}

#[cfg(target_os = "linux")]
fn output_text(output: &Output) -> String {
    format!(
        "status={} stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[cfg(target_os = "linux")]
async fn python_seed(path: &Path, journal: &str) -> Result<(), String> {
    let output = python_output(path, journal, "seed").await?;
    if output.status.success() && String::from_utf8_lossy(&output.stdout).contains("SEED_COMMITTED")
    {
        Ok(())
    } else {
        Err(format!("Python seed failed: {}", output_text(&output)))
    }
}

#[cfg(target_os = "linux")]
async fn terminate_python(child: &mut Child) {
    let _ = child.start_kill();
    let _ = tokio::time::timeout(PYTHON_KILL_TIMEOUT, child.wait()).await;
}

#[cfg(target_os = "linux")]
fn attempt_marker_summary(
    status: &std::process::ExitStatus,
    stdout: &str,
    stderr: &[u8],
) -> String {
    let mut markers = Vec::new();
    for line in stdout.lines().map(str::trim) {
        let marker = if line == "READY" {
            Some("READY")
        } else if line == "COMMIT_OK" {
            Some("COMMIT_OK")
        } else if line.starts_with("COMMIT_ERROR:") {
            Some("COMMIT_ERROR")
        } else if line.starts_with("PREFLIGHT_ERROR:") {
            Some("PREFLIGHT_ERROR")
        } else {
            None
        };
        if let Some(marker) = marker {
            markers.push(marker);
        }
    }
    format!(
        "status={status} markers={} stderr_present={}",
        if markers.is_empty() {
            "none".to_owned()
        } else {
            markers.join(",")
        },
        !stderr.is_empty()
    )
}

#[cfg(target_os = "linux")]
async fn python_attempt(
    path: &Path,
    journal: &str,
    faults: &FaultPlan,
    point: FaultPoint,
) -> Result<String, String> {
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(PYTHON_SQLITE)
        .arg(path)
        .arg(journal)
        .arg("attempt")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("phase=attempt preflight spawn_failed=true: {error}"))?;
    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| "phase=attempt preflight stdout_pipe=false".to_owned())?,
    );
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "phase=attempt preflight stdin_pipe=false".to_owned())?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| "phase=attempt preflight stderr_pipe=false".to_owned())?;

    let mut first_line = String::new();
    match tokio::time::timeout(PYTHON_TIMEOUT, stdout.read_line(&mut first_line)).await {
        Ok(Ok(length)) if length != 0 && first_line.trim() == "READY" => {}
        Ok(Ok(0)) => {
            terminate_python(&mut child).await;
            return Err("phase=attempt preflight ready=false marker=eof".to_owned());
        }
        Ok(Ok(_)) => {
            terminate_python(&mut child).await;
            let marker = if first_line.trim().starts_with("PREFLIGHT_ERROR:") {
                "PREFLIGHT_ERROR"
            } else {
                "unexpected"
            };
            return Err(format!(
                "phase=attempt preflight ready=false marker={marker}"
            ));
        }
        Ok(Err(error)) => {
            terminate_python(&mut child).await;
            return Err(format!(
                "phase=attempt preflight ready=false read_error={error}"
            ));
        }
        Err(_) => {
            terminate_python(&mut child).await;
            return Err("phase=attempt preflight ready=false timed_out=true".to_owned());
        }
    }

    faults.arm(point);
    if let Err(error) = stdin.write_all(b"GO\n").await {
        terminate_python(&mut child).await;
        return Err(format!(
            "phase=attempt ready=true fault={} go_write_failed={error}",
            point.label()
        ));
    }
    if let Err(error) = stdin.flush().await {
        terminate_python(&mut child).await;
        return Err(format!(
            "phase=attempt ready=true fault={} go_flush_failed={error}",
            point.label()
        ));
    }
    drop(stdin);

    let status = match tokio::time::timeout(PYTHON_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            terminate_python(&mut child).await;
            return Err(format!(
                "phase=attempt ready=true fault={} wait_failed={error}",
                point.label()
            ));
        }
        Err(_) => {
            terminate_python(&mut child).await;
            return Err(format!(
                "phase=attempt ready=true fault={} timed_out=true",
                point.label()
            ));
        }
    };

    let mut stdout_tail = Vec::new();
    match tokio::time::timeout(PYTHON_KILL_TIMEOUT, stdout.read_to_end(&mut stdout_tail)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            return Err(format!(
                "phase=attempt ready=true fault={} stdout_drain_failed={error}",
                point.label()
            ));
        }
        Err(error) => {
            return Err(format!(
                "phase=attempt ready=true fault={} stdout_drain_timeout={error}",
                point.label()
            ));
        }
    }
    let mut stderr_bytes = Vec::new();
    match tokio::time::timeout(PYTHON_KILL_TIMEOUT, stderr.read_to_end(&mut stderr_bytes)).await {
        Ok(Ok(_)) => {}
        Ok(Err(error)) => {
            return Err(format!(
                "phase=attempt ready=true fault={} stderr_drain_failed={error}",
                point.label()
            ));
        }
        Err(error) => {
            return Err(format!(
                "phase=attempt ready=true fault={} stderr_drain_timeout={error}",
                point.label()
            ));
        }
    }
    let mut stdout_text = first_line;
    stdout_text.push_str(&String::from_utf8_lossy(&stdout_tail));
    let diagnostics = attempt_marker_summary(&status, &stdout_text, &stderr_bytes);
    if status.success() || stdout_text.lines().any(|line| line.trim() == "COMMIT_OK") {
        return Err(format!(
            "phase=attempt ready=true fault={} commit_ack=success {diagnostics}",
            point.label()
        ));
    }
    if !stdout_text
        .lines()
        .any(|line| line.trim().starts_with("COMMIT_ERROR:"))
    {
        return Err(format!(
            "phase=attempt ready=true fault={} missing_commit_error {diagnostics}",
            point.label()
        ));
    }
    Ok(diagnostics)
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
enum ReopenedPayload {
    Baseline,
    Attempted,
}

#[cfg(target_os = "linux")]
async fn python_verify(path: &Path, journal: &str) -> Result<ReopenedPayload, String> {
    let output = python_output(path, journal, "verify").await?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !output.status.success() || !stdout.contains("INTEGRITY_OK") {
        return Err(format!(
            "Python reopen verification failed: {}",
            output_text(&output)
        ));
    }
    if stdout.contains("PAYLOAD=baseline") {
        Ok(ReopenedPayload::Baseline)
    } else if stdout.contains("PAYLOAD=attempted") {
        Ok(ReopenedPayload::Attempted)
    } else {
        Err(format!(
            "Python reported unexpected payload: {}",
            output_text(&output)
        ))
    }
}

#[cfg(target_os = "linux")]
fn mount_options() -> MountOptions {
    MountOptions {
        mode: MountMode::Rootless,
        default_permissions: false,
        init_timeout: STARTUP_TIMEOUT,
        unmount_timeout: UNMOUNT_TIMEOUT,
        ..MountOptions::default()
    }
}

#[cfg(target_os = "linux")]
async fn cleanup_case(case: &CasePaths) -> Result<(), String> {
    wait_unmounted(&case.mountpoint).await?;
    fs::remove_dir(&case.mountpoint)
        .map_err(|error| format!("remove verified mountpoint: {error}"))?;
    for base in [&case.metadata, &case.blocks] {
        for suffix in ["", "-journal", "-wal", "-shm"] {
            let mut path = base.as_os_str().to_os_string();
            path.push(suffix);
            match fs::remove_file(PathBuf::from(path)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("remove backend file: {error}")),
            }
        }
    }
    fs::remove_dir(&case.root).map_err(|error| format!("remove test root: {error}"))
}

#[cfg(target_os = "linux")]
async fn native_fault_case(
    point: FaultPoint,
    journal: &str,
    tools: &NativeTools,
) -> Result<(), String> {
    let case = new_case(journal, point);
    let database = case.mountpoint.join(format!("{journal}.sqlite"));
    let faults = FaultPlan::new();
    let filesystem = match open_faulted(
        &case.metadata,
        &case.blocks,
        faults.clone(),
        &format!("native-{}-{journal}", point.label()),
    )
    .await
    {
        Ok(filesystem) => filesystem,
        Err(error) => {
            let _ = cleanup_case(&case).await;
            return Err(format!("open faulted stores: {error}"));
        }
    };
    let mounted = match tokio::time::timeout(
        STARTUP_TIMEOUT,
        mount(
            Arc::new(filesystem.clone()),
            &case.mountpoint,
            mount_options(),
        ),
    )
    .await
    {
        Ok(Ok(mounted)) => mounted,
        Ok(Err(error)) => {
            faults.clear();
            let _ = filesystem.shutdown().await;
            drop(filesystem);
            let _ = cleanup_case(&case).await;
            return Err(format!("mount split SQLite filesystem: {error}"));
        }
        Err(_) => {
            faults.clear();
            let _ = filesystem.shutdown().await;
            drop(filesystem);
            let _ = cleanup_case(&case).await;
            return Err("mount split SQLite filesystem timed out".to_owned());
        }
    };

    if let Err(error) = python_seed(&database, journal).await {
        faults.clear();
        let cleanup = stop_filesystem(mounted, &filesystem, tools, &case.mountpoint).await;
        drop(filesystem);
        let _ = cleanup_case(&case).await;
        return Err(format!(
            "seed baseline database: {error}; cleanup={cleanup:?}"
        ));
    }

    let attempt = python_attempt(&database, journal, &faults, point).await;
    let hits = faults.hit_count();
    faults.clear();
    let cleanup = stop_filesystem(mounted, &filesystem, tools, &case.mountpoint).await;
    if let Err(error) = cleanup {
        drop(filesystem);
        let _ = cleanup_case(&case).await;
        return Err(format!("cleanup after fault: {error}"));
    }
    drop(filesystem);
    let attempt_diagnostics = match attempt {
        Ok(diagnostics) => diagnostics,
        Err(error) => {
            let _ = cleanup_case(&case).await;
            return Err(format!(
                "phase=attempt fault={} hits={hits}: {error}",
                point.label()
            ));
        }
    };
    if hits == 0 {
        let _ = cleanup_case(&case).await;
        return Err(format!(
            "phase=attempt fault={} hits=0: fault was not exercised",
            point.label()
        ));
    }
    eprintln!(
        "sqlite fault phase=attempt fault={} hits={hits} {attempt_diagnostics}",
        point.label()
    );

    let reopened = match open_plain(
        &case.metadata,
        &case.blocks,
        &format!("native-reopen-{}-{journal}", point.label()),
    )
    .await
    {
        Ok(reopened) => reopened,
        Err(error) => {
            let _ = cleanup_case(&case).await;
            return Err(format!("reopen split SQLite filesystem: {error}"));
        }
    };
    let remounted = match tokio::time::timeout(
        STARTUP_TIMEOUT,
        mount(
            Arc::new(reopened.clone()),
            &case.mountpoint,
            mount_options(),
        ),
    )
    .await
    {
        Ok(Ok(remounted)) => remounted,
        Ok(Err(error)) => {
            let _ = reopened.shutdown().await;
            drop(reopened);
            let _ = cleanup_case(&case).await;
            return Err(format!("remount after fault: {error}"));
        }
        Err(_) => {
            let _ = reopened.shutdown().await;
            drop(reopened);
            let _ = cleanup_case(&case).await;
            return Err("remount after fault timed out".to_owned());
        }
    };
    let verification = python_verify(&database, journal).await;
    let remount_cleanup = stop_filesystem(remounted, &reopened, tools, &case.mountpoint).await;
    drop(reopened);
    if let Err(error) = remount_cleanup {
        let _ = cleanup_case(&case).await;
        return Err(format!("cleanup after reopen: {error}"));
    }
    let payload = match verification {
        Ok(payload) => payload,
        Err(error) => {
            let _ = cleanup_case(&case).await;
            return Err(error);
        }
    };
    if point.is_uncertain_commit() {
        if !matches!(
            payload,
            ReopenedPayload::Baseline | ReopenedPayload::Attempted
        ) {
            let _ = cleanup_case(&case).await;
            return Err(format!("invalid uncertain payload: {payload:?}"));
        }
    } else if payload != ReopenedPayload::Baseline {
        let _ = cleanup_case(&case).await;
        return Err(format!(
            "{} fault unexpectedly persisted attempted data",
            point.label()
        ));
    }
    cleanup_case(&case).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux /dev/fuse, fusermount3, Python SQLite, and explicit native opt-in"]
async fn sqlite_backend_faults_against_real_python_on_linux_fuse() {
    #[cfg(not(target_os = "linux"))]
    panic!("Linux FUSE backend-fault acceptance is unsupported on macOS");

    #[cfg(target_os = "linux")]
    {
        let tools = native_tools().await;
        for journal in ["DELETE", "WAL"] {
            for point in FaultPoint::ALL {
                native_fault_case(point, journal, &tools)
                    .await
                    .unwrap_or_else(|error| {
                        panic!(
                            "{}/{journal} backend fault case failed: {error}",
                            point.label()
                        )
                    });
            }
        }
    }
}
