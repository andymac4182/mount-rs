//! Opt-in native macOS NFS acceptance for concurrent TiDB metadata with TiDB
//! or RustFS blocks.
//!
//! The caller owns the disposable TiDB/PD/TiKV topology. This fixture creates
//! unique volume keys and native mountpoints; it never changes the service.

#![cfg(target_os = "macos")]

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Barrier};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MOUNT_READY_TIMEOUT: Duration = Duration::from_secs(40);
const VISIBILITY_TIMEOUT: Duration = Duration::from_secs(30);
const CLEAN_EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(15);
const LOAD_FILES_PER_WRITER: usize = 20;
const LOAD_DURATION_LIMIT: Duration = Duration::from_secs(180);

#[test]
#[ignore = "requires opt-in native macOS NFS and a disposable loopback TiDB service"]
fn cli_one_process_tidb_two_mountpoints_share_writable_files() {
    let sql_port = require_disposable_endpoint();
    let scope = TestScope::new(sql_port);
    let config = scope.write_config("a");
    let mut cli = NativeMount::spawn(&scope, &config, true);
    cli.wait_ready()
        .expect("one CLI mounts both TiDB NFS views");

    let a_payload = b"one CLI TiDB view A writes";
    let b_payload = b"one CLI TiDB view B writes";
    write_and_sync(&scope.mountpoint_a.join("from-a"), a_payload).expect("A view creates a file");
    await_bytes(&scope.mountpoint_b.join("from-a"), a_payload)
        .expect("B view reads A's acknowledged file");
    write_and_sync(&scope.mountpoint_b.join("from-b"), b_payload).expect("B view creates a file");
    await_bytes(&scope.mountpoint_a.join("from-b"), b_payload)
        .expect("A view reads B's acknowledged file");
    fs::rename(
        scope.mountpoint_b.join("from-b"),
        scope.mountpoint_b.join("renamed-from-b"),
    )
    .expect("B view renames its file");
    await_bytes(&scope.mountpoint_a.join("renamed-from-b"), b_payload)
        .expect("A view sees B's rename");
    await_absent(&scope.mountpoint_a.join("from-b")).expect("A sees old name disappear");
    fs::remove_file(scope.mountpoint_a.join("from-a")).expect("A unlinks its file");
    await_absent(&scope.mountpoint_b.join("from-a")).expect("B sees A's unlink");

    cli.clean_stop()
        .expect("one CLI clean SIGINT and both unmounts");
    println!("TIDB_NATIVE_ONE_PROCESS_PASS writable_views=2");
    scope.finish();
}

#[test]
#[ignore = "requires opt-in native macOS NFS and a disposable loopback TiDB service"]
fn cli_two_process_tidb_volume_stays_coherent_under_load_and_reopens() {
    run_two_process(NativeBlocks::Tidb);
}

#[test]
#[ignore = "requires opt-in native macOS NFS and disposable loopback TiDB and RustFS services"]
fn cli_two_process_tidb_metadata_rustfs_blocks_stays_coherent_under_load_and_reopens() {
    assert_eq!(
        std::env::var("MOUNT_RS_CLI_NATIVE_RUSTFS_DISPOSABLE")
            .ok()
            .as_deref(),
        Some("1"),
        "set MOUNT_RS_CLI_NATIVE_RUSTFS_DISPOSABLE=1 to opt in"
    );
    run_two_process(NativeBlocks::RustFs);
}

#[derive(Clone, Copy)]
enum NativeBlocks {
    Tidb,
    RustFs,
}

