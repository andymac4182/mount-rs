//! SIGKILL of the `mount-rs` process while SQLite is writing through its
//! FUSE mount, with the volume's metadata in an actual TiDB.
//!
//! `scripts/test-tidb-rustfs-consumers.sh` runs this on Linux with the TiDB
//! that `scripts/test-tidb.sh` owns and the RustFS that
//! `scripts/test-rustfs.sh` owns. Each case:
//!
//! 1. mounts a split-store volume (TiDB metadata, TiDB or RustFS blocks) with
//!    the real `mount-rs mount` binary, in the default exclusive mode with a
//!    30 second writer lease;
//! 2. runs Python's sqlite3 against two databases on the mount, one in
//!    rollback-journal (`DELETE`) mode and one in `WAL` mode, both with
//!    `synchronous=FULL`, committing four-row transactions back to back and
//!    printing each `COMMIT` after SQLite returns from it;
//! 3. sends SIGKILL to the mount process right after SQLite has begun a new
//!    transaction, so the kill lands inside one;
//! 4. clears the dead FUSE mount, mounts the volume again with a new process
//!    (which the dead process's lease refuses until it expires, the
//!    documented recovery), and has SQLite check each database:
//!    `PRAGMA integrity_check` is `ok`, every transaction whose `COMMIT`
//!    returned is present with the right bytes, and the interrupted
//!    transaction is either all there or not there at all.
//!
//! This is a crash of the mounting process only. The kernel, TiDB and the
//! disks keep running, so it says nothing about power loss or host fsync.

#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::FileTypeExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const RUN_ENV: &str = "MOUNT_RS_RUN_NATIVE_TIDB_SQLITE_CRASH";
const LEASE_TTL_MS: u64 = 30_000;
const MOUNT_READY_TIMEOUT: Duration = Duration::from_secs(60);
const WORKLOAD_TIMEOUT: Duration = Duration::from_secs(300);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(15);
const REMOUNT_DEADLINE: Duration = Duration::from_secs(LEASE_TTL_MS / 1000 + 120);
/// Transactions each database must have committed before the kill.
const COMMITS_BEFORE_KILL: u64 = 8;
const ROWS_PER_TRANSACTION: u64 = 4;
const JOURNALS: [&str; 2] = ["DELETE", "WAL"];

#[derive(Clone, Copy)]
enum Blocks {
    Tidb,
    RustFs,
}

impl Blocks {
    fn label(self) -> &'static str {
        match self {
            Self::Tidb => "tidb",
            Self::RustFs => "rustfs",
        }
    }
}

#[test]
#[ignore = "requires Linux FUSE, Python sqlite3, a disposable TiDB and an explicit opt-in"]
fn sigkill_of_the_mount_mid_transaction_keeps_sqlite_on_tidb_blocks_intact() {
    run_case(Blocks::Tidb);
}

#[test]
#[ignore = "requires Linux FUSE, Python sqlite3, a disposable TiDB and RustFS, and an explicit opt-in"]
fn sigkill_of_the_mount_mid_transaction_keeps_sqlite_on_rustfs_blocks_intact() {
    run_case(Blocks::RustFs);
}

fn run_case(blocks: Blocks) {
    require_prerequisites();
    let scope = Scope::new(blocks);
    let config = scope.write_config(blocks);

    let mut first = MountProcess::spawn(&config, &scope.mountpoint);
    first
        .wait_ready()
        .unwrap_or_else(|error| panic!("first mount: {error}"));

    let mut workload = Workload::spawn(&scope.mountpoint);
    let kill_point = workload.wait_inside_transaction(&mut first);
    // SAFETY: the child is the live mount-rs process this test spawned.
    let killed = unsafe { libc::kill(first.child.id() as libc::pid_t, libc::SIGKILL) };
    assert_eq!(killed, 0, "SIGKILL the mount-rs process");
    let status = first.wait_exit();
    assert_eq!(
        status.signal(),
        Some(libc::SIGKILL),
        "the mount process ended by SIGKILL: {status}"
    );
    let progress = workload.finish_after_kill();
    eprintln!(
        "TIDB_SQLITE_MOUNT_CRASH_KILLED blocks={} after={kill_point} committed={:?} in_flight={:?}",
        blocks.label(),
        progress.committed,
        progress.in_flight,
    );
    for journal in JOURNALS {
        assert!(
            progress.committed.get(journal).copied().unwrap_or(0) >= COMMITS_BEFORE_KILL,
            "{journal} committed too little before the kill: {progress:?}"
        );
    }
    clear_dead_mount(&scope.mountpoint).expect("clear the dead FUSE mount");

    let (mut second, refused) = remount_after_lease(&config, &scope.mountpoint);
    assert!(
        refused > 0,
        "a new mount was accepted while the killed process's lease was still live"
    );
    let verified = verify_databases(&scope.mountpoint, &progress);
    second.clean_stop();
    verified.unwrap_or_else(|error| panic!("SQLite after the mount crash: {error}"));

    eprintln!(
        "TIDB_SQLITE_MOUNT_CRASH_PASS metadata=tidb blocks={} transport=fuse signal=KILL \
         committed_delete={} committed_wal={} in_flight={} integrity=ok lease_refusals={refused}",
        blocks.label(),
        progress.committed["DELETE"],
        progress.committed["WAL"],
        kill_point,
    );
    scope.finish();
}

