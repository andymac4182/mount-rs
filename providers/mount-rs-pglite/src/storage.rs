//! Independent PGlite metadata and immutable block providers.
//!
//! These providers use separate tables and separate PostgreSQL-wire
//! connections, so metadata and blocks may be selected independently by the
//! filesystem composition layer. Durability is an explicit caller assertion:
//! the default is volatile because a PGlite server may be in-memory, and a
//! successful SQL acknowledgement is not proof of host-disk persistence.
//! This provider never makes a host-safe-mount claim about the server's data
//! directory.

use async_trait::async_trait;
use md5::{Digest, Md5};
use mount_rs_core::diagnostics::profile::{self, Event};
use mount_rs_core::storage::InodeId;
use mount_rs_core::storage::{
    BlockId, BlockStore, CheckoutRequest, ConcurrentBackingId, ConcurrentModeState,
    DelegatedCheckin, DelegatedPublish, DelegatedRecovery, DelegationState, DirectoryGrant,
    GrantToken, InodeMetadataSnapshot, InodeModeState, InodeVersion, LoadedInode, LoadedMetadata,
    MetadataStore, Namespace, NodeMetadata, WriterLease, decode_inode_namespace,
    encode_inode_namespace, validate_inode_publication, validate_node_kind,
};
use mount_rs_core::versioning::{
    PublicationId, ReadLease, ReadLeaseRequest, VersionHead, VersionId, VersionInfo, VersionKind,
    VersionPublication, VersionedMetadataStore, VolumeId,
};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard};
use tokio::task::JoinHandle;
use tokio_postgres::types::Type;
use tokio_postgres::{Client, NoTls};

use super::{CloseGate, postgres_error};

// The clock is evaluated by PostgreSQL/PGlite, never supplied by the client.
// clock_timestamp() deliberately uses the current provider clock even inside
// a transaction, which matters when a transaction waits for a row lock.
const NOW: &str = "CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)";
const CONCURRENT_WRITE_MODE: &str = "MRC1";
const BOUND_CONCURRENT_WRITE_MODE: &str = "MRC2";
// A pre-conversion client still checks fence<INT8_MAX before acquisition.
// Exhausting the fence in the same atomic update as the mode marker therefore
// blocks old binaries without relying on their awareness of write_mode.
const CONCURRENT_FENCE_SENTINEL: i64 = i64::MAX;

const METADATA_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_metadata (
 volume_key TEXT PRIMARY KEY NOT NULL,
 revision BIGINT NOT NULL CHECK(revision>=0),
 namespace TEXT,
 owner TEXT,
 fence BIGINT NOT NULL CHECK(fence>=0),
 expires BIGINT NOT NULL,
 volume_id TEXT,
 write_mode TEXT,
 backing_id TEXT, delegation TEXT);
ALTER TABLE mount_rs_metadata ADD COLUMN IF NOT EXISTS volume_id TEXT;
ALTER TABLE mount_rs_metadata ADD COLUMN IF NOT EXISTS write_mode TEXT;
ALTER TABLE mount_rs_metadata ADD COLUMN IF NOT EXISTS backing_id TEXT;
ALTER TABLE mount_rs_metadata ADD COLUMN IF NOT EXISTS delegation TEXT;
CREATE TABLE IF NOT EXISTS mount_rs_inode_guards (
 volume_key TEXT NOT NULL, inode BIGINT NOT NULL CHECK(inode>0),
 generation BIGINT NOT NULL CHECK(generation>0),
 revision BIGINT NOT NULL CHECK(revision>=0), node TEXT NOT NULL,
 PRIMARY KEY(volume_key,inode));
";

const BLOCK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_blocks (
 volume_key TEXT NOT NULL, id TEXT NOT NULL, bytes BYTEA NOT NULL,
 PRIMARY KEY (volume_key, id));
CREATE TABLE IF NOT EXISTS mount_rs_block_authority (
 volume_key TEXT PRIMARY KEY NOT NULL,
 backing_id TEXT NOT NULL);";

const NOW_SELECT: &str = "SELECT CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)";
const VERSION_SCHEMA_NAME: &str = "mount-rs-versioning";
// Version 2 marks stores that can contain the namespace-publication kind.
// Keep version 1 until the first such record commits so older readers can reopen.
const VERSION_SCHEMA_BASE_VERSION: i64 = 1;
const VERSION_SCHEMA_VERSION: i64 = 2;
const VERSION_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mount_rs_schema_versions (
 schema_name TEXT PRIMARY KEY NOT NULL,
 schema_version BIGINT NOT NULL CHECK(schema_version>=0));
CREATE TABLE IF NOT EXISTS mount_rs_version_state (
 volume_key TEXT PRIMARY KEY NOT NULL,
 volume_id TEXT NOT NULL,
 head_id TEXT,
 next_sequence BIGINT NOT NULL CHECK(next_sequence>0),
 next_read_fence BIGINT NOT NULL CHECK(next_read_fence>=0));
CREATE TABLE IF NOT EXISTS mount_rs_versions (
 volume_key TEXT NOT NULL,
 id TEXT NOT NULL,
 volume_id TEXT NOT NULL,
 sequence BIGINT NOT NULL CHECK(sequence>0),
 parent_id TEXT,
 restored_from TEXT,
 forked_from TEXT,
 namespace TEXT NOT NULL,
 block_store_id TEXT NOT NULL,
 kind TEXT NOT NULL,
 created_at_ms BIGINT NOT NULL,
 durable BOOLEAN NOT NULL,
 operation_id TEXT NOT NULL,
 PRIMARY KEY (volume_key, id),
 UNIQUE (volume_key, operation_id));
CREATE TABLE IF NOT EXISTS mount_rs_version_pins (
 volume_key TEXT NOT NULL,
 view_id TEXT NOT NULL,
 volume_id TEXT NOT NULL,
 version_id TEXT NOT NULL,
 owner TEXT NOT NULL,
 fence BIGINT NOT NULL CHECK(fence>0),
 expires BIGINT NOT NULL CHECK(expires>=0),
 PRIMARY KEY (volume_key, view_id));
";

/// Connection-scoped selection for one independent metadata/block volume.
///
/// `durable` is deliberately not inferred from the URL or from successful
/// network acknowledgements. Set it only when the caller has configured the
/// PGlite server with a persistence policy suitable for its application.
#[derive(Debug, Clone)]
pub struct PgliteStorageOptions {
    pub volume_key: String,
    pub durable: bool,
}

impl PgliteStorageOptions {
    pub fn new(volume_key: impl Into<String>) -> Self {
        Self {
            volume_key: volume_key.into(),
            durable: false,
        }
    }

    pub fn with_durable(mut self, durable: bool) -> Self {
        self.durable = durable;
        self
    }
}

impl Default for PgliteStorageOptions {
    fn default() -> Self {
        Self::new("mount-rs")
    }
}

#[derive(Clone)]
struct Database {
    client: Arc<Mutex<Option<Client>>>,
    connection: Arc<Mutex<Option<JoinHandle<()>>>>,
    close_gate: CloseGate,
    volume_key: String,
    durable: bool,
}

impl Database {
    async fn connect(
        connection_string: &str,
        schema: &str,
        options: PgliteStorageOptions,
    ) -> Result<Self> {
        if options.volume_key.is_empty() {
            return Err(
                FsError::new(ErrorCode::Einval).with_message("PGlite volume key must not be empty")
            );
        }
        let (client, connection) = tokio_postgres::connect(connection_string, NoTls)
            .await
            .map_err(postgres_error)?;
        let connection = tokio::spawn(async move {
            let _ = connection.await;
        });
        let database = Self {
            client: Arc::new(Mutex::new(Some(client))),
            connection: Arc::new(Mutex::new(Some(connection))),
            close_gate: CloseGate::new(),
            volume_key: options.volume_key,
            durable: options.durable,
        };
        database
            .client
            .lock()
            .await
            .as_ref()
            .ok_or_else(connection_closed)?
            .batch_execute(schema)
            .await
            .map_err(postgres_error)?;
        Ok(database)
    }

