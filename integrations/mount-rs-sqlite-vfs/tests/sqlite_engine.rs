use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_sqlite_vfs::{
    AccessMode, Backend, HostDirectory, LockLevel, OpenOptions, SqliteVfs, VfsError, VfsFile,
    VfsOptions,
};
use rusqlite::{Connection, OpenFlags};

fn temp_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "mount-rs-sqlite-vfs-{label}-{}-{nonce}",
        std::process::id()
    ))
}

fn flags() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_FULL_MUTEX
}

fn host_vfs(root: &Path, suffix: &str) -> SqliteVfs {
    host_vfs_with_options(root, suffix, VfsOptions::default())
}

fn host_vfs_with_options(root: &Path, suffix: &str, options: VfsOptions) -> SqliteVfs {
    let backend = Arc::new(HostDirectory::new(root).expect("host backend"));
    SqliteVfs::with_options(
        &format!("mount_rs_test_{suffix}_{}", std::process::id()),
        backend,
        options,
    )
    .expect("register VFS")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum JournalMode {
    Delete,
    Truncate,
    Persist,
}

impl JournalMode {
    fn pragma(self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Truncate => "TRUNCATE",
            Self::Persist => "PERSIST",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Truncate => "truncate",
            Self::Persist => "persist",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SyncLevel {
    Normal,
    Full,
    Extra,
}

impl SyncLevel {
    fn pragma(self) -> &'static str {
        match self {
            Self::Normal => "NORMAL",
            Self::Full => "FULL",
            Self::Extra => "EXTRA",
        }
    }

    fn value(self) -> i64 {
        match self {
            Self::Normal => 1,
            Self::Full => 2,
            Self::Extra => 3,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Full => "full",
            Self::Extra => "extra",
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
struct LedgerRow {
    id: i64,
    body: Vec<u8>,
}

fn assert_effective_configuration(
    connection: &Connection,
    journal_mode: JournalMode,
    sync_level: SyncLevel,
    phase: &str,
) {
    let actual_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("{phase}: query effective journal mode: {error}"));
    assert_eq!(
        actual_mode.to_ascii_uppercase(),
        journal_mode.pragma(),
        "{phase}: SQLite changed the requested journal mode"
    );

    let actual_sync: i64 = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("{phase}: query effective synchronous level: {error}"));
    assert_eq!(
        actual_sync,
        sync_level.value(),
        "{phase}: SQLite changed the requested synchronous level"
    );
}

fn configure_connection(
    connection: &Connection,
    journal_mode: JournalMode,
    sync_level: SyncLevel,
    phase: &str,
) {
    let journal_pragma = format!("PRAGMA journal_mode={};", journal_mode.pragma());
    let actual_mode: String = connection
        .query_row(&journal_pragma, [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("{phase}: set journal mode: {error}"));
    assert_eq!(
        actual_mode.to_ascii_uppercase(),
        journal_mode.pragma(),
        "{phase}: requested journal mode was not accepted"
    );

    connection
        .execute_batch(&format!("PRAGMA synchronous={};", sync_level.pragma()))
        .unwrap_or_else(|error| panic!("{phase}: set synchronous level: {error}"));
    assert_effective_configuration(connection, journal_mode, sync_level, phase);
}

fn assert_ledger(connection: &Connection, expected: &[LedgerRow], phase: &str) {
    let mut statement = connection
        .prepare("SELECT id, body FROM records ORDER BY id")
        .unwrap_or_else(|error| panic!("{phase}: prepare ledger query: {error}"));
    let actual = statement
        .query_map([], |row| {
            Ok(LedgerRow {
                id: row.get(0)?,
                body: row.get(1)?,
            })
        })
        .unwrap_or_else(|error| panic!("{phase}: read ledger: {error}"))
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap_or_else(|error| panic!("{phase}: collect ledger: {error}"));
    assert_eq!(actual, expected, "{phase}: independent row ledger mismatch");

    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("{phase}: integrity check: {error}"));
    assert_eq!(integrity, "ok", "{phase}: integrity check failed");
}

fn assert_journal_artifact(root: &Path, database: &str, mode: JournalMode, phase: &str) {
    let journal = root.join(format!("{database}-journal"));
    match mode {
        JournalMode::Delete => assert!(!journal.exists(), "{phase}: DELETE left a journal"),
        JournalMode::Truncate => {
            let metadata = fs::metadata(&journal)
                .unwrap_or_else(|error| panic!("{phase}: TRUNCATE journal is missing: {error}"));
            assert_eq!(
                metadata.len(),
                0,
                "{phase}: TRUNCATE journal was not emptied"
            );
        }
        JournalMode::Persist => {
            assert!(journal.exists(), "{phase}: PERSIST journal is missing");
        }
    }
    assert!(
        !root.join(format!("{database}-wal")).exists(),
        "{phase}: rollback mode created a WAL file"
    );
    assert!(
        !root.join(format!("{database}-shm")).exists(),
        "{phase}: rollback mode created a shared-memory file"
    );
}

#[test]
fn rollback_journal_round_trip_and_integrity_check() {
    let root = temp_root("round-trip");
    let vfs = host_vfs(&root, "round_trip");

    {
        let mut connection = vfs.open("state.db", flags()).expect("open database");
        connection
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
                 PRAGMA synchronous=FULL;
                 CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
                 INSERT INTO records(body) VALUES (x'000102ff');",
            )
            .expect("initial transaction");

        connection
            .with_connection_mut(|connection| {
                let transaction = connection.transaction()?;
                transaction.execute("INSERT INTO records(body) VALUES (?1)", [vec![9_u8, 8, 7]])?;
                transaction.rollback()
            })
            .expect("rollback");

        let count: i64 = connection
            .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
            .expect("count");
        assert_eq!(count, 1);
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("integrity check");
        assert_eq!(integrity, "ok");
    }

    assert!(root.join("state.db").exists());
    assert!(!root.join("state.db-journal").exists());

    {
        let connection = vfs.open("state.db", flags()).expect("reopen database");
        let body: Vec<u8> = connection
            .query_row("SELECT body FROM records", [], |row| row.get(0))
            .expect("binary body");
        assert_eq!(body, vec![0, 1, 2, 255]);
        let integrity: String = connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .expect("reopen integrity check");
        assert_eq!(integrity, "ok");
    }

    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn rollback_journal_matrix_records_modes_sync_and_reopen_ledger() {
    let cells = [
        (JournalMode::Delete, SyncLevel::Normal),
        (JournalMode::Delete, SyncLevel::Full),
        (JournalMode::Delete, SyncLevel::Extra),
        (JournalMode::Truncate, SyncLevel::Normal),
        (JournalMode::Truncate, SyncLevel::Full),
        (JournalMode::Truncate, SyncLevel::Extra),
        (JournalMode::Persist, SyncLevel::Normal),
        (JournalMode::Persist, SyncLevel::Full),
        (JournalMode::Persist, SyncLevel::Extra),
    ];

    for (cell, (journal_mode, sync_level)) in cells.into_iter().enumerate() {
        let label = format!("matrix-{}-{}", journal_mode.label(), sync_level.label());
        let root = temp_root(&label);
        let vfs = host_vfs_with_options(
            &root,
            &format!("matrix_{cell}"),
            VfsOptions {
                // The matrix intentionally exercises NORMAL as well as FULL
                // and EXTRA.  The backend still performs its declared sync;
                // this option only permits SQLite to request NORMAL.
                require_full_sync: false,
            },
        );
        let database = format!("{label}.db");
        let first = LedgerRow {
            id: 1,
            body: vec![0, 1, 2, 255],
        };
        let rolled_back = LedgerRow {
            id: 2,
            body: vec![9, 8, 7, 6],
        };
        let committed = LedgerRow {
            id: 3,
            body: vec![0, 255, 17, 34, 51],
        };
        let mut ledger = vec![first];

        {
            let connection = vfs
                .open(&database, flags())
                .unwrap_or_else(|error| panic!("{label}: open initial connection: {error}"));
            configure_connection(
                &connection,
                journal_mode,
                sync_level,
                &format!("{label}: initial configuration"),
            );
            connection
                .execute_batch("CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB NOT NULL);")
                .unwrap_or_else(|error| panic!("{label}: create schema: {error}"));
            connection
                .execute(
                    "INSERT INTO records(id, body) VALUES (?1, ?2)",
                    rusqlite::params![ledger[0].id, &ledger[0].body],
                )
                .unwrap_or_else(|error| panic!("{label}: insert initial row: {error}"));
            assert_ledger(
                &connection,
                &ledger,
                &format!("{label}: after initial commit"),
            );
            assert_effective_configuration(
                &connection,
                journal_mode,
                sync_level,
                &format!("{label}: after initial commit"),
            );

            connection
                .execute_batch("BEGIN IMMEDIATE;")
                .unwrap_or_else(|error| panic!("{label}: begin rollback transaction: {error}"));
            connection
                .execute(
                    "INSERT INTO records(id, body) VALUES (?1, ?2)",
                    rusqlite::params![rolled_back.id, &rolled_back.body],
                )
                .unwrap_or_else(|error| panic!("{label}: insert rollback row: {error}"));
            connection
                .execute_batch("ROLLBACK;")
                .unwrap_or_else(|error| panic!("{label}: rollback transaction: {error}"));
            assert_ledger(&connection, &ledger, &format!("{label}: after rollback"));
            assert_effective_configuration(
                &connection,
                journal_mode,
                sync_level,
                &format!("{label}: after rollback"),
            );

            connection
                .execute(
                    "INSERT INTO records(id, body) VALUES (?1, ?2)",
                    rusqlite::params![committed.id, &committed.body],
                )
                .unwrap_or_else(|error| panic!("{label}: insert committed row: {error}"));
            ledger.push(committed);
            assert_ledger(
                &connection,
                &ledger,
                &format!("{label}: after committed row"),
            );
            assert_effective_configuration(
                &connection,
                journal_mode,
                sync_level,
                &format!("{label}: after committed row"),
            );
        }

        assert_journal_artifact(
            &root,
            &database,
            journal_mode,
            &format!("{label}: after initial close"),
        );

        {
            let connection = vfs
                .open(&database, flags())
                .unwrap_or_else(|error| panic!("{label}: reopen connection: {error}"));
            let reopened_mode: String = connection
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap_or_else(|error| panic!("{label}: query reopened journal mode: {error}"));
            assert_eq!(
                reopened_mode.to_ascii_uppercase(),
                JournalMode::Delete.pragma(),
                "{label}: non-WAL rollback mode did not reopen at SQLite's DELETE default"
            );
            configure_connection(
                &connection,
                journal_mode,
                sync_level,
                &format!("{label}: reopened configuration"),
            );
            assert_effective_configuration(
                &connection,
                journal_mode,
                sync_level,
                &format!("{label}: after reopened configuration"),
            );
            assert_ledger(&connection, &ledger, &format!("{label}: after reopen"));
        }

        assert_journal_artifact(
            &root,
            &database,
            journal_mode,
            &format!("{label}: after reopen close"),
        );
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("{label}: cleanup: {error}"));
    }
}

#[test]
fn wal_request_is_rejected_instead_of_falling_back() {
    let root = temp_root("wal");
    let vfs = host_vfs(&root, "wal");
    let connection = vfs.open("wal.db", flags()).expect("open database");
    connection
        .execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE t(value INTEGER);")
        .expect("initial schema");

    let error = connection
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get::<_, String>(0))
        .expect_err("rollback-only VFS must reject WAL");
    assert!(!error.to_string().is_empty());
    assert!(!root.join("wal.db-wal").exists());
    assert!(!root.join("wal.db-shm").exists());
    drop(connection);
    fs::remove_dir_all(root).expect("cleanup");
}

#[cfg(target_os = "windows")]
#[test]
fn windows_native_locks_preserve_rollback_state_transitions() {
    let root = temp_root("windows-lock-states");
    let backend = HostDirectory::new(&root).expect("host backend");
    let options = OpenOptions {
        read_only: false,
        create: true,
        delete_on_close: false,
        kind: mount_rs_sqlite_vfs::FileKind::MainDatabase,
        raw_flags: 0,
    };
    let mut reader = backend.open(b"locks.db", options).expect("reader");
    let mut writer = backend.open(b"locks.db", options).expect("writer");

    reader.lock(LockLevel::Shared).expect("shared reader lock");
    writer
        .lock(LockLevel::Shared)
        .expect("second shared reader lock");
    writer
        .lock(LockLevel::Reserved)
        .expect("reserved lock allows existing readers");
    writer
        .lock(LockLevel::Pending)
        .expect("pending lock blocks new readers");
    assert!(matches!(
        writer.lock(LockLevel::Exclusive),
        Err(VfsError::Busy)
    ));

    reader.unlock(LockLevel::None).expect("release reader");
    writer
        .lock(LockLevel::Exclusive)
        .expect("exclusive lock after reader release");
    writer.unlock(LockLevel::None).expect("release writer");
    drop(writer);
    drop(reader);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn duplicate_registration_is_rejected_and_name_open_survives_wrapper_drop() {
    let root = temp_root("registration");
    let name = format!("mount_rs_test_registration_{}", std::process::id());
    let vfs = SqliteVfs::new(
        &name,
        Arc::new(HostDirectory::new(&root).expect("host backend")),
    )
    .expect("register VFS");
    let connection = vfs
        .open("registration.db", flags())
        .expect("open through wrapper");
    drop(vfs);

    let external = Connection::open_with_flags_and_vfs("registration-external.db", flags(), &name)
        .expect("name-based open must retain the registration");
    drop(external);

    let duplicate = SqliteVfs::new(
        &name,
        Arc::new(HostDirectory::new(&root).expect("second host backend")),
    )
    .expect_err("duplicate VFS names must not be replaced");
    assert!(duplicate.to_string().contains("already registered"));

    drop(connection);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn two_connections_observe_reserved_lock_contention() {
    let root = temp_root("locks");
    let vfs = host_vfs(&root, "locks");
    let first = vfs.open("locks.db", flags()).expect("first connection");
    first
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA busy_timeout=0;
             CREATE TABLE t(value INTEGER);
             BEGIN IMMEDIATE",
        )
        .expect("reserve lock");

    let second = vfs.open("locks.db", flags()).expect("second connection");
    second
        .execute_batch("PRAGMA busy_timeout=0")
        .expect("busy timeout");
    let error = second
        .execute_batch("BEGIN IMMEDIATE")
        .expect_err("second writer must observe the first writer's lock");
    let message = error.to_string().to_ascii_lowercase();
    assert!(
        message.contains("busy") || message.contains("locked"),
        "{message}"
    );

    first.execute_batch("ROLLBACK").expect("release lock");
    second
        .execute_batch("BEGIN IMMEDIATE; ROLLBACK")
        .expect("lock becomes available");
    drop(second);
    drop(first);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn child_process_observes_lock_contention() {
    if std::env::var_os("MOUNT_RS_SQLITE_VFS_CHILD").is_some() {
        return;
    }
    let root = temp_root("process-lock");
    let vfs = host_vfs(&root, "process");
    let first = vfs.open("process.db", flags()).expect("parent connection");
    first
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA busy_timeout=0;
             CREATE TABLE t(value INTEGER);
             BEGIN IMMEDIATE",
        )
        .expect("parent lock");

    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("child_process_lock_attempt")
        .arg("--nocapture")
        .env("MOUNT_RS_SQLITE_VFS_CHILD", "1")
        .env("MOUNT_RS_SQLITE_VFS_ROOT", &root)
        .status()
        .expect("spawn child");
    assert!(status.success(), "child status: {status}");

    first
        .execute_batch("ROLLBACK")
        .expect("release parent lock");
    drop(first);
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn child_process_lock_attempt() {
    if std::env::var_os("MOUNT_RS_SQLITE_VFS_CHILD").is_none() {
        return;
    }
    let root = PathBuf::from(std::env::var_os("MOUNT_RS_SQLITE_VFS_ROOT").expect("child root"));
    let vfs = host_vfs(&root, "child");
    let connection = vfs.open("process.db", flags()).expect("child connection");
    connection
        .execute_batch("PRAGMA busy_timeout=0")
        .expect("busy timeout");
    let error = connection
        .execute_batch("BEGIN IMMEDIATE")
        .expect_err("child must observe parent process lock");
    let message = error.to_string().to_ascii_lowercase();
    assert!(
        message.contains("busy") || message.contains("locked"),
        "{message}"
    );
}

struct FaultBackend {
    inner: HostDirectory,
    fail_sync: Arc<AtomicBool>,
}

impl Backend for FaultBackend {
    fn open(&self, name: &[u8], options: OpenOptions) -> Result<Box<dyn VfsFile>, VfsError> {
        Ok(Box::new(FaultFile {
            inner: self.inner.open(name, options)?,
            fail_sync: Arc::clone(&self.fail_sync),
        }))
    }

    fn delete(&self, name: &[u8], sync_dir: bool) -> Result<(), VfsError> {
        self.inner.delete(name, sync_dir)
    }

    fn access(&self, name: &[u8], mode: AccessMode) -> Result<bool, VfsError> {
        self.inner.access(name, mode)
    }

    fn full_pathname(&self, name: &[u8]) -> Result<Vec<u8>, VfsError> {
        self.inner.full_pathname(name)
    }

    fn randomness(&self, output: &mut [u8]) -> Result<(), VfsError> {
        self.inner.randomness(output)
    }

    fn temporary_name(&self) -> Result<Vec<u8>, VfsError> {
        self.inner.temporary_name()
    }
}

struct FaultFile {
    inner: Box<dyn VfsFile>,
    fail_sync: Arc<AtomicBool>,
}

impl VfsFile for FaultFile {
    fn read_at(&mut self, output: &mut [u8], offset: u64) -> Result<usize, VfsError> {
        self.inner.read_at(output, offset)
    }

    fn write_at(&mut self, input: &[u8], offset: u64) -> Result<(), VfsError> {
        self.inner.write_at(input, offset)
    }

    fn truncate(&mut self, size: u64) -> Result<(), VfsError> {
        self.inner.truncate(size)
    }

    fn sync(&mut self, data_only: bool) -> Result<(), VfsError> {
        if self.fail_sync.load(Ordering::SeqCst) {
            return Err(VfsError::Other("injected sync failure".to_owned()));
        }
        self.inner.sync(data_only)
    }

    fn size(&mut self) -> Result<u64, VfsError> {
        self.inner.size()
    }

    fn lock(&mut self, level: LockLevel) -> Result<(), VfsError> {
        self.inner.lock(level)
    }

    fn unlock(&mut self, level: LockLevel) -> Result<(), VfsError> {
        self.inner.unlock(level)
    }

    fn check_reserved_lock(&mut self) -> Result<bool, VfsError> {
        self.inner.check_reserved_lock()
    }

    fn sector_size(&self) -> i32 {
        self.inner.sector_size()
    }

    fn device_characteristics(&self) -> i32 {
        self.inner.device_characteristics()
    }
}

#[test]
fn sync_fault_is_reported_and_reopen_stays_integrity_checked() {
    let root = temp_root("fault");
    let fail_sync = Arc::new(AtomicBool::new(false));
    let backend = Arc::new(FaultBackend {
        inner: HostDirectory::new(&root).expect("host backend"),
        fail_sync: Arc::clone(&fail_sync),
    });
    let vfs = SqliteVfs::new(
        &format!("mount_rs_test_fault_{}", std::process::id()),
        backend,
    )
    .expect("register VFS");
    let connection = vfs.open("fault.db", flags()).expect("open database");
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             CREATE TABLE t(value INTEGER);",
        )
        .expect("initial schema");

    fail_sync.store(true, Ordering::SeqCst);
    let error = connection
        .execute("INSERT INTO t(value) VALUES (42)", [])
        .expect_err("sync fault must fail the write transaction");
    assert!(!error.to_string().is_empty());
    drop(connection);

    fail_sync.store(false, Ordering::SeqCst);
    let connection = vfs.open("fault.db", flags()).expect("reopen database");
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity check");
    assert_eq!(integrity, "ok");
    fs::remove_dir_all(root).expect("cleanup");
}
