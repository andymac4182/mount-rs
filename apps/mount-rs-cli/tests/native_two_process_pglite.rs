//! Opt-in macOS NFS acceptance for two independent CLI processes using one
//! PGlite socket server and one split volume, with PGlite or RustFS blocks.
//!
//! One Node process owns the PGlite data directory at a time. The test never
//! opens two independent embedded engines on the same database directory.

#![cfg(target_os = "macos")]

use std::fs;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Barrier};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MOUNT_READY_TIMEOUT: Duration = Duration::from_secs(40);
const SERVER_READY_TIMEOUT: Duration = Duration::from_secs(30);
const VISIBILITY_TIMEOUT: Duration = Duration::from_secs(30);
const CLEAN_EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(15);
const LOAD_FILES_PER_WRITER: usize = 20;
const LOAD_DURATION_LIMIT: Duration = Duration::from_secs(120);

#[test]
#[ignore = "requires opt-in macOS native NFS and installed tests/pglite Node dependencies"]
fn cli_two_process_pglite_volume_stays_coherent_under_load_and_reopens() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_PGLITE_TWO_PROCESS");
    run_two_process(NativeBlocks::Pglite);
}

#[test]
#[ignore = "requires opt-in macOS native NFS, isolated PGlite, and disposable RustFS"]
fn cli_two_process_pglite_metadata_rustfs_blocks_stays_coherent_under_load_and_reopens() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_PGLITE_TWO_PROCESS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_RUSTFS_DISPOSABLE");
    run_two_process(NativeBlocks::RustFs);
}

#[derive(Clone, Copy)]
enum NativeBlocks {
    Pglite,
    RustFs,
}

