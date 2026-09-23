//! Opt-in macOS acceptance for two independent writers on one FoundationDB volume.
//!
//! Run only against an owned, disposable local cluster with a native arm64
//! `libfdb_c.dylib`. The test owns its CLI processes, config files, and exact
//! NFS mountpoints; the FoundationDB cluster itself belongs to the caller.

#![cfg(all(target_os = "macos", target_arch = "aarch64", feature = "foundationdb"))]

use std::fs;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
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
const RUSTFS_LOAD_FILES_PER_WRITER: usize = 20;

#[test]
fn test_scope_preserves_root_for_symlinked_mountpoint() {
    let mut scope = TestScope::new();
    let unrelated = scope.root.with_extension("unrelated-symlink-target");
    fs::create_dir(&unrelated).expect("create unrelated dry target");
    fs::remove_dir(&scope.mountpoint_a).expect("remove empty dry A mountpoint");
    symlink(&unrelated, &scope.mountpoint_a).expect("redirect dry A mountpoint");

    let result = scope.cleanup();
    let preserved = scope.root.exists();
    if preserved {
        fs::remove_file(&scope.mountpoint_a).expect("remove dry symlink");
        fs::create_dir(&scope.mountpoint_a).expect("restore run-owned dry mountpoint");
        scope.cleanup().expect("remove safe dry scope");
    }
    fs::remove_dir(&unrelated).expect("remove unrelated dry target");
    assert!(
        result.is_err(),
        "symlinked mountpoint was accepted for cleanup"
    );
    assert!(preserved, "uncertain owned root must remain available");
}

#[test]
#[ignore = "requires opt-in native NFS, an owned disposable FoundationDB cluster, and native libfdb_c"]
fn cli_two_process_foundationdb_volume_stays_coherent_and_reopens() {
    run_two_process(false);
}

#[test]
#[ignore = "requires opt-in native NFS, disposable FoundationDB and RustFS services, and native libfdb_c"]
fn cli_two_process_foundationdb_rustfs_volume_stays_coherent_and_reopens() {
    run_two_process(true);
}

