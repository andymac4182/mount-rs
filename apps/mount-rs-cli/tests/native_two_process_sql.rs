//! Opt-in macOS acceptance for two CLI processes sharing local SQLite backing.
//!
//! These metadata/block SQLite databases are local to the host. The NFS mounts
//! are two independent native client/server lifecycles. A separate diagnostic
//! opens a SQLite application database *inside* both NFS views; that lock
//! probe does not certify SQLite-over-NFS safety and never commits after a
//! detected lock bypass.

#![cfg(target_os = "macos")]

use std::ffi::{CStr, CString};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Barrier};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const READY_TIMEOUT: Duration = Duration::from_secs(40);
const VISIBILITY_TIMEOUT: Duration = Duration::from_secs(30);
const CLEAN_EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(15);
const PYTHON_TIMEOUT: Duration = Duration::from_secs(30);
const LOAD_ROUNDS: usize = 12;

#[test]
#[ignore = "requires opt-in native macOS NFS and disposable local SQLite backing"]
fn two_cli_processes_share_sqlite_backing_and_reopen() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_SQLITE_TWO_PROCESS");
    run_journal("DELETE");
    if std::env::var("MOUNT_RS_CLI_NATIVE_SQLITE_WAL")
        .ok()
        .as_deref()
        == Some("1")
    {
        run_journal("WAL");
    }
}

#[test]
#[ignore = "requires opt-in one-CLI/two-view native macOS NFS with disposable local SQLite backing"]
fn one_cli_process_serves_two_writable_sqlite_views() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_SQLITE_ONE_PROCESS");
    run_one_process_two_views();
}

#[test]
#[ignore = "diagnostic only: disposable SQLite backing databases seen through two NFS views"]
fn sqlite_backing_files_on_nfs_are_unsupported() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_SQLITE_NFS_BACKING");
    run_nfs_backing_diagnostic();
}

#[test]
#[ignore = "requires opt-in native macOS NFS with local metadata and disposable NFS SQLite blocks"]
fn sqlite_blocks_on_nfs_with_local_metadata_are_unsupported() {
    require_opt_in("MOUNT_RS_CLI_NATIVE_NFS");
    require_opt_in("MOUNT_RS_CLI_NATIVE_SQLITE_NFS_BLOCKS");
    run_mixed_nfs_blocks_rejection();
}

fn require_opt_in(name: &str) {
    assert_eq!(
        std::env::var(name).ok().as_deref(),
        Some("1"),
        "set {name}=1 to opt into disposable native acceptance"
    );
}

#[test]
fn mounted_nfs_report_accepts_colored_stdout_without_warnings() {
    let owned = "/private/tmp/mount-rs-owned/mount-a";
    assert!(reported_nfs_mount(&format!(
        "stdout: mounted nfs at {owned} (source: 127.0.0.1:/)"
    )));
    assert!(reported_nfs_mount(&format!(
        "stdout: \u{1b}[32mmounted\u{1b}[0m nfs at {owned} (source: 127.0.0.1:/)"
    )));
    assert!(!reported_nfs_mount(&format!(
        "stderr: mounted nfs at {owned}"
    )));
    assert!(!reported_nfs_mount(&format!(
        "stdout: warning: mounted nfs at {owned}"
    )));
    assert!(!reported_nfs_mount(&format!(
        "stdout: unmounted nfs at {owned}"
    )));
}

