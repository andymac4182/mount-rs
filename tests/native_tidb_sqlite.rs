//! Real SQLite hosting through native Linux FUSE with TiDB metadata.
//!
//! `REQUIREMENTS.md` asks for a SQLite database file hosted on a mounted
//! filesystem whose metadata lives in an actual TiDB. Two compositions run:
//! TiDB metadata with TiDB blocks, and TiDB metadata with RustFS blocks.
//!
//! Both services are owned by the caller. `scripts/test-tidb.sh` supplies
//! `MOUNT_RS_TIDB_URL` after checking that the endpoint identifies as TiDB,
//! and `scripts/test-rustfs.sh` supplies the loopback `R2_*` environment.
//! Each case uses a unique volume key and block prefix. The Python fixture
//! exercises actual SQLite journal locking, rollback, committed recovery and
//! integrity inside the mount; the filesystem is then closed, opened again
//! over fresh TiDB connections, mounted again, and every committed row is
//! read back by SQLite.
//!
//! A pass shows that SQLite's file and lock traffic is served correctly with
//! TiDB metadata on one host. It is not evidence for power loss, for a crash
//! of the mounting process, or for SQLite shared between hosts.

#[cfg(target_os = "linux")]
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
#[cfg(target_os = "linux")]
use mount_rs_core::Loopback;
#[cfg(target_os = "linux")]
use mount_rs_core::storage::{BlockStore, MetadataStore};
#[cfg(target_os = "linux")]
use mount_rs_fuse::mount::{FuseMount, MountMode, MountOptions, mount};
#[cfg(target_os = "linux")]
use mount_rs_r2::{R2BlockStore, R2Config};
#[cfg(target_os = "linux")]
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::os::unix::fs::FileTypeExt;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use std::sync::Arc;
#[cfg(target_os = "linux")]
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(target_os = "linux")]
use tokio::process::Command;

#[cfg(target_os = "linux")]
const RUN_ENV: &str = "MOUNT_RS_RUN_NATIVE_TIDB_SQLITE";
#[cfg(target_os = "linux")]
const TIDB_URL_ENV: &str = "MOUNT_RS_TIDB_URL";
#[cfg(target_os = "linux")]
const MOUNT_TIMEOUT: Duration = Duration::from_secs(30);
#[cfg(target_os = "linux")]
const SQLITE_TIMEOUT: Duration = Duration::from_secs(300);
#[cfg(target_os = "linux")]
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(20);
#[cfg(target_os = "linux")]
const MOUNTPOINT_TIMEOUT: Duration = Duration::from_secs(5);

#[cfg(target_os = "linux")]
struct NativeTools {
    fusermount: String,
}

#[cfg(target_os = "linux")]
struct NativeFuseCleanup {
    mount: Option<FuseMount>,
    mountpoint: PathBuf,
    fusermount: String,
    cleaned: bool,
}

#[cfg(target_os = "linux")]
impl NativeFuseCleanup {
    fn new(mountpoint: PathBuf, fusermount: String) -> Self {
        Self {
            mount: None,
            mountpoint,
            fusermount,
            cleaned: false,
        }
    }

    fn set_mount(&mut self, mount: FuseMount) {
        self.mount = Some(mount);
    }

    async fn cleanup(&mut self) -> Result<(), String> {
        if self.cleaned {
            return Ok(());
        }

        let mut warnings = Vec::new();
        if let Some(mount) = self.mount.as_ref() {
            match tokio::time::timeout(UNMOUNT_TIMEOUT, mount.unmount()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => warnings.push(format!("graceful unmount: {error}")),
                Err(_) => warnings.push(format!("graceful unmount exceeded {UNMOUNT_TIMEOUT:?}")),
            }
        }

        if is_mounted(&self.mountpoint).await?
            && let Err(error) = force_unmount(&self.fusermount, &self.mountpoint).await
        {
            warnings.push(error);
        }

        if is_mounted(&self.mountpoint).await? {
            return Err(format!(
                "FUSE mount remains live at {}; refusing to remove it{}",
                self.mountpoint.display(),
                if warnings.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", warnings.join("; "))
                }
            ));
        }
        wait_unmounted(&self.mountpoint).await?;

