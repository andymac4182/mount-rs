use async_trait::async_trait;
use mount_rs_core::delegation::{
    CheckoutRequest, DelegatedCheckin, DelegatedPublish, DelegatedRecovery, DelegationState,
    DirectoryGrant,
};
use mount_rs_core::diagnostics::profile::{self, Event};
use mount_rs_core::storage::{
    BlockId, BlockStore, ConcurrentBackingId, ConcurrentModeState, InodeId, InodeMetadataSnapshot,
    InodeModeState, InodeVersion, LoadedInode, LoadedMetadata, MetadataStore, Namespace,
    NodeMetadata, WriterLease, decode_inode_namespace, encode_inode_namespace,
    validate_inode_publication, validate_node_kind,
};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use mysql_async::prelude::Queryable;
use mysql_async::{
    Conn, Error as MysqlError, IsolationLevel, Opts, OptsBuilder, Params, Pool, Transaction, TxOpts,
};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::OnceCell;

/// A conservative default below TiDB's default single-entry and packet
/// limits. Deployments that raise the corresponding TiDB/TiKV limits may
/// select a larger value explicitly.
pub const DEFAULT_MAX_BLOCK_BYTES: usize = 4 * 1024 * 1024;

/// Namespace JSON is kept below the same conservative single-entry limit.
pub const DEFAULT_MAX_NAMESPACE_BYTES: usize = 4 * 1024 * 1024;

const MAX_SCOPE_CHARS: usize = 255;
const BLOCK_ID_LENGTH: usize = 65;
const BLOCK_ID_PREFIX: u8 = b'b';
const NOW_MS: &str = "CAST(UNIX_TIMESTAMP(CURRENT_TIMESTAMP(3)) * 1000 AS SIGNED)";
const CONCURRENT_WRITE_MODE: &str = "MRC1";
const BOUND_CONCURRENT_WRITE_MODE: &str = "MRC2";
// Older clients cannot acquire a signed BIGINT fence beyond this value. The
// marker and exhausted fence are published in the same atomic statement.
const CONCURRENT_FENCE_SENTINEL: i64 = i64::MAX;
const REVISION_PROBE_SQL: &str = "SELECT revision,write_mode FROM mount_rs_tidb_metadata \
    USE INDEX (idx_mount_rs_volume_revision_mode) WHERE volume_key=?";

const INODE_AUTHORITY_SQL: &str = "SELECT revision,write_mode,backing_id,owner,fence,expires
    FROM mount_rs_tidb_metadata USE INDEX (idx_mount_rs_inode_authority) WHERE volume_key=?";
const INODE_READ_SQL: &str = "SELECT m.revision,m.write_mode,m.backing_id,m.owner,m.fence,m.expires,
    i.inode,i.generation,i.revision,CASE WHEN i.generation=? AND i.revision=? THEN NULL ELSE i.node END
    FROM mount_rs_tidb_metadata m USE INDEX (idx_mount_rs_inode_authority)
    LEFT JOIN mount_rs_tidb_inodes i ON i.volume_key=m.volume_key AND i.inode=?
    WHERE m.volume_key=?";

const METADATA_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_tidb_metadata (
    volume_key VARBINARY(1020) NOT NULL,
    revision BIGINT NOT NULL,
    namespace LONGTEXT NULL,
    owner VARBINARY(1020) NULL,
    fence BIGINT NOT NULL,
    expires BIGINT NOT NULL,
    write_mode VARBINARY(4) NULL,
    backing_id VARBINARY(32) NULL,
    delegation LONGTEXT NULL,
    PRIMARY KEY (volume_key)
)";

const INODE_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_tidb_inodes (
    volume_key VARBINARY(1020) NOT NULL,
    inode BIGINT NOT NULL,
    generation BIGINT NOT NULL,
    revision BIGINT NOT NULL,
    node LONGTEXT NOT NULL,
    PRIMARY KEY (volume_key, inode)
)";

const BLOCK_AUTHORITY_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_tidb_block_authority (
    volume_key VARBINARY(1020) NOT NULL,
    backing_id VARBINARY(32) NOT NULL,
    PRIMARY KEY (volume_key)
)";

const BLOCK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_tidb_blocks (
    volume_key VARBINARY(1020) NOT NULL,
    id VARBINARY(65) NOT NULL,
    bytes LONGBLOB NOT NULL,
    PRIMARY KEY (volume_key, id)
)";

/// Options shared by independently connected metadata and block stores.
///
/// `durable` is caller-declared. The adapter cannot infer whether a TiDB
/// deployment's TiKV replicas and sync-log settings meet an application's
/// crash/power-loss durability target from a MySQL URL alone.
#[derive(Debug, Clone)]
pub struct TidbStorageOptions {
    pub volume_key: String,
    pub durable: bool,
    pub max_block_bytes: usize,
    pub max_namespace_bytes: usize,
}

impl TidbStorageOptions {
    pub fn new(volume_key: impl Into<String>) -> Self {
        Self {
            volume_key: volume_key.into(),
            durable: false,
            max_block_bytes: DEFAULT_MAX_BLOCK_BYTES,
            max_namespace_bytes: DEFAULT_MAX_NAMESPACE_BYTES,
        }
    }

    pub fn with_durable(mut self, durable: bool) -> Self {
        self.durable = durable;
        self
    }

    pub fn with_max_block_bytes(mut self, max_block_bytes: usize) -> Self {
        self.max_block_bytes = max_block_bytes;
        self
    }

    pub fn with_max_namespace_bytes(mut self, max_namespace_bytes: usize) -> Self {
        self.max_namespace_bytes = max_namespace_bytes;
        self
    }

    fn validate(&self) -> Result<()> {
        validate_scope(&self.volume_key, "TiDB volume key")?;
        if self.max_block_bytes == 0 {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("TiDB maximum block size must be positive"));
        }
        if self.max_namespace_bytes == 0 {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("TiDB maximum namespace size must be positive"));
        }
        Ok(())
    }
}

impl Default for TidbStorageOptions {
    fn default() -> Self {
        Self::new("mount-rs")
    }
}

#[derive(Clone)]
struct Database {
    pool: Pool,
    volume_key: String,
    durable: bool,
    max_block_bytes: usize,
    max_namespace_bytes: usize,
    shared_context: Option<Arc<TidbPoolInner>>,
}

impl Database {
    async fn connect(
        url: &str,
        options: TidbStorageOptions,
        schema: &str,
        ensure_metadata_row: bool,
    ) -> Result<Self> {
        if url.trim().is_empty() {
            return Err(FsError::new(ErrorCode::Einval).with_message("TiDB URL must not be empty"));
        }
        options.validate()?;
        let opts = Opts::from_url(url)
            .map_err(|error| db_error("parse TiDB URL", MysqlError::Url(error)))?;
        // This pool is private to the mount-rs provider. Keep the verified
        // pessimistic transaction mode on each session instead of paying
        // mysql_async's COM_RESET_CONNECTION round trip every time a pooled
        // connection is returned. The provider owns all statements run on
        // these connections and every newly created session is configured and
        // verified by the callback below.
        let pool_options = opts.pool_opts().clone().with_reset_connection(false);
        let pool = Pool::new(
            OptsBuilder::from_opts(opts)
                .pool_opts(pool_options)
                .after_connect(|connection| Box::pin(configure_pessimistic_session(connection))),
        );
        let database = Self {
            pool,
            volume_key: options.volume_key,
            durable: options.durable,
            max_block_bytes: options.max_block_bytes,
            max_namespace_bytes: options.max_namespace_bytes,
            shared_context: None,
        };

        let mut connection = database
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("connect to TiDB", error))?;
        initialize_schema(&mut connection, schema, ensure_metadata_row).await?;
        if ensure_metadata_row {
            initialize_metadata_row(&mut connection, &database.volume_key).await?;
        }
        Ok(database)
    }

    /// Complete the provider's post-commit acknowledgement round trip.
    ///
    /// This is the `flush` hook required by the storage contract, but the
    /// `SELECT 1` below is not a storage-engine fsync command. A successful
    /// round trip proves that a fresh connection can observe the server after
    /// prior acknowledged statements; committed-transaction durability still
    /// depends on the caller's TiDB/TiKV replication and sync-log policy.
    async fn acknowledgement_barrier(&self) -> Result<()> {
        let mut connection = self
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("flush TiDB acknowledgement round trip", error))?;
        connection
            .query_drop("SELECT 1")
            .await
            .map_err(|error| db_error("flush TiDB acknowledgement round trip", error))
    }

    async fn close(&self) -> Result<()> {
        if self.shared_context.is_some() {
            return Ok(());
        }
        self.pool
            .clone()
            .disconnect()
            .await
            .map_err(|error| db_error("close TiDB connection pool", error))
    }
}

async fn initialize_schema(
    connection: &mut Conn,
    schema: &str,
    ensure_metadata_row: bool,
) -> Result<()> {
    connection
        .query_drop(schema)
        .await
        .map_err(|error| db_error("initialize TiDB schema", error))?;
    if ensure_metadata_row {
        connection
            .query_drop(INODE_SCHEMA)
            .await
            .map_err(|error| db_error("initialize TiDB inode schema", error))?;
        // Additive upgrades retain the existing lease row and namespace.
        // New columns remain NULL until an explicit enrollment/migration.
        for statement in [
            "ALTER TABLE mount_rs_tidb_metadata ADD COLUMN IF NOT EXISTS delegation LONGTEXT NULL",
            "ALTER TABLE mount_rs_tidb_metadata ADD COLUMN IF NOT EXISTS write_mode VARBINARY(4) NULL",
            "ALTER TABLE mount_rs_tidb_metadata ADD COLUMN IF NOT EXISTS backing_id VARBINARY(32) NULL",
            "ALTER TABLE mount_rs_tidb_metadata ADD INDEX IF NOT EXISTS idx_mount_rs_volume_revision (volume_key, revision)",
            "ALTER TABLE mount_rs_tidb_metadata ADD INDEX IF NOT EXISTS idx_mount_rs_volume_revision_mode (volume_key, revision, write_mode)",
            "ALTER TABLE mount_rs_tidb_metadata ADD INDEX IF NOT EXISTS idx_mount_rs_inode_authority (volume_key, revision, write_mode, backing_id, owner, fence, expires)",
        ] {
            connection
                .query_drop(statement)
                .await
                .map_err(|error| db_error("upgrade TiDB metadata schema", error))?;
        }
    } else {
        connection
            .query_drop(BLOCK_AUTHORITY_SCHEMA)
            .await
            .map_err(|error| db_error("initialize TiDB block authority schema", error))?;
    }
    Ok(())
}
async fn initialize_metadata_row(connection: &mut Conn, volume_key: &str) -> Result<()> {
    connection
        .exec_drop(
            "INSERT INTO mount_rs_tidb_metadata
                        (volume_key, revision, namespace, owner, fence, expires)
                     VALUES (?, 0, NULL, NULL, 0, 0)
                     ON DUPLICATE KEY UPDATE volume_key=volume_key",
            (volume_key,),
        )
        .await
        .map_err(|error| db_error("initialize TiDB metadata row", error))?;
    Ok(())
}

struct TidbPoolInner {
    pool: Pool,
    metadata_schema: OnceCell<()>,
    block_schema: OnceCell<()>,
    closed: AtomicBool,
}

/// Explicit service-owned pool for one complete connection identity.
///
/// Credentials, database, TLS and session options are parsed once. Every
/// connection retains the same verified session hook as private stores. Store
/// close only releases its caller's lifecycle; call `close` on this context
/// after all filesystems have shut down. No process-global pool is retained.
#[derive(Clone)]
pub struct TidbPoolContext(Arc<TidbPoolInner>);
impl TidbPoolContext {
    pub fn new(url: &str, max_connections: usize) -> Result<Self> {
        if max_connections == 0 {
            return Err(
                FsError::new(ErrorCode::Einval).with_message("TiDB pool maximum must be positive")
            );
        }
        let constraints =
            mysql_async::PoolConstraints::new(0, max_connections).ok_or_else(|| {
                FsError::new(ErrorCode::Einval).with_message("TiDB pool maximum must be positive")
            })?;
        let opts = Opts::from_url(url)
            .map_err(|error| db_error("parse TiDB URL", MysqlError::Url(error)))?;
        let pool_options = opts
            .pool_opts()
            .clone()
            .with_reset_connection(false)
            .with_constraints(constraints);
        let pool = Pool::new(
            OptsBuilder::from_opts(opts)
                .pool_opts(pool_options)
                .after_connect(|connection| Box::pin(configure_pessimistic_session(connection))),
        );
        Ok(Self(Arc::new(TidbPoolInner {
            pool,
            metadata_schema: OnceCell::new(),
            block_schema: OnceCell::new(),
            closed: AtomicBool::new(false),
        })))
    }