fn run_journal(journal: &str) {
    let scope = TestScope::new(journal);
    configure_local_journal(&scope.metadata, journal);
    configure_local_journal(&scope.blocks, journal);
    let config_a = scope.write_config("a");
    let config_b = scope.write_config("b");

    let mut mount_a = NativeMount::spawn(&scope.cli_binary, &config_a, &scope.mountpoint_a);
    mount_a
        .wait_ready()
        .expect("first SQLite-backed native NFS mount");
    let mut mount_b = NativeMount::spawn(&scope.cli_binary, &config_b, &scope.mountpoint_b);
    mount_b
        .wait_ready()
        .expect("second SQLite-backed native NFS mount");
    assert!(mount_a.is_alive(), "CLI A exited before shared-volume I/O");
    assert!(mount_b.is_alive(), "CLI B exited before shared-volume I/O");

    let a_payload = b"created by CLI A while B was mounted";
    let b_payload = b"created by CLI B while A was mounted";
    fs::write(scope.mountpoint_a.join("created-a"), a_payload).expect("A creates file");
    await_bytes(&scope.mountpoint_b.join("created-a"), a_payload)
        .expect("B sees A's created bytes");
    fs::write(scope.mountpoint_b.join("created-b"), b_payload).expect("B creates file");
    await_bytes(&scope.mountpoint_a.join("created-b"), b_payload)
        .expect("A sees B's created bytes");

    let first_range = vec![b'1'; 4096];
    let second_range = vec![b'2'; 4096];
    let merged_ranges = [first_range.as_slice(), second_range.as_slice()].concat();
    fs::write(scope.mountpoint_a.join("shared-ranges"), vec![b'.'; 8192])
        .expect("seed shared range file");
    await_bytes(&scope.mountpoint_b.join("shared-ranges"), &vec![b'.'; 8192])
        .expect("B sees shared seed");
    let handle_a = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_a.join("shared-ranges"))
        .expect("A opens shared range handle");
    let handle_b = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_b.join("shared-ranges"))
        .expect("B opens shared range handle");
    let barrier = Arc::new(Barrier::new(3));
    let range_a = spawn_range_writer(handle_a, 0, first_range, Arc::clone(&barrier));
    let range_b = spawn_range_writer(handle_b, 4096, second_range, Arc::clone(&barrier));
    barrier.wait();
    range_a
        .join()
        .expect("join A range writer")
        .expect("A range write");
    range_b
        .join()
        .expect("join B range writer")
        .expect("B range write");
    await_bytes(&scope.mountpoint_a.join("shared-ranges"), &merged_ranges)
        .expect("A sees merged ranges");
    await_bytes(&scope.mountpoint_b.join("shared-ranges"), &merged_ranges)
        .expect("B sees merged ranges");

    fs::rename(
        scope.mountpoint_a.join("created-a"),
        scope.mountpoint_a.join("renamed-a"),
    )
    .expect("A renames its file");
    await_bytes(&scope.mountpoint_b.join("renamed-a"), a_payload).expect("B sees A's rename");
    await_absent(&scope.mountpoint_b.join("created-a")).expect("B no longer sees old A name");
    fs::remove_file(scope.mountpoint_b.join("renamed-a")).expect("B unlinks A's renamed file");
    await_absent(&scope.mountpoint_a.join("renamed-a")).expect("A sees B's unlink");

    let load_started = Instant::now();
    bounded_load(&scope);
    eprintln!(
        "SQLITE_CLI_NATIVE_LOAD journal={journal} processes=2 rounds_per_process={LOAD_ROUNDS} elapsed_ms={}",
        load_started.elapsed().as_millis()
    );

    if journal == "DELETE"
        && std::env::var("MOUNT_RS_CLI_NATIVE_SQLITE_INNER_MATRIX")
            .ok()
            .as_deref()
            == Some("1")
    {
        run_inner_sqlite_matrix(&scope);
    } else {
        let inner_lock_status = probe_inner_sqlite_lock(&scope);
        eprintln!(
            "SQLITE_INNER_NFS_LOCK journal={journal} status={inner_lock_status} clients=2 servers=2"
        );
    }

    mount_b
        .clean_stop()
        .expect("B clean SIGINT and native unmount");
    mount_a
        .clean_stop()
        .expect("A clean SIGINT and native unmount");
    assert!(!is_mounted_at(&scope.mountpoint_a).expect("inspect A mount entry"));
    assert!(!is_mounted_at(&scope.mountpoint_b).expect("inspect B mount entry"));
    check_backing_integrity(&scope, journal);

    let mut reopened = NativeMount::spawn(&scope.cli_binary, &config_a, &scope.mountpoint_a);
    reopened
        .wait_ready()
        .expect("fresh CLI process reopens SQLite volume");
    await_bytes(&scope.mountpoint_a.join("created-b"), b_payload)
        .expect("fresh mount retains B-created file");
    await_bytes(&scope.mountpoint_a.join("shared-ranges"), &merged_ranges)
        .expect("fresh mount retains both ranges");
    await_absent(&scope.mountpoint_a.join("renamed-a")).expect("fresh mount retains unlink");
    verify_load_files(&scope.mountpoint_a);
    reopened
        .clean_stop()
        .expect("fresh CLI clean SIGINT/unmount");
    check_backing_integrity(&scope, journal);
    scope.finish();
}