fn run_two_process(blocks: NativeBlocks) {
    let scope = TestScope::new();
    let volume_key = format!("mount-rs/cli-two-process-pglite/{}", scope.run_name());
    let data_dir = scope.root.join("pglite-data");
    fs::create_dir(&data_dir).expect("create run-owned PGlite data directory");
    let mut server = PgliteServer::start(&data_dir);
    server
        .wait_ready()
        .expect("isolated PGlite socket readiness");
    let block_provider = match blocks {
        NativeBlocks::Pglite => None,
        NativeBlocks::RustFs => Some(scope.rustfs_blocks_config()),
    };
    let config_a = scope.write_config_with_blocks("a", &volume_key, block_provider.as_ref());
    let config_b = scope.write_config_with_blocks("b", &volume_key, block_provider.as_ref());

    let mut mount_a = NativeMount::spawn(
        &scope.cli_binary,
        &config_a,
        &scope.mountpoint_a,
        server.url(),
    );
    mount_a.wait_ready().expect("first PGlite NFS mount");
    let mut mount_b = NativeMount::spawn(
        &scope.cli_binary,
        &config_b,
        &scope.mountpoint_b,
        server.url(),
    );
    mount_b.wait_ready().expect("second PGlite NFS mount");
    assert!(
        mount_a.is_alive() && mount_b.is_alive(),
        "both CLIs must stay mounted"
    );

    let a_payload = b"created on A while both PGlite CLIs were mounted";
    let b_payload = b"created on B while both PGlite CLIs were mounted";
    fs::write(scope.mountpoint_a.join("created-by-a"), a_payload).expect("A creates a file");
    await_bytes(&scope.mountpoint_b.join("created-by-a"), a_payload).expect("B sees A's file");
    fs::write(scope.mountpoint_b.join("created-by-b"), b_payload).expect("B creates a file");
    await_bytes(&scope.mountpoint_a.join("created-by-b"), b_payload).expect("A sees B's file");

    let load_started = Instant::now();
    let acknowledged_calls = run_bounded_load(&scope).expect("two-CLI PGlite load");
    let load_elapsed = load_started.elapsed();
    assert!(
        load_elapsed <= LOAD_DURATION_LIMIT,
        "bounded PGlite load exceeded {LOAD_DURATION_LIMIT:?}: {load_elapsed:?}"
    );
    let load_label = match blocks {
        NativeBlocks::Pglite => "PGLITE_NATIVE_LOAD_PASS",
        NativeBlocks::RustFs => "PGLITE_RUSTFS_NATIVE_LOAD_PASS",
    };
    println!(
        "{load_label} files_per_writer={LOAD_FILES_PER_WRITER} acknowledged_calls={acknowledged_calls} elapsed_ms={}",
        load_elapsed.as_millis()
    );

    let shared_seed = vec![b'.'; 8 * 1024];
    let shared_name = "shared-disjoint-ranges";
    fs::write(scope.mountpoint_a.join(shared_name), &shared_seed).expect("seed common file");
    await_bytes(&scope.mountpoint_b.join(shared_name), &shared_seed).expect("B sees common file");
    let handle_a = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_a.join(shared_name))
        .expect("open common file through A");
    let handle_b = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_b.join(shared_name))
        .expect("open common file through B");
    let first_range = vec![b'1'; 4096];
    let second_range = vec![b'2'; 4096];
    let barrier = Arc::new(Barrier::new(3));
    let first = spawn_range_writer(handle_a, 0, first_range.clone(), Arc::clone(&barrier));
    let second = spawn_range_writer(handle_b, 4096, second_range.clone(), Arc::clone(&barrier));
    barrier.wait();
    first
        .join()
        .expect("join A writer")
        .expect("A first-range write");
    second
        .join()
        .expect("join B writer")
        .expect("B second-range write");
    let mut merged_ranges = first_range;
    merged_ranges.extend_from_slice(&second_range);
    await_bytes(&scope.mountpoint_a.join(shared_name), &merged_ranges)
        .expect("A reads both acknowledged writes");
    await_bytes(&scope.mountpoint_b.join(shared_name), &merged_ranges)
        .expect("B reads both acknowledged writes");

    fs::rename(
        scope.mountpoint_a.join("created-by-a"),
        scope.mountpoint_a.join("renamed-by-a"),
    )
    .expect("A renames a file");
    await_bytes(&scope.mountpoint_b.join("renamed-by-a"), a_payload).expect("B sees A's rename");
    await_absent(&scope.mountpoint_b.join("created-by-a")).expect("B sees old name removed");
    fs::remove_file(scope.mountpoint_b.join("renamed-by-a")).expect("B unlinks renamed file");
    await_absent(&scope.mountpoint_a.join("renamed-by-a")).expect("A sees B's unlink");

    mount_b
        .clean_stop()
        .expect("B clean SIGINT and NFS unmount");
    mount_a
        .clean_stop()
        .expect("A clean SIGINT and NFS unmount");
    assert!(!is_mounted_at(&scope.mountpoint_a).expect("read mount table for A"));
    assert!(!is_mounted_at(&scope.mountpoint_b).expect("read mount table for B"));

    // Reopen the same data directory through a fresh server only after the
    // first socket server has fully shut down. This also checks persistence
    // of the mode marker, metadata revision, and block bytes.
    server
        .clean_stop()
        .expect("first PGlite server clean shutdown");
    let mut restarted = PgliteServer::start(&data_dir);
    restarted
        .wait_ready()
        .expect("restarted PGlite server readiness");
    let mut reopened = NativeMount::spawn(
        &scope.cli_binary,
        &config_a,
        &scope.mountpoint_a,
        restarted.url(),
    );
    reopened.wait_ready().expect("fresh CLI NFS reopen");
    await_bytes(&scope.mountpoint_a.join("created-by-b"), b_payload)
        .expect("fresh process reads B's file");
    await_bytes(&scope.mountpoint_a.join(shared_name), &merged_ranges)
        .expect("fresh process reads both same-file ranges");
    await_absent(&scope.mountpoint_a.join("renamed-by-a")).expect("fresh process sees B's unlink");
    reopened
        .clean_stop()
        .expect("fresh CLI clean SIGINT and unmount");
    restarted
        .clean_stop()
        .expect("restarted PGlite server clean shutdown");
    scope.finish();
}