fn run_two_process(blocks: NativeBlocks) {
    let sql_port = require_disposable_endpoint();
    let scope = TestScope::new(sql_port);
    let block_provider = match blocks {
        NativeBlocks::Tidb => None,
        NativeBlocks::RustFs => Some(scope.rustfs_blocks_config()),
    };
    let config_a = scope.write_config_with_blocks("a", block_provider.as_ref());
    let config_b = scope.write_config_with_blocks("b", block_provider.as_ref());
    let mut mount_a = NativeMount::spawn(&scope, &config_a, false);
    mount_a.wait_ready().expect("first TiDB NFS mount");
    let mut mount_b = NativeMount::spawn(&scope, &config_b, false);
    mount_b.wait_ready().expect("second TiDB NFS mount");
    assert!(
        mount_a.is_alive() && mount_b.is_alive(),
        "both CLIs remain alive"
    );

    let started = Instant::now();
    run_bounded_load(&scope).expect("two independent TiDB CLIs load");
    let elapsed = started.elapsed();
    assert!(
        elapsed <= LOAD_DURATION_LIMIT,
        "TiDB load exceeded {LOAD_DURATION_LIMIT:?}: {elapsed:?}"
    );
    let load_label = match blocks {
        NativeBlocks::Tidb => "TIDB_NATIVE_LOAD_PASS",
        NativeBlocks::RustFs => "TIDB_RUSTFS_NATIVE_LOAD_PASS",
    };
    println!(
        "{load_label} files_per_writer={LOAD_FILES_PER_WRITER} acknowledged_writes={} acknowledged_calls={} elapsed_ms={}",
        LOAD_FILES_PER_WRITER * 2,
        LOAD_FILES_PER_WRITER * 2 * 4,
        elapsed.as_millis()
    );

    let shared_name = "shared-disjoint-ranges";
    let shared_seed = vec![b'.'; 8192];
    write_and_sync(&scope.mountpoint_a.join(shared_name), &shared_seed).expect("seed shared file");
    await_bytes(&scope.mountpoint_b.join(shared_name), &shared_seed).expect("B sees shared seed");
    let first = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_a.join(shared_name))
        .expect("open A shared handle");
    let second = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_b.join(shared_name))
        .expect("open B shared handle");
    let barrier = Arc::new(Barrier::new(3));
    let left = spawn_range_writer(first, 0, vec![b'1'; 4096], Arc::clone(&barrier));
    let right = spawn_range_writer(second, 4096, vec![b'2'; 4096], Arc::clone(&barrier));
    barrier.wait();
    // Join both workers before inspecting either failure, so cleanup cannot
    // race a writer left running after its peer failed.
    let left_result = left.join();
    let right_result = right.join();
    left_result
        .expect("join A range writer")
        .expect("A range write and sync");
    right_result
        .expect("join B range writer")
        .expect("B range write and sync");
    let merged = [vec![b'1'; 4096], vec![b'2'; 4096]].concat();
    for mountpoint in [&scope.mountpoint_a, &scope.mountpoint_b] {
        await_bytes(&mountpoint.join(shared_name), &merged)
            .expect("both acknowledged disjoint ranges survive");
    }

    let original = b"held inode survives remote rename, replacement and unlink";
    let replacement = b"new inode under the original pathname";
    write_and_sync(&scope.mountpoint_a.join("held-original"), original).expect("seed held inode");
    await_bytes(&scope.mountpoint_b.join("held-original"), original)
        .expect("B sees original inode");
    let mut held = fs::File::open(scope.mountpoint_b.join("held-original"))
        .expect("hold B inode while A changes names");
    fs::rename(
        scope.mountpoint_a.join("held-original"),
        scope.mountpoint_a.join("renamed-original"),
    )
    .expect("A renames held inode");
    await_bytes(&scope.mountpoint_b.join("renamed-original"), original).expect("B sees A's rename");
    await_absent(&scope.mountpoint_b.join("held-original"))
        .expect("B sees original name disappear");
    write_and_sync(&scope.mountpoint_a.join("held-original"), replacement)
        .expect("A replaces original pathname with a new inode");
    await_bytes(&scope.mountpoint_b.join("held-original"), replacement)
        .expect("B sees replacement pathname");
    assert_handle_bytes(&mut held, original).expect("held handle keeps original inode identity");
    fs::remove_file(scope.mountpoint_a.join("renamed-original"))
        .expect("A unlinks inode held by B");
    await_absent(&scope.mountpoint_b.join("renamed-original")).expect("B sees A's unlink");
    assert_handle_bytes(&mut held, original)
        .expect("B held inode remains readable after remote unlink");
    drop(held);

    mount_b.clean_stop().expect("B clean SIGINT and unmount");
    mount_a.clean_stop().expect("A clean SIGINT and unmount");
    let mut reopened = NativeMount::spawn(&scope, &config_a, false);
    reopened.wait_ready().expect("fresh TiDB CLI reopen");
    for writer in ['a', 'b'] {
        for index in 0..LOAD_FILES_PER_WRITER {
            await_bytes(
                &scope.mountpoint_a.join(load_name(writer, index)),
                &payload(writer, index),
            )
            .expect("fresh CLI sees every acknowledged load file");
        }
    }
    await_bytes(&scope.mountpoint_a.join(shared_name), &merged)
        .expect("fresh CLI sees merged disjoint ranges");
    await_bytes(&scope.mountpoint_a.join("held-original"), replacement)
        .expect("fresh CLI sees replacement inode bytes");
    await_absent(&scope.mountpoint_a.join("renamed-original"))
        .expect("fresh CLI sees durable unlink");
    reopened
        .clean_stop()
        .expect("fresh CLI clean SIGINT and unmount");
    let pass_label = match blocks {
        NativeBlocks::Tidb => "TIDB_NATIVE_TWO_PROCESS_PASS",
        NativeBlocks::RustFs => "TIDB_RUSTFS_NATIVE_TWO_PROCESS_PASS",
    };
    println!(
        "{pass_label} writable_processes=2 fresh_reopen_files={}",
        LOAD_FILES_PER_WRITER * 2
    );
    scope.finish();
}