fn run_one_process_two_views() {
    let scope = TestScope::new("ONE-CLI-DELETE");
    configure_local_journal(&scope.metadata, "DELETE");
    configure_local_journal(&scope.blocks, "DELETE");
    let config = scope.write_config("a");
    let mut mounted = NativeMount::spawn_with_extra(
        &scope.cli_binary,
        &config,
        &scope.mountpoint_a,
        Some(&scope.mountpoint_b),
    );
    mounted.wait_ready().expect("one SQLite CLI mounts view A");
    let deadline = Instant::now() + READY_TIMEOUT;
    while !is_mounted_at(&scope.mountpoint_b).expect("inspect one-CLI second mount entry") {
        assert!(
            Instant::now() < deadline,
            "one SQLite CLI did not mount view B"
        );
        thread::sleep(Duration::from_millis(100));
    }
    fs::write(
        scope.mountpoint_a.join("one-cli-a"),
        b"A writes via one CLI",
    )
    .expect("view A creates file");
    await_bytes(
        &scope.mountpoint_b.join("one-cli-a"),
        b"A writes via one CLI",
    )
    .expect("view B sees A file");
    fs::write(
        scope.mountpoint_b.join("one-cli-b"),
        b"B writes via one CLI",
    )
    .expect("view B creates file");
    await_bytes(
        &scope.mountpoint_a.join("one-cli-b"),
        b"B writes via one CLI",
    )
    .expect("view A sees B file");

    let seed = vec![b'.'; 8192];
    fs::write(scope.mountpoint_a.join("one-cli-shared"), &seed).expect("seed one-CLI shared file");
    await_bytes(&scope.mountpoint_b.join("one-cli-shared"), &seed)
        .expect("view B sees shared seed");
    let handle_a = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_a.join("one-cli-shared"))
        .expect("open shared A handle");
    let handle_b = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(scope.mountpoint_b.join("one-cli-shared"))
        .expect("open shared B handle");
    let barrier = Arc::new(Barrier::new(3));
    let first = spawn_range_writer(handle_a, 0, vec![b'1'; 4096], Arc::clone(&barrier));
    let second = spawn_range_writer(handle_b, 4096, vec![b'2'; 4096], Arc::clone(&barrier));
    barrier.wait();
    first
        .join()
        .expect("join one-CLI A writer")
        .expect("A range write");
    second
        .join()
        .expect("join one-CLI B writer")
        .expect("B range write");
    let expected = [vec![b'1'; 4096], vec![b'2'; 4096]].concat();
    await_bytes(&scope.mountpoint_a.join("one-cli-shared"), &expected)
        .expect("view A sees merged file");
    await_bytes(&scope.mountpoint_b.join("one-cli-shared"), &expected)
        .expect("view B sees merged file");

    fs::rename(
        scope.mountpoint_a.join("one-cli-a"),
        scope.mountpoint_a.join("one-cli-renamed"),
    )
    .expect("view A renames file");
    await_bytes(
        &scope.mountpoint_b.join("one-cli-renamed"),
        b"A writes via one CLI",
    )
    .expect("view B sees rename");
    fs::remove_file(scope.mountpoint_b.join("one-cli-renamed"))
        .expect("view B unlinks renamed file");
    await_absent(&scope.mountpoint_a.join("one-cli-renamed")).expect("view A sees unlink");
    let started = Instant::now();
    bounded_load(&scope);
    eprintln!(
        "SQLITE_ONE_CLI_NATIVE_LOAD views=2 rounds_per_view={LOAD_ROUNDS} elapsed_ms={}",
        started.elapsed().as_millis()
    );
    mounted.clean_stop().expect("one CLI clean SIGINT/unmount");
    assert!(!is_mounted_at(&scope.mountpoint_a).expect("inspect one-CLI A unmount"));
    assert!(!is_mounted_at(&scope.mountpoint_b).expect("inspect one-CLI B unmount"));
    check_backing_integrity(&scope, "DELETE");

    let mut fresh = NativeMount::spawn(&scope.cli_binary, &config, &scope.mountpoint_a);
    fresh.wait_ready().expect("fresh one-CLI SQLite reopen");
    await_bytes(
        &scope.mountpoint_a.join("one-cli-b"),
        b"B writes via one CLI",
    )
    .expect("reopen retains B-created file");
    await_bytes(&scope.mountpoint_a.join("one-cli-shared"), &expected)
        .expect("reopen retains merged file");
    await_absent(&scope.mountpoint_a.join("one-cli-renamed")).expect("reopen retains unlink");
    verify_load_files(&scope.mountpoint_a);
    fresh.clean_stop().expect("fresh one-CLI clean unmount");
    scope.finish();
}