    async fn lock_client(&self) -> Result<MutexGuard<'_, Option<Client>>> {
        if !self.close_gate.is_open() {
            return Err(connection_closed());
        }
        let client = self.client.lock().await;
        if client.is_none() || !self.close_gate.is_open() {
            return Err(connection_closed());
        }
        Ok(client)
    }

    /// Close the client and wait for the PostgreSQL-wire task to observe the
    /// dropped sender. Waiting here is important for PGlite's bounded server:
    /// a subsequent filesystem may connect immediately after shutdown without
    /// racing the old socket's teardown.
    async fn close(&self) -> Result<()> {
        if self.close_gate.start() {
            let database = self.clone();
            // Keep teardown owned by a task independent of this caller. If a
            // close future is canceled, later callers still await this same
            // teardown rather than observing partially detached state.
            tokio::spawn(async move {
                database.finish_close().await;
            });
        }
        self.close_gate.wait().await;
        Ok(())
    }

    async fn finish_close(&self) {
        let client = self.client.lock().await.take();
        drop(client);
        let connection = self.connection.lock().await.take();
        if let Some(connection) = connection {
            let _ = connection.await;
        }
        self.close_gate.complete();
    }

    async fn ensure_metadata_row(&self) -> Result<()> {
        let client = self.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "INSERT INTO mount_rs_metadata
                    (volume_key, revision, namespace, owner, fence, expires, volume_id)
                 VALUES ($1, 0, NULL, NULL, 0, 0, 'pglite-' || md5($1))
                 ON CONFLICT (volume_key) DO NOTHING",
                &[(&self.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "UPDATE mount_rs_metadata
                 SET volume_id = 'pglite-' || md5(volume_key)
                 WHERE volume_key=$1 AND (volume_id IS NULL OR volume_id='')",
                &[(&self.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        Ok(())
    }

    async fn flush(&self) -> Result<()> {
        // Successful execute/commit calls already received the server's
        // acknowledgement. This round trip is the provider barrier and also
        // propagates a dead connection to fsync callers.
        let client = self.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .batch_execute("SELECT 1")
            .await
            .map_err(postgres_error)
    }
}

/// Fenced single-writer metadata persisted in an external PGlite database.
/// Namespace JSON contains attributes and block references only; file bytes
/// are stored by [`PgliteBlockStore`].
#[derive(Clone)]
pub struct PgliteMetadataStore(Database, VolumeId);

fn decode_delegation_row(row: &tokio_postgres::Row) -> Result<(DelegationState, Namespace)> {
    if row.get::<_, Option<String>>(0).as_deref() != Some("MRC3")
        || row.get::<_, Option<String>>(4).is_some()
        || row.get::<_, i64>(5) != CONCURRENT_FENCE_SENTINEL
        || row.get::<_, i64>(6) != 0
    {
        return Err(stale());
    }
    let state: DelegationState =
        serde_json::from_str(&row.get::<_, Option<String>>(2).ok_or_else(stale)?)
            .map_err(backend_error)?;
    if row.get::<_, Option<String>>(1).as_deref() != Some(state.backing.to_hex().as_str()) {
        return Err(stale());
    }
    let ns: Namespace = serde_json::from_str(&row.get::<_, Option<String>>(3).ok_or_else(stale)?)
        .map_err(backend_error)?;
    state.validate(&ns)?;
    Ok((state, ns))
}

impl PgliteMetadataStore {
    async fn delegation_transaction<T>(
        &self,
        backing: ConcurrentBackingId,
        expected: Option<u64>,
        force_revision: bool,
        retired_receipt: Option<&GrantToken>,
        operation: impl FnOnce(&mut DelegationState, &mut Namespace) -> Result<T> + Send,
    ) -> Result<T> {
        let mut guard = self.0.lock_client().await?;
        let tx = guard
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        let row = tx.query_one("SELECT write_mode, backing_id, delegation, namespace, owner, fence, expires, revision FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE", &[&self.0.volume_key]).await.map_err(postgres_error)?;
        let (mut state, mut ns) = decode_delegation_row(&row)?;
        if state.backing != backing {
            return Err(stale());
        }
        let revision = nonnegative(row.get(7), "revision")?;
        if expected.is_some_and(|value| value != revision)
            && !retired_receipt.is_some_and(|token| state.retired.contains(token))
        {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let before = serde_json::to_string(&ns).map_err(backend_error)?;
        let result = operation(&mut state, &mut ns)?;
        state.validate(&ns)?;
        let after = serde_json::to_string(&ns).map_err(backend_error)?;
        let next = if force_revision || before != after {
            revision.checked_add(1).ok_or_else(stale)?
        } else {
            revision
        };
        let next = i64::try_from(next).map_err(|_| stale())?;
        let state = serde_json::to_string(&state).map_err(backend_error)?;
        let changed = tx.execute("UPDATE mount_rs_metadata SET revision=$2, namespace=$3, delegation=$4 WHERE volume_key=$1", &[&self.0.volume_key, &next, &after, &state]).await.map_err(postgres_error)?;
        if changed != 1 {
            return Err(stale());
        }
        tx.commit().await.map_err(postgres_error)?;
        Ok(result)
    }

    /// Connect to a PGlite PostgreSQL-wire endpoint and initialize the
    /// provider's metadata table. The endpoint is expected to provide the
    /// desired external durability policy.
    pub async fn connect(connection_string: &str) -> Result<Self> {
        Self::connect_with_options(connection_string, PgliteStorageOptions::default()).await
    }

    pub async fn connect_with_key(
        connection_string: &str,
        volume_key: impl Into<String>,
    ) -> Result<Self> {
        Self::connect_with_options(connection_string, PgliteStorageOptions::new(volume_key)).await
    }

    pub async fn connect_with_options(
        connection_string: &str,
        options: PgliteStorageOptions,
    ) -> Result<Self> {
        let database = Database::connect(connection_string, METADATA_SCHEMA, options).await?;
        database.ensure_metadata_row().await?;
        initialize_version_schema(&database).await?;
        let volume_id = load_volume_id(&database).await?;
        Ok(Self(database, volume_id))
    }

    /// Close the underlying PostgreSQL-wire client. This is idempotent and is
    /// intended for owners that retain the store behind an `Arc`, such as the
    /// N-API chunked filesystem shutdown callback.
    pub async fn close(&self) -> Result<()> {
        self.0.close().await
    }
}

/// Immutable blocks persisted in a table independent from metadata.
#[derive(Clone)]
pub struct PgliteBlockStore(Database);

impl PgliteBlockStore {
    /// Connect to a PGlite PostgreSQL-wire endpoint and initialize the
    /// provider's block table.
    pub async fn connect(connection_string: &str) -> Result<Self> {
        Self::connect_with_options(connection_string, PgliteStorageOptions::default()).await
    }

    pub async fn connect_with_key(
        connection_string: &str,
        volume_key: impl Into<String>,
    ) -> Result<Self> {
        Self::connect_with_options(connection_string, PgliteStorageOptions::new(volume_key)).await
    }

    pub async fn connect_with_options(
        connection_string: &str,
        options: PgliteStorageOptions,
    ) -> Result<Self> {
        Ok(Self(
            Database::connect(connection_string, BLOCK_SCHEMA, options).await?,
        ))
    }

    /// Close the underlying PostgreSQL-wire client. This is idempotent.
    pub async fn close(&self) -> Result<()> {
        self.0.close().await
    }
}

fn connection_closed() -> FsError {
    FsError::new(ErrorCode::Ebadf).with_syscall("PGlite connection")
}

fn block_id(bytes: &[u8]) -> String {
    // Preserve the existing PostgreSQL identity contract exactly: the old
    // query was md5(encode($1, 'hex')), which hashes the lowercase ASCII hex
    // representation rather than the raw bytes. Computing that same value
    // locally removes one provider round trip from every unique block put.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut hasher = Md5::new();
    for &byte in bytes {
        hasher.update([HEX[(byte >> 4) as usize], HEX[(byte & 0x0f) as usize]]);
    }
    let digest = hasher.finalize();
    let mut id = String::with_capacity(digest.len() * 2);
    for byte in digest {
        id.push(HEX[(byte >> 4) as usize] as char);
        id.push(HEX[(byte & 0x0f) as usize] as char);
    }
    id
}

fn incompatible_schema(message: impl Into<String>) -> FsError {
    FsError::new(ErrorCode::Enotsup)
        .with_syscall("PGlite versioning schema")
        .with_message(message.into())
}

async fn load_volume_id(database: &Database) -> Result<VolumeId> {
    let client = database.lock_client().await?;
    let row = client
        .as_ref()
        .ok_or_else(connection_closed)?
        .query_typed_opt(
            "SELECT volume_id FROM mount_rs_metadata WHERE volume_key=$1",
            &[(&database.volume_key, Type::TEXT)],
        )
        .await
        .map_err(postgres_error)?
        .ok_or_else(|| incompatible_schema("PGlite metadata row is missing"))?;
    let value = row
        .get::<_, Option<String>>(0)
        .ok_or_else(|| incompatible_schema("PGlite metadata volume_id is missing"))?;
    VolumeId::new(value).map_err(|_| incompatible_schema("PGlite volume_id is invalid"))
}

async fn initialize_version_schema(database: &Database) -> Result<()> {
    let mut client = database.lock_client().await?;
    let tx = client
        .as_mut()
        .ok_or_else(connection_closed)?
        .transaction()
        .await
        .map_err(postgres_error)?;
    tx.batch_execute(VERSION_SCHEMA)
        .await
        .map_err(postgres_error)?;

    let stored_version = tx
        .query_typed_opt(
            "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=$1",
            &[(&VERSION_SCHEMA_NAME, Type::TEXT)],
        )
        .await
        .map_err(postgres_error)?
        .map(|row| row.get::<_, i64>(0));
    if stored_version.is_some_and(|version| version > VERSION_SCHEMA_VERSION) {
        return Err(incompatible_schema(
            "PGlite versioning schema is newer than this mount-rs build",
        ));
    }

    let volume_id = load_volume_id_from_transaction(&tx, database).await?;
    let state = tx
        .query_typed_opt(
            "SELECT volume_id, next_sequence, next_read_fence
             FROM mount_rs_version_state WHERE volume_key=$1 FOR UPDATE",
            &[(&database.volume_key, Type::TEXT)],
        )
        .await
        .map_err(postgres_error)?;
    match state {
        Some(row) => {
            let state_volume = row.get::<_, String>(0);
            let next_sequence = row.get::<_, i64>(1);
            let next_read_fence = row.get::<_, i64>(2);
            if state_volume != volume_id.0 || next_sequence <= 0 || next_read_fence < 0 {
                return Err(incompatible_schema(
                    "PGlite version state is inconsistent with its metadata volume",
                ));
            }
        }
        None => {
            tx.execute_typed(
                "INSERT INTO mount_rs_version_state
                 (volume_key, volume_id, head_id, next_sequence, next_read_fence)
                 VALUES ($1, $2, NULL, 1, 0)",
                &[
                    (&database.volume_key, Type::TEXT),
                    (&volume_id.0, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?;
        }
    }

    let invalid_versions = tx
        .query_typed_opt(
            "SELECT EXISTS(
                 SELECT 1 FROM mount_rs_versions
                 WHERE volume_key=$1 AND (volume_id IS NULL OR volume_id<>$2)
             )",
            &[
                (&database.volume_key, Type::TEXT),
                (&volume_id.0, Type::TEXT),
            ],
        )
        .await
        .map_err(postgres_error)?
        .ok_or_else(|| incompatible_schema("PGlite schema query returned no row"))?
        .get::<_, bool>(0);
    let invalid_pins = tx
        .query_typed_opt(
            "SELECT EXISTS(
                 SELECT 1 FROM mount_rs_version_pins
                 WHERE volume_key=$1 AND (volume_id IS NULL OR volume_id<>$2)
             )",
            &[
                (&database.volume_key, Type::TEXT),
                (&volume_id.0, Type::TEXT),
            ],
        )
        .await
        .map_err(postgres_error)?
        .ok_or_else(|| incompatible_schema("PGlite schema query returned no row"))?
        .get::<_, bool>(0);
    if invalid_versions || invalid_pins {
        return Err(incompatible_schema(
            "PGlite version records or pins cross provider volumes",
        ));
    }

    let head_id = tx
        .query_typed_opt(
            "SELECT head_id FROM mount_rs_version_state WHERE volume_key=$1",
            &[(&database.volume_key, Type::TEXT)],
        )
        .await
        .map_err(postgres_error)?
        .and_then(|row| row.get::<_, Option<String>>(0));
    if let Some(head_id) = head_id {
        let head = VersionId::decode(&head_id)
            .map_err(|_| incompatible_schema("PGlite version head is malformed"))?;
        if head.volume != volume_id {
            return Err(incompatible_schema(
                "PGlite version head belongs to another provider volume",
            ));
        }
        let row = tx
            .query_typed_opt(
                &format!("{VERSION_SELECT} WHERE volume_key=$1 AND id=$2 AND volume_id=$3"),
                &[
                    (&database.volume_key, Type::TEXT),
                    (&head_id, Type::TEXT),
                    (&volume_id.0, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| {
                incompatible_schema("PGlite version head references a missing version")
            })?;
        let record = decode_version_row(&row)
            .and_then(|record| {
                record.validate_for_volume(&volume_id)?;
                Ok(record)
            })
            .map_err(|_| incompatible_schema("PGlite version head record is invalid"))?;
        if record.id != head {
            return Err(incompatible_schema(
                "PGlite version head differs from its record identity",
            ));
        }
    }

    let schema_version = if stored_version == Some(VERSION_SCHEMA_VERSION) {
        VERSION_SCHEMA_VERSION
    } else {
        VERSION_SCHEMA_BASE_VERSION
    };
    tx.execute_typed(
        "INSERT INTO mount_rs_schema_versions(schema_name, schema_version)
         VALUES ($1, $2)
         ON CONFLICT(schema_name) DO UPDATE SET schema_version=excluded.schema_version",
        &[
            (&VERSION_SCHEMA_NAME, Type::TEXT),
            (&schema_version, Type::INT8),
        ],
    )
    .await
    .map_err(postgres_error)?;
    tx.commit().await.map_err(postgres_error)
}

async fn load_volume_id_from_transaction(
    tx: &tokio_postgres::Transaction<'_>,
    database: &Database,
) -> Result<VolumeId> {
    let row = tx
        .query_typed_opt(
            "SELECT volume_id FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE",
            &[(&database.volume_key, Type::TEXT)],
        )
        .await
        .map_err(postgres_error)?
        .ok_or_else(|| incompatible_schema("PGlite metadata row is missing"))?;
    let value = row
        .get::<_, Option<String>>(0)
        .ok_or_else(|| incompatible_schema("PGlite metadata volume_id is missing"))?;
    VolumeId::new(value).map_err(|_| incompatible_schema("PGlite volume_id is invalid"))
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

struct PinRenewalIdentity {
    volume_matches: bool,
    version_matches: bool,
    request_owner_matches: bool,
    lease_owner_matches: bool,
}

struct PinRenewalToken {
    fence: i64,
    expires_at_ms: i64,
}

fn plan_view_pin_renewal(
    identity: PinRenewalIdentity,
    current: PinRenewalToken,
    lease: PinRenewalToken,
    now_ms: i64,
    ttl_ms: i64,
) -> Result<i64> {
    if !identity.volume_matches
        || !identity.version_matches
        || !identity.request_owner_matches
        || !identity.lease_owner_matches
        || current.fence != lease.fence
        || current.expires_at_ms != lease.expires_at_ms
        || current.expires_at_ms <= now_ms
    {
        return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
    }
    now_ms
        .checked_add(ttl_ms)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))
}

fn nonnegative(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| backend_error(format!("invalid PGlite {field}")))
}

fn version_kind_name(kind: &VersionKind) -> &'static str {
    match kind {
        VersionKind::Initial => "initial",
        VersionKind::Snapshot => "snapshot",
        VersionKind::NamespacePublication => "namespace-publication",
        VersionKind::Restore => "restore",
        VersionKind::Fork => "fork",
    }
}

fn parse_version_kind(value: &str) -> Result<VersionKind> {
    match value {
        "initial" => Ok(VersionKind::Initial),
        "snapshot" => Ok(VersionKind::Snapshot),
        "namespace-publication" => Ok(VersionKind::NamespacePublication),
        "restore" => Ok(VersionKind::Restore),
        "fork" => Ok(VersionKind::Fork),
        _ => Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("load version")
            .with_message(format!("unknown PGlite version kind '{value}'"))),
    }
}

fn decode_version_row(row: &tokio_postgres::Row) -> Result<VersionInfo> {
    decode_version_row_at(row, 0)
}

fn decode_version_row_at(row: &tokio_postgres::Row, base: usize) -> Result<VersionInfo> {
    let id_text = row.get::<_, String>(base);
    let volume_text = row.get::<_, String>(base + 2);
    let volume = VolumeId::new(volume_text)?;
    let sequence = nonnegative(row.get::<_, i64>(base + 3), "version sequence")?;
    let id = VersionId::new(volume.clone(), sequence)?;
    if id.encode() != id_text {
        return Err(FsError::new(ErrorCode::Eio)
            .with_syscall("load version")
            .with_message("stored PGlite version id does not match its volume and sequence"));
    }
    let parent = row
        .get::<_, Option<String>>(base + 4)
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let restored_from = row
        .get::<_, Option<String>>(base + 5)
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let forked_from = row
        .get::<_, Option<String>>(base + 6)
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let namespace = serde_json::from_str(&row.get::<_, String>(base + 7)).map_err(backend_error)?;
    let block_store_id =
        mount_rs_core::versioning::BlockStoreId::new(row.get::<_, String>(base + 8))?;
    let info = VersionInfo {
        id,
        parent,
        restored_from,
        forked_from,
        kind: parse_version_kind(&row.get::<_, String>(base + 9))?,
        namespace,
        block_store_id,
        created_at_ms: row.get::<_, i64>(base + 10),
        durable: row.get::<_, bool>(base + 11),
    };
    info.validate()?;
    Ok(info)
}

fn check_stored_version_row_at(
    row: &tokio_postgres::Row,
    base: usize,
    expected: &VersionId,
    syscall: &'static str,
) -> Result<()> {
    let record = decode_version_row_at(row, base)
        .and_then(|record| {
            record.validate_for_volume(&expected.volume)?;
            Ok(record)
        })
        .map_err(|_| {
            FsError::new(ErrorCode::Eio)
                .with_syscall(syscall)
                .with_message("stored PGlite version record is unreadable")
        })?;
    if &record.id != expected {
        return Err(FsError::new(ErrorCode::Eio)
            .with_syscall(syscall)
            .with_message("stored PGlite version identity differs from its record"));
    }
    Ok(())
}

const VERSION_SELECT: &str = "SELECT id, volume_key, volume_id, sequence,
 parent_id, restored_from, forked_from, namespace, block_store_id, kind,
 created_at_ms, durable FROM mount_rs_versions";

fn inode_authority(row: &tokio_postgres::Row, backing: ConcurrentBackingId) -> Result<u64> {
    if row.get::<_, Option<String>>(0).as_deref() != Some("MRC4")
        || row.get::<_, Option<String>>(1).as_deref() != Some(backing.to_hex().as_str())
        || row.get::<_, Option<String>>(2).is_some()
        || row.get::<_, i64>(3) != CONCURRENT_FENCE_SENTINEL
        || row.get::<_, i64>(4) != 0
    {
        return Err(stale());
    }
    let generation = nonnegative(row.get(5), "inode structural generation")?;
    if generation == 0 {
        return Err(stale());
    }
    Ok(generation)
}

fn decode_inode_node(inode: InodeId, json: &str) -> Result<NodeMetadata> {
    profile::add(Event::InodeReturned, json.len() as u64);
    let node: NodeMetadata = serde_json::from_str(json).map_err(backend_error)?;
    if inode == 0 || node.stats.ino != inode {
        return Err(stale());
    }
    validate_node_kind(&node)?;
    Ok(node)
}

fn encode_inode_node(node: &NodeMetadata) -> Result<String> {
    let json = serde_json::to_string(node).map_err(backend_error)?;
    profile::add(Event::InodeSerialized, json.len() as u64);
    Ok(json)
}

fn inode_conflict() -> FsError {
    FsError::new(ErrorCode::Eagain).with_syscall("publish inode metadata")
}
fn inode_signed(value: u64) -> Result<i64> {
    i64::try_from(value).map_err(|_| FsError::new(ErrorCode::Eoverflow))
}

#[async_trait]
impl MetadataStore for PgliteMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    fn publish_includes_flush_barrier(&self) -> bool {
        // `publish` awaits the successful PostgreSQL statement acknowledgement.
        // The provider flush is only a post-commit acknowledgement/dead-
        // connection probe; explicit syncfs still performs it.
        true
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT revision, namespace, write_mode FROM mount_rs_metadata WHERE volume_key = $1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        if row.get::<_, Option<String>>(2).as_deref() == Some("MRC4") {
            return Err(stale());
        }
        let revision = nonnegative(row.get::<_, i64>(0), "metadata revision")?;
        let namespace = row
            .get::<_, Option<String>>(1)
            .map(|json| {
                profile::add(Event::NamespaceReturned, json.len() as u64);
                serde_json::from_str(&json)
            })
            .transpose()
            .map_err(backend_error)?;
        Ok(LoadedMetadata {
            revision,
            namespace,
        })
    }

    async fn load_if_changed(&self, known_revision: u64) -> Result<Option<LoadedMetadata>> {
        // An out-of-range caller revision cannot match the signed provider
        // revision. Zero forces a full payload read, including initialization.
        let known = i64::try_from(known_revision).unwrap_or(0);
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT revision,
                        CASE WHEN revision > 0 AND revision = $2 THEN NULL ELSE namespace END, write_mode
                 FROM mount_rs_metadata WHERE volume_key = $1",
                &[(&self.0.volume_key, Type::TEXT), (&known, Type::INT8)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        if row.get::<_, Option<String>>(2).as_deref() == Some("MRC4") {
            return Err(stale());
        }
        let revision = nonnegative(row.get::<_, i64>(0), "metadata revision")?;
        if revision != 0 && revision == known_revision {
            return Ok(None);
        }
        // A single statement snapshot supplies both revision and payload. The
        // CASE projection suppresses unchanged namespace bytes before the wire.
        let namespace = row
            .get::<_, Option<String>>(1)
            .map(|json| {
                profile::add(Event::NamespaceReturned, json.len() as u64);
                serde_json::from_str(&json)
            })
            .transpose()
            .map_err(backend_error)?;
        Ok(Some(LoadedMetadata {
            revision,
            namespace,
        }))
    }

    async fn inode_mode_state(&self) -> Result<Option<InodeModeState>> {
        let client = self.0.lock_client().await?;
        let row = client.as_ref().ok_or_else(connection_closed)?.query_one(
            "SELECT write_mode, backing_id, owner, fence, expires, revision FROM mount_rs_metadata WHERE volume_key=$1", &[&self.0.volume_key]
        ).await.map_err(postgres_error)?;
        if row.get::<_, Option<String>>(0).as_deref() != Some("MRC4") {
            return Ok(None);
        }
        let backing =
            ConcurrentBackingId::from_hex(&row.get::<_, Option<String>>(1).ok_or_else(stale)?)
                .map_err(|_| stale())?;
        Ok(Some(InodeModeState {
            backing,
            structural_generation: inode_authority(&row, backing)?,
        }))
    }

    async fn prepare_inode_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        let expected = inode_signed(expected_revision)?;
        let generation = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        let row = tx.query_one("SELECT write_mode, backing_id, owner, fence, expires, revision, namespace FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE", &[&self.0.volume_key]).await.map_err(postgres_error)?;
        if row.get::<_, Option<String>>(0).as_deref() != Some("MRC2")
            || row.get::<_, Option<String>>(1).as_deref() != Some(backing.to_hex().as_str())
            || row.get::<_, Option<String>>(2).is_some()
            || row.get::<_, i64>(3) != CONCURRENT_FENCE_SENTINEL
            || row.get::<_, i64>(4) != 0
        {
            return Err(stale());
        }
        if row.get::<_, i64>(5) != expected {
            return Err(inode_conflict());
        }
        if expected == 0 {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        let namespace: Namespace =
            serde_json::from_str(&row.get::<_, Option<String>>(6).ok_or_else(stale)?)
                .map_err(backend_error)?;
        namespace.validate()?;
        let guards = tx
            .query(
                "SELECT inode FROM mount_rs_inode_guards WHERE volume_key=$1 FOR UPDATE",
                &[&self.0.volume_key],
            )
            .await
            .map_err(postgres_error)?;
        if !guards.is_empty() {
            return Err(stale());
        }
        for (inode, node) in &namespace.nodes {
            let node = encode_inode_node(node)?;
            tx.execute("INSERT INTO mount_rs_inode_guards(volume_key,inode,generation,revision,node) VALUES($1,$2,$3,0,$4)", &[&self.0.volume_key, &inode_signed(*inode)?, &generation, &node]).await.map_err(postgres_error)?;
        }
        let encoded =
            String::from_utf8(encode_inode_namespace(&namespace)?).map_err(backend_error)?;
        profile::add(Event::NamespaceSerialized, encoded.len() as u64);
        tx.execute(
            "UPDATE mount_rs_metadata SET write_mode='MRC4',revision=$2,namespace=$3 WHERE volume_key=$1",
            &[&self.0.volume_key, &generation, &encoded],
        )
        .await
        .map_err(postgres_error)?;
        tx.commit().await.map_err(postgres_error)
    }

    async fn load_inode_snapshot(
        &self,
        backing: ConcurrentBackingId,
    ) -> Result<InodeMetadataSnapshot> {
        let client = self.0.lock_client().await?;
        // One statement snapshot supplies the root and every authoritative guard.
        let rows = client.as_ref().ok_or_else(connection_closed)?.query(
            "SELECT m.write_mode,m.backing_id,m.owner,m.fence,m.expires,m.revision,m.namespace,g.inode,g.generation,g.revision,g.node FROM mount_rs_metadata m LEFT JOIN mount_rs_inode_guards g ON g.volume_key=m.volume_key WHERE m.volume_key=$1 ORDER BY g.inode", &[&self.0.volume_key]
        ).await.map_err(postgres_error)?;
        let first = rows.first().ok_or_else(stale)?;
        let generation = inode_authority(first, backing)?;
        let mut namespace = decode_inode_namespace(
            first
                .get::<_, Option<String>>(6)
                .ok_or_else(stale)?
                .as_bytes(),
        )?;
        let base_keys: Vec<_> = namespace.nodes.keys().copied().collect();
        namespace.nodes.clear();
        let mut revisions = BTreeMap::new();
        for row in rows {
            let inode = nonnegative(row.get::<_, Option<i64>>(7).ok_or_else(stale)?, "inode")?;
            if nonnegative(row.get(8), "guard generation")? != generation {
                return Err(stale());
            }
            let node = decode_inode_node(inode, &row.get::<_, String>(10))?;
            namespace.nodes.insert(inode, node);
            revisions.insert(inode, nonnegative(row.get(9), "inode revision")?);
        }
        if !base_keys
            .iter()
            .copied()
            .eq(namespace.nodes.keys().copied())
        {
            return Err(stale());
        }
        let snapshot = InodeMetadataSnapshot {
            structural_generation: generation,
            namespace,
            inode_revisions: revisions,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    async fn load_inode(
        &self,
        backing: ConcurrentBackingId,
        inode: InodeId,
    ) -> Result<LoadedInode> {
        self.load_inode_if_changed(backing, inode, None)
            .await?
            .ok_or_else(stale)
    }

    async fn load_inode_if_changed(
        &self,
        backing: ConcurrentBackingId,
        inode: InodeId,
        known: Option<InodeVersion>,
    ) -> Result<Option<LoadedInode>> {
        let inode_sql = inode_signed(inode)?;
        let known_gen = known
            .and_then(|v| i64::try_from(v.structural_generation).ok())
            .unwrap_or(-1);
        let known_rev = known
            .and_then(|v| i64::try_from(v.inode_revision).ok())
            .unwrap_or(-1);
        let client = self.0.lock_client().await?;
        let row = client.as_ref().ok_or_else(connection_closed)?.query_opt(
            "SELECT m.write_mode,m.backing_id,m.owner,m.fence,m.expires,m.revision,g.generation,g.revision,CASE WHEN m.revision=$3 AND g.generation=$3 AND g.revision=$4 THEN NULL ELSE g.node END FROM mount_rs_metadata m JOIN mount_rs_inode_guards g ON g.volume_key=m.volume_key WHERE m.volume_key=$1 AND g.inode=$2", &[&self.0.volume_key,&inode_sql,&known_gen,&known_rev]
        ).await.map_err(postgres_error)?.ok_or_else(stale)?;
        let generation = inode_authority(&row, backing)?;
        if nonnegative(row.get(6), "guard generation")? != generation {
            return Err(stale());
        }
        let version = InodeVersion {
            structural_generation: generation,
            inode_revision: nonnegative(row.get(7), "inode revision")?,
        };
        if known == Some(version) {
            return Ok(None);
        }
        let node = decode_inode_node(inode, &row.get::<_, Option<String>>(8).ok_or_else(stale)?)?;
        Ok(Some(LoadedInode { version, node }))
    }

    async fn publish_inode_if_version(
        &self,
        backing: ConcurrentBackingId,
        inode: InodeId,
        expected: InodeVersion,
        node: NodeMetadata,
    ) -> Result<InodeVersion> {
        let inode_sql = inode_signed(inode)?;
        let expected_gen = inode_signed(expected.structural_generation)?;
        let expected_rev = inode_signed(expected.inode_revision)?;
        let next = expected_rev
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        // Lock only this guard first. A structural writer locks all guards before
        // replacing them; a waiting publisher must reread fresh root authority.
        let guard = tx.query_opt("SELECT generation,revision,node FROM mount_rs_inode_guards WHERE volume_key=$1 AND inode=$2 FOR UPDATE", &[&self.0.volume_key,&inode_sql]).await.map_err(postgres_error)?;
        let root = tx.query_one("SELECT write_mode,backing_id,owner,fence,expires,revision FROM mount_rs_metadata WHERE volume_key=$1", &[&self.0.volume_key]).await.map_err(postgres_error)?;
        let generation = inode_authority(&root, backing)?;
        if generation != expected.structural_generation {
            return Err(inode_conflict());
        }
        let guard = guard.ok_or_else(stale)?;
        if guard.get::<_, i64>(0) != expected_gen || guard.get::<_, i64>(1) != expected_rev {
            return Err(inode_conflict());
        }
        let original = decode_inode_node(inode, &guard.get::<_, String>(2))?;
        validate_inode_publication(inode, &original, &node)?;
        let json = encode_inode_node(&node)?;
        let changed = tx.execute("UPDATE mount_rs_inode_guards SET revision=$3,node=$4 WHERE volume_key=$1 AND inode=$2 AND generation=$5 AND revision=$6", &[&self.0.volume_key,&inode_sql,&next,&json,&expected_gen,&expected_rev]).await.map_err(postgres_error)?;
        if changed != 1 {
            return Err(stale());
        }
        tx.commit().await.map_err(postgres_error)?;
        Ok(InodeVersion {
            structural_generation: generation,
            inode_revision: next as u64,
        })
    }

    async fn publish_structure_if_versions(
        &self,
        backing: ConcurrentBackingId,
        expected_generation: u64,
        expected_inode_revisions: &BTreeMap<InodeId, u64>,
        namespace: Namespace,
    ) -> Result<u64> {
        namespace.validate()?;
        let expected = inode_signed(expected_generation)?;
        let next = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        let root = tx.query_one("SELECT write_mode,backing_id,owner,fence,expires,revision,namespace FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE", &[&self.0.volume_key]).await.map_err(postgres_error)?;
        if inode_authority(&root, backing)? != expected_generation {
            return Err(inode_conflict());
        }
        let rows = tx.query("SELECT inode,generation,revision,node FROM mount_rs_inode_guards WHERE volume_key=$1 ORDER BY inode FOR UPDATE", &[&self.0.volume_key]).await.map_err(postgres_error)?;
        let mut actual = BTreeMap::new();
        let mut nodes = BTreeMap::new();
        for row in rows {
            if row.get::<_, i64>(1) != expected {
                return Err(stale());
            }
            let inode = nonnegative(row.get(0), "inode")?;
            let node = decode_inode_node(inode, &row.get::<_, String>(3))?;
            nodes.insert(inode, node);
            actual.insert(inode, nonnegative(row.get(2), "inode revision")?);
        }
        let mut base = decode_inode_namespace(
            root.get::<_, Option<String>>(6)
                .ok_or_else(stale)?
                .as_bytes(),
        )?;
        if !actual.keys().eq(base.nodes.keys()) {
            return Err(stale());
        }
        base.nodes = nodes;
        base.validate()?;
        if &actual != expected_inode_revisions {
            return Err(inode_conflict());
        }
        let json = String::from_utf8(encode_inode_namespace(&namespace)?).map_err(backend_error)?;
        profile::add(Event::NamespaceSerialized, json.len() as u64);
        tx.execute(
            "DELETE FROM mount_rs_inode_guards WHERE volume_key=$1",
            &[&self.0.volume_key],
        )
        .await
        .map_err(postgres_error)?;
        for (inode, node) in &namespace.nodes {
            let node = encode_inode_node(node)?;
            tx.execute("INSERT INTO mount_rs_inode_guards(volume_key,inode,generation,revision,node) VALUES($1,$2,$3,0,$4)", &[&self.0.volume_key,&inode_signed(*inode)?,&next,&node]).await.map_err(postgres_error)?;
        }
        tx.execute(
            "UPDATE mount_rs_metadata SET revision=$2,namespace=$3 WHERE volume_key=$1",
            &[&self.0.volume_key, &next, &json],
        )
        .await
        .map_err(postgres_error)?;
        tx.commit().await.map_err(postgres_error)?;
        Ok(next as u64)
    }

    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT write_mode, backing_id, owner, fence, expires
                 FROM mount_rs_metadata WHERE volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        let mode = row.get::<_, Option<String>>(0);
        let backing = row.get::<_, Option<String>>(1);
        let owner = row.get::<_, Option<String>>(2);
        let fence = row.get::<_, i64>(3);
        let expires = row.get::<_, i64>(4);
        match (mode.as_deref(), backing.as_deref()) {
            (None, None) if fence != CONCURRENT_FENCE_SENTINEL => Ok(ConcurrentModeState::Legacy),
            (Some(CONCURRENT_WRITE_MODE), None)
                if owner.is_none() && fence == CONCURRENT_FENCE_SENTINEL && expires == 0 =>
            {
                Ok(ConcurrentModeState::Mrc1)
            }
            (Some(BOUND_CONCURRENT_WRITE_MODE), Some(id))
                if owner.is_none() && fence == CONCURRENT_FENCE_SENTINEL && expires == 0 =>
            {
                Ok(ConcurrentModeState::Mrc2(
                    ConcurrentBackingId::from_hex(id).map_err(|_| stale())?,
                ))
            }
            (Some(BOUND_CONCURRENT_WRITE_MODE), _) | (Some("MRC3"), _) | (Some("MRC4"), _) => {
                Err(stale())
            }
            _ => Err(backend_error(
                "PGlite concurrent mode, backing ID, and fence disagree",
            )),
        }
    }

    async fn preflight_new_bound_mode(&self) -> Result<()> {
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT write_mode IS NULL AND backing_id IS NULL AND revision=0
                        AND namespace IS NULL AND owner IS NULL AND fence=0 AND expires=0
                        AND EXISTS (SELECT 1 FROM mount_rs_version_state
                                    WHERE volume_key=$1 AND head_id IS NULL
                                      AND next_sequence=1 AND next_read_fence=0)
                        AND NOT EXISTS (SELECT 1 FROM mount_rs_versions WHERE volume_key=$1)
                        AND NOT EXISTS (SELECT 1 FROM mount_rs_version_pins WHERE volume_key=$1)
                 FROM mount_rs_metadata WHERE volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        if row.get::<_, bool>(0) {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Ebusy).with_syscall("preflight new bound PGlite metadata"))
        }
    }

    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        let client = self.0.lock_client().await?;
        let backing_text = backing.to_hex();
        let changed = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "UPDATE mount_rs_metadata SET write_mode=$2, backing_id=$3, fence=$4
                 WHERE volume_key=$1 AND write_mode IS NULL AND backing_id IS NULL
                   AND revision=0 AND namespace IS NULL
                   AND owner IS NULL AND fence=0 AND expires=0
                   AND EXISTS (
                     SELECT 1 FROM mount_rs_version_state
                     WHERE volume_key=$1 AND head_id IS NULL
                       AND next_sequence=1 AND next_read_fence=0)
                   AND NOT EXISTS (SELECT 1 FROM mount_rs_versions WHERE volume_key=$1)
                   AND NOT EXISTS (SELECT 1 FROM mount_rs_version_pins WHERE volume_key=$1)",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&BOUND_CONCURRENT_WRITE_MODE, Type::TEXT),
                    (&backing_text, Type::TEXT),
                    (&CONCURRENT_FENCE_SENTINEL, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed == 1 {
            return Ok(());
        }
        drop(client);
        match self.concurrent_mode_state().await? {
            ConcurrentModeState::Mrc2(id) if id == backing => Ok(()),
            ConcurrentModeState::Mrc2(_) => Err(stale()),
            ConcurrentModeState::Mrc1 => Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("prepare bound concurrent PGlite volume")
                .with_message("MRC1 requires migrate-concurrent-backing")),
            ConcurrentModeState::Legacy => Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("prepare bound concurrent PGlite volume")),
        }
    }

    async fn delegation_state(&self) -> Result<Option<DelegationState>> {
        let client = self.0.lock_client().await?;
        let row = client.as_ref().ok_or_else(connection_closed)?.query_one(
            "SELECT write_mode, backing_id, delegation, namespace, owner, fence, expires FROM mount_rs_metadata WHERE volume_key=$1", &[&self.0.volume_key]
        ).await.map_err(postgres_error)?;
        if row.get::<_, Option<String>>(0).as_deref() != Some("MRC3") {
            return Ok(None);
        }
        let (state, _) = decode_delegation_row(&row)?;
        Ok(Some(state))
    }

    async fn prepare_delegated_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        let mut guard = self.0.lock_client().await?;
        let tx = guard
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        let row = tx.query_one("SELECT write_mode, backing_id, delegation, namespace, owner, fence, expires, revision FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE", &[&self.0.volume_key]).await.map_err(postgres_error)?;
        let revision = nonnegative(row.get(7), "revision")?;
        if row.get::<_, Option<String>>(0).as_deref() == Some("MRC3") {
            let (state, _) = decode_delegation_row(&row)?;
            if state.backing != backing {
                return Err(stale());
            }
            tx.commit().await.map_err(postgres_error)?;
            return Ok(());
        }
        if revision != expected_revision {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        let mode = row.get::<_, Option<String>>(0);
        let valid_mode = mode.is_none() || mode.as_deref() == Some(BOUND_CONCURRENT_WRITE_MODE);
        if !valid_mode || row.get::<_, Option<String>>(4).is_some() {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        if mode.is_some()
            && row.get::<_, Option<String>>(1).as_deref() != Some(backing.to_hex().as_str())
        {
            return Err(stale());
        }
        if row.get::<_, Option<String>>(2).is_some()
            || (mode.is_none()
                && (row.get::<_, Option<String>>(1).is_some()
                    || row.get::<_, i64>(5) == CONCURRENT_FENCE_SENTINEL))
            || (mode.is_some() && row.get::<_, i64>(5) != CONCURRENT_FENCE_SENTINEL)
            || row.get::<_, i64>(6) != 0
        {
            return Err(stale());
        }
        let ns: Namespace =
            serde_json::from_str(&row.get::<_, Option<String>>(3).ok_or_else(stale)?)
                .map_err(backend_error)?;
        ns.validate()?;
        let historical: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM mount_rs_versions WHERE volume_key=$1) OR EXISTS(SELECT 1 FROM mount_rs_version_pins WHERE volume_key=$1) OR EXISTS(SELECT 1 FROM mount_rs_version_state WHERE volume_key=$1 AND (head_id IS NOT NULL OR next_sequence<>1 OR next_read_fence<>0))", &[&self.0.volume_key]).await.map_err(postgres_error)?.get(0);
        if historical {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        let state = serde_json::to_string(&DelegationState::new(backing)).map_err(backend_error)?;
        let changed = tx.execute("UPDATE mount_rs_metadata SET write_mode='MRC3', backing_id=$2, delegation=$3, owner=NULL, fence=$4, expires=0 WHERE volume_key=$1", &[&self.0.volume_key, &backing.to_hex(), &state, &CONCURRENT_FENCE_SENTINEL]).await.map_err(postgres_error)?;
        if changed != 1 {
            return Err(stale());
        }
        tx.commit().await.map_err(postgres_error)?;
        Ok(())
    }

    async fn checkout(&self, request: &CheckoutRequest) -> Result<DirectoryGrant> {
        self.delegation_transaction(request.backing, None, false, None, |state, ns| {
            state.checkout(ns, request.root, &request.owner)
        })
        .await
    }
    async fn publish_delegated(
        &self,
        request: &DelegatedPublish,
        namespace: Namespace,
    ) -> Result<u64> {
        self.delegation_transaction(
            request.backing,
            Some(request.expected_revision),
            true,
            None,
            |state, ns| {
                state.authorize_publish(ns, &namespace, &request.token)?;
                *ns = namespace;
                Ok(())
            },
        )
        .await?;
        request.expected_revision.checked_add(1).ok_or_else(stale)
    }
    async fn checkin(&self, request: &DelegatedCheckin) -> Result<()> {
        self.delegation_transaction(
            request.backing,
            Some(request.expected_revision),
            false,
            Some(&request.token),
            |state, ns| state.checkin(&request.token, ns),
        )
        .await
    }
    async fn recover(&self, request: &DelegatedRecovery) -> Result<()> {
        self.delegation_transaction(request.backing, None, false, None, |state, ns| {
            state.recover(request.root, request.expected_fence, ns)
        })
        .await
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        if owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let ttl = ttl_ms(ttl)?;
        let owner = owner.to_owned();
        let sql = format!(
            "UPDATE mount_rs_metadata SET owner=$2, fence=fence+1, expires={NOW}+$3
             WHERE volume_key=$1 AND write_mode IS NULL
               AND (owner IS NULL OR expires<={NOW})
               AND fence<9223372036854775807 AND $3<=9223372036854775807-{NOW}
             RETURNING fence, expires"
        );
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                &sql,
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&owner, Type::TEXT),
                    (&ttl, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| FsError::new(ErrorCode::Eagain).with_syscall("acquire writer"))?;
        Ok(WriterLease {
            owner,
            fence: nonnegative(row.get(0), "metadata fence")?,
            expires_at_ms: nonnegative(row.get(1), "metadata expiry")?,
        })
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        let ttl = ttl_ms(ttl)?;
        let (fence, expires) = lease_numbers(lease)?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET expires={NOW}+$5
             WHERE volume_key=$1 AND write_mode IS NULL
               AND owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}
               AND $5<=9223372036854775807-{NOW}
             RETURNING expires"
        );
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                &sql,
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                    (&ttl, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(stale)?;
        Ok(WriterLease {
            expires_at_ms: nonnegative(row.get(0), "metadata expiry")?,
            ..lease.clone()
        })
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        let (fence, expires) = lease_numbers(lease)?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET owner=NULL, expires=0
             WHERE volume_key=$1 AND write_mode IS NULL
               AND owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}"
        );
        let client = self.0.lock_client().await?;
        let changed = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                &sql,
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?;
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
        profile::add(Event::NamespaceSerialized, namespace.len() as u64);

        let mut client = self.0.lock_client().await?;
        let client = client.as_mut().ok_or_else(connection_closed)?;
        // A single PostgreSQL DML statement is an atomic autocommit
        // transaction. Keep the common successful CAS to one server round
        // trip; only a zero-row result needs the explicit locked read below
        // to retain the stale-versus-revision classification.
        let changed = client
            .execute_typed(
                &format!(
                    "UPDATE mount_rs_metadata SET revision=$2, namespace=$3
                     WHERE volume_key=$1 AND revision=$4 AND write_mode IS NULL
                       AND owner IS NOT DISTINCT FROM $5 AND fence=$6 AND expires=$7
                       AND expires>{NOW}"
                ),
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&next, Type::INT8),
                    (&namespace, Type::TEXT),
                    (&expected, Type::INT8),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed != 1 {
            // The conditional update is the successful-path CAS. Only the
            // exceptional path needs a locked read to preserve the exact
            // stale-versus-revision-conflict classification that callers
            // receive from the former preflight SELECT.
            let tx = client.transaction().await.map_err(postgres_error)?;
            let state = match tx
                .query_typed_opt(
                    &format!(
                        "SELECT
                             (write_mode IS NULL AND owner IS NOT DISTINCT FROM $2 AND fence=$3 AND expires=$4
                              AND expires>{NOW}) AS lease_valid,
                             (revision=$5) AS revision_valid
                         FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE"
                    ),
                    &[
                        (&self.0.volume_key, Type::TEXT),
                        (&lease.owner, Type::TEXT),
                        (&fence, Type::INT8),
                        (&expires, Type::INT8),
                        (&expected, Type::INT8),
                    ],
                )
                .await
            {
                Ok(Some(row)) => (row.get::<_, bool>(0), row.get::<_, bool>(1)),
                Ok(None) => {
                    let _ = tx.rollback().await;
                    return Err(backend_error("PGlite metadata row is missing"));
                }
                Err(error) => {
                    let _ = tx.rollback().await;
                    return Err(postgres_error(error));
                }
            };
            let error = if !state.0 {
                stale()
            } else if !state.1 {
                FsError::new(ErrorCode::Eagain).with_syscall("publish metadata")
            } else {
                // The row was locked and the revision and lease were still
                // valid, so an unexplained zero-row CAS is fail-closed.
                stale()
            };
            let _ = tx.rollback().await;
            return Err(error);
        }
        Ok(next as u64)
    }

    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        namespace.validate()?;
        let expected =
            i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let next = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let namespace = serde_json::to_string(&namespace).map_err(backend_error)?;
        profile::add(Event::NamespaceSerialized, namespace.len() as u64);
        let backing_text = backing.to_hex();
        let client = self.0.lock_client().await?;
        // A failed acknowledgement may follow a committed update. Preserve
        // transport errors; only a confirmed zero-row update is classified.
        let changed = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "UPDATE mount_rs_metadata SET revision=$2, namespace=$3
                 WHERE volume_key=$1 AND revision=$4 AND write_mode=$5
                   AND owner IS NULL AND fence=$6 AND expires=0 AND backing_id=$7",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&next, Type::INT8),
                    (&namespace, Type::TEXT),
                    (&expected, Type::INT8),
                    (&BOUND_CONCURRENT_WRITE_MODE, Type::TEXT),
                    (&CONCURRENT_FENCE_SENTINEL, Type::INT8),
                    (&backing_text, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed == 1 {
            return Ok(next as u64);
        }
        if changed != 0 {
            return Err(backend_error("PGlite bound CAS changed multiple rows"));
        }
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT revision, write_mode, backing_id, owner, fence, expires
                 FROM mount_rs_metadata WHERE volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        let mode = row.get::<_, Option<String>>(1);
        let actual = row.get::<_, Option<String>>(2);
        if mode.as_deref() == Some(BOUND_CONCURRENT_WRITE_MODE)
            && actual.as_deref() != Some(backing_text.as_str())
        {
            return Err(stale());
        }
        if mode.as_deref() != Some(BOUND_CONCURRENT_WRITE_MODE)
            || row.get::<_, Option<String>>(3).is_some()
            || row.get::<_, i64>(4) != CONCURRENT_FENCE_SENTINEL
            || row.get::<_, i64>(5) != 0
        {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("publish bound concurrent PGlite metadata"));
        }
        if row.get::<_, i64>(0) != expected {
            return Err(FsError::new(ErrorCode::Eagain)
                .with_syscall("publish bound concurrent PGlite metadata"));
        }
        Err(backend_error(
            "PGlite bound CAS returned zero for unchanged revision",
        ))
    }

    async fn migrate_mrc1_to_bound_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        let expected =
            i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let backing_text = backing.to_hex();
        let client = self.0.lock_client().await?;
        let changed = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "UPDATE mount_rs_metadata SET write_mode=$2, backing_id=$3
                 WHERE volume_key=$1 AND revision=$4 AND write_mode=$5 AND backing_id IS NULL
                   AND owner IS NULL AND fence=$6 AND expires=0
                   AND EXISTS (
                     SELECT 1 FROM mount_rs_version_state
                     WHERE volume_key=$1 AND head_id IS NULL
                       AND next_sequence=1 AND next_read_fence=0)
                   AND NOT EXISTS (SELECT 1 FROM mount_rs_versions WHERE volume_key=$1)
                   AND NOT EXISTS (SELECT 1 FROM mount_rs_version_pins WHERE volume_key=$1)",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&BOUND_CONCURRENT_WRITE_MODE, Type::TEXT),
                    (&backing_text, Type::TEXT),
                    (&expected, Type::INT8),
                    (&CONCURRENT_WRITE_MODE, Type::TEXT),
                    (&CONCURRENT_FENCE_SENTINEL, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed == 1 {
            return Ok(());
        }
        if changed != 0 {
            return Err(backend_error("PGlite migration changed multiple rows"));
        }
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT revision FROM mount_rs_metadata WHERE volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        if row.get::<_, i64>(0) != expected {
            return Err(
                FsError::new(ErrorCode::Eagain).with_syscall("migrate concurrent PGlite mode")
            );
        }
        Err(FsError::new(ErrorCode::Ebusy).with_syscall("migrate concurrent PGlite mode"))
    }

    async fn preflight_mrc1_to_bound_mode(&self, expected_revision: u64) -> Result<()> {
        let expected =
            i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT revision,
                        write_mode=$2 AND backing_id IS NULL AND owner IS NULL
                        AND fence=$3 AND expires=0
                        AND EXISTS (SELECT 1 FROM mount_rs_version_state
                                    WHERE volume_key=$1 AND head_id IS NULL
                                      AND next_sequence=1 AND next_read_fence=0)
                        AND NOT EXISTS (SELECT 1 FROM mount_rs_versions WHERE volume_key=$1)
                        AND NOT EXISTS (SELECT 1 FROM mount_rs_version_pins WHERE volume_key=$1)
                 FROM mount_rs_metadata WHERE volume_key=$1",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&CONCURRENT_WRITE_MODE, Type::TEXT),
                    (&CONCURRENT_FENCE_SENTINEL, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        if row.get::<_, i64>(0) != expected {
            return Err(
                FsError::new(ErrorCode::Eagain).with_syscall("preflight MRC1 PGlite metadata")
            );
        }
        if row.get::<_, bool>(1) {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Ebusy).with_syscall("preflight MRC1 PGlite metadata"))
        }
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }
}