fn require_disposable_endpoint() -> u16 {
    for name in [
        "MOUNT_RS_CLI_NATIVE_NFS",
        "MOUNT_RS_CLI_NATIVE_TIDB_TWO_PROCESS",
    ] {
        assert_eq!(
            std::env::var(name).ok().as_deref(),
            Some("1"),
            "set {name}=1 to opt in"
        );
    }
    let url = std::env::var("MOUNT_RS_TIDB_URL").expect("set disposable MOUNT_RS_TIDB_URL");
    loopback_sql_port(&url).expect("native TiDB acceptance requires a loopback mysql:// endpoint")
}

fn loopback_sql_port(url: &str) -> Option<u16> {
    let authority = url.strip_prefix("mysql://")?.split('/').next()?;
    let host = authority.rsplit('@').next()?;
    host.strip_prefix("127.0.0.1:")?
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
}

fn loopback_rustfs_port(endpoint: &str) -> Option<u16> {
    endpoint
        .strip_prefix("http://127.0.0.1:")?
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)
}

fn feature_cli_in_test_profile(test_executable: &Path) -> io::Result<PathBuf> {
    let profile = test_executable
        .parent()
        .filter(|deps| deps.file_name().is_some_and(|name| name == "deps"))
        .and_then(Path::parent)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "test executable is outside Cargo deps layout",
            )
        })?;
    Ok(profile.join(format!("mount-rs{}", std::env::consts::EXE_SUFFIX)))
}

fn payload(writer: char, index: usize) -> Vec<u8> {
    format!("{writer}-{index:02}-").repeat(128).into_bytes()
}

fn load_name(writer: char, index: usize) -> String {
    format!("load-{writer}-{index:02}")
}

fn write_and_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

fn run_bounded_load(scope: &TestScope) -> io::Result<()> {
    let barrier = Arc::new(Barrier::new(3));
    let worker = |mountpoint: PathBuf, writer: char, barrier: Arc<Barrier>| {
        thread::spawn(move || -> io::Result<()> {
            barrier.wait();
            for index in 0..LOAD_FILES_PER_WRITER {
                let path = mountpoint.join(load_name(writer, index));
                let bytes = payload(writer, index);
                write_and_sync(&path, &bytes)?;
                if fs::read(&path)? != bytes {
                    return Err(io::Error::other(format!(
                        "local read mismatch at {}",
                        path.display()
                    )));
                }
            }
            Ok(())
        })
    };
    let a = worker(scope.mountpoint_a.clone(), 'a', Arc::clone(&barrier));
    let b = worker(scope.mountpoint_b.clone(), 'b', Arc::clone(&barrier));
    barrier.wait();
    let a_result = a.join().expect("join A load worker");
    let b_result = b.join().expect("join B load worker");
    a_result?;
    b_result?;
    for index in 0..LOAD_FILES_PER_WRITER {
        await_bytes(
            &scope.mountpoint_b.join(load_name('a', index)),
            &payload('a', index),
        )?;
        await_bytes(
            &scope.mountpoint_a.join(load_name('b', index)),
            &payload('b', index),
        )?;
    }
    Ok(())
}

