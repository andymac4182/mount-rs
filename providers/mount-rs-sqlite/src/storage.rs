//! Independently selectable SQLite metadata and immutable block providers.
//! Each database contains one namespace. Metadata and blocks may reside in
//! different databases, or either provider may be paired with another backend.

use async_trait::async_trait;
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::versioning::{
    PublicationId, ReadLease, ReadLeaseRequest, VersionHead, VersionId, VersionInfo, VersionKind,
    VersionPublication, VersionedMetadataStore, VolumeId,
};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// SQLite supplies the clock inside the same statement as lease validation.
const NOW: &str = "CAST(unixepoch('subsec') * 1000 AS INTEGER)";
const NOW_SELECT: &str = "SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)";
const CONCURRENT_WRITE_MODE: &str = "MRC1";
const CONCURRENT_FENCE_SENTINEL: i64 = i64::MAX;
const MAX_SQLITE_BUSY_RETRIES: usize = 16;
const SQLITE_BUSY_RETRY_BUDGET: Duration = Duration::from_secs(30);
const CONCURRENT_PUBLISH_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

fn sqlite_busy_known_noncommit(
    error: &rusqlite::Error,
    confirmed_noncommit: bool,
    syscall: &'static str,
) -> Option<FsError> {
    // BEGIN IMMEDIATE cannot have written if it returned SQLITE_BUSY. A busy
    // COMMIT leaves the transaction active; rusqlite's default Drop rolls it
    // back. Check autocommit after Drop before allowing the caller to replay.
    (error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) && confirmed_noncommit)
        .then(|| {
            FsError::new(ErrorCode::Eagain)
                .with_syscall(syscall)
                .with_message("SQLite writer contention did not commit")
        })
}

async fn sqlite_busy_backoff(attempt: usize) {
    let window = 250_u64 << attempt.min(6);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| u64::from(duration.subsec_nanos()));
    let jitter = (tick ^ (attempt as u64).wrapping_mul(0x9e3779b97f4a7c15)) % window;
    async_io::Timer::after(Duration::from_micros((window + jitter).min(20_000))).await;
}

#[cfg(target_os = "macos")]
fn require_local_concurrent_backing(path: &Path, kind: &'static str) -> Result<()> {
    use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

    let path = path.canonicalize().map_err(|error| {
        FsError::new(ErrorCode::Eio)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message(format!("cannot resolve SQLite {kind} file: {error}"))
    })?;
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        FsError::new(ErrorCode::Einval)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message(format!("SQLite {kind} path contains a NUL byte"))
    })?;
    let mut filesystem = MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(path.as_ptr(), filesystem.as_mut_ptr()) } != 0 {
        return Err(FsError::new(ErrorCode::Eio)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message(format!(
                "cannot inspect SQLite {kind} filesystem: {}",
                std::io::Error::last_os_error()
            )));
    }
    let filesystem = unsafe { filesystem.assume_init() };
    if filesystem.f_flags & (libc::MNT_LOCAL as u32) == 0 {
        let verb = if kind == "blocks" {
            "require"
        } else {
            "requires"
        };
        return Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("prepare concurrent SQLite volume")
            .with_message(format!(
                "concurrent SQLite {kind} {verb} a local filesystem; NFS/network-backed databases are unsupported",
            )));
    }
    Ok(())
}

#[derive(Clone)]
struct Database {
    connection: Arc<Mutex<Connection>>,
    durable: bool,
}

impl Database {
    fn open(path: Option<&Path>, schema: &str) -> Result<Self> {
        if let Some(path) = path {
            super::ensure_parent_directory(path)?;
        }
        let connection = match path {
            Some(path) => Connection::open(path),
            None => Connection::open_in_memory(),
        }
        .map_err(backend_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(backend_error)?;
        connection
            .execute_batch("PRAGMA synchronous=FULL;")
            .map_err(backend_error)?;
        connection.execute_batch(schema).map_err(backend_error)?;
        // Empty paths and :memory: are not durable even when passed to open().
        let durable = connection.path().is_some_and(|path| !path.is_empty());
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            durable,
        })
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| backend_error("SQLite storage lock poisoned"))
    }

    fn with_concurrent_publish_timeout<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let mut connection = self.lock()?;
        let original_ms: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .map_err(backend_error)?;
        let original_ms = u64::try_from(original_ms)
            .map_err(|_| backend_error("SQLite busy timeout is negative"))?;
        connection
            .busy_timeout(CONCURRENT_PUBLISH_BUSY_TIMEOUT)
            .map_err(backend_error)?;
        let result = operation(&mut connection);
        // Restore under the same mutex even when the CAS fails. If restoration
        // fails after a commit, fail closed instead of reporting a success
        // while the connection has an unexpected contention policy.
        connection
            .busy_timeout(Duration::from_millis(original_ms))
            .map_err(backend_error)?;
        result
    }

    fn flush(&self) -> Result<()> {
        // Every modifying statement is an autocommit transaction with FULL
        // synchronous durability. There are no deferred writes to flush.
        let connection = self.lock()?;
        if !connection.is_autocommit() {
            return Err(backend_error(
                "SQLite storage has an unfinished transaction",
            ));
        }
        Ok(())
    }
}

const METADATA_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_metadata (
 id INTEGER PRIMARY KEY CHECK(id=1),
 revision INTEGER NOT NULL CHECK(revision>=0), namespace TEXT,
 owner TEXT, fence INTEGER NOT NULL CHECK(fence>=0), expires INTEGER NOT NULL);
 INSERT OR IGNORE INTO mount_rs_metadata
 (id, revision, namespace, owner, fence, expires) VALUES(1,0,NULL,NULL,0,0);";
const BLOCK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_blocks (
 id TEXT PRIMARY KEY NOT NULL, bytes BLOB NOT NULL);";
const SCHEMA_VERSION_TABLE: &str = "mount_rs_schema_versions";
const VERSION_SCHEMA_NAME: &str = "mount-rs-versioning";
const VERSION_SCHEMA_VERSION: i64 = 1;

const VERSION_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mount_rs_version_state (
 id INTEGER PRIMARY KEY CHECK(id=1),
 volume_id TEXT NOT NULL,
 head_id TEXT,
 next_sequence INTEGER NOT NULL CHECK(next_sequence>0),
 next_read_fence INTEGER NOT NULL CHECK(next_read_fence>=0));
