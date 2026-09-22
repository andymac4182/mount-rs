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
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::versioning::{
    PublicationId, ReadLease, ReadLeaseRequest, VersionHead, VersionId, VersionInfo, VersionKind,
    VersionPublication, VersionedMetadataStore, VolumeId,
};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
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

const METADATA_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_metadata (
 volume_key TEXT PRIMARY KEY NOT NULL,
 revision BIGINT NOT NULL CHECK(revision>=0),
 namespace TEXT,
 owner TEXT,
 fence BIGINT NOT NULL CHECK(fence>=0),
 expires BIGINT NOT NULL,
 volume_id TEXT);
ALTER TABLE mount_rs_metadata ADD COLUMN IF NOT EXISTS volume_id TEXT;
";

const BLOCK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_blocks (
 volume_key TEXT NOT NULL, id TEXT NOT NULL, bytes BYTEA NOT NULL,
 PRIMARY KEY (volume_key, id));";

const NOW_SELECT: &str = "SELECT CAST(EXTRACT(EPOCH FROM clock_timestamp()) * 1000 AS BIGINT)";
const VERSION_SCHEMA_NAME: &str = "mount-rs-versioning";
const VERSION_SCHEMA_VERSION: i64 = 1;
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

impl PgliteMetadataStore {
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
        let exists = tx
            .query_typed_opt(
                "SELECT EXISTS(
                     SELECT 1 FROM mount_rs_versions
                     WHERE volume_key=$1 AND id=$2 AND volume_id=$3
                 )",
                &[
                    (&database.volume_key, Type::TEXT),
                    (&head_id, Type::TEXT),
                    (&volume_id.0, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| incompatible_schema("PGlite schema query returned no row"))?
            .get::<_, bool>(0);
        if !exists {
            return Err(incompatible_schema(
                "PGlite version head references a missing version",
            ));
        }
    }

    tx.execute_typed(
        "INSERT INTO mount_rs_schema_versions(schema_name, schema_version)
         VALUES ($1, $2)
         ON CONFLICT(schema_name) DO UPDATE SET schema_version=excluded.schema_version",
        &[
            (&VERSION_SCHEMA_NAME, Type::TEXT),
            (&VERSION_SCHEMA_VERSION, Type::INT8),
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

fn nonnegative(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| backend_error(format!("invalid PGlite {field}")))
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
            .with_message(format!("unknown PGlite version kind '{value}'"))),
    }
}

fn decode_version_row(row: &tokio_postgres::Row) -> Result<VersionInfo> {
    let id_text = row.get::<_, String>(0);
    let volume_text = row.get::<_, String>(2);
    let volume = VolumeId::new(volume_text)?;
    let sequence = nonnegative(row.get::<_, i64>(3), "version sequence")?;
    let id = VersionId::new(volume.clone(), sequence)?;
    if id.encode() != id_text {
        return Err(FsError::new(ErrorCode::Eio)
            .with_syscall("load version")
            .with_message("stored PGlite version id does not match its volume and sequence"));
    }
    let parent = row
        .get::<_, Option<String>>(4)
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let restored_from = row
        .get::<_, Option<String>>(5)
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let forked_from = row
        .get::<_, Option<String>>(6)
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let namespace = serde_json::from_str(&row.get::<_, String>(7)).map_err(backend_error)?;
    let block_store_id = mount_rs_core::versioning::BlockStoreId::new(row.get::<_, String>(8))?;
    let info = VersionInfo {
        id,
        parent,
        restored_from,
        forked_from,
        kind: parse_version_kind(&row.get::<_, String>(9))?,
        namespace,
        block_store_id,
        created_at_ms: row.get::<_, i64>(10),
        durable: row.get::<_, bool>(11),
    };
    info.validate()?;
    Ok(info)
}