fn spawn_range_writer(
    mut file: fs::File,
    offset: u64,
    bytes: Vec<u8>,
    barrier: Arc<Barrier>,
) -> JoinHandle<io::Result<()>> {
    thread::spawn(move || {
        barrier.wait();
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&bytes)?;
        file.sync_all()
    })
}

fn assert_handle_bytes(file: &mut fs::File, expected: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(0))?;
    let mut actual = Vec::new();
    file.read_to_end(&mut actual)?;
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::other("held handle changed inode bytes"))
    }
}

fn await_bytes(path: &Path, expected: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + VISIBILITY_TIMEOUT;
    loop {
        let observation = match fs::read(path) {
            Ok(actual) if actual == expected => return Ok(()),
            Ok(actual) => format!("read {} bytes, expected {}", actual.len(), expected.len()),
            Err(error) => error.to_string(),
        };
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "{} not visible with expected bytes: {observation}",
                path.display()
            )));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn await_absent(path: &Path) -> io::Result<()> {
    let deadline = Instant::now() + VISIBILITY_TIMEOUT;
    loop {
        match fs::metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Ok(_) | Err(_) if Instant::now() < deadline => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "{} remains visible after unlink",
                    path.display()
                )));
            }
            Err(error) => return Err(error),
        }
        thread::sleep(Duration::from_millis(100));
    }
}

struct TestScope {
    root: PathBuf,
    root_identity: (u64, u64),
    cli_binary: PathBuf,
    mountpoint_a: PathBuf,
    mountpoint_b: PathBuf,
    armed: bool,
}