fn require_prerequisites() {
    assert_eq!(
        std::env::var(RUN_ENV).ok().as_deref(),
        Some("1"),
        "set {RUN_ENV}=1 to opt into the Linux FUSE TiDB SQLite crash test"
    );
    let url = std::env::var("MOUNT_RS_TIDB_URL").unwrap_or_default();
    assert!(
        !url.trim().is_empty(),
        "MOUNT_RS_TIDB_URL is required; scripts/test-tidb.sh supplies it"
    );
    let device = fs::metadata("/dev/fuse").expect("Linux FUSE device /dev/fuse is required");
    assert!(
        device.file_type().is_char_device(),
        "/dev/fuse must be a character device"
    );
    for tool in ["python3", "fusermount3"] {
        let status = Command::new(tool)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        assert!(
            status.is_ok_and(|status| status.success()),
            "{tool} is required"
        );
    }
}

struct Scope {
    root: PathBuf,
    mountpoint: PathBuf,
    volume_key: String,
    armed: bool,
}

impl Scope {
    fn new(blocks: Blocks) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after Unix epoch")
            .as_nanos();
        let name = format!(
            "mount-rs-tidb-sqlite-crash-{}-{}-{nanos}",
            blocks.label(),
            std::process::id()
        );
        let root = std::env::temp_dir().join(&name);
        fs::create_dir(&root).expect("create the test directory");
        let mountpoint = root.join("mnt");
        fs::create_dir(&mountpoint).expect("create the mountpoint");
        Self {
            root,
            mountpoint,
            volume_key: format!("mount-rs/cli-tidb-sqlite-crash/{name}"),
            armed: true,
        }
    }

    fn write_config(&self, blocks: Blocks) -> PathBuf {
        let metadata = serde_json::json!({
            "kind": "tidb", "connection": { "env": "MOUNT_RS_TIDB_URL" },
            "volume_key": self.volume_key, "durable": true
        });
        let blocks = match blocks {
            Blocks::Tidb => metadata.clone(),
            Blocks::RustFs => {
                let required = |name: &str| {
                    let value = std::env::var(name).unwrap_or_else(|_| panic!("missing {name}"));
                    assert!(!value.is_empty(), "{name} must not be empty");
                    value
                };
                let endpoint = required("RUSTFS_ENDPOINT");
                assert!(
                    endpoint.starts_with("http://127.0.0.1:")
                        || endpoint.starts_with("http://localhost:"),
                    "the RustFS endpoint must be the harness's loopback service"
                );
                required("RUSTFS_ACCESS_KEY_ID");
                required("RUSTFS_SECRET_ACCESS_KEY");
                serde_json::json!({
                    "kind": "rustfs", "endpoint": endpoint,
                    "bucket": required("RUSTFS_BUCKET"), "region": required("RUSTFS_REGION"),
                    "prefix": format!("{}/blocks", self.volume_key),
                    "access_key_id": { "env": "RUSTFS_ACCESS_KEY_ID" },
                    "secret_access_key": { "env": "RUSTFS_SECRET_ACCESS_KEY" },
                    "durable": true
                })
            }
        };
        let config = serde_json::json!({
            "version": 1, "mountpoint": self.mountpoint, "transport": "fuse",
            "driver": { "kind": "splitstore", "storage": {
                "metadata": metadata, "blocks": blocks, "chunk_size_bytes": 4096,
                "owner": "cli-tidb-sqlite-crash", "lease_ttl_ms": LEASE_TTL_MS
            }}
        });
        let path = self.root.join("config.json");
        fs::write(
            &path,
            serde_json::to_vec_pretty(&config).expect("serialize the mount config"),
        )
        .expect("write the mount config");
        path
    }

    fn finish(mut self) {
        self.cleanup()
            .unwrap_or_else(|error| panic!("test directory cleanup: {error}"));
    }

    fn cleanup(&mut self) -> Result<(), String> {
        if !self.armed {
            return Ok(());
        }
        clear_dead_mount(&self.mountpoint)?;
        if is_mounted_at(&self.mountpoint) {
            return Err(format!("{} is still mounted", self.mountpoint.display()));
        }
        fs::remove_dir(&self.mountpoint)
            .map_err(|error| format!("remove {}: {error}", self.mountpoint.display()))?;
        let config = self.root.join("config.json");
        if config.exists() {
            fs::remove_file(&config).map_err(|error| format!("remove config: {error}"))?;
        }
        fs::remove_dir(&self.root)
            .map_err(|error| format!("remove {}: {error}", self.root.display()))?;
        self.armed = false;
        Ok(())
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        if self.armed
            && let Err(error) = self.cleanup()
        {
            eprintln!("TiDB SQLite crash test cleanup during unwinding: {error}");
        }
    }
}