const VERSION_SELECT: &str = "SELECT id, volume_key, volume_id, sequence,
 parent_id, restored_from, forked_from, namespace, block_store_id, kind,
 created_at_ms, durable FROM mount_rs_versions";

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
                "SELECT revision, namespace FROM mount_rs_metadata WHERE volume_key = $1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        let revision = nonnegative(row.get::<_, i64>(0), "metadata revision")?;
        let namespace = row
            .get::<_, Option<String>>(1)
            .map(|json| serde_json::from_str(&json))
            .transpose()
            .map_err(backend_error)?;
        Ok(LoadedMetadata {
            revision,
            namespace,
        })
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        if owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let ttl = ttl_ms(ttl)?;
        let owner = owner.to_owned();
        let sql = format!(
            "UPDATE mount_rs_metadata SET owner=$2, fence=fence+1, expires={NOW}+$3
             WHERE volume_key=$1 AND (owner IS NULL OR expires<={NOW})
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
             WHERE volume_key=$1 AND owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}
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
             WHERE volume_key=$1 AND owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}"
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
                     WHERE volume_key=$1 AND revision=$4
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
                             (owner IS NOT DISTINCT FROM $2 AND fence=$3 AND expires=$4
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
        let metadata = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT revision FROM mount_rs_metadata WHERE volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite metadata row is missing"))?;
        let revision = nonnegative(metadata.get::<_, i64>(0), "metadata revision")?;
        let state = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT head_id FROM mount_rs_version_state WHERE volume_key=$1",
                &[(&self.0.volume_key, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| incompatible_schema("PGlite version state is missing"))?;
        let Some(head_id) = state.get::<_, Option<String>>(0) else {
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
        let exists = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT EXISTS(
                     SELECT 1 FROM mount_rs_versions
                     WHERE volume_key=$1 AND id=$2 AND volume_id=$3
                 )",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&head_id, Type::TEXT),
                    (&self.1.0, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| incompatible_schema("PGlite version query returned no row"))?
            .get::<_, bool>(0);
        if !exists {
            return Err(FsError::new(ErrorCode::Eio)
                .with_syscall("version head")
                .with_message("stored PGlite version head references a missing version"));
        }
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
                         (owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}) AS lease_valid,
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
            let exists = tx
                .query_typed_opt(
                    "SELECT EXISTS(
                         SELECT 1 FROM mount_rs_versions
                         WHERE volume_key=$1 AND id=$2 AND volume_id=$3
                     )",
                    &[
                        (&self.0.volume_key, Type::TEXT),
                        (&head_id, Type::TEXT),
                        (&self.1.0, Type::TEXT),
                    ],
                )
                .await
                .map_err(postgres_error)?
                .ok_or_else(|| incompatible_schema("PGlite version query returned no row"))?
                .get::<_, bool>(0);
            if !exists {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("publish version")
                    .with_message("stored PGlite version head references a missing version"));
            }
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
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let metadata_update_sql = format!(
            "UPDATE mount_rs_metadata SET revision=$2, namespace=$3
                 WHERE volume_key=$1 AND revision=$4
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
        let exists = tx
            .query_typed_opt(
                "SELECT EXISTS(
                     SELECT 1 FROM mount_rs_versions
                     WHERE volume_key=$1 AND id=$2 AND volume_id=$3
                 )",
                &[
                    (&self.0.volume_key, Type::TEXT),
                    (&encoded, Type::TEXT),
                    (&self.1.0, Type::TEXT),
                ],
            )
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| incompatible_schema("PGlite version query returned no row"))?
            .get::<_, bool>(0);
        if !exists {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("open version view"));
        }
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
        if current_volume != self.1.0
            || current_version != lease.version.encode()
            || current_owner != request.owner
            || current_owner != lease.owner
            || current_fence != fence
            || current_expires != expires
            || current_expires <= now_ms
        {
            return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
        }
        let next_expires = now_ms
            .checked_add(ttl_ms)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
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
                    (&self.1.0, Type::TEXT),
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
                    "SELECT owner=$2 AND fence=$3 AND expires=$4 AND expires>{NOW}
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

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let bytes = bytes.to_vec();
        let client = self.0.lock_client().await?;
        // PostgreSQL's built-in md5/encode functions give the same opaque
        // identity for equal bytes without requiring a PGlite extension.
        // A collision is checked below and never aliases different content.
        let row = client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt("SELECT md5(encode($1, 'hex'))", &[(&bytes, Type::BYTEA)])
            .await
            .map_err(postgres_error)?
            .ok_or_else(|| backend_error("PGlite did not return a block identity"))?;
        let id = row.get::<_, String>(0);
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
        client
            .as_ref()
            .ok_or_else(connection_closed)?
            .query_typed_opt(
                "SELECT bytes FROM mount_rs_blocks WHERE volume_key=$1 AND id=$2",
                &[(&self.0.volume_key, Type::TEXT), (&id.0, Type::TEXT)],
            )
            .await
            .map_err(postgres_error)?
            .map(|row| row.get::<_, Vec<u8>>(0))
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("get block"))
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
    use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
    use mount_rs_core::storage::{BlockExtent, FileLayout, NodeData, NodeMetadata};
    use mount_rs_core::{FsDriver, MemoryFs};
    use std::collections::BTreeMap;

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
}