    async fn database(&self, options: TidbStorageOptions, metadata: bool) -> Result<Database> {
        options.validate()?;
        self.require_open()?;
        let cell = if metadata {
            &self.0.metadata_schema
        } else {
            &self.0.block_schema
        };
        cell.get_or_try_init(|| async {
            let mut connection = self
                .0
                .pool
                .get_conn()
                .await
                .map_err(|e| db_error("initialize TiDB context", e))?;
            initialize_schema(
                &mut connection,
                if metadata {
                    METADATA_SCHEMA
                } else {
                    BLOCK_SCHEMA
                },
                metadata,
            )
            .await
        })
        .await?;
        self.require_open()?;
        if metadata {
            let mut connection = self
                .0
                .pool
                .get_conn()
                .await
                .map_err(|e| db_error("open TiDB context volume", e))?;
            initialize_metadata_row(&mut connection, &options.volume_key).await?;
        }
        self.require_open()?;
        Ok(Database {
            pool: self.0.pool.clone(),
            volume_key: options.volume_key,
            durable: options.durable,
            max_block_bytes: options.max_block_bytes,
            max_namespace_bytes: options.max_namespace_bytes,
            shared_context: Some(self.0.clone()),
        })
    }
    fn require_open(&self) -> Result<()> {
        if self.0.closed.load(Ordering::SeqCst) {
            Err(FsError::new(ErrorCode::Estale).with_message("TiDB pool context is closed"))
        } else {
            Ok(())
        }
    }
    pub async fn metadata(&self, options: TidbStorageOptions) -> Result<TidbMetadataStore> {
        self.database(options, true).await.map(TidbMetadataStore)
    }
    pub async fn blocks(&self, options: TidbStorageOptions) -> Result<TidbBlockStore> {
        self.database(options, false).await.map(TidbBlockStore)
    }
    /// Permanently close this service's pool, after all driver shutdowns.
    pub async fn close(&self) -> Result<()> {
        self.0.closed.store(true, Ordering::SeqCst);
        self.0
            .pool
            .clone()
            .disconnect()
            .await
            .map_err(|e| db_error("close TiDB context", e))
    }
}

/// Fenced single-writer metadata persisted in TiDB.
#[derive(Clone)]
pub struct TidbMetadataStore(Database);

impl TidbMetadataStore {
    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_with_options(url, TidbStorageOptions::default()).await
    }

    pub async fn connect_with_key(url: &str, volume_key: impl Into<String>) -> Result<Self> {
        Self::connect_with_options(url, TidbStorageOptions::new(volume_key)).await
    }

    pub async fn connect_with_options(url: &str, options: TidbStorageOptions) -> Result<Self> {
        Ok(Self(
            Database::connect(url, options, METADATA_SCHEMA, true).await?,
        ))
    }

    /// Stop a privately connected store's pool. Context-backed stores leave
    /// their shared pool alive; the service must close its TidbPoolContext
    /// after every filesystem shutdown. Private close is idempotent and
    /// subsequent operations return a backend connection error.
    pub async fn close(&self) -> Result<()> {
        self.0.close().await
    }
}

/// Immutable content-addressed blocks persisted independently from metadata.
#[derive(Clone)]
pub struct TidbBlockStore(Database);

impl TidbBlockStore {
    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_with_options(url, TidbStorageOptions::default()).await
    }

    pub async fn connect_with_key(url: &str, volume_key: impl Into<String>) -> Result<Self> {
        Self::connect_with_options(url, TidbStorageOptions::new(volume_key)).await
    }

    pub async fn connect_with_options(url: &str, options: TidbStorageOptions) -> Result<Self> {
        Ok(Self(
            Database::connect(url, options, BLOCK_SCHEMA, false).await?,
        ))
    }

    /// Close private resources. Context-backed resources remain owned by
    /// TidbPoolContext until the service explicitly closes that context.
    pub async fn close(&self) -> Result<()> {
        self.0.close().await
    }
}

#[derive(Debug)]
struct LeaseRow {
    owner: Option<String>,
    fence: u64,
    expires_at_ms: u64,
    now_ms: u64,
}

#[derive(Debug)]
struct ConcurrentRow {
    revision: u64,
    namespace_empty: bool,
    mode: Option<Vec<u8>>,
    backing: Option<Vec<u8>>,
    owner: Option<Vec<u8>>,
    fence: i64,
    expires: i64,
}

impl ConcurrentRow {
    fn mode_state(&self) -> Result<ConcurrentModeState> {
        nonnegative(self.fence, "metadata fence")?;
        nonnegative(self.expires, "metadata expiry")?;
        let fenced =
            self.owner.is_none() && self.fence == CONCURRENT_FENCE_SENTINEL && self.expires == 0;
        match (self.mode.as_deref(), self.backing.as_deref()) {
            (None, None) if self.fence != CONCURRENT_FENCE_SENTINEL => {
                Ok(ConcurrentModeState::Legacy)
            }
            (Some(mode), None) if mode == CONCURRENT_WRITE_MODE.as_bytes() && fenced => {
                Ok(ConcurrentModeState::Mrc1)
            }
            (Some(mode), Some(id)) if mode == BOUND_CONCURRENT_WRITE_MODE.as_bytes() && fenced => {
                Ok(ConcurrentModeState::Mrc2(backing_from_bytes(id)?))
            }
            (Some(mode), _) if mode == BOUND_CONCURRENT_WRITE_MODE.as_bytes() => Err(stale()),
            // MRC4 fences this legacy protocol before any legacy publication.
            // Exact inode authority is verified separately by inode_mode_state.
            (Some(b"MRC4"), _) => Err(stale()),
            _ => Err(backend_error(
                "TiDB concurrent mode, backing ID, and fence disagree",
            )),
        }
    }

    fn is_pristine(&self) -> bool {
        self.mode.is_none()
            && self.backing.is_none()
            && self.revision == 0
            && self.namespace_empty
            && self.owner.is_none()
            && self.fence == 0
            && self.expires == 0
    }
}

type ConcurrentSqlRow = (
    i64,
    bool,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    i64,
    i64,
);

async fn concurrent_row<C: Queryable>(
    connection: &mut C,
    volume_key: &str,
) -> Result<ConcurrentRow> {
    let row: Option<ConcurrentSqlRow> = connection
        .exec_first(
            "SELECT revision, namespace IS NULL, write_mode, backing_id, owner, fence, expires
         FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (volume_key,),
        )
        .await
        .map_err(|error| db_error("read TiDB concurrent mode", error))?;
    let (revision, namespace_empty, mode, backing, owner, fence, expires) =
        row.ok_or_else(|| backend_error("TiDB metadata row is missing"))?;
    Ok(ConcurrentRow {
        revision: nonnegative(revision, "metadata revision")?,
        namespace_empty,
        mode,
        backing,
        owner,
        fence,
        expires,
    })
}

fn concurrent_busy(operation: &str) -> FsError {
    FsError::new(ErrorCode::Ebusy).with_syscall(operation)
}

async fn configure_pessimistic_session(
    connection: &mut Conn,
) -> std::result::Result<(), MysqlError> {
    // A server's GLOBAL autocommit=0 must not turn an acknowledged statement
    // publication into a connection-local, uncommitted transaction.
    connection.query_drop("SET SESSION autocommit=1").await?;
    let autocommit: Option<i64> = connection
        .query_first("SELECT @@SESSION.autocommit")
        .await?;
    require_autocommit(autocommit).map_err(|error| MysqlError::Other(Box::new(error)))?;
    // TiDB Cloud Starter/Essential may expose tidb_txn_mode as a read-only
    // variable. In that case the provider must verify the effective value,
    // never infer it from the deployment name.
    match connection
        .query_drop("SET SESSION tidb_txn_mode='pessimistic'")
        .await
    {
        Ok(()) => {}
        Err(error) if is_read_only_txn_mode(&error) => {}
        Err(error) => return Err(error),
    }
    let mode: Option<String> = connection
        .query_first("SELECT @@SESSION.tidb_txn_mode")
        .await?;
    require_pessimistic_mode(mode.as_deref())
        .map_err(|error| MysqlError::Other(Box::new(error)))?;
    connection
        .query_drop("SET SESSION transaction_isolation='REPEATABLE-READ'")
        .await?;
    let isolation: Option<String> = connection
        .query_first("SELECT @@SESSION.transaction_isolation")
        .await?;
    require_repeatable_read_isolation(isolation.as_deref())
        .map_err(|error| MysqlError::Other(Box::new(error)))
}

async fn begin_pessimistic(connection: &mut Conn) -> Result<Transaction<'_>> {
    // Sessions are configured and verified once when the pool creates them.
    // Keep the driver's transaction guard so cancellation and early-return
    // paths are rolled back before a pooled connection is reused.
    let options = TxOpts::default();
    connection
        .start_transaction(options)
        .await
        .map_err(|error| db_error("begin TiDB transaction", error))
}

fn require_pessimistic_mode(mode: Option<&str>) -> Result<()> {
    if mode.is_some_and(|value| value.eq_ignore_ascii_case("pessimistic")) {
        Ok(())
    } else {
        Err(backend_error(
            "TiDB session must use pessimistic transactions",
        ))
    }
}

fn require_autocommit(autocommit: Option<i64>) -> Result<()> {
    if autocommit == Some(1) {
        Ok(())
    } else {
        Err(backend_error("TiDB session must enable autocommit"))
    }
}

fn require_repeatable_read_isolation(isolation: Option<&str>) -> Result<()> {
    if isolation.is_some_and(|value| {
        value.eq_ignore_ascii_case("repeatable-read")
            || value.eq_ignore_ascii_case("repeatable read")
    }) {
        Ok(())
    } else {
        Err(backend_error(
            "TiDB session must use repeatable-read isolation",
        ))
    }
}

async fn rollback_and<T>(transaction: Transaction<'_>, error: FsError) -> Result<T> {
    let _ = transaction.rollback().await;
    Err(error)
}

fn ambiguous_commit_error(operation: &str, error: &MysqlError) -> FsError {
    // A connection failure after COMMIT was sent cannot distinguish a
    // committed transaction from a rolled-back one. Keep this separate from
    // statement-conflict mapping so callers cannot safely replay the whole
    // operation just because the server error code is normally retryable.
    FsError::backend(format!(
        "TiDB {operation} commit outcome is unknown: {}",
        mysql_error_detail(error)
    ))
}

async fn commit(transaction: Transaction<'_>, operation: &str) -> Result<()> {
    transaction
        .commit()
        .await
        .map_err(|error| ambiguous_commit_error(operation, &error))
}

async fn changed_query<C, P>(
    connection: &mut C,
    statement: &str,
    params: P,
    operation: &str,
) -> Result<u64>
where
    C: Queryable,
    P: Into<Params> + Send,
{
    let result = connection
        .exec_iter(statement, params)
        .await
        .map_err(|error| db_error(operation, error))?;
    let changed = result.affected_rows();
    result
        .drop_result()
        .await
        .map_err(|error| db_error(operation, error))?;
    Ok(changed)
}

fn autocommit_error(operation: &str, error: MysqlError) -> FsError {
    // TiDB can report a statement-level write conflict before the
    // autocommit transaction is committed, so preserve the existing retryable
    // conflict classification for that server response. A connection or
    // other acknowledgement error cannot distinguish commit from rollback;
    // fail closed instead of allowing the caller to replay the publication.
    if is_retryable_conflict(&error) {
        db_error(operation, error)
    } else {
        ambiguous_commit_error(operation, &error)
    }
}

async fn changed_autocommit_query<P>(
    connection: &mut Conn,
    statement: &str,
    params: P,
    operation: &str,
) -> Result<u64>
where
    P: Into<Params> + Send,
{
    let result = connection
        .exec_iter(statement, params)
        .await
        .map_err(|error| autocommit_error(operation, error))?;
    let changed = result.affected_rows();
    result
        .drop_result()
        .await
        .map_err(|error| autocommit_error(operation, error))?;
    Ok(changed)
}

