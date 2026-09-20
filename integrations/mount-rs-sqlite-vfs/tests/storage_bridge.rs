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
    StorageOptions, VfsError,
};
use rusqlite::OpenFlags;

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
    }
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
    fs::remove_dir_all(root).expect("cleanup");
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