struct MountProcess {
    child: Child,
    lines: Receiver<String>,
    stdout: Option<JoinHandle<()>>,
    stderr: Option<JoinHandle<Vec<String>>>,
    mountpoint: PathBuf,
    output: Vec<String>,
    reaped: bool,
}

impl MountProcess {
    fn spawn(config: &Path, mountpoint: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
            .args(["mount", "--config"])
            .arg(config)
            .arg("--quiet")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mount-rs");
        let (sender, lines) = mpsc::channel();
        let stdout = child.stdout.take().expect("mount-rs stdout");
        let stdout = thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let stderr = child.stderr.take().expect("mount-rs stderr");
        let stderr = thread::spawn(move || {
            BufReader::new(stderr)
                .lines()
                .map_while(Result::ok)
                .collect::<Vec<_>>()
        });
        Self {
            child,
            lines,
            stdout: Some(stdout),
            stderr: Some(stderr),
            mountpoint: mountpoint.to_path_buf(),
            output: Vec::new(),
            reaped: false,
        }
    }

    /// `Ok` once the CLI reports the mount; `Err` with its stderr if it exits.
    fn wait_ready(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + MOUNT_READY_TIMEOUT;
        let mut reported = false;
        while Instant::now() < deadline {
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    reported |= strip_ansi(&line).contains("mounted fuse");
                    self.output.push(line);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => thread::sleep(Duration::from_millis(50)),
            }
            if reported && is_mounted_at(&self.mountpoint) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait().expect("poll mount-rs") {
                self.reaped = true;
                let stderr = self.join_readers();
                return Err(format!(
                    "mount-rs exited before mounting: {status}; stdout={:?}; stderr={stderr:?}",
                    self.output
                ));
            }
        }
        Err(format!(
            "mount-rs did not mount within {MOUNT_READY_TIMEOUT:?}; stdout={:?}",
            self.output
        ))
    }

    fn join_readers(&mut self) -> Vec<String> {
        if let Some(stdout) = self.stdout.take() {
            let _ = stdout.join();
        }
        self.output.extend(self.lines.try_iter());
        self.stderr
            .take()
            .map(|stderr| stderr.join().unwrap_or_default())
            .unwrap_or_default()
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll mount-rs") {
                self.reaped = true;
                self.join_readers();
                return status;
            }
            assert!(Instant::now() < deadline, "mount-rs did not exit");
            thread::sleep(Duration::from_millis(50));
        }
    }

    /// The normal SIGINT stop, which unmounts and releases the lease.
    fn clean_stop(&mut self) {
        // SAFETY: the child is the live mount-rs process this test spawned.
        let signalled = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(signalled, 0, "SIGINT the mount-rs process");
        let status = self.wait_exit();
        let stderr = self.join_readers();
        assert!(
            status.success(),
            "mount-rs did not stop cleanly: {status}; stdout={:?}; stderr={stderr:?}",
            self.output
        );
        assert!(
            !is_mounted_at(&self.mountpoint),
            "the mount remained after a clean stop"
        );
    }
}

impl Drop for MountProcess {
    fn drop(&mut self) {
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        let _ = clear_dead_mount(&self.mountpoint);
    }
}

/// The interrupted transaction for each database is the last one that began
/// without a `COMMIT` line.
#[derive(Debug, Default)]
struct Progress {
    committed: BTreeMap<String, u64>,
    in_flight: BTreeMap<String, u64>,
}