async fn locked_lease_row<C: Queryable>(
    connection: &mut C,
    volume_key: &str,
) -> Result<Option<LeaseRow>> {
    let row: Option<(Option<String>, i64, i64, i64)> = connection
        .exec_first(
            format!(
                "SELECT owner, fence, expires, {NOW_MS}
                 FROM mount_rs_tidb_metadata
                 WHERE volume_key=?
                 FOR UPDATE"
            ),
            (volume_key,),
        )
        .await
        .map_err(|error| db_error("read TiDB metadata lease", error))?;
    row.map(|(owner, fence, expires_at_ms, now_ms)| {
        Ok(LeaseRow {
            owner,
            fence: nonnegative(fence, "metadata fence")?,
            expires_at_ms: nonnegative(expires_at_ms, "metadata expiry")?,
            now_ms: nonnegative(now_ms, "TiDB provider clock")?,
        })
    })
    .transpose()
}

async fn locked_publish_row<C: Queryable>(
    connection: &mut C,
    volume_key: &str,
) -> Result<Option<(u64, LeaseRow)>> {
    let row: Option<(i64, Option<String>, i64, i64, i64)> = connection
        .exec_first(
            format!(
                "SELECT revision, owner, fence, expires, {NOW_MS}
                 FROM mount_rs_tidb_metadata
                 WHERE volume_key=?
                 FOR UPDATE"
            ),
            (volume_key,),
        )
        .await
        .map_err(|error| db_error("read TiDB metadata publication state", error))?;
    row.map(|(revision, owner, fence, expires_at_ms, now_ms)| {
        Ok((
            nonnegative(revision, "metadata revision")?,
            LeaseRow {
                owner,
                fence: nonnegative(fence, "metadata fence")?,
                expires_at_ms: nonnegative(expires_at_ms, "metadata expiry")?,
                now_ms: nonnegative(now_ms, "TiDB provider clock")?,
            },
        ))
    })
    .transpose()
}

fn ttl_ms(ttl: Duration) -> Result<u64> {
    let value = u64::try_from(ttl.as_millis())
        .map_err(|_| FsError::new(ErrorCode::Eoverflow).with_message("TiDB lease TTL overflow"))?;
    if value == 0 || value > i64::MAX as u64 {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("TiDB lease TTL must fit a positive signed millisecond value"));
    }
    Ok(value)
}

fn nonnegative(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value).map_err(|_| backend_error(format!("invalid TiDB {field}")))
}

fn signed(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        FsError::new(ErrorCode::Eoverflow).with_message(format!("TiDB {field} overflows BIGINT"))
    })
}

fn expiry(now_ms: u64, ttl_ms: u64) -> Result<u64> {
    now_ms.checked_add(ttl_ms).ok_or_else(|| {
        FsError::new(ErrorCode::Eoverflow).with_message("TiDB lease expiry overflow")
    })
}

fn stale() -> FsError {
    FsError::new(ErrorCode::Estale).with_syscall("TiDB metadata lease")
}

fn backing_from_bytes(bytes: &[u8]) -> Result<ConcurrentBackingId> {
    let text = std::str::from_utf8(bytes).map_err(|_| stale())?;
    ConcurrentBackingId::from_hex(text).map_err(|_| stale())
}

fn revision_conflict() -> FsError {
    FsError::new(ErrorCode::Eagain).with_syscall("TiDB publish metadata")
}

fn validate_scope(value: &str, what: &str) -> Result<()> {
    if value.is_empty() || value.chars().count() > MAX_SCOPE_CHARS || value.contains('\0') {
        return Err(FsError::new(ErrorCode::Einval).with_message(format!(
            "{what} must be non-empty, at most {MAX_SCOPE_CHARS} characters, and contain no NUL"
        )));
    }
    Ok(())
}

fn validate_block_id(id: &BlockId) -> Result<()> {
    let bytes = id.0.as_bytes();
    if bytes.len() != BLOCK_ID_LENGTH
        || bytes[0] != BLOCK_ID_PREFIX
        || !bytes[1..]
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(FsError::new(ErrorCode::Einval)
            .with_syscall("TiDB block ID")
            .with_message("invalid TiDB SHA-256 block ID"));
    }
    Ok(())
}

fn block_id(bytes: &[u8]) -> BlockId {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(BLOCK_ID_LENGTH);
    encoded.push('b');
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    BlockId(encoded)
}

fn db_error(operation: &str, error: MysqlError) -> FsError {
    let detail = mysql_error_detail(&error);
    if is_retryable_conflict(&error) {
        return FsError::new(ErrorCode::Eagain)
            .with_syscall(operation)
            .with_message(format!(
                "TiDB transaction conflict; retry the complete operation: {detail}"
            ));
    }
    backend_error(format!("TiDB {operation}: {detail}"))
}

fn mysql_error_detail(error: &MysqlError) -> String {
    // mysql_async's URL error variants describe the rejected URL parameter or
    // parse operation. Do not surface that detail: a malformed connection URL
    // may contain credentials, and the filesystem error has no safe reason to
    // repeat them. Other driver/server errors do not contain the input URL.
    if matches!(error, MysqlError::Url(_)) {
        "connection URL was rejected".to_owned()
    } else {
        error.to_string()
    }
}

fn is_retryable_conflict(error: &MysqlError) -> bool {
    let MysqlError::Server(server) = error else {
        return false;
    };
    matches!(server.code, 1205 | 1213 | 9006 | 9007)
        || server
            .message
            .to_ascii_lowercase()
            .contains("write conflict")
        || server
            .message
            .to_ascii_lowercase()
            .contains("try again later")
}

fn is_read_only_txn_mode(error: &MysqlError) -> bool {
    let MysqlError::Server(server) = error else {
        return false;
    };
    server.code == 1238
        && server
            .message
            .to_ascii_lowercase()
            .contains("tidb_txn_mode")
}

fn delegation_legacy_enrollment_valid(backing: Option<&[u8]>, fence: i64) -> bool {
    backing.is_none() && (0..CONCURRENT_FENCE_SENTINEL).contains(&fence)
}
fn decode_delegation_authority(
    mode: Option<&[u8]>,
    backing: Option<&[u8]>,
    raw: Option<&[u8]>,
) -> Result<Option<DelegationState>> {
    if mode != Some(b"MRC3") {
        return if raw.is_some() {
            Err(stale())
        } else {
            Ok(None)
        };
    }
    let backing = ConcurrentBackingId::from_bytes(
        backing.ok_or_else(stale)?.try_into().map_err(|_| stale())?,
    )
    .map_err(|_| stale())?;
    let state: DelegationState =
        serde_json::from_slice(raw.ok_or_else(stale)?).map_err(backend_error)?;
    if state.backing != backing {
        return Err(stale());
    }
    Ok(Some(state))
}

#[derive(Clone)]
enum DelegationCommand {
    Inspect,
    Prepare(ConcurrentBackingId, u64),
    Checkout(CheckoutRequest),
    Publish(DelegatedPublish, Namespace),
    Checkin(DelegatedCheckin),
    Recover(DelegatedRecovery),
}
struct DelegationResult {
    state: Option<DelegationState>,
    grant: Option<DirectoryGrant>,
    revision: u64,
}
impl TidbMetadataStore {
    async fn delegation_transaction(&self, command: DelegationCommand) -> Result<DelegationResult> {
        let inspect = matches!(command, DelegationCommand::Inspect);
        let mut conn = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|e| db_error("connect TiDB delegation", e))?;
        let mut tx = begin_pessimistic(&mut conn).await?;
        #[allow(clippy::type_complexity)]
        let row: Option<(i64,Option<String>,Option<Vec<u8>>,Option<Vec<u8>>,Option<Vec<u8>>,i64,i64,Option<String>)> = tx.exec_first(
            "SELECT revision, namespace, write_mode, backing_id, owner, fence, expires, delegation FROM mount_rs_tidb_metadata WHERE volume_key=? FOR UPDATE",(&self.0.volume_key,)).await.map_err(|e| db_error("lock TiDB delegation",e))?;
        let (revision, namespace, mode, backing, owner, fence, expires, raw) =
            row.ok_or_else(|| backend_error("missing TiDB delegation row"))?;
        let mut revision = nonnegative(revision, "delegation revision")?;
        let backing_binary = backing
            .as_deref()
            .map(backing_from_bytes)
            .transpose()?
            .map(|b| b.as_bytes());
        let mut state = decode_delegation_authority(
            mode.as_deref(),
            backing_binary.as_ref().map(|b| b.as_slice()),
            raw.as_deref().map(str::as_bytes),
        )?;
        if matches!(command, DelegationCommand::Inspect) && state.is_none() {
            commit(tx, "inspect delegation").await?;
            return Ok(DelegationResult {
                state: None,
                grant: None,
                revision,
            });
        }
        let namespace = namespace.ok_or_else(|| {
            FsError::new(ErrorCode::Ebusy)
                .with_message("initialize namespace before delegation enrollment")
        })?;
        profile::add(Event::NamespaceReturned, namespace.len() as u64);
        let mut ns: Namespace = serde_json::from_str(&namespace).map_err(backend_error)?;
        ns.validate()?;
        let mut grant = None;
        let mut changed_ns = false;
        if let DelegationCommand::Prepare(requested, expected) = command {
            if revision != expected {
                return Err(revision_conflict());
            }
            if owner.is_some() || expires != 0 {
                return Err(FsError::new(ErrorCode::Ebusy));
            }
            if let Some(current) = &state {
                if current.backing != requested || fence != CONCURRENT_FENCE_SENTINEL {
                    return Err(stale());
                }
            } else {
                let valid = match mode.as_deref() {
                    None => delegation_legacy_enrollment_valid(backing.as_deref(), fence),
                    Some(b"MRC2") => {
                        backing_binary == Some(requested.as_bytes())
                            && fence == CONCURRENT_FENCE_SENTINEL
                    }
                    _ => false,
                };
                if !valid {
                    return Err(stale());
                }
                state = Some(DelegationState::new(requested));
            }
        } else {
            let current = state
                .as_mut()
                .ok_or_else(|| FsError::new(ErrorCode::Enotsup))?;
            if owner.is_some() || fence != CONCURRENT_FENCE_SENTINEL || expires != 0 {
                return Err(stale());
            }
            current.validate(&ns)?;
            let requested = match &command {
                DelegationCommand::Checkout(r) => Some(r.backing),
                DelegationCommand::Publish(r, _) => Some(r.backing),
                DelegationCommand::Checkin(r) => Some(r.backing),
                DelegationCommand::Recover(r) => Some(r.backing),
                _ => None,
            };
            if requested.is_some_and(|b| b != current.backing) {
                return Err(stale());
            }
            match command {
                DelegationCommand::Checkout(r) => {
                    grant = Some(current.checkout(&ns, r.root, &r.owner)?)
                }
                DelegationCommand::Publish(r, new) => {
                    if current
                        .grants
                        .get(&r.token.root)
                        .is_none_or(|g| g.token != r.token)
                    {
                        return Err(stale());
                    }
                    if revision != r.expected_revision {
                        return Err(revision_conflict());
                    }
                    current.authorize_publish(&ns, &new, &r.token)?;
                    ns = new;
                    changed_ns = true;
                }
                DelegationCommand::Checkin(r) => {
                    if !current.retired.contains(&r.token) && revision != r.expected_revision {
                        return Err(revision_conflict());
                    }
                    current.checkin(&r.token, &ns)?;
                }
                DelegationCommand::Recover(r) => {
                    let before = ns.clone();
                    current.recover(r.root, r.expected_fence, &mut ns)?;
                    changed_ns = before.nodes != ns.nodes;
                }
                DelegationCommand::Inspect => {}
                DelegationCommand::Prepare(_, _) => unreachable!(),
            }
        }
        let state = state.unwrap();
        state.validate(&ns)?;
        if changed_ns {
            revision = revision
                .checked_add(1)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        }
        let json = serde_json::to_string(&ns).map_err(backend_error)?;
        profile::add(Event::NamespaceSerialized, json.len() as u64);
        let authority = serde_json::to_string(&state).map_err(backend_error)?;
        if json
            .len()
            .checked_add(authority.len())
            .is_none_or(|n| n > self.0.max_namespace_bytes)
        {
            return Err(FsError::new(ErrorCode::Efbig));
        }
        if !inspect {
            tx.exec_drop("UPDATE mount_rs_tidb_metadata SET revision=?,namespace=?,write_mode='MRC3',backing_id=?,owner=NULL,fence=?,expires=0,delegation=? WHERE volume_key=?",
              (signed(revision,"delegation revision")?,json,state.backing.to_hex(),CONCURRENT_FENCE_SENTINEL,authority,&self.0.volume_key)).await.map_err(|e|db_error("write TiDB delegation",e))?;
        }
        commit(tx, "delegation").await?;
        Ok(DelegationResult {
            state: Some(state),
            grant,
            revision,
        })
    }
}