#[test]
#[ignore = "requires opt-in macOS native NFS and installed tests/pglite Node dependencies"]
fn cli_one_process_pglite_two_mountpoints_share_writable_files() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_PGLITE_TWO_PROCESS");
    let scope = TestScope::new();
    let volume_key = format!("mount-rs/cli-one-process-pglite/{}", scope.run_name());
    let data_dir = scope.root.join("pglite-data");
    fs::create_dir(&data_dir).expect("create run-owned PGlite data directory");
    let mut server = PgliteServer::start(&data_dir);
    server
        .wait_ready()
        .expect("isolated PGlite socket readiness");
    let config = scope.write_config("a", &volume_key);
    let mut cli = NativeMount::spawn_with_also(
        &scope.cli_binary,
        &config,
        &scope.mountpoint_a,
        &scope.mountpoint_b,
        server.url(),
    );
    cli.wait_ready().expect("one CLI mounts both NFS views");
    assert!(
        cli.is_alive(),
        "one CLI must serve both views simultaneously"
    );

    let a_payload = b"one CLI view A writes through shared PGlite";
    let b_payload = b"one CLI view B writes through shared PGlite";
    fs::write(scope.mountpoint_a.join("from-a"), a_payload).expect("A view creates a file");
    await_bytes(&scope.mountpoint_b.join("from-a"), a_payload).expect("B view reads A's file");
    fs::write(scope.mountpoint_b.join("from-b"), b_payload).expect("B view creates a file");
    await_bytes(&scope.mountpoint_a.join("from-b"), b_payload).expect("A view reads B's file");
    fs::rename(
        scope.mountpoint_b.join("from-b"),
        scope.mountpoint_b.join("renamed-from-b"),
    )
    .expect("B view renames its file");
    await_bytes(&scope.mountpoint_a.join("renamed-from-b"), b_payload)
        .expect("A view sees B's rename");

    cli.clean_stop()
        .expect("one CLI clean SIGINT and both unmounts");
    assert!(!is_mounted_at(&scope.mountpoint_a).expect("read A mount table"));
    assert!(!is_mounted_at(&scope.mountpoint_b).expect("read B mount table"));
    server.clean_stop().expect("PGlite server clean shutdown");
    scope.finish();
}

#[test]
#[ignore = "requires opt-in macOS native NFS and installed tests/pglite Node dependencies"]
fn cli_two_process_pglite_fails_closed_when_socket_slots_are_exhausted() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_PGLITE_TWO_PROCESS");
    let scope = TestScope::new();
    let volume_key = format!("mount-rs/cli-pglite-slot-limit/{}", scope.run_name());
    let data_dir = scope.root.join("pglite-data");
    fs::create_dir(&data_dir).expect("create run-owned PGlite data directory");
    // Each splitstore CLI opens separate metadata and block connections.
    // Three slots admit one complete CLI but not both simultaneously.
    let mut server = PgliteServer::start_with_max_connections(&data_dir, 3);
    server
        .wait_ready()
        .expect("limited PGlite server readiness");
    let config_a = scope.write_config("a", &volume_key);
    let config_b = scope.write_config("b", &volume_key);
    let mut mount_a = NativeMount::spawn(
        &scope.cli_binary,
        &config_a,
        &scope.mountpoint_a,
        server.url(),
    );
    mount_a
        .wait_ready()
        .expect("first CLI with two socket slots");
    let first_payload = b"first mount remains usable under a socket slot limit";
    fs::write(scope.mountpoint_a.join("first-survives"), first_payload)
        .expect("first mount writes before competing connection");

    let mut mount_b = NativeMount::spawn(
        &scope.cli_binary,
        &config_b,
        &scope.mountpoint_b,
        server.url(),
    );
    let rejection = mount_b.wait_ready().expect_err(
        "a second two-connection splitstore CLI must fail with only three socket slots",
    );
    assert!(
        rejection.contains("error communicating with the server")
            || rejection.contains("connection closed"),
        "expected a socket transport rejection before NFS mount: {rejection}"
    );
    println!("PGLITE_NATIVE_SOCKET_LIMIT_REJECTION {rejection}");
    assert!(
        !is_mounted_at(&scope.mountpoint_b).expect("read B mount table"),
        "rejected second CLI must not leave a partial NFS mount"
    );
    await_bytes(&scope.mountpoint_a.join("first-survives"), first_payload)
        .expect("first mount remains readable after second CLI fails");
    mount_a
        .clean_stop()
        .expect("first CLI clean SIGINT and unmount");
    server
        .clean_stop()
        .expect("limited PGlite server clean shutdown");
    scope.finish();
}