impl TestScope {
    fn new(sql_port: u16) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after Unix epoch")
            .as_nanos();
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mount-rs-cli-tidb-two-process-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create run-owned native TiDB root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("restrict owned root");
        let root_identity = checked_plain_directory(&root).expect("record owned root identity");
        let mountpoint_a = root.join("mount-a");
        let mountpoint_b = root.join("mount-b");
        fs::create_dir(&mountpoint_a).expect("create A mountpoint");
        fs::create_dir(&mountpoint_b).expect("create B mountpoint");
        let cli_binary = root.join("mount-rs-test-cli");
        let scope = Self {
            root,
            root_identity,
            cli_binary,
            mountpoint_a,
            mountpoint_b,
            armed: true,
        };
        println!(
            "TIDB_NATIVE_OWNED_SCOPE root={} dev={} ino={} sql_port={sql_port}",
            scope.root.display(),
            root_identity.0,
            root_identity.1
        );
        let test_executable =
            std::env::current_exe().expect("locate current Cargo test executable");
        let cli =
            feature_cli_in_test_profile(&test_executable).expect("locate current profile CLI");
        fs::copy(cli, &scope.cli_binary).expect("pin current profile CLI into owned scope");
        fs::set_permissions(&scope.cli_binary, fs::Permissions::from_mode(0o700))
            .expect("make pinned CLI executable");
        scope
    }

    fn run_name(&self) -> &str {
        self.root
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF8 owned run name")
    }

    fn write_config(&self, writer: &str) -> PathBuf {
        self.write_config_with_blocks(writer, None)
    }

    fn rustfs_blocks_config(&self) -> serde_json::Value {
        let required = |name: &str| {
            let value = std::env::var(name).unwrap_or_else(|_| panic!("missing {name}"));
            assert!(!value.is_empty(), "{name} must not be empty");
            value
        };
        let endpoint = required("RUSTFS_ENDPOINT");
        let port = loopback_rustfs_port(&endpoint)
            .expect("native RustFS acceptance requires a loopback HTTP endpoint");
        // Check credentials without writing their values to configs or logs.
        required("RUSTFS_ACCESS_KEY_ID");
        required("RUSTFS_SECRET_ACCESS_KEY");
        println!(
            "TIDB_NATIVE_OWNED_RUSTFS run={} rustfs_port={port}",
            self.run_name()
        );
        serde_json::json!({
            "kind": "rustfs", "endpoint": endpoint,
            "bucket": required("RUSTFS_BUCKET"), "region": required("RUSTFS_REGION"),
            "prefix": format!("mount-rs-cli-tidb/{}/blocks", self.run_name()),
            "access_key_id": { "env": "RUSTFS_ACCESS_KEY_ID" },
            "secret_access_key": { "env": "RUSTFS_SECRET_ACCESS_KEY" }, "durable": true
        })
    }

    fn write_config_with_blocks(
        &self,
        writer: &str,
        blocks: Option<&serde_json::Value>,
    ) -> PathBuf {
        let mountpoint = match writer {
            "a" => &self.mountpoint_a,
            "b" => &self.mountpoint_b,
            _ => panic!("unknown writer"),
        };
        let provider = serde_json::json!({
            "kind": "tidb", "connection": { "env": "MOUNT_RS_TIDB_URL" },
            "volume_key": format!("mount-rs/cli-native-tidb/{}", self.run_name()), "durable": true
        });
        let blocks = blocks.cloned().unwrap_or_else(|| provider.clone());
        let config = serde_json::json!({
            "version": 1, "mountpoint": mountpoint, "transport": "nfs",
            "driver": { "kind": "splitstore", "storage": {
                "concurrent_writes": true, "metadata": provider, "blocks": blocks,
                "chunk_size_bytes": 4096, "owner": format!("cli-native-tidb-{writer}")
            }}
        });
        let path = self.root.join(format!("config-{writer}.json"));
        fs::write(
            &path,
            serde_json::to_vec_pretty(&config).expect("serialize native TiDB config"),
        )
        .expect("write owned config");
        path
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        if checked_plain_directory(&self.root)? != self.root_identity {
            return Err(format!(
                "owned root identity changed; preserving {}",
                self.root.display()
            ));
        }
        checked_plain_directory(&self.mountpoint_a)?;
        checked_plain_directory(&self.mountpoint_b)?;
        let mut errors = Vec::new();
        for target in [&self.mountpoint_a, &self.mountpoint_b] {
            if let Err(error) = unmount_exact_bounded(target) {
                errors.push(error);
            }
        }
        if is_mounted_at(&self.mountpoint_a)? || is_mounted_at(&self.mountpoint_b)? {
            return Err(format!(
                "owned native mount remains; preserving {}: {}",
                self.root.display(),
                errors.join("; ")
            ));
        }
        if checked_plain_directory(&self.root)? != self.root_identity {
            return Err(format!(
                "owned root changed before removal; preserving {}",
                self.root.display()
            ));
        }
        fs::remove_dir_all(&self.root)
            .map_err(|error| format!("remove {}: {error}", self.root.display()))?;
        self.armed = false;
        println!(
            "TIDB_NATIVE_OWNED_SCOPE_REMOVED root={}",
            self.root.display()
        );
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn finish(mut self) {
        self.cleanup().expect("clean exact owned native artifacts");
    }
}

impl Drop for TestScope {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("native TiDB scope cleanup failed: {error}");
        }
    }
}

struct NativeMount {
    child: Child,
    mountpoints: Vec<PathBuf>,
    lines: Receiver<String>,
    readers: Vec<JoinHandle<()>>,
    output: Vec<String>,
    cleaned: bool,
}

