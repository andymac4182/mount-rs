use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_memory::{ManualClock, MemoryBlockStore, MemoryMetadataStore};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use mount_rs_sqlite_vfs::{
    Backend, FileKind, InlineExecutor, LockLevel, OpenOptions, SqliteVfs, StorageBackend,
    StorageOptions, VfsError, VfsOptions, WalScope,
};
use rusqlite::{Connection, OpenFlags};

#[derive(Clone)]
struct FaultBlockStore {
    inner: MemoryBlockStore,
    fail_flush: Arc<AtomicBool>,
}

#[derive(Clone)]
struct FaultMetadataStore {
    inner: MemoryMetadataStore,
    fail_publish: Arc<AtomicBool>,
}

#[async_trait]
impl BlockStore for FaultBlockStore {
    fn durable(&self) -> bool {
        false
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.inner.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        if self.fail_flush.load(Ordering::SeqCst) {
            return Err(FsError::new(ErrorCode::Eio).with_message("injected block barrier failure"));
        }
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.inner.delete(id).await
    }
}

#[async_trait]
impl MetadataStore for FaultMetadataStore {
    fn durable(&self) -> bool {
        false
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        self.inner.load().await
    }

    async fn acquire_writer(&self, owner: &str, ttl: std::time::Duration) -> Result<WriterLease> {
        self.inner.acquire_writer(owner, ttl).await
    }

    async fn renew_writer(
        &self,
        lease: &WriterLease,
        ttl: std::time::Duration,
    ) -> Result<WriterLease> {
        self.inner.renew_writer(lease, ttl).await
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        self.inner.release_writer(lease).await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        if self.fail_publish.load(Ordering::SeqCst) {
            return Err(
                FsError::new(ErrorCode::Eio).with_message("injected metadata publication failure")
            );
        }
        self.inner
            .publish(expected_revision, lease, namespace)
            .await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }
}

#[derive(Clone)]
struct ProbeBlockStore {
    inner: MemoryBlockStore,
    durable_called: Arc<AtomicBool>,
}

#[async_trait]
impl BlockStore for ProbeBlockStore {
    fn durable(&self) -> bool {
        self.durable_called.store(true, Ordering::SeqCst);
        false
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.inner.put(bytes).await
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.inner.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.inner.delete(id).await
    }
}

#[derive(Clone)]
struct ProbeMetadataStore {
    inner: MemoryMetadataStore,
    durable_called: Arc<AtomicBool>,
}

#[async_trait]
impl MetadataStore for ProbeMetadataStore {
    fn durable(&self) -> bool {
        self.durable_called.store(true, Ordering::SeqCst);
        false
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        self.inner.load().await
    }

    async fn acquire_writer(&self, owner: &str, ttl: std::time::Duration) -> Result<WriterLease> {
        self.inner.acquire_writer(owner, ttl).await
    }

    async fn renew_writer(
        &self,
        lease: &WriterLease,
        ttl: std::time::Duration,
    ) -> Result<WriterLease> {
        self.inner.renew_writer(lease, ttl).await
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        self.inner.release_writer(lease).await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        self.inner
            .publish(expected_revision, lease, namespace)
            .await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }
}

fn flags() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_FULL_MUTEX
}

fn temp_root(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "mount-rs-sqlite-vfs-storage-{label}-{}-{nonce}",
        std::process::id()
    ))
}

fn memory_backend(owner: &str) -> StorageBackend<MemoryMetadataStore, MemoryBlockStore> {
    StorageBackend::new(
        MemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        StorageOptions::volatile_for_tests(owner, 4096).expect("memory options"),
    )
    .expect("memory storage bridge")
}

fn sqlite_backend(
    root: &Path,
    owner: &str,
) -> StorageBackend<SqliteMetadataStore, SqliteBlockStore> {
    fs::create_dir_all(root).expect("storage root");
    StorageBackend::new(
        SqliteMetadataStore::open(root.join("metadata.sqlite")).expect("metadata store"),
        SqliteBlockStore::open(root.join("blocks.sqlite")).expect("block store"),
        StorageOptions::new(owner, 4096).expect("durable options"),
    )
    .expect("durable storage bridge")
}

