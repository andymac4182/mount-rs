use async_trait::async_trait;
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use mysql_async::prelude::Queryable;
use mysql_async::{
    Conn, Error as MysqlError, Opts, OptsBuilder, Params, Pool, Transaction, TxOpts,
};
use sha2::{Digest, Sha256};
use std::time::Duration;

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

const METADATA_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_tidb_metadata (
    volume_key VARBINARY(1020) NOT NULL,
    revision BIGINT NOT NULL,
    namespace LONGTEXT NULL,
    owner VARBINARY(1020) NULL,
    fence BIGINT NOT NULL,
    expires BIGINT NOT NULL,
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
        };

        let mut connection = database
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("connect to TiDB", error))?;
        connection
            .query_drop(schema)
            .await
            .map_err(|error| db_error("initialize TiDB schema", error))?;
        if ensure_metadata_row {
            connection
                .exec_drop(
                    "INSERT INTO mount_rs_tidb_metadata
                        (volume_key, revision, namespace, owner, fence, expires)
                     VALUES (?, 0, NULL, NULL, 0, 0)
                     ON DUPLICATE KEY UPDATE volume_key=volume_key",
                    (&database.volume_key,),
                )
                .await
                .map_err(|error| db_error("initialize TiDB metadata row", error))?;
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
        self.pool
            .clone()
            .disconnect()
            .await
            .map_err(|error| db_error("close TiDB connection pool", error))
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

    /// Stop this store's pool. It is idempotent from the caller's point of
    /// view; subsequent operations return a backend connection error.
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

async fn configure_pessimistic_session(
    connection: &mut Conn,
) -> std::result::Result<(), MysqlError> {
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

#[async_trait]
impl MetadataStore for TidbMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    fn publish_includes_flush_barrier(&self) -> bool {
        // A successful COMMIT acknowledgement is the same provider barrier
        // as the extra connection probe; ambiguous COMMIT errors still fail
        // closed and explicit syncfs retains the probe.
        true
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let mut connection = self
            .0
            .pool
            .get_conn()
            .await
            .map_err(|error| db_error("load TiDB metadata", error))?;
        let row: Option<(i64, Option<String>)> = connection
            .exec_first(
                "SELECT revision, namespace
                 FROM mount_rs_tidb_metadata
                 WHERE volume_key=?",
                (&self.0.volume_key,),
            )
            .await
            .map_err(|error| db_error("load TiDB metadata", error))?;
        let Some((revision, namespace)) = row else {
            return Err(backend_error("TiDB metadata row is missing"));
        };
        let revision = nonnegative(revision, "metadata revision")?;
        let namespace = namespace
            .map(|json| serde_json::from_str(&json).map_err(backend_error))
            .transpose()?;
        Ok(LoadedMetadata {
            revision,
            namespace,
        })
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
             WHERE volume_key=? AND fence=? AND expires=?",
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
        let mut transaction = begin_pessimistic(&mut connection).await?;
        let changed = match changed_query(
            &mut transaction,
            &format!(
                "UPDATE mount_rs_tidb_metadata
                 SET revision=?, namespace=?
                 WHERE volume_key=? AND revision=? AND owner=? AND fence=? AND expires=?
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
        .await
        {
            Ok(changed) => changed,
            Err(error) => return rollback_and(transaction, error).await,
        };
        if changed != 1 {
            // The conditional update is the successful-path CAS. Only the
            // exceptional path needs the locked read to preserve the former
            // stale-versus-revision-conflict classification.
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
        commit(transaction, "publish metadata").await?;
        Ok(next_revision as u64)
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
        connection
            .exec_drop(
                "INSERT INTO mount_rs_tidb_blocks (volume_key, id, bytes)
                 VALUES (?, ?, ?)
                 ON DUPLICATE KEY UPDATE id=id",
                (&self.0.volume_key, &id.0, bytes.to_vec()),
            )
            .await
            .map_err(|error| db_error("put TiDB block", error))?;
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
    use super::*;
    use mysql_async::{DriverError, ServerError};

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