CREATE TABLE IF NOT EXISTS mount_rs_versions (
 id TEXT PRIMARY KEY NOT NULL,
 volume_id TEXT NOT NULL,
 sequence INTEGER NOT NULL CHECK(sequence>0),
 parent_id TEXT,
 restored_from TEXT,
 forked_from TEXT,
 namespace TEXT NOT NULL,
 block_store_id TEXT NOT NULL,
 kind TEXT NOT NULL,
 created_at_ms INTEGER NOT NULL,
 durable INTEGER NOT NULL CHECK(durable IN (0,1)),
 operation_id TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS mount_rs_version_pins (
 view_id TEXT PRIMARY KEY NOT NULL,
 volume_id TEXT NOT NULL,
 version_id TEXT NOT NULL,
 owner TEXT NOT NULL,
 fence INTEGER NOT NULL CHECK(fence>0),
 expires INTEGER NOT NULL CHECK(expires>=0));";

fn initialize_version_schema(database: &Database) -> Result<()> {
    let mut connection = database.lock()?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(backend_error)?;

    let schema_columns = table_columns(&tx, SCHEMA_VERSION_TABLE)?;
    if let Some(columns) = schema_columns {
        require_columns(
            SCHEMA_VERSION_TABLE,
            &columns,
            &["schema_name", "schema_version"],
        )?;
    } else {
        tx.execute_batch(
            "CREATE TABLE mount_rs_schema_versions (
                 schema_name TEXT PRIMARY KEY NOT NULL,
                 schema_version INTEGER NOT NULL CHECK(schema_version>=0)
             )",
        )
        .map_err(backend_error)?;
    }
    require_primary_key(&tx, SCHEMA_VERSION_TABLE, "schema_name")?;

    let stored_version: Option<i64> = tx
        .query_row(
            "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=?1",
            params![VERSION_SCHEMA_NAME],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend_error)?;
    if stored_version.is_some_and(|version| !(0..=VERSION_SCHEMA_VERSION).contains(&version)) {
        return Err(incompatible_schema(
            "unsupported mount-rs versioning schema version",
        ));
    }
    let schema_is_current = stored_version == Some(VERSION_SCHEMA_VERSION);

    let metadata_columns = table_columns(&tx, "mount_rs_metadata")?
        .ok_or_else(|| incompatible_schema("mount_rs_metadata table is missing"))?;
    require_columns(
        "mount_rs_metadata",
        &metadata_columns,
        &["id", "revision", "namespace", "owner", "fence", "expires"],
    )?;
    require_primary_key(&tx, "mount_rs_metadata", "id")?;
    let has_volume_id = metadata_columns.contains("volume_id");
    let has_write_mode = metadata_columns.contains("write_mode");
    if schema_is_current && !has_volume_id {
        return Err(incompatible_schema(
            "current versioning schema is missing metadata.volume_id",
        ));
    }
    if !has_volume_id {
        tx.execute(
            "ALTER TABLE mount_rs_metadata ADD COLUMN volume_id TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    if !has_write_mode {
        tx.execute(
            "ALTER TABLE mount_rs_metadata ADD COLUMN write_mode TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    let (mode, owner, fence, expires): (Option<String>, Option<String>, i64, i64) = tx
        .query_row(
            "SELECT write_mode, owner, fence, expires FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(backend_error)?;
    match mode.as_deref() {
        None if fence != CONCURRENT_FENCE_SENTINEL => {}
        Some(CONCURRENT_WRITE_MODE)
            if owner.is_none() && fence == CONCURRENT_FENCE_SENTINEL && expires == 0 => {}
        _ => {
            return Err(incompatible_schema(
                "concurrent write mode marker and legacy fence disagree",
            ));
        }
    }
    tx.execute(
        "UPDATE mount_rs_metadata
         SET volume_id = 'sqlite-' || lower(hex(randomblob(16)))
         WHERE id=1 AND (volume_id IS NULL OR volume_id='')",
        [],
    )
    .map_err(backend_error)?;
    let volume_text: String = tx
        .query_row(
            "SELECT volume_id FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    let volume = VolumeId::new(volume_text.clone())
        .map_err(|_| incompatible_schema("stored SQLite provider volume_id is invalid"))?;

    let version_tables = [
        (
            "mount_rs_version_state",
            [
                "id",
                "volume_id",
                "head_id",
                "next_sequence",
                "next_read_fence",
            ]
            .as_slice(),
        ),
        (
            "mount_rs_versions",
            [
                "id",
                "volume_id",
                "sequence",
                "parent_id",
                "restored_from",
                "forked_from",
                "namespace",
                "block_store_id",
                "kind",
                "created_at_ms",
                "durable",
                "operation_id",
            ]
            .as_slice(),
        ),
        (
            "mount_rs_version_pins",
            [
                "view_id",
                "volume_id",
                "version_id",
                "owner",
                "fence",
                "expires",
            ]
            .as_slice(),
        ),
    ];
    for (table, columns) in version_tables {
        if let Some(existing) = table_columns(&tx, table)? {
            require_columns(table, &existing, columns)?;
        } else if schema_is_current {
            return Err(incompatible_schema(format!(
                "current versioning schema is missing {table}"
            )));
        }
    }
    if !schema_is_current {
        tx.execute_batch(VERSION_SCHEMA).map_err(backend_error)?;
    }
    for (table, columns) in version_tables {
        let existing = table_columns(&tx, table)?
            .ok_or_else(|| incompatible_schema(format!("{table} table is missing")))?;
        require_columns(table, &existing, columns)?;
    }
    require_primary_key(&tx, "mount_rs_version_state", "id")?;
    require_primary_key(&tx, "mount_rs_versions", "id")?;
    require_unique_column(&tx, "mount_rs_versions", "operation_id")?;
    require_primary_key(&tx, "mount_rs_version_pins", "view_id")?;

    let state_volume: Option<String> = tx
        .query_row(
            "SELECT volume_id FROM mount_rs_version_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend_error)?;
    match state_volume {
        Some(state_volume) if state_volume != volume_text => {
            return Err(incompatible_schema(
                "version state belongs to another provider volume",
            ));
        }
        Some(_) => {}
        None => {
            tx.execute(
                "INSERT INTO mount_rs_version_state
                 (id, volume_id, head_id, next_sequence, next_read_fence)
                 VALUES (1, ?1, NULL, 1, 0)",
                params![volume_text],
            )
            .map_err(backend_error)?;
        }
    }
    let invalid_state: bool = tx
        .query_row(
            "SELECT next_sequence<=0 OR next_read_fence<0
             FROM mount_rs_version_state WHERE id=1",
            [],
            |row| row.get::<_, Option<bool>>(0),
        )
        .map_err(backend_error)?
        .unwrap_or(true);
    if invalid_state {
        return Err(incompatible_schema("version state counters are invalid"));
    }
    let invalid_versions: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM mount_rs_versions
             WHERE volume_id IS NULL OR volume_id<>?1)",
            params![volume.0],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    let invalid_pins: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM mount_rs_version_pins
             WHERE volume_id IS NULL OR volume_id<>?1)",
            params![volume.0],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    if invalid_versions || invalid_pins {
        return Err(incompatible_schema(
            "version records or pins cross provider volumes",
        ));
    }
    let persisted_head: Option<String> = tx
        .query_row(
            "SELECT head_id FROM mount_rs_version_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    if let Some(head_id) = persisted_head {
        let head = VersionId::decode(&head_id)
            .map_err(|_| incompatible_schema("stored version head is malformed"))?;
        if head.volume != volume {
            return Err(incompatible_schema(
                "stored version head belongs to another provider volume",
            ));
        }
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mount_rs_versions
                 WHERE id=?1 AND volume_id=?2)",
                params![head_id, volume.0],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        if !exists {
            return Err(incompatible_schema(
                "stored version head references a missing version",
            ));
        }
    }

    tx.execute(
        "INSERT INTO mount_rs_schema_versions(schema_name, schema_version)
         VALUES (?1, ?2)
         ON CONFLICT(schema_name) DO UPDATE SET schema_version=excluded.schema_version",
        params![VERSION_SCHEMA_NAME, VERSION_SCHEMA_VERSION],
    )
    .map_err(backend_error)?;
    tx.commit().map_err(backend_error)
}

fn table_columns(connection: &Connection, table: &str) -> Result<Option<BTreeSet<String>>> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            params![table],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    if !exists {
        return Ok(None);
    }
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(backend_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(backend_error)?;
    let mut columns = BTreeSet::new();
    for row in rows {
        columns.insert(row.map_err(backend_error)?);
    }
    Ok(Some(columns))
}

fn require_primary_key(connection: &Connection, table: &str, column: &str) -> Result<()> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(backend_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)? > 0))
        })
        .map_err(backend_error)?;
    for row in rows {
        let (name, primary_key) = row.map_err(backend_error)?;
        if name == column && primary_key {
            return Ok(());
        }
    }
    Err(incompatible_schema(format!(
        "{table}.{column} must be a primary key"
    )))
}

fn require_unique_column(connection: &Connection, table: &str, column: &str) -> Result<()> {
    let indexes = {
        let mut statement = connection
            .prepare(&format!("PRAGMA index_list(\"{table}\")"))
            .map_err(backend_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)? != 0))
            })
            .map_err(backend_error)?;
        let mut indexes = Vec::new();
        for row in rows {
            indexes.push(row.map_err(backend_error)?);
        }
        indexes
    };

    for (index, unique) in indexes {
        if !unique {
            continue;
        }
        let pragma_index = index.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("PRAGMA index_info(\"{pragma_index}\")"))
            .map_err(backend_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, Option<String>>(2))
            .map_err(backend_error)?;
        let mut columns = Vec::new();
        for row in rows {
            if let Some(name) = row.map_err(backend_error)? {
                columns.push(name);
            }
        }
        if columns.len() == 1 && columns[0] == column {
            return Ok(());
        }
    }

    Err(incompatible_schema(format!(
        "{table}.{column} must have a unique constraint"
    )))
}

fn require_columns(table: &str, actual: &BTreeSet<String>, expected: &[&str]) -> Result<()> {
    let missing = expected
        .iter()
        .filter(|column| !actual.contains(**column))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(incompatible_schema(format!(
            "{table} is missing required columns: {}",
            missing.join(", ")
        )))
    }
}

fn incompatible_schema(message: impl Into<String>) -> FsError {
    FsError::new(ErrorCode::Enotsup)
        .with_syscall("sqlite schema")
        .with_message(message.into())
}

fn load_volume_id(database: &Database) -> Result<VolumeId> {
    let value: String = database
        .lock()?
        .query_row(
            "SELECT volume_id FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    VolumeId::new(value)
}

/// Fenced single-writer metadata transactions; no file bytes are stored here.
#[derive(Clone)]
pub struct SqliteMetadataStore(Database, VolumeId);

impl SqliteMetadataStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let database = Database::open(Some(path.as_ref()), METADATA_SCHEMA)?;
        let store = Self::from_database(database)?;
        Ok(store)
    }
    pub fn in_memory() -> Result<Self> {
        Self::from_database(Database::open(None, METADATA_SCHEMA)?)
    }

    fn from_database(database: Database) -> Result<Self> {
        initialize_version_schema(&database)?;
        let volume_id = load_volume_id(&database)?;
        Ok(Self(database, volume_id))
    }
}

/// Immutable blocks, independently configurable from the metadata database.
#[derive(Clone)]
pub struct SqliteBlockStore(Database);

impl SqliteBlockStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self(Database::open(Some(path.as_ref()), BLOCK_SCHEMA)?))
    }
    pub fn in_memory() -> Result<Self> {
        Ok(Self(Database::open(None, BLOCK_SCHEMA)?))
    }

    fn put_once(&self, bytes: &[u8]) -> Result<BlockId> {
        let mut connection = self.0.lock()?;
        // Database-generated random identities avoid accidental aliasing when
        // namespaces use distinct block databases. Collisions fail, never overwrite.
        let id: String = connection
            .query_row("SELECT lower(hex(randomblob(32)))", [], |row| row.get(0))
            .map_err(backend_error)?;
        let was_autocommit = connection.is_autocommit();
        let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate)
        {
            Ok(transaction) => transaction,
            Err(error) => {
                return Err(
                    sqlite_busy_known_noncommit(&error, was_autocommit, "put block")
                        .unwrap_or_else(|| backend_error(error)),
                );
            }
        };
        let inserted = transaction
            .execute(
                "INSERT INTO mount_rs_blocks(id,bytes) VALUES(?1,?2)",
                params![id, bytes],
            )
            .map_err(backend_error)?;
        if inserted != 1 {
            return Err(FsError::new(ErrorCode::Eio)
                .with_syscall("put block")
                .with_message("SQLite block insert was not applied"));
        }
        if let Err(error) = transaction.commit() {
            return Err(sqlite_busy_known_noncommit(
                &error,
                connection.is_autocommit(),
                "put block",
            )
            .unwrap_or_else(|| backend_error(error)));
        }
        Ok(BlockId(id))
    }
}