fn run_nfs_backing_diagnostic() {
    let scope = TestScope::new("NFS-BACKING");
    let memory_config = scope.write_memory_config();
    let mut backing = NativeMount::spawn_with_extra(
        &scope.cli_binary,
        &memory_config,
        &scope.backing_a,
        Some(&scope.backing_b),
    );
    backing
        .wait_ready()
        .expect("disposable backing view A mounts");
    let deadline = Instant::now() + READY_TIMEOUT;
    while !is_mounted_at(&scope.backing_b).expect("inspect disposable backing view B") {
        assert!(
            Instant::now() < deadline,
            "disposable backing view B did not mount"
        );
        thread::sleep(Duration::from_millis(100));
    }

    let lock = probe_sqlite_lock(&scope.backing_a, &scope.backing_b)
        .expect("candidate SQLite backing lock probe must initialize");
    eprintln!("SQLITE_NFS_BACKING_LOCK={lock} clients=2 servers=2 backing=memory-nfs");

    let metadata_a = scope.backing_a.join("metadata.sqlite");
    let metadata_b = scope.backing_b.join("metadata.sqlite");
    let configured = try_configure_journal(&metadata_a, "DELETE")
        .expect("candidate NFS metadata SQLite file initializes");
    eprintln!(
        "SQLITE_NFS_BACKING_INIT path={} journal={configured}",
        metadata_a.display()
    );
    configure_local_journal(&scope.blocks, "DELETE");
    let deadline = Instant::now() + VISIBILITY_TIMEOUT;
    while !metadata_b.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        metadata_b.exists(),
        "candidate NFS metadata file must appear in second view"
    );

    let config_a = scope.write_config_at("a", &metadata_a, &scope.blocks);
    let config_b = scope.write_config_at("b", &metadata_b, &scope.blocks);
    let mut writer_a = NativeMount::spawn(&scope.cli_binary, &config_a, &scope.mountpoint_a);
    let first_ready = writer_a.wait_ready();
    eprintln!("SQLITE_NFS_BACKING_CLI_A_READY={first_ready:?}");
    let mut writer_b = NativeMount::spawn(&scope.cli_binary, &config_b, &scope.mountpoint_b);
    let second_ready = writer_b.wait_ready();
    eprintln!("SQLITE_NFS_BACKING_CLI_B_READY={second_ready:?}");
    eprintln!("SQLITE_NFS_BACKING_CLI_B_STOP={:?}", writer_b.clean_stop());
    eprintln!("SQLITE_NFS_BACKING_CLI_B_OUTPUT={:?}", writer_b.output);
    eprintln!("SQLITE_NFS_BACKING_CLI_A_STOP={:?}", writer_a.clean_stop());
    eprintln!("SQLITE_NFS_BACKING_CLI_A_OUTPUT={:?}", writer_a.output);
    eprintln!(
        "SQLITE_NFS_BACKING_INTEGRITY={:?}",
        inspect_backing_integrity(&metadata_a, &scope.blocks)
    );
    backing
        .clean_stop()
        .expect("stop disposable NFS backing views");
    scope.finish();
    const NONLOCAL_REJECTION: &str = "concurrent SQLite metadata requires a local filesystem; NFS/network-backed databases are unsupported";
    assert!(
        first_ready
            .as_ref()
            .err()
            .is_some_and(|error| error.contains(NONLOCAL_REJECTION))
            && second_ready
                .as_ref()
                .err()
                .is_some_and(|error| error.contains(NONLOCAL_REJECTION))
            && !writer_a.output.iter().any(|line| reported_nfs_mount(line))
            && !writer_b.output.iter().any(|line| reported_nfs_mount(line)),
        "concurrent SQLite NFS backing must report local-filesystem rejection before native mount; A={first_ready:?}, B={second_ready:?}, A output={:?}, B output={:?}",
        writer_a.output,
        writer_b.output
    );
}

fn run_mixed_nfs_blocks_rejection() {
    let scope = TestScope::new("MIXED-NFS-BLOCKS");
    let memory_config = scope.write_memory_config();
    let mut backing = NativeMount::spawn(&scope.cli_binary, &memory_config, &scope.backing_a);
    backing
        .wait_ready()
        .expect("disposable NFS blocks backing view mounts");

    let blocks_on_nfs = scope.backing_a.join("blocks.sqlite");
    configure_local_journal(&scope.metadata, "DELETE");
    try_configure_journal(&blocks_on_nfs, "DELETE")
        .expect("initialize disposable NFS block SQLite database");
    let config = scope.write_config_at("a", &scope.metadata, &blocks_on_nfs);
    let mut writer = NativeMount::spawn(&scope.cli_binary, &config, &scope.mountpoint_a);
    let ready = writer.wait_ready();
    eprintln!("SQLITE_MIXED_NFS_BLOCKS_READY={ready:?}");
    writer
        .clean_stop()
        .expect("stop local-metadata/NFS-block candidate CLI");
    eprintln!("SQLITE_MIXED_NFS_BLOCKS_OUTPUT={:?}", writer.output);

    let metadata_row = inspect_fresh_metadata_row(&scope.metadata)
        .expect("inspect local SQLite metadata after candidate startup");
    eprintln!(
        "SQLITE_MIXED_NFS_BLOCKS_METADATA={}",
        serde_json::json!({
            "revision": metadata_row["revision"],
            "namespace_present": !metadata_row["namespace"].is_null(),
            "fence": metadata_row["fence"],
            "write_mode": metadata_row["write_mode"],
            "integrity": metadata_row["integrity"]
        })
    );
    let backing_integrity = inspect_backing_integrity(&scope.metadata, &blocks_on_nfs)
        .expect("inspect candidate metadata and NFS block SQLite integrity");
    eprintln!("SQLITE_MIXED_NFS_BLOCKS_INTEGRITY={backing_integrity:?}");
    backing
        .clean_stop()
        .expect("stop disposable NFS block backing view");
    scope.finish();

    const NONLOCAL_BLOCKS_REJECTION: &str = "concurrent SQLite blocks require a local filesystem; NFS/network-backed databases are unsupported";
    assert!(
        ready
            .as_ref()
            .err()
            .is_some_and(|error| error.contains(NONLOCAL_BLOCKS_REJECTION))
            && !writer.output.iter().any(|line| reported_nfs_mount(line)),
        "concurrent SQLite NFS blocks with local metadata must reject before native mount; ready={ready:?}, output={:?}",
        writer.output
    );
    assert_eq!(metadata_row["integrity"], "ok");
    assert_eq!(metadata_row["revision"], 0);
    assert!(metadata_row["namespace"].is_null());
    assert!(metadata_row["owner"].is_null());
    assert_eq!(metadata_row["fence"], 0);
    assert_eq!(metadata_row["expires"], 0);
    assert!(
        metadata_row["write_mode"].is_null(),
        "failed startup promoted local SQLite metadata to MRC1: {metadata_row}"
    );
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

fn bounded_load(scope: &TestScope) {
    let barrier = Arc::new(Barrier::new(3));
    let mut writers = Vec::new();
    for (writer, mountpoint) in [scope.mountpoint_a.clone(), scope.mountpoint_b.clone()]
        .into_iter()
        .enumerate()
    {
        let barrier = Arc::clone(&barrier);
        writers.push(thread::spawn(move || -> io::Result<()> {
            barrier.wait();
            for round in 0..LOAD_ROUNDS {
                let original = mountpoint.join(format!("load-{writer}-{round}"));
                let renamed = mountpoint.join(format!("load-renamed-{writer}-{round}"));
                let bytes = load_payload(writer, round);
                fs::write(&original, bytes)?;
                fs::rename(&original, &renamed)?;
                if round % 2 == 0 {
                    fs::remove_file(&renamed)?;
                }
            }
            Ok(())
        }));
    }
    barrier.wait();
    for writer in writers {
        writer
            .join()
            .expect("join bounded CLI load writer")
            .expect("bounded CLI load file lifecycle");
    }
    verify_load_files(&scope.mountpoint_a);
    verify_load_files(&scope.mountpoint_b);
}

fn load_payload(writer: usize, round: usize) -> Vec<u8> {
    let marker = format!("writer={writer};round={round};").into_bytes();
    let mut bytes = vec![b'X'; 4096];
    bytes[..marker.len()].copy_from_slice(&marker);
    bytes
}

fn verify_load_files(mountpoint: &Path) {
    for writer in 0..2 {
        for round in 0..LOAD_ROUNDS {
            let name = mountpoint.join(format!("load-renamed-{writer}-{round}"));
            if round % 2 == 0 {
                await_absent(&name).expect("removed load file stays absent");
            } else {
                await_bytes(&name, &load_payload(writer, round))
                    .expect("retained load file has exact contents");
            }
        }
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
                "{} did not expose expected bytes; last={observation}",
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
            Ok(_) if Instant::now() < deadline => {}
            Err(_) if Instant::now() < deadline => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "{} remains visible",
                    path.display()
                )));
            }
            Err(error) => return Err(error),
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn configure_local_journal(path: &Path, journal: &str) {
    try_configure_journal(path, journal).expect("configure owned SQLite backing journal");
}