impl Progress {
    fn observe(&mut self, line: &str) {
        let fields: Vec<&str> = line.split(' ').collect();
        let [kind, journal, txn] = fields.as_slice() else {
            return;
        };
        let Ok(txn) = txn.parse::<u64>() else {
            return;
        };
        match *kind {
            "BEGIN" => {
                self.in_flight.insert((*journal).to_owned(), txn);
            }
            "COMMIT" => {
                assert_eq!(
                    self.in_flight.remove(*journal),
                    Some(txn),
                    "COMMIT {journal} {txn} without its BEGIN"
                );
                let previous = self.committed.insert((*journal).to_owned(), txn);
                assert_eq!(
                    previous.unwrap_or(0) + 1,
                    txn,
                    "{journal} transactions committed out of order"
                );
            }
            _ => {}
        }
    }
}

struct Workload {
    child: Child,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
    progress: Progress,
}

impl Workload {
    fn spawn(mountpoint: &Path) -> Self {
        let mut child = Command::new("python3")
            .args(["-c", WORKLOAD])
            .arg(mountpoint)
            .arg(ROWS_PER_TRANSACTION.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the SQLite workload");
        let (sender, lines) = mpsc::channel();
        let stdout = child.stdout.take().expect("workload stdout");
        let reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
            progress: Progress::default(),
        }
    }

    /// Return once both databases have committed enough and SQLite has just
    /// begun another transaction.
    fn wait_inside_transaction(&mut self, mount: &mut MountProcess) -> String {
        let deadline = Instant::now() + WORKLOAD_TIMEOUT;
        loop {
            match self.lines.recv_timeout(Duration::from_millis(100)) {
                Ok(line) => {
                    self.progress.observe(&line);
                    let enough = JOURNALS.iter().all(|journal| {
                        self.progress.committed.get(*journal).copied().unwrap_or(0)
                            >= COMMITS_BEFORE_KILL
                    });
                    if enough && line.starts_with("BEGIN ") {
                        return line;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    let status = self.child.wait().expect("reap the workload");
                    panic!(
                        "the SQLite workload ended early: {status}; {:?}",
                        self.progress
                    );
                }
            }
            if let Some(status) = mount.child.try_wait().expect("poll mount-rs") {
                mount.reaped = true;
                let stderr = mount.join_readers();
                panic!("mount-rs died during the workload: {status}; stderr={stderr:?}");
            }
            assert!(
                Instant::now() < deadline,
                "the workload did not reach {COMMITS_BEFORE_KILL} commits per database: {:?}",
                self.progress
            );
        }
    }

    /// With the mount gone, SQLite's next file operation fails. Collect every
    /// line it printed, including any `COMMIT` that returned before the kill.
    fn finish_after_kill(mut self) -> Progress {
        let deadline = Instant::now() + EXIT_TIMEOUT;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("poll the workload") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                break self.child.wait().expect("reap the workload");
            }
            thread::sleep(Duration::from_millis(50));
        };
        assert!(
            !status.success(),
            "the SQLite workload cannot finish successfully without its mount"
        );
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        for line in self.lines.try_iter() {
            self.progress.observe(&line);
        }
        self.progress
    }
}

fn remount_after_lease(config: &Path, mountpoint: &Path) -> (MountProcess, u32) {
    let deadline = Instant::now() + REMOUNT_DEADLINE;
    let mut refused = 0_u32;
    loop {
        let mut mount = MountProcess::spawn(config, mountpoint);
        match mount.wait_ready() {
            Ok(()) => return (mount, refused),
            Err(error)
                if Instant::now() < deadline
                    && (error.contains("acquire writer") || error.contains("EAGAIN")) =>
            {
                refused += 1;
                drop(mount);
                thread::sleep(Duration::from_secs(2));
            }
            Err(error) => panic!("remount after the crash: {error}"),
        }
    }
}

fn verify_databases(mountpoint: &Path, progress: &Progress) -> Result<(), String> {
    let mut arguments = vec![
        mountpoint.display().to_string(),
        ROWS_PER_TRANSACTION.to_string(),
    ];
    for journal in JOURNALS {
        arguments.push(progress.committed[journal].to_string());
        arguments.push(
            progress
                .in_flight
                .get(journal)
                .copied()
                .unwrap_or(0)
                .to_string(),
        );
    }
    let output = Command::new("python3")
        .args(["-c", VERIFY])
        .args(&arguments)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("spawn the SQLite verifier: {error}"))?;
    eprint!("{}", String::from_utf8_lossy(&output.stdout));
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "verifier exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