fn ttl_ms(ttl: Duration) -> Result<i64> {
    let value = i64::try_from(ttl.as_millis()).map_err(|_| FsError::new(ErrorCode::Einval))?;
    if value == 0 {
        return Err(FsError::new(ErrorCode::Einval));
    }
    Ok(value)
}

fn lease_numbers(lease: &WriterLease) -> Result<(i64, i64)> {
    Ok((
        i64::try_from(lease.fence).map_err(|_| stale())?,
        i64::try_from(lease.expires_at_ms).map_err(|_| stale())?,
    ))
}
fn stale() -> FsError {
    FsError::new(ErrorCode::Estale).with_syscall("metadata lease")
}

fn version_kind_name(kind: &VersionKind) -> &'static str {
    match kind {
        VersionKind::Initial => "initial",
        VersionKind::Snapshot => "snapshot",
        VersionKind::Restore => "restore",
        VersionKind::Fork => "fork",
    }
}

fn parse_version_kind(value: &str) -> Result<VersionKind> {
    match value {
        "initial" => Ok(VersionKind::Initial),
        "snapshot" => Ok(VersionKind::Snapshot),
        "restore" => Ok(VersionKind::Restore),
        "fork" => Ok(VersionKind::Fork),
        _ => Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("load version")
            .with_message(format!("unknown version kind '{value}'"))),
    }
}

#[derive(Debug)]
struct RawVersion {
    id: String,
    parent_id: Option<String>,
    restored_from: Option<String>,
    forked_from: Option<String>,
    namespace: String,
    block_store_id: String,
    kind: String,
    created_at_ms: i64,
    durable: bool,
    volume_id: String,
    sequence: u64,
}

fn raw_version(row: &Row<'_>) -> rusqlite::Result<RawVersion> {
    Ok(RawVersion {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        restored_from: row.get(2)?,
        forked_from: row.get(3)?,
        namespace: row.get(4)?,
        block_store_id: row.get(5)?,
        kind: row.get(6)?,
        created_at_ms: row.get(7)?,
        durable: row.get::<_, i64>(8)? != 0,
        volume_id: row.get(9)?,
        sequence: row.get(10)?,
    })
}

fn decode_raw_version(raw: RawVersion) -> Result<VersionInfo> {
    let volume = VolumeId::new(raw.volume_id)?;
    let id = VersionId::new(volume.clone(), raw.sequence)?;
    if raw.id != id.encode() {
        return Err(FsError::new(ErrorCode::Einval)
            .with_syscall("load version")
            .with_message("stored version id does not match its volume and sequence"));
    }
    let parent = raw
        .parent_id
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let restored_from = raw
        .restored_from
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let forked_from = raw
        .forked_from
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let namespace = serde_json::from_str(&raw.namespace).map_err(backend_error)?;
    let block_store_id = mount_rs_core::versioning::BlockStoreId::new(raw.block_store_id)?;
    let info = VersionInfo {
        id,
        parent,
        restored_from,
        forked_from,
        kind: parse_version_kind(&raw.kind)?,
        namespace,
        block_store_id,
        created_at_ms: raw.created_at_ms,
        durable: raw.durable,
    };
    info.validate()?;
    Ok(info)
}

const VERSION_SELECT: &str = "SELECT id, parent_id, restored_from, forked_from,
 namespace, block_store_id, kind, created_at_ms, durable, volume_id, sequence
 FROM mount_rs_versions";

fn load_version_from_connection(connection: &Connection, id: &VersionId) -> Result<VersionInfo> {
    let raw = connection
        .query_row(
            &format!("{VERSION_SELECT} WHERE id=?1"),
            params![id.encode()],
            raw_version,
        )
        .optional()
        .map_err(backend_error)?
        .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("load version"))?;
    let info = decode_raw_version(raw)?;
    info.validate_for_volume(&id.volume)?;
    if info.id != *id {
        return Err(FsError::new(ErrorCode::Enoent).with_syscall("load version"));
    }
    Ok(info)
}

#[async_trait]
impl MetadataStore for SqliteMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    fn publish_includes_flush_barrier(&self) -> bool {
        // SQLite is opened with synchronous=FULL and publish commits an
        // autocommit transaction before returning.
        true
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let connection = self.0.lock()?;
        let (revision, namespace): (u64, Option<String>) = connection
            .query_row(
                "SELECT revision, namespace FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(backend_error)?;
        Ok(LoadedMetadata {
            revision,
            namespace: namespace
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(backend_error)?,
        })
    }

    async fn prepare_concurrent_mode(&self) -> Result<()> {
        if !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("prepare concurrent SQLite volume")
                .with_message(
                    "independent concurrent writers require a file-backed SQLite database",
                ));
        }
        let mut connection = self.0.lock()?;
        #[cfg(target_os = "macos")]
        {
            let path = connection
                .path()
                .filter(|path| !path.is_empty())
                .ok_or_else(|| {
                    FsError::new(ErrorCode::Eio)
                        .with_syscall("inspect concurrent SQLite backing")
                        .with_message("SQLite metadata file path is unavailable")
                })?;
            require_local_concurrent_backing(Path::new(path), "metadata")?;
        }
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let (mode, revision, namespace, owner, fence, expires): (
            Option<String>,
            i64,
            Option<String>,
            Option<String>,
            i64,
            i64,
        ) = tx
            .query_row(
                "SELECT write_mode, revision, namespace, owner, fence, expires
                 FROM mount_rs_metadata WHERE id=1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .map_err(backend_error)?;
        if mode.as_deref() == Some(CONCURRENT_WRITE_MODE) {
            if owner.is_none() && fence == CONCURRENT_FENCE_SENTINEL && expires == 0 {
                return Ok(());
            }
            return Err(backend_error(
                "SQLite concurrent mode marker and fence sentinel disagree",
            ));
        }
        if mode.is_some() {
            return Err(incompatible_schema(
                "unsupported SQLite concurrent write mode",
            ));
        }
        let (head, versions, pins): (Option<String>, i64, i64) = tx
            .query_row(
                "SELECT head_id,
                  (SELECT count(*) FROM mount_rs_versions),
                  (SELECT count(*) FROM mount_rs_version_pins)
                 FROM mount_rs_version_state WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .map_err(backend_error)?;
        if revision != 0
            || namespace.is_some()
            || owner.is_some()
            || fence != 0
            || expires != 0
            || head.is_some()
            || versions != 0
            || pins != 0
        {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("prepare concurrent SQLite volume")
                .with_message("a fenced legacy volume needs an offline migration"));
        }
        let changed = tx
            .execute(
                "UPDATE mount_rs_metadata SET write_mode=?1, fence=?2
             WHERE id=1 AND write_mode IS NULL AND revision=0 AND namespace IS NULL
               AND owner IS NULL AND fence=0 AND expires=0",
                params![CONCURRENT_WRITE_MODE, CONCURRENT_FENCE_SENTINEL],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            return Err(backend_error(
                "SQLite concurrent mode update returned an unexplained zero-row result",
            ));
        }
        tx.commit().map_err(backend_error)
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        if owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let ttl = ttl_ms(ttl)?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET owner=?1, fence=fence+1, expires={NOW}+?2
            WHERE id=1 AND write_mode IS NULL AND (owner IS NULL OR expires<={NOW})
              AND fence<9223372036854775807 AND ?2<=9223372036854775807-{NOW}"
        );
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let changed = tx
            .execute(&sql, params![owner, ttl])
            .map_err(backend_error)?;
        if changed > 1 {
            return Err(backend_error(
                "SQLite writer acquisition changed multiple rows",
            ));
        }
        let lease = if changed == 1 {
            let (fence, expires_at_ms): (u64, u64) = tx
                .query_row(
                    "SELECT fence, expires FROM mount_rs_metadata WHERE id=1 AND owner=?1",
                    params![owner],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(backend_error)?;
            Some(WriterLease {
                owner: owner.to_owned(),
                fence,
                expires_at_ms,
            })
        } else {
            None
        };
        let mode: Option<String> = if lease.is_none() {
            tx.query_row(
                "SELECT write_mode FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?
        } else {
            None
        };
        tx.commit().map_err(backend_error)?;
        if let Some(lease) = lease {
            return Ok(lease);
        }
        if mode.is_some() {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("acquire writer")
                .with_message("SQLite volume is in concurrent write mode"));
        }
        Err(FsError::new(ErrorCode::Eagain).with_syscall("acquire writer"))
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        let ttl = ttl_ms(ttl)?;
        let (fence, expires) = lease_numbers(lease)?;
        let mut connection = self.0.lock()?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET expires={NOW}+?4
            WHERE id=1 AND write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}
              AND ?4<=9223372036854775807-{NOW}"
        );
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let changed = tx
            .execute(&sql, params![lease.owner, fence, expires, ttl])
            .map_err(backend_error)?;
        if changed > 1 {
            return Err(backend_error("SQLite writer renewal changed multiple rows"));
        }
        let expiry: Option<u64> = if changed == 1 {
            Some(
                tx.query_row(
                    "SELECT expires FROM mount_rs_metadata WHERE id=1 AND owner=?1 AND fence=?2",
                    params![lease.owner, fence],
                    |row| row.get(0),
                )
                .map_err(backend_error)?,
            )
        } else {
            None
        };
        tx.commit().map_err(backend_error)?;
        let expiry = expiry.ok_or_else(stale)?;
        Ok(WriterLease {
            expires_at_ms: expiry,
            ..lease.clone()
        })
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        let (fence, expires) = lease_numbers(lease)?;
        let connection = self.0.lock()?;
        let changed = connection
            .execute(
                &format!(
                    "UPDATE mount_rs_metadata SET owner=NULL, expires=0
            WHERE id=1 AND write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}"
                ),
                params![lease.owner, fence, expires],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            return Err(stale());
        }
        Ok(())
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        let expected =
            i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let next = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let (fence, expires) = lease_numbers(lease)?;
        let namespace = serde_json::to_string(&namespace).map_err(backend_error)?;
        let mut connection = self.0.lock()?;
        // The successful publication path is one fenced conditional UPDATE.
        // SQLite makes a single autocommit statement atomic and durable under
        // the configured synchronous policy, so avoid opening and committing
        // a second explicit transaction for the common case. A zero-row
        // result opens the Immediate transaction only for the locked
        // stale/revision classification below.
        let changed = connection
            .execute(
                &format!(
                    "UPDATE mount_rs_metadata SET revision=?1, namespace=?2
                     WHERE id=1 AND write_mode IS NULL AND revision=?3 AND owner=?4 AND fence=?5
                       AND expires=?6 AND expires>{NOW}"
                ),
                params![next, namespace, expected, lease.owner, fence, expires],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            // The conditional update is the successful-path CAS. Only the
            // exceptional path needs a read to preserve the stale-versus-
            // revision-conflict classification of the former lease preflight.
            let tx = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(backend_error)?;
            let (valid, actual_revision): (bool, i64) = tx
                .query_row(
                    &format!(
                        "SELECT write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}, revision
                         FROM mount_rs_metadata WHERE id=1"
                    ),
                    params![lease.owner, fence, expires],
                    |row| Ok((row.get::<_, Option<bool>>(0)?.unwrap_or(false), row.get(1)?)),
                )
                .map_err(backend_error)?;
            if !valid {
                return Err(stale());
            }
            if actual_revision != expected {
                return Err(FsError::new(ErrorCode::Eagain).with_syscall("publish metadata"));
            }
            // The row was still valid and at the expected revision, so an
            // unexplained zero-row CAS fails closed.
            return Err(stale());
        }
        Ok(next as u64)
    }

    async fn publish_if_revision(
        &self,
        expected_revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        namespace.validate()?;
        let expected =
            i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let next = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let namespace_json = serde_json::to_string(&namespace).map_err(backend_error)?;
        self.0.with_concurrent_publish_timeout(|connection| {
            let was_autocommit = connection.is_autocommit();
            let tx = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
                Ok(tx) => tx,
                Err(error) => {
                    return Err(sqlite_busy_known_noncommit(
                        &error,
                        was_autocommit,
                        "publish concurrent SQLite metadata",
                    )
                    .unwrap_or_else(|| backend_error(error)));
                }
            };
            let (mode, owner, fence, expires, actual_revision): (
                Option<String>,
                Option<String>,
                i64,
                i64,
                i64,
            ) = tx
                .query_row(
                    "SELECT write_mode, owner, fence, expires, revision
                 FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                        ))
                    },
                )
                .map_err(backend_error)?;
            if mode.as_deref() != Some(CONCURRENT_WRITE_MODE)
                || owner.is_some()
                || fence != CONCURRENT_FENCE_SENTINEL
                || expires != 0
            {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("publish concurrent SQLite metadata")
                    .with_message("concurrent mode is missing or its legacy fence is invalid"));
            }
            if actual_revision != expected {
                return Err(FsError::new(ErrorCode::Eagain)
                    .with_syscall("publish concurrent SQLite metadata"));
            }
            let changed = tx
                .execute(
                    "UPDATE mount_rs_metadata SET revision=?1, namespace=?2
                 WHERE id=1 AND write_mode=?3 AND owner IS NULL AND fence=?4
                   AND expires=0 AND revision=?5",
                    params![
                        next,
                        namespace_json,
                        CONCURRENT_WRITE_MODE,
                        CONCURRENT_FENCE_SENTINEL,
                        expected
                    ],
                )
                .map_err(backend_error)?;
            if changed != 1 {
                return Err(backend_error(
                    "SQLite concurrent publication CAS returned an unexplained zero-row update",
                ));
            }
            if let Err(error) = tx.commit() {
                return Err(sqlite_busy_known_noncommit(
                    &error,
                    connection.is_autocommit(),
                    "publish concurrent SQLite metadata",
                )
                .unwrap_or_else(|| backend_error(error)));
            }
            Ok(next as u64)
        })
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush()
    }
}