fn try_configure_journal(path: &Path, journal: &str) -> Result<String, String> {
    const SCRIPT: &str = r#"
import sqlite3, sys
db = sqlite3.connect(sys.argv[1], timeout=3)
try:
    actual = db.execute("PRAGMA journal_mode=" + sys.argv[2]).fetchone()[0].upper()
    assert actual == sys.argv[2], (actual, sys.argv[2])
    db.execute("PRAGMA synchronous=FULL")
    print("SQLITE_BACKING_JOURNAL=" + actual, flush=True)
finally:
    db.close()
"#;
    let output = run_python_bounded(
        ["-c", SCRIPT]
            .into_iter()
            .map(str::to_owned)
            .chain([path.to_string_lossy().into_owned(), journal.to_owned()])
            .collect(),
    )?;
    if !output.contains(&format!("SQLITE_BACKING_JOURNAL={journal}")) {
        return Err(format!("SQLite selected a different journal: {output}"));
    }
    Ok(journal.to_owned())
}

fn check_backing_integrity(scope: &TestScope, journal: &str) {
    const SCRIPT: &str = r#"
import json, sqlite3, sys
for name, path in (("metadata", sys.argv[1]), ("blocks", sys.argv[2])):
    db = sqlite3.connect(path, timeout=3)
    try:
        integrity = db.execute("PRAGMA integrity_check").fetchone()[0]
        actual_mode = db.execute("PRAGMA journal_mode").fetchone()[0].upper()
        assert integrity == "ok", (name, integrity)
        assert actual_mode == sys.argv[3], (name, actual_mode, sys.argv[3])
        print(json.dumps({"case":"backing_integrity", "name":name,
                          "journal":actual_mode, "integrity":integrity}), flush=True)
    finally:
        db.close()
"#;
    let args = vec![
        "-c".to_owned(),
        SCRIPT.to_owned(),
        scope.metadata.to_string_lossy().into_owned(),
        scope.blocks.to_string_lossy().into_owned(),
        journal.to_owned(),
    ];
    let output = run_python_bounded(args).expect("check both local SQLite backing databases");
    for line in output.lines() {
        eprintln!("SQLITE_CLI_{line}");
    }
}

fn probe_inner_sqlite_lock(scope: &TestScope) -> String {
    probe_sqlite_lock(&scope.mountpoint_a, &scope.mountpoint_b)
        .expect("probe SQLite application lock inside both NFS views")
}