async fn begin_inode_write(connection: &mut Conn) -> Result<Transaction<'_>> {
    // Locking reads may wait across another structural commit. TiDB must then
    // read root authority from a fresh statement snapshot, not start_ts.
    let mut options = TxOpts::default();
    options.with_isolation_level(IsolationLevel::ReadCommitted);
    connection
        .start_transaction(options)
        .await
        .map_err(|e| db_error("begin TiDB inode publication", e))
}

fn inode_authority(
    row: &ConcurrentRow,
    expected: Option<ConcurrentBackingId>,
) -> Result<InodeModeState> {
    if row.mode.as_deref() != Some(b"MRC4")
        || row.owner.is_some()
        || row.fence != CONCURRENT_FENCE_SENTINEL
        || row.expires != 0
        || row.namespace_empty
        || row.revision == 0
    {
        return Err(stale());
    }
    let backing = backing_from_bytes(row.backing.as_deref().ok_or_else(stale)?)?;
    if expected.is_some_and(|id| id != backing) {
        return Err(stale());
    }
    Ok(InodeModeState {
        backing,
        structural_generation: row.revision,
    })
}

type InodeAuthoritySqlRow = (
    i64,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    Option<Vec<u8>>,
    i64,
    i64,
);
fn compact_inode_authority(
    row: InodeAuthoritySqlRow,
    backing: Option<ConcurrentBackingId>,
) -> Result<InodeModeState> {
    let (revision, mode, actual_backing, owner, fence, expires) = row;
    // MRC4 enrollment atomically guarantees initialized namespace membership.
    // Hot reads audit compact authority and selected guards; complete snapshots
    // additionally audit namespace presence and cross-inode structure.
    inode_authority(
        &ConcurrentRow {
            revision: nonnegative(revision, "metadata revision")?,
            namespace_empty: false,
            mode,
            backing: actual_backing,
            owner,
            fence,
            expires,
        },
        backing,
    )
}
async fn compact_inode_authority_row<C: Queryable>(
    conn: &mut C,
    volume: &str,
    backing: ConcurrentBackingId,
) -> Result<InodeModeState> {
    let row: Option<InodeAuthoritySqlRow> =
        conn.exec_first(INODE_AUTHORITY_SQL, (volume,))
            .await
            .map_err(|e| db_error("read compact TiDB inode authority", e))?;
    compact_inode_authority(row.ok_or_else(stale)?, Some(backing))
}

type InodeSqlRow = (i64, i64, i64, String);
fn decode_inode(row: InodeSqlRow, generation: u64) -> Result<(InodeId, LoadedInode)> {
    let (inode, actual_generation, revision, json) = row;
    let inode = nonnegative(inode, "inode ID")?;
    if inode == 0 || nonnegative(actual_generation, "inode generation")? != generation {
        return Err(stale());
    }
    let node: NodeMetadata = serde_json::from_str(&json).map_err(backend_error)?;
    validate_node_kind(&node)?;
    if node.stats.ino != inode {
        return Err(backend_error("TiDB inode identity disagrees"));
    }
    Ok((
        inode,
        LoadedInode {
            version: InodeVersion {
                structural_generation: generation,
                inode_revision: nonnegative(revision, "inode revision")?,
            },
            node,
        },
    ))
}

async fn replace_inode_guards(
    tx: &mut Transaction<'_>,
    volume: &str,
    generation: u64,
    namespace: &Namespace,
) -> Result<()> {
    tx.exec_drop(
        "DELETE FROM mount_rs_tidb_inodes WHERE volume_key=?",
        (volume,),
    )
    .await
    .map_err(|e| db_error("replace TiDB inode guards", e))?;
    for (&inode, node) in &namespace.nodes {
        let json = serde_json::to_string(node).map_err(backend_error)?;
        profile::add(Event::InodeSerialized, json.len() as u64);
        tx.exec_drop("INSERT INTO mount_rs_tidb_inodes (volume_key,inode,generation,revision,node) VALUES (?,?,?,0,?)",
            (volume, signed(inode,"inode ID")?, signed(generation,"inode generation")?, json)).await
            .map_err(|e| db_error("insert TiDB inode guard",e))?;
    }
    Ok(())
}