fn run_two_process(rustfs_blocks: bool) {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_FOUNDATIONDB_TWO_PROCESS");
    require_opt_in("MOUNT_RS_FOUNDATIONDB_DISPOSABLE_CLUSTER");
    if rustfs_blocks {
        require_opt_in("MOUNT_RS_CLI_NATIVE_RUSTFS_DISPOSABLE");
    }
    let cluster_file = PathBuf::from(
        std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE")
            .expect("set MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE to the disposable cluster file"),
    );
    let cluster_description =
        fs::read_to_string(&cluster_file).expect("read disposable FoundationDB cluster file");
    assert!(
        cluster_description.contains("@127.0.0.1:"),
        "two-process native acceptance requires a loopback disposable cluster"
    );

    let mut scope = TestScope::new();
    let volume_key = format!("mount-rs/cli-two-process/{}", scope.run_name());
    let blocks = if rustfs_blocks {
        rustfs_blocks_config(&format!("{volume_key}/rustfs-blocks"))
    } else {
        serde_json::json!({
            "kind": "foundationdb",
            "cluster_file": cluster_file,
            "volume_key": volume_key,
            "durable": true,
            "lease_authority": "revision-cas"
        })
    };
    let config_a = scope.write_config("a", &cluster_file, &volume_key, &blocks);
    let config_b = scope.write_config("b", &cluster_file, &volume_key, &blocks);

    // Both children stay alive throughout the bidirectional I/O below. The
    // second mount must open the same durable namespace while A is mounted.
    let mut mount_a = NativeMount::spawn(&scope.cli_binary, &config_a, &scope.mountpoint_a);
    mount_a.wait_ready().expect("first FoundationDB NFS mount");
    let mut mount_b = NativeMount::spawn(&scope.cli_binary, &config_b, &scope.mountpoint_b);
    mount_b.wait_ready().expect("second FoundationDB NFS mount");
    assert!(mount_a.is_alive(), "A exited before shared-volume I/O");
    assert!(mount_b.is_alive(), "B exited before shared-volume I/O");

    let a_created = "created-by-a";
    let b_created = "created-by-b";
    let a_payload = b"A created this file while B was mounted";
    let b_payload = b"B created this file while A was mounted";
    fs::write(scope.mountpoint_a.join(a_created), a_payload).expect("A creates a file");
    await_bytes(&scope.mountpoint_b.join(a_created), a_payload)
        .expect("B sees A's published file and bytes");
    fs::write(scope.mountpoint_b.join(b_created), b_payload).expect("B creates a file");
    await_bytes(&scope.mountpoint_a.join(b_created), b_payload)
        .expect("A sees B's published file and bytes");

    // Start both writes together. Distinct files exercise two concurrent
    // namespace publications; each writer's result must survive the other.
    let barrier = Arc::new(Barrier::new(3));
    let payload_a = vec![b'A'; 8 * 1024];
    let payload_b = vec![b'B'; 8 * 1024];
    let writer_a = spawn_writer(
        scope.mountpoint_a.join("concurrent-a"),
        payload_a.clone(),
        Arc::clone(&barrier),
    );
    let writer_b = spawn_writer(
        scope.mountpoint_b.join("concurrent-b"),
        payload_b.clone(),
        Arc::clone(&barrier),
    );
    barrier.wait();
    writer_a
        .join()
        .expect("join A writer")
        .expect("A concurrent write");
    writer_b
        .join()
        .expect("join B writer")
        .expect("B concurrent write");
    for mountpoint in [&scope.mountpoint_a, &scope.mountpoint_b] {
        await_bytes(&mountpoint.join("concurrent-a"), &payload_a)
            .expect("A concurrent file survives both publications");
        await_bytes(&mountpoint.join("concurrent-b"), &payload_b)
            .expect("B concurrent file survives both publications");
    }

    // Open one common file through both mounts before the writes begin. Each
    // handle updates a disjoint range, so a stale whole-file publication must
    // rebase or merge rather than erase the other range.
    let shared_name = "shared-disjoint-ranges";
    let shared_seed = vec![b'.'; 8 * 1024];
    fs::write(scope.mountpoint_a.join(shared_name), &shared_seed).expect("seed shared file");
    await_bytes(&scope.mountpoint_b.join(shared_name), &shared_seed)
        .expect("B sees common seed before opening its handle");
    let handle_a = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_a.join(shared_name))
        .expect("open shared file through A");
    let handle_b = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_b.join(shared_name))
        .expect("open shared file through B");
    let shared_times_before_a = diagnostic_file_times(&scope.mountpoint_a.join(shared_name));
    let shared_times_before_b = diagnostic_file_times(&scope.mountpoint_b.join(shared_name));
    mount_a.drain_output();
    mount_b.drain_output();
    let shared_trace_start_a = mount_a.output.len();
    let shared_trace_start_b = mount_b.output.len();
    let first_range = vec![b'1'; 4096];
    let second_range = vec![b'2'; 4096];
    let barrier = Arc::new(Barrier::new(3));
    let range_a = spawn_range_writer(handle_a, 0, first_range.clone(), Arc::clone(&barrier));
    let range_b = spawn_range_writer(handle_b, 4096, second_range.clone(), Arc::clone(&barrier));
    barrier.wait();
    range_a
        .join()
        .expect("join A range writer")
        .expect("A writes first range");
    range_b
        .join()
        .expect("join B range writer")
        .expect("B writes second range");
    let shared_times_after_a = diagnostic_file_times(&scope.mountpoint_a.join(shared_name));
    let shared_times_after_b = diagnostic_file_times(&scope.mountpoint_b.join(shared_name));
    let mut merged_ranges = first_range;
    merged_ranges.extend_from_slice(&second_range);
    let live_a = await_bytes(&scope.mountpoint_a.join(shared_name), &merged_ranges);
    let live_b = await_bytes(&scope.mountpoint_b.join(shared_name), &merged_ranges);
    if live_a.is_err() || live_b.is_err() {
        let observed_a = describe_disjoint_ranges(&scope.mountpoint_a.join(shared_name));
        let observed_b = describe_disjoint_ranges(&scope.mountpoint_b.join(shared_name));
        let stop_b = mount_b.clean_stop();
        let b_request_log =
            mount_b.describe_shared_requests_since(shared_trace_start_b, shared_name);
        let stop_a = mount_a.clean_stop();
        let a_request_log =
            mount_a.describe_shared_requests_since(shared_trace_start_a, shared_name);
        let (reopen, durable, reopen_stop) = if stop_a.is_ok() && stop_b.is_ok() {
            let mut fresh = NativeMount::spawn(&scope.cli_binary, &config_a, &scope.mountpoint_a);
            let ready = fresh.wait_ready();
            let durable = if ready.is_ok() {
                describe_disjoint_ranges(&scope.mountpoint_a.join(shared_name))
            } else {
                "fresh process did not mount".to_owned()
            };
            let stop = fresh.clean_stop();
            (ready, durable, stop)
        } else {
            (
                Err("fresh reopen skipped after failed unmount".to_owned()),
                "not read".to_owned(),
                Ok(()),
            )
        };
        let cleanup = scope.cleanup();
        panic!(
            "both acknowledged same-file writes must preserve disjoint ranges: live A={live_a:?}, live B={live_b:?}; A bytes={observed_a}; B bytes={observed_b}; A times before={shared_times_before_a} after={shared_times_after_a}; B times before={shared_times_before_b} after={shared_times_after_b}; A requests={a_request_log}; B requests={b_request_log}; normal SIGINT A={stop_a:?}, B={stop_b:?}; fresh reopen={reopen:?}, durable bytes={durable}, fresh SIGINT={reopen_stop:?}, cleanup={cleanup:?}"
        );
    }

    if rustfs_blocks {
        // Both independent CLI processes publish separate namespace and block
        // updates concurrently. Fresh reopen below checks every acknowledged sync.
        let barrier = Arc::new(Barrier::new(3));
        let started = Instant::now();
        let load_a = spawn_load_writer(
            scope.mountpoint_a.clone(),
            'a',
            RUSTFS_LOAD_FILES_PER_WRITER,
            Arc::clone(&barrier),
        );
        let load_b = spawn_load_writer(
            scope.mountpoint_b.clone(),
            'b',
            RUSTFS_LOAD_FILES_PER_WRITER,
            Arc::clone(&barrier),
        );
        barrier.wait();
        let a_acks = load_a
            .join()
            .expect("join RustFS load writer A")
            .expect("A load writes");
        let b_acks = load_b
            .join()
            .expect("join RustFS load writer B")
            .expect("B load writes");
        assert_eq!(a_acks + b_acks, 2 * RUSTFS_LOAD_FILES_PER_WRITER);
        for mountpoint in [&scope.mountpoint_a, &scope.mountpoint_b] {
            verify_load_files(mountpoint).expect("both live RustFS mounts see every load write");
        }
        assert!(
            mount_a.is_alive() && mount_b.is_alive(),
            "both load CLIs must remain mounted"
        );
        println!(
            "NATIVE_FDB_RUSTFS_LOAD_PASS acknowledgements={} elapsed_ms={}",
            a_acks + b_acks,
            started.elapsed().as_millis()
        );
    }

    let nfs_options_before_rename = describe_nfs_mounts(&scope);
    let directory_times_before_a = diagnostic_file_times(&scope.mountpoint_a);
    let directory_times_before_b = diagnostic_file_times(&scope.mountpoint_b);
    mount_b.drain_output();
    let rename_trace_start_b = mount_b.output.len();
    fs::rename(
        scope.mountpoint_a.join(a_created),
        scope.mountpoint_a.join("renamed-by-a"),
    )
    .expect("A renames its file");
    let directory_times_after_a = diagnostic_file_times(&scope.mountpoint_a);
    let directory_times_after_b = diagnostic_file_times(&scope.mountpoint_b);
    let live_rename = await_bytes(&scope.mountpoint_b.join("renamed-by-a"), a_payload);
    if live_rename.is_err() {
        mount_b.drain_output();
        let rename_poll_end_b = mount_b.output.len();
        let a_names = describe_directory(&scope.mountpoint_a);
        let b_names = describe_directory(&scope.mountpoint_b);
        let b_new_after_readdir = fs::read(scope.mountpoint_b.join("renamed-by-a"));
        let b_old_after_readdir = fs::read(scope.mountpoint_b.join(a_created));
        let nfs_options_after_rename = describe_nfs_mounts(&scope);
        let stop_b = mount_b.clean_stop();
        let b_lookup_log =
            mount_b.describe_rename_requests_between(rename_trace_start_b, rename_poll_end_b);
        let b_readdir_log =
            mount_b.describe_rename_requests_between(rename_poll_end_b, mount_b.output.len());
        let stop_a = mount_a.clean_stop();
        let (reopen, durable_new, durable_old, reopen_stop) = if stop_a.is_ok() && stop_b.is_ok() {
            let mut fresh = NativeMount::spawn(&scope.cli_binary, &config_a, &scope.mountpoint_a);
            let ready = fresh.wait_ready();
            let durable_new = if ready.is_ok() {
                await_bytes(&scope.mountpoint_a.join("renamed-by-a"), a_payload)
            } else {
                Err(io::Error::other("fresh process did not mount"))
            };
            let durable_old = if ready.is_ok() {
                await_absent(&scope.mountpoint_a.join(a_created))
            } else {
                Err(io::Error::other("fresh process did not mount"))
            };
            let stop = fresh.clean_stop();
            (ready, durable_new, durable_old, stop)
        } else {
            (
                Err("fresh reopen skipped after failed unmount".to_owned()),
                Err(io::Error::other("not read")),
                Err(io::Error::other("not read")),
                Ok(()),
            )
        };
        let cleanup = scope.cleanup();
        panic!(
            "B must see A's acknowledged rename: live lookup={live_rename:?}; A directory={a_names}; B directory={b_names}; B new after READDIR={b_new_after_readdir:?}; B old after READDIR={b_old_after_readdir:?}; A root times before={directory_times_before_a} after={directory_times_after_a}; B root times before={directory_times_before_b} after={directory_times_after_b}; NFS options before={nfs_options_before_rename}; NFS options after={nfs_options_after_rename}; B poll requests={b_lookup_log}; B READDIR/after requests={b_readdir_log}; normal SIGINT A={stop_a:?}, B={stop_b:?}; fresh reopen={reopen:?}, durable new={durable_new:?}, durable old absent={durable_old:?}, fresh SIGINT={reopen_stop:?}, cleanup={cleanup:?}"
        );
    }
    await_absent(&scope.mountpoint_b.join(a_created)).expect("B no longer sees old name");
    fs::remove_file(scope.mountpoint_b.join("renamed-by-a")).expect("B removes renamed file");
    await_absent(&scope.mountpoint_a.join("renamed-by-a")).expect("A sees B's remove");

    let stop_b = mount_b.clean_stop();
    let stop_a = mount_a.clean_stop();
    assert!(stop_b.is_ok(), "B did not cleanly unmount: {stop_b:?}");
    assert!(stop_a.is_ok(), "A did not cleanly unmount: {stop_a:?}");
    assert!(!is_mounted_at(&scope.mountpoint_a).expect("read mount table after A stops"));
    assert!(!is_mounted_at(&scope.mountpoint_b).expect("read mount table after B stops"));

    // The first CLI child is gone. A third process must reopen the same
    // provider-backed bytes and namespace through a fresh kernel mount.
    let mut reopened = NativeMount::spawn(&scope.cli_binary, &config_a, &scope.mountpoint_a);
    reopened
        .wait_ready()
        .expect("fresh FoundationDB NFS reopen");
    await_bytes(&scope.mountpoint_a.join(b_created), b_payload)
        .expect("fresh process sees B's file");
    await_bytes(&scope.mountpoint_a.join("concurrent-a"), &payload_a)
        .expect("fresh process sees A's concurrent file");
    await_bytes(&scope.mountpoint_a.join("concurrent-b"), &payload_b)
        .expect("fresh process sees B's concurrent file");
    await_bytes(&scope.mountpoint_a.join(shared_name), &merged_ranges)
        .expect("fresh process sees both disjoint ranges");
    if rustfs_blocks {
        verify_load_files(&scope.mountpoint_a)
            .expect("fresh process preserves every acknowledged RustFS load file");
    }
    await_absent(&scope.mountpoint_a.join("renamed-by-a")).expect("fresh process sees B's removal");
    reopened
        .clean_stop()
        .expect("fresh process clean SIGINT/unmount");
    scope.finish();
}