fn run_inner_sqlite_matrix(scope: &TestScope) {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/sqlite_nfs_adversarial.py");
    let output = run_python_with_timeout(
        vec![
            script.to_string_lossy().into_owned(),
            "--mount".to_owned(),
            scope.mountpoint_a.to_string_lossy().into_owned(),
            "--second-view".to_owned(),
            scope.mountpoint_b.to_string_lossy().into_owned(),
            "--workers".to_owned(),
            "2".to_owned(),
            "--transactions".to_owned(),
            "8".to_owned(),
        ],
        Duration::from_secs(420),
    )
    .expect("run SQLite application journal/load matrix inside two native NFS views");
    let mut summary = None;
    for line in output.lines() {
        let report: serde_json::Value =
            serde_json::from_str(line).expect("parse SQLite NFS matrix report");
        if report["case"] == "summary" {
            summary = Some(report.clone());
        }
        eprintln!("SQLITE_INNER_NFS_MATRIX {report}");
    }
    let summary = summary.expect("SQLite NFS matrix emitted a summary");
    assert_eq!(summary["failed"], 0, "SQLite NFS matrix failed: {summary}");
}

fn probe_sqlite_lock(first: &Path, second: &Path) -> Result<String, String> {
    let script =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/sqlite_nfs_adversarial.py");
    let args = vec![
        script.to_string_lossy().into_owned(),
        "--mount".to_owned(),
        first.to_string_lossy().into_owned(),
        "--second-view".to_owned(),
        second.to_string_lossy().into_owned(),
        "--lock-only".to_owned(),
    ];
    let output = run_python_bounded(args)?;
    let lock = output
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|value| value["case"] == "second_view_lock")
        .ok_or_else(|| format!("inner SQLite NFS probe produced no lock result: {output}"))?;
    if lock["integrity"] != "ok" || lock["rolled_back_value"] != 7 {
        return Err(format!("inner SQLite lock probe changed data: {lock}"));
    }
    let status = lock["status"]
        .as_str()
        .ok_or_else(|| format!("inner SQLite lock status missing: {lock}"))?;
    if !matches!(status, "blocked" | "lock_bypassed") {
        return Err(format!("inner SQLite lock probe error: {lock}"));
    }
    Ok(status.to_owned())
}

fn inspect_backing_integrity(metadata: &Path, blocks: &Path) -> Result<String, String> {
    const SCRIPT: &str = r#"
import json, sqlite3, sys
for name, path in (("metadata", sys.argv[1]), ("blocks", sys.argv[2])):
    db = sqlite3.connect(path, timeout=3)
    try:
        integrity = db.execute("PRAGMA integrity_check").fetchone()[0]
        journal = db.execute("PRAGMA journal_mode").fetchone()[0].upper()
        print(json.dumps({"name":name, "integrity":integrity, "journal":journal}), flush=True)
    finally:
        db.close()
"#;
    run_python_bounded(vec![
        "-c".to_owned(),
        SCRIPT.to_owned(),
        metadata.to_string_lossy().into_owned(),
        blocks.to_string_lossy().into_owned(),
    ])
}

fn inspect_fresh_metadata_row(path: &Path) -> Result<serde_json::Value, String> {
    const SCRIPT: &str = r#"
import json, sqlite3, sys
db = sqlite3.connect(sys.argv[1], timeout=3)
try:
    columns = {row[1] for row in db.execute("PRAGMA table_info(mount_rs_metadata)")}
    assert {"revision", "namespace", "owner", "fence", "expires"} <= columns, columns
    selected = "revision, namespace, owner, fence, expires"
    if "write_mode" in columns:
        selected += ", write_mode"
    row = db.execute("SELECT " + selected + " FROM mount_rs_metadata WHERE id=1").fetchone()
    assert row is not None, "missing metadata row"
    result = dict(zip(("revision", "namespace", "owner", "fence", "expires", "write_mode"),
                      (*row, None) if len(row) == 5 else row))
    result["integrity"] = db.execute("PRAGMA integrity_check").fetchone()[0]
    print(json.dumps(result), flush=True)
finally:
    db.close()
"#;
    let output = run_python_bounded(vec![
        "-c".to_owned(),
        SCRIPT.to_owned(),
        path.to_string_lossy().into_owned(),
    ])?;
    serde_json::from_str(output.trim())
        .map_err(|error| format!("parse local metadata row {output:?}: {error}"))
}

fn run_python_bounded(args: Vec<String>) -> Result<String, String> {
    run_python_with_timeout(args, PYTHON_TIMEOUT)
}

fn run_python_with_timeout(args: Vec<String>, timeout: Duration) -> Result<String, String> {
    let mut child = Command::new("python3")
        .args(&args)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("start Python fixture: {error}"))?;
    let stdout = child.stdout.take().expect("capture Python stdout");
    let stderr = child.stderr.take().expect("capture Python stderr");
    let stdout_reader = thread::spawn(move || {
        let mut text = String::new();
        BufReader::new(stdout)
            .read_to_string(&mut text)
            .map(|_| text)
    });
    let stderr_reader = thread::spawn(move || {
        let mut text = String::new();
        BufReader::new(stderr)
            .read_to_string(&mut text)
            .map(|_| text)
    });
    let status = wait_child_bounded(&mut child, timeout);
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let stdout = stdout_reader
        .join()
        .map_err(|_| "join Python stdout reader".to_owned())?
        .map_err(|error| error.to_string())?;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "join Python stderr reader".to_owned())?
        .map_err(|error| error.to_string())?;
    let status = status.ok_or_else(|| format!("Python fixture exceeded {timeout:?}"))?;
    if !status.success() {
        return Err(format!(
            "Python fixture exited {status}; stdout={stdout}; stderr={stderr}"
        ));
    }
    Ok(stdout)
}