        fs::remove_dir(&self.mountpoint).map_err(|error| {
            format!(
                "remove verified unmounted FUSE mountpoint {}: {error}",
                self.mountpoint.display()
            )
        })?;
        self.mount = None;
        self.cleaned = true;
        if !warnings.is_empty() {
            eprintln!(
                "native TiDB FUSE cleanup recovered after: {}",
                warnings.join("; ")
            );
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
impl Drop for NativeFuseCleanup {
    fn drop(&mut self) {
        // The guard deliberately never recursively removes the path. If an
        // async cleanup failed while the mount was still live, FuseMount's
        // own Drop implementation gets a final unmount request and the
        // mountpoint remains available for the caller/CI to diagnose.
        if !self.cleaned && self.mount.is_some() {
            eprintln!(
                "native TiDB FUSE cleanup incomplete; preserving {}",
                self.mountpoint.display()
            );
        }
    }
}

#[cfg(target_os = "linux")]
async fn command_available(command: &str) -> bool {
    tokio::time::timeout(
        MOUNTPOINT_TIMEOUT,
        Command::new(command)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
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
        env::var(RUN_ENV).as_deref(),
        Ok("1"),
        "set {RUN_ENV}=1 to opt into the Linux FUSE/TiDB SQLite-hosting harness"
    );
    let device = fs::metadata("/dev/fuse").expect("Linux FUSE device /dev/fuse is required");
    assert!(
        device.file_type().is_char_device(),
        "/dev/fuse must be a character device"
    );
    assert!(
        command_available("python3").await,
        "python3 is required for the real SQLite fixture"
    );
    assert!(
        command_available("mountpoint").await,
        "mountpoint is required for bounded cleanup verification"
    );
    let mut fusermount = None;
    for candidate in ["fusermount3", "fusermount"] {
        if command_available(candidate).await {
            fusermount = Some(candidate);
            break;
        }
    }
    NativeTools {
        fusermount: fusermount
            .unwrap_or_else(|| {
                panic!("fusermount3 or fusermount is required for bounded FUSE cleanup")
            })
            .to_owned(),
    }
}

#[cfg(target_os = "linux")]
async fn is_mounted(path: &Path) -> Result<bool, String> {
    let status = tokio::time::timeout(
        MOUNTPOINT_TIMEOUT,
        Command::new("mountpoint")
            .arg("-q")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .status(),
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
async fn force_unmount(helper: &str, path: &Path) -> Result<(), String> {
    let status = tokio::time::timeout(
        UNMOUNT_TIMEOUT,
        Command::new(helper)
            .arg("-u")
            .arg("-z")
            .arg("--")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .map_err(|_| format!("{helper} unmount timed out for {}", path.display()))?
    .map_err(|error| format!("start {helper} for {}: {error}", path.display()))?;
    if !status.success() && is_mounted(path).await? {
        return Err(format!(
            "{helper} failed to unmount {} with {status}",
            path.display()
        ));
    }
    wait_unmounted(path).await
}

#[cfg(target_os = "linux")]
fn mount_options() -> MountOptions {
    MountOptions {
        mode: MountMode::Rootless,
        default_permissions: false,
        init_timeout: MOUNT_TIMEOUT,
        unmount_timeout: UNMOUNT_TIMEOUT,
        ..MountOptions::default()
    }
}

#[cfg(target_os = "linux")]
async fn run_sqlite_fixture(mountpoint: &Path) -> Result<(), String> {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sqlite_hosting.py");
    let status = tokio::time::timeout(
        SQLITE_TIMEOUT,
        Command::new("python3")
            .arg(&script)
            .arg(mountpoint)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .status(),
    )
    .await
    .map_err(|_| format!("Python SQLite fixture exceeded {SQLITE_TIMEOUT:?}"))?
    .map_err(|error| format!("start Python SQLite fixture: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("Python SQLite fixture exited with {status}"))
    }
}

#[cfg(target_os = "linux")]
async fn verify_reopened_sqlite<M, B>(
    filesystem: &ChunkedFs<M, B>,
    tools: &NativeTools,
) -> Result<(), String>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
{
    let loopback = Loopback::new(filesystem.clone());
    for path in ["/delete.sqlite", "/wal.sqlite"] {
        let stats = loopback
            .stat(path)
            .await
            .map_err(|error| format!("reopened filesystem stat {path} failed: {error}"))?;
        if stats.size == 0 {
            return Err(format!("reopened SQLite database {path} is empty"));
        }
        let bytes = loopback
            .read_file(path)
            .await
            .map_err(|error| format!("reopened filesystem read {path} failed: {error}"))?;
        if bytes.len() as u64 != stats.size {
            return Err(format!(
                "reopened SQLite database {path} length changed from {} to {}",
                stats.size,
                bytes.len()
            ));
        }
    }
    // A nonempty byte stream is not proof that SQLite can recover it. Mount
    // the fresh provider connections and ask the real engine to verify every
    // committed row without recreating the schema or reseeding the database.
    let mountpoint = tempfile::tempdir()
        .map_err(|error| format!("create reopen mountpoint: {error}"))?
        .keep();
    let mut cleanup = NativeFuseCleanup::new(mountpoint.clone(), tools.fusermount.clone());
    let verification = async {
        let mounted = tokio::time::timeout(
            MOUNT_TIMEOUT,
            mount(Arc::new(filesystem.clone()), &mountpoint, mount_options()),
        )
        .await
        .map_err(|_| "reopen mount timed out".to_owned())?
        .map_err(|error| format!("reopen mount failed: {error}"))?;
        cleanup.set_mount(mounted);
        let output = tokio::time::timeout(
            SQLITE_TIMEOUT,
            Command::new("python3")
                .arg("-c")
                .arg(r#"
import pathlib, sqlite3, sys
root = pathlib.Path(sys.argv[1])
for journal in ('DELETE', 'WAL'):
    db = sqlite3.connect(root / (journal.lower() + '.sqlite'))
    try:
        assert db.execute('PRAGMA journal_mode').fetchone()[0].upper() == journal
        db.execute('PRAGMA synchronous=FULL')
        assert db.execute('PRAGMA integrity_check').fetchall() == [('ok',)]
        rows = db.execute('SELECT id, payload FROM items ORDER BY id').fetchall()
        assert len(rows) == 17, len(rows)
        assert rows[0] == (1, b'uncommitted'), rows[0]
        assert all(payload == b'x' * 8192 for _, payload in rows[1:])
        print(f'TIDB_SQLITE_REOPEN_OK sqlite={sqlite3.sqlite_version} journal={journal}', flush=True)
    finally:
        db.close()
"#)
                .arg(&mountpoint)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await
        .map_err(|_| "reopened SQLite verification timed out".to_owned())?
        .map_err(|error| format!("spawn reopened SQLite verifier: {error}"))?;
        if !output.status.success() {
            return Err(format!(
                "reopened SQLite verification failed: {} {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        eprint!("{}", String::from_utf8_lossy(&output.stdout));
        Ok(())
    }
    .await;
    let cleanup_result = cleanup.cleanup().await;
    match (verification, cleanup_result) {
        (Ok(()), Ok(())) => Ok(()),
        (result, cleanup) => Err(format!(
            "reopen verification={result:?}; cleanup={cleanup:?}"
        )),
    }
}

#[cfg(target_os = "linux")]
fn combine_errors(parts: [Option<String>; 4]) -> Option<String> {
    parts
        .into_iter()
        .flatten()
        .reduce(|left, right| format!("{left}; {right}"))
}

#[cfg(target_os = "linux")]
async fn run_mounted_case<M, B, RF, Fut>(
    label: &str,
    filesystem: ChunkedFs<M, B>,
    reopen: RF,
    tools: &NativeTools,
) -> Result<(), String>
where
    M: MetadataStore + 'static,
    B: BlockStore + 'static,
    RF: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<ChunkedFs<M, B>, String>>,
{
    let mountpoint = tempfile::tempdir()
        .map_err(|error| format!("create {label} mountpoint: {error}"))?
        .keep();
    let mut cleanup = NativeFuseCleanup::new(mountpoint.clone(), tools.fusermount.clone());
    match tokio::time::timeout(
        MOUNT_TIMEOUT,
        mount(Arc::new(filesystem.clone()), &mountpoint, mount_options()),
    )
    .await
    {
        Ok(Ok(mounted)) => {
            cleanup.set_mount(mounted);
        }
        Ok(Err(error)) => {
            let cleanup_result = cleanup.cleanup().await;
            let shutdown_result = filesystem.shutdown().await;
            drop(filesystem);
            return Err(combine_errors([
                Some(format!("{label} mount failed: {error}")),
                cleanup_result
                    .err()
                    .map(|error| format!("cleanup: {error}")),
                shutdown_result
                    .err()
                    .map(|error| format!("filesystem shutdown: {error}")),
                None,
            ])
            .unwrap_or_else(|| format!("{label} mount failed without diagnostics")));
        }
        Err(_) => {
            let cleanup_result = cleanup.cleanup().await;
            let shutdown_result = filesystem.shutdown().await;
            drop(filesystem);
            return Err(combine_errors([
                Some(format!("{label} mount exceeded {MOUNT_TIMEOUT:?}")),
                cleanup_result
                    .err()
                    .map(|error| format!("cleanup: {error}")),
                shutdown_result
                    .err()
                    .map(|error| format!("filesystem shutdown: {error}")),
                None,
            ])
            .unwrap_or_else(|| format!("{label} mount timed out without diagnostics")));
        }
    }

    let fixture_result = run_sqlite_fixture(&mountpoint).await;
    let cleanup_result = cleanup.cleanup().await;
    let shutdown_result = filesystem.shutdown().await;
    drop(filesystem);
    if let Some(error) = combine_errors([
        fixture_result
            .err()
            .map(|error| format!("SQLite hosting: {error}")),
        cleanup_result
            .err()
            .map(|error| format!("cleanup: {error}")),
        shutdown_result
            .err()
            .map(|error| format!("filesystem shutdown: {error}")),
        None,
    ]) {
        return Err(format!("{label}: {error}"));
    }

    let reopened = reopen().await?;
    let verification = verify_reopened_sqlite(&reopened, tools).await;
    let reopen_shutdown = reopened.shutdown().await;
    drop(reopened);
    combine_errors([
        verification.err(),
        reopen_shutdown
            .err()
            .map(|error| format!("reopened filesystem shutdown: {error}")),
        None,
        None,
    ])
    .map_or(Ok(()), |error| Err(format!("{label}: {error}")))
}

#[cfg(target_os = "linux")]
fn unique_key(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    format!("native-tidb-sqlite/{label}/{}/{nanos}", std::process::id())
}

#[cfg(target_os = "linux")]
fn chunked_options(owner: &str) -> ChunkedOptions {
    ChunkedOptions::fixed(owner, 4096)
        .expect("fixed-size chunking configuration")
        .with_root_mode(0o777)
}

#[cfg(target_os = "linux")]
fn tidb_url() -> String {
    let url = env::var(TIDB_URL_ENV)
        .unwrap_or_else(|_| panic!("{TIDB_URL_ENV} is required for the TiDB SQLite-hosting test"));
    assert!(!url.trim().is_empty(), "{TIDB_URL_ENV} must not be empty");
    url
}

#[cfg(target_os = "linux")]
fn tidb_options(volume_key: &str) -> TidbStorageOptions {
    TidbStorageOptions::new(volume_key).with_durable(true)
}

/// The RustFS service owned by the outer harness. A plain `http://` endpoint
/// is required so that a real Cloudflare R2 account, which is only reachable
/// over HTTPS, can never be written to by this test.
#[cfg(target_os = "linux")]
fn rustfs_config() -> R2Config {
    let config = R2Config::from_env()
        .unwrap_or_else(|_| panic!("R2_ENDPOINT, R2_BUCKET, and R2 credentials are required"));
    assert!(
        config.endpoint.starts_with("http://"),
        "the RustFS block store must be a local http:// endpoint"
    );
    config
}

#[cfg(target_os = "linux")]
async fn tidb_tidb_case(url: &str, tools: &NativeTools) -> Result<(), String> {
    let key = unique_key("tidb-tidb");
    let filesystem = ChunkedFs::open(
        TidbMetadataStore::connect_with_options(url, tidb_options(&key))
            .await
            .map_err(|error| format!("connect TiDB metadata: {error}"))?,
        TidbBlockStore::connect_with_options(url, tidb_options(&key))
            .await
            .map_err(|error| format!("connect TiDB blocks: {error}"))?,
        chunked_options("native-tidb-tidb"),
    )
    .await
    .map_err(|error| format!("open TiDB/TiDB filesystem: {error}"))?;
    let url = url.to_owned();
    run_mounted_case(
        "TiDB metadata + TiDB blocks",
        filesystem,
        move || async move {
            ChunkedFs::open(
                TidbMetadataStore::connect_with_options(&url, tidb_options(&key))
                    .await
                    .map_err(|error| format!("reconnect TiDB metadata: {error}"))?,
                TidbBlockStore::connect_with_options(&url, tidb_options(&key))
                    .await
                    .map_err(|error| format!("reconnect TiDB blocks: {error}"))?,
                chunked_options("native-tidb-tidb-reopen"),
            )
            .await
            .map_err(|error| format!("reopen TiDB/TiDB filesystem: {error}"))
        },
        tools,
    )
    .await
}

#[cfg(target_os = "linux")]
async fn tidb_rustfs_case(url: &str, tools: &NativeTools) -> Result<(), String> {
    let key = unique_key("tidb-rustfs");
    let config = rustfs_config();
    let filesystem = ChunkedFs::open(
        TidbMetadataStore::connect_with_options(url, tidb_options(&key))
            .await
            .map_err(|error| format!("connect TiDB metadata: {error}"))?,
        R2BlockStore::from_config(&config, key.clone())
            .map_err(|error| format!("configure RustFS blocks: {error}"))?,
        chunked_options("native-tidb-rustfs"),
    )
    .await
    .map_err(|error| format!("open TiDB/RustFS filesystem: {error}"))?;
    let url = url.to_owned();
    run_mounted_case(
        "TiDB metadata + RustFS blocks",
        filesystem,
        move || async move {
            ChunkedFs::open(
                TidbMetadataStore::connect_with_options(&url, tidb_options(&key))
                    .await
                    .map_err(|error| format!("reconnect TiDB metadata: {error}"))?,
                R2BlockStore::from_config(&config, key.clone())
                    .map_err(|error| format!("reconfigure RustFS blocks: {error}"))?,
                chunked_options("native-tidb-rustfs-reopen"),
            )
            .await
            .map_err(|error| format!("reopen TiDB/RustFS filesystem: {error}"))
        },
        tools,
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux /dev/fuse, fusermount3, Python SQLite, an actual TiDB URL and an explicit opt-in"]
async fn native_fuse_hosts_sqlite_on_tidb_metadata_and_tidb_blocks() {
    #[cfg(not(target_os = "linux"))]
    panic!("native TiDB SQLite hosting over FUSE is Linux-only");

    #[cfg(target_os = "linux")]
    {
        let tools = native_tools().await;
        tidb_tidb_case(&tidb_url(), &tools)
            .await
            .unwrap_or_else(|error| panic!("native FUSE case failed: {error}"));
        eprintln!("TIDB_SQLITE_HOSTING_PASS metadata=tidb blocks=tidb transport=fuse");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires Linux /dev/fuse, fusermount3, Python SQLite, an actual TiDB URL, a RustFS endpoint and an explicit opt-in"]
async fn native_fuse_hosts_sqlite_on_tidb_metadata_and_rustfs_blocks() {
    #[cfg(not(target_os = "linux"))]
    panic!("native TiDB SQLite hosting over FUSE is Linux-only");

    #[cfg(target_os = "linux")]
    {
        let tools = native_tools().await;
        tidb_rustfs_case(&tidb_url(), &tools)
            .await
            .unwrap_or_else(|error| panic!("native FUSE case failed: {error}"));
        eprintln!("TIDB_SQLITE_HOSTING_PASS metadata=tidb blocks=rustfs transport=fuse");
    }
}