#[async_trait]
impl VersionedMetadataStore for PgliteMetadataStore {
    fn volume_id(&self) -> VolumeId {
        self.1.clone()
    }

    async fn version_head(&self) -> Result<Option<VersionHead>> {
        let client = self.0.lock_client().await?;
        // One statement observes one committed snapshot; successive SELECTs
        // could mix a pre-publication revision with a post-publication head.
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT m.revision, s.head_id, s.volume_key IS NOT NULL,
                        v.id, v.volume_key, v.volume_id, v.sequence,
                        v.parent_id, v.restored_from, v.forked_from, v.namespace,
                        v.block_store_id, v.kind, v.created_at_ms, v.durable
                 FROM mount_rs_metadata m
                 LEFT JOIN mount_rs_version_state s ON s.volume_key=m.volume_key
                 LEFT JOIN mount_rs_versions v ON v.volume_key=m.volume_key
                      AND v.id=s.head_id AND v.volume_id=$2
                 WHERE m.volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT), (&self.1.0, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        let revision = nonnegative(row.get::<_, i64>(0), "metadata revision")?;
        if !row.get::<_, bool>(2) {
            return Err(incompatible_schema("PGlite version state is missing"));
        }
        let Some(head_id) = row.get::<_, Option<String>>(1) else {
            return Ok(None);
        };
        let version = VersionId::decode(&head_id).map_err(|_| {
            FsError::new(ErrorCode::Eio)
                .with_syscall("version head")
                .with_message("stored PGlite version head is malformed")
        })?;
        if version.volume != self.1 {
            return Err(FsError::new(ErrorCode::Eio)
                .with_syscall("version head")
                .with_message("stored PGlite version head belongs to another volume"));
        }
        if row.get::<_, Option<String>>(3).is_none() {
            return Err(FsError::new(ErrorCode::Eio)
                .with_syscall("version head")
                .with_message("stored PGlite version head references a missing version"));
        }
        check_stored_version_row_at(&row, 3, &version, "version head")?;
        Ok(Some(VersionHead { version, revision }))
    }