struct TestScope {
    root: PathBuf,
    cli_binary: PathBuf,
    mountpoint_a: PathBuf,
    mountpoint_b: PathBuf,
    backing_a: PathBuf,
    backing_b: PathBuf,
    metadata: PathBuf,
    blocks: PathBuf,
    armed: bool,
}

impl TestScope {
    fn new(journal: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after Unix epoch")
            .as_nanos();
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "mount-rs-cli-sqlite-two-process-{journal}-{}-{nanos}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&root).expect("create run-owned SQLite CLI test root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))
            .expect("restrict run-owned SQLite CLI test root");
        let mountpoint_a = root.join("mount-a");
        let mountpoint_b = root.join("mount-b");
        let backing_a = root.join("backing-a");
        let backing_b = root.join("backing-b");
        fs::create_dir(&mountpoint_a).expect("create A mountpoint");
        fs::create_dir(&mountpoint_b).expect("create B mountpoint");
        fs::create_dir(&backing_a).expect("create backing A mountpoint");
        fs::create_dir(&backing_b).expect("create backing B mountpoint");
        let cli_binary = root.join("mount-rs-test-cli");
        fs::copy(env!("CARGO_BIN_EXE_mount-rs"), &cli_binary)
            .expect("pin built CLI executable in test-owned root");
        fs::set_permissions(&cli_binary, fs::Permissions::from_mode(0o700))
            .expect("make pinned CLI executable");
        Self {
            metadata: root.join("metadata.sqlite"),
            blocks: root.join("blocks.sqlite"),
            root,
            cli_binary,
            mountpoint_a,
            mountpoint_b,
            backing_a,
            backing_b,
            armed: true,
        }
    }

    fn write_config(&self, writer: &str) -> PathBuf {
        self.write_config_at(writer, &self.metadata, &self.blocks)
    }

    fn write_memory_config(&self) -> PathBuf {
        let config = serde_json::json!({
            "version": 1,
            "mountpoint": self.backing_a,
            "transport": "nfs",
            "driver": {"kind": "memory"}
        });
        let path = self.root.join("config-disposable-memory-backing.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&config).expect("serialize memory config"),
        )
        .expect("write owned memory backing NFS config");
        path
    }

    fn write_config_at(&self, writer: &str, metadata: &Path, blocks: &Path) -> PathBuf {
        let mountpoint = match writer {
            "a" => &self.mountpoint_a,
            "b" => &self.mountpoint_b,
            _ => panic!("unknown writer"),
        };
        let config = serde_json::json!({
            "version": 1,
            "mountpoint": mountpoint,
            "transport": "nfs",
            "driver": {
                "kind": "splitstore",
                "storage": {
                    "concurrent_writes": true,
                    "metadata": {"kind": "sqlite", "path": metadata},
                    "blocks": {"kind": "sqlite", "path": blocks},
                    "chunk_size_bytes": 4096,
                    "owner": format!("sqlite-cli-writer-{writer}")
                }
            }
        });
        let path = self.root.join(format!("config-{writer}.json"));
        fs::write(
            &path,
            serde_json::to_vec_pretty(&config).expect("serialize config"),
        )
        .expect("write owned CLI config");
        path
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        let mut errors = Vec::new();
        for path in [
            &self.mountpoint_a,
            &self.mountpoint_b,
            &self.backing_a,
            &self.backing_b,
        ] {
            if let Err(error) = unmount_exact_bounded(path) {
                errors.push(error);
            }
        }
        if [
            &self.mountpoint_a,
            &self.mountpoint_b,
            &self.backing_a,
            &self.backing_b,
        ]
        .iter()
        .any(|path| is_mounted_at(path).unwrap_or(true))
        {
            return Err(format!(
                "preserving {} because an exact NFS mount remains: {}",
                self.root.display(),
                errors.join("; ")
            ));
        }
        fs::remove_dir_all(&self.root)
            .map_err(|error| format!("remove owned root {}: {error}", self.root.display()))?;
        self.armed = false;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn finish(mut self) {
        self.cleanup()
            .expect("remove exact owned SQLite CLI test paths");
    }
}