const WORKLOAD: &str = r#"
import os, sqlite3, sys
root, rows = sys.argv[1], int(sys.argv[2])
def payload(txn, row):
    return bytes([(txn * 31 + row) % 251]) * 8192
dbs = []
for journal in ('DELETE', 'WAL'):
    db = sqlite3.connect(os.path.join(root, journal.lower() + '.sqlite'), isolation_level=None, timeout=30)
    assert db.execute(f'PRAGMA journal_mode={journal}').fetchone()[0].upper() == journal
    db.execute('PRAGMA synchronous=FULL')
    db.execute('CREATE TABLE items (txn INTEGER NOT NULL, row INTEGER NOT NULL, payload BLOB NOT NULL, PRIMARY KEY (txn, row))')
    dbs.append((journal, db))
txn = 0
while True:
    txn += 1
    for journal, db in dbs:
        print(f'BEGIN {journal} {txn}', flush=True)
        db.execute('BEGIN IMMEDIATE')
        for row in range(rows):
            db.execute('INSERT INTO items VALUES (?, ?, ?)', (txn, row, payload(txn, row)))
        db.execute('COMMIT')
        print(f'COMMIT {journal} {txn}', flush=True)
"#;

const VERIFY: &str = r#"
import os, sqlite3, sys
root, rows = sys.argv[1], int(sys.argv[2])
expected = {'DELETE': (int(sys.argv[3]), int(sys.argv[4])), 'WAL': (int(sys.argv[5]), int(sys.argv[6]))}
def payload(txn, row):
    return bytes([(txn * 31 + row) % 251]) * 8192
for journal, (committed, in_flight) in expected.items():
    db = sqlite3.connect(os.path.join(root, journal.lower() + '.sqlite'), timeout=30)
    try:
        assert db.execute('PRAGMA journal_mode').fetchone()[0].upper() == journal
        check = db.execute('PRAGMA integrity_check').fetchall()
        assert check == [('ok',)], f'{journal} integrity_check: {check}'
        found = {}
        for txn, row, data in db.execute('SELECT txn, row, payload FROM items'):
            assert data == payload(txn, row), f'{journal} txn {txn} row {row} has the wrong bytes'
            found.setdefault(txn, set()).add(row)
        for txn, present in found.items():
            assert present == set(range(rows)), f'{journal} txn {txn} is partial: rows {sorted(present)}'
        for txn in range(1, committed + 1):
            assert txn in found, f'{journal} lost committed transaction {txn} of {committed}'
        extra = sorted(set(found) - set(range(1, committed + 1)))
        assert extra in ([], [in_flight]), f'{journal} has transactions nobody began: {extra}'
        outcome = 'present' if extra else 'absent'
        print(f'TIDB_SQLITE_CRASH_VERIFY journal={journal} committed={committed} in_flight={in_flight} in_flight_outcome={outcome} integrity=ok', flush=True)
    finally:
        db.close()
"#;

fn clear_dead_mount(mountpoint: &Path) -> Result<(), String> {
    if !is_mounted_at(mountpoint) {
        return Ok(());
    }
    // A killed FUSE server leaves the kernel mount behind. A lazy unmount
    // detaches it even while a dead handle is still open.
    for (program, args) in [
        ("fusermount3", &["-u", "-z"] as &[&str]),
        ("fusermount", &["-u", "-z"] as &[&str]),
    ] {
        let Ok(mut child) = Command::new(program)
            .args(args)
            .arg(mountpoint)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        let deadline = Instant::now() + UNMOUNT_TIMEOUT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        if !is_mounted_at(mountpoint) {
            return Ok(());
        }
    }
    Err(format!(
        "FUSE mount remains at {} after a lazy unmount",
        mountpoint.display()
    ))
}

fn is_mounted_at(target: &Path) -> bool {
    let target = target.to_string_lossy();
    fs::read_to_string("/proc/self/mounts")
        .map(|table| {
            table.lines().any(|line| {
                let mut fields = line.split_whitespace();
                let _source = fields.next();
                fields.next().is_some_and(|path| path == target)
            })
        })
        .unwrap_or(false)
}

fn strip_ansi(value: &str) -> String {
    let mut plain = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(character) = chars.next() {
        if character != '\u{1b}' {
            plain.push(character);
            continue;
        }
        if chars.next() != Some('[') {
            continue;
        }
        for final_byte in chars.by_ref() {
            if final_byte.is_ascii_alphabetic() {
                break;
            }
        }
    }
    plain
}