    async fn load_version(&self, id: &VersionId) -> Result<VersionInfo> {
        if id.volume != self.1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("load version"));
        }
        let encoded = id.encode();
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                &format!("{VERSION_SELECT} WHERE volume_key=$1 AND id=$2"),
                &[(&self.0.volume_key, Type::TEXT), (&encoded, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("load version"))?;
        let info = decode_version_row(&row)?;
        info.validate_for_volume(&self.1)?;
        if info.id != *id {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("load version"));
        }
        Ok(info)
    }

    async fn list_versions(&self) -> Result<Vec<VersionInfo>> {
        let client = self.0.lock_client().await?;
        let rows = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed(
                &format!("{VERSION_SELECT} WHERE volume_key=$1 ORDER BY sequence"),
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        rows.into_iter()
            .map(|row| {
                let version = decode_version_row(&row)?;
                version.validate_for_volume(&self.1)?;
                Ok(version)
            })
            .collect()
    }

    async fn find_publication(&self, operation_id: &PublicationId) -> Result<Option<VersionInfo>> {
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT id FROM mount_rs_versions
                 WHERE volume_key=$1 AND operation_id=$2",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&operation_id.0, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let encoded = row.get::<_, String>(0);
        let id = VersionId::decode(&encoded).map_err(|_| {
            FsError::new(ErrorCode::Eio)
                .with_syscall("find publication")
                .with_message("stored PGlite publication references a malformed version")
        })?;
        drop(client);
        self.load_version(&id).await.map(Some)
    }

    async fn publish_version(
        &self,
        lease: &WriterLease,
        publication: VersionPublication,
    ) -> Result<VersionInfo> {
        publication.validate(&self.1)?;
        if publication.durable && !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("publish version")
                .with_message("PGlite provider is not configured as durable"));
        }
        let expected_revision = i64::try_from(publication.expected_revision)
            .map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let (fence, expires) = lease_numbers(lease)?;
        let operation_id = publication.operation_id.0.clone();
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;

        let existing = tx
            .query_typed_opt(
                &format!("{VERSION_SELECT} WHERE volume_key=$1 AND operation_id=$2"),
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&operation_id, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if let Some(row) = existing {
            let committed = decode_version_row(&row)?;
            committed.validate_for_volume(&self.1)?;
            if !publication.matches_committed(&committed)? {
                return Err(FsError::new(ErrorCode::Eexist)
                    .with_syscall("publish version")
                    .with_message("publication id was reused with a different payload"));
            }
            tx.commit().await.map_err(postgres_error)?;
            return Ok(committed);
        }

        let state = tx
            .query_typed_opt(
                &format!(
                    "SELECT
                         (write_mode IS NULL AND owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}) AS lease_valid,
                         (revision=$5) AS revision_valid,
                         (SELECT head_id FROM mount_rs_version_state WHERE volume_key=$1)
                     FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE"
                ),
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                    (&expected_revision, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        if !state.get::<_, bool>(0) {
            return Err(stale());
        }
        if !state.get::<_, bool>(1) {
            return Err(FsError::new(ErrorCode::Eagain).with_syscall("publish version"));
        }
        let head_id = state.get::<_, Option<String>>(2);
        let expected_parent = publication.expected_parent.as_ref().map(VersionId::encode);
        if head_id != expected_parent {
            return Err(FsError::new(ErrorCode::Eagain)
                .with_syscall("publish version")
                .with_message("version head changed concurrently"));
        }
        if let Some(head_id) = head_id.as_deref() {
            let head = VersionId::decode(head_id).map_err(|_| {
                FsError::new(ErrorCode::Eio)
                    .with_syscall("publish version")
                    .with_message("stored PGlite version head is malformed")
            })?;
            let row = tx
                .query_typed_opt(
                    &format!("{VERSION_SELECT} WHERE volume_key=$1 AND id=$2 AND volume_id=$3"),
                    &[
                        (&self.0.volume_key, Type::TEXT),
                        (&head_id, Type::TEXT),
                        (&self.1.0, Type::TEXT),
                    ],
                )
                .await
                .map_err(postgres_error)?
                .ok_or_else(|| {
                    FsError::new(ErrorCode::Eio)
                        .with_syscall("publish version")
                        .with_message("stored PGlite version head references a missing version")
                })?;
            check_stored_version_row_at(&row, 0, &head, "publish version")?;
        }

        let state_row = tx
            .query_typed_opt(
                "SELECT next_sequence FROM mount_rs_version_state
                 WHERE volume_key=$1 FOR UPDATE",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| incompatible_schema("PGlite version state is missing"))?;
        let sequence = nonnegative(state_row.get::<_, i64>(0), "version sequence")?;
        let sequence_i64 =
            i64::try_from(sequence).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let id = VersionId::new(self.1.clone(), sequence)?;
        let id_text = id.encode();
        let created_at_ms = tx
            .query_typed_opt(NOW_SELECT, &[])
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite clock query returned no row"))?
            .get::<_, i64>(0);
        let namespace = serde_json::to_string(&publication.namespace).map_err(backend_error)?;
        let parent_id = publication.expected_parent.as_ref().map(VersionId::encode);
        let restored_from = publication.restored_from.as_ref().map(VersionId::encode);
        let forked_from = publication.forked_from.as_ref().map(VersionId::encode);
        let kind = version_kind_name(&publication.kind);
        let durable = publication.durable;
        tx.execute_typed(
            "INSERT INTO mount_rs_versions
             (volume_key, id, volume_id, sequence, parent_id, restored_from,
              forked_from, namespace, block_store_id, kind, created_at_ms,
              durable, operation_id)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            &[
                (&self.0.volume_key, Type::TEXT),
                (&id_text, Type::TEXT),
                (&self.1.0, Type::TEXT),
                (&sequence_i64, Type::INT8),
                (&parent_id, Type::TEXT),
                (&restored_from, Type::TEXT),
                (&forked_from, Type::TEXT),
                (&namespace, Type::TEXT),
                (&publication.block_store_id.0, Type::TEXT),
                (&kind, Type::TEXT),
                (&created_at_ms, Type::INT8),
                (&durable, Type::BOOL),
                (&operation_id, Type::TEXT),
            ],
        )
        .await
        .map_err(postgres_error)?;
        if publication.kind == VersionKind::NamespacePublication {
            let changed = tx
                .execute_typed(
                    "UPDATE mount_rs_schema_versions SET schema_version=$2
                     WHERE schema_name=$1 AND schema_version BETWEEN $3 AND $2",
                    &[
                        (&VERSION_SCHEMA_NAME, Type::TEXT),
                        (&VERSION_SCHEMA_VERSION, Type::INT8),
                        (&VERSION_SCHEMA_BASE_VERSION, Type::INT8),
                    ],
                )
                .await
                .map_err(postgres_error)?;
            if changed != 1 {
                return Err(incompatible_schema("version kind schema gate is missing"));
            }
        }
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let metadata_update_sql = format!(
            "UPDATE mount_rs_metadata SET revision=$2, namespace=$3
                 WHERE volume_key=$1 AND revision=$4 AND write_mode IS NULL
                   AND owner=$5 AND fence=$6 AND expires=$7 AND expires>{NOW}"
        );
        let changed = tx
            .execute_typed(
                &metadata_update_sql,
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&next_revision, Type::INT8),
                    (&namespace, Type::TEXT),
                    (&expected_revision, Type::INT8),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed != 1 {
            return Err(stale());
        }
        let next_sequence = sequence_i64
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        tx.execute_typed(
            "UPDATE mount_rs_version_state SET head_id=$2, next_sequence=$3
             WHERE volume_key=$1",
            &[
                (&self.0.volume_key, Type::TEXT),
                (&id_text, Type::TEXT),
                (&next_sequence, Type::INT8),
            ],
        )
        .await
        .map_err(postgres_error)?;
        tx.commit().await.map_err(postgres_error)?;
        if publication.durable {
            self.0.flush().await?;
        }

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
        let ttl_ms =
            i64::try_from(request.validate()?).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        if id.volume != self.1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("open version view"));
        }
        let encoded = id.encode();
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        // Serialize with deletion before observing version existence, then
        // retain the volume lock until the new pin commits.
        tx.query_typed_opt(
            "SELECT 1 FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE",
            &[(&self.0.volume_key, Type::TEXT)],
        )
        .await
        .map_err(postgres_error)?
        .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        let row = tx
            .query_typed_opt(
                &format!("{VERSION_SELECT} WHERE volume_key=$1 AND id=$2 AND volume_id=$3"),
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&encoded, Type::TEXT),
                    (&self.1.0, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("open version view"))?;
        check_stored_version_row_at(&row, 0, id, "open version view")?;
        let state = tx
            .query_typed_opt(
                "SELECT next_read_fence FROM mount_rs_version_state
                 WHERE volume_key=$1 FOR UPDATE",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| incompatible_schema("PGlite version state is missing"))?;
        let current_fence = state.get::<_, i64>(0);
        let next_fence = current_fence
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let now_ms = tx
            .query_typed_opt(NOW_SELECT, &[])
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite clock query returned no row"))?
            .get::<_, i64>(0);
        let expires = now_ms
            .checked_add(ttl_ms)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let view_id = format!("{}-view-{next_fence}", self.1);
        tx.execute_typed(
            "UPDATE mount_rs_version_state SET next_read_fence=$2 WHERE volume_key=$1",
            &[(&self.0.volume_key, Type::TEXT), (&next_fence, Type::INT8)],
        )
        .await
        .map_err(postgres_error)?;
        tx.execute_typed(
            "INSERT INTO mount_rs_version_pins
             (volume_key, view_id, volume_id, version_id, owner, fence, expires)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
            &[
                (&self.0.volume_key, Type::TEXT),
                (&view_id, Type::TEXT),
                (&self.1.0, Type::TEXT),
                (&encoded, Type::TEXT),
                (&request.owner, Type::TEXT),
                (&next_fence, Type::INT8),
                (&expires, Type::INT8),
            ],
        )
        .await
        .map_err(postgres_error)?;
        tx.commit().await.map_err(postgres_error)?;
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
        if lease.volume != self.1 || lease.version.volume != self.1 {
            return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
        }
        let ttl_ms =
            i64::try_from(request.validate()?).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let fence = i64::try_from(lease.fence).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let expires =
            i64::try_from(lease.expires_at_ms).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        // An expired pin can be deleted while it is being renewed unless both
        // operations lock this same volume row before checking the pin.
        tx.query_typed_opt(
            "SELECT 1 FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE",
            &[(&self.0.volume_key, Type::TEXT)],
        )
        .await
        .map_err(postgres_error)?
        .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        let current = tx
            .query_typed_opt(
                "SELECT volume_id, version_id, owner, fence, expires
                 FROM mount_rs_version_pins
                 WHERE volume_key=$1 AND view_id=$2 FOR UPDATE",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.view_id, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?;
        let Some(current) = current else {
            return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
        };
        let current_volume = current.get::<_, String>(0);
        let current_version = current.get::<_, String>(1);
        let current_owner = current.get::<_, String>(2);
        let current_fence = current.get::<_, i64>(3);
        let current_expires = current.get::<_, i64>(4);
        let now_ms = tx
            .query_typed_opt(NOW_SELECT, &[])
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite clock query returned no row"))?
            .get::<_, i64>(0);
        let next_expires = plan_view_pin_renewal(
            PinRenewalIdentity {
                volume_matches: current_volume == self.1.0,
                version_matches: current_version == lease.version.encode(),
                request_owner_matches: current_owner == request.owner,
                lease_owner_matches: current_owner == lease.owner,
            },
            PinRenewalToken {
                fence: current_fence,
                expires_at_ms: current_expires,
            },
            PinRenewalToken {
                fence,
                expires_at_ms: expires,
            },
            now_ms,
            ttl_ms,
        )?;
        let changed = tx
            .execute_typed(
                "UPDATE mount_rs_version_pins SET expires=$3
                 WHERE volume_key=$1 AND view_id=$2 AND fence=$4 AND expires=$5",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.view_id, Type::TEXT),
                    (&next_expires, Type::INT8),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed != 1 {
            return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
        }
        tx.commit().await.map_err(postgres_error)?;
        Ok(ReadLease {
            expires_at_ms: u64::try_from(next_expires).map_err(|_| FsError::new(ErrorCode::Eio))?,
            ..lease.clone()
        })
    }

    async fn close_view_pin(&self, lease: &ReadLease) -> Result<()> {
        let fence = i64::try_from(lease.fence).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let expires =
            i64::try_from(lease.expires_at_ms).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let encoded = lease.version.encode();
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        let changed = tx
            .execute_typed(
                "DELETE FROM mount_rs_version_pins
                 WHERE volume_key=$1 AND view_id=$2 AND volume_id=$3
                   AND version_id=$4 AND owner=$5 AND fence=$6 AND expires=$7",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.view_id, Type::TEXT),
                    (&lease.volume.0, Type::TEXT),
                    (&encoded, Type::TEXT),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed == 1 {
            tx.commit().await.map_err(postgres_error)?;
            return Ok(());
        }
        let exists = tx
            .query_typed_opt(
                "SELECT EXISTS(
                     SELECT 1 FROM mount_rs_version_pins
                     WHERE volume_key=$1 AND view_id=$2
                 )",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.view_id, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| incompatible_schema("PGlite pin query returned no row"))?
            .get::<_, bool>(0);
        tx.commit().await.map_err(postgres_error)?;
        if exists {
            Err(FsError::new(ErrorCode::Estale).with_syscall("close view"))
        } else {
            Ok(())
        }
    }

    async fn delete_version(&self, lease: &WriterLease, id: &VersionId) -> Result<()> {
        if id.volume != self.1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("delete version"));
        }
        let (fence, expires) = lease_numbers(lease)?;
        let encoded = id.encode();
        let mut client = self.0.lock_client().await?;
        let tx = client
            .as_mut()
            .ok_or_else(connection_closed)?
            .transaction()
            .await
            .map_err(postgres_error)?;
        let valid = tx
            .query_typed_opt(
                &format!(
                    "SELECT write_mode IS NULL AND owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}
                     FROM mount_rs_metadata WHERE volume_key=$1 FOR UPDATE"
                ),
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&lease.owner, Type::TEXT),
                    (&fence, Type::INT8),
                    (&expires, Type::INT8),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?
            .get::<_, bool>(0);
        if !valid {
            return Err(stale());
        }
        let head_id = tx
            .query_typed_opt(
                "SELECT head_id FROM mount_rs_version_state WHERE volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| incompatible_schema("PGlite version state is missing"))?
            .get::<_, Option<String>>(0);
        if head_id.as_deref() == Some(&encoded) {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("delete version")
                .with_message("current version cannot be deleted"));
        }
        let protected = tx
            .query_typed_opt(
                &format!(
                    "SELECT EXISTS(
                         SELECT 1 FROM mount_rs_version_pins
                         WHERE volume_key=$1 AND version_id=$2 AND expires>{NOW}
                     )"
                ),
                &[(&self.0.volume_key, Type::TEXT), (&encoded, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| incompatible_schema("PGlite pin query returned no row"))?
            .get::<_, bool>(0);
        if protected {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("delete version")
                .with_message("version has an active read lease"));
        }
        let changed = tx
            .execute_typed(
                "DELETE FROM mount_rs_versions WHERE volume_key=$1 AND id=$2",
                &[(&self.0.volume_key, Type::TEXT), (&encoded, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        if changed != 1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("delete version"));
        }
        tx.commit().await.map_err(postgres_error)
    }
}

#[async_trait]
impl BlockStore for PgliteBlockStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        let candidate = uuid::Uuid::new_v4().simple().to_string();
        let client = self.0.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "INSERT INTO mount_rs_block_authority (volume_key, backing_id) VALUES ($1, $2)
                 ON CONFLICT (volume_key) DO NOTHING",
                &[(&self.0.volume_key, Type::TEXT), (&candidate, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT backing_id FROM mount_rs_block_authority WHERE volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite block authority disappeared"))?;
        ConcurrentBackingId::from_hex(&row.get::<_, String>(0))
            .map_err(|_| backend_error("PGlite block authority ID is invalid"))
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        let client = self.0.lock_client().await?;
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT backing_id FROM mount_rs_block_authority WHERE volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        let Some(row) = row else {
            return Err(stale());
        };
        let actual =
            ConcurrentBackingId::from_hex(&row.get::<_, String>(0)).map_err(|_| stale())?;
        if actual != expected {
            return Err(stale());
        }
        Ok(())
    }

    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        let bytes = self.get(id).await?;
        if block_id(&bytes) != id.0 {
            return Err(FsError::new(ErrorCode::Eio).with_syscall("verify PGlite migration block"));
        }
        Ok(bytes)
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let bytes = bytes.to_vec();
        let client = self.0.lock_client().await?;
        // A collision is checked below and never aliases different content.
        let id = block_id(&bytes);
        let changed = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "INSERT INTO mount_rs_blocks (volume_key, id, bytes) VALUES ($1, $2, $3)
                 ON CONFLICT (volume_key, id) DO NOTHING",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&id, Type::TEXT),
                    (&bytes, Type::BYTEA),
                ],
            )
            .await
            .map_err(postgres_error)?;
        if changed == 1 {
            return Ok(BlockId(id));
        }

        let existing = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT bytes FROM mount_rs_blocks WHERE volume_key=$1 AND id=$2",
                &[(&self.0.volume_key, Type::TEXT), (&id, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite block conflict disappeared"))?
            .get::<_, Vec<u8>>(0);
        if existing == bytes {
            Ok(BlockId(id))
        } else {
            Err(backend_error("PGlite block identity collision"))
        }
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        let client = self.0.lock_client().await?;
        let bytes = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT bytes FROM mount_rs_blocks WHERE volume_key=$1 AND id=$2",
                &[(&self.0.volume_key, Type::TEXT), (&id.0, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .map(|row| row.get::<_, Vec<u8>>(0))
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("get block"))?;
        drop(client);
        if block_id(&bytes) != id.0 {
            return Err(FsError::new(ErrorCode::Eio).with_syscall("verify PGlite block digest"));
        }
        Ok(bytes)
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        let client = self.0.lock_client().await?;
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .execute_typed(
                "DELETE FROM mount_rs_blocks WHERE volume_key=$1 AND id=$2",
                &[(&self.0.volume_key, Type::TEXT), (&id.0, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?;
        Ok(())
    }
}

#[cfg(kani)]
mod verification {
    use super::*;

    #[kani::proof]
    #[kani::unwind(8)]
    fn view_pin_renewal_requires_exact_live_token_and_checked_expiry() {
        let volume_matches: bool = kani::any();
        let version_matches: bool = kani::any();
        let request_owner_matches: bool = kani::any();
        let lease_owner_matches: bool = kani::any();
        let current_fence: i64 = kani::any();
        let lease_fence: i64 = kani::any();
        let current_expires: i64 = kani::any();
        let lease_expires: i64 = kani::any();
        let now_ms: i64 = kani::any();
        let ttl_ms: i64 = kani::any();
        kani::assume(current_fence > 0);
        kani::assume(lease_fence >= 0);
        kani::assume(current_expires >= 0);
        kani::assume(lease_expires >= 0);
        kani::assume(now_ms >= 0);
        kani::assume(ttl_ms > 0);

        let observed = plan_view_pin_renewal(
            PinRenewalIdentity {
                volume_matches,
                version_matches,
                request_owner_matches,
                lease_owner_matches,
            },
            PinRenewalToken {
                fence: current_fence,
                expires_at_ms: current_expires,
            },
            PinRenewalToken {
                fence: lease_fence,
                expires_at_ms: lease_expires,
            },
            now_ms,
            ttl_ms,
        )
        .map_err(|error| error.code);
        let exact_identity = volume_matches
            && version_matches
            && request_owner_matches
            && lease_owner_matches
            && current_fence == lease_fence
            && current_expires == lease_expires;
        let wide_expiry = i128::from(now_ms) + i128::from(ttl_ms);
        let expected = if !exact_identity || current_expires <= now_ms {
            Err(ErrorCode::Estale)
        } else if wide_expiry > i128::from(i64::MAX) {
            Err(ErrorCode::Eoverflow)
        } else {
            Ok(wide_expiry as i64)
        };

        kani::cover!(exact_identity && current_expires > now_ms && observed.is_ok());
        kani::cover!(exact_identity && current_expires == now_ms && observed.is_err());
        kani::cover!(
            exact_identity && current_expires < now_ms && observed == Err(ErrorCode::Estale)
        );
        kani::cover!(
            !volume_matches
                && version_matches
                && request_owner_matches
                && lease_owner_matches
                && current_fence == lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && !version_matches
                && request_owner_matches
                && lease_owner_matches
                && current_fence == lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && version_matches
                && !request_owner_matches
                && lease_owner_matches
                && current_fence == lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && version_matches
                && request_owner_matches
                && lease_owner_matches
                && current_fence != lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && version_matches
                && request_owner_matches
                && !lease_owner_matches
                && current_fence == lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && version_matches
                && request_owner_matches
                && lease_owner_matches
                && current_fence == lease_fence
                && current_expires != lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            exact_identity
                && current_expires > now_ms
                && wide_expiry > i128::from(i64::MAX)
                && observed == Err(ErrorCode::Eoverflow)
        );
        kani::cover!(
            exact_identity && current_expires > now_ms && wide_expiry == i128::from(i64::MAX)
        );
        assert_eq!(observed, expected);
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn signed_writer_lease_boundaries_are_checked() {
        let seconds: u64 = kani::any();
        let nanos: u32 = kani::any();
        let fence: u64 = kani::any();
        let expiry: u64 = kani::any();
        kani::assume(nanos < 1_000_000_000);

        let exact_ms = u128::from(seconds) * 1_000 + u128::from(nanos / 1_000_000);
        let observed_ttl = ttl_ms(Duration::new(seconds, nanos)).map_err(|error| error.code);
        let expected_ttl = if exact_ms == 0 || exact_ms > i64::MAX as u128 {
            Err(ErrorCode::Einval)
        } else {
            Ok(exact_ms as i64)
        };
        assert_eq!(observed_ttl, expected_ttl);

        let lease = WriterLease {
            owner: "writer".to_owned(),
            fence,
            expires_at_ms: expiry,
        };
        let observed_token = lease_numbers(&lease).map_err(|error| error.code);
        let expected_token = if fence > i64::MAX as u64 || expiry > i64::MAX as u64 {
            Err(ErrorCode::Estale)
        } else {
            Ok((fence as i64, expiry as i64))
        };
        assert_eq!(observed_token, expected_token);

        kani::cover!(exact_ms == 0 && observed_ttl == Err(ErrorCode::Einval));
        kani::cover!(exact_ms == 1 && observed_ttl == Ok(1));
        kani::cover!(exact_ms == i64::MAX as u128 && observed_ttl == Ok(i64::MAX));
        kani::cover!(exact_ms > i64::MAX as u128 && observed_ttl == Err(ErrorCode::Einval));
        kani::cover!(
            fence <= i64::MAX as u64 && expiry <= i64::MAX as u64 && observed_token.is_ok()
        );
        kani::cover!(
            fence > i64::MAX as u64 && expiry <= i64::MAX as u64 && observed_token.is_err()
        );
        kani::cover!(
            fence <= i64::MAX as u64 && expiry > i64::MAX as u64 && observed_token.is_err()
        );
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::io::{BufRead, BufReader, Read};
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use std::thread::{self, JoinHandle};
    use std::time::{Duration, Instant};

    const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);

    pub(crate) struct PgliteServer {
        child: Child,
        connection_string: String,
        stdout_reader: Option<JoinHandle<()>>,
    }

    impl PgliteServer {
        pub(crate) fn start() -> Self {
            Self::start_with_max_connections(8)
        }

        pub(crate) fn start_with_max_connections(max_connections: usize) -> Self {
            let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/pglite/server.mjs");
            assert!(
                script.is_file(),
                "PGlite server helper is missing: {}",
                script.display()
            );
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            let mut child = Command::new("node")
                .arg(script)
                .env("PGLITE_PORT", port.to_string())
                .env("PGLITE_MAX_CONNECTIONS", max_connections.to_string())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap_or_else(|error| panic!("start node PGlite helper: {error}"));

            let stderr = child
                .stderr
                .take()
                .unwrap_or_else(|| panic!("PGlite helper did not expose a stderr pipe"));
            let stderr_reader = thread::spawn(move || {
                let mut stderr = stderr;
                let mut output = String::new();
                let _ = stderr.read_to_string(&mut output);
                output
            });

            let stdout = child
                .stdout
                .take()
                .unwrap_or_else(|| panic!("PGlite helper did not expose a readiness stdout pipe"));
            let (ready_tx, ready_rx) = mpsc::channel();
            let stdout_reader = thread::spawn(move || {
                let mut stdout = BufReader::new(stdout);
                let mut line = String::new();
                loop {
                    line.clear();
                    match stdout.read_line(&mut line) {
                        Ok(0) => {
                            let _ = ready_tx.send(Ok(None));
                            return;
                        }
                        Ok(_) => {
                            if ready_tx.send(Ok(Some(line.clone()))).is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            let _ = ready_tx.send(Err(error));
                            return;
                        }
                    }
                }
            });

            let deadline = Instant::now() + STARTUP_TIMEOUT;
            let mut endpoint = None;
            let mut startup_error = None;
            while endpoint.is_none() && startup_error.is_none() {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    startup_error = Some(format!(
                        "PGlite helper startup exceeded {STARTUP_TIMEOUT:?}"
                    ));
                    break;
                }
                match ready_rx.recv_timeout(remaining) {
                    Ok(Ok(Some(line))) => {
                        if let Some(value) = line.strip_prefix("PGLITE_READY ") {
                            endpoint = Some(value.trim().to_owned());
                        }
                    }
                    Ok(Ok(None)) => {
                        startup_error = Some("PGlite helper exited before PGLITE_READY".to_owned());
                    }
                    Ok(Err(error)) => {
                        startup_error = Some(format!("read PGlite helper readiness: {error}"));
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        startup_error = Some(format!(
                            "PGlite helper startup exceeded {STARTUP_TIMEOUT:?}"
                        ));
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        startup_error =
                            Some("PGlite helper readiness reader disconnected".to_owned());
                    }
                }
            }

            let endpoint = match (endpoint, startup_error) {
                (Some(endpoint), None) => endpoint,
                (_, Some(error)) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    let stderr = stderr_reader.join().unwrap_or_default();
                    if stderr.trim().is_empty() {
                        panic!("{error}");
                    }
                    panic!("{error}: {}", stderr.trim());
                }
                _ => unreachable!("PGlite startup ended without readiness or an error"),
            };
            let announced_port = match endpoint
                .strip_prefix("127.0.0.1:")
                .and_then(|value| value.parse::<u16>().ok())
            {
                Some(port) => port,
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_reader.join();
                    panic!("PGlite helper reported invalid readiness endpoint: {endpoint}");
                }
            };
            if announced_port != port {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_reader.join();
                panic!(
                    "PGlite helper readiness endpoint did not match the reserved port: {endpoint}"
                );
            }
            Self {
                child,
                connection_string: format!(
                    "postgresql://postgres:postgres@127.0.0.1:{port}/postgres?sslmode=disable"
                ),
                stdout_reader: Some(stdout_reader),
            }
        }

        pub(crate) fn connection_string(&self) -> &str {
            &self.connection_string
        }
    }

    impl Drop for PgliteServer {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(stdout_reader) = self.stdout_reader.take() {
                let _ = stdout_reader.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::PgliteServer;
    use super::*;
    use mount_rs_core::FsDriver;
    use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
    use mount_rs_core::storage::{
        BlockExtent, ConcurrentModeState, FileLayout, NodeData, NodeMetadata,
    };
    use mount_rs_core::versioning::BlockStoreId;
    use mount_rs_memfs::MemoryFs;
    use std::collections::BTreeMap;

    mod wire_pause {
        use std::io::{self, Read, Write};
        use std::net::{Shutdown, TcpListener, TcpStream};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::{Arc, mpsc};
        use std::thread::{self, JoinHandle};
        use std::time::Duration;

        pub(super) struct QueryPause {
            pub(super) connection_string: String,
            armed: Arc<AtomicBool>,
            capture_first: Arc<AtomicBool>,
            first_query: Option<mpsc::Receiver<String>>,
            paused: Option<mpsc::Receiver<()>>,
            release: mpsc::Sender<()>,
            worker: JoinHandle<io::Result<()>>,
        }

        impl QueryPause {
            pub(super) fn new(server_url: &str, sql_fragment: &'static str) -> Self {
                let server_port: u16 = server_url
                    .split('@')
                    .nth(1)
                    .unwrap()
                    .split('/')
                    .next()
                    .unwrap()
                    .rsplit(':')
                    .next()
                    .unwrap()
                    .parse()
                    .unwrap();
                let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
                let proxy_port = listener.local_addr().unwrap().port();
                let armed = Arc::new(AtomicBool::new(false));
                let capture_first = Arc::new(AtomicBool::new(false));
                let pending = Arc::new(AtomicBool::new(false));
                let (first_query_tx, first_query) = mpsc::channel();
                let (paused_tx, paused) = mpsc::channel();
                let (release, release_rx) = mpsc::channel();
                let worker = thread::spawn({
                    let armed = armed.clone();
                    let capture_first = capture_first.clone();
                    move || {
                        let (frontend, _) = listener.accept()?;
                        let backend = TcpStream::connect(("127.0.0.1", server_port))?;
                        let forward = thread::spawn({
                            let pending = pending.clone();
                            let armed = armed.clone();
                            let capture_first = capture_first.clone();
                            let frontend = frontend.try_clone()?;
                            let backend = backend.try_clone()?;
                            move || {
                                let result = forward_frontend(
                                    frontend,
                                    backend.try_clone()?,
                                    armed,
                                    capture_first,
                                    first_query_tx,
                                    pending,
                                    sql_fragment,
                                );
                                backend.shutdown(Shutdown::Write)?;
                                result
                            }
                        });
                        let result =
                            forward_backend(backend, frontend, pending, paused_tx, release_rx);
                        forward.join().unwrap()?;
                        result
                    }
                });
                Self {
                    connection_string: format!(
                        "postgresql://postgres:postgres@127.0.0.1:{proxy_port}/postgres?sslmode=disable"
                    ),
                    armed,
                    capture_first,
                    first_query: Some(first_query),
                    paused: Some(paused),
                    release,
                    worker,
                }
            }

            pub(super) fn arm(&self) {
                self.capture_first.store(true, Ordering::SeqCst);
                self.armed.store(true, Ordering::SeqCst);
            }

            pub(super) async fn first_query(&mut self) -> String {
                let first_query = self.first_query.take().unwrap();
                tokio::task::spawn_blocking(move || {
                    first_query
                        .recv_timeout(Duration::from_secs(10))
                        .expect("provider did not send a query after the wire trace was armed")
                })
                .await
                .unwrap()
            }

            pub(super) async fn wait_until_paused(&mut self) {
                let paused = self.paused.take().unwrap();
                tokio::task::spawn_blocking(move || {
                    paused
                        .recv_timeout(Duration::from_secs(10))
                        .expect("reader query did not reach the wire pause");
                })
                .await
                .unwrap();
            }

            pub(super) fn release(&self) {
                self.release.send(()).unwrap();
            }

            pub(super) fn finish(self) {
                self.worker.join().unwrap().unwrap();
            }
        }

        fn read_frame(input: &mut TcpStream) -> io::Result<Option<(u8, Vec<u8>)>> {
            let mut tag = [0];
            match input.read_exact(&mut tag) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
                Err(error) => return Err(error),
            }
            let mut length = [0; 4];
            input.read_exact(&mut length)?;
            let length = u32::from_be_bytes(length) as usize;
            if !(4..=1024 * 1024).contains(&length) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "wire frame length",
                ));
            }
            let mut frame = Vec::with_capacity(1 + length);
            frame.push(tag[0]);
            frame.extend_from_slice(&(length as u32).to_be_bytes());
            frame.resize(1 + length, 0);
            input.read_exact(&mut frame[5..])?;
            Ok(Some((tag[0], frame)))
        }

        fn forward_frontend(
            mut frontend: TcpStream,
            mut backend: TcpStream,
            armed: Arc<AtomicBool>,
            capture_first: Arc<AtomicBool>,
            first_query: mpsc::Sender<String>,
            pending: Arc<AtomicBool>,
            sql_fragment: &str,
        ) -> io::Result<()> {
            // Startup is the only frontend frame without a type byte.
            let mut length = [0; 4];
            frontend.read_exact(&mut length)?;
            let length = u32::from_be_bytes(length) as usize;
            if !(4..=1024 * 1024).contains(&length) {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "startup length"));
            }
            let mut startup = vec![0; length];
            startup[..4].copy_from_slice(&(length as u32).to_be_bytes());
            frontend.read_exact(&mut startup[4..])?;
            backend.write_all(&startup)?;
            while let Some((tag, frame)) = read_frame(&mut frontend)? {
                let sql = match tag {
                    b'P' => frame[5..].split(|byte| *byte == 0).nth(1),
                    b'Q' => frame[5..].split(|byte| *byte == 0).next(),
                    _ => None,
                };
                if let Some(sql) = sql {
                    if sql.windows(6).any(|window| window == b"SELECT")
                        && capture_first.swap(false, Ordering::SeqCst)
                    {
                        first_query
                            .send(String::from_utf8_lossy(sql).into_owned())
                            .map_err(io::Error::other)?;
                    }
                    if sql
                        .windows(sql_fragment.len())
                        .any(|window| window == sql_fragment.as_bytes())
                        && armed.swap(false, Ordering::SeqCst)
                    {
                        pending.store(true, Ordering::SeqCst);
                    }
                }
                backend.write_all(&frame)?;
            }
            Ok(())
        }

        fn forward_backend(
            mut backend: TcpStream,
            mut frontend: TcpStream,
            pending: Arc<AtomicBool>,
            paused: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        ) -> io::Result<()> {
            let mut held = Vec::new();
            while let Some((tag, frame)) = read_frame(&mut backend)? {
                if pending.load(Ordering::SeqCst) {
                    held.extend_from_slice(&frame);
                    if tag == b'Z' {
                        paused.send(()).map_err(io::Error::other)?;
                        release
                            .recv_timeout(Duration::from_secs(10))
                            .map_err(io::Error::other)?;
                        frontend.write_all(&held)?;
                        held.clear();
                        pending.store(false, Ordering::SeqCst);
                    }
                } else {
                    frontend.write_all(&frame)?;
                }
            }
            Ok(())
        }
    }

    fn version_publication(
        revision: u64,
        parent: Option<VersionId>,
        operation: &str,
        namespace: Namespace,
    ) -> VersionPublication {
        VersionPublication {
            expected_revision: revision,
            expected_parent: parent,
            operation_id: PublicationId::new(operation).unwrap(),
            namespace,
            block_store_id: BlockStoreId::new("pglite-race-test").unwrap(),
            kind: VersionKind::Snapshot,
            restored_from: None,
            forked_from: None,
            durable: false,
        }
    }

    async fn read_lease_fixture(
        url: &str,
        volume_key: &str,
    ) -> (PgliteMetadataStore, PgliteBlockStore, ReadLease) {
        let options = PgliteStorageOptions::new(volume_key);
        let metadata = PgliteMetadataStore::connect_with_options(url, options.clone())
            .await
            .unwrap();
        let blocks = PgliteBlockStore::connect_with_options(url, options)
            .await
            .unwrap();
        let block = blocks.put(b"abc").await.unwrap();
        let writer = metadata
            .acquire_writer("forged-token-writer", Duration::from_secs(60))
            .await
            .unwrap();
        let version = metadata
            .publish_version(
                &writer,
                version_publication(0, None, "forged-token-version", namespace(block).await),
            )
            .await
            .unwrap();
        metadata.release_writer(&writer).await.unwrap();
        let pin = metadata
            .open_view_pin(
                &version.id,
                ReadLeaseRequest {
                    owner: "forged-token-reader".to_owned(),
                    ttl: Duration::from_secs(60),
                },
            )
            .await
            .unwrap();
        (metadata, blocks, pin)
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn version_one_schema_stays_readable_until_publication_and_rejects_future_schema() {
        let server = PgliteServer::start();
        let url = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("version-kind-schema-upgrade");
            let store = PgliteMetadataStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            let namespace = namespace(block).await;
            let lease = store
                .acquire_writer("schema-upgrade", Duration::from_secs(60))
                .await
                .unwrap();
            let first = store
                .publish_version(
                    &lease,
                    version_publication(0, None, "before-upgrade", namespace.clone()),
                )
                .await
                .unwrap();
            store.release_writer(&lease).await.unwrap();
            let volume = store.volume_id();
            {
                let client = store.0.lock_client().await.unwrap();
                let schema = client
                    .as_ref()
                    .unwrap()
                    .query_typed_one(
                        "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=$1",
                        &[(&VERSION_SCHEMA_NAME, Type::TEXT)],
                    )
                    .await
                    .unwrap();
                assert_eq!(schema.get::<_, i64>(0), VERSION_SCHEMA_BASE_VERSION);
            }
            store.close().await.unwrap();

            let reopened = PgliteMetadataStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            assert_eq!(reopened.volume_id(), volume);
            assert_eq!(reopened.version_head().await.unwrap().unwrap().version, first.id);
            assert_eq!(reopened.list_versions().await.unwrap().len(), 1);
            {
                let client = reopened.0.lock_client().await.unwrap();
                let schema = client
                    .as_ref()
                    .unwrap()
                    .query_typed_one(
                        "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=$1",
                        &[(&VERSION_SCHEMA_NAME, Type::TEXT)],
                    )
                    .await
                    .unwrap();
                assert_eq!(schema.get::<_, i64>(0), VERSION_SCHEMA_BASE_VERSION);
            }
            let lease = reopened
                .acquire_writer("schema-upgrade", Duration::from_secs(60))
                .await
                .unwrap();
            let mut next = version_publication(
                1,
                Some(first.id),
                "after-upgrade",
                namespace,
            );
            next.kind = VersionKind::NamespacePublication;
            let second = reopened.publish_version(&lease, next).await.unwrap();
            reopened.release_writer(&lease).await.unwrap();
            {
                let client = reopened.0.lock_client().await.unwrap();
                let client = client.as_ref().unwrap();
                let schema = client
                    .query_typed_one(
                        "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=$1",
                        &[(&VERSION_SCHEMA_NAME, Type::TEXT)],
                    )
                    .await
                    .unwrap();
                assert_eq!(schema.get::<_, i64>(0), VERSION_SCHEMA_VERSION);
                let kind = client
                    .query_typed_one(
                        "SELECT kind FROM mount_rs_versions WHERE volume_key=$1 AND id=$2",
                        &[
                            (&options.volume_key, Type::TEXT),
                            (&second.id.encode(), Type::TEXT),
                        ],
                    )
                    .await
                    .unwrap();
                assert_eq!(kind.get::<_, String>(0), "namespace-publication");
                client
                    .execute_typed(
                        "UPDATE mount_rs_schema_versions SET schema_version=99 WHERE schema_name=$1",
                        &[(&VERSION_SCHEMA_NAME, Type::TEXT)],
                    )
                    .await
                    .unwrap();
            }
            let error = match PgliteMetadataStore::connect_with_options(url, options).await {
                Ok(_) => panic!("future version schema must be rejected"),
                Err(error) => error,
            };
            assert!(error.is(ErrorCode::Enotsup));
            {
                let client = reopened.0.lock_client().await.unwrap();
                let schema = client
                    .as_ref()
                    .unwrap()
                    .query_typed_one(
                        "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=$1",
                        &[(&VERSION_SCHEMA_NAME, Type::TEXT)],
                    )
                    .await
                    .unwrap();
                assert_eq!(schema.get::<_, i64>(0), 99);
            }
            reopened.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    async fn published_version_fixture(
        url: &str,
        volume_key: &str,
    ) -> (
        PgliteMetadataStore,
        PgliteBlockStore,
        WriterLease,
        VersionInfo,
        Namespace,
    ) {
        let options = PgliteStorageOptions::new(volume_key);
        let metadata = PgliteMetadataStore::connect_with_options(url, options.clone())
            .await
            .unwrap();
        let blocks = PgliteBlockStore::connect_with_options(url, options)
            .await
            .unwrap();
        let block = blocks.put(b"abc").await.unwrap();
        let namespace = namespace(block).await;
        let writer = metadata
            .acquire_writer("corrupt-row-writer", Duration::from_secs(60))
            .await
            .unwrap();
        let version = metadata
            .publish_version(
                &writer,
                version_publication(0, None, "corrupt-row-first", namespace.clone()),
            )
            .await
            .unwrap();
        (metadata, blocks, writer, version, namespace)
    }

    async fn corrupt_version_row(metadata: &PgliteMetadataStore, id: &VersionId, sql: &str) {
        let client = metadata.0.lock_client().await.unwrap();
        let encoded = id.encode();
        assert_eq!(
            client
                .as_ref()
                .unwrap()
                .execute_typed(
                    sql,
                    &[(&metadata.0.volume_key, Type::TEXT), (&encoded, Type::TEXT),],
                )
                .await
                .unwrap(),
            1
        );
    }

    #[test]
    fn block_id_matches_postgresql_md5_hex_contract() {
        assert_eq!(block_id(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(block_id(b"abc"), "0e04049912772820000827e2da893ae2");
        assert_eq!(block_id(&[0, 0xff]), "74a76031f936ac0e7b0d1b176e3ee7d7");
    }

    async fn cancel_close_while_client_is_held(database: &Database) {
        let client_guard = database.client.lock().await;
        let canceled = tokio::spawn({
            let database = database.clone();
            async move { database.close().await }
        });
        tokio::task::yield_now().await;
        assert!(database.close_gate.is_closing());
        canceled.abort();
        assert!(canceled.await.unwrap_err().is_cancelled());

        let second = tokio::spawn({
            let database = database.clone();
            async move { database.close().await }
        });
        tokio::task::yield_now().await;
        assert!(!second.is_finished());
        drop(client_guard);
        second.await.unwrap().unwrap();
    }

    async fn namespace(block: BlockId) -> Namespace {
        let stats = MemoryFs::empty().stat("/").await.unwrap();
        let root = stats.ino;
        let root_stats = mount_rs_core::Stats { nlink: 2, ..stats };
        let file = NodeMetadata {
            stats: mount_rs_core::Stats {
                ino: root + 1,
                mode: mount_rs_core::S_IFREG | 0o644,
                nlink: 1,
                size: 3,
                blocks: 1,
                ..root_stats.clone()
            },
            data: NodeData::File(FileLayout {
                chunker: FixedSizeChunker::new(4096).unwrap().config(),
                extents: vec![BlockExtent {
                    file_offset: 0,
                    block,
                    block_offset: 0,
                    length: 3,
                }],
            }),
        };
        let namespace = Namespace {
            format_version: 1,
            root,
            next_inode: root + 2,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: FixedSizeChunker::new(4096).unwrap().config(),
            nodes: BTreeMap::from([
                (
                    root,
                    NodeMetadata {
                        stats: root_stats,
                        data: NodeData::Directory {
                            entries: vec![mount_rs_core::storage::DirectoryEntry {
                                name: "file".to_owned(),
                                inode: root + 1,
                            }],
                        },
                    },
                ),
                (root + 1, file),
            ]),
        };
        namespace.validate().unwrap();
        namespace
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn inode_guards_cas_fold_and_reopen() {
        let configured_url = std::env::var("PGLITE_DATABASE_URL").ok();
        let server = configured_url.is_none().then(PgliteServer::start);
        let connection_string = configured_url
            .as_deref()
            .unwrap_or_else(|| server.as_ref().unwrap().connection_string());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metadata =
                PgliteMetadataStore::connect_with_key(connection_string, "inode-guards")
                    .await
                    .unwrap();
            let blocks =
                PgliteBlockStore::connect_with_key(connection_string, "inode-guards")
                    .await
                    .unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            metadata
                .prepare_bound_concurrent_mode(backing)
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            let ns = namespace(block).await;
            let inode = ns.root + 1;
            metadata
                .publish_bound_if_revision(backing, 0, ns.clone())
                .await
                .unwrap();
            assert!(metadata.prepare_inode_mode(backing, 0).await.is_err());
            metadata.prepare_inode_mode(backing, 1).await.unwrap();
            assert!(metadata.load().await.is_err());
            assert!(metadata.load_if_changed(1).await.is_err());
            assert!(metadata.concurrent_mode_state().await.is_err());
            assert!(
                metadata
                    .publish_bound_if_revision(backing, 1, ns.clone())
                    .await
                    .is_err()
            );
            // This is the exact conditional projection used by pre-MRC4 readers.
            // Enrollment must force a changed payload which their decoder rejects.
            {
                let client = metadata.0.lock_client().await.unwrap();
                let old_revision: i64 = 1;
                let row = client.as_ref().unwrap().query_one(
                    "SELECT revision, CASE WHEN revision > 0 AND revision = $2 THEN NULL ELSE namespace END FROM mount_rs_metadata WHERE volume_key = $1",
                    &[&metadata.0.volume_key, &old_revision]
                ).await.unwrap();
                assert_eq!(row.get::<_, i64>(0), 2);
                let payload = row.get::<_, Option<String>>(1).unwrap();
                assert!(serde_json::from_str::<Namespace>(&payload).is_err());
            }
            let initial = metadata.load_inode_snapshot(backing).await.unwrap();
            assert_eq!(initial.structural_generation, 2);
            assert_eq!(
                initial.inode_revisions,
                BTreeMap::from([(ns.root, 0), (inode, 0)])
            );
            let file = metadata.load_inode(backing, inode).await.unwrap();
            assert!(
                metadata
                    .load_inode_if_changed(backing, inode, Some(file.version))
                    .await
                    .unwrap()
                    .is_none()
            );
            let mut changed = file.node.clone();
            changed.stats.mtime_ms += 1;
            let version = metadata
                .publish_inode_if_version(backing, inode, file.version, changed.clone())
                .await
                .unwrap();
            assert_eq!(version.inode_revision, 1);
            assert_eq!(
                metadata
                    .publish_inode_if_version(backing, inode, file.version, changed.clone())
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Eagain
            );
            assert_eq!(
                metadata
                    .publish_structure_if_versions(backing, 2, &initial.inode_revisions, ns.clone())
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Eagain
            );
            let latest = metadata.load_inode_snapshot(backing).await.unwrap();
            assert_eq!(latest.namespace.nodes[&inode], changed);
            assert_eq!(
                metadata
                    .publish_structure_if_versions(
                        backing,
                        2,
                        &BTreeMap::from([(inode, 1)]),
                        latest.namespace.clone()
                    )
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Eagain
            );
            let generation = metadata
                .publish_structure_if_versions(
                    backing,
                    2,
                    &latest.inode_revisions,
                    latest.namespace,
                )
                .await
                .unwrap();
            assert_eq!(generation, 3);
            assert_eq!(
                metadata
                    .publish_inode_if_version(backing, inode, version, changed.clone())
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Eagain
            );
            metadata.close().await.unwrap();
            let reopened =
                PgliteMetadataStore::connect_with_key(connection_string, "inode-guards")
                    .await
                    .unwrap();
            assert_eq!(
                reopened
                    .inode_mode_state()
                    .await
                    .unwrap()
                    .unwrap()
                    .structural_generation,
                3
            );
            let file = reopened.load_inode(backing, inode).await.unwrap();
            assert_eq!(
                file.version,
                InodeVersion {
                    structural_generation: 3,
                    inode_revision: 0
                }
            );
            assert_eq!(file.node, changed);
            let root = ns.root;
            assert!(
                reopened
                    .publish_inode_if_version(backing, root, file.version, ns.nodes[&root].clone())
                    .await
                    .is_err()
            );
            let mut malformed_kind = changed.clone();
            malformed_kind.stats.mode = mount_rs_core::S_IFDIR | 0o644;
            let mut malformed_extent = changed.clone();
            if let NodeData::File(layout) = &mut malformed_extent.data {
                layout.extents[0].length = changed.stats.size + 1;
            }
            for malformed in [malformed_kind, malformed_extent] {
                let json = serde_json::to_string(&malformed).unwrap();
                {
                    let client = reopened.0.lock_client().await.unwrap();
                    client.as_ref().unwrap().execute(
                        "UPDATE mount_rs_inode_guards SET revision=1,node=$3 WHERE volume_key=$1 AND inode=$2",
                        &[&reopened.0.volume_key,&inode_signed(inode).unwrap(),&json]
                    ).await.unwrap();
                }
                assert!(reopened.load_inode(backing, inode).await.is_err());
                assert!(reopened.load_inode_if_changed(backing, inode, Some(file.version)).await.is_err());
                assert!(reopened.load_inode_snapshot(backing).await.is_err());
                let malformed_version = InodeVersion { structural_generation: 3, inode_revision: 1 };
                assert!(reopened.publish_inode_if_version(backing, inode, malformed_version, changed.clone()).await.is_err());
                assert!(reopened.publish_structure_if_versions(backing, 3, &BTreeMap::from([(ns.root,0),(inode,1)]), ns.clone()).await.is_err());
            }
            reopened.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn conditional_load_omits_only_fresh_nonzero_matches() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metadata = PgliteMetadataStore::connect_with_key(
                server.connection_string(),
                "conditional-load",
            )
            .await
            .unwrap();
            let blocks =
                PgliteBlockStore::connect_with_key(server.connection_string(), "conditional-load")
                    .await
                    .unwrap();
            assert_eq!(
                metadata.load_if_changed(0).await.unwrap().unwrap().revision,
                0
            );
            let lease = metadata
                .acquire_writer("conditional-reader", Duration::from_secs(60))
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            let payload = namespace(block).await;
            metadata.publish(0, &lease, payload.clone()).await.unwrap();
            assert!(metadata.load_if_changed(1).await.unwrap().is_none());
            assert_eq!(
                serde_json::to_string(
                    &metadata
                        .load_if_changed(0)
                        .await
                        .unwrap()
                        .unwrap()
                        .namespace
                )
                .unwrap(),
                serde_json::to_string(&Some(payload.clone())).unwrap(),
            );
            assert_eq!(
                metadata
                    .load_if_changed(u64::MAX)
                    .await
                    .unwrap()
                    .unwrap()
                    .revision,
                1
            );
            metadata.publish(1, &lease, payload).await.unwrap();
            assert_eq!(
                metadata.load_if_changed(1).await.unwrap().unwrap().revision,
                2
            );
            {
                let client = metadata.0.lock_client().await.unwrap();
                client.as_ref().unwrap().execute_typed(
                    "UPDATE mount_rs_metadata SET namespace='malformed-json' WHERE volume_key=$1",
                    &[(&metadata.0.volume_key, Type::TEXT)],
                ).await.unwrap();
            }
            // An unchanged revision is not an audit of out-of-band payload edits.
            assert!(metadata.load_if_changed(2).await.unwrap().is_none());
            assert!(metadata.load().await.is_err());
            assert!(metadata.load_if_changed(1).await.is_err());
            {
                let client = metadata.0.lock_client().await.unwrap();
                client.as_ref().unwrap().batch_execute(
                    "ALTER TABLE mount_rs_metadata DROP CONSTRAINT mount_rs_metadata_revision_check;
                     UPDATE mount_rs_metadata SET revision=-1;",
                ).await.unwrap();
            }
            assert!(metadata.load_if_changed(u64::MAX).await.is_err());
            {
                let client = metadata.0.lock_client().await.unwrap();
                client
                    .as_ref()
                    .unwrap()
                    .execute_typed(
                        "DELETE FROM mount_rs_metadata WHERE volume_key=$1",
                        &[(&metadata.0.volume_key, Type::TEXT)],
                    )
                    .await
                    .unwrap();
            }
            assert!(metadata.load_if_changed(2).await.is_err());
            metadata.close().await.unwrap();
            assert!(metadata.load_if_changed(2).await.is_err());
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn conditional_load_revision_and_payload_share_one_committed_snapshot() {
        let server = PgliteServer::start();
        let url = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("conditional-load-snapshot");
            let writer = PgliteMetadataStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            let first = namespace(block.clone()).await;
            let mut second = namespace(block).await;
            second.umask = 0o077;
            let lease = writer
                .acquire_writer("snapshot-writer", Duration::from_secs(60))
                .await
                .unwrap();
            writer.publish(0, &lease, first.clone()).await.unwrap();
            let mut proxy = wire_pause::QueryPause::new(url, "mount_rs_metadata");
            let reader =
                PgliteMetadataStore::connect_with_options(&proxy.connection_string, options)
                    .await
                    .unwrap();
            proxy.arm();
            let reading = tokio::spawn(async move {
                let loaded = reader.load_if_changed(0).await;
                reader.close().await.unwrap();
                loaded
            });
            proxy.wait_until_paused().await;
            writer.publish(1, &lease, second).await.unwrap();
            proxy.release();
            let observed = reading.await.unwrap().unwrap().unwrap();
            proxy.finish();
            assert_eq!(observed.revision, 1);
            assert_eq!(observed.namespace.unwrap().umask, first.umask);
            let current = writer.load_if_changed(1).await.unwrap().unwrap();
            assert_eq!(current.revision, 2);
            assert_eq!(current.namespace.unwrap().umask, 0o077);
            writer.release_writer(&lease).await.unwrap();
            writer.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    // Only the offline migration tests create an MRC1 volume. Normal
    // concurrent clients prepare an MRC2 volume with a block authority ID.
    async fn seed_test_owned_mrc1_volume(metadata: &PgliteMetadataStore) {
        let client = metadata.0.lock_client().await.unwrap();
        let changed = client
            .as_ref()
            .unwrap()
            .execute_typed(
                "UPDATE mount_rs_metadata SET write_mode=$2, fence=$3
                 WHERE volume_key=$1 AND write_mode IS NULL AND backing_id IS NULL
                   AND revision=0 AND namespace IS NULL
                   AND owner IS NULL AND fence=0 AND expires=0",
                &[
                    (&metadata.0.volume_key, Type::TEXT),
                    (&CONCURRENT_WRITE_MODE, Type::TEXT),
                    (&CONCURRENT_FENCE_SENTINEL, Type::INT8),
                ],
            )
            .await
            .unwrap();
        assert_eq!(changed, 1, "test-owned MRC1 fixture must start fresh");
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn version_head_revision_and_id_share_one_committed_snapshot() {
        let server = PgliteServer::start();
        let url = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("version-head-snapshot");
            let writer = PgliteMetadataStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            let namespace = namespace(block).await;
            let lease = writer
                .acquire_writer("snapshot-writer", Duration::from_secs(60))
                .await
                .unwrap();
            let first = writer
                .publish_version(
                    &lease,
                    version_publication(0, None, "snapshot-first", namespace.clone()),
                )
                .await
                .unwrap();

            let mut proxy = wire_pause::QueryPause::new(url, "mount_rs_metadata");
            let reader =
                PgliteMetadataStore::connect_with_options(&proxy.connection_string, options)
                    .await
                    .unwrap();
            proxy.arm();
            let reading = tokio::spawn(async move {
                let head = reader.version_head().await;
                reader.close().await.unwrap();
                head
            });
            proxy.wait_until_paused().await;
            let second = writer
                .publish_version(
                    &lease,
                    version_publication(1, Some(first.id.clone()), "snapshot-second", namespace),
                )
                .await
                .unwrap();
            proxy.release();
            let observed = reading.await.unwrap().unwrap().unwrap();
            proxy.finish();
            assert_eq!(
                observed,
                VersionHead {
                    version: first.id,
                    revision: 1,
                },
                "the first statement completed before the second publication committed"
            );
            assert_eq!(
                writer.version_head().await.unwrap().unwrap(),
                VersionHead {
                    version: second.id,
                    revision: 2,
                }
            );
            writer.release_writer(&lease).await.unwrap();
            writer.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    /// One historical version and one pin exercise both serialized lock
    /// orders: opening before deletion, then deletion before another opening.
    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn opening_a_pin_serializes_with_deleting_its_version() {
        let server = PgliteServer::start();
        let url = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("pin-delete-race");
            let writer = PgliteMetadataStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            let namespace = namespace(block).await;
            let lease = writer
                .acquire_writer("pin-delete-writer", Duration::from_secs(60))
                .await
                .unwrap();
            let first = writer
                .publish_version(
                    &lease,
                    version_publication(0, None, "pin-delete-first", namespace.clone()),
                )
                .await
                .unwrap();
            writer
                .publish_version(
                    &lease,
                    version_publication(1, Some(first.id.clone()), "pin-delete-second", namespace),
                )
                .await
                .unwrap();

            let mut proxy = wire_pause::QueryPause::new(url, "mount_rs_versions");
            let reader =
                PgliteMetadataStore::connect_with_options(&proxy.connection_string, options)
                    .await
                    .unwrap();
            proxy.arm();
            let opening = tokio::spawn({
                let reader = reader.clone();
                let id = first.id.clone();
                async move {
                    reader
                        .open_view_pin(
                            &id,
                            ReadLeaseRequest {
                                owner: "pin-delete-reader".to_owned(),
                                ttl: Duration::from_secs(60),
                            },
                        )
                        .await
                }
            });
            proxy.wait_until_paused().await;
            let first_query = proxy.first_query().await;
            let mut deletion = tokio::spawn({
                let writer = writer.clone();
                let lease = lease.clone();
                let id = first.id.clone();
                async move { writer.delete_version(&lease, &id).await }
            });
            let early_delete = tokio::time::timeout(Duration::from_secs(2), &mut deletion).await;
            let deleted_early = early_delete.is_ok();
            proxy.release();
            let pin = opening.await.unwrap().unwrap();
            let deletion_result = match early_delete {
                Ok(result) => result.unwrap(),
                Err(_) => deletion.await.unwrap(),
            };
            assert!(
                !deleted_early,
                "deletion completed while the pin opener was still in its transaction"
            );
            assert!(deletion_result.unwrap_err().is(ErrorCode::Ebusy));
            assert_eq!(writer.load_version(&first.id).await.unwrap().id, first.id);
            reader.close_view_pin(&pin).await.unwrap();
            writer.delete_version(&lease, &first.id).await.unwrap();
            assert!(
                writer
                    .load_version(&first.id)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Enoent)
            );
            assert!(
                reader
                    .open_view_pin(
                        &first.id,
                        ReadLeaseRequest {
                            owner: "pin-delete-reader".to_owned(),
                            ttl: Duration::from_secs(60),
                        },
                    )
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Enoent)
            );

            reader.close().await.unwrap();
            proxy.finish();
            assert!(
                first_query.contains("mount_rs_metadata") && first_query.contains("FOR UPDATE"),
                "pin opener checked version existence before locking its volume: {first_query:?}"
            );
            writer.release_writer(&lease).await.unwrap();
            writer.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn renewing_a_pin_locks_the_volume_before_reading_the_pin() {
        let server = PgliteServer::start();
        let url = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("renew-pin-lock");
            let writer = PgliteMetadataStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(url, options.clone())
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            let lease = writer
                .acquire_writer("renew-pin-writer", Duration::from_secs(60))
                .await
                .unwrap();
            let version = writer
                .publish_version(
                    &lease,
                    version_publication(0, None, "renew-pin-version", namespace(block).await),
                )
                .await
                .unwrap();
            let pin = writer
                .open_view_pin(
                    &version.id,
                    ReadLeaseRequest {
                        owner: "renew-pin-reader".to_owned(),
                        ttl: Duration::from_secs(60),
                    },
                )
                .await
                .unwrap();

            let mut proxy = wire_pause::QueryPause::new(url, "mount_rs_version_pins");
            let renewer =
                PgliteMetadataStore::connect_with_options(&proxy.connection_string, options)
                    .await
                    .unwrap();
            proxy.arm();
            let renewing = tokio::spawn({
                let renewer = renewer.clone();
                let pin = pin.clone();
                async move {
                    renewer
                        .renew_view_pin(
                            &pin,
                            ReadLeaseRequest {
                                owner: "renew-pin-reader".to_owned(),
                                ttl: Duration::from_secs(120),
                            },
                        )
                        .await
                }
            });
            proxy.wait_until_paused().await;
            let first_query = proxy.first_query().await;
            proxy.release();
            let renewed = renewing.await.unwrap().unwrap();
            renewer.close_view_pin(&renewed).await.unwrap();
            renewer.close().await.unwrap();
            proxy.finish();
            assert!(
                first_query.contains("mount_rs_metadata") && first_query.contains("FOR UPDATE"),
                "pin renewer read the pin before locking its volume: {first_query:?}"
            );
            writer.release_writer(&lease).await.unwrap();
            writer.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn forged_volume_cannot_renew_a_live_pin() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (metadata, blocks, pin) =
                read_lease_fixture(server.connection_string(), "forged-renew-volume").await;
            let mut forged = pin.clone();
            forged.volume = VolumeId::new("wrong-renew-volume").unwrap();
            let request = ReadLeaseRequest {
                owner: "forged-token-reader".to_owned(),
                ttl: Duration::from_secs(120),
            };
            assert!(
                metadata
                    .renew_view_pin(&forged, request.clone())
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            let renewed = metadata.renew_view_pin(&pin, request).await.unwrap();
            metadata.close_view_pin(&renewed).await.unwrap();
            metadata.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn forged_volume_cannot_close_a_live_pin_and_missing_close_is_idempotent() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (metadata, blocks, pin) =
                read_lease_fixture(server.connection_string(), "forged-close-volume").await;
            let mut forged = pin.clone();
            forged.volume = VolumeId::new("wrong-close-volume").unwrap();
            assert!(
                metadata
                    .close_view_pin(&forged)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            let renewed = metadata
                .renew_view_pin(
                    &pin,
                    ReadLeaseRequest {
                        owner: "forged-token-reader".to_owned(),
                        ttl: Duration::from_secs(120),
                    },
                )
                .await
                .unwrap();
            metadata.close_view_pin(&renewed).await.unwrap();
            metadata.close_view_pin(&forged).await.unwrap();
            metadata.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn version_head_rejects_an_unloadable_record_in_one_snapshot() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let corruptions = [
                (
                    "sequence",
                    "UPDATE mount_rs_versions SET sequence=sequence+1 WHERE volume_key=$1 AND id=$2",
                ),
                (
                    "namespace",
                    "UPDATE mount_rs_versions SET namespace='not-json' WHERE volume_key=$1 AND id=$2",
                ),
                (
                    "kind",
                    "UPDATE mount_rs_versions SET kind='unknown' WHERE volume_key=$1 AND id=$2",
                ),
                (
                    "block-store",
                    "UPDATE mount_rs_versions SET block_store_id='' WHERE volume_key=$1 AND id=$2",
                ),
                (
                    "lineage",
                    "UPDATE mount_rs_versions SET parent_id='wrong-volume:1' WHERE volume_key=$1 AND id=$2",
                ),
            ];
            for (case, sql) in corruptions {
                let volume_key = format!("unloadable-head-{case}");
                let (metadata, blocks, writer, version, _) =
                    published_version_fixture(server.connection_string(), &volume_key).await;
                corrupt_version_row(&metadata, &version.id, sql).await;
                assert!(metadata.load_version(&version.id).await.is_err());
                let error = match metadata.version_head().await {
                    Ok(head) => panic!("{case} head remained visible: {head:?}"),
                    Err(error) => error,
                };
                assert!(error.is(ErrorCode::Eio), "{case} head error: {error:?}");
                metadata.release_writer(&writer).await.unwrap();
                metadata.close().await.unwrap();
                blocks.close().await.unwrap();
            }
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn publication_rejects_an_unloadable_current_parent() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (metadata, blocks, writer, version, namespace) =
                published_version_fixture(server.connection_string(), "corrupt-current-parent")
                    .await;
            corrupt_version_row(
                &metadata,
                &version.id,
                "UPDATE mount_rs_versions SET namespace='not-json' WHERE volume_key=$1 AND id=$2",
            )
            .await;
            let result = metadata
                .publish_version(
                    &writer,
                    version_publication(
                        1,
                        Some(version.id.clone()),
                        "corrupt-row-second",
                        namespace,
                    ),
                )
                .await;
            let error = match result {
                Ok(_) => panic!("published from an unreadable parent"),
                Err(error) => error,
            };
            assert!(error.is(ErrorCode::Eio));
            assert_eq!(metadata.load().await.unwrap().revision, 1);
            metadata.release_writer(&writer).await.unwrap();
            metadata.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn reconnect_rejects_an_unloadable_current_head() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let volume_key = "corrupt-reconnect-head";
            let (metadata, blocks, writer, version, _) =
                published_version_fixture(server.connection_string(), volume_key).await;
            corrupt_version_row(
                &metadata,
                &version.id,
                "UPDATE mount_rs_versions SET namespace='not-json' WHERE volume_key=$1 AND id=$2",
            )
            .await;
            metadata.release_writer(&writer).await.unwrap();
            metadata.close().await.unwrap();
            let reopened =
                PgliteMetadataStore::connect_with_key(server.connection_string(), volume_key).await;
            let error = match reopened {
                Ok(_) => panic!("reconnected to an unloadable head"),
                Err(error) => error,
            };
            assert!(error.is(ErrorCode::Enotsup));
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn opening_a_pin_rejects_an_unloadable_historical_version() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (metadata, blocks, writer, first, namespace) =
                published_version_fixture(server.connection_string(), "corrupt-pin-target").await;
            metadata
                .publish_version(
                    &writer,
                    version_publication(1, Some(first.id.clone()), "corrupt-pin-second", namespace),
                )
                .await
                .unwrap();
            corrupt_version_row(
                &metadata,
                &first.id,
                "UPDATE mount_rs_versions SET namespace='not-json' WHERE volume_key=$1 AND id=$2",
            )
            .await;
            assert!(metadata.load_version(&first.id).await.is_err());
            let pin = metadata
                .open_view_pin(
                    &first.id,
                    ReadLeaseRequest {
                        owner: "corrupt-pin-reader".to_owned(),
                        ttl: Duration::from_secs(60),
                    },
                )
                .await;
            let error = match pin {
                Ok(_) => panic!("opened a pin for an unreadable historical version"),
                Err(error) => error,
            };
            assert!(error.is(ErrorCode::Eio));
            metadata.delete_version(&writer, &first.id).await.unwrap();
            metadata.release_writer(&writer).await.unwrap();
            metadata.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn delegated_grants_are_persisted_fenced_and_retry_safe() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metadata =
                PgliteMetadataStore::connect_with_key(server.connection_string(), "delegation")
                    .await
                    .unwrap();
            let blocks =
                PgliteBlockStore::connect_with_key(server.connection_string(), "delegation")
                    .await
                    .unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            let ns = namespace(blocks.put(b"abc").await.unwrap()).await;
            let lease = metadata
                .acquire_writer("initializer", Duration::from_secs(60))
                .await
                .unwrap();
            metadata.publish(0, &lease, ns.clone()).await.unwrap();
            assert!(
                metadata
                    .prepare_delegated_mode(backing, 1)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Ebusy)
            );
            metadata.release_writer(&lease).await.unwrap();
            metadata.prepare_delegated_mode(backing, 1).await.unwrap();
            let request = CheckoutRequest {
                backing,
                root: ns.root,
                owner: "session-one".into(),
            };
            let grant = metadata.checkout(&request).await.unwrap();
            assert_eq!(metadata.checkout(&request).await.unwrap(), grant);
            assert!(
                metadata
                    .acquire_writer("old", Duration::from_secs(60))
                    .await
                    .is_err()
            );
            assert!(
                metadata
                    .publish_bound_if_revision(backing, 1, ns.clone())
                    .await
                    .is_err()
            );
            let mut hostile = ns.clone();
            hostile.umask = 0;
            let publish = DelegatedPublish {
                backing,
                token: grant.token.clone(),
                expected_revision: 1,
            };
            assert!(
                metadata
                    .publish_delegated(&publish, hostile)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            assert_eq!(
                metadata
                    .publish_delegated(&publish, ns.clone())
                    .await
                    .unwrap(),
                2
            );
            let checkin = DelegatedCheckin {
                backing,
                token: grant.token.clone(),
                expected_revision: 2,
            };
            metadata.checkin(&checkin).await.unwrap();
            metadata.checkin(&checkin).await.unwrap();
            assert!(metadata.checkout(&request).await.is_err());
            let newer = metadata
                .checkout(&CheckoutRequest {
                    owner: "session-two".into(),
                    ..request
                })
                .await
                .unwrap();
            assert!(newer.token.fence > grant.token.fence);
            metadata
                .publish_delegated(
                    &DelegatedPublish {
                        backing,
                        token: newer.token.clone(),
                        expected_revision: 2,
                    },
                    ns.clone(),
                )
                .await
                .unwrap();
            metadata.checkin(&checkin).await.unwrap();
            assert_eq!(
                metadata.delegation_state().await.unwrap().unwrap().grants[&ns.root].token,
                newer.token
            );

            assert!(
                metadata
                    .recover(&DelegatedRecovery {
                        backing,
                        root: ns.root,
                        expected_fence: grant.token.fence
                    })
                    .await
                    .is_err()
            );
            metadata
                .recover(&DelegatedRecovery {
                    backing,
                    root: ns.root,
                    expected_fence: newer.token.fence,
                })
                .await
                .unwrap();
            let peer =
                PgliteMetadataStore::connect_with_key(server.connection_string(), "delegation")
                    .await
                    .unwrap();
            assert_eq!(
                peer.delegation_state().await.unwrap(),
                metadata.delegation_state().await.unwrap()
            );
            peer.close().await.unwrap();
            metadata.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn fresh_concurrent_mode_persists_and_fences_legacy_writers() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metadata =
                PgliteMetadataStore::connect_with_key(connection_string, "concurrent-fresh")
                    .await
                    .unwrap();
            let blocks = PgliteBlockStore::connect_with_key(connection_string, "concurrent-fresh")
                .await
                .unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            metadata
                .prepare_bound_concurrent_mode(backing)
                .await
                .unwrap();
            metadata
                .prepare_bound_concurrent_mode(backing)
                .await
                .unwrap();
            metadata.close().await.unwrap();

            let reopened =
                PgliteMetadataStore::connect_with_key(connection_string, "concurrent-fresh")
                    .await
                    .unwrap();
            reopened
                .prepare_bound_concurrent_mode(backing)
                .await
                .unwrap();
            assert_eq!(
                reopened.concurrent_mode_state().await.unwrap(),
                ConcurrentModeState::Mrc2(backing)
            );
            assert!(
                reopened
                    .acquire_writer("legacy-writer", Duration::from_secs(60))
                    .await
                    .is_err(),
                "a legacy lease must never be acquired after concurrent conversion"
            );
            // This is the pre-change acquisition condition: it knows
            // nothing about write_mode. The max-fence sentinel still makes
            // it return no row, so an older binary cannot claim the volume.
            let client = reopened.0.lock_client().await.unwrap();
            let old_owner = "older-binary".to_owned();
            let old_sql = format!(
                "UPDATE mount_rs_metadata SET owner=$2, fence=fence+1, expires={NOW}+60000
                 WHERE volume_key=$1 AND (owner IS NULL OR expires<={NOW})
                   AND fence<9223372036854775807
                 RETURNING fence"
            );
            let old_acquire = client
                .as_ref()
                .unwrap()
                .query_typed_opt(
                    &old_sql,
                    &[
                        (&reopened.0.volume_key, Type::TEXT),
                        (&old_owner, Type::TEXT),
                    ],
                )
                .await
                .unwrap();
            assert!(
                old_acquire.is_none(),
                "an older binary bypassed the sentinel"
            );
            drop(client);
            reopened.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn concurrent_publications_have_one_winner_and_one_known_conflict() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("concurrent-cas");
            let first =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let second =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(connection_string, options)
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            blocks.flush().await.unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            first.prepare_bound_concurrent_mode(backing).await.unwrap();
            second.prepare_bound_concurrent_mode(backing).await.unwrap();

            let namespace = namespace(block).await;
            let a = tokio::spawn({
                let first = first.clone();
                let namespace = namespace.clone();
                async move { first.publish_bound_if_revision(backing, 0, namespace).await }
            });
            let b = tokio::spawn({
                let second = second.clone();
                let namespace = namespace.clone();
                async move { second.publish_bound_if_revision(backing, 0, namespace).await }
            });
            let (a, b) = (a.await.unwrap(), b.await.unwrap());
            assert!(
                matches!((&a, &b), (Ok(1), Err(error)) | (Err(error), Ok(1)) if error.is(ErrorCode::Eagain)),
                "exactly one revision-zero publication must commit: {a:?}, {b:?}"
            );
            let loaded = second.load().await.unwrap();
            assert_eq!(loaded.revision, 1);
            assert_eq!(
                serde_json::to_value(loaded.namespace).unwrap(),
                serde_json::to_value(Some(namespace)).unwrap()
            );
            first.close().await.unwrap();
            second.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn previously_leased_pglite_volume_requires_offline_migration() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let metadata =
                PgliteMetadataStore::connect_with_key(connection_string, "concurrent-legacy")
                    .await
                    .unwrap();
            let blocks = PgliteBlockStore::connect_with_key(connection_string, "concurrent-legacy")
                .await
                .unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            let lease = metadata
                .acquire_writer("old-client", Duration::from_secs(60))
                .await
                .unwrap();
            assert!(
                metadata
                    .prepare_bound_concurrent_mode(backing)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Ebusy)
            );
            metadata.release_writer(&lease).await.unwrap();
            assert!(
                metadata
                    .prepare_bound_concurrent_mode(backing)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Ebusy),
                "an expired or released old fence still needs an offline migration"
            );
            metadata.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn historical_pglite_volume_cannot_enter_concurrent_mode() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for (suffix, seed_sql) in [
                (
                    "revision",
                    "UPDATE mount_rs_metadata SET revision=1 WHERE volume_key=$1",
                ),
                (
                    "namespace",
                    "UPDATE mount_rs_metadata SET namespace='{}' WHERE volume_key=$1",
                ),
                (
                    "version-head",
                    "UPDATE mount_rs_version_state SET head_id='seed-version' WHERE volume_key=$1",
                ),
                (
                    "version-row",
                    "INSERT INTO mount_rs_versions
                     (volume_key,id,volume_id,sequence,parent_id,restored_from,forked_from,
                      namespace,block_store_id,kind,created_at_ms,durable,operation_id)
                     VALUES($1,'seed-version','seed-volume',1,NULL,NULL,NULL,'{}',
                            'seed-blocks','snapshot',0,FALSE,'seed-op')",
                ),
                (
                    "version-pin",
                    "INSERT INTO mount_rs_version_pins
                     (volume_key,view_id,volume_id,version_id,owner,fence,expires)
                     VALUES($1,'seed-view','seed-volume','seed-version','seed-owner',1,0)",
                ),
            ] {
                let key = format!("concurrent-historical-{suffix}");
                let metadata = PgliteMetadataStore::connect_with_key(connection_string, &key)
                    .await
                    .unwrap();
                let blocks = PgliteBlockStore::connect_with_key(connection_string, &key)
                    .await
                    .unwrap();
                let backing = blocks.prepare_concurrent_backing().await.unwrap();
                let client = metadata.0.lock_client().await.unwrap();
                client
                    .as_ref()
                    .unwrap()
                    .execute_typed(seed_sql, &[(&metadata.0.volume_key, Type::TEXT)])
                    .await
                    .unwrap();
                drop(client);

                let error = metadata
                    .prepare_bound_concurrent_mode(backing)
                    .await
                    .unwrap_err();
                assert!(error.is(ErrorCode::Ebusy), "{suffix}: {error:?}");
                let client = metadata.0.lock_client().await.unwrap();
                let row = client
                    .as_ref()
                    .unwrap()
                    .query_typed_opt(
                        "SELECT write_mode, fence FROM mount_rs_metadata WHERE volume_key=$1",
                        &[(&metadata.0.volume_key, Type::TEXT)],
                    )
                    .await
                    .unwrap()
                    .unwrap();
                assert!(row.get::<_, Option<String>>(0).is_none(), "{suffix}");
                assert_eq!(row.get::<_, i64>(1), 0, "{suffix}");
                drop(client);
                metadata.close().await.unwrap();
                blocks.close().await.unwrap();
            }
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn concurrent_conversion_race_cannot_admit_a_legacy_writer() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("concurrent-conversion-race");
            let metadata =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let legacy = PgliteMetadataStore::connect_with_options(connection_string, options)
                .await
                .unwrap();
            let blocks = PgliteBlockStore::connect_with_key(connection_string, "concurrent-conversion-race")
                .await
                .unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            let conversion = tokio::spawn({
                let metadata = metadata.clone();
                async move { metadata.prepare_bound_concurrent_mode(backing).await }
            });
            let old_acquire = tokio::spawn({
                let legacy = legacy.clone();
                async move { legacy.acquire_writer("old", Duration::from_secs(60)).await }
            });
            let (conversion, old_acquire) =
                (conversion.await.unwrap(), old_acquire.await.unwrap());
            match (conversion, old_acquire) {
                (Ok(()), Err(error)) if error.is(ErrorCode::Eagain) => {
                    metadata.prepare_bound_concurrent_mode(backing).await.unwrap();
                }
                (Err(error), Ok(lease)) if error.is(ErrorCode::Ebusy) => {
                    assert!(metadata.prepare_bound_concurrent_mode(backing).await.unwrap_err().is(ErrorCode::Ebusy));
                    legacy.release_writer(&lease).await.unwrap();
                }
                (conversion, old_acquire) => {
                    panic!("mode conversion and legacy writer must never both win: {conversion:?}, {old_acquire:?}");
                }
            }
            metadata.close().await.unwrap();
            legacy.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn simultaneous_pglite_clients_can_initialize_one_concurrent_volume() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string().to_owned();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let first = tokio::spawn({
                let connection_string = connection_string.clone();
                async move {
                    PgliteMetadataStore::connect_with_key(&connection_string, "simultaneous-open")
                        .await
                }
            });
            let second = tokio::spawn({
                let connection_string = connection_string.clone();
                async move {
                    PgliteMetadataStore::connect_with_key(&connection_string, "simultaneous-open")
                        .await
                }
            });
            let first = first.await.unwrap().unwrap();
            let second = second.await.unwrap().unwrap();
            let blocks =
                PgliteBlockStore::connect_with_key(&connection_string, "simultaneous-open")
                    .await
                    .unwrap();
            first.preflight_new_bound_mode().await.unwrap();
            second.preflight_new_bound_mode().await.unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            let conversion = tokio::spawn({
                let first = first.clone();
                async move { first.prepare_bound_concurrent_mode(backing).await }
            });
            let other_conversion = tokio::spawn({
                let second = second.clone();
                async move { second.prepare_bound_concurrent_mode(backing).await }
            });
            conversion.await.unwrap().unwrap();
            other_conversion.await.unwrap().unwrap();
            first.close().await.unwrap();
            second.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn legacy_lease_operations_reject_a_concurrent_mode_marker() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("concurrent-marker-guard");
            let metadata =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(connection_string, options)
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            let lease = metadata
                .acquire_writer("legacy-writer", Duration::from_secs(60))
                .await
                .unwrap();
            // Simulate an inconsistent migration or a damaged marker. A
            // correct conversion never leaves a lease live, but operations
            // must fail closed if the marker appears while one is present.
            let client = metadata.0.lock_client().await.unwrap();
            let backing_text = backing.to_hex();
            client
                .as_ref()
                .unwrap()
                .execute_typed(
                    "UPDATE mount_rs_metadata SET write_mode=$2, backing_id=$3 WHERE volume_key=$1",
                    &[
                        (&metadata.0.volume_key, Type::TEXT),
                        (&BOUND_CONCURRENT_WRITE_MODE, Type::TEXT),
                        (&backing_text, Type::TEXT),
                    ],
                )
                .await
                .unwrap();
            drop(client);
            assert!(
                metadata
                    .renew_writer(&lease, Duration::from_secs(60))
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            assert!(
                metadata
                    .publish(0, &lease, namespace(block).await)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            assert!(
                metadata
                    .release_writer(&lease)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            metadata.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn bound_concurrent_publication_rejects_a_damaged_legacy_fence() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("concurrent-damaged-fence");
            let metadata =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(connection_string, options)
                .await
                .unwrap();
            let block = blocks.put(b"abc").await.unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            metadata
                .prepare_bound_concurrent_mode(backing)
                .await
                .unwrap();
            let client = metadata.0.lock_client().await.unwrap();
            client
                .as_ref()
                .unwrap()
                .execute_typed(
                    "UPDATE mount_rs_metadata SET fence=0 WHERE volume_key=$1",
                    &[(&metadata.0.volume_key, Type::TEXT)],
                )
                .await
                .unwrap();
            drop(client);
            assert!(
                metadata
                    .prepare_bound_concurrent_mode(backing)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            assert!(
                metadata
                    .publish_bound_if_revision(backing, 0, namespace(block).await)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Ebusy),
                "a damaged mode marker must not become a retryable CAS miss"
            );
            metadata.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn disconnected_pglite_socket_is_never_a_revision_conflict() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string().to_owned();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("concurrent-disconnected-socket");
            let metadata =
                PgliteMetadataStore::connect_with_options(&connection_string, options.clone())
                    .await
                    .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(&connection_string, options)
                .await
                .unwrap();
            let backing = blocks.prepare_concurrent_backing().await.unwrap();
            metadata
                .prepare_bound_concurrent_mode(backing)
                .await
                .unwrap();
            blocks.close().await.unwrap();
            let namespace = namespace(BlockId("unused-disconnected-block".to_owned())).await;
            drop(server);
            let error = metadata
                .publish_bound_if_revision(backing, 0, namespace)
                .await
                .unwrap_err();
            assert!(
                error.is(ErrorCode::Eio),
                "unexpected socket error: {error:?}"
            );
            metadata.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn split_stores_enforce_durability_fencing_cas_and_immutable_blocks() {
        let server = PgliteServer::start();
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("pglite-storage-test");
            let metadata =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let competitor =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(connection_string, options)
                .await
                .unwrap();
            assert!(!metadata.durable());
            assert!(!blocks.durable());

            let other_volume = PgliteMetadataStore::connect_with_key(
                connection_string,
                "pglite-storage-other-volume",
            )
            .await
            .unwrap();
            assert_eq!(other_volume.load().await.unwrap().revision, 0);
            let other_lease = other_volume
                .acquire_writer("other-volume", Duration::from_secs(60))
                .await
                .unwrap();
            other_volume.release_writer(&other_lease).await.unwrap();

            let assured = PgliteMetadataStore::connect_with_options(
                connection_string,
                PgliteStorageOptions::new("pglite-storage-assured").with_durable(true),
            )
            .await
            .unwrap();
            assert!(assured.durable());

            let bytes = b"pglite";
            let id = blocks.put(bytes).await.unwrap();
            assert_eq!(blocks.put(bytes).await.unwrap(), id);
            assert_eq!(blocks.get(&id).await.unwrap(), bytes);
            let orphan = blocks.put(b"orphan").await.unwrap();
            blocks.flush().await.unwrap();

            let first = metadata
                .acquire_writer("pglite-test-first", Duration::from_secs(60))
                .await
                .unwrap();
            assert!(
                competitor
                    .acquire_writer("pglite-test-second", Duration::from_secs(60))
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Eagain)
            );
            let current = metadata.load().await.unwrap();
            let renewed = metadata
                .renew_writer(&first, Duration::from_secs(120))
                .await
                .unwrap();
            assert!(renewed.expires_at_ms > first.expires_at_ms);
            assert!(
                metadata
                    .publish(current.revision, &first, namespace(id.clone()).await)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            let revision = metadata
                .publish(current.revision, &renewed, namespace(id.clone()).await)
                .await
                .unwrap();
            assert_eq!(revision, current.revision + 1);
            assert!(metadata.load().await.unwrap().namespace.is_some());
            assert!(
                metadata
                    .publish(current.revision, &renewed, namespace(id.clone()).await)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Eagain)
            );
            metadata.release_writer(&renewed).await.unwrap();
            let second = competitor
                .acquire_writer("pglite-test-second", Duration::from_secs(60))
                .await
                .unwrap();
            assert!(second.fence > renewed.fence);
            assert!(
                metadata
                    .publish(revision, &renewed, namespace(id.clone()).await)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );

            blocks.delete(&orphan).await.unwrap();
            assert!(blocks.get(&orphan).await.unwrap_err().is(ErrorCode::Enoent));
            competitor.release_writer(&second).await.unwrap();
            metadata.flush().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn normal_block_read_rejects_same_length_tamper() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let blocks = PgliteBlockStore::connect_with_key(
                server.connection_string(),
                "normal-block-read-tamper",
            )
            .await
            .unwrap();
            let original = b"original";
            let tampered = b"tampered".to_vec();
            assert_eq!(original.len(), tampered.len());
            let id = blocks.put(original).await.unwrap();
            assert_eq!(blocks.get(&id).await.unwrap(), original);
            assert_ne!(block_id(&tampered), id.0);

            let peer = PgliteBlockStore::connect_with_key(
                server.connection_string(),
                "normal-block-read-tamper",
            )
            .await
            .unwrap();
            let client = peer.0.lock_client().await.unwrap();
            assert_eq!(
                client
                    .as_ref()
                    .unwrap()
                    .execute_typed(
                        "UPDATE mount_rs_blocks SET bytes=$3 WHERE volume_key=$1 AND id=$2",
                        &[
                            (&peer.0.volume_key, Type::TEXT),
                            (&id.0, Type::TEXT),
                            (&tampered, Type::BYTEA),
                        ],
                    )
                    .await
                    .unwrap(),
                1
            );
            drop(client);
            peer.close().await.unwrap();

            let error = blocks.get(&id).await.unwrap_err();
            assert!(error.is(ErrorCode::Eio), "unexpected error: {error:?}");
            blocks.close().await.unwrap();
        });
    }

    /// Keep the server deliberately bounded so an early close return leaves a
    /// visible connection slot behind. This covers cancellation, simultaneous
    /// closes on clones, repeated idempotent calls, post-close operations, and
    /// reopening both provider types after complete teardown.
    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn bounded_server_close_is_shared_cancellation_safe_and_reusable() {
        let server = PgliteServer::start_with_max_connections(2);
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let options = PgliteStorageOptions::new("close-lifecycle");
            let metadata =
                PgliteMetadataStore::connect_with_options(connection_string, options.clone())
                    .await
                    .unwrap();
            let blocks = PgliteBlockStore::connect_with_options(connection_string, options)
                .await
                .unwrap();
            let block = blocks.put(b"close").await.unwrap();

            cancel_close_while_client_is_held(&metadata.0).await;
            for _ in 0..3 {
                metadata.close().await.unwrap();
            }
            assert!(metadata.load().await.unwrap_err().is(ErrorCode::Ebadf));

            cancel_close_while_client_is_held(&blocks.0).await;
            for _ in 0..3 {
                blocks.close().await.unwrap();
            }
            assert!(blocks.get(&block).await.unwrap_err().is(ErrorCode::Ebadf));

            let reopened_metadata =
                PgliteMetadataStore::connect_with_key(connection_string, "close-reopened")
                    .await
                    .unwrap();
            let reopened_blocks =
                PgliteBlockStore::connect_with_key(connection_string, "close-reopened")
                    .await
                    .unwrap();
            reopened_metadata.close().await.unwrap();
            reopened_blocks.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn readiness_handshake_does_not_consume_bounded_connection_slot() {
        // With one slot, a readiness connection would compete directly with
        // the first real client. The successful first connection proves that
        // PGLITE_READY itself did not enter the server's bounded set; the
        // immediate reopen proves the real client released its slot.
        let server = PgliteServer::start_with_max_connections(1);
        let connection_string = server.connection_string();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let first =
                PgliteMetadataStore::connect_with_key(connection_string, "readiness-slot-first")
                    .await
                    .expect("first real connection should fit after readiness");
            first.close().await.unwrap();
            let reopened =
                PgliteMetadataStore::connect_with_key(connection_string, "readiness-slot-reopened")
                    .await
                    .expect("closing the real connection should release its slot");
            reopened.close().await.unwrap();
        });
    }
    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn concurrent_backing_is_stable_per_server_and_key_and_fences_metadata() {
        let server = PgliteServer::start();
        let other_server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let url = server.connection_string();
            let a = PgliteBlockStore::connect_with_key(url, "bound-a").await.unwrap();
            let a_reopened = PgliteBlockStore::connect_with_key(url, "bound-a").await.unwrap();
            let b = PgliteBlockStore::connect_with_key(url, "bound-b").await.unwrap();
            let other = PgliteBlockStore::connect_with_key(other_server.connection_string(), "bound-a")
                .await.unwrap();
            let a_id = a.prepare_concurrent_backing().await.unwrap();
            assert_eq!(a_id, a_reopened.prepare_concurrent_backing().await.unwrap());
            let b_id = b.prepare_concurrent_backing().await.unwrap();
            assert_ne!(a_id, b_id);
            assert_ne!(a_id, other.prepare_concurrent_backing().await.unwrap());
            a.verify_concurrent_backing(a_id).await.unwrap();
            assert!(b.verify_concurrent_backing(a_id).await.unwrap_err().is(ErrorCode::Estale));

            let metadata = PgliteMetadataStore::connect_with_key(url, "bound-a").await.unwrap();
            let metadata_peer = PgliteMetadataStore::connect_with_key(url, "bound-a").await.unwrap();
            metadata.prepare_bound_concurrent_mode(a_id).await.unwrap();
            metadata_peer.prepare_bound_concurrent_mode(a_id).await.unwrap();
            assert_eq!(metadata.concurrent_mode_state().await.unwrap(), ConcurrentModeState::Mrc2(a_id));
            assert!(metadata.prepare_bound_concurrent_mode(b_id).await.unwrap_err().is(ErrorCode::Estale));
            let block = a.put(b"abc").await.unwrap();
            let namespace = namespace(block).await;
            assert!(metadata.publish_bound_if_revision(b_id, 0, namespace.clone()).await.unwrap_err().is(ErrorCode::Estale));
            assert_eq!(metadata.load().await.unwrap().revision, 0);
            let first_task = tokio::spawn({
                let metadata = metadata.clone();
                let namespace = namespace.clone();
                async move { metadata.publish_bound_if_revision(a_id, 0, namespace).await }
            });
            let second_task = tokio::spawn({
                let metadata_peer = metadata_peer.clone();
                async move { metadata_peer.publish_bound_if_revision(a_id, 0, namespace).await }
            });
            let (first, second) = (first_task.await.unwrap(), second_task.await.unwrap());
            assert!(matches!((&first, &second), (Ok(1), Err(error)) | (Err(error), Ok(1)) if error.is(ErrorCode::Eagain)));
            metadata.close().await.unwrap();
            metadata_peer.close().await.unwrap();
            a.close().await.unwrap();
            a_reopened.close().await.unwrap();
            b.close().await.unwrap();
            other.close().await.unwrap();
        });
    }

    #[test]
    #[ignore = "requires the isolated tests/pglite Node server and its dependencies"]
    fn concurrent_backing_migration_requires_exact_revision_and_direct_blocks() {
        let server = PgliteServer::start();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let url = server.connection_string();
            let metadata = PgliteMetadataStore::connect_with_key(url, "bound-migration")
                .await
                .unwrap();
            let blocks = PgliteBlockStore::connect_with_key(url, "bound-migration")
                .await
                .unwrap();
            seed_test_owned_mrc1_volume(&metadata).await;
            assert_eq!(
                metadata.concurrent_mode_state().await.unwrap(),
                ConcurrentModeState::Mrc1
            );
            assert!(
                metadata
                    .preflight_mrc1_to_bound_mode(1)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Eagain)
            );
            metadata.preflight_mrc1_to_bound_mode(0).await.unwrap();
            let id = blocks.prepare_concurrent_backing().await.unwrap();
            assert!(
                metadata
                    .prepare_bound_concurrent_mode(id)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Ebusy)
            );
            assert!(
                metadata
                    .migrate_mrc1_to_bound_mode(id, 1)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Eagain)
            );
            assert_eq!(
                metadata.concurrent_mode_state().await.unwrap(),
                ConcurrentModeState::Mrc1
            );
            let block = blocks.put(b"abc").await.unwrap();
            assert_eq!(blocks.get_for_migration(&block).await.unwrap(), b"abc");
            metadata.migrate_mrc1_to_bound_mode(id, 0).await.unwrap();
            assert_eq!(
                metadata.concurrent_mode_state().await.unwrap(),
                ConcurrentModeState::Mrc2(id)
            );
            assert_eq!(metadata.load().await.unwrap().revision, 0);
            let client = metadata.0.lock_client().await.unwrap();
            client
                .as_ref()
                .unwrap()
                .execute_typed(
                    "UPDATE mount_rs_metadata SET backing_id=NULL WHERE volume_key=$1",
                    &[(&metadata.0.volume_key, Type::TEXT)],
                )
                .await
                .unwrap();
            drop(client);
            assert!(
                metadata
                    .concurrent_mode_state()
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            let client = metadata.0.lock_client().await.unwrap();
            let backing_text = id.to_hex();
            client
                .as_ref()
                .unwrap()
                .execute_typed(
                    "UPDATE mount_rs_metadata SET backing_id=$2 WHERE volume_key=$1",
                    &[
                        (&metadata.0.volume_key, Type::TEXT),
                        (&backing_text, Type::TEXT),
                    ],
                )
                .await
                .unwrap();
            drop(client);
            let client = blocks.0.lock_client().await.unwrap();
            client
                .as_ref()
                .unwrap()
                .execute_typed(
                    "DELETE FROM mount_rs_block_authority WHERE volume_key=$1",
                    &[(&blocks.0.volume_key, Type::TEXT)],
                )
                .await
                .unwrap();
            drop(client);
            assert!(
                blocks
                    .verify_concurrent_backing(id)
                    .await
                    .unwrap_err()
                    .is(ErrorCode::Estale)
            );
            metadata.close().await.unwrap();
            blocks.close().await.unwrap();
        });
    }
}