#[async_trait]
impl VersionedMetadataStore for SqliteMetadataStore {
    fn volume_id(&self) -> VolumeId {
        self.1.clone()
    }

    async fn version_head(&self) -> Result<Option<VersionHead>> {
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(backend_error)?;
        let revision: i64 = tx
            .query_row(
                "SELECT revision FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        let head_id: Option<String> = tx
            .query_row(
                "SELECT head_id FROM mount_rs_version_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        let head = if let Some(id) = head_id {
            let version = VersionId::decode(&id).map_err(|_| {
                FsError::new(ErrorCode::Eio)
                    .with_syscall("version head")
                    .with_message("stored version head is malformed")
            })?;
            if version.volume != self.1 {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("version head")
                    .with_message("stored version head belongs to another volume"));
            }
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM mount_rs_versions
                     WHERE id=?1 AND volume_id=?2)",
                    params![id, self.1.0],
                    |row| row.get(0),
                )
                .map_err(backend_error)?;
            if !exists {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("version head")
                    .with_message("stored version head references a missing version"));
            }
            Some(VersionHead {
                version,
                revision: u64::try_from(revision).map_err(|_| FsError::new(ErrorCode::Eio))?,
            })
        } else {
            None
        };
        tx.commit().map_err(backend_error)?;
        Ok(head)
    }

    async fn load_version(&self, id: &VersionId) -> Result<VersionInfo> {
        if id.volume != self.1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("load version"));
        }
        let connection = self.0.lock()?;
        load_version_from_connection(&connection, id)
    }

    async fn list_versions(&self) -> Result<Vec<VersionInfo>> {
        let connection = self.0.lock()?;
        let mut statement = connection
            .prepare(&format!("{VERSION_SELECT} ORDER BY sequence"))
            .map_err(backend_error)?;
        let rows = statement
            .query_map([], raw_version)
            .map_err(backend_error)?;
        let mut versions = Vec::new();
        for row in rows {
            let version = decode_raw_version(row.map_err(backend_error)?)?;
            version.validate_for_volume(&self.1)?;
            versions.push(version);
        }
        Ok(versions)
    }

    async fn find_publication(&self, operation_id: &PublicationId) -> Result<Option<VersionInfo>> {
        let connection = self.0.lock()?;
        let id: Option<String> = connection
            .query_row(
                "SELECT id FROM mount_rs_versions WHERE operation_id=?1",
                params![operation_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend_error)?;
        id.map(|id| {
            let version = VersionId::decode(&id)?;
            load_version_from_connection(&connection, &version)
        })
        .transpose()
    }

    async fn publish_version(
        &self,
        lease: &WriterLease,
        publication: VersionPublication,
    ) -> Result<VersionInfo> {
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;

        let existing_id: Option<String> = tx
            .query_row(
                "SELECT id FROM mount_rs_versions WHERE operation_id=?1",
                params![publication.operation_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend_error)?;
        if let Some(existing_id) = existing_id {
            let raw = tx
                .query_row(
                    &format!("{VERSION_SELECT} WHERE id=?1"),
                    params![existing_id],
                    raw_version,
                )
                .map_err(backend_error)?;
            let existing = decode_raw_version(raw)?;
            existing.validate_for_volume(&self.1)?;
            if !publication.matches_committed(&existing)? {
                return Err(FsError::new(ErrorCode::Eexist)
                    .with_syscall("publish version")
                    .with_message("publication id was reused with a different payload"));
            }
            tx.commit().map_err(backend_error)?;
            return Ok(existing);
        }

        // Only a new publication reaches the writer-lease and CAS checks.
        // A committed operation is immutable and may be reconciled after the
        // original writer lease has expired.
        publication.validate(&self.1)?;
        if publication.durable && !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("publish version")
                .with_message("in-memory SQLite cannot publish a durable version"));
        }
        let expected_revision = i64::try_from(publication.expected_revision)
            .map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let (fence, expires) = lease_numbers(lease)?;
        let valid: bool = tx
            .query_row(
                &format!(
                    "SELECT write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}
                     FROM mount_rs_metadata WHERE id=1"
                ),
                params![lease.owner, fence, expires],
                |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
            )
            .map_err(backend_error)?;
        if !valid {
            return Err(stale());
        }
        let (actual_revision, head_id): (i64, Option<String>) = tx
            .query_row(
                "SELECT revision, (SELECT head_id FROM mount_rs_version_state WHERE id=1)
                 FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(backend_error)?;
        if actual_revision != expected_revision {
            return Err(FsError::new(ErrorCode::Eagain)
                .with_syscall("publish version")
                .with_message(format!(
                    "metadata revision conflict: expected {}, actual {actual_revision}",
                    publication.expected_revision
                )));
        }
        if let Some(head_id) = head_id.as_deref() {
            let exists: bool = tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM mount_rs_versions
                     WHERE id=?1 AND volume_id=?2)",
                    params![head_id, self.1.0],
                    |row| row.get(0),
                )
                .map_err(backend_error)?;
            if !exists {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("publish version")
                    .with_message("stored version head references a missing version"));
            }
        }
        let expected_parent = publication.expected_parent.as_ref().map(VersionId::encode);
        if head_id != expected_parent {
            return Err(FsError::new(ErrorCode::Eagain)
                .with_syscall("publish version")
                .with_message("version head changed concurrently"));
        }

        let next_sequence: i64 = tx
            .query_row(
                "SELECT next_sequence FROM mount_rs_version_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        let sequence = u64::try_from(next_sequence)
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("invalid version sequence"))?;
        let id = VersionId::new(self.1.clone(), sequence)?;
        let created_at_ms: i64 = tx
            .query_row(NOW_SELECT, [], |row| row.get(0))
            .map_err(backend_error)?;
        let namespace = serde_json::to_string(&publication.namespace).map_err(backend_error)?;
        let parent_id = publication.expected_parent.as_ref().map(VersionId::encode);
        let restored_from = publication.restored_from.as_ref().map(VersionId::encode);
        let forked_from = publication.forked_from.as_ref().map(VersionId::encode);
        tx.execute(
            "INSERT INTO mount_rs_versions
             (id, volume_id, sequence, parent_id, restored_from, forked_from,
              namespace, block_store_id, kind, created_at_ms, durable, operation_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id.encode(),
                self.1.0,
                i64::try_from(sequence).map_err(|_| FsError::new(ErrorCode::Eoverflow))?,
                parent_id,
                restored_from,
                forked_from,
                namespace,
                publication.block_store_id.0,
                version_kind_name(&publication.kind),
                created_at_ms,
                if publication.durable { 1_i64 } else { 0_i64 },
                publication.operation_id.0,
            ],
        )
        .map_err(backend_error)?;
        let revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let namespace_json =
            serde_json::to_string(&publication.namespace).map_err(backend_error)?;
        let changed = tx
            .execute(
                "UPDATE mount_rs_metadata SET revision=?1, namespace=?2
                 WHERE id=1 AND write_mode IS NULL AND revision=?3",
                params![revision, namespace_json, expected_revision],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            return Err(FsError::new(ErrorCode::Eagain).with_syscall("publish version"));
        }
        tx.execute(
            "UPDATE mount_rs_version_state SET head_id=?1, next_sequence=?2 WHERE id=1",
            params![
                id.encode(),
                next_sequence
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?
            ],
        )
        .map_err(backend_error)?;
        tx.commit().map_err(backend_error)?;

        Ok(VersionInfo {
            id,
            parent: publication.expected_parent,
            restored_from: publication.restored_from,
            forked_from: publication.forked_from,
            kind: publication.kind,
            namespace: publication.namespace,
            block_store_id: publication.block_store_id,
            created_at_ms,
            durable: publication.durable,
        })
    }

    async fn open_view_pin(&self, id: &VersionId, request: ReadLeaseRequest) -> Result<ReadLease> {
        let ttl_ms = request.validate()?;
        let ttl_ms = i64::try_from(ttl_ms).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        if id.volume != self.1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("open version view"));
        }
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mount_rs_versions
                 WHERE id=?1 AND volume_id=?2)",
                params![id.encode(), self.1.0],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        if !exists {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("open version view"));
        }
        let now_ms: i64 = tx
            .query_row(NOW_SELECT, [], |row| row.get(0))
            .map_err(backend_error)?;
        let next_fence: i64 = tx
            .query_row(
                "SELECT next_read_fence FROM mount_rs_version_state WHERE id=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(backend_error)?
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let expires = now_ms
            .checked_add(ttl_ms)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let view_id = format!("{}-view-{next_fence}", self.1);
        tx.execute(
            "UPDATE mount_rs_version_state SET next_read_fence=?1 WHERE id=1",
            params![next_fence],
        )
        .map_err(backend_error)?;
        tx.execute(
            "INSERT INTO mount_rs_version_pins
             (view_id, volume_id, version_id, owner, fence, expires)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                view_id,
                self.1.0,
                id.encode(),
                request.owner,
                next_fence,
                expires
            ],
        )
        .map_err(backend_error)?;
        tx.commit().map_err(backend_error)?;
        Ok(ReadLease {
            volume: self.1.clone(),
            version: id.clone(),
            view_id,
            owner: request.owner,
            fence: u64::try_from(next_fence).map_err(|_| FsError::new(ErrorCode::Eio))?,
            expires_at_ms: u64::try_from(expires).map_err(|_| FsError::new(ErrorCode::Eio))?,
        })
    }

    async fn renew_view_pin(
        &self,
        lease: &ReadLease,
        request: ReadLeaseRequest,
    ) -> Result<ReadLease> {
        let ttl_ms = request.validate()?;
        let ttl_ms = i64::try_from(ttl_ms).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let fence = i64::try_from(lease.fence).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let expires =
            i64::try_from(lease.expires_at_ms).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let current: Option<(String, String, String, i64, i64)> = tx
            .query_row(
                "SELECT volume_id, version_id, owner, fence, expires
                 FROM mount_rs_version_pins WHERE view_id=?1",
                params![lease.view_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(backend_error)?;
        let Some((volume, version, owner, current_fence, current_expires)) = current else {
            return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
        };
        let now_ms: i64 = tx
            .query_row(NOW_SELECT, [], |row| row.get(0))
            .map_err(backend_error)?;
        if volume != self.1.0
            || version != lease.version.encode()
            || owner != request.owner
            || owner != lease.owner
            || current_fence != fence
            || current_expires != expires
            || current_expires <= now_ms
        {
            return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
        }
        let next_expires = now_ms
            .checked_add(ttl_ms)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        tx.execute(
            "UPDATE mount_rs_version_pins SET expires=?1 WHERE view_id=?2
             AND fence=?3 AND expires=?4",
            params![next_expires, lease.view_id, fence, expires],
        )
        .map_err(backend_error)?;
        tx.commit().map_err(backend_error)?;
        Ok(ReadLease {
            expires_at_ms: u64::try_from(next_expires).map_err(|_| FsError::new(ErrorCode::Eio))?,
            ..lease.clone()
        })
    }

    async fn close_view_pin(&self, lease: &ReadLease) -> Result<()> {
        let fence = i64::try_from(lease.fence).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let expires =
            i64::try_from(lease.expires_at_ms).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let changed = tx
            .execute(
                "DELETE FROM mount_rs_version_pins
                 WHERE view_id=?1 AND volume_id=?2 AND version_id=?3 AND owner=?4
                   AND fence=?5 AND expires=?6",
                params![
                    lease.view_id,
                    self.1.0,
                    lease.version.encode(),
                    lease.owner,
                    fence,
                    expires
                ],
            )
            .map_err(backend_error)?;
        if changed == 1 {
            tx.commit().map_err(backend_error)?;
            return Ok(());
        }
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mount_rs_version_pins WHERE view_id=?1)",
                params![lease.view_id],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        tx.commit().map_err(backend_error)?;
        if exists {
            Err(FsError::new(ErrorCode::Estale).with_syscall("close view"))
        } else {
            // Missing pins are intentionally idempotent so cleanup after a
            // provider expiry or an earlier close does not fail the caller.
            Ok(())
        }
    }

    async fn delete_version(&self, lease: &WriterLease, id: &VersionId) -> Result<()> {
        if id.volume != self.1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("delete version"));
        }
        let (fence, expires) = lease_numbers(lease)?;
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let valid: bool = tx
            .query_row(
                &format!(
                    "SELECT write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}
                     FROM mount_rs_metadata WHERE id=1"
                ),
                params![lease.owner, fence, expires],
                |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
            )
            .map_err(backend_error)?;
        if !valid {
            return Err(stale());
        }
        let head_id: Option<String> = tx
            .query_row(
                "SELECT head_id FROM mount_rs_version_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        if head_id.as_deref() == Some(&id.encode()) {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("delete version")
                .with_message("current version cannot be deleted"));
        }
        let protected: bool = tx
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM mount_rs_version_pins
                     WHERE version_id=?1 AND expires>{NOW})"
                ),
                params![id.encode()],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        if protected {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("delete version")
                .with_message("version has an active read lease"));
        }
        let changed = tx
            .execute(
                "DELETE FROM mount_rs_versions WHERE id=?1",
                params![id.encode()],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("delete version"));
        }
        tx.commit().map_err(backend_error)?;
        Ok(())
    }
}