fn vfs_name(label: &str) -> String {
    format!("mount_rs_storage_{label}_{}", std::process::id())
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

fn rollback_matrix_cells() -> [(JournalMode, SyncLevel); 9] {
    [
        (JournalMode::Delete, SyncLevel::Normal),
        (JournalMode::Delete, SyncLevel::Full),
        (JournalMode::Delete, SyncLevel::Extra),
        (JournalMode::Truncate, SyncLevel::Normal),
        (JournalMode::Truncate, SyncLevel::Full),
        (JournalMode::Truncate, SyncLevel::Extra),
        (JournalMode::Persist, SyncLevel::Normal),
        (JournalMode::Persist, SyncLevel::Full),
        (JournalMode::Persist, SyncLevel::Extra),
    ]
}

fn storage_matrix_vfs(backend: Arc<dyn Backend>, label: &str) -> SqliteVfs {
    SqliteVfs::with_options(
        &vfs_name(label),
        backend,
        VfsOptions {
            // This test intentionally exercises NORMAL as well as FULL and
            // EXTRA. The StorageBackend still performs its declared sync;
            // this policy only permits SQLite to request NORMAL.
            require_full_sync: false,
            wal_scope: WalScope::Disabled,
        },
    )
    .expect("register storage matrix VFS")
}

fn storage_wal_vfs(backend: Arc<dyn Backend>, label: &str) -> SqliteVfs {
    storage_wal_vfs_with_policy(backend, label, false)
}

fn durable_storage_wal_vfs(backend: Arc<dyn Backend>, label: &str) -> SqliteVfs {
    storage_wal_vfs_with_policy(backend, label, true)
}

fn storage_wal_vfs_with_policy(
    backend: Arc<dyn Backend>,
    label: &str,
    require_full_sync: bool,
) -> SqliteVfs {
    SqliteVfs::with_options(
        &vfs_name(label),
        backend,
        VfsOptions {
            require_full_sync,
            wal_scope: WalScope::ProcessLocal,
        },
    )
    .expect("register storage WAL VFS")
}

fn memory_matrix_backend(
    metadata: MemoryMetadataStore,
    blocks: MemoryBlockStore,
    owner: &str,
) -> StorageBackend<MemoryMetadataStore, MemoryBlockStore> {
    StorageBackend::new(
        metadata,
        blocks,
        StorageOptions::volatile_for_tests(owner, 4096).expect("memory matrix options"),
    )
    .expect("memory matrix storage bridge")
}

fn sqlite_matrix_backend(
    root: &Path,
    owner: &str,
) -> StorageBackend<SqliteMetadataStore, SqliteBlockStore> {
    fs::create_dir_all(root).expect("matrix storage root");
    StorageBackend::new(
        SqliteMetadataStore::open(root.join("metadata.sqlite")).expect("matrix metadata store"),
        SqliteBlockStore::open(root.join("blocks.sqlite")).expect("matrix block store"),
        StorageOptions::new(owner, 4096).expect("matrix durable options"),
    )
    .expect("durable matrix storage bridge")
}

fn assert_matrix_configuration(
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

fn configure_matrix_connection(
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
    assert_matrix_configuration(connection, journal_mode, sync_level, phase);
}

fn assert_matrix_ledger(connection: &Connection, expected: &[LedgerRow], phase: &str) {
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

fn write_storage_matrix_cell(
    vfs: &SqliteVfs,
    database: &str,
    journal_mode: JournalMode,
    sync_level: SyncLevel,
    label: &str,
) -> Vec<LedgerRow> {
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
    let connection = vfs
        .open(database, flags())
        .unwrap_or_else(|error| panic!("{label}: open initial connection: {error}"));

    configure_matrix_connection(
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
    assert_matrix_ledger(
        &connection,
        &ledger,
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
    assert_matrix_ledger(&connection, &ledger, &format!("{label}: after rollback"));
    assert_matrix_configuration(
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
    assert_matrix_ledger(
        &connection,
        &ledger,
        &format!("{label}: after committed row"),
    );
    assert_matrix_configuration(
        &connection,
        journal_mode,
        sync_level,
        &format!("{label}: after committed row"),
    );
    drop(connection);
    ledger
}

fn assert_storage_matrix_reopen(
    vfs: &SqliteVfs,
    database: &str,
    journal_mode: JournalMode,
    sync_level: SyncLevel,
    expected: &[LedgerRow],
    label: &str,
) {
    let connection = vfs
        .open(database, flags())
        .unwrap_or_else(|error| panic!("{label}: reopen connection: {error}"));
    let reopened_mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap_or_else(|error| panic!("{label}: query reopened journal mode: {error}"));
    assert_eq!(
        reopened_mode.to_ascii_uppercase(),
        JournalMode::Delete.pragma(),
        "{label}: non-WAL rollback mode did not reopen at SQLite's DELETE default"
    );
    configure_matrix_connection(
        &connection,
        journal_mode,
        sync_level,
        &format!("{label}: reopened configuration"),
    );
    assert_matrix_ledger(&connection, expected, &format!("{label}: after reopen"));
    drop(connection);
}

fn assert_invalid_options(options: StorageOptions, expected: &'static str) {
    let metadata_called = Arc::new(AtomicBool::new(false));
    let blocks_called = Arc::new(AtomicBool::new(false));
    let result = StorageBackend::with_executor(
        ProbeMetadataStore {
            inner: MemoryMetadataStore::new(),
            durable_called: Arc::clone(&metadata_called),
        },
        ProbeBlockStore {
            inner: MemoryBlockStore::new(),
            durable_called: Arc::clone(&blocks_called),
        },
        options,
        InlineExecutor,
    );

    match result {
        Err(VfsError::InvalidInput(message)) => assert_eq!(message, expected),
        Err(error) => panic!("invalid options returned the wrong error: {error}"),
        Ok(_) => panic!("invalid options unexpectedly constructed a storage bridge"),
    }
    assert!(!metadata_called.load(Ordering::SeqCst));
    assert!(!blocks_called.load(Ordering::SeqCst));
}

#[test]
fn with_executor_rejects_invalid_public_options_before_provider_inspection() {
    let empty_owner = StorageOptions {
        owner: String::new(),
        ..StorageOptions::default()
    };
    assert_invalid_options(empty_owner, "storage owner must not be empty");

    let mut zero_chunk = StorageOptions::new("mutated-chunk", 4096).expect("valid options");
    zero_chunk.chunk_size = 0;
    assert_invalid_options(zero_chunk, "storage chunk size must be positive");

    let mut zero_ttl = StorageOptions::new("mutated-ttl", 4096).expect("valid options");
    zero_ttl.lease_ttl = std::time::Duration::ZERO;
    assert_invalid_options(zero_ttl, "storage lease TTL must be positive");
}

#[test]
fn memory_bridge_runs_real_sqlite_engine_as_a_volatile_gate() {
    let backend = Arc::new(memory_backend("memory-engine"));
    let vfs = SqliteVfs::new(&vfs_name("memory_engine"), backend).expect("register VFS");
    let connection = vfs.open("memory.db", flags()).expect("open database");
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
             INSERT INTO records(body) VALUES (x'0001ff');",
        )
        .expect("SQLite transaction");

    let body: Vec<u8> = connection
        .query_row("SELECT body FROM records", [], |row| row.get(0))
        .expect("read blob");
    assert_eq!(body, [0, 1, 255]);
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("integrity check");
    assert_eq!(integrity, "ok");
}

#[test]
fn durable_sqlite_provider_pair_runs_real_sqlite_engine() {
    let root = temp_root("durable-engine");
    let backend = Arc::new(sqlite_backend(&root, "durable-engine"));
    assert!(backend.provider_is_durable());
    let vfs = SqliteVfs::new(&vfs_name("durable_engine"), backend).expect("register VFS");
    {
        let connection = vfs.open("durable.db", flags()).expect("open database");
        connection
            .execute_batch(
                "PRAGMA journal_mode=DELETE;
                 PRAGMA synchronous=FULL;
                 CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
                 INSERT INTO records(body) VALUES (x'00ff');",
            )
            .expect("durable transaction");
        assert!(matches!(vfs.close(), Err(VfsError::Busy)));
    }
    vfs.close()
        .expect("close durable VFS before provider reopen");
    let backend = Arc::new(sqlite_backend(&root, "durable-reopen"));
    let vfs = SqliteVfs::new(&vfs_name("durable_reopen"), backend).expect("register reopen VFS");
    let connection = vfs.open("durable.db", flags()).expect("reopen database");
    let body: Vec<u8> = connection
        .query_row("SELECT body FROM records", [], |row| row.get(0))
        .expect("reopen blob");
    assert_eq!(body, [0, 255]);
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("reopen integrity check");
    assert_eq!(integrity, "ok");
    drop(connection);
    vfs.close().expect("close reopened durable VFS");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn memory_storage_backend_runs_complete_rollback_matrix_as_volatile_evidence() {
    for (cell, (journal_mode, sync_level)) in rollback_matrix_cells().into_iter().enumerate() {
        let label = format!(
            "memory-matrix-{}-{}",
            journal_mode.label(),
            sync_level.label()
        );
        let database = format!("{label}.db");
        let metadata = MemoryMetadataStore::new();
        let blocks = MemoryBlockStore::new();

        // This is a volatile close/reopen check: both bridge instances use
        // cloned in-memory providers, so it does not claim process-restart
        // durability or persistence outside this test process.
        let first_backend =
            memory_matrix_backend(metadata.clone(), blocks.clone(), &format!("{label}-first"));
        assert!(
            !first_backend.provider_is_durable(),
            "{label}: memory pair must remain volatile"
        );
        let first_vfs = storage_matrix_vfs(
            Arc::new(first_backend),
            &format!("memory_matrix_{cell}_first"),
        );
        let ledger =
            write_storage_matrix_cell(&first_vfs, &database, journal_mode, sync_level, &label);
        first_vfs
            .close()
            .unwrap_or_else(|error| panic!("{label}: close volatile VFS: {error}"));

        let reopened_backend = memory_matrix_backend(metadata, blocks, &format!("{label}-reopen"));
        assert!(
            !reopened_backend.provider_is_durable(),
            "{label}: reopened memory pair must remain volatile"
        );
        let reopened_vfs = storage_matrix_vfs(
            Arc::new(reopened_backend),
            &format!("memory_matrix_{cell}_reopen"),
        );
        assert_storage_matrix_reopen(
            &reopened_vfs,
            &database,
            journal_mode,
            sync_level,
            &ledger,
            &label,
        );
        reopened_vfs
            .close()
            .unwrap_or_else(|error| panic!("{label}: close reopened volatile VFS: {error}"));
    }
}

#[test]
fn durable_sqlite_storage_backend_runs_complete_rollback_matrix_and_reopens() {
    for (cell, (journal_mode, sync_level)) in rollback_matrix_cells().into_iter().enumerate() {
        let label = format!(
            "durable-matrix-{}-{}",
            journal_mode.label(),
            sync_level.label()
        );
        let root = temp_root(&label);
        let database = format!("{label}.db");

        // Durable evidence uses fresh SQLite metadata/block provider objects
        // on reopen over the same provider files, unlike the memory test.
        let first_backend = sqlite_matrix_backend(&root, &format!("{label}-first"));
        assert!(
            first_backend.provider_is_durable(),
            "{label}: SQLite provider pair must be durable"
        );
        let first_vfs = storage_matrix_vfs(
            Arc::new(first_backend),
            &format!("durable_matrix_{cell}_first"),
        );
        let ledger =
            write_storage_matrix_cell(&first_vfs, &database, journal_mode, sync_level, &label);
        first_vfs
            .close()
            .unwrap_or_else(|error| panic!("{label}: close durable VFS: {error}"));

        let reopened_backend = sqlite_matrix_backend(&root, &format!("{label}-reopen"));
        assert!(
            reopened_backend.provider_is_durable(),
            "{label}: reopened SQLite provider pair must be durable"
        );
        let reopened_vfs = storage_matrix_vfs(
            Arc::new(reopened_backend),
            &format!("durable_matrix_{cell}_reopen"),
        );
        assert_storage_matrix_reopen(
            &reopened_vfs,
            &database,
            journal_mode,
            sync_level,
            &ledger,
            &label,
        );
        reopened_vfs
            .close()
            .unwrap_or_else(|error| panic!("{label}: close reopened durable VFS: {error}"));
        fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("{label}: cleanup: {error}"));
    }
}

#[test]
fn memory_storage_backend_runs_process_local_wal_as_volatile_evidence() {
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    let backend = memory_matrix_backend(metadata.clone(), blocks.clone(), "memory-wal-first");
    assert!(!backend.provider_is_durable());
    let vfs = storage_wal_vfs(Arc::new(backend), "memory_wal_first");
    let writer = vfs.open("wal.db", flags()).expect("open memory WAL writer");
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .expect("enable memory WAL");
    assert_eq!(mode.to_ascii_uppercase(), "WAL");
    writer
        .execute_batch(
            "PRAGMA synchronous=NORMAL;
             CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
             INSERT INTO records(id, body) VALUES (1, x'0001ff');",
        )
        .expect("create memory WAL schema");
    assert_eq!(
        writer
            .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
            .expect("query memory WAL synchronous"),
        1
    );
    let reader = vfs.open("wal.db", flags()).expect("open memory WAL reader");
    reader
        .execute_batch("BEGIN;")
        .expect("begin memory snapshot");
    assert_eq!(
        reader
            .query_row("SELECT count(*) FROM records", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    writer
        .execute(
            "INSERT INTO records(id, body) VALUES (2, ?1)",
            [vec![9_u8, 8, 7]],
        )
        .expect("memory WAL writer commit");
    assert_eq!(
        reader
            .query_row("SELECT count(*) FROM records", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    reader
        .execute_batch("ROLLBACK;")
        .expect("end memory snapshot");
    let checkpoint: (i64, i64, i64) = writer
        .query_row("PRAGMA wal_checkpoint(PASSIVE)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("memory WAL checkpoint");
    assert!(checkpoint.0 >= 0);
    drop(reader);
    drop(writer);
    vfs.close().expect("close memory WAL VFS");

    let reopened_backend = memory_matrix_backend(metadata, blocks, "memory-wal-reopen");
    assert!(!reopened_backend.provider_is_durable());
    let reopened_vfs = storage_wal_vfs(Arc::new(reopened_backend), "memory_wal_reopen");
    let reopened = reopened_vfs
        .open("wal.db", flags())
        .expect("reopen memory WAL");
    let reopened_mode: String = reopened
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("query memory WAL mode");
    assert_eq!(reopened_mode.to_ascii_uppercase(), "WAL");
    let ledger: Vec<(i64, Vec<u8>)> = reopened
        .prepare("SELECT id, body FROM records ORDER BY id")
        .expect("prepare memory WAL ledger")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query memory WAL ledger")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect memory WAL ledger");
    assert_eq!(ledger, vec![(1, vec![0, 1, 255]), (2, vec![9, 8, 7])]);
    let integrity: String = reopened
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("memory WAL integrity");
    assert_eq!(integrity, "ok");
    drop(reopened);
    reopened_vfs.close().expect("close reopened memory WAL VFS");
}

#[test]
fn durable_sqlite_storage_backend_runs_process_local_wal_and_reopens() {
    let root = temp_root("durable-wal");
    let backend = sqlite_matrix_backend(&root, "durable-wal-first");
    assert!(backend.provider_is_durable());
    let vfs = durable_storage_wal_vfs(Arc::new(backend), "durable_wal_first");
    let writer = vfs
        .open("wal.db", flags())
        .expect("open durable WAL writer");
    let mode: String = writer
        .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
        .expect("enable durable WAL");
    assert_eq!(mode.to_ascii_uppercase(), "WAL");
    writer
        .execute_batch("PRAGMA synchronous=FULL;")
        .expect("set durable WAL synchronous");
    assert_eq!(
        writer
            .query_row("PRAGMA synchronous", [], |row| row.get::<_, i64>(0))
            .expect("query durable WAL synchronous"),
        2
    );
    writer
        .execute_batch(
            "CREATE TABLE records(id INTEGER PRIMARY KEY, body BLOB NOT NULL);
             INSERT INTO records(id, body) VALUES (1, x'0001ff');",
        )
        .expect("create durable WAL schema");
    let reader = vfs
        .open("wal.db", flags())
        .expect("open durable WAL reader");
    reader
        .execute_batch("BEGIN;")
        .expect("begin durable snapshot");
    assert_eq!(
        reader
            .query_row("SELECT count(*) FROM records", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    writer
        .execute(
            "INSERT INTO records(id, body) VALUES (2, ?1)",
            [vec![9_u8, 8, 7]],
        )
        .expect("durable WAL writer commit");
    assert_eq!(
        reader
            .query_row("SELECT count(*) FROM records", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
    reader
        .execute_batch("ROLLBACK;")
        .expect("end durable snapshot");
    let checkpoint: (i64, i64, i64) = writer
        .query_row("PRAGMA wal_checkpoint(FULL)", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .expect("durable WAL checkpoint");
    assert!(checkpoint.0 >= 0);
    drop(reader);
    drop(writer);
    vfs.close().expect("close durable WAL VFS");

    let reopened_backend = sqlite_matrix_backend(&root, "durable-wal-reopen");
    assert!(reopened_backend.provider_is_durable());
    let reopened_vfs = durable_storage_wal_vfs(Arc::new(reopened_backend), "durable_wal_reopen");
    let reopened = reopened_vfs
        .open("wal.db", flags())
        .expect("reopen durable WAL");
    let ledger: Vec<(i64, Vec<u8>)> = reopened
        .prepare("SELECT id, body FROM records ORDER BY id")
        .expect("prepare durable WAL ledger")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("query durable WAL ledger")
        .collect::<rusqlite::Result<Vec<_>>>()
        .expect("collect durable WAL ledger");
    assert_eq!(ledger, vec![(1, vec![0, 1, 255]), (2, vec![9, 8, 7])]);
    let integrity: String = reopened
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("durable WAL integrity");
    assert_eq!(integrity, "ok");
    drop(reopened);
    reopened_vfs
        .close()
        .expect("close reopened durable WAL VFS");
    fs::remove_dir_all(root).expect("cleanup durable WAL");
}

#[test]
fn process_local_wal_block_fault_fails_closed_and_reopens_last_published_ledger() {
    let clock = Arc::new(ManualClock::new(3_000));
    let metadata = MemoryMetadataStore::with_clock(clock.clone());
    let fail_flush = Arc::new(AtomicBool::new(false));
    let blocks = FaultBlockStore {
        inner: MemoryBlockStore::new(),
        fail_flush: Arc::clone(&fail_flush),
    };
    let options = |owner: &str| {
        StorageOptions::volatile_for_tests(owner, 4096)
            .expect("fault WAL options")
            .with_lease_ttl(std::time::Duration::from_millis(10))
            .expect("short fault WAL lease")
    };
    let backend = Arc::new(
        StorageBackend::new(metadata.clone(), blocks.clone(), options("wal-fault-first"))
            .expect("fault WAL bridge"),
    );
    let vfs = storage_wal_vfs(backend, "wal_fault_first");
    let connection = vfs.open("fault-wal.db", flags()).expect("open fault WAL");
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE records(value INTEGER);
             INSERT INTO records VALUES (0);",
        )
        .expect("initial WAL publication");
    fail_flush.store(true, Ordering::SeqCst);
    let error = connection
        .execute("INSERT INTO records VALUES (1)", [])
        .expect_err("WAL block barrier fault must reach SQLite");
    assert!(!error.to_string().is_empty());
    drop(connection);
    vfs.close().expect("close failed WAL VFS");

    clock.advance_ms(1_000);
    fail_flush.store(false, Ordering::SeqCst);
    let backend = Arc::new(
        StorageBackend::new(metadata, blocks, options("wal-fault-reopen"))
            .expect("restart WAL bridge"),
    );
    let vfs = storage_wal_vfs(backend, "wal_fault_reopen");
    let connection = vfs.open("fault-wal.db", flags()).expect("reopen fault WAL");
    let count: i64 = connection
        .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
        .expect("reopen fault WAL rows");
    assert_eq!(count, 1);
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("reopen fault WAL integrity");
    assert_eq!(integrity, "ok");
    drop(connection);
    vfs.close().expect("close reopened fault WAL VFS");
}

#[test]
fn storage_backend_rejects_host_local_wal_without_a_host_mapping() {
    let backend = Arc::new(memory_backend("wal-capability"));
    let result = SqliteVfs::with_options(
        &vfs_name("wal_capability_reject"),
        backend,
        VfsOptions {
            require_full_sync: false,
            wal_scope: WalScope::HostLocal,
        },
    );
    assert!(matches!(
        result,
        Err(VfsError::Unsupported("requested WAL shared-memory scope"))
    ));

    let result = SqliteVfs::with_options(
        &vfs_name("wal_durability_reject"),
        Arc::new(memory_backend("wal-durability-capability")),
        VfsOptions {
            require_full_sync: true,
            wal_scope: WalScope::ProcessLocal,
        },
    );
    assert!(matches!(
        result,
        Err(VfsError::Unsupported(
            "WAL full synchronization requires a durable backend"
        ))
    ));
}

#[test]
fn storage_bridge_rejects_unsafe_journal_modes_without_fallback() {
    let backend = Arc::new(memory_backend("unsafe-journal"));
    let vfs = storage_matrix_vfs(backend, "unsafe_journal");
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
    vfs.close().expect("close VFS");
}

#[test]
fn failed_block_barrier_is_not_acknowledged_and_restart_reopens_old_integrity() {
    let clock = Arc::new(ManualClock::new(1_000));
    let metadata = MemoryMetadataStore::with_clock(clock.clone());
    let fail_flush = Arc::new(AtomicBool::new(false));
    let blocks = FaultBlockStore {
        inner: MemoryBlockStore::new(),
        fail_flush: Arc::clone(&fail_flush),
    };
    let options = |owner: &str| {
        StorageOptions::volatile_for_tests(owner, 4096)
            .expect("volatile options")
            .with_lease_ttl(std::time::Duration::from_millis(10))
            .expect("short lease")
    };
    let backend = Arc::new(
        StorageBackend::new(metadata.clone(), blocks.clone(), options("fault-first"))
            .expect("fault bridge"),
    );
    let vfs = SqliteVfs::new(&vfs_name("fault_first"), backend).expect("register fault VFS");
    let connection = vfs.open("fault.db", flags()).expect("open fault database");
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             CREATE TABLE records(value INTEGER);
             INSERT INTO records VALUES (0);",
        )
        .expect("initial durable publication");

    fail_flush.store(true, Ordering::SeqCst);
    let error = connection
        .execute("INSERT INTO records VALUES (1)", [])
        .expect_err("failed block barrier must reach SQLite");
    assert!(!error.to_string().is_empty());
    drop(connection);

    // A provider barrier failure poisons this bridge.  A later SQLite
    // connection through the same registration must fail to open rather than
    // observing an in-memory or partially published namespace.  Recovery is
    // only allowed after the bridge is rebuilt below.
    assert!(
        vfs.open("fault.db", flags()).is_err(),
        "failed storage bridge must reject a subsequent SQLite open"
    );
    vfs.close()
        .expect("close failed block-barrier VFS registration");

    // The failed bridge stays failed closed. A restart after provider lease
    // expiry must reopen the last published namespace, not a partial write.
    clock.advance_ms(1_000);
    fail_flush.store(false, Ordering::SeqCst);
    let backend = Arc::new(
        StorageBackend::new(metadata, blocks, options("fault-restart")).expect("restart bridge"),
    );
    let vfs = SqliteVfs::new(&vfs_name("fault_restart"), backend).expect("register restart VFS");
    let connection = vfs
        .open("fault.db", flags())
        .expect("reopen fault database");
    let count: i64 = connection
        .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
        .expect("reopen rows");
    assert_eq!(count, 1);
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("restart integrity check");
    assert_eq!(integrity, "ok");
}

#[test]
fn failed_metadata_publication_does_not_expose_partial_blocks_after_restart() {
    let clock = Arc::new(ManualClock::new(2_000));
    let fail_publish = Arc::new(AtomicBool::new(false));
    let metadata = FaultMetadataStore {
        inner: MemoryMetadataStore::with_clock(clock.clone()),
        fail_publish: Arc::clone(&fail_publish),
    };
    let blocks = MemoryBlockStore::new();
    let options = |owner: &str| {
        StorageOptions::volatile_for_tests(owner, 4096)
            .expect("volatile options")
            .with_lease_ttl(std::time::Duration::from_millis(10))
            .expect("short lease")
    };
    let backend = Arc::new(
        StorageBackend::new(metadata.clone(), blocks.clone(), options("publish-first"))
            .expect("publication bridge"),
    );
    let vfs = SqliteVfs::new(&vfs_name("publish_first"), backend).expect("register publish VFS");
    let connection = vfs
        .open("publish.db", flags())
        .expect("open publication database");
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             CREATE TABLE records(value INTEGER);
             INSERT INTO records VALUES (0);",
        )
        .expect("initial publication");

    fail_publish.store(true, Ordering::SeqCst);
    let error = connection
        .execute("INSERT INTO records VALUES (1)", [])
        .expect_err("failed metadata publication must reach SQLite");
    assert!(!error.to_string().is_empty());
    drop(connection);

    // Metadata publication is the namespace visibility boundary.  Once it
    // fails, the existing registration must stay closed to new SQLite
    // handles until a fresh bridge is constructed.
    assert!(
        vfs.open("publish.db", flags()).is_err(),
        "failed storage bridge must reject a subsequent SQLite open"
    );
    vfs.close()
        .expect("close failed metadata-publication VFS registration");

    clock.advance_ms(1_000);
    fail_publish.store(false, Ordering::SeqCst);
    let backend = Arc::new(
        StorageBackend::new(metadata, blocks, options("publish-restart"))
            .expect("publication restart bridge"),
    );
    let vfs = SqliteVfs::new(&vfs_name("publish_restart"), backend)
        .expect("register publication restart VFS");
    let connection = vfs
        .open("publish.db", flags())
        .expect("reopen publication database");
    let count: i64 = connection
        .query_row("SELECT count(*) FROM records", [], |row| row.get(0))
        .expect("reopen publication rows");
    assert_eq!(count, 1);
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("publication restart integrity check");
    assert_eq!(integrity, "ok");
}

#[test]
fn unrelated_handle_cannot_write_while_another_handle_owns_shared() {
    let backend = memory_backend("handle-authority");
    let options = OpenOptions {
        read_only: false,
        create: true,
        delete_on_close: false,
        kind: FileKind::MainDatabase,
        raw_flags: 0,
        wal_scope: WalScope::Disabled,
    };
    let mut first = backend.open(b"authority.db", options).expect("first open");
    first.lock(LockLevel::Shared).expect("first shared lock");
    let mut second = backend.open(b"authority.db", options).expect("second open");
    assert!(matches!(
        second.write_at(b"not authorized", 0),
        Err(VfsError::Busy)
    ));
    assert!(matches!(
        second.lock(LockLevel::Shared),
        Err(VfsError::Busy)
    ));
    first.unlock(LockLevel::None).expect("release first owner");
    second.lock(LockLevel::Shared).expect("second shared lock");
    second
        .lock(LockLevel::Reserved)
        .expect("second reserved lock");
    second.write_at(b"authorized", 0).expect("authorized write");
    second.sync(false).expect("authorized sync");
    second
        .unlock(LockLevel::None)
        .expect("release second owner");
}

#[test]
fn stale_reader_cannot_promote_to_writer_and_latest_commit_survives() {
    let backend = Arc::new(memory_backend("promotion"));
    let vfs = SqliteVfs::new(&vfs_name("promotion"), backend).expect("register VFS");
    let first = vfs.open("promotion.db", flags()).expect("first connection");
    first
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA busy_timeout=0;
             CREATE TABLE records(value INTEGER);
             INSERT INTO records VALUES (0);",
        )
        .expect("initial schema");
    // Open the second pager before the first one obtains its conservative
    // volume lease.  A later open may itself report SQLITE_BUSY when a reader
    // is already active; pre-opening makes the promotion check deterministic.
    let second = vfs
        .open("promotion.db", flags())
        .expect("second connection");
    first
        .execute_batch("BEGIN")
        .expect("first read transaction");
    let value: i64 = first
        .query_row("SELECT value FROM records", [], |row| row.get(0))
        .expect("stale reader snapshot");
    assert_eq!(value, 0);

    second
        .execute_batch("PRAGMA busy_timeout=0")
        .expect("busy timeout");
    let error = second
        .execute_batch("BEGIN IMMEDIATE")
        .expect_err("reader must not be promoted over the active owner");
    let message = error.to_string().to_ascii_lowercase();
    assert!(
        message.contains("busy") || message.contains("locked"),
        "{message}"
    );

    first
        .execute_batch("ROLLBACK")
        .expect("release stale reader");
    second
        .execute_batch("BEGIN IMMEDIATE; UPDATE records SET value=1; COMMIT")
        .expect("writer after reader release");
    let committed: i64 = second
        .query_row("SELECT value FROM records", [], |row| row.get(0))
        .expect("read committed value");
    assert_eq!(committed, 1);
}

#[test]
fn separate_process_cannot_promote_reader_to_writer_on_durable_bridge() {
    if std::env::var_os("MOUNT_RS_STORAGE_CHILD").is_some() {
        return;
    }
    let root = temp_root("process-promotion");
    let backend = Arc::new(sqlite_backend(&root, "parent-process"));
    let vfs = SqliteVfs::new(&vfs_name("process_parent"), backend).expect("register VFS");
    let connection = vfs.open("process.db", flags()).expect("parent connection");
    connection
        .execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA busy_timeout=0;
             CREATE TABLE records(value INTEGER);
             INSERT INTO records VALUES (0);
             BEGIN",
        )
        .expect("parent read transaction");
    let _: i64 = connection
        .query_row("SELECT value FROM records", [], |row| row.get(0))
        .expect("parent snapshot");

    let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .arg("--exact")
        .arg("child_process_storage_bridge_lock_attempt")
        .arg("--nocapture")
        .env("MOUNT_RS_STORAGE_CHILD", "1")
        .env("MOUNT_RS_STORAGE_ROOT", &root)
        .status()
        .expect("spawn child");
    assert!(status.success(), "child status: {status}");

    connection
        .execute_batch("ROLLBACK")
        .expect("release parent");
    drop(connection);
    vfs.close().expect("close parent VFS");
    fs::remove_dir_all(root).expect("cleanup");
}

#[test]
fn child_process_storage_bridge_lock_attempt() {
    if std::env::var_os("MOUNT_RS_STORAGE_CHILD").is_none() {
        return;
    }
    let root = PathBuf::from(std::env::var_os("MOUNT_RS_STORAGE_ROOT").expect("child root"));
    let backend = Arc::new(sqlite_backend(&root, "child-process"));
    let vfs = SqliteVfs::new(&vfs_name("process_child"), backend).expect("register child VFS");
    let error = match vfs.open("process.db", flags()) {
        Ok(connection) => connection
            .execute_batch("PRAGMA busy_timeout=0; BEGIN IMMEDIATE")
            .expect_err("child writer must observe parent lease"),
        Err(error) => error,
    };
    let message = error.to_string().to_ascii_lowercase();
    assert!(
        message.contains("busy") || message.contains("locked"),
        "{message}"
    );
}