impl Drop for TestScope {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("SQLite CLI test scope cleanup failed: {error}");
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
        Self::spawn_with_extra(cli_binary, config, mountpoint, None)
    }

    fn spawn_with_extra(
        cli_binary: &Path,
        config: &Path,
        mountpoint: &Path,
        extra_mountpoint: Option<&Path>,
    ) -> Self {
        let mut command = Command::new(cli_binary);
        command.args(["mount", "--config"]).arg(config);
        let trace = std::env::var("MOUNT_RS_CLI_SQLITE_NFS_BACKING_TRACE")
            .ok()
            .as_deref()
            == Some("1");
        command.arg(if trace { "--verbose" } else { "--quiet" });
        if let Some(extra) = extra_mountpoint {
            command.arg("--also-mountpoint").arg(extra);
        }
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn run-owned SQLite NFS CLI child");
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
        let deadline = Instant::now() + READY_TIMEOUT;
        let mut reported = false;
        loop {
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    reported |= reported_nfs_mount(&line);
                    self.output.push(line);
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => {}
            }
            if reported && is_mounted_at(&self.mountpoint)? {
                return Ok(());
            }
            if !self.is_alive() || Instant::now() >= deadline {
                let mount_diagnostic = inspect_owned_mount(&self.mountpoint);
                let _ = self.stop(false);
                return Err(format!(
                    "CLI did not mount {} within {READY_TIMEOUT:?}; mount_diagnostic={mount_diagnostic}; output={:?}",
                    self.mountpoint.display(),
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
            // SAFETY: this is the exact process ID owned by this guard.
            let sent = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
            if sent != 0 {
                eprintln!(
                    "signal owned CLI child failed: {}",
                    io::Error::last_os_error()
                );
            }
        }
        let status = wait_child_bounded(&mut self.child, CLEAN_EXIT_TIMEOUT);
        if status.is_none() {
            let _ = self.child.kill();
        }
        let status = status.or_else(|| self.child.wait().ok());
        if let Some(reader) = self.stdout_reader.take() {
            reader.join().expect("join CLI stdout reader");
        }
        if let Some(reader) = self.stderr_reader.take() {
            reader.join().expect("join CLI stderr reader");
        }
        self.output.extend(self.lines.try_iter());
        let normally_mounted = is_mounted_at(&self.mountpoint)?;
        let unmount = unmount_exact_bounded(&self.mountpoint);
        self.cleaned = unmount.is_ok();
        if expect_clean {
            let status = status.ok_or_else(|| "owned CLI had no exit status".to_owned())?;
            if !status.success()
                || normally_mounted
                || !self.output.iter().any(|line| line.contains("unmounted"))
            {
                return Err(format!(
                    "CLI exited {status} without normal unmount; mounted={normally_mounted}; output={:?}; fallback={unmount:?}",
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
            eprintln!("owned SQLite CLI child cleanup failed: {error}");
        }
    }
}

fn spawn_line_reader(
    pipe: impl Read + Send + 'static,
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
        .map_err(|error| format!("spawn exact NFS umount: {error}"))?;
    if wait_child_bounded(&mut child, UNMOUNT_TIMEOUT).is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    if is_mounted_at(mountpoint)? {
        Err(format!("exact NFS mount {} remains", mountpoint.display()))
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

fn reported_nfs_mount(line: &str) -> bool {
    if !line.starts_with("stdout: ") {
        return false;
    }
    let mut plain = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            plain.push(character);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for final_byte in chars.by_ref() {
            if ('@'..='~').contains(&final_byte) {
                break;
            }
        }
    }
    plain.starts_with("stdout: mounted nfs at ")
}

fn inspect_owned_mount(target: &Path) -> String {
    let canonical = fs::canonicalize(target)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| format!("canonicalize failed: {error}"));
    let statfs = CString::new(target.as_os_str().as_bytes())
        .map_err(|error| error.to_string())
        .and_then(|path| {
            let mut info = std::mem::MaybeUninit::<libc::statfs>::uninit();
            // SAFETY: the path is NUL-terminated and info is writable storage for statfs.
            let status = unsafe { libc::statfs(path.as_ptr(), info.as_mut_ptr()) };
            if status != 0 {
                return Err(io::Error::last_os_error().to_string());
            }
            // SAFETY: statfs succeeded and initialized the full result.
            let info = unsafe { info.assume_init() };
            // SAFETY: macOS statfs string fields are NUL-terminated arrays.
            let fs_type = unsafe { CStr::from_ptr(info.f_fstypename.as_ptr()) }.to_string_lossy();
            // SAFETY: macOS statfs string fields are NUL-terminated arrays.
            let mounted_on = unsafe { CStr::from_ptr(info.f_mntonname.as_ptr()) }.to_string_lossy();
            // SAFETY: macOS statfs string fields are NUL-terminated arrays.
            let mounted_from =
                unsafe { CStr::from_ptr(info.f_mntfromname.as_ptr()) }.to_string_lossy();
            Ok(format!(
                "type={fs_type:?} on={mounted_on:?} from={mounted_from:?}"
            ))
        });
    let mount_table = Command::new("mount").output().map(|output| {
        let table = String::from_utf8_lossy(&output.stdout);
        let matching_lines = table
            .lines()
            .filter(|line| line.contains("nfs") || line.contains(&target.display().to_string()))
            .collect::<Vec<_>>();
        let parsed = mount_rs_nfs::parse_mount_table(mount_rs_nfs::NfsPlatform::Macos, &table);
        format!(
            "exit={} parsed={} matching_raw={matching_lines:?}",
            output.status,
            parsed.len()
        )
    });
    format!("target={target:?} canonical={canonical} statfs={statfs:?} mount_table={mount_table:?}")
}