fn require_opt_in(name: &str) {
    assert_eq!(
        std::env::var(name).ok().as_deref(),
        Some("1"),
        "set {name}=1 to opt into this native acceptance test"
    );
}

fn rustfs_blocks_config(prefix: &str) -> serde_json::Value {
    let endpoint = std::env::var("RUSTFS_ENDPOINT").expect("set the disposable RustFS endpoint");
    assert!(
        endpoint.starts_with("http://127.0.0.1:") || endpoint.starts_with("http://localhost:"),
        "native RustFS acceptance requires a loopback disposable endpoint"
    );
    let bucket = std::env::var("RUSTFS_BUCKET").expect("set RUSTFS_BUCKET");
    let region = std::env::var("RUSTFS_REGION").expect("set RUSTFS_REGION");
    for name in ["RUSTFS_ACCESS_KEY_ID", "RUSTFS_SECRET_ACCESS_KEY"] {
        assert!(
            std::env::var(name).is_ok_and(|value| !value.trim().is_empty()),
            "set {name} for the disposable RustFS service"
        );
    }
    serde_json::json!({
        "kind": "rustfs",
        "endpoint": endpoint,
        "bucket": bucket,
        "region": region,
        "prefix": prefix,
        "access_key_id": { "env": "RUSTFS_ACCESS_KEY_ID" },
        "secret_access_key": { "env": "RUSTFS_SECRET_ACCESS_KEY" },
        "durable": true
    })
}

