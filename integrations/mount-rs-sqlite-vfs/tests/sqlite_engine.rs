use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_sqlite_vfs::{
    AccessMode, Backend, HostDirectory, LockLevel, OpenOptions, SqliteVfs, VfsError, VfsFile,
    VfsOptions, WalScope,
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

fn host_wal_vfs(root: &Path, suffix: &str) -> SqliteVfs {
    host_vfs_with_options(
        root,
        suffix,
        VfsOptions {
            require_full_sync: false,
            wal_scope: WalScope::HostLocal,
        },
    )
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

    vfs.close().expect("close VFS");
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
                wal_scope: mount_rs_sqlite_vfs::WalScope::Disabled,
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
        vfs.close()
            .unwrap_or_else(|error| panic!("{label}: close VFS: {error}"));
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
    vfs.close().expect("close VFS");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn unsafe_journal_modes_are_rejected_instead_of_falling_back() {
    let root = temp_root("unsafe-journal");
    let vfs = host_vfs(&root, "unsafe_journal");
    let connection = vfs.open("unsafe.db", flags()).expect("open database");
    connection
        .execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE t(value INTEGER);")
        .expect("initial rollback schema");

    for mode in ["MEMORY", "OFF"] {
        let pragma = format!("PRAGMA journal_mode={mode}");
        let error = connection
            .query_row(&pragma, [], |row| row.get::<_, String>(0))
            .expect_err("unsafe journal mode must be rejected");
        assert!(!error.to_string().is_empty(), "{mode}: missing error");
        let effective: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("query effective journal mode");
        assert_eq!(effective.to_ascii_uppercase(), "DELETE", "{mode}");
    }

    drop(connection);
    assert!(!root.join("unsafe.db-wal").exists());
    assert!(!root.join("unsafe.db-shm").exists());
    vfs.close().expect("close VFS");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn host_wal_round_trip_reader_writer_checkpoint_and_reopen() {
    let root = temp_root("wal-host");
    let vfs = host_wal_vfs(&root, "wal_host");
    let flags = flags();
    let first = vfs.open("wal.db", flags).expect("open writer");
    let mode: String = first
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .expect("enable WAL");
    assert_eq!(mode.to_ascii_uppercase(), "WAL");
    first
        .execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
             INSERT INTO records(id, body) VALUES (1, x'0001ff');",
        )
        .expect("create WAL schema");
    assert_eq!(
        first
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap()
            .to_ascii_uppercase(),
        "WAL"
    );
    assert_eq!(
        first
            .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );

    let second = vfs.open("wal.db", flags).expect("open reader");
    assert!(root.join("wal.db-wal").exists(), "WAL file was not created");
    assert!(
        root.join("wal.db-shm").exists(),
        "WAL shared-memory file was not created"
    );
    second
        .execute_batch("PRAGMA synchronous=NORMAL;")
        .expect("configure reader");
    second
        .execute_batch("BEGIN;")
        .expect("begin reader snapshot");
    let before: i64 = second
        .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
        .expect("read snapshot");
    assert_eq!(before, 1);
    first
        .execute(
            "INSERT INTO records(id, body) VALUES (2, ?1)",
            [vec![9_u8, 8, 7]],
        )
        .expect("WAL writer commit");
    let during: i64 = second
        .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
        .expect("read stable snapshot");
    assert_eq!(during, 1);
    second
        .execute_batch("ROLLBACK;")
        .expect("end reader snapshot");
    let checkpoint: (i64, i64, i64) = first
        .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("checkpoint WAL");
    assert!(checkpoint.0 >= 0);
    assert!(checkpoint.1 >= checkpoint.2);
    drop(second);
    drop(first);

    let reopened = vfs.open("wal.db", flags).expect("reopen WAL database");
    let reopened_mode: String = reopened
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("query reopened WAL mode");
    assert_eq!(reopened_mode.to_ascii_uppercase(), "WAL");
    let mut rows = reopened
        .prepare("SELECT id, body FROM records ORDER BY id")
        .expect("prepare reopened ledger");
    let ledger = rows
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Vec<u8>>(1)?))
        })
        .expect("query reopened ledger")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect reopened ledger");
    drop(rows);
    assert_eq!(ledger, vec![(1, vec![0, 1, 255]), (2, vec![9, 8, 7])]);
    let integrity: String = reopened
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("reopened WAL integrity");
    assert_eq!(integrity, "ok");
    drop(reopened);
    vfs.close().expect("close WAL VFS");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn host_wal_cross_process_reader_writer_checkpoint_and_reopen() {
    if std::env::var_os("MOUNT_RS_SQLITE_VFS_WAL_CHILD").is_some() {
        return;
    }
    let root = temp_root("wal-process");
    let vfs = host_wal_vfs(&root, "wal_process_parent");
    let first = vfs
        .open("process-wal.db", flags())
        .expect("open WAL writer");
    let mode: String = first
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .expect("enable process WAL");
    assert_eq!(mode.to_ascii_uppercase(), "WAL");
    first
        .execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
             INSERT INTO records(id, body) VALUES (1, x'0001ff');",
        )
        .expect("create process WAL schema");
    drop(first);

    let reader = vfs
        .open("process-wal.db", flags())
        .expect("open process reader");
    reader
        .execute_batch("PRAGMA synchronous=NORMAL; BEGIN;")
        .expect("begin process reader snapshot");
    assert_eq!(
        reader
            .query_row("SELECT count(*) FROM records", [], |row| row
                .get::<_, i64>(0))
            .expect("read process snapshot"),
        1
    );

    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("child_process_wal_writer")
        .arg("--nocapture")
        .env("MOUNT_RS_SQLITE_VFS_WAL_CHILD", "1")
        .env("MOUNT_RS_SQLITE_VFS_WAL_ROOT", &root)
        .status()
        .expect("spawn WAL child");
    assert!(status.success(), "WAL child status: {status}");

    assert_eq!(
        reader
            .query_row("SELECT count(*) FROM records", [], |row| row
                .get::<_, i64>(0))
            .expect("read stable process snapshot"),
        1
    );
    reader
        .execute_batch("ROLLBACK;")
        .expect("end process reader snapshot");
    drop(reader);

    let checkpoint = vfs
        .open("process-wal.db", flags())
        .expect("open checkpoint connection");
    let checkpoint_result: (i64, i64, i64) = checkpoint
        .query_row("PRAGMA wal_checkpoint(FULL)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("checkpoint process WAL");
    assert!(checkpoint_result.0 >= 0);
    assert!(checkpoint_result.1 >= checkpoint_result.2);
    drop(checkpoint);

    let reopened = vfs
        .open("process-wal.db", flags())
        .expect("reopen process WAL");
    assert_eq!(
        reopened
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .expect("query process WAL reopen mode")
            .to_ascii_uppercase(),
        "WAL"
    );
    reopened
        .execute_batch("PRAGMA synchronous=NORMAL;")
        .expect("configure process WAL reopen synchronous");
    assert_eq!(
        reopened
            .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
            .expect("query process WAL reopen synchronous"),
        1
    );
    assert_ledger(
        &reopened,
        &[
            LedgerRow {
                id: 1,
                body: vec![0, 1, 255],
            },
            LedgerRow {
                id: 2,
                body: vec![9, 8, 7],
            },
        ],
        "process WAL reopen",
    );
    drop(reopened);
    vfs.close().expect("close process WAL VFS");
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
        wal_scope: mount_rs_sqlite_vfs::WalScope::Disabled,
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
    let owner = vfs.clone();
    drop(vfs);

    let external = Connection::open_with_flags_and_vfs("registration-external.db", flags(), &name)
        .expect("name-based open must retain the registration");
    assert!(matches!(owner.close(), Err(VfsError::Busy)));
    drop(external);
    assert!(matches!(owner.close(), Err(VfsError::Busy)));

    let duplicate = SqliteVfs::new(
        &name,
        Arc::new(HostDirectory::new(&root).expect("second host backend")),
    )
    .expect_err("duplicate VFS names must not be replaced");
    assert!(duplicate.to_string().contains("already registered"));

    drop(connection);
    owner.close().expect("close VFS after name-open connection");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn borrowed_connection_lifetime_blocks_close_until_wrapper_drop() {
    let root = temp_root("borrowed-close");
    let vfs = host_vfs(&root, "borrowed_close");
    let mut connection = vfs.open("borrowed.db", flags()).expect("open database");
    connection.with_connection_mut(|connection| {
        connection
            .execute_batch("PRAGMA journal_mode=DELETE; CREATE TABLE t(value INTEGER);")
            .expect("initial schema");
        assert!(matches!(vfs.close(), Err(VfsError::Busy)));
    });
    assert!(matches!(vfs.close(), Err(VfsError::Busy)));
    drop(connection);
    vfs.close()
        .expect("close VFS after borrowed connection drop");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn extracted_connection_keeps_file_lease_after_wrapper_drop() {
    let root = temp_root("extracted-close");
    let vfs = host_vfs(&root, "extracted_close");
    let mut wrapper = vfs.open("extracted.db", flags()).expect("open database");
    let extracted = wrapper.with_connection_mut(|connection| {
        std::mem::replace(connection, Connection::open_in_memory().unwrap())
    });
    drop(wrapper);
    assert!(matches!(vfs.close(), Err(VfsError::Busy)));
    extracted
        .execute_batch("CREATE TABLE t(value INTEGER)")
        .unwrap();
    drop(extracted);
    vfs.close().expect("close after extracted connection");
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
    vfs.close().expect("close VFS");
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
    vfs.close().expect("close VFS");
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

#[test]
fn child_process_wal_writer() {
    if std::env::var_os("MOUNT_RS_SQLITE_VFS_WAL_CHILD").is_none() {
        return;
    }
    let root =
        PathBuf::from(std::env::var_os("MOUNT_RS_SQLITE_VFS_WAL_ROOT").expect("WAL child root"));
    let vfs = host_wal_vfs(&root, "wal_process_child");
    let connection = vfs
        .open("process-wal.db", flags())
        .expect("child WAL connection");
    assert_eq!(
        connection
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .expect("query child WAL mode")
            .to_ascii_uppercase(),
        "WAL"
    );
    connection
        .execute_batch("PRAGMA synchronous=NORMAL;")
        .expect("configure child WAL synchronous");
    connection
        .execute(
            "INSERT INTO records(id, body) VALUES (2, ?1)",
            [vec![9_u8, 8, 7]],
        )
        .expect("child WAL writer commit");
    drop(connection);
    vfs.close().expect("close child WAL VFS");
}

struct BlockingBackend {
    inner: HostDirectory,
    block_next_open: Arc<AtomicBool>,
    started: Arc<Barrier>,
    release: Arc<Barrier>,
}

impl Backend for BlockingBackend {
    fn open(&self, name: &[u8], options: OpenOptions) -> Result<Box<dyn VfsFile>, VfsError> {
        if self.block_next_open.swap(false, Ordering::SeqCst) {
            self.started.wait();
            self.release.wait();
        }
        self.inner.open(name, options)
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

#[test]
fn close_race_with_active_open_fails_closed_and_releases_backend() {
    let root = temp_root("close-race");
    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let block_next_open = Arc::new(AtomicBool::new(true));
    let name = format!("mount_rs_test_close_race_{}", std::process::id());
    let vfs = SqliteVfs::new(
        &name,
        Arc::new(BlockingBackend {
            inner: HostDirectory::new(&root).expect("host backend"),
            block_next_open: Arc::clone(&block_next_open),
            started: Arc::clone(&started),
            release: Arc::clone(&release),
        }),
    )
    .expect("register VFS");

    let opener = vfs.clone();
    let opening = std::thread::spawn(move || opener.open("race.db", flags()));
    started.wait();
    assert!(matches!(vfs.close(), Err(VfsError::Busy)));

    release.wait();
    let connection = opening.join().expect("open thread").expect("open database");
    assert!(matches!(vfs.close(), Err(VfsError::Busy)));
    drop(connection);

    vfs.close().expect("close VFS after race quiesces");
    assert!(Connection::open_with_flags_and_vfs("after-close.db", flags(), &name).is_err());

    let replacement = SqliteVfs::new(
        &name,
        Arc::new(HostDirectory::new(&root).expect("replacement host backend")),
    )
    .expect("reuse closed VFS name");
    assert!(vfs.open("wrong-backend.db", flags()).is_err());
    assert!(!root.join("wrong-backend.db").exists());
    vfs.close()
        .expect("old close is idempotent after name reuse");
    let replacement_connection = replacement
        .open("replacement.db", flags())
        .expect("open replacement database");
    drop(replacement_connection);
    replacement.close().expect("close replacement VFS");
    fs::remove_dir_all(root).expect("cleanup");
}

type DropHook = Box<dyn FnOnce() + Send>;

struct ReentrantDropBackend {
    inner: HostDirectory,
    hook: Arc<Mutex<Option<DropHook>>>,
}

impl Drop for ReentrantDropBackend {
    fn drop(&mut self) {
        let hook = self.hook.lock().expect("drop hook lock").take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

impl Backend for ReentrantDropBackend {
    fn open(&self, name: &[u8], options: OpenOptions) -> Result<Box<dyn VfsFile>, VfsError> {
        self.inner.open(name, options)
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

#[test]
fn close_drops_backend_outside_registry_lock() {
    let root = temp_root("reentrant-drop");
    let child_root = temp_root("reentrant-drop-child");
    let (done_sender, done_receiver) = std::sync::mpsc::channel();
    let child_root_for_hook = child_root.clone();
    let hook: Arc<Mutex<Option<DropHook>>> = Arc::new(Mutex::new(Some(Box::new(move || {
        let name = format!("mount_rs_test_reentrant_child_{}", std::process::id());
        let child = SqliteVfs::new(
            &name,
            Arc::new(HostDirectory::new(&child_root_for_hook).expect("child host backend")),
        )
        .expect("reentrant VFS registration");
        child.close().expect("reentrant VFS close");
        fs::remove_dir_all(&child_root_for_hook).expect("reentrant child cleanup");
        done_sender.send(()).expect("reentrant drop notification");
    }))));
    let name = format!("mount_rs_test_reentrant_{}", std::process::id());
    let vfs = SqliteVfs::new(
        &name,
        Arc::new(ReentrantDropBackend {
            inner: HostDirectory::new(&root).expect("host backend"),
            hook,
        }),
    )
    .expect("register VFS");

    vfs.close().expect("close reentrant VFS");
    done_receiver
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("backend drop must be able to reenter the registry");
    fs::remove_dir_all(root).expect("cleanup");
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
    drop(connection);
    vfs.close().expect("close VFS");
    fs::remove_dir_all(root).expect("cleanup");
}