fn require_opt_in(name: &str) {
    assert_eq!(
        std::env::var(name).ok().as_deref(),
        Some("1"),
        "set {name}=1 to opt into this native acceptance test"
    );
}

fn payload(writer: char, index: usize) -> Vec<u8> {
    format!("{writer}-{index:02}-").repeat(128).into_bytes()
}

fn run_bounded_load(scope: &TestScope) -> io::Result<usize> {
    let barrier = Arc::new(Barrier::new(3));
    let worker = |mountpoint: PathBuf, writer: char, barrier: Arc<Barrier>| {
        thread::spawn(move || -> io::Result<()> {
            barrier.wait();
            for index in 0..LOAD_FILES_PER_WRITER {
                let path = mountpoint.join(format!("load-{writer}-{index:02}"));
                let bytes = payload(writer, index);
                fs::write(&path, &bytes)?;
                if fs::read(&path)? != bytes {
                    return Err(io::Error::other(format!(
                        "self-read mismatch after writing {}",
                        path.display()
                    )));
                }
            }
            Ok(())
        })
    };
    let first = worker(scope.mountpoint_a.clone(), 'a', Arc::clone(&barrier));
    let second = worker(scope.mountpoint_b.clone(), 'b', Arc::clone(&barrier));
    barrier.wait();
    first.join().expect("join A load worker")?;
    second.join().expect("join B load worker")?;
    for index in 0..LOAD_FILES_PER_WRITER {
        let a_name = format!("load-a-{index:02}");
        let b_name = format!("load-b-{index:02}");
        await_bytes(&scope.mountpoint_b.join(&a_name), &payload('a', index))?;
        await_bytes(&scope.mountpoint_a.join(&b_name), &payload('b', index))?;
        fs::remove_file(scope.mountpoint_b.join(a_name))?;
        fs::remove_file(scope.mountpoint_a.join(b_name))?;
    }
    for index in 0..LOAD_FILES_PER_WRITER {
        await_absent(&scope.mountpoint_a.join(format!("load-a-{index:02}")))?;
        await_absent(&scope.mountpoint_b.join(format!("load-b-{index:02}")))?;
    }
    // One write, local read, cross-mount read, and unlink for each file.
    Ok(LOAD_FILES_PER_WRITER * 2 * 4)
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

fn await_bytes(path: &Path, expected: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + VISIBILITY_TIMEOUT;
    loop {
        let observation = match fs::read(path) {
            Ok(actual) if actual == expected => return Ok(()),
            Ok(actual) => format!(
                "read {} bytes, expected {}; actual prefix={:?}",
                actual.len(),
                expected.len(),
                &actual[..actual.len().min(24)]
            ),
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
                    "{} remained visible after unlink",
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
    cli_binary: PathBuf,
    mountpoint_a: PathBuf,
    mountpoint_b: PathBuf,
    armed: bool,
}

impl TestScope {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_nanos();
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mount-rs-cli-pglite-two-process-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create run-owned native test directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("restrict run-owned native test directory");
        let mountpoint_a = root.join("mount-a");
        let mountpoint_b = root.join("mount-b");
        fs::create_dir(&mountpoint_a).expect("create A NFS mountpoint");
        fs::create_dir(&mountpoint_b).expect("create B NFS mountpoint");
        // Pin the exact test-built executable while shared Cargo artifacts
        // may be replaced by another feature build in a separate agent.
        let cli_binary = root.join("mount-rs-test-cli");
        fs::copy(env!("CARGO_BIN_EXE_mount-rs"), &cli_binary)
            .expect("copy this test's CLI binary into owned scope");
        fs::set_permissions(&cli_binary, fs::Permissions::from_mode(0o700))
            .expect("make run-owned test CLI executable");
        Self {
            root,
            cli_binary,
            mountpoint_a,
            mountpoint_b,
            armed: true,
        }
    }

    fn run_name(&self) -> &str {
        self.root
            .file_name()
            .and_then(|name| name.to_str())
            .expect("UTF-8 run-owned test name")
    }

    fn write_config(&self, writer: &str, volume_key: &str) -> PathBuf {
        self.write_config_with_blocks(writer, volume_key, None)
    }

    fn rustfs_blocks_config(&self) -> serde_json::Value {
        let required = |name: &str| {
            let value = std::env::var(name).unwrap_or_else(|_| panic!("missing {name}"));
            assert!(!value.is_empty(), "{name} must not be empty");
            value
        };
        let endpoint = required("RUSTFS_ENDPOINT");
        let port = endpoint
            .strip_prefix("http://127.0.0.1:")
            .and_then(|port| port.parse::<u16>().ok())
            .filter(|port| *port > 0);
        assert!(
            port.is_some(),
            "disposable RustFS endpoint must be loopback HTTP"
        );
        required("RUSTFS_ACCESS_KEY_ID");
        required("RUSTFS_SECRET_ACCESS_KEY");
        let test_prefix = required("RUSTFS_TEST_PREFIX");
        serde_json::json!({
            "kind": "rustfs",
            "endpoint": endpoint,
            "bucket": required("RUSTFS_BUCKET"),
            "region": required("RUSTFS_REGION"),
            "prefix": format!("{}/cli-pglite/{}/blocks", test_prefix.trim_end_matches('/'), self.run_name()),
            "access_key_id": { "env": "RUSTFS_ACCESS_KEY_ID" },
            "secret_access_key": { "env": "RUSTFS_SECRET_ACCESS_KEY" },
            "durable": true
        })
    }

    fn write_config_with_blocks(
        &self,
        writer: &str,
        volume_key: &str,
        blocks: Option<&serde_json::Value>,
    ) -> PathBuf {
        let mountpoint = match writer {
            "a" => &self.mountpoint_a,
            "b" => &self.mountpoint_b,
            _ => panic!("unknown PGlite test writer {writer}"),
        };
        let provider = serde_json::json!({
            "kind": "pglite",
            "connection": { "env": "PGLITE_DATABASE_URL" },
            "volume_key": volume_key,
            "durable": true
        });
        let blocks = blocks.cloned().unwrap_or_else(|| provider.clone());
        let config = serde_json::json!({
            "version": 1,
            "mountpoint": mountpoint,
            "transport": "nfs",
            "driver": {
                "kind": "splitstore",
                "storage": {
                    "concurrent_writes": true,
                    "metadata": provider,
                    "blocks": blocks,
                    "chunk_size_bytes": 4096,
                    "owner": format!("mount-rs-cli-pglite-two-process-{writer}")
                }
            }
        });
        let path = self.root.join(format!("config-{writer}.json"));
        fs::write(
            &path,
            serde_json::to_vec_pretty(&config).expect("serialize PGlite NFS config"),
        )
        .expect("write run-owned PGlite NFS config");
        path
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        let mut errors = Vec::new();
        for mountpoint in [&self.mountpoint_a, &self.mountpoint_b] {
            if let Err(error) = unmount_exact_bounded(mountpoint) {
                errors.push(error);
            }
        }
        if is_mounted_at(&self.mountpoint_a)? || is_mounted_at(&self.mountpoint_b)? {
            return Err(format!(
                "owned NFS mount remains; preserving {}: {}",
                self.root.display(),
                errors.join("; ")
            ));
        }
        fs::remove_dir_all(&self.root)
            .map_err(|error| format!("remove {}: {error}", self.root.display()))?;
        self.armed = false;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn finish(mut self) {
        self.cleanup()
            .expect("clean exact run-owned native artifacts");
    }
}

impl Drop for TestScope {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("PGlite test scope cleanup failed: {error}");
        }
    }
}

struct PgliteServer {
    child: Child,
    url: String,
    ready_line: String,
    lines: Receiver<String>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    output: Vec<String>,
    stopped: bool,
}

impl PgliteServer {
    fn start(data_dir: &Path) -> Self {
        Self::start_with_max_connections(data_dir, 12)
    }

    fn start_with_max_connections(data_dir: &Path, max_connections: usize) -> Self {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/pglite/server.mjs");
        assert!(
            script.is_file(),
            "missing PGlite socket server: {}",
            script.display()
        );
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("reserve loopback PGlite port");
        let port = listener
            .local_addr()
            .expect("read reserved PGlite port")
            .port();
        drop(listener);
        let mut child = Command::new("node")
            .arg(&script)
            .env("PGLITE_PORT", port.to_string())
            .env("PGLITE_DATA_DIR", data_dir)
            .env("PGLITE_MAX_CONNECTIONS", max_connections.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn run-owned PGlite socket server");
        let (sender, lines) = mpsc::channel();
        let stdout_reader = spawn_line_reader(
            child.stdout.take().expect("capture PGlite stdout"),
            "server stdout",
            sender.clone(),
        );
        let stderr_reader = spawn_line_reader(
            child.stderr.take().expect("capture PGlite stderr"),
            "server stderr",
            sender,
        );
        Self {
            child,
            url: format!(
                "postgresql://postgres:postgres@127.0.0.1:{port}/postgres?sslmode=disable"
            ),
            ready_line: format!("PGLITE_READY 127.0.0.1:{port}"),
            lines,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
            output: Vec::new(),
            stopped: false,
        }
    }

    fn url(&self) -> &str {
        &self.url
    }

    fn wait_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + SERVER_READY_TIMEOUT;
        loop {
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    let ready = line.ends_with(&self.ready_line);
                    self.output.push(line);
                    if ready {
                        return Ok(());
                    }
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
            }
            if self
                .child
                .try_wait()
                .map_err(|error| error.to_string())?
                .is_some()
                || Instant::now() >= deadline
            {
                return Err(format!(
                    "owned PGlite socket server did not become ready: {:?}",
                    self.output
                ));
            }
        }
    }

    fn clean_stop(&mut self) -> Result<(), String> {
        self.stop(true)
    }

    fn stop(&mut self, expect_clean: bool) -> Result<(), String> {
        if self.stopped {
            return Ok(());
        }
        if self
            .child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_none()
        {
            // SAFETY: only signal the exact child PID owned by this guard.
            let sent = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
            if sent != 0 {
                return Err(format!(
                    "SIGTERM owned PGlite server: {}",
                    io::Error::last_os_error()
                ));
            }
        }
        let status = wait_child_bounded(&mut self.child, CLEAN_EXIT_TIMEOUT);
        if status.is_none() {
            let _ = self.child.kill();
        }
        let final_status = status.or_else(|| self.child.wait().ok());
        if let Some(reader) = self.stdout_reader.take() {
            reader.join().expect("join PGlite stdout reader");
        }
        if let Some(reader) = self.stderr_reader.take() {
            reader.join().expect("join PGlite stderr reader");
        }
        self.output.extend(self.lines.try_iter());
        self.stopped = true;
        if expect_clean && !final_status.is_some_and(|status| status.success()) {
            return Err(format!(
                "PGlite server did not cleanly stop: status={final_status:?}; output={:?}",
                self.output
            ));
        }
        Ok(())
    }
}