#[async_trait]
impl MetadataStore for TidbMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    fn publish_includes_flush_barrier(&self) -> bool {
        // A successful autocommit DML acknowledgement includes the
        // single-statement TiDB commit. Lost acknowledgements fail closed as
        // ambiguous, and explicit syncfs retains the connection probe.
        true
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("load TiDB metadata", error))?;
        let row: Option<(i64, Option<String>, Option<Vec<u8>>)> = connection
            .exec_first(
                "SELECT revision, namespace, write_mode
                 FROM mount_rs_tidb_metadata
                 WHERE volume_key=?",
                (&self.0.volume_key,),
            )
            .await
            .map_err(|error| db_error("load TiDB metadata", error))?;
        let Some((revision, namespace, mode)) = row else {
            return Err(backend_error("TiDB metadata row is missing"));
        };
        if mode.as_deref() == Some(b"MRC4") {
            return Err(stale());
        }
        let revision = nonnegative(revision, "metadata revision")?;
        let namespace = namespace
            .map(|json| {
                profile::add(Event::NamespaceReturned, json.len() as u64);
                serde_json::from_str(&json).map_err(backend_error)
            })
            .transpose()?;
        Ok(LoadedMetadata {
            revision,
            namespace,
        })
    }

    async fn inode_mode_state(&self) -> Result<Option<InodeModeState>> {
        let mut conn = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|e| db_error("read TiDB inode mode", e))?;
        let row: Option<InodeAuthoritySqlRow> = conn
            .exec_first(INODE_AUTHORITY_SQL, (&self.0.volume_key,))
            .await
            .map_err(|e| db_error("read TiDB inode mode", e))?;
        let row = row.ok_or_else(stale)?;
        if row.1.as_deref() != Some(b"MRC4") {
            return Ok(None);
        }
        compact_inode_authority(row, None).map(Some)
    }

    async fn prepare_inode_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        signed(expected_revision, "metadata revision")?;
        let generation = expected_revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let generation_sql = signed(generation, "structural generation")?;
        let mut conn = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|e| db_error("prepare TiDB inode mode", e))?;
        let mut tx = begin_inode_write(&mut conn).await?;
        locked_lease_row(&mut tx, &self.0.volume_key)
            .await?
            .ok_or_else(stale)?;
        let row = concurrent_row(&mut tx, &self.0.volume_key).await?;
        if row.revision != expected_revision
            || row.revision == 0
            || row.namespace_empty
            || row.mode_state()? != ConcurrentModeState::Mrc2(backing)
        {
            return rollback_and(tx, stale()).await;
        }
        let json: String = tx
            .exec_first(
                "SELECT namespace FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&self.0.volume_key,),
            )
            .await
            .map_err(|e| db_error("read TiDB inode enrollment namespace", e))?
            .ok_or_else(stale)?;
        let namespace: Namespace = serde_json::from_str(&json).map_err(backend_error)?;
        namespace.validate()?;
        let wrapped = encode_inode_namespace(&namespace)?;
        profile::add(Event::NamespaceSerialized, wrapped.len() as u64);
        if wrapped.len() > self.0.max_namespace_bytes {
            return rollback_and(tx, FsError::new(ErrorCode::Efbig)).await;
        }
        replace_inode_guards(&mut tx, &self.0.volume_key, generation, &namespace).await?;
        tx.exec_drop(
            "UPDATE mount_rs_tidb_metadata SET write_mode='MRC4',revision=?,namespace=? WHERE volume_key=?",
            (generation_sql,wrapped,&self.0.volume_key),
        )
        .await
        .map_err(|e| db_error("enroll TiDB inode mode", e))?;
        commit(tx, "enroll inode mode").await
    }

    async fn load_inode_snapshot(
        &self,
        backing: ConcurrentBackingId,
    ) -> Result<InodeMetadataSnapshot> {
        let mut conn = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|e| db_error("load TiDB inode snapshot", e))?;
        let mut tx = begin_pessimistic(&mut conn).await?;
        let mode = inode_authority(
            &concurrent_row(&mut tx, &self.0.volume_key).await?,
            Some(backing),
        )?;
        let json: String = tx
            .exec_first(
                "SELECT namespace FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&self.0.volume_key,),
            )
            .await
            .map_err(|e| db_error("load TiDB inode snapshot", e))?
            .ok_or_else(stale)?;
        let mut namespace = decode_inode_namespace(json.as_bytes())?;
        let rows: Vec<InodeSqlRow> = tx.exec("SELECT inode,generation,revision,node FROM mount_rs_tidb_inodes WHERE volume_key=? ORDER BY inode",(&self.0.volume_key,)).await
            .map_err(|e| db_error("load TiDB inode guards",e))?;
        let mut inode_revisions = BTreeMap::new();
        for row in rows {
            profile::add(Event::InodeReturned, row.3.len() as u64);
            let (inode, loaded) = decode_inode(row, mode.structural_generation)?;
            if !namespace.nodes.contains_key(&inode) {
                return Err(stale());
            }
            namespace.nodes.insert(inode, loaded.node);
            inode_revisions.insert(inode, loaded.version.inode_revision);
        }
        let snapshot = InodeMetadataSnapshot {
            structural_generation: mode.structural_generation,
            namespace,
            inode_revisions,
        };
        snapshot.validate()?;
        tx.rollback()
            .await
            .map_err(|e| db_error("finish TiDB inode snapshot", e))?;
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
        let inode_sql = signed(inode, "inode ID")?;
        let (known_generation, known_revision) = known
            .and_then(|version| {
                Some((
                    i64::try_from(version.structural_generation).ok()?,
                    i64::try_from(version.inode_revision).ok()?,
                ))
            })
            .unwrap_or((-1, -1));
        let mut conn = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|e| db_error("load TiDB inode", e))?;
        type ReadRow = (
            i64,
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            Option<Vec<u8>>,
            i64,
            i64,
            Option<i64>,
            Option<i64>,
            Option<i64>,
            Option<String>,
        );
        // One statement snapshot checks compact authority and the selected
        // guard. CASE suppresses unchanged JSON on the MySQL wire entirely.
        let row: Option<ReadRow> = conn
            .exec_first(
                INODE_READ_SQL,
                (
                    known_generation,
                    known_revision,
                    inode_sql,
                    &self.0.volume_key,
                ),
            )
            .await
            .map_err(|e| db_error("load TiDB inode", e))?;
        let (
            revision,
            mode,
            actual_backing,
            owner,
            fence,
            expires,
            actual_inode,
            generation,
            inode_revision,
            json,
        ) = row.ok_or_else(stale)?;
        let authority = compact_inode_authority(
            (revision, mode, actual_backing, owner, fence, expires),
            Some(backing),
        )?;
        let actual_inode = actual_inode.ok_or_else(stale)?;
        let generation = generation.ok_or_else(stale)?;
        let inode_revision = inode_revision.ok_or_else(stale)?;
        if actual_inode != inode_sql
            || nonnegative(generation, "inode generation")? != authority.structural_generation
        {
            return Err(stale());
        }
        let version = InodeVersion {
            structural_generation: authority.structural_generation,
            inode_revision: nonnegative(inode_revision, "inode revision")?,
        };
        if known == Some(version) {
            return Ok(None);
        }
        let json = json.ok_or_else(stale)?;
        profile::add(Event::InodeReturned, json.len() as u64);
        let (_, loaded) = decode_inode(
            (actual_inode, generation, inode_revision, json),
            authority.structural_generation,
        )?;
        Ok(Some(loaded))
    }

    async fn publish_inode_if_version(
        &self,
        backing: ConcurrentBackingId,
        inode: InodeId,
        expected: InodeVersion,
        node: NodeMetadata,
    ) -> Result<InodeVersion> {
        let inode_sql = signed(inode, "inode ID")?;
        signed(expected.structural_generation, "structural generation")?;
        signed(expected.inode_revision, "inode revision")?;
        let next = expected
            .inode_revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let next_sql = signed(next, "inode revision")?;
        let json = serde_json::to_string(&node).map_err(backend_error)?;
        profile::add(Event::InodeSerialized, json.len() as u64);
        if json.len() > self.0.max_namespace_bytes {
            return Err(FsError::new(ErrorCode::Efbig));
        }
        let mut conn = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|e| db_error("publish TiDB inode", e))?;
        // READ COMMITTED makes the nonlocking root read fresh after waiting for
        // the selected guard. File writers never lock or write the root key.
        let mut tx = begin_inode_write(&mut conn).await?;
        let row: Option<InodeSqlRow> = tx.exec_first("SELECT inode,generation,revision,node FROM mount_rs_tidb_inodes WHERE volume_key=? AND inode=? FOR UPDATE",(&self.0.volume_key,inode_sql)).await
            .map_err(|e| db_error("lock TiDB inode guard",e))?;
        let mode = compact_inode_authority_row(&mut tx, &self.0.volume_key, backing).await?;
        // A structural unlink may have removed the guard while the caller
        // flushed blocks. Its older generation is a proven no-commit conflict.
        // A missing guard under the same generation remains corruption.
        if mode.structural_generation != expected.structural_generation {
            return rollback_and(tx, FsError::new(ErrorCode::Eagain)).await;
        }
        let row = row.ok_or_else(stale)?;
        profile::add(Event::InodeReturned, row.3.len() as u64);
        let (_, original) = decode_inode(row, mode.structural_generation)?;
        if original.version != expected {
            return rollback_and(tx, FsError::new(ErrorCode::Eagain)).await;
        }
        validate_inode_publication(inode, &original.node, &node)?;
        tx.exec_drop(
            "UPDATE mount_rs_tidb_inodes SET revision=?,node=? WHERE volume_key=? AND inode=?",
            (next_sql, json, &self.0.volume_key, inode_sql),
        )
        .await
        .map_err(|e| db_error("publish TiDB inode guard", e))?;
        commit(tx, "publish inode").await?;
        Ok(InodeVersion {
            inode_revision: next,
            ..expected
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
        signed(expected_generation, "structural generation")?;
        for (&inode, &revision) in expected_inode_revisions {
            signed(inode, "inode ID")?;
            signed(revision, "inode revision")?;
        }
        let next = expected_generation
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let next_sql = signed(next, "structural generation")?;
        let json = encode_inode_namespace(&namespace)?;
        profile::add(Event::NamespaceSerialized, json.len() as u64);
        if json.len() > self.0.max_namespace_bytes {
            return Err(FsError::new(ErrorCode::Efbig));
        }
        let mut conn = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|e| db_error("publish TiDB structure", e))?;
        let mut tx = begin_inode_write(&mut conn).await?;
        locked_lease_row(&mut tx, &self.0.volume_key)
            .await?
            .ok_or_else(stale)?;
        // Every existing inode is locked, including guards for deleted inodes.
        // Thus no earlier or later file publication can be folded away.
        let rows: Vec<InodeSqlRow> = tx.exec("SELECT inode,generation,revision,node FROM mount_rs_tidb_inodes WHERE volume_key=? ORDER BY inode FOR UPDATE",(&self.0.volume_key,)).await
            .map_err(|e| db_error("lock TiDB structural guards",e))?;
        let mode = inode_authority(
            &concurrent_row(&mut tx, &self.0.volume_key).await?,
            Some(backing),
        )?;
        if mode.structural_generation != expected_generation {
            return rollback_and(tx, FsError::new(ErrorCode::Eagain)).await;
        }
        let mut actual = BTreeMap::new();
        let mut authoritative_nodes = BTreeMap::new();
        for row in rows {
            profile::add(Event::InodeReturned, row.3.len() as u64);
            let (inode, loaded) = decode_inode(row, expected_generation)?;
            actual.insert(inode, loaded.version.inode_revision);
            authoritative_nodes.insert(inode, loaded.node);
        }
        if &actual != expected_inode_revisions {
            return rollback_and(tx, FsError::new(ErrorCode::Eagain)).await;
        }
        // Detect missing known guards against the old structural membership.
        let old_json: String = tx
            .exec_first(
                "SELECT namespace FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&self.0.volume_key,),
            )
            .await
            .map_err(|e| db_error("read TiDB structural membership", e))?
            .ok_or_else(stale)?;
        let mut old = decode_inode_namespace(old_json.as_bytes())?;
        if !old.nodes.keys().eq(actual.keys()) {
            return rollback_and(tx, stale()).await;
        }
        old.nodes = authoritative_nodes;
        old.validate()?;
        replace_inode_guards(&mut tx, &self.0.volume_key, next, &namespace).await?;
        tx.exec_drop(
            "UPDATE mount_rs_tidb_metadata SET revision=?,namespace=? WHERE volume_key=?",
            (next_sql, json, &self.0.volume_key),
        )
        .await
        .map_err(|e| db_error("publish TiDB structure", e))?;
        commit(tx, "publish structure").await?;
        Ok(next)
    }

    async fn delegation_state(&self) -> Result<Option<DelegationState>> {
        Ok(self
            .delegation_transaction(DelegationCommand::Inspect)
            .await?
            .state)
    }
    async fn prepare_delegated_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        self.delegation_transaction(DelegationCommand::Prepare(backing, expected_revision))
            .await?;
        Ok(())
    }
    async fn checkout(&self, request: &CheckoutRequest) -> Result<DirectoryGrant> {
        self.delegation_transaction(DelegationCommand::Checkout(request.clone()))
            .await?
            .grant
            .ok_or_else(|| backend_error("missing grant"))
    }
    async fn publish_delegated(
        &self,
        request: &DelegatedPublish,
        namespace: Namespace,
    ) -> Result<u64> {
        Ok(self
            .delegation_transaction(DelegationCommand::Publish(request.clone(), namespace))
            .await?
            .revision)
    }
    async fn checkin(&self, request: &DelegatedCheckin) -> Result<()> {
        self.delegation_transaction(DelegationCommand::Checkin(request.clone()))
            .await?;
        Ok(())
    }
    async fn recover(&self, request: &DelegatedRecovery) -> Result<()> {
        self.delegation_transaction(DelegationCommand::Recover(request.clone()))
            .await?;
        Ok(())
    }

    async fn load_if_changed(&self, known_revision: u64) -> Result<Option<LoadedMetadata>> {
        // A zero revision is uninitialized, not a validated namespace. TiDB
        // revisions are signed BIGINTs; an unrepresentable caller revision
        // cannot match and must conservatively load the current namespace.
        let Ok(known_revision) = i64::try_from(known_revision) else {
            return self.load().await.map(Some);
        };
        if known_revision == 0 {
            return self.load().await.map(Some);
        }
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("conditionally load TiDB metadata", error))?;
        // The secondary index covers this header query without fetching the
        // primary row's large namespace value from TiKV. A changed revision
        // goes through the existing validated full load on a fresh snapshot.
        let revision: Option<(i64, Option<Vec<u8>>)> = connection
            .exec_first(REVISION_PROBE_SQL, (&self.0.volume_key,))
            .await
            .map_err(|error| db_error("conditionally load TiDB metadata", error))?;
        let Some((revision, mode)) = revision else {
            return Err(backend_error("TiDB metadata row is missing"));
        };
        let revision = nonnegative(revision, "metadata revision")?;
        if mode.as_deref() == Some(b"MRC4") {
            return Err(stale());
        }
        if revision == known_revision as u64 {
            return Ok(None);
        }
        drop(connection);
        self.load().await.map(Some)
    }

    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("read TiDB concurrent mode", error))?;
        concurrent_row(&mut connection, &self.0.volume_key)
            .await?
            .mode_state()
    }

    async fn preflight_new_bound_mode(&self) -> Result<()> {
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("preflight new bound TiDB metadata", error))?;
        let row = concurrent_row(&mut connection, &self.0.volume_key).await?;
        if row.is_pristine() {
            Ok(())
        } else {
            Err(concurrent_busy("preflight new bound TiDB metadata"))
        }
    }

    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("prepare bound concurrent TiDB metadata", error))?;
        let changed = changed_autocommit_query(
            &mut connection,
            "UPDATE mount_rs_tidb_metadata SET write_mode=?, backing_id=?, fence=?
             WHERE volume_key=? AND write_mode IS NULL AND backing_id IS NULL
               AND revision=0 AND namespace IS NULL AND owner IS NULL AND fence=0 AND expires=0",
            (
                BOUND_CONCURRENT_WRITE_MODE,
                backing.to_hex(),
                CONCURRENT_FENCE_SENTINEL,
                &self.0.volume_key,
            ),
            "prepare bound concurrent TiDB metadata",
        )
        .await?;
        if changed == 1 {
            return Ok(());
        }
        if changed != 0 {
            return Err(backend_error("TiDB bound enrollment changed multiple rows"));
        }
        match concurrent_row(&mut connection, &self.0.volume_key)
            .await?
            .mode_state()?
        {
            ConcurrentModeState::Mrc2(actual) if actual == backing => Ok(()),
            ConcurrentModeState::Mrc2(_) => Err(stale()),
            ConcurrentModeState::Legacy | ConcurrentModeState::Mrc1 => {
                Err(concurrent_busy("prepare bound concurrent TiDB metadata"))
            }
        }
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        validate_scope(owner, "TiDB writer owner")?;
        let ttl_ms = ttl_ms(ttl)?;
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("acquire TiDB writer", error))?;
        let mut transaction = begin_pessimistic(&mut connection).await?;
        let row = match locked_lease_row(&mut transaction, &self.0.volume_key).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                return rollback_and(transaction, backend_error("TiDB metadata row is missing"))
                    .await;
            }
            Err(error) => return rollback_and(transaction, error).await,
        };
        if row.owner.is_some() && row.now_ms < row.expires_at_ms {
            return rollback_and(
                transaction,
                FsError::new(ErrorCode::Eagain).with_syscall("TiDB acquire writer"),
            )
            .await;
        }
        let next_fence = row
            .fence
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow));
        let next_fence = match next_fence {
            Ok(value) => value,
            Err(error) => return rollback_and(transaction, error).await,
        };
        let expires_at_ms = match expiry(row.now_ms, ttl_ms) {
            Ok(value) => value,
            Err(error) => return rollback_and(transaction, error).await,
        };
        let fence_i64 = match signed(next_fence, "metadata fence") {
            Ok(value) => value,
            Err(error) => return rollback_and(transaction, error).await,
        };
        let expires_i64 = match signed(expires_at_ms, "metadata expiry") {
            Ok(value) => value,
            Err(error) => return rollback_and(transaction, error).await,
        };
        let old_fence_i64 = match signed(row.fence, "metadata fence") {
            Ok(value) => value,
            Err(error) => return rollback_and(transaction, error).await,
        };
        let old_expires_i64 = match signed(row.expires_at_ms, "metadata expiry") {
            Ok(value) => value,
            Err(error) => return rollback_and(transaction, error).await,
        };
        let changed = match changed_query(
            &mut transaction,
            "UPDATE mount_rs_tidb_metadata
             SET owner=?, fence=?, expires=?
             WHERE volume_key=? AND fence=? AND expires=?
               AND write_mode IS NULL AND backing_id IS NULL",
            (
                owner,
                fence_i64,
                expires_i64,
                &self.0.volume_key,
                old_fence_i64,
                old_expires_i64,
            ),
            "acquire TiDB writer",
        )
        .await
        {
            Ok(changed) => changed,
            Err(error) => return rollback_and(transaction, error).await,
        };
        if changed != 1 {
            return rollback_and(transaction, stale()).await;
        }
        commit(transaction, "acquire writer").await?;
        Ok(WriterLease {
            owner: owner.to_owned(),
            fence: next_fence,
            expires_at_ms,
        })
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        validate_scope(&lease.owner, "TiDB writer owner")?;
        let ttl_ms = ttl_ms(ttl)?;
        let lease_fence = signed(lease.fence, "metadata fence")?;
        let lease_expires = signed(lease.expires_at_ms, "metadata expiry")?;
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("renew TiDB writer", error))?;
        let mut transaction = begin_pessimistic(&mut connection).await?;
        let row = match locked_lease_row(&mut transaction, &self.0.volume_key).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                return rollback_and(transaction, backend_error("TiDB metadata row is missing"))
                    .await;
            }
            Err(error) => return rollback_and(transaction, error).await,
        };
        if row.owner.as_deref() != Some(&lease.owner)
            || row.fence != lease.fence
            || row.expires_at_ms != lease.expires_at_ms
            || row.now_ms >= row.expires_at_ms
        {
            return rollback_and(transaction, stale()).await;
        }
        let expires_at_ms = match expiry(row.now_ms, ttl_ms) {
            Ok(value) => value,
            Err(error) => return rollback_and(transaction, error).await,
        };
        let expires_i64 = match signed(expires_at_ms, "metadata expiry") {
            Ok(value) => value,
            Err(error) => return rollback_and(transaction, error).await,
        };
        let changed = match changed_query(
            &mut transaction,
            &format!(
                "UPDATE mount_rs_tidb_metadata SET expires=?
                 WHERE volume_key=? AND owner=? AND fence=? AND expires=?
                   AND write_mode IS NULL AND backing_id IS NULL
                   AND expires>{NOW_MS}"
            ),
            (
                expires_i64,
                &self.0.volume_key,
                &lease.owner,
                lease_fence,
                lease_expires,
            ),
            "renew TiDB writer",
        )
        .await
        {
            Ok(changed) => changed,
            Err(error) => return rollback_and(transaction, error).await,
        };
        if changed != 1 {
            return rollback_and(transaction, stale()).await;
        }
        commit(transaction, "renew writer").await?;
        Ok(WriterLease {
            expires_at_ms,
            ..lease.clone()
        })
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        validate_scope(&lease.owner, "TiDB writer owner")?;
        let lease_fence = signed(lease.fence, "metadata fence")?;
        let lease_expires = signed(lease.expires_at_ms, "metadata expiry")?;
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("release TiDB writer", error))?;
        let mut transaction = begin_pessimistic(&mut connection).await?;
        let row = match locked_lease_row(&mut transaction, &self.0.volume_key).await {
            Ok(Some(row)) => row,
            Ok(None) => {
                return rollback_and(transaction, backend_error("TiDB metadata row is missing"))
                    .await;
            }
            Err(error) => return rollback_and(transaction, error).await,
        };
        if row.owner.as_deref() != Some(&lease.owner)
            || row.fence != lease.fence
            || row.expires_at_ms != lease.expires_at_ms
            || row.now_ms >= row.expires_at_ms
        {
            return rollback_and(transaction, stale()).await;
        }
        let changed = match changed_query(
            &mut transaction,
            &format!(
                "UPDATE mount_rs_tidb_metadata SET owner=NULL, expires=0
                 WHERE volume_key=? AND owner=? AND fence=? AND expires=?
                   AND write_mode IS NULL AND backing_id IS NULL
                   AND expires>{NOW_MS}"
            ),
            (&self.0.volume_key, &lease.owner, lease_fence, lease_expires),
            "release TiDB writer",
        )
        .await
        {
            Ok(changed) => changed,
            Err(error) => return rollback_and(transaction, error).await,
        };
        if changed != 1 {
            return rollback_and(transaction, stale()).await;
        }
        commit(transaction, "release writer").await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        validate_scope(&lease.owner, "TiDB writer owner")?;
        let expected = signed(expected_revision, "metadata revision")?;
        let lease_fence = signed(lease.fence, "metadata fence")?;
        let lease_expires = signed(lease.expires_at_ms, "metadata expiry")?;
        let next_revision = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let namespace = serde_json::to_string(&namespace).map_err(backend_error)?;
        profile::add(Event::NamespaceSerialized, namespace.len() as u64);
        if namespace.len() > self.0.max_namespace_bytes {
            return Err(FsError::new(ErrorCode::Efbig)
                .with_syscall("TiDB publish metadata")
                .with_message("serialized namespace exceeds the configured TiDB limit"));
        }

        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("publish TiDB metadata", error))?;
        // A conditional UPDATE is already an atomic single-statement
        // transaction in TiDB. The successful CAS therefore avoids the
        // START/COMMIT round trips used by the exceptional classification
        // path below. The predicate carries the full lease and revision fence;
        // no row lock is needed on the successful path.
        let changed = changed_autocommit_query(
            &mut connection,
            &format!(
                "UPDATE mount_rs_tidb_metadata
                 SET revision=?, namespace=?
                 WHERE volume_key=? AND revision=? AND owner=? AND fence=? AND expires=?
                   AND write_mode IS NULL AND backing_id IS NULL
                   AND expires>{NOW_MS}"
            ),
            (
                next_revision,
                &namespace,
                &self.0.volume_key,
                expected,
                &lease.owner,
                lease_fence,
                lease_expires,
            ),
            "publish TiDB metadata",
        )
        .await?;
        if changed != 1 {
            // The conditional update is the successful-path CAS. Only the
            // exceptional path needs the locked read to preserve the former
            // stale-versus-revision-conflict classification.
            let mut transaction = begin_pessimistic(&mut connection).await?;
            let row = match locked_publish_row(&mut transaction, &self.0.volume_key).await {
                Ok(row) => row,
                Err(error) => return rollback_and(transaction, error).await,
            };
            let Some((revision, lease_row)) = row else {
                return rollback_and(transaction, backend_error("TiDB metadata row is missing"))
                    .await;
            };
            if lease_row.owner.as_deref() != Some(&lease.owner)
                || lease_row.fence != lease.fence
                || lease_row.expires_at_ms != lease.expires_at_ms
                || lease_row.now_ms >= lease_row.expires_at_ms
            {
                return rollback_and(transaction, stale()).await;
            }
            if revision != expected_revision {
                return rollback_and(transaction, revision_conflict()).await;
            }
            return rollback_and(transaction, stale()).await;
        }
        Ok(next_revision as u64)
    }

    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        namespace.validate()?;
        let expected = signed(expected_revision, "metadata revision")?;
        let next = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let namespace = serde_json::to_string(&namespace).map_err(backend_error)?;
        profile::add(Event::NamespaceSerialized, namespace.len() as u64);
        if namespace.len() > self.0.max_namespace_bytes {
            return Err(FsError::new(ErrorCode::Efbig)
                .with_syscall("TiDB publish bound metadata")
                .with_message("serialized namespace exceeds the configured TiDB limit"));
        }
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("publish bound TiDB metadata", error))?;
        // TiDB may retry its own single-statement autocommit internally. The
        // revision predicate still permits exactly one committed publication.
        // An unknown acknowledgement is never replayed by this provider.
        let changed = changed_autocommit_query(
            &mut connection,
            "UPDATE mount_rs_tidb_metadata SET revision=?, namespace=?
             WHERE volume_key=? AND revision=? AND write_mode=? AND backing_id=?
               AND owner IS NULL AND fence=? AND expires=0",
            (
                next,
                &namespace,
                &self.0.volume_key,
                expected,
                BOUND_CONCURRENT_WRITE_MODE,
                backing.to_hex(),
                CONCURRENT_FENCE_SENTINEL,
            ),
            "publish bound TiDB metadata",
        )
        .await?;
        if changed == 1 {
            return Ok(next as u64);
        }
        if changed != 0 {
            return Err(backend_error("TiDB bound CAS changed multiple rows"));
        }
        let row = concurrent_row(&mut connection, &self.0.volume_key).await?;
        match row.mode_state()? {
            ConcurrentModeState::Mrc2(actual) if actual != backing => return Err(stale()),
            ConcurrentModeState::Mrc2(_) => {}
            ConcurrentModeState::Legacy | ConcurrentModeState::Mrc1 => {
                return Err(concurrent_busy("publish bound TiDB metadata"));
            }
        }
        if row.revision != expected_revision {
            return Err(revision_conflict());
        }
        Err(backend_error(
            "TiDB bound CAS returned zero for unchanged revision",
        ))
    }

    async fn migrate_mrc1_to_bound_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        let expected = signed(expected_revision, "metadata revision")?;
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("migrate MRC1 TiDB metadata", error))?;
        let changed = changed_autocommit_query(
            &mut connection,
            "UPDATE mount_rs_tidb_metadata SET write_mode=?, backing_id=?
             WHERE volume_key=? AND revision=? AND write_mode=? AND backing_id IS NULL
               AND owner IS NULL AND fence=? AND expires=0",
            (
                BOUND_CONCURRENT_WRITE_MODE,
                backing.to_hex(),
                &self.0.volume_key,
                expected,
                CONCURRENT_WRITE_MODE,
                CONCURRENT_FENCE_SENTINEL,
            ),
            "migrate MRC1 TiDB metadata",
        )
        .await?;
        if changed == 1 {
            return Ok(());
        }
        if changed != 0 {
            return Err(backend_error("TiDB MRC1 migration changed multiple rows"));
        }
        let row = concurrent_row(&mut connection, &self.0.volume_key).await?;
        if row.revision != expected_revision {
            Err(revision_conflict())
        } else {
            Err(concurrent_busy("migrate MRC1 TiDB metadata"))
        }
    }

    async fn preflight_mrc1_to_bound_mode(&self, expected_revision: u64) -> Result<()> {
        signed(expected_revision, "metadata revision")?;
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("preflight MRC1 TiDB metadata", error))?;
        let row = concurrent_row(&mut connection, &self.0.volume_key).await?;
        if row.revision != expected_revision {
            return Err(revision_conflict());
        }
        match row.mode_state()? {
            ConcurrentModeState::Mrc1 => Ok(()),
            ConcurrentModeState::Legacy | ConcurrentModeState::Mrc2(_) => {
                Err(concurrent_busy("preflight MRC1 TiDB metadata"))
            }
        }
    }

    async fn flush(&self) -> Result<()> {
        self.0.acknowledgement_barrier().await
    }
}