impl NativeMount {
    fn spawn(scope: &TestScope, config: &Path, also: bool) -> Self {
        let primary = if config
            .file_name()
            .is_some_and(|name| name == "config-b.json")
        {
            &scope.mountpoint_b
        } else {
            &scope.mountpoint_a
        };
        let mut mountpoints = vec![primary.clone()];
        let mut command = Command::new(&scope.cli_binary);
        command
            .args(["mount", "--config"])
            .arg(config)
            .arg("--quiet");
        if also {
            command.arg("--also-mountpoint").arg(&scope.mountpoint_b);
            mountpoints.push(scope.mountpoint_b.clone());
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn exact owned TiDB CLI child");
        println!(
            "TIDB_NATIVE_OWNED_CLI pid={} binary={} mountpoints={mountpoints:?}",
            child.id(),
            scope.cli_binary.display()
        );
        let (sender, lines) = mpsc::channel();
        let readers = vec![
            spawn_line_reader(
                child.stdout.take().expect("capture CLI stdout"),
                "stdout",
                sender.clone(),
            ),
            spawn_line_reader(
                child.stderr.take().expect("capture CLI stderr"),
                "stderr",
                sender,
            ),
        ];
        Self {
            child,
            mountpoints,
            lines,
            readers,
            output: Vec::new(),
            cleaned: false,
        }
    }

    fn is_alive(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("poll exact owned CLI child")
            .is_none()
    }

    fn all_unmounted(&self) -> Result<bool, String> {
        for mountpoint in &self.mountpoints {
            if is_mounted_at(mountpoint)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn wait_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + MOUNT_READY_TIMEOUT;
        let mut reported_ready = false;
        loop {
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    if line.contains("mounted nfs") {
                        reported_ready = true;
                        println!("TIDB_NATIVE_OWNED_NFS pid={} {line}", self.child.id());
                    }
                    self.output.push(line);
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
            }
            let mut all_ready = true;
            for mountpoint in &self.mountpoints {
                all_ready &= is_mounted_at(mountpoint)?;
            }
            if reported_ready && all_ready {
                return Ok(());
            }
            if !self.is_alive() || Instant::now() >= deadline {
                let _ = self.stop(false);
                return Err(format!(
                    "CLI did not mount {:?} within {MOUNT_READY_TIMEOUT:?}; output={:?}",
                    self.mountpoints, self.output
                ));
            }
        }
    }

    fn clean_stop(&mut self) -> Result<(), String> {
        self.stop(true)
    }

    fn stop(&mut self, expect_clean: bool) -> Result<(), String> {
        if self.cleaned {
            return Ok(());
        }
        if self.is_alive() {
            // SAFETY: this is the exact child PID owned by this guard.
            let result = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
            if result != 0 {
                return Err(format!(
                    "SIGINT owned CLI child: {}",
                    io::Error::last_os_error()
                ));
            }
        }
        let status = wait_child_bounded(&mut self.child, CLEAN_EXIT_TIMEOUT);
        if status.is_none() {
            let _ = self.child.kill();
        }
        let final_status = status.or_else(|| self.child.wait().ok());
        for reader in self.readers.drain(..) {
            reader.join().expect("join owned CLI output reader");
        }
        self.output.extend(self.lines.try_iter());
        let normal_unmounted = self.all_unmounted();
        let mut errors = Vec::new();
        for mountpoint in &self.mountpoints {
            if let Err(error) = unmount_exact_bounded(mountpoint) {
                errors.push(error);
            }
        }
        self.cleaned = self.all_unmounted()?;
        for line in self
            .output
            .iter()
            .filter(|line| line.contains("MOUNT_RS_FAILURE_TRACE"))
        {
            eprintln!("TIDB_NATIVE_CLI_FAILURE pid={} {line}", self.child.id());
        }
        println!(
            "TIDB_NATIVE_OWNED_CLI_EXIT pid={} status={final_status:?} unmounted={}",
            self.child.id(),
            self.cleaned
        );
        if expect_clean
            && (!final_status.is_some_and(|status| status.success())
                || !normal_unmounted?
                || !self.output.iter().any(|line| line.contains("unmounted")))
        {
            return Err(format!(
                "CLI failed normal clean unmount: status={final_status:?} output={:?} fallback={errors:?}",
                self.output
            ));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }
}

impl Drop for NativeMount {
    fn drop(&mut self) {
        if let Err(error) = self.stop(false) {
            eprintln!("owned TiDB CLI cleanup failed: {error}");
        }
    }
}

fn spawn_line_reader(
    pipe: impl io::Read + Send + 'static,
    stream: &'static str,
    sender: mpsc::Sender<String>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(pipe).lines().map_while(Result::ok) {
            let _ = sender.send(format!("{stream}: {line}"));
        }
    })
}