fn spawn_writer(
    path: PathBuf,
    bytes: Vec<u8>,
    barrier: Arc<Barrier>,
) -> JoinHandle<io::Result<()>> {
    thread::spawn(move || {
        barrier.wait();
        fs::write(path, bytes)
    })
}

fn load_payload(writer: char, index: usize) -> Vec<u8> {
    let mut bytes = vec![writer as u8; 4096];
    let identity = format!("{writer}:{index:02}");
    bytes[..identity.len()].copy_from_slice(identity.as_bytes());
    bytes
}

fn load_name(writer: char, index: usize) -> String {
    format!("load-{writer}-{index:02}")
}

fn spawn_load_writer(
    mountpoint: PathBuf,
    writer: char,
    count: usize,
    barrier: Arc<Barrier>,
) -> JoinHandle<io::Result<usize>> {
    thread::spawn(move || {
        barrier.wait();
        for index in 0..count {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(mountpoint.join(load_name(writer, index)))?;
            file.write_all(&load_payload(writer, index))?;
            file.sync_all()?;
        }
        Ok(count)
    })
}

fn verify_load_files(mountpoint: &Path) -> io::Result<()> {
    for writer in ['a', 'b'] {
        for index in 0..RUSTFS_LOAD_FILES_PER_WRITER {
            await_bytes(
                &mountpoint.join(load_name(writer, index)),
                &load_payload(writer, index),
            )?;
        }
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

fn await_bytes(path: &Path, expected: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + VISIBILITY_TIMEOUT;
    loop {
        let observation = match fs::read(path) {
            Ok(actual) if actual == expected => return Ok(()),
            Ok(actual) => {
                let first_mismatch = actual
                    .iter()
                    .zip(expected)
                    .position(|(actual, expected)| actual != expected);
                format!(
                    "read {} bytes; first mismatch={first_mismatch:?}; actual prefix={:?}; expected prefix={:?}",
                    actual.len(),
                    &actual[..actual.len().min(16)],
                    &expected[..expected.len().min(16)]
                )
            }
            Err(error) => error.to_string(),
        };
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!(
                "{} did not become visible with expected bytes; last observation: {observation}",
                path.display()
            )));
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn describe_disjoint_ranges(path: &Path) -> String {
    match fs::read(path) {
        Ok(bytes) => {
            let first_ones = bytes
                .iter()
                .take(4096)
                .filter(|byte| **byte == b'1')
                .count();
            let second_twos = bytes
                .iter()
                .skip(4096)
                .take(4096)
                .filter(|byte| **byte == b'2')
                .count();
            let prefix = &bytes[..bytes.len().min(16)];
            let middle = bytes.get(4096..4112).unwrap_or(&[]);
            format!(
                "length={} first-range-ones={first_ones}/4096 second-range-twos={second_twos}/4096 prefix={prefix:?} offset-4096={middle:?}",
                bytes.len()
            )
        }
        Err(error) => format!("read failed: {error}"),
    }
}

fn diagnostic_file_times(path: &Path) -> String {
    if std::env::var("MOUNT_RS_CLI_NATIVE_FDB_TRACE")
        .ok()
        .as_deref()
        != Some("1")
    {
        return "trace disabled".to_owned();
    }
    match fs::metadata(path) {
        Ok(metadata) => format!(
            "inode={} mtime={}.{:09} ctime={}.{:09} size={}",
            metadata.ino(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
            metadata.size()
        ),
        Err(error) => format!("metadata failed: {error}"),
    }
}

fn describe_directory(path: &Path) -> String {
    let mut names = match fs::read_dir(path) {
        Ok(entries) => entries
            .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
            .collect::<io::Result<Vec<_>>>(),
        Err(error) => return format!("READDIR failed: {error}"),
    };
    match names.as_mut() {
        Ok(names) => {
            names.sort();
            format!("READDIR names={names:?}")
        }
        Err(error) => format!("READDIR entry failed: {error}"),
    }
}

fn describe_nfs_mounts(scope: &TestScope) -> String {
    let output = match Command::new("nfsstat").arg("-m").output() {
        Ok(output) if output.status.success() => output,
        Ok(output) => return format!("nfsstat -m exited {}", output.status),
        Err(error) => return format!("nfsstat -m failed: {error}"),
    };
    let table = String::from_utf8_lossy(&output.stdout);
    let lines = table.lines().collect::<Vec<_>>();
    let mut matching = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if line.contains(scope.run_name()) {
            matching.extend(lines.iter().skip(index).take(6).copied());
        }
    }
    format!("{matching:?}")
}

fn await_absent(path: &Path) -> io::Result<()> {
    let deadline = Instant::now() + VISIBILITY_TIMEOUT;
    loop {
        match fs::metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Ok(_) if Instant::now() < deadline => {}
            Err(_) if Instant::now() < deadline => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "{} remained visible after removal",
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
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mount-rs-cli-two-process-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create run-owned native test directory");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("restrict run-owned native test directory");
        let root_metadata =
            fs::symlink_metadata(&root).expect("stat run-owned native test directory");
        let root_identity = (root_metadata.dev(), root_metadata.ino());
        let mountpoint_a = root.join("mount-a");
        let mountpoint_b = root.join("mount-b");
        fs::create_dir(&mountpoint_a).expect("create A NFS mountpoint");
        fs::create_dir(&mountpoint_b).expect("create B NFS mountpoint");
        // Cargo's shared target can be rewritten by an unrelated feature-off
        // build while this test's children are still mounted. Pin exactly the
        // feature-on executable built for this test through fresh reopen.
        let cli_binary = root.join("mount-rs-feature-on-test-cli");
        fs::copy(env!("CARGO_BIN_EXE_mount-rs"), &cli_binary)
            .expect("copy this test's feature-on CLI into its run-owned scope");
        fs::set_permissions(&cli_binary, fs::Permissions::from_mode(0o700))
            .expect("make run-owned test CLI executable");
        Self {
            root,
            root_identity,
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
            .expect("UTF-8 unique test run name")
    }

    fn write_config(
        &self,
        writer: &str,
        cluster_file: &Path,
        volume_key: &str,
        blocks: &serde_json::Value,
    ) -> PathBuf {
        let mountpoint = match writer {
            "a" => &self.mountpoint_a,
            "b" => &self.mountpoint_b,
            _ => panic!("unknown writer"),
        };
        let metadata = serde_json::json!({
            "kind": "foundationdb",
            "cluster_file": cluster_file,
            "volume_key": volume_key,
            "durable": true,
            "lease_authority": "revision-cas"
        });
        let path = self.root.join(format!("config-{writer}.json"));
        let config = serde_json::json!({
            "version": 1,
            "mountpoint": mountpoint,
            "transport": "nfs",
            "driver": {
                "kind": "splitstore",
                "storage": {
                    // This is the sole test-side schema knob for the
                    // manifest-revision CAS concurrent-volume mode.
                    "concurrent_writes": true,
                    "metadata": metadata,
                    "blocks": blocks,
                    "chunk_size_bytes": 4096,
                    "owner": format!("mount-rs-cli-two-process-{writer}")
                }
            }
        });
        fs::write(
            &path,
            serde_json::to_vec_pretty(&config).expect("serialize two-process FDB config"),
        )
        .expect("write run-owned FoundationDB NFS config");
        path
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        if checked_plain_directory(&self.root)? != self.root_identity {
            return Err(format!(
                "run-owned native root changed; preserving {}",
                self.root.display()
            ));
        }
        // Reject a redirected endpoint before looking up or unmounting either.
        checked_plain_directory(&self.mountpoint_a)?;
        checked_plain_directory(&self.mountpoint_b)?;
        let mut errors = Vec::new();
        for target in [&self.mountpoint_a, &self.mountpoint_b] {
            if let Err(error) = unmount_exact_bounded(target) {
                errors.push(error);
            }
        }
        // A failed mount-table read is not proof that an NFS mount detached.
        // Preserve the exact run-owned tree until both entries can be checked.
        if is_mounted_at(&self.mountpoint_a)? || is_mounted_at(&self.mountpoint_b)? {
            return Err(format!(
                "run-owned NFS mount remains; preserving {}; cleanup errors: {}",
                self.root.display(),
                errors.join("; ")
            ));
        }
        if checked_plain_directory(&self.root)? != self.root_identity {
            return Err(format!(
                "run-owned native root changed before removal; preserving {}",
                self.root.display()
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
            .expect("clean run-owned native test artifacts");
    }
}

impl Drop for TestScope {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("native test scope cleanup failed: {error}");
        }
    }
}

struct NativeMount {
    child: Child,
    mountpoint: PathBuf,
    lines: Receiver<String>,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    output: Vec<String>,
    cleaned: bool,
}

impl NativeMount {
    fn spawn(cli_binary: &Path, config: &Path, mountpoint: &Path) -> Self {
        let trace_native = std::env::var("MOUNT_RS_CLI_NATIVE_FDB_TRACE")
            .ok()
            .as_deref()
            == Some("1");
        let mut command = Command::new(cli_binary);
        // macOS strips DYLD_LIBRARY_PATH when Cargo crosses its /bin/sh
        // wrapper. Supply the owned native client directory at the final,
        // unprotected CLI child boundary instead.
        if let Some(library_dir) = std::env::var_os("MOUNT_RS_NATIVE_FDB_CLIENT_LIB_DIR") {
            command.env("DYLD_LIBRARY_PATH", library_dir);
        }
        command.args(["mount", "--config"]).arg(config);
        if trace_native {
            command.arg("--verbose");
        } else {
            command.arg("--quiet");
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn run-owned FoundationDB NFS CLI child");
        let (sender, lines) = mpsc::channel();
        let stdout_reader = spawn_line_reader(
            child.stdout.take().expect("capture CLI stdout"),
            "stdout",
            sender.clone(),
        );
        let stderr_reader = spawn_line_reader(
            child.stderr.take().expect("capture CLI stderr"),
            "stderr",
            sender,
        );
        Self {
            child,
            mountpoint: mountpoint.to_path_buf(),
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
            .expect("poll owned CLI child")
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
            if reported_ready && is_mounted_at(&self.mountpoint)? {
                return Ok(());
            }
            if !self.is_alive() || Instant::now() >= deadline {
                let _ = self.stop(false);
                return Err(format!(
                    "CLI did not mount {} within {MOUNT_READY_TIMEOUT:?}; output={:?}",
                    self.mountpoint.display(),
                    self.output
                ));
            }
        }
    }

    fn clean_stop(&mut self) -> Result<(), String> {
        self.stop(true)
    }

    fn drain_output(&mut self) {
        self.output.extend(self.lines.try_iter());
    }

    fn describe_rename_requests_between(&self, start: usize, end: usize) -> String {
        let lines = &self.output[start..end];
        let count = |operation: &str, path: &str| {
            lines
                .iter()
                .filter(|line| {
                    let mut tokens = line.split_whitespace();
                    tokens.nth(2) == Some(operation) && tokens.next() == Some(path)
                })
                .count()
        };
        let new_lstat = lines
            .iter()
            .filter(|line| {
                let mut tokens = line.split_whitespace();
                tokens.nth(2) == Some("lstat") && tokens.next() == Some("/renamed-by-a")
            })
            .collect::<Vec<_>>();
        let new_tail = new_lstat
            .iter()
            .rev()
            .take(5)
            .rev()
            .copied()
            .collect::<Vec<_>>();
        format!(
            "total={} root-lstat={} root-readdir={} new-lstat={} new-ENOENT={} old-lstat={} new-tail={new_tail:?}",
            lines.len(),
            count("lstat", "/"),
            count("readdir", "/"),
            new_lstat.len(),
            new_lstat
                .iter()
                .filter(|line| line.contains("ENOENT"))
                .count(),
            count("lstat", "/created-by-a")
        )
    }

    fn describe_shared_requests_since(&self, start: usize, name: &str) -> String {
        let file_lines = self.output[start..]
            .iter()
            .filter(|line| line.contains(name))
            .collect::<Vec<_>>();
        let operation_count = |operation: &str| {
            file_lines
                .iter()
                .filter(|line| line.split_whitespace().nth(2) == Some(operation))
                .count()
        };
        let read_lines = file_lines
            .iter()
            .filter(|line| line.split_whitespace().nth(2) == Some("read"))
            .copied()
            .collect::<Vec<_>>();
        let read_tail = read_lines
            .iter()
            .rev()
            .take(5)
            .rev()
            .copied()
            .collect::<Vec<_>>();
        let file_tail = file_lines
            .iter()
            .rev()
            .take(8)
            .rev()
            .copied()
            .collect::<Vec<_>>();
        format!(
            "post-write file requests total={} read={} lstat={} stat={} write={} sync={} read-tail={read_tail:?} file-tail={file_tail:?}",
            file_lines.len(),
            operation_count("read"),
            operation_count("lstat"),
            operation_count("stat"),
            operation_count("write"),
            operation_count("sync")
        )
    }

    fn stop(&mut self, expect_clean: bool) -> Result<(), String> {
        if self.cleaned {
            return Ok(());
        }
        if self.is_alive() {
            // SAFETY: this is the exact child PID owned by this guard.
            let sent = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
            if sent != 0 {
                eprintln!(
                    "SIGINT failed for owned CLI child: {}",
                    io::Error::last_os_error()
                );
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
        // Inspect the kernel entry before the bounded fallback helper. A
        // forced fallback may clean up a failed CLI unmount, but it must not
        // turn that failure into a passing normal SIGINT assertion.
        let normal_mounted = is_mounted_at(&self.mountpoint);
        let unmount = unmount_exact_bounded(&self.mountpoint);
        self.cleaned = unmount.is_ok();
        if expect_clean {
            let status =
                final_status.ok_or_else(|| "owned CLI child had no exit status".to_owned())?;
            let normal_mounted = normal_mounted?;
            if !status.success()
                || normal_mounted
                || !self.output.iter().any(|line| line.contains("unmounted"))
            {
                return Err(format!(
                    "CLI exited {status} without clean unmount; normal NFS entry mounted={normal_mounted}; output={:?}; exact fallback={unmount:?}",
                    self.output
                ));
            }
        }
        unmount
    }
}

impl Drop for NativeMount {
    fn drop(&mut self) {
        if let Err(error) = self.stop(false) {
            eprintln!("run-owned CLI child cleanup failed: {error}");
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
        return Err(format!(
            "run-owned NFS mountpoint {} changed before umount",
            mountpoint.display()
        ));
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
        .map_err(|error| format!("stat run-owned NFS path {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "run-owned NFS path {} is a symlink; preserving owned root",
            path.display()
        ));
    }
    if !metadata.file_type().is_dir() {
        return Err(format!(
            "run-owned NFS path {} is not a directory; preserving owned root",
            path.display()
        ));
    }
    Ok((metadata.dev(), metadata.ino()))
}