impl Drop for PgliteServer {
    fn drop(&mut self) {
        if let Err(error) = self.stop(false) {
            eprintln!("owned PGlite socket cleanup failed: {error}");
        }
    }
}

struct NativeMount {
    child: Child,
    mountpoint: PathBuf,
    also_mountpoint: Option<PathBuf>,
    lines: Receiver<String>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    output: Vec<String>,
    cleaned: bool,
}

impl NativeMount {
    fn spawn(cli_binary: &Path, config: &Path, mountpoint: &Path, database_url: &str) -> Self {
        Self::spawn_internal(cli_binary, config, mountpoint, None, database_url)
    }

    fn spawn_with_also(
        cli_binary: &Path,
        config: &Path,
        mountpoint: &Path,
        also_mountpoint: &Path,
        database_url: &str,
    ) -> Self {
        Self::spawn_internal(
            cli_binary,
            config,
            mountpoint,
            Some(also_mountpoint),
            database_url,
        )
    }

    fn spawn_internal(
        cli_binary: &Path,
        config: &Path,
        mountpoint: &Path,
        also_mountpoint: Option<&Path>,
        database_url: &str,
    ) -> Self {
        let mut command = Command::new(cli_binary);
        command.args(["mount", "--config"]).arg(config);
        if let Some(also_mountpoint) = also_mountpoint {
            command.arg("--also-mountpoint").arg(also_mountpoint);
        }
        let mut child = command
            .arg("--quiet")
            .env("PGLITE_DATABASE_URL", database_url)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn exact run-owned PGlite CLI child");
        let (sender, lines) = mpsc::channel();
        let stdout_reader = spawn_line_reader(
            child.stdout.take().expect("capture CLI stdout"),
            "CLI stdout",
            sender.clone(),
        );
        let stderr_reader = spawn_line_reader(
            child.stderr.take().expect("capture CLI stderr"),
            "CLI stderr",
            sender,
        );
        Self {
            child,
            mountpoint: mountpoint.to_path_buf(),
            also_mountpoint: also_mountpoint.map(Path::to_path_buf),
            lines,
            stdout_reader: Some(stdout_reader),
            stderr_reader: Some(stderr_reader),
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

    fn wait_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + MOUNT_READY_TIMEOUT;
        let mut reported_ready = false;
        loop {
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    if line.contains("mounted nfs") {
                        reported_ready = true;
                    }
                    self.output.push(line);
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
            }
            let both_ready = match self.also_mountpoint.as_ref() {
                Some(also) => is_mounted_at(also)?,
                None => true,
            };
            if reported_ready && is_mounted_at(&self.mountpoint)? && both_ready {
                return Ok(());
            }
            if !self.is_alive() || Instant::now() >= deadline {
                let _ = self.stop(false);
                return Err(format!(
                    "CLI did not mount {} and {:?} within {MOUNT_READY_TIMEOUT:?}; output={:?}",
                    self.mountpoint.display(),
                    self.also_mountpoint,
                    self.output
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
            // SAFETY: only signal the exact child PID owned by this guard.
            let sent = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
            if sent != 0 {
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
        if let Some(reader) = self.stdout_reader.take() {
            reader.join().expect("join CLI stdout reader");
        }
        if let Some(reader) = self.stderr_reader.take() {
            reader.join().expect("join CLI stderr reader");
        }
        self.output.extend(self.lines.try_iter());
        // Observe the normal exit before any forced exact-path unmount. A
        // successful fallback must not hide a failed CLI unmount.
        let normal_mounted = is_mounted_at(&self.mountpoint);
        let normal_also_mounted = self
            .also_mountpoint
            .as_ref()
            .map(|also| is_mounted_at(also))
            .transpose();
        let fallback_primary = unmount_exact_bounded(&self.mountpoint);
        let fallback_also = self
            .also_mountpoint
            .as_ref()
            .map(|also| unmount_exact_bounded(also))
            .transpose();
        let fallback = fallback_primary.and(fallback_also.map(|_| ()));
        self.cleaned = fallback.is_ok();
        if expect_clean {
            let status =
                final_status.ok_or_else(|| "owned CLI child had no exit status".to_owned())?;
            let normal_mounted = normal_mounted?;
            let normal_also_mounted = normal_also_mounted?.unwrap_or(false);
            if !status.success()
                || normal_mounted
                || normal_also_mounted
                || !self.output.iter().any(|line| line.contains("unmounted"))
            {
                return Err(format!(
                    "CLI exited {status} without clean unmount; normal NFS entries mounted=({normal_mounted}, {normal_also_mounted}); output={:?}; exact fallback={fallback:?}",
                    self.output
                ));
            }
        }
        fallback
    }
}

impl Drop for NativeMount {
    fn drop(&mut self) {
        if let Err(error) = self.stop(false) {
            eprintln!("owned PGlite CLI child cleanup failed: {error}");
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
            Ok(None) if Instant::now() >= deadline => return None,
            Ok(None) => thread::sleep(Duration::from_millis(100)),
            Err(_) => return None,
        }
    }
}

fn unmount_exact_bounded(mountpoint: &Path) -> Result<(), String> {
    if !is_mounted_at(mountpoint)? {
        return Ok(());
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
            "exact-path NFS mount {} remains after bounded umount",
            mountpoint.display()
        ))
    } else {
        Ok(())
    }
}

fn is_mounted_at(target: &Path) -> Result<bool, String> {
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