#[async_trait]
impl BlockStore for SqliteBlockStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    async fn prepare_concurrent_mode(&self) -> Result<()> {
        if !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("prepare concurrent SQLite blocks")
                .with_message(
                    "independent concurrent writers require a file-backed SQLite block database",
                ));
        }
        #[cfg(target_os = "macos")]
        {
            let connection = self.0.lock()?;
            let path = connection
                .path()
                .filter(|path| !path.is_empty())
                .ok_or_else(|| {
                    FsError::new(ErrorCode::Eio)
                        .with_syscall("inspect concurrent SQLite backing")
                        .with_message("SQLite blocks file path is unavailable")
                })?;
            require_local_concurrent_backing(Path::new(path), "blocks")?;
        }
        Ok(())
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let started = Instant::now();
        for attempt in 0..MAX_SQLITE_BUSY_RETRIES {
            match self.put_once(bytes) {
                Ok(id) => return Ok(id),
                Err(error) if error.code == ErrorCode::Eagain => {
                    if attempt + 1 == MAX_SQLITE_BUSY_RETRIES
                        || started.elapsed() >= SQLITE_BUSY_RETRY_BUDGET
                    {
                        return Err(error.with_message(
                            "SQLite block writer remained busy after bounded retries",
                        ));
                    }
                    // Each put_once releases the SQLite connection lock before
                    // yielding, so another writer can finish its transaction.
                    sqlite_busy_backoff(attempt).await;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("bounded SQLite block retry loop always returns")
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.0
            .lock()?
            .query_row(
                "SELECT bytes FROM mount_rs_blocks WHERE id=?1",
                params![id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend_error)?
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("get block"))
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush()
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.0
            .lock()?
            .execute("DELETE FROM mount_rs_blocks WHERE id=?1", params![id.0])
            .map_err(backend_error)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::{
        FsDriver,
        chunking::{Chunker, FixedSizeChunker},
        storage::{NodeData, NodeMetadata},
    };
    use mount_rs_memfs::MemoryFs;
    use std::{
        collections::BTreeMap,
        future::Future,
        task::{Context, Poll, Waker},
        time::Instant,
    };

    fn run<T>(future: impl Future<Output = T>) -> T {
        let mut future = Box::pin(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("SQLite operations must complete synchronously"),
        }
    }

    fn namespace() -> Namespace {
        let stats = run(MemoryFs::empty().stat("/")).unwrap();
        let root = stats.ino;
        Namespace {
            format_version: 1,
            root,
            next_inode: root + 1,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: FixedSizeChunker::new(4096).unwrap().config(),
            nodes: BTreeMap::from([(
                root,
                NodeMetadata {
                    stats,
                    data: NodeData::Directory { entries: vec![] },
                },
            )]),
        }
    }

    #[test]
    fn concurrent_publish_begin_busy_is_a_known_noncommit() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        run(store.prepare_concurrent_mode()).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();

        let error = run(store.publish_if_revision(0, namespace())).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        assert!(store.0.lock().unwrap().is_autocommit());
        holder.execute_batch("ROLLBACK").unwrap();
        let unloaded = run(store.load()).unwrap();
        assert_eq!(unloaded.revision, 0);
        assert!(unloaded.namespace.is_none());
        assert_eq!(run(store.publish_if_revision(0, namespace())).unwrap(), 1);

        drop(holder);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_publish_commit_busy_rolls_back_before_retry() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        run(store.prepare_concurrent_mode()).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let revision: i64 = reader
            .query_row(
                "SELECT revision FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 0);

        let error = run(store.publish_if_revision(0, namespace())).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        assert!(store.0.lock().unwrap().is_autocommit());
        reader.execute_batch("ROLLBACK").unwrap();
        let unloaded = run(store.load()).unwrap();
        assert_eq!(unloaded.revision, 0);
        assert!(unloaded.namespace.is_none());
        assert_eq!(run(store.publish_if_revision(0, namespace())).unwrap(), 1);

        drop(reader);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_publish_begin_busy_uses_scoped_short_timeout() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        run(store.prepare_concurrent_mode()).unwrap();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();

        let started = Instant::now();
        let error = run(store.publish_if_revision(0, namespace())).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "one CAS contention attempt should be shorter than the default 5s"
        );
        let timeout: i64 = store
            .0
            .lock()
            .unwrap()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000, "scoped CAS timeout must be restored");
        holder.execute_batch("ROLLBACK").unwrap();
        assert_eq!(run(store.publish_if_revision(0, namespace())).unwrap(), 1);

        drop(holder);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_publish_commit_busy_uses_scoped_short_timeout() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        run(store.prepare_concurrent_mode()).unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        assert_eq!(
            reader
                .query_row(
                    "SELECT revision FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .unwrap(),
            0
        );

        let started = Instant::now();
        let error = run(store.publish_if_revision(0, namespace())).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "one CAS contention attempt should be shorter than the default 5s"
        );
        assert!(store.0.lock().unwrap().is_autocommit());
        let timeout: i64 = store
            .0
            .lock()
            .unwrap()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000, "scoped CAS timeout must be restored");
        reader.execute_batch("ROLLBACK").unwrap();
        let unloaded = run(store.load()).unwrap();
        assert_eq!(unloaded.revision, 0);
        assert!(unloaded.namespace.is_none());
        assert_eq!(run(store.publish_if_revision(0, namespace())).unwrap(), 1);

        drop(reader);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn lease_fencing_revision_conflicts_and_expiry_fail_closed() {
        let store = SqliteMetadataStore::in_memory().unwrap();
        assert!(!store.durable());
        assert!(run(store.load()).unwrap().namespace.is_none());
        assert!(
            run(store.acquire_writer("", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Einval)
        );
        assert!(
            run(store.acquire_writer("a", Duration::ZERO))
                .unwrap_err()
                .is(ErrorCode::Einval)
        );
        let first = run(store.acquire_writer("a", Duration::from_secs(60))).unwrap();
        assert!(
            run(store.acquire_writer("a", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        assert_eq!(run(store.publish(0, &first, namespace())).unwrap(), 1);
        assert!(
            run(store.publish(0, &first, namespace()))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        let renewed = run(store.renew_writer(&first, Duration::from_secs(120))).unwrap();
        assert!(
            run(store.publish(1, &first, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert_eq!(run(store.publish(1, &renewed, namespace())).unwrap(), 2);
        // Deterministic provider-clock expiration: no timing-sensitive sleeps.
        store
            .0
            .lock()
            .unwrap()
            .execute("UPDATE mount_rs_metadata SET expires=0", [])
            .unwrap();
        assert!(
            run(store.renew_writer(&renewed, Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert!(
            run(store.publish(2, &renewed, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        let second = run(store.acquire_writer("b", Duration::from_secs(60))).unwrap();
        assert!(second.fence > renewed.fence);
        assert!(
            run(store.release_writer(&renewed))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert_eq!(run(store.publish(2, &second, namespace())).unwrap(), 3);
        run(store.release_writer(&second)).unwrap();
        assert!(
            run(store.publish(3, &second, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        let third = run(store.acquire_writer("a", Duration::from_secs(60))).unwrap();
        assert!(third.fence > second.fence);
        run(store.flush()).unwrap();
        assert_eq!(run(store.load()).unwrap().revision, 3);
    }

    #[test]
    fn acquire_writer_does_not_acknowledge_a_lease_before_rollback_journal_commit() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        {
            let connection = store.0.lock().unwrap();
            let journal: String = connection
                .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "delete");
            connection.busy_timeout(Duration::from_millis(40)).unwrap();
        }
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let initial: (Option<String>, i64, i64) = reader
            .query_row(
                "SELECT owner, fence, expires FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(initial, (None, 0, 0));

        let result = run(store.acquire_writer("ghost-owner", Duration::from_secs(30)));
        reader.execute_batch("ROLLBACK").unwrap();
        drop(reader);
        drop(store);
        let reopened = Connection::open(&path).unwrap();
        let durable: (Option<String>, i64, i64) = reopened
            .query_row(
                "SELECT owner, fence, expires FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        drop(reopened);
        std::fs::remove_file(path).unwrap();
        assert_eq!(durable, initial, "failed commit must leave lease unchanged");
        assert!(
            result.is_err(),
            "acquire acknowledged an uncommitted lease: {result:?}"
        );
    }

    #[test]
    fn renew_writer_does_not_acknowledge_an_expiry_before_rollback_journal_commit() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let lease = run(store.acquire_writer("original-owner", Duration::from_secs(30))).unwrap();
        {
            let connection = store.0.lock().unwrap();
            let journal: String = connection
                .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "delete");
            connection.busy_timeout(Duration::from_millis(40)).unwrap();
        }
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let initial_expiry: i64 = reader
            .query_row(
                "SELECT expires FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(initial_expiry as u64, lease.expires_at_ms);

        let result = run(store.renew_writer(&lease, Duration::from_secs(120)));
        reader.execute_batch("ROLLBACK").unwrap();
        drop(reader);
        drop(store);
        let reopened = Connection::open(&path).unwrap();
        let durable_expiry: i64 = reopened
            .query_row(
                "SELECT expires FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(reopened);
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            durable_expiry, initial_expiry,
            "failed commit must leave expiry unchanged"
        );
        assert!(
            result.is_err(),
            "renew acknowledged an uncommitted expiry: {result:?}"
        );
    }

    #[test]
    fn separate_connections_share_persisted_concurrent_mode_and_cas() {
        let path = super::super::tests::unique_database_path();
        let first = SqliteMetadataStore::open(&path).unwrap();
        let second = SqliteMetadataStore::open(&path).unwrap();

        run(first.prepare_concurrent_mode()).unwrap();
        run(second.prepare_concurrent_mode()).unwrap();
        assert!(
            run(second.acquire_writer("legacy", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Ebusy)
        );

        assert_eq!(run(first.publish_if_revision(0, namespace())).unwrap(), 1);
        assert!(
            run(second.publish_if_revision(0, namespace()))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        assert_eq!(run(second.load()).unwrap().revision, 1);
        assert_eq!(run(second.publish_if_revision(1, namespace())).unwrap(), 2);
        drop(first);
        drop(second);

        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(run(reopened.load()).unwrap().revision, 2);
        run(reopened.prepare_concurrent_mode()).unwrap();
        let connection = reopened.0.lock().unwrap();
        let (mode, fence): (Option<String>, i64) = connection
            .query_row(
                "SELECT write_mode, fence FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(mode.as_deref(), Some("MRC1"));
        assert_eq!(fence, i64::MAX);
        assert_eq!(
            connection
                .execute(
                    "UPDATE mount_rs_metadata SET owner='old', fence=fence+1
                     WHERE id=1 AND owner IS NULL AND fence<9223372036854775807",
                    [],
                )
                .unwrap(),
            0,
            "the old acquisition query must stay fenced"
        );
        drop(connection);
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_mode_rejects_non_durable_and_historically_fenced_volumes() {
        let memory = SqliteMetadataStore::in_memory().unwrap();
        assert!(
            run(memory.prepare_concurrent_mode())
                .unwrap_err()
                .is(ErrorCode::Enotsup)
        );

        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let lease = run(store.acquire_writer("old", Duration::from_secs(60))).unwrap();
        run(store.release_writer(&lease)).unwrap();
        assert!(
            run(store.prepare_concurrent_mode())
                .unwrap_err()
                .is(ErrorCode::Ebusy)
        );
        let (mode, fence): (Option<String>, i64) = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT write_mode, fence FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(mode.is_none());
        assert_eq!(fence, lease.fence as i64);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_publication_fails_closed_on_corrupt_fence_and_sqlite_io_error() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        run(store.prepare_concurrent_mode()).unwrap();

        store
            .0
            .lock()
            .unwrap()
            .execute("UPDATE mount_rs_metadata SET fence=0 WHERE id=1", [])
            .unwrap();
        assert!(
            run(store.publish_if_revision(0, namespace()))
                .unwrap_err()
                .is(ErrorCode::Ebusy)
        );
        assert_eq!(run(store.load()).unwrap().revision, 0);
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET fence=9223372036854775807 WHERE id=1",
                [],
            )
            .unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute_batch("PRAGMA query_only=ON")
            .unwrap();
        let error = run(store.publish_if_revision(0, namespace())).unwrap_err();
        assert!(!error.is(ErrorCode::Eagain));
        assert_eq!(run(store.load()).unwrap().revision, 0);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_version_mutations_reject_a_concurrent_mode_marker() {
        let store = SqliteMetadataStore::in_memory().unwrap();
        let lease = run(store.acquire_writer("old", Duration::from_secs(60))).unwrap();
        let first = run(store.publish_version(
            &lease,
            VersionPublication {
                expected_revision: 0,
                expected_parent: None,
                operation_id: PublicationId::new("first-old-version").unwrap(),
                namespace: namespace(),
                block_store_id: mount_rs_core::versioning::BlockStoreId::new("blocks").unwrap(),
                kind: VersionKind::Snapshot,
                restored_from: None,
                forked_from: None,
                durable: false,
            },
        ))
        .unwrap();
        let second = run(store.publish_version(
            &lease,
            VersionPublication {
                expected_revision: 1,
                expected_parent: Some(first.id.clone()),
                operation_id: PublicationId::new("second-old-version").unwrap(),
                namespace: namespace(),
                block_store_id: mount_rs_core::versioning::BlockStoreId::new("blocks").unwrap(),
                kind: VersionKind::Snapshot,
                restored_from: None,
                forked_from: None,
                durable: false,
            },
        ))
        .unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET write_mode='MRC1' WHERE id=1",
                [],
            )
            .unwrap();

        assert!(
            run(store.delete_version(&lease, &first.id))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert!(
            run(store.publish_version(
                &lease,
                VersionPublication {
                    expected_revision: 2,
                    expected_parent: Some(second.id),
                    operation_id: PublicationId::new("third-old-version").unwrap(),
                    namespace: namespace(),
                    block_store_id: mount_rs_core::versioning::BlockStoreId::new("blocks").unwrap(),
                    kind: VersionKind::Snapshot,
                    restored_from: None,
                    forked_from: None,
                    durable: false,
                },
            ))
            .unwrap_err()
            .is(ErrorCode::Estale)
        );
        assert_eq!(run(store.load()).unwrap().revision, 2);
    }

    #[test]
    fn legacy_acquire_and_mode_conversion_race_without_dual_authority() {
        use std::sync::{Arc, Barrier};

        for journal in ["DELETE", "WAL"] {
            for _ in 0..8 {
                let path = super::super::tests::unique_database_path();
                let old = SqliteMetadataStore::open(&path).unwrap();
                old.0
                    .lock()
                    .unwrap()
                    .execute_batch(&format!("PRAGMA journal_mode={journal};"))
                    .unwrap();
                let concurrent = SqliteMetadataStore::open(&path).unwrap();
                let start = Arc::new(Barrier::new(3));
                let old_start = Arc::clone(&start);
                let concurrent_start = Arc::clone(&start);
                let old_task = std::thread::spawn(move || {
                    old_start.wait();
                    run(old.acquire_writer("old", Duration::from_secs(60)))
                });
                let concurrent_task = std::thread::spawn(move || {
                    concurrent_start.wait();
                    run(concurrent.prepare_concurrent_mode())
                });
                start.wait();
                let old_result = old_task.join().unwrap();
                let concurrent_result = concurrent_task.join().unwrap();
                let reopened = SqliteMetadataStore::open(&path).unwrap();
                match (old_result, concurrent_result) {
                    (Ok(lease), Err(error)) if error.is(ErrorCode::Ebusy) => {
                        assert_eq!(
                            run(reopened.acquire_writer("new", Duration::from_secs(60)))
                                .unwrap_err()
                                .code,
                            ErrorCode::Eagain
                        );
                        run(reopened.release_writer(&lease)).unwrap();
                        assert!(
                            run(reopened.prepare_concurrent_mode())
                                .unwrap_err()
                                .is(ErrorCode::Ebusy)
                        );
                    }
                    (Err(error), Ok(())) if error.is(ErrorCode::Ebusy) => {
                        assert_eq!(
                            run(reopened.publish_if_revision(0, namespace())).unwrap(),
                            1
                        );
                    }
                    (old_result, concurrent_result) => panic!(
                        "journal {journal}: both modes must not win or both fail: {old_result:?}, {concurrent_result:?}"
                    ),
                }
                drop(reopened);
                std::fs::remove_file(&path).unwrap();
                for sidecar in ["-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{sidecar}", path.display()));
                }
            }
        }
    }

    #[test]
    fn ignored_mode_update_must_not_report_concurrent_authority() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER suppress_concurrent_mode
                 BEFORE UPDATE OF write_mode ON mount_rs_metadata
                 BEGIN SELECT RAISE(IGNORE); END;",
            )
            .unwrap();
        let error = run(store.prepare_concurrent_mode()).unwrap_err();
        assert!(!error.is(ErrorCode::Eagain));
        let (mode, fence): (Option<String>, i64) = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT write_mode, fence FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(mode.is_none());
        assert_eq!(fence, 0);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_sqlite_blocks_require_shared_file_backing() {
        let memory = SqliteBlockStore::in_memory().unwrap();
        let error = run(memory.prepare_concurrent_mode()).unwrap_err();
        assert!(error.is(ErrorCode::Enotsup));
        assert!(
            error
                .to_string()
                .contains("file-backed SQLite block database")
        );

        let path = super::super::tests::unique_database_path();
        let file = SqliteBlockStore::open(&path).unwrap();
        run(file.prepare_concurrent_mode()).unwrap();
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn block_put_does_not_acknowledge_a_returning_row_before_rollback_journal_commit() {
        let path = super::super::tests::unique_database_path();
        let blocks = SqliteBlockStore::open(&path).unwrap();
        {
            let connection = blocks.0.lock().unwrap();
            connection
                .execute_batch("PRAGMA journal_mode=DELETE;")
                .unwrap();
            connection.busy_timeout(Duration::from_millis(40)).unwrap();
            let journal: String = connection
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "delete");
        }

        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN;").unwrap();
        let count: i64 = reader
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "reader must hold a SHARED snapshot before put");

        let result = futures_lite::future::block_on(
            blocks.put(b"commit must precede block acknowledgement"),
        );
        let reader_count: i64 = reader
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        reader.execute_batch("ROLLBACK;").unwrap();
        drop(reader);
        drop(blocks);

        let reopened = SqliteBlockStore::open(&path).unwrap();
        let committed_count: i64 = reopened
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        drop(reopened);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(reader_count, 0, "locked reader cannot see the insert");
        assert_eq!(
            committed_count, 0,
            "failed commit must not publish the insert"
        );
        assert!(
            result.is_err(),
            "put acknowledged an uncommitted RETURNING row: {result:?}"
        );
    }

    #[test]
    fn block_put_retries_busy_begin_after_writer_releases() {
        let path = super::super::tests::unique_database_path();
        let blocks = SqliteBlockStore::open(&path).unwrap();
        blocks
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            holder.execute_batch("ROLLBACK").unwrap();
        });

        let id = futures_lite::future::block_on(blocks.put(b"begin-busy-retry"))
            .expect("transient writer lock must permit one acknowledged block");
        release.join().unwrap();
        let reopened = SqliteBlockStore::open(&path).unwrap();
        assert_eq!(run(reopened.get(&id)).unwrap(), b"begin-busy-retry");
        drop(reopened);
        drop(blocks);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn block_put_retries_busy_commit_after_reader_releases() {
        let path = super::super::tests::unique_database_path();
        let blocks = SqliteBlockStore::open(&path).unwrap();
        blocks
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        assert_eq!(
            reader
                .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            reader.execute_batch("ROLLBACK").unwrap();
        });

        let id = futures_lite::future::block_on(blocks.put(b"commit-busy-retry"))
            .expect("transient reader lock must permit one acknowledged block");
        release.join().unwrap();
        let reopened = SqliteBlockStore::open(&path).unwrap();
        assert_eq!(run(reopened.get(&id)).unwrap(), b"commit-busy-retry");
        let durable_count: i64 = reopened
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            durable_count, 1,
            "a rolled-back attempt must not leave a duplicate block"
        );
        drop(reopened);
        drop(blocks);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn block_put_bounds_persistent_writer_lock_without_acknowledgement() {
        let path = super::super::tests::unique_database_path();
        let blocks = SqliteBlockStore::open(&path).unwrap();
        blocks
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();

        let started = Instant::now();
        let error = futures_lite::future::block_on(blocks.put(b"never-acknowledged")).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "short SQLite busy timeout must bound retries"
        );
        assert!(blocks.0.lock().unwrap().is_autocommit());
        holder.execute_batch("ROLLBACK").unwrap();
        let reopened = SqliteBlockStore::open(&path).unwrap();
        let count: i64 = reopened
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        drop(reopened);
        drop(holder);
        drop(blocks);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn block_ids_are_immutable_and_separate_from_metadata() {
        let blocks = SqliteBlockStore::in_memory().unwrap();
        assert!(!blocks.durable());
        let a = run(blocks.put(&[0, 255, 1, 2])).unwrap();
        let b = run(blocks.put(&[9])).unwrap();
        assert_ne!(a, b);
        assert_eq!(run(blocks.get(&a)).unwrap(), [0, 255, 1, 2]);
        assert_eq!(run(blocks.get(&b)).unwrap(), [9]);
        run(blocks.flush()).unwrap();
        run(blocks.delete(&a)).unwrap();
        assert!(run(blocks.get(&a)).unwrap_err().is(ErrorCode::Enoent));
        assert_eq!(run(blocks.get(&b)).unwrap(), [9]);
        let other = SqliteBlockStore::in_memory().unwrap();
        assert!(run(other.get(&b)).unwrap_err().is(ErrorCode::Enoent));
        let tables: u64 = blocks
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='mount_rs_metadata'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn version_schema_migrates_legacy_six_column_metadata_and_reopens_idempotently() {
        let path = super::super::tests::unique_database_path();
        let legacy_namespace = serde_json::to_string(&namespace()).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mount_rs_metadata (
                     id INTEGER PRIMARY KEY CHECK(id=1),
                     revision INTEGER NOT NULL CHECK(revision>=0),
                     namespace TEXT,
                     owner TEXT,
                     fence INTEGER NOT NULL CHECK(fence>=0),
                     expires INTEGER NOT NULL
                 );
                 INSERT INTO mount_rs_metadata
                 (id, revision, namespace, owner, fence, expires)
                 VALUES (1, 7, NULL, NULL, 0, 0);",
            )
            .unwrap();
        connection
            .execute(
                "UPDATE mount_rs_metadata SET namespace=?1 WHERE id=1",
                params![legacy_namespace],
            )
            .unwrap();
        drop(connection);

        let store = SqliteMetadataStore::open(&path).unwrap();
        let volume = store.volume_id();
        let loaded = run(store.load()).unwrap();
        assert_eq!(loaded.revision, 7);
        assert!(loaded.namespace.is_some());
        drop(store);

        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(reopened.volume_id(), volume);
        let connection = reopened.0.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT schema_version FROM mount_rs_schema_versions
                     WHERE schema_name=?1",
                    params![VERSION_SCHEMA_NAME],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            VERSION_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM mount_rs_version_state WHERE id=1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        drop(connection);
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn incompatible_version_schema_is_rejected_without_partial_migration() {
        let path = super::super::tests::unique_database_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mount_rs_metadata (
                     id INTEGER PRIMARY KEY CHECK(id=1),
                     revision INTEGER NOT NULL CHECK(revision>=0),
                     namespace TEXT,
                     owner TEXT,
                     fence INTEGER NOT NULL CHECK(fence>=0),
                     expires INTEGER NOT NULL
                 );
                 INSERT INTO mount_rs_metadata
                 (id, revision, namespace, owner, fence, expires)
                 VALUES (1, 0, NULL, NULL, 0, 0);
                 CREATE TABLE mount_rs_versions (id TEXT PRIMARY KEY NOT NULL);",
            )
            .unwrap();
        drop(connection);

        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("incompatible version schema must be rejected"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));

        let connection = Connection::open(&path).unwrap();
        let metadata_has_volume: bool = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM pragma_table_info('mount_rs_metadata')
                     WHERE name='volume_id'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!metadata_has_volume);
        let version_table_columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('mount_rs_versions')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version_table_columns, 1);
        let schema_version_table_exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master
                     WHERE type='table' AND name='mount_rs_schema_versions'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!schema_version_table_exists);
        drop(connection);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn version_schema_rejects_missing_operation_id_uniqueness_without_partial_migration() {
        let path = super::super::tests::unique_database_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mount_rs_metadata (
                     id INTEGER PRIMARY KEY CHECK(id=1),
                     revision INTEGER NOT NULL CHECK(revision>=0),
                     namespace TEXT,
                     owner TEXT,
                     fence INTEGER NOT NULL CHECK(fence>=0),
                     expires INTEGER NOT NULL
                 );
                 INSERT INTO mount_rs_metadata
                 (id, revision, namespace, owner, fence, expires)
                 VALUES (1, 0, NULL, NULL, 0, 0);
                 CREATE TABLE mount_rs_versions (
                     id TEXT PRIMARY KEY NOT NULL,
                     volume_id TEXT NOT NULL,
                     sequence INTEGER NOT NULL,
                     parent_id TEXT,
                     restored_from TEXT,
                     forked_from TEXT,
                     namespace TEXT NOT NULL,
                     block_store_id TEXT NOT NULL,
                     kind TEXT NOT NULL,
                     created_at_ms INTEGER NOT NULL,
                     durable INTEGER NOT NULL,
                     operation_id TEXT NOT NULL
                 );",
            )
            .unwrap();
        drop(connection);

        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("missing operation uniqueness must be rejected"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));

        let connection = Connection::open(&path).unwrap();
        let metadata_has_volume: bool = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM pragma_table_info('mount_rs_metadata')
                     WHERE name='volume_id'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!metadata_has_volume);
        for table in [
            SCHEMA_VERSION_TABLE,
            "mount_rs_version_state",
            "mount_rs_version_pins",
        ] {
            let exists: bool = connection
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
                     )",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!exists, "migration left table {table}");
        }
        drop(connection);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn future_version_schema_is_rejected_and_preserved() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_schema_versions SET schema_version=99
                 WHERE schema_name=?1",
                params![VERSION_SCHEMA_NAME],
            )
            .unwrap();
        drop(store);

        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("future version schema must be rejected"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT schema_version FROM mount_rs_schema_versions
                     WHERE schema_name=?1",
                    params![VERSION_SCHEMA_NAME],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            99
        );
        drop(connection);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn version_head_and_read_lease_validate_persisted_identities() {
        let metadata = SqliteMetadataStore::in_memory().unwrap();
        let writer =
            run(metadata.acquire_writer("version-writer", Duration::from_secs(60))).unwrap();
        let publication = VersionPublication {
            expected_revision: 0,
            expected_parent: None,
            operation_id: PublicationId::new("sqlite-integrity").unwrap(),
            namespace: namespace(),
            block_store_id: mount_rs_core::versioning::BlockStoreId::new("sqlite-blocks").unwrap(),
            kind: VersionKind::Snapshot,
            restored_from: None,
            forked_from: None,
            durable: false,
        };
        let version = run(metadata.publish_version(&writer, publication)).unwrap();
        let pin = run(metadata.open_view_pin(
            &version.id,
            ReadLeaseRequest {
                owner: "reader".to_owned(),
                ttl: Duration::from_secs(60),
            },
        ))
        .unwrap();
        let forged = ReadLease {
            owner: "forged".to_owned(),
            ..pin.clone()
        };
        assert!(
            run(metadata.renew_view_pin(
                &forged,
                ReadLeaseRequest {
                    owner: pin.owner.clone(),
                    ttl: Duration::from_secs(60),
                },
            ))
            .unwrap_err()
            .is(ErrorCode::Estale)
        );

        metadata
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_version_state SET head_id=?1 WHERE id=1",
                params![format!("{}:999", metadata.volume_id())],
            )
            .unwrap();
        assert!(run(metadata.version_head()).unwrap_err().is(ErrorCode::Eio));
    }

    #[test]
    fn opening_version_schema_rejects_a_dangling_head_atomically() {
        let path = super::super::tests::unique_database_path();
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let lease = run(metadata.acquire_writer("head-writer", Duration::from_secs(60))).unwrap();
        let publication = VersionPublication {
            expected_revision: 0,
            expected_parent: None,
            operation_id: PublicationId::new("dangling-head").unwrap(),
            namespace: namespace(),
            block_store_id: mount_rs_core::versioning::BlockStoreId::new("head-blocks").unwrap(),
            kind: VersionKind::Snapshot,
            restored_from: None,
            forked_from: None,
            durable: true,
        };
        run(metadata.publish_version(&lease, publication)).unwrap();
        drop(metadata);

        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE mount_rs_version_state SET head_id=?1 WHERE id=1",
                params!["missing-volume:999"],
            )
            .unwrap();
        drop(connection);

        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("dangling version head must be rejected during open"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn separate_files_reopen_and_independent_connections_enforce_one_writer() {
        let path = super::super::tests::unique_database_path();
        let block_path = path.with_extension("blocks.db");
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let competitor = SqliteMetadataStore::open(&path).unwrap();
        let blocks = SqliteBlockStore::open(&block_path).unwrap();
        assert!(metadata.durable());
        assert!(blocks.durable());
        let lease = run(metadata.acquire_writer("first", Duration::from_secs(60))).unwrap();
        assert!(
            run(competitor.acquire_writer("second", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        let id = run(blocks.put(&[1, 2, 3, 0, 255])).unwrap();
        run(blocks.flush()).unwrap();
        assert_eq!(run(metadata.publish(0, &lease, namespace())).unwrap(), 1);
        run(metadata.flush()).unwrap();
        run(metadata.release_writer(&lease)).unwrap();
        let new_lease = run(competitor.acquire_writer("second", Duration::from_secs(60))).unwrap();
        assert!(new_lease.fence > lease.fence);
        assert!(
            run(metadata.publish(1, &lease, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        drop(metadata);
        drop(competitor);
        drop(blocks);
        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(run(reopened.load()).unwrap().revision, 1);
        let reopened_blocks = SqliteBlockStore::open(&block_path).unwrap();
        assert_eq!(run(reopened_blocks.get(&id)).unwrap(), [1, 2, 3, 0, 255]);
        assert_eq!(
            reopened
                .0
                .lock()
                .unwrap()
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        drop(reopened);
        drop(reopened_blocks);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(block_path).unwrap();
    }
}