#[async_trait]
impl BlockStore for TidbBlockStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("prepare TiDB block authority", error))?;
        // The server generates the candidate, avoiding a client-local identity
        // or a dependency on which pool/host first opens the backing scope.
        let candidate: String = connection
            .query_first("SELECT LOWER(REPLACE(UUID(), '-', ''))")
            .await
            .map_err(|error| db_error("generate TiDB block authority", error))?
            .ok_or_else(|| backend_error("TiDB UUID query returned no row"))?;
        ConcurrentBackingId::from_hex(&candidate)
            .map_err(|_| backend_error("TiDB generated an invalid block authority ID"))?;
        changed_autocommit_query(
            &mut connection,
            "INSERT INTO mount_rs_tidb_block_authority (volume_key, backing_id) VALUES (?, ?)
             ON DUPLICATE KEY UPDATE volume_key=volume_key",
            (&self.0.volume_key, candidate),
            "prepare TiDB block authority",
        )
        .await?;
        let actual: Option<Vec<u8>> = connection
            .exec_first(
                "SELECT backing_id FROM mount_rs_tidb_block_authority WHERE volume_key=?",
                (&self.0.volume_key,),
            )
            .await
            .map_err(|error| db_error("read TiDB block authority", error))?;
        backing_from_bytes(
            &actual.ok_or_else(|| backend_error("TiDB block authority disappeared"))?,
        )
        .map_err(|_| backend_error("TiDB block authority ID is invalid"))
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("verify TiDB block authority", error))?;
        let actual: Option<Vec<u8>> = connection
            .exec_first(
                "SELECT backing_id FROM mount_rs_tidb_block_authority WHERE volume_key=?",
                (&self.0.volume_key,),
            )
            .await
            .map_err(|error| db_error("verify TiDB block authority", error))?;
        let actual = backing_from_bytes(&actual.ok_or_else(stale)?)?;
        if actual == expected {
            Ok(())
        } else {
            Err(stale())
        }
    }

    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        let bytes = self.get(id).await?;
        if block_id(&bytes) != *id {
            return Err(FsError::new(ErrorCode::Eio).with_syscall("verify TiDB migration block"));
        }
        Ok(bytes)
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        if bytes.len() > self.0.max_block_bytes {
            return Err(FsError::new(ErrorCode::Efbig)
                .with_syscall("TiDB put block")
                .with_message("block exceeds the configured TiDB limit"));
        }
        let id = block_id(bytes);
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("put TiDB block", error))?;
        // The duplicate branch updates only the key. It can never replace
        // bytes written by an older layout, even when two writers race.
        let result = connection
            .exec_iter(
                "INSERT INTO mount_rs_tidb_blocks (volume_key, id, bytes)
                 VALUES (?, ?, ?)
                 ON DUPLICATE KEY UPDATE id=id",
                (&self.0.volume_key, &id.0, bytes.to_vec()),
            )
            .await
            .map_err(|error| db_error("put TiDB block", error))?;
        // A new row is unambiguous: the server reports one affected row and
        // the bytes in the INSERT are the bytes now protected by the primary
        // key. Duplicate/no-op and server-specific affected-row results keep
        // the read-back collision check below; never treat those as verified
        // merely because the write statement succeeded.
        let inserted = result.affected_rows() == 1;
        result
            .drop_result()
            .await
            .map_err(|error| db_error("finish TiDB block put", error))?;
        if inserted {
            return Ok(id);
        }
        let existing: Option<Vec<u8>> = connection
            .exec_first(
                "SELECT bytes FROM mount_rs_tidb_blocks
                 WHERE volume_key=? AND id=?",
                (&self.0.volume_key, &id.0),
            )
            .await
            .map_err(|error| db_error("verify TiDB block", error))?;
        match existing {
            Some(existing) if existing == bytes => Ok(id),
            Some(_) => Err(backend_error("TiDB block identity collision")),
            None => Err(backend_error("TiDB block disappeared after put")),
        }
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        validate_block_id(id)?;
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("get TiDB block", error))?;
        connection
            .exec_first(
                "SELECT bytes FROM mount_rs_tidb_blocks
                 WHERE volume_key=? AND id=?",
                (&self.0.volume_key, &id.0),
            )
            .await
            .map_err(|error| db_error("get TiDB block", error))?
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("get TiDB block"))
    }

    async fn flush(&self) -> Result<()> {
        self.0.acknowledgement_barrier().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        validate_block_id(id)?;
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("delete TiDB block", error))?;
        connection
            .exec_drop(
                "DELETE FROM mount_rs_tidb_blocks WHERE volume_key=? AND id=?",
                (&self.0.volume_key, &id.0),
            )
            .await
            .map_err(|error| db_error("delete TiDB block", error))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn delegated_enrollment_cannot_reset_an_exhausted_legacy_fence() {
        assert!(!delegation_legacy_enrollment_valid(
            None,
            CONCURRENT_FENCE_SENTINEL
        ));
        assert!(!delegation_legacy_enrollment_valid(None, -1));
        assert!(delegation_legacy_enrollment_valid(None, 17));
        assert!(!delegation_legacy_enrollment_valid(Some(b"dangling"), 17));
    }

    #[test]
    fn delegation_authority_rejects_old_protocols_and_partial_markers() {
        let backing = ConcurrentBackingId::from_bytes([9; 16]).unwrap();
        let state = mount_rs_core::delegation::DelegationState::new(backing);
        let raw = serde_json::to_vec(&state).unwrap();
        assert_eq!(
            decode_delegation_authority(Some(b"MRC3"), Some(&backing.as_bytes()), Some(&raw))
                .unwrap(),
            Some(state)
        );
        assert!(
            decode_delegation_authority(Some(b"MRC2"), Some(&backing.as_bytes()), Some(&raw))
                .is_err()
        );
        assert!(decode_delegation_authority(Some(b"MRC3"), None, Some(&raw)).is_err());
        assert!(decode_delegation_authority(Some(b"MRC3"), Some(&[8; 16]), Some(&raw)).is_err());
        assert!(
            decode_delegation_authority(Some(b"MRC3"), Some(&backing.as_bytes()), Some(b"{} "))
                .is_err()
        );
        assert!(
            decode_delegation_authority(None, None, None)
                .unwrap()
                .is_none()
        );
    }

    use super::*;
    use mysql_async::{DriverError, ServerError};

    #[cfg(unix)]
    #[tokio::test]
    #[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
    async fn actual_tidb_inode_versions_preserve_unrelated_writes_and_fence_structure() {
        use mount_rs_core::{
            FsDriver,
            chunking::{Chunker, FixedSizeChunker},
            storage::{DirectoryEntry, FileLayout, NodeData},
        };
        let url = std::env::var("MOUNT_RS_TIDB_URL").expect("actual TiDB URL required");
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let key = format!("inode-cas-{}-{stamp}", std::process::id());
        let store = TidbMetadataStore::connect_with_options(&url, TidbStorageOptions::new(&key))
            .await
            .unwrap();
        let other = TidbMetadataStore::connect_with_options(&url, TidbStorageOptions::new(&key))
            .await
            .unwrap();
        let backing = ConcurrentBackingId::from_bytes([9; 16]).unwrap();
        store.prepare_bound_concurrent_mode(backing).await.unwrap();
        let stats = mount_rs_memfs::MemoryFs::empty().stat("/").await.unwrap();
        let root = stats.ino;
        let chunker = FixedSizeChunker::new(4096).unwrap().config();
        let mut ns = Namespace {
            format_version: 1,
            root,
            next_inode: root + 3,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: chunker.clone(),
            nodes: BTreeMap::from([(
                root,
                NodeMetadata {
                    stats,
                    data: NodeData::Directory { entries: vec![] },
                },
            )]),
        };
        for inode in [root + 1, root + 2] {
            let mut stats = ns.nodes[&root].stats.clone();
            stats.ino = inode;
            stats.mode = mount_rs_core::S_IFREG | 0o644;
            stats.nlink = 1;
            stats.size = 0;
            stats.blocks = 0;
            ns.nodes.insert(
                inode,
                NodeMetadata {
                    stats,
                    data: NodeData::File(FileLayout {
                        chunker: chunker.clone(),
                        extents: vec![],
                    }),
                },
            );
            let NodeData::Directory { entries } = &mut ns.nodes.get_mut(&root).unwrap().data else {
                unreachable!()
            };
            entries.push(DirectoryEntry {
                name: format!("file-{inode}"),
                inode,
            });
        }
        ns.validate().unwrap();
        store
            .publish_bound_if_revision(backing, 0, ns)
            .await
            .unwrap();
        store.prepare_inode_mode(backing, 1).await.unwrap();
        let mut connection = store.0.pool.get_conn().await.unwrap();
        // Exact historical mode-blind revision probe must invalidate old G=1.
        let old_probe: i64 = connection.exec_first("SELECT revision FROM mount_rs_tidb_metadata USE INDEX (idx_mount_rs_volume_revision) WHERE volume_key=?",(&key,)).await.unwrap().unwrap();
        assert_ne!(old_probe, 1);
        let old_payload: String = connection
            .exec_first(
                "SELECT namespace FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&key,),
            )
            .await
            .unwrap()
            .unwrap();
        assert!(serde_json::from_str::<Namespace>(&old_payload).is_err());
        let plan: Vec<mysql_async::Row> = connection
            .exec(format!("EXPLAIN {INODE_AUTHORITY_SQL}"), (&key,))
            .await
            .unwrap();
        let operators: Vec<String> = plan
            .iter()
            .map(|row| row.get::<String, _>("id").unwrap())
            .collect();
        assert!(
            operators
                .iter()
                .any(|operator| operator.starts_with("IndexReader")),
            "compact authority must be covered: {operators:?}"
        );
        assert!(
            !operators
                .iter()
                .any(|operator| operator.starts_with("TableReader")
                    || operator.starts_with("IndexLookUp")),
            "compact authority must not fetch namespace row: {operators:?}"
        );
        drop(connection);
        assert!(store.load().await.is_err());
        assert!(store.load_if_changed(1).await.is_err());
        assert!(store.concurrent_mode_state().await.is_err());
        let stale_snapshot = store.load_inode_snapshot(backing).await.unwrap();
        let first = store.load_inode(backing, root + 1).await.unwrap();
        let second = other.load_inode(backing, root + 2).await.unwrap();
        let mut first_node = first.node.clone();
        first_node.stats.mtime_ms += 1;
        let mut second_node = second.node.clone();
        second_node.stats.mtime_ms += 2;
        let (a, b) = tokio::join!(
            store.publish_inode_if_version(backing, root + 1, first.version, first_node.clone()),
            other.publish_inode_if_version(backing, root + 2, second.version, second_node.clone())
        );
        let a = a.unwrap();
        let b = b.unwrap();
        assert_eq!(a.structural_generation, 2);
        assert_eq!(b.structural_generation, 2);
        assert_eq!(a.inode_revision, 1);
        assert_eq!(b.inode_revision, 1);
        assert!(
            store
                .load_inode_if_changed(backing, root + 1, Some(a))
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .publish_structure_if_versions(
                    backing,
                    2,
                    &stale_snapshot.inode_revisions,
                    stale_snapshot.namespace
                )
                .await
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        let current = store.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(current.namespace.nodes[&(root + 1)], first_node);
        assert_eq!(current.namespace.nodes[&(root + 2)], second_node);
        let generation = store
            .publish_structure_if_versions(backing, 2, &current.inode_revisions, current.namespace)
            .await
            .unwrap();
        assert_eq!(generation, 3);
        assert!(
            store
                .publish_inode_if_version(backing, root + 1, a, first_node)
                .await
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        let next = store.load_inode(backing, root + 1).await.unwrap();
        assert_eq!(
            next.version,
            InodeVersion {
                structural_generation: 3,
                inode_revision: 0
            }
        );
        let before_unlink = store.load_inode_snapshot(backing).await.unwrap();
        let removed = store.load_inode(backing, root + 2).await.unwrap();
        let mut unlinked = before_unlink.namespace;
        unlinked.nodes.remove(&(root + 2));
        let NodeData::Directory { entries } = &mut unlinked.nodes.get_mut(&root).unwrap().data
        else {
            unreachable!()
        };
        entries.retain(|entry| entry.inode != root + 2);
        store
            .publish_structure_if_versions(backing, 3, &before_unlink.inode_revisions, unlinked)
            .await
            .unwrap();
        assert!(
            store
                .publish_inode_if_version(backing, root + 2, removed.version, removed.node)
                .await
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        let before_corruption = store.load_inode_snapshot(backing).await.unwrap();
        let good_root = serde_json::to_string(&before_corruption.namespace.nodes[&root]).unwrap();
        let mut corrupt_root = before_corruption.namespace.nodes[&root].clone();
        let NodeData::Directory { entries } = &mut corrupt_root.data else {
            unreachable!()
        };
        entries.push(DirectoryEntry {
            name: "dangling".to_owned(),
            inode: root + 99,
        });
        let corrupt_json = serde_json::to_string(&corrupt_root).unwrap();
        let mut connection = store.0.pool.get_conn().await.unwrap();
        connection
            .exec_drop(
                "UPDATE mount_rs_tidb_inodes SET node=? WHERE volume_key=? AND inode=?",
                (&corrupt_json, &key, signed(root, "inode ID").unwrap()),
            )
            .await
            .unwrap();
        drop(connection);
        assert!(
            store
                .publish_structure_if_versions(
                    backing,
                    4,
                    &before_corruption.inode_revisions,
                    before_corruption.namespace
                )
                .await
                .is_err()
        );
        let mut connection = store.0.pool.get_conn().await.unwrap();
        let preserved: String = connection
            .exec_first(
                "SELECT node FROM mount_rs_tidb_inodes WHERE volume_key=? AND inode=?",
                (&key, signed(root, "inode ID").unwrap()),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(preserved, corrupt_json);
        connection
            .exec_drop(
                "UPDATE mount_rs_tidb_inodes SET node=? WHERE volume_key=? AND inode=?",
                (&good_root, &key, signed(root, "inode ID").unwrap()),
            )
            .await
            .unwrap();
        drop(connection);
        let known = store.load_inode(backing, root + 1).await.unwrap();
        let mut connection = store.0.pool.get_conn().await.unwrap();
        connection
            .exec_drop(
                "DELETE FROM mount_rs_tidb_inodes WHERE volume_key=? AND inode=?",
                (&key, signed(root + 1, "inode ID").unwrap()),
            )
            .await
            .unwrap();
        drop(connection);
        let missing = store
            .publish_inode_if_version(backing, root + 1, known.version, known.node)
            .await
            .unwrap_err();
        assert!(missing.is(ErrorCode::Estale) || missing.is(ErrorCode::Eio));
        store.close().await.unwrap();
        other.close().await.unwrap();
    }

    #[test]
    fn inode_authority_requires_exact_mode_backing_and_fence() {
        let backing = ConcurrentBackingId::from_bytes([4; 16]).unwrap();
        let mut row = ConcurrentRow {
            revision: 7,
            namespace_empty: false,
            mode: Some(b"MRC4".to_vec()),
            backing: Some(backing.to_hex().into_bytes()),
            owner: None,
            fence: CONCURRENT_FENCE_SENTINEL,
            expires: 0,
        };
        let mode = inode_authority(&row, Some(backing)).unwrap();
        assert_eq!(mode.structural_generation, 7);
        assert!(row.mode_state().is_err());
        assert!(
            inode_authority(
                &row,
                Some(ConcurrentBackingId::from_bytes([5; 16]).unwrap())
            )
            .is_err()
        );
        row.owner = Some(b"old".to_vec());
        assert!(inode_authority(&row, Some(backing)).is_err());
        row.owner = None;
        row.fence = 1;
        assert!(inode_authority(&row, Some(backing)).is_err());
        row.fence = CONCURRENT_FENCE_SENTINEL;
        row.namespace_empty = true;
        assert!(inode_authority(&row, Some(backing)).is_err());
        row.namespace_empty = false;
        row.revision = 0;
        assert!(inode_authority(&row, Some(backing)).is_err());
        row.revision = 7;
        row.mode = Some(b"MRC2".to_vec());
        assert!(inode_authority(&row, Some(backing)).is_err());
    }

    #[tokio::test]
    #[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
    async fn actual_tidb_unchanged_revision_uses_covering_index() {
        let url = std::env::var("MOUNT_RS_TIDB_URL").expect("actual TiDB URL required");
        let pool = Pool::from_url(&url).unwrap();
        let mut connection = pool.get_conn().await.unwrap();
        let version: String = connection
            .query_first("SELECT VERSION()")
            .await
            .unwrap()
            .unwrap();
        assert!(version.to_ascii_lowercase().contains("tidb"));
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let key = format!("tidb-index-plan-{}-{stamp}", std::process::id());
        let metadata = TidbMetadataStore::connect_with_options(&url, TidbStorageOptions::new(&key))
            .await
            .expect("connect metadata and migrate the covering revision index");
        let plan: Vec<mysql_async::Row> = connection
            .exec(format!("EXPLAIN {REVISION_PROBE_SQL}"), (&key,))
            .await
            .expect("explain the exact production revision probe");
        let operators: Vec<String> = plan
            .iter()
            .map(|row| row.get::<String, _>("id").expect("EXPLAIN operator id"))
            .collect();
        assert!(
            operators
                .iter()
                .any(|operator| operator.starts_with("IndexReader")),
            "revision probe must use TiDB IndexReader, got {operators:?}"
        );
        assert!(
            !operators
                .iter()
                .any(|operator| operator.starts_with("TableReader")),
            "revision probe must not fetch the namespace table row, got {operators:?}"
        );
        metadata.close().await.unwrap();
        drop(connection);
        pool.disconnect().await.unwrap();
    }

    fn pristine_concurrent_row() -> ConcurrentRow {
        ConcurrentRow {
            revision: 0,
            namespace_empty: true,
            mode: None,
            backing: None,
            owner: None,
            fence: 0,
            expires: 0,
        }
    }

    #[tokio::test]
    #[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
    async fn actual_tidb_context_enforces_session_bound_and_verified_reuse() {
        let url = std::env::var("MOUNT_RS_TIDB_URL").unwrap();
        let context = TidbPoolContext::new(&url, 2).unwrap();
        let mut first = context.0.pool.get_conn().await.unwrap();
        let second = context.0.pool.get_conn().await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(50), context.0.pool.get_conn())
                .await
                .is_err()
        );
        let settings: (String, String, u8) = first
            .query_first("SELECT @@tidb_txn_mode, @@transaction_isolation, @@autocommit")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settings.0.to_ascii_lowercase(), "pessimistic");
        assert_eq!(settings.1.to_ascii_uppercase(), "REPEATABLE-READ");
        assert_eq!(settings.2, 1);
        drop(first);
        second.disconnect().await.unwrap();
        let mut reused = context.0.pool.get_conn().await.unwrap();
        let enabled: u8 = reused
            .query_first("SELECT @@autocommit")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(enabled, 1);
        let old_id: u64 = reused
            .query_first("SELECT CONNECTION_ID()")
            .await
            .unwrap()
            .unwrap();
        reused.disconnect().await.unwrap();
        let mut reconnected = context.0.pool.get_conn().await.unwrap();
        let settings: (String, String, u8, u64) = reconnected
            .query_first(
                "SELECT @@tidb_txn_mode, @@transaction_isolation, @@autocommit, CONNECTION_ID()",
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(settings.0.to_ascii_lowercase(), "pessimistic");
        assert_eq!(settings.1.to_ascii_uppercase(), "REPEATABLE-READ");
        assert_eq!(settings.2, 1);
        assert_ne!(settings.3, old_id);
        drop(reconnected);
        context.close().await.unwrap();
        assert!(context.0.pool.get_conn().await.is_err());
    }

    #[test]
    fn inode_mode_fences_legacy_inspectors_with_known_stale_authority() {
        let mut row = pristine_concurrent_row();
        row.mode = Some(b"MRC4".to_vec());
        row.backing = Some(
            ConcurrentBackingId::from_bytes([4; 16])
                .unwrap()
                .to_hex()
                .into_bytes(),
        );
        row.fence = CONCURRENT_FENCE_SENTINEL;
        assert_eq!(row.mode_state().unwrap_err().code, ErrorCode::Estale);
    }

    #[test]
    fn concurrent_markers_require_canonical_identity_and_exhausted_legacy_fence() {
        let mut row = pristine_concurrent_row();
        assert_eq!(row.mode_state().unwrap(), ConcurrentModeState::Legacy);
        assert!(row.is_pristine());
        row.mode = Some(CONCURRENT_WRITE_MODE.as_bytes().to_vec());
        row.fence = CONCURRENT_FENCE_SENTINEL;
        assert_eq!(row.mode_state().unwrap(), ConcurrentModeState::Mrc1);
        assert!(!row.is_pristine());
        let backing = ConcurrentBackingId::from_bytes([4; 16]).unwrap();
        row.mode = Some(BOUND_CONCURRENT_WRITE_MODE.as_bytes().to_vec());
        row.backing = Some(backing.to_hex().into_bytes());
        assert_eq!(
            row.mode_state().unwrap(),
            ConcurrentModeState::Mrc2(backing)
        );
        row.fence = 1;
        assert!(row.mode_state().unwrap_err().is(ErrorCode::Estale));
        row.fence = CONCURRENT_FENCE_SENTINEL;
        row.owner = Some(b"old-writer".to_vec());
        assert!(row.mode_state().unwrap_err().is(ErrorCode::Estale));
        row.owner = None;
        row.expires = 1;
        assert!(row.mode_state().unwrap_err().is(ErrorCode::Estale));
        row.expires = 0;
        for malformed in ["", "00000000000000000000000000000000", "wrong"] {
            row.backing = Some(malformed.as_bytes().to_vec());
            assert!(row.mode_state().unwrap_err().is(ErrorCode::Estale));
        }
        row.mode = Some(b"bad".to_vec());
        assert!(row.mode_state().unwrap_err().is(ErrorCode::Eio));
    }

    #[test]
    fn unstamped_exhausted_fence_and_partial_markers_are_never_legacy() {
        let mut row = pristine_concurrent_row();
        row.fence = CONCURRENT_FENCE_SENTINEL;
        assert!(row.mode_state().unwrap_err().is(ErrorCode::Eio));
        row.fence = 0;
        row.backing = Some(
            ConcurrentBackingId::from_bytes([4; 16])
                .unwrap()
                .to_hex()
                .into_bytes(),
        );
        assert!(row.mode_state().unwrap_err().is(ErrorCode::Eio));
        assert!(!row.is_pristine());
    }

    #[test]
    fn binary_authority_corruption_is_an_error_without_utf8_row_conversion() {
        let mut row = pristine_concurrent_row();
        row.mode = Some(BOUND_CONCURRENT_WRITE_MODE.as_bytes().to_vec());
        row.backing = Some(vec![0xff]);
        row.fence = CONCURRENT_FENCE_SENTINEL;
        assert!(row.mode_state().unwrap_err().is(ErrorCode::Estale));
        assert!(
            backing_from_bytes(&[0xff])
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        row.mode = Some(vec![0xff]);
        assert!(row.mode_state().unwrap_err().is(ErrorCode::Eio));
    }

    #[test]
    fn sessions_must_verify_enabled_autocommit() {
        assert!(require_autocommit(Some(1)).is_ok());
        for value in [None, Some(0), Some(-1), Some(2)] {
            assert!(require_autocommit(value).unwrap_err().is(ErrorCode::Eio));
        }
    }

    #[test]
    fn effective_transaction_mode_must_be_verified() {
        assert!(require_pessimistic_mode(Some("pessimistic")).is_ok());
        assert!(require_pessimistic_mode(Some("PESSIMISTIC")).is_ok());
        for mode in [None, Some(""), Some("optimistic"), Some("unknown")] {
            assert!(require_pessimistic_mode(mode).is_err());
        }
    }

    #[test]
    fn effective_transaction_isolation_must_be_repeatable_read() {
        assert!(require_repeatable_read_isolation(Some("REPEATABLE-READ")).is_ok());
        assert!(require_repeatable_read_isolation(Some("repeatable read")).is_ok());
        for isolation in [None, Some(""), Some("READ-COMMITTED"), Some("SERIALIZABLE")] {
            assert!(require_repeatable_read_isolation(isolation).is_err());
        }
    }

    #[test]
    fn block_ids_are_stable_and_strictly_validated() {
        let first = block_id(b"same bytes");
        assert_eq!(first, block_id(b"same bytes"));
        assert!(validate_block_id(&first).is_ok());
        assert!(validate_block_id(&BlockId("b".to_owned())).is_err());
        assert!(validate_block_id(&BlockId("B".to_owned())).is_err());
    }

    #[test]
    fn options_use_conservative_limits_and_reject_zero() {
        let options = TidbStorageOptions::default();
        assert_eq!(options.max_block_bytes, DEFAULT_MAX_BLOCK_BYTES);
        assert_eq!(options.max_namespace_bytes, DEFAULT_MAX_NAMESPACE_BYTES);
        assert!(
            TidbStorageOptions::new("scope")
                .with_max_block_bytes(0)
                .validate()
                .is_err()
        );
        assert!(TidbStorageOptions::new("").validate().is_err());
    }

    #[test]
    fn lease_time_arithmetic_rejects_invalid_and_overflowing_values() {
        assert!(ttl_ms(Duration::ZERO).is_err());
        assert_eq!(ttl_ms(Duration::from_millis(1)).unwrap(), 1);
        assert!(ttl_ms(Duration::from_millis(i64::MAX as u64 + 1)).is_err());
        assert_eq!(expiry(100, 25).unwrap(), 125);
        assert!(expiry(u64::MAX, 1).is_err());
        assert!(nonnegative(-1, "metadata fence").is_err());
        assert!(signed(u64::MAX, "metadata fence").is_err());
    }

    #[test]
    fn commit_errors_remain_ambiguous_even_for_retryable_server_codes() {
        let server_error = MysqlError::Server(ServerError {
            code: 1205,
            message: "Lock wait timeout exceeded".to_owned(),
            state: "HY000".to_owned(),
        });
        let error = ambiguous_commit_error("publish metadata", &server_error);
        assert!(error.is(ErrorCode::Eio));
        assert!(!error.is(ErrorCode::Eagain));
        assert!(error.to_string().contains("commit outcome is unknown"));

        let connection_error = MysqlError::Driver(DriverError::ConnectionClosed);
        let error = ambiguous_commit_error("release writer", &connection_error);
        assert!(error.is(ErrorCode::Eio));
        assert!(error.to_string().contains("commit outcome is unknown"));
    }

    #[test]
    fn autocommit_statement_errors_preserve_conflicts_and_fail_closed_acknowledgements() {
        let conflict = MysqlError::Server(ServerError {
            code: 1205,
            message: "Lock wait timeout exceeded".to_owned(),
            state: "HY000".to_owned(),
        });
        assert!(autocommit_error("publish metadata", conflict).is(ErrorCode::Eagain));

        let connection_error = MysqlError::Driver(DriverError::ConnectionClosed);
        let error = autocommit_error("publish metadata", connection_error);
        assert!(error.is(ErrorCode::Eio));
        assert!(error.to_string().contains("commit outcome is unknown"));
    }

    #[test]
    fn malformed_url_errors_do_not_echo_credentials() {
        let password = "tidb-test-secret";
        let url = format!("mysql://mount:{password}@127.0.0.1/test?compression=not-a-mode");
        let error = match Pool::from_url(&url) {
            Ok(_) => panic!("the invalid connection URL must be rejected"),
            Err(error) => error,
        };
        let rendered = db_error("parse TiDB URL", error).to_string();
        assert!(rendered.contains("connection URL was rejected"));
        assert!(!rendered.contains(password));
    }
}