fn wait_child_bounded(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => return None,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
        }
    }
}

fn unmount_exact_bounded(mountpoint: &Path) -> Result<(), String> {
    let original_identity = checked_plain_directory(mountpoint)?;
    if !is_mounted_at(mountpoint)? {
        return Ok(());
    }
    if checked_plain_directory(mountpoint)? != original_identity {
        return Err("owned NFS mountpoint changed before umount".into());
    }
    let mut child = Command::new("umount")
        .arg("-f")
        .arg(mountpoint)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("spawn exact-path NFS umount: {error}"))?;
    if wait_child_bounded(&mut child, UNMOUNT_TIMEOUT).is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    if is_mounted_at(mountpoint)? {
        Err(format!(
            "owned mount {} remains after bounded umount",
            mountpoint.display()
        ))
    } else {
        Ok(())
    }
}

fn is_mounted_at(target: &Path) -> Result<bool, String> {
    checked_plain_directory(target)?;
    let output = Command::new("mount")
        .output()
        .map_err(|error| format!("read macOS mount table: {error}"))?;
    if !output.status.success() {
        return Err(format!("mount table command exited {}", output.status));
    }
    let canonical_target = fs::canonicalize(target).unwrap_or_else(|_| target.to_path_buf());
    let table = String::from_utf8_lossy(&output.stdout);
    Ok(
        mount_rs_nfs::parse_mount_table(mount_rs_nfs::NfsPlatform::Macos, &table)
            .iter()
            .any(|entry| {
                let entry_target = Path::new(&entry.target);
                entry_target == target
                    || fs::canonicalize(entry_target).is_ok_and(|path| path == canonical_target)
            }),
    )
}

fn checked_plain_directory(path: &Path) -> Result<(u64, u64), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("stat owned path {}: {error}", path.display()))?;
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "owned path {} is not a plain directory; preserving root",
            path.display()
        ));
    }
    Ok((metadata.dev(), metadata.ino()))
}

#[test]
fn native_tidb_endpoint_guard_rejects_remote_and_zero_ports() {
    assert_eq!(
        loopback_sql_port("mysql://root@127.0.0.1:4000/test"),
        Some(4000)
    );
    assert_eq!(
        loopback_sql_port("mysql://u:secret@127.0.0.1:65535/test?require_ssl=true"),
        Some(65535)
    );
    for url in [
        "mysql://root@remote:4000/test",
        "mysql://root@127.0.0.1:0/test",
        "mysql://root@127.0.0.1:65536/test",
        "mysql://root@127.0.0.1:4000.remote/test",
        "postgres://root@127.0.0.1:4000/test",
    ] {
        assert_eq!(loopback_sql_port(url), None);
    }
    assert_eq!(loopback_rustfs_port("http://127.0.0.1:9000"), Some(9000));
    for endpoint in [
        "http://remote:9000",
        "http://127.0.0.1:0",
        "http://127.0.0.1:65536",
        "http://127.0.0.1:9000.remote",
        "https://127.0.0.1:9000",
    ] {
        assert_eq!(loopback_rustfs_port(endpoint), None);
    }
}

#[test]
fn native_tidb_cli_profile_uses_current_test_location() {
    let current = Path::new("/new-target/debug/deps/native_two_process_tidb-cached");
    assert_eq!(
        feature_cli_in_test_profile(current).unwrap(),
        Path::new("/new-target/debug/mount-rs")
    );
    assert!(feature_cli_in_test_profile(Path::new("/new-target/debug/test")).is_err());
}

#[test]
fn native_tidb_cleanup_rejects_symlinked_paths() {
    let scope = tempfile::tempdir().unwrap();
    let real = scope.path().join("real");
    let redirected = scope.path().join("redirected");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &redirected).unwrap();
    assert!(checked_plain_directory(&real).is_ok());
    assert!(checked_plain_directory(&redirected).is_err());
    assert!(unmount_exact_bounded(&redirected).is_err());
    assert!(real.exists());
}
