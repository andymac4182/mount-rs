//! FoundationDB-backed `mount-rs` metadata and immutable block stores.
//!
//! The provider is registered in the core workspace, but its native client is
//! still opt-in. Managed constructors boot one shared native client network.
//! Applications must drop every provider handle and stop/join that network at
//! their terminal process boundary with [`shutdown_client_network`].

#![cfg(all(
    feature = "foundationdb",
    any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )
))]

use async_trait::async_trait;
use foundationdb::api::{FdbApiBuilder, NetworkAutoStop};
use foundationdb::options::TransactionOption;
use foundationdb::{Database, FdbError, TransactOption, Transaction};
use mount_rs_core::chunking::{ChunkerConfig, from_config};
use mount_rs_core::storage::{
    BlockId, BlockStore, ConcurrentBackingId, ConcurrentModeState, LoadedMetadata, MetadataStore,
    Namespace, NodeData, WriterLease,
};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use sha2::{Digest, Sha256};
use std::convert::TryFrom;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::Level;
use uuid::Uuid;

/// FoundationDB's hard key limit.
pub const FOUNDATIONDB_MAX_KEY_BYTES: usize = 10_000;
/// FoundationDB's hard value limit.
pub const FOUNDATIONDB_MAX_VALUE_BYTES: usize = 100_000;
/// FoundationDB's hard transaction affected-data limit.
///
/// This is a decimal limit. A metadata payload must be smaller than this
/// because publication also affects shard keys, the manifest, lease reads,
/// and the compaction range endpoints.
pub const FOUNDATIONDB_MAX_TRANSACTION_BYTES: usize = 10_000_000;
/// Conservative default block limit, below the FoundationDB value limit.
pub const DEFAULT_MAX_BLOCK_BYTES: usize = 64 * 1024;
/// Conservative metadata shard size, below the FoundationDB value limit.
pub const DEFAULT_METADATA_CHUNK_BYTES: usize = 8 * 1024;
/// Conservative maximum namespace payload for one atomic publication.
pub const DEFAULT_MAX_METADATA_BYTES: usize = 512 * 1024;
/// Default native-client transaction timeout.
pub const DEFAULT_TRANSACTION_TIMEOUT: Duration = Duration::from_secs(5);
/// Default per-transaction retry limit.
pub const DEFAULT_TRANSACTION_RETRY_LIMIT: i32 = 8;
// A client Database handle can exist before its coordinator is reachable.
// Keep startup verification shorter than ordinary data transactions and
// independent of caller-provided (possibly much larger) retry bounds.
const CONCURRENT_BLOCK_PREFLIGHT_TIMEOUT: Duration = Duration::from_secs(2);
const CONCURRENT_BLOCK_PREFLIGHT_RETRY_LIMIT: i32 = 2;

const LEASE_MAGIC: &[u8; 4] = b"MRL1";
const MANIFEST_MAGIC: &[u8; 4] = b"MRM1";
const LEASE_HEADER_BYTES: usize = 4 + 8 + 8 + 4;
const MANIFEST_BYTES: usize = 4 + 8 + 4 + 8;
const KEY_SEPARATOR: &[u8] = b"\0";
const METADATA_MANIFEST_SUFFIX: &[u8] = b"meta/manifest";
const METADATA_CHUNK_SUFFIX: &[u8] = b"meta/chunk/";
const METADATA_LEASE_SUFFIX: &[u8] = b"meta/lease";
const METADATA_FENCE_SUFFIX: &[u8] = b"meta/fence";
const METADATA_WRITE_MODE_SUFFIX: &[u8] = b"meta/write-mode";
const METADATA_BACKING_SUFFIX: &[u8] = b"meta/backing-id";
const CONCURRENT_WRITE_MODE: &[u8; 4] = b"MRC1";
const BOUND_CONCURRENT_WRITE_MODE: &[u8; 4] = b"MRC2";
// The old fenced provider decodes this key as a 12-byte MRF1 record. A
// different magic makes old clients fail before they can acquire a lease.
const CONCURRENT_FENCE_SENTINEL: &[u8; FENCE_BYTES] = b"MRCF\0\0\0\0\0\0\0\0";
const LEASE_ORACLE_SUFFIX: &[u8] = b"meta/lease-oracle";
const FLUSH_SUFFIX: &[u8] = b"flush";
const BLOCK_SUFFIX: &[u8] = b"block/";
const BLOCK_AUTHORITY_SUFFIX: &[u8] = b"block-authority";
const BLOCK_ID_TEXT_BYTES: usize = 7 + 64; // "sha256:" plus the hex digest.
const ORACLE_MAGIC: &[u8; 4] = b"MRO1";
const ORACLE_BYTES: usize = 4 + 8;
const FENCE_MAGIC: &[u8; 4] = b"MRF1";
const FENCE_BYTES: usize = 4 + 8;

/// Bounds for one FoundationDB-backed storage volume.
#[derive(Debug, Clone, Copy)]
pub struct FoundationDbLimits {
    /// Maximum bytes accepted by [`BlockStore::put`].
    pub max_block_bytes: usize,
    /// Bytes in each metadata shard.
    pub metadata_chunk_bytes: usize,
    /// Maximum serialized namespace accepted by either metadata publication path.
    pub max_metadata_bytes: usize,
    /// Native transaction timeout applied on each transaction attempt.
    pub transaction_timeout: Duration,
    /// Native retry limit applied on each transaction attempt.
    pub transaction_retry_limit: i32,
}

impl Default for FoundationDbLimits {
    fn default() -> Self {
        Self {
            max_block_bytes: DEFAULT_MAX_BLOCK_BYTES,
            metadata_chunk_bytes: DEFAULT_METADATA_CHUNK_BYTES,
            max_metadata_bytes: DEFAULT_MAX_METADATA_BYTES,
            transaction_timeout: DEFAULT_TRANSACTION_TIMEOUT,
            transaction_retry_limit: DEFAULT_TRANSACTION_RETRY_LIMIT,
        }
    }
}

impl FoundationDbLimits {
    fn validate(self) -> Result<Self> {
        if self.max_block_bytes == 0 || self.max_block_bytes > FOUNDATIONDB_MAX_VALUE_BYTES {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("FoundationDB block limit exceeds the value limit"));
        }
        if self.metadata_chunk_bytes == 0
            || self.metadata_chunk_bytes > FOUNDATIONDB_MAX_VALUE_BYTES
        {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("FoundationDB metadata chunk limit exceeds the value limit"));
        }
        if self.max_metadata_bytes == 0
            || self.max_metadata_bytes > FOUNDATIONDB_MAX_TRANSACTION_BYTES
            || self.max_metadata_bytes < self.metadata_chunk_bytes
        {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("FoundationDB metadata limit must fit within one transaction"));
        }
        if self.transaction_timeout.is_zero()
            || self.transaction_timeout.as_millis() > i32::MAX as u128
        {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("FoundationDB transaction timeout is invalid"));
        }
        if self.transaction_retry_limit <= 0 {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("FoundationDB transaction retry limit must be positive"));
        }
        Ok(self)
    }
}

/// Validate one persisted chunker configuration against this provider's block
/// value bound before opening or mutating a filesystem.
///
/// The core chunker remains backend-independent. A caller composing it with
/// FoundationDB must run this preflight so a fixed-size layout cannot select a
/// chunk larger than the block value limit and discover the incompatibility
/// only after writing file data.
pub fn validate_chunker_config(config: &ChunkerConfig, limits: FoundationDbLimits) -> Result<()> {
    let limits = limits.validate()?;
    from_config(config)?;
    if config.algorithm == "fixed-size" {
        let size = config
            .parameters
            .get("chunk_size")
            .and_then(|size| usize::try_from(*size).ok())
            .ok_or_else(|| FsError::new(ErrorCode::Einval).with_message("invalid chunk_size"))?;
        if size > limits.max_block_bytes {
            return Err(FsError::new(ErrorCode::Efbig).with_message(format!(
                "FoundationDB fixed-size chunk ({size} bytes) exceeds the {}-byte block limit",
                limits.max_block_bytes
            )));
        }
    }
    Ok(())
}

fn validate_namespace_chunkers(namespace: &Namespace, limits: FoundationDbLimits) -> Result<()> {
    validate_chunker_config(&namespace.default_chunker, limits)?;
    for node in namespace.nodes.values() {
        if let NodeData::File(layout) = &node.data {
            validate_chunker_config(&layout.chunker, limits)?;
        }
    }
    Ok(())
}

/// Classifies the trust boundary of a lease-time source.
///
/// `SharedProvider` is reserved for an application-owned authority whose time
/// value is safe to use for independent writers. The other variants are
/// intentionally not accepted by [`FoundationDbStorageOptions::with_production_lease_oracle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseAuthorityKind {
    /// The implementation has not declared a production trust boundary.
    Unverified,
    /// An independent shared provider/authority owns the time value.
    SharedProvider,
    /// One explicitly trusted authority owns this FoundationDB keyspace.
    SingleAuthority,
    /// A process-local or development-only clock.
    Development,
}

/// A shared provider-time authority used to validate metadata leases.
///
/// FoundationDB has no authoritative wall-clock API. This contract is
/// therefore asynchronous so a provider can consult a durable clock/oracle
/// transaction before validating a lease. The authority must fail closed when
/// unavailable; a local caller must never use its own wall clock directly to
/// validate a distributed lease.
#[async_trait]
pub trait LeaseOracle: Send + Sync {
    async fn now_ms(&self) -> Result<u64>;

    /// Read provider time inside an already-open metadata transaction when the
    /// authority can share that transaction's read version. The default keeps
    /// custom clocks and legacy authorities compatible by using their normal
    /// asynchronous read path.
    async fn now_ms_in_transaction(&self, _transaction: &Transaction) -> Result<u64> {
        self.now_ms().await
    }

    /// Declare the authority boundary of this time source.
    ///
    /// Implementations must override this only when an application-owned
    /// authority really provides the declared guarantee. The conservative
    /// default keeps custom test clocks out of the production configuration
    /// path.
    fn authority_kind(&self) -> LeaseAuthorityKind {
        LeaseAuthorityKind::Unverified
    }
}

/// Backwards-compatible name for the lease authority contract.
pub use LeaseOracle as LeaseClock;

/// Local system-clock lease source for development and opt-in integration tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLeaseClock;

#[async_trait]
impl LeaseOracle for SystemLeaseClock {
    async fn now_ms(&self) -> Result<u64> {
        system_now_ms()
    }

    fn authority_kind(&self) -> LeaseAuthorityKind {
        LeaseAuthorityKind::Development
    }
}

fn system_now_ms() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| backend_error(format!("system clock is before Unix epoch: {error}")))
        .and_then(|duration| {
            u64::try_from(duration.as_millis()).map_err(|_| FsError::new(ErrorCode::Eoverflow))
        })
}

fn duration_millis(duration: Duration, description: &str) -> Result<u64> {
    let millis = u64::try_from(duration.as_millis())
        .map_err(|_| FsError::new(ErrorCode::Eoverflow).with_message(description))?;
    if millis == 0 {
        return Err(FsError::new(ErrorCode::Einval).with_message(description));
    }
    Ok(millis)
}

fn observation_time_ms() -> u64 {
    system_now_ms().unwrap_or(0)
}

/// Process-local publication counters for one shared FoundationDB authority.
///
/// The snapshot is intended for application-owned metrics and alerting. Its
/// timestamps describe observations made by this process; they are not used
/// for lease safety decisions, which continue to use the persisted provider
/// time and the validated publication policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaseAuthorityStats {
    /// Number of accepted publication attempts, including failed attempts.
    pub publication_attempts: u64,
    /// Number of successful authority transactions.
    pub publication_successes: u64,
    /// Number of failed or fail-closed publication attempts.
    pub publication_failures: u64,
    /// Last provider-time value successfully written or retained.
    pub last_published_time_ms: Option<u64>,
    /// Local wall-clock observation time of the last successful publication.
    pub last_success_at_ms: Option<u64>,
    /// Local wall-clock observation time of the last failed publication.
    pub last_failure_at_ms: Option<u64>,
}

#[derive(Default)]
struct LeaseAuthorityStatsInner {
    publication_attempts: AtomicU64,
    publication_successes: AtomicU64,
    publication_failures: AtomicU64,
    last_published_time_ms: AtomicU64,
    last_success_at_ms: AtomicU64,
    last_failure_at_ms: AtomicU64,
}

impl LeaseAuthorityStatsInner {
    fn record_attempt(&self) {
        self.publication_attempts.fetch_add(1, Ordering::Relaxed);
    }

    fn record_success(&self, published_time_ms: u64) {
        self.publication_successes.fetch_add(1, Ordering::Relaxed);
        self.last_published_time_ms
            .store(published_time_ms, Ordering::Relaxed);
        self.last_success_at_ms
            .store(observation_time_ms(), Ordering::Relaxed);
    }

    fn record_failure(&self) {
        self.publication_failures.fetch_add(1, Ordering::Relaxed);
        self.last_failure_at_ms
            .store(observation_time_ms(), Ordering::Relaxed);
    }

    fn snapshot(&self) -> LeaseAuthorityStats {
        LeaseAuthorityStats {
            publication_attempts: self.publication_attempts.load(Ordering::Relaxed),
            publication_successes: self.publication_successes.load(Ordering::Relaxed),
            publication_failures: self.publication_failures.load(Ordering::Relaxed),
            last_published_time_ms: non_zero(self.last_published_time_ms.load(Ordering::Relaxed)),
            last_success_at_ms: non_zero(self.last_success_at_ms.load(Ordering::Relaxed)),
            last_failure_at_ms: non_zero(self.last_failure_at_ms.load(Ordering::Relaxed)),
        }
    }
}

/// Process-local read counters for one shared FoundationDB lease oracle.
///
/// Applications can export these fields as reader-health and authority-age
/// signals. A read failure never falls back to a local clock; the provider
/// lease path remains fail-closed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LeaseOracleStats {
    /// Number of authority-read attempts, including failed reads.
    pub read_attempts: u64,
    /// Number of successful authority reads.
    pub read_successes: u64,
    /// Number of failed or fail-closed authority reads.
    pub read_failures: u64,
    /// Last provider-time value observed by a successful read.
    pub last_observed_time_ms: Option<u64>,
    /// Local wall-clock observation time of the last successful read.
    pub last_success_at_ms: Option<u64>,
    /// Local wall-clock observation time of the last failed read.
    pub last_failure_at_ms: Option<u64>,
}

#[derive(Default)]
struct LeaseOracleStatsInner {
    read_attempts: AtomicU64,
    read_successes: AtomicU64,
    read_failures: AtomicU64,
    last_observed_time_ms: AtomicU64,
    last_success_at_ms: AtomicU64,
    last_failure_at_ms: AtomicU64,
}

impl LeaseOracleStatsInner {
    fn record_attempt(&self) {
        self.read_attempts.fetch_add(1, Ordering::Relaxed);
    }

    fn record_success(&self, observed_time_ms: u64) {
        self.read_successes.fetch_add(1, Ordering::Relaxed);
        self.last_observed_time_ms
            .store(observed_time_ms, Ordering::Relaxed);
        self.last_success_at_ms
            .store(observation_time_ms(), Ordering::Relaxed);
    }

    fn record_failure(&self) {
        self.read_failures.fetch_add(1, Ordering::Relaxed);
        self.last_failure_at_ms
            .store(observation_time_ms(), Ordering::Relaxed);
    }

    fn snapshot(&self) -> LeaseOracleStats {
        LeaseOracleStats {
            read_attempts: self.read_attempts.load(Ordering::Relaxed),
            read_successes: self.read_successes.load(Ordering::Relaxed),
            read_failures: self.read_failures.load(Ordering::Relaxed),
            last_observed_time_ms: non_zero(self.last_observed_time_ms.load(Ordering::Relaxed)),
            last_success_at_ms: non_zero(self.last_success_at_ms.load(Ordering::Relaxed)),
            last_failure_at_ms: non_zero(self.last_failure_at_ms.load(Ordering::Relaxed)),
        }
    }
}

fn non_zero(value: u64) -> Option<u64> {
    (value != 0).then_some(value)
}

/// Emit a bounded authority-health event for an application-owned collector.
///
/// The event intentionally contains only counters, timestamps and a fixed
/// outcome label. It never includes the FoundationDB cluster path, key prefix,
/// credentials or provider error text. A configured tracing/OpenTelemetry
/// subscriber may route this event to the application's logs/collector; the
/// provider itself does not install a subscriber or claim alert ownership.
fn emit_lease_authority_telemetry(outcome: &'static str, stats: &LeaseAuthorityStats) {
    tracing::event!(
        target: "mount_rs.foundationdb.authority",
        Level::INFO,
        telemetry_schema = "mount-rs.telemetry.v1",
        event_name = "mount_rs.foundationdb.lease_authority",
        boundary = "provider.foundationdb.lease_authority",
        operation = "publish",
        outcome = %outcome,
        publication_attempts = stats.publication_attempts,
        publication_successes = stats.publication_successes,
        publication_failures = stats.publication_failures,
        last_published_time_ms = stats.last_published_time_ms.unwrap_or(0),
        last_success_at_ms = stats.last_success_at_ms.unwrap_or(0),
        last_failure_at_ms = stats.last_failure_at_ms.unwrap_or(0),
    );
}

/// Emit a bounded shared-reader health event for an application-owned
/// collector. Zero timestamps mean that the corresponding observation has not
/// occurred in this process yet.
fn emit_lease_oracle_telemetry(outcome: &'static str, stats: &LeaseOracleStats) {
    tracing::event!(
        target: "mount_rs.foundationdb.authority",
        Level::INFO,
        telemetry_schema = "mount-rs.telemetry.v1",
        event_name = "mount_rs.foundationdb.lease_oracle",
        boundary = "provider.foundationdb.lease_authority",
        operation = "read",
        outcome = %outcome,
        reader_attempts = stats.read_attempts,
        reader_successes = stats.read_successes,
        reader_failures = stats.read_failures,
        last_observed_time_ms = stats.last_observed_time_ms.unwrap_or(0),
        last_success_at_ms = stats.last_success_at_ms.unwrap_or(0),
        last_failure_at_ms = stats.last_failure_at_ms.unwrap_or(0),
    );
}

/// Safety policy for publishing the shared lease authority.
///
/// The authority service still owns the scheduling loop, but constructing a
/// policy makes the production relationship explicit: publication must be
/// more frequent than the lease TTL, and one accepted wall-clock advance may
/// not exceed one lease TTL. Callers should retain publication failures as
/// operational clock/authority alerts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeasePublicationPolicy {
    /// Maximum age of a lease issued against the shared authority.
    pub lease_ttl: Duration,
    /// Authority publication cadence. This must be shorter than `lease_ttl`.
    pub publication_interval: Duration,
    /// Maximum forward wall-clock advance accepted in one publication.
    pub max_forward_jump: Duration,
}

impl LeasePublicationPolicy {
    /// Construct and validate a shared-authority publication policy.
    pub fn new(
        lease_ttl: Duration,
        publication_interval: Duration,
        max_forward_jump: Duration,
    ) -> Result<Self> {
        Self {
            lease_ttl,
            publication_interval,
            max_forward_jump,
        }
        .validate()
    }

    /// Revalidate a policy, including policies built with a struct literal.
    pub fn validate(self) -> Result<Self> {
        let lease_ttl_ms =
            duration_millis(self.lease_ttl, "FoundationDB lease TTL must be positive")?;
        let publication_interval_ms = duration_millis(
            self.publication_interval,
            "FoundationDB lease publication interval must be positive",
        )?;
        let max_forward_jump_ms = duration_millis(
            self.max_forward_jump,
            "FoundationDB authority forward-jump bound must be positive",
        )?;
        if publication_interval_ms >= lease_ttl_ms {
            return Err(FsError::new(ErrorCode::Einval).with_message(
                "FoundationDB lease publication interval must be shorter than the lease TTL",
            ));
        }
        if max_forward_jump_ms > lease_ttl_ms {
            return Err(FsError::new(ErrorCode::Einval).with_message(
                "FoundationDB authority forward-jump bound must not exceed the lease TTL",
            ));
        }
        Ok(self)
    }
}

/// A FoundationDB-backed shared lease oracle.
///
/// Every call transactionally reads and advances one persisted monotonic
/// millisecond value. The local system clock is used only to propose forward
/// progress; persisted time is never reduced. Lease validation uses the
/// returned shared value, never a caller's local wall clock. If the cluster or
/// oracle transaction is unavailable, the call fails and lease operations fail
/// closed. A deployment should grant this keyspace only to the storage/clock
/// authority and monitor the host clock used to advance it.
#[derive(Clone)]
pub struct FoundationDbLeaseOracle {
    db: Arc<Database>,
    key: Vec<u8>,
    limits: FoundationDbLimits,
}

impl FoundationDbLeaseOracle {
    fn for_volume(db: Arc<Database>, prefix: &[u8], limits: FoundationDbLimits) -> Result<Self> {
        Ok(Self {
            db,
            key: Keyspace::new(prefix).lease_oracle(),
            limits: limits.validate()?,
        })
    }

    /// Build an oracle for an already-open FoundationDB handle.
    pub fn from_database(
        db: Arc<Database>,
        prefix: impl AsRef<[u8]>,
        limits: FoundationDbLimits,
    ) -> Result<Self> {
        let prefix = prefix.as_ref();
        if prefix.is_empty() || prefix.contains(&0) {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("FoundationDB oracle prefix is invalid"));
        }
        Self::for_volume(db, prefix, limits)
    }
}

#[async_trait]
impl LeaseOracle for FoundationDbLeaseOracle {
    async fn now_ms(&self) -> Result<u64> {
        let db = Arc::clone(&self.db);
        let key = self.key.clone();
        let limits = self.limits;
        db.transact_boxed(
            (),
            move |trx, _| {
                let key = key.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    let current = get_owned(trx, &key)
                        .await?
                        .map(|bytes| decode_oracle_time(&bytes).map_err(TxnError::Fs))
                        .transpose()?;
                    let proposed = system_now_ms().map_err(TxnError::Fs)?;
                    let now_ms = current.unwrap_or(0).max(proposed);
                    let bytes = encode_oracle_time(now_ms);
                    trx.set(&key, &bytes);
                    Ok(now_ms)
                })
            },
            transaction_options(limits, TransactionPolicy::Idempotent),
        )
        .await
        .map_err(TxnError::into_fs)
    }

    fn authority_kind(&self) -> LeaseAuthorityKind {
        LeaseAuthorityKind::SingleAuthority
    }
}

fn lease_oracle_parts(
    db: Arc<Database>,
    prefix: impl AsRef<[u8]>,
    limits: FoundationDbLimits,
) -> Result<(Arc<Database>, Vec<u8>, FoundationDbLimits)> {
    let prefix = prefix.as_ref();
    validate_lease_authority_prefix(prefix)?;
    Ok((db, Keyspace::new(prefix).lease_oracle(), limits.validate()?))
}

/// Write-side authority for a protected shared FoundationDB lease time.
///
/// Run this from the one authority service that is allowed to publish the
/// lease-time record. Storage workers should receive a
/// [`FoundationDbSharedLeaseOracle`] backed by credentials that can read this
/// record but cannot write it. Publishing is monotonic, so an authority clock
/// that moves backwards cannot make an already-issued lease live longer than
/// the previously published time. If the authority is unavailable, readers
/// fail closed rather than substituting their local clocks.
#[derive(Clone)]
pub struct FoundationDbLeaseAuthority {
    db: Arc<Database>,
    key: Vec<u8>,
    limits: FoundationDbLimits,
    stats: Arc<LeaseAuthorityStatsInner>,
    _network: Option<Arc<NetworkAutoStop>>,
}

impl FoundationDbLeaseAuthority {
    /// Build an authority over an already-open FoundationDB handle.
    pub fn from_database(
        db: Arc<Database>,
        prefix: impl AsRef<[u8]>,
        limits: FoundationDbLimits,
    ) -> Result<Self> {
        let (db, key, limits) = lease_oracle_parts(db, prefix, limits)?;
        Ok(Self {
            db,
            key,
            limits,
            stats: Arc::new(LeaseAuthorityStatsInner::default()),
            _network: None,
        })
    }

    /// Boot the process-wide FoundationDB client network and connect the
    /// write-side authority service from a cluster file.
    pub fn connect(
        path: impl AsRef<Path>,
        prefix: impl AsRef<[u8]>,
        limits: FoundationDbLimits,
    ) -> Result<Self> {
        let network = client_network()?;
        let path = path.as_ref().to_str().ok_or_else(|| {
            FsError::new(ErrorCode::Einval).with_message("cluster path is not UTF-8")
        })?;
        let db = Database::from_path(path).map_err(fdb_error)?;
        let (db, key, limits) = lease_oracle_parts(Arc::new(db), prefix, limits)?;
        Ok(Self {
            db,
            key,
            limits,
            stats: Arc::new(LeaseAuthorityStatsInner::default()),
            _network: Some(network),
        })
    }

    /// Return clone-shared publication counters for application-owned
    /// telemetry and alerting.
    pub fn stats(&self) -> LeaseAuthorityStats {
        self.stats.snapshot()
    }

    /// Publish a provider-time sample, retaining the larger value already in
    /// the record when the authority clock moved backwards.
    pub async fn publish_now_ms(&self, now_ms: u64) -> Result<u64> {
        self.publish_now_ms_internal(now_ms, None).await
    }

    /// Publish a provider-time sample while bounding one forward advance.
    ///
    /// A forward advance larger than `max_forward_jump` fails closed without
    /// changing the authority record. This protects lease expiry from a
    /// forward host-clock jump and from an authority that was offline longer
    /// than the deployment's publication policy. Callers should invoke this
    /// method on a cadence shorter than the shortest lease TTL and retain the
    /// error as an operational clock/authority alert.
    pub async fn publish_now_ms_with_max_forward_jump(
        &self,
        now_ms: u64,
        max_forward_jump: Duration,
    ) -> Result<u64> {
        let max_forward_jump_ms = duration_millis(
            max_forward_jump,
            "FoundationDB authority forward-jump bound must be positive",
        )?;
        self.publish_now_ms_internal(now_ms, Some(max_forward_jump_ms))
            .await
    }

    /// Publish an explicit authority sample under a validated lease policy.
    pub async fn publish_now_ms_with_policy(
        &self,
        now_ms: u64,
        policy: LeasePublicationPolicy,
    ) -> Result<u64> {
        let policy = policy.validate()?;
        self.publish_now_ms_with_max_forward_jump(now_ms, policy.max_forward_jump)
            .await
    }

    async fn publish_now_ms_internal(
        &self,
        now_ms: u64,
        max_forward_jump_ms: Option<u64>,
    ) -> Result<u64> {
        self.stats.record_attempt();
        if now_ms == 0 {
            self.stats.record_failure();
            let stats = self.stats.snapshot();
            emit_lease_authority_telemetry("error", &stats);
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("FoundationDB lease authority time must be non-zero"));
        }
        let db = Arc::clone(&self.db);
        let key = self.key.clone();
        let limits = self.limits;
        let result = db
            .transact_boxed(
                (),
                move |trx, _| {
                    let key = key.clone();
                    Box::pin(async move {
                        configure_transaction(trx, limits)?;
                        let current = get_owned(trx, &key)
                            .await?
                            .map(|bytes| decode_oracle_time(&bytes).map_err(TxnError::Fs))
                            .transpose()?;
                        let published = authority_time_sample(current, now_ms, max_forward_jump_ms)
                            .map_err(TxnError::Fs)?;
                        trx.set(&key, &encode_oracle_time(published));
                        Ok(published)
                    })
                },
                transaction_options(limits, TransactionPolicy::Idempotent),
            )
            .await
            .map_err(TxnError::into_fs);
        match result {
            Ok(published) => {
                self.stats.record_success(published);
                let stats = self.stats.snapshot();
                emit_lease_authority_telemetry("ok", &stats);
                Ok(published)
            }
            Err(error) => {
                self.stats.record_failure();
                let stats = self.stats.snapshot();
                emit_lease_authority_telemetry("error", &stats);
                Err(error)
            }
        }
    }

    /// Publish the authority process's current wall-clock sample.
    ///
    /// This method belongs only in the authority service. Storage workers must
    /// use [`FoundationDbSharedLeaseOracle`] and never call it themselves.
    pub async fn publish_system_now_ms(&self) -> Result<u64> {
        self.publish_now_ms(system_now_ms()?).await
    }

    /// Publish the authority process's wall-clock sample with a bounded
    /// forward-jump policy. See [`Self::publish_now_ms_with_max_forward_jump`]
    /// for the safety boundary and scheduling requirement.
    pub async fn publish_system_now_ms_with_max_forward_jump(
        &self,
        max_forward_jump: Duration,
    ) -> Result<u64> {
        self.publish_now_ms_with_max_forward_jump(system_now_ms()?, max_forward_jump)
            .await
    }

    /// Publish the authority process's wall-clock sample under a validated
    /// lease publication policy. The caller remains responsible for invoking
    /// this method on the policy's publication cadence.
    pub async fn publish_system_now_ms_with_policy(
        &self,
        policy: LeasePublicationPolicy,
    ) -> Result<u64> {
        let policy = policy.validate()?;
        self.publish_system_now_ms_with_max_forward_jump(policy.max_forward_jump)
            .await
    }

    /// Create the read-only oracle view for this authority key.
    pub fn shared_oracle(&self) -> FoundationDbSharedLeaseOracle {
        FoundationDbSharedLeaseOracle {
            db: Arc::clone(&self.db),
            key: self.key.clone(),
            limits: self.limits,
            stats: Arc::new(LeaseOracleStatsInner::default()),
            _network: self._network.clone(),
        }
    }
}

/// Read-only view of a protected FoundationDB provider-time authority.
///
/// This oracle never writes the authority record and never consults a local
/// clock. A missing or unavailable authority is an error, so writer-lease
/// operations fail closed. The deployment must enforce the read-only boundary
/// with its FoundationDB tenant/credential policy; this type's API itself has
/// no write method.
#[derive(Clone)]
pub struct FoundationDbSharedLeaseOracle {
    db: Arc<Database>,
    key: Vec<u8>,
    limits: FoundationDbLimits,
    stats: Arc<LeaseOracleStatsInner>,
    _network: Option<Arc<NetworkAutoStop>>,
}

impl FoundationDbSharedLeaseOracle {
    /// Build a read-only oracle over an already-open FoundationDB handle.
    pub fn from_database(
        db: Arc<Database>,
        prefix: impl AsRef<[u8]>,
        limits: FoundationDbLimits,
    ) -> Result<Self> {
        let (db, key, limits) = lease_oracle_parts(db, prefix, limits)?;
        Ok(Self {
            db,
            key,
            limits,
            stats: Arc::new(LeaseOracleStatsInner::default()),
            _network: None,
        })
    }

    /// Boot the process-wide FoundationDB client network and connect a
    /// read-only shared-provider oracle from a cluster file.
    pub fn connect(
        path: impl AsRef<Path>,
        prefix: impl AsRef<[u8]>,
        limits: FoundationDbLimits,
    ) -> Result<Self> {
        let network = client_network()?;
        let path = path.as_ref().to_str().ok_or_else(|| {
            FsError::new(ErrorCode::Einval).with_message("cluster path is not UTF-8")
        })?;
        let db = Database::from_path(path).map_err(fdb_error)?;
        let mut oracle = Self::from_database(Arc::new(db), prefix, limits)?;
        oracle._network = Some(network);
        Ok(oracle)
    }

    /// Return clone-shared read counters for application-owned telemetry and
    /// authority-age/failure alerting.
    pub fn stats(&self) -> LeaseOracleStats {
        self.stats.snapshot()
    }
}

#[async_trait]
impl LeaseOracle for FoundationDbSharedLeaseOracle {
    async fn now_ms(&self) -> Result<u64> {
        self.stats.record_attempt();
        let db = Arc::clone(&self.db);
        let key = self.key.clone();
        let limits = self.limits;
        let result = db
            .transact_boxed(
                (),
                move |trx, _| {
                    let key = key.clone();
                    Box::pin(async move {
                        configure_transaction(trx, limits)?;
                        let published = read_shared_authority_time(trx, &key).await?;
                        Ok(published)
                    })
                },
                transaction_options(limits, TransactionPolicy::Idempotent),
            )
            .await
            .map_err(TxnError::into_fs);
        match result {
            Ok(observed) => {
                self.stats.record_success(observed);
                let stats = self.stats.snapshot();
                emit_lease_oracle_telemetry("ok", &stats);
                Ok(observed)
            }
            Err(error) => {
                self.stats.record_failure();
                let stats = self.stats.snapshot();
                emit_lease_oracle_telemetry("error", &stats);
                Err(error)
            }
        }
    }

    async fn now_ms_in_transaction(&self, transaction: &Transaction) -> Result<u64> {
        self.stats.record_attempt();
        let result = read_shared_authority_time(transaction, &self.key)
            .await
            .map_err(TxnError::into_fs);
        match result {
            Ok(observed) => {
                self.stats.record_success(observed);
                let stats = self.stats.snapshot();
                emit_lease_oracle_telemetry("ok", &stats);
                Ok(observed)
            }
            Err(error) => {
                self.stats.record_failure();
                let stats = self.stats.snapshot();
                emit_lease_oracle_telemetry("error", &stats);
                Err(error)
            }
        }
    }

    fn authority_kind(&self) -> LeaseAuthorityKind {
        LeaseAuthorityKind::SharedProvider
    }
}

/// Configuration for one independent FoundationDB keyspace.
#[derive(Clone)]
pub struct FoundationDbStorageOptions {
    prefix: Vec<u8>,
    durable: bool,
    limits: FoundationDbLimits,
    oracle: Option<Arc<dyn LeaseOracle>>,
    auto_oracle: bool,
    require_shared_lease_authority: bool,
}

impl FoundationDbStorageOptions {
    /// Create options for a volume-scoped key prefix.
    pub fn new(prefix: impl AsRef<[u8]>) -> Self {
        Self {
            prefix: prefix.as_ref().to_vec(),
            durable: false,
            limits: FoundationDbLimits::default(),
            // Lease time is deliberately not inferred from the host clock.
            // Callers must provide a shared oracle or explicitly opt into the
            // persisted single-authority development oracle below.
            oracle: None,
            auto_oracle: false,
            require_shared_lease_authority: false,
        }
    }

    /// Assert that the selected FoundationDB cluster has the required durable
    /// storage/failure policy. This cannot be inferred from a cluster file.
    pub fn with_durable(mut self, durable: bool) -> Self {
        self.durable = durable;
        self
    }

    /// Replace the transaction/value bounds after validating them at open.
    pub fn with_limits(mut self, limits: FoundationDbLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Supply a trusted shared provider-time source for lease expiry.
    pub fn with_oracle<C: LeaseOracle + 'static>(mut self, oracle: C) -> Self {
        self.oracle = Some(Arc::new(oracle));
        self.auto_oracle = false;
        self
    }

    /// Supply an already shared provider-time source.
    pub fn with_oracle_arc(mut self, oracle: Arc<dyn LeaseOracle>) -> Self {
        self.oracle = Some(oracle);
        self.auto_oracle = false;
        self
    }

    /// Supply an application-owned production lease authority.
    ///
    /// The implementation must declare [`LeaseAuthorityKind::SharedProvider`]
    /// from [`LeaseOracle::authority_kind`]. Unverified, local, and
    /// single-authority clocks are rejected when the storage handle is opened.
    /// This is a trust-boundary assertion by the application; it does not turn
    /// a local clock into a distributed authority.
    pub fn with_production_lease_oracle<C: LeaseOracle + 'static>(mut self, oracle: C) -> Self {
        self.oracle = Some(Arc::new(oracle));
        self.auto_oracle = false;
        self.require_shared_lease_authority = true;
        self
    }

    /// Explicitly use the FoundationDB-backed persisted lease oracle.
    ///
    /// This oracle advances persisted time with
    /// `max(persisted_time, local_system_time)`. It is suitable only when one
    /// trusted authority controls this keyspace and the host clock, or for
    /// development/test clusters. It is not a cross-host wall-clock
    /// authority. Production deployments with independent writers must
    /// provide their protected shared authority through [`Self::with_oracle`]
    /// or [`Self::with_oracle_arc`].
    pub fn with_persisted_lease_oracle(mut self) -> Self {
        self.oracle = None;
        self.auto_oracle = true;
        self
    }

    /// Backwards-compatible name for [`Self::with_oracle`].
    pub fn with_clock<C: LeaseClock + 'static>(self, clock: C) -> Self {
        self.with_oracle(clock)
    }

    /// Backwards-compatible name for [`Self::with_oracle_arc`].
    pub fn with_clock_arc(self, clock: Arc<dyn LeaseClock>) -> Self {
        self.with_oracle_arc(clock)
    }

    /// Explicitly keep lease operations fail closed until an oracle is supplied.
    ///
    /// This is also the default for [`Self::new`]. Lease operations return
    /// `ENOTSUP` until an oracle is configured; block and metadata reads still
    /// work.
    pub fn without_lease_oracle(mut self) -> Self {
        self.oracle = None;
        self.auto_oracle = false;
        self
    }

    /// Explicitly opt into the local system clock for development only.
    ///
    /// This is intentionally not part of [`Self::new`]'s defaults. It is not a
    /// cross-host provider-time authority and must not be used for a mounted
    /// volume whose writers can run on machines with independent clocks.
    pub fn with_system_clock(self) -> Self {
        self.with_oracle(SystemLeaseClock)
    }

    fn validate(self) -> Result<Self> {
        validate_prefix(&self.prefix)?;
        let limits = self.limits.validate()?;
        if self.require_shared_lease_authority {
            let Some(oracle) = self.oracle.as_ref() else {
                return Err(FsError::enotsup("FoundationDB production lease authority")
                    .with_message(
                        "a shared provider lease authority is required for production leases",
                    ));
            };
            if oracle.authority_kind() != LeaseAuthorityKind::SharedProvider {
                return Err(FsError::enotsup("FoundationDB production lease authority")
                    .with_message(
                        "the selected lease oracle is not declared as a shared provider authority",
                    ));
            }
        }
        let affected_bytes = metadata_publication_affected_bytes(
            &self.prefix,
            limits.metadata_chunk_bytes,
            limits.max_metadata_bytes,
        )?;
        if affected_bytes > FOUNDATIONDB_MAX_TRANSACTION_BYTES {
            return Err(metadata_transaction_too_large(affected_bytes));
        }
        Ok(Self { limits, ..self })
    }
}

impl Default for FoundationDbStorageOptions {
    fn default() -> Self {
        Self::new("mount-rs")
    }
}

/// A connected FoundationDB volume. Metadata and block stores are separate
/// handles over the same volume-scoped keyspace.
#[derive(Clone)]
pub struct FoundationDbStorage {
    inner: Arc<Inner>,
}

impl FoundationDbStorage {
    /// Build a storage volume from an already-created database handle.
    ///
    /// The caller must have called `foundationdb::boot()` and must retain its
    /// network guard until all handles are dropped.
    pub fn from_database(db: Database, options: FoundationDbStorageOptions) -> Result<Self> {
        Self::from_database_with_network(db, options, None)
    }

    fn from_database_with_network(
        db: Database,
        options: FoundationDbStorageOptions,
        network: Option<Arc<NetworkAutoStop>>,
    ) -> Result<Self> {
        let options = options.validate()?;
        let db = Arc::new(db);
        let prefix = options.prefix;
        let limits = options.limits;
        let oracle = match (options.oracle, options.auto_oracle) {
            (Some(oracle), _) => Some(oracle),
            (None, true) => Some(Arc::new(FoundationDbLeaseOracle::for_volume(
                Arc::clone(&db),
                &prefix,
                limits,
            )?) as Arc<dyn LeaseOracle>),
            (None, false) => None,
        };
        Ok(Self {
            inner: Arc::new(Inner {
                db,
                prefix,
                durable: options.durable,
                limits,
                oracle,
                _network: network,
            }),
        })
    }

    /// Open a database using an explicit FoundationDB cluster file.
    ///
    /// This does not boot the native client network; see [`Self::from_database`].
    pub fn from_cluster_file(
        path: impl AsRef<Path>,
        options: FoundationDbStorageOptions,
    ) -> Result<Self> {
        let path = path.as_ref().to_str().ok_or_else(|| {
            FsError::new(ErrorCode::Einval).with_message("cluster path is not UTF-8")
        })?;
        let db = Database::from_path(path).map_err(fdb_error)?;
        Self::from_database(db, options)
    }

    /// Open the platform default cluster file.
    pub fn from_default_cluster_file(options: FoundationDbStorageOptions) -> Result<Self> {
        Self::from_cluster_file(foundationdb::default_config_path(), options)
    }

    /// Boot the process-wide FoundationDB client network and open a cluster
    /// file. The returned storage retains the network guard for as long as any
    /// clone of this storage remains alive.
    ///
    /// FoundationDB permits one client-network initialization per process, so
    /// all consumer-facing providers in this process share the same guard.
    /// The native client remains alive across sequential opens until the
    /// application calls [`shutdown_client_network`] at its terminal boundary,
    /// after every provider handle and operation has been dropped. Applications
    /// that own a different network lifecycle should use [`Self::from_database`]
    /// instead and retain their own guard.
    pub fn connect(path: impl AsRef<Path>, options: FoundationDbStorageOptions) -> Result<Self> {
        let network = client_network()?;
        let path = path.as_ref().to_str().ok_or_else(|| {
            FsError::new(ErrorCode::Einval).with_message("cluster path is not UTF-8")
        })?;
        let db = Database::from_path(path).map_err(fdb_error)?;
        Self::from_database_with_network(db, options, Some(network))
    }

    pub fn metadata(&self) -> FoundationDbMetadataStore {
        FoundationDbMetadataStore(Arc::clone(&self.inner))
    }

    pub fn blocks(&self) -> FoundationDbBlockStore {
        FoundationDbBlockStore(Arc::clone(&self.inner))
    }
}

struct Inner {
    db: Arc<Database>,
    prefix: Vec<u8>,
    durable: bool,
    limits: FoundationDbLimits,
    oracle: Option<Arc<dyn LeaseOracle>>,
    _network: Option<Arc<NetworkAutoStop>>,
}

/// Retry policy for a FoundationDB transaction closure.
///
/// FoundationDB can report an error after a commit was applied but before the
/// client received the acknowledgement. Retrying a closure that allocates a
/// lease, changes its expiry, releases it, or publishes a revision can then
/// execute the operation twice. Those metadata operations therefore fail
/// closed on a maybe-committed result. Content-addressed block operations and
/// reads are safe to retry because repeating them has the same observable
/// result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransactionPolicy {
    Idempotent,
    FailClosedOnMaybeCommitted,
}

impl TransactionPolicy {
    fn is_idempotent(self) -> bool {
        matches!(self, Self::Idempotent)
    }
}

/// Build the options passed to the foundationdb-rs production retry loop.
///
/// This must remain the single construction point for both the metadata/block
/// wrappers and the storage-backed lease oracle. In particular, changing the
/// retry limit or timeout must not accidentally change whether a commit-unknown
/// result is replayed.
fn transaction_options(limits: FoundationDbLimits, policy: TransactionPolicy) -> TransactOption {
    TransactOption {
        retry_limit: Some(limits.transaction_retry_limit as u32),
        time_out: Some(limits.transaction_timeout),
        // foundationdb-rs retries a maybe-committed result only when this
        // flag is true. Keep it false for metadata mutations whose
        // acknowledgement could be lost after the mutation committed.
        is_idempotent: policy.is_idempotent(),
    }
}

impl Inner {
    async fn transact_with_policy<T, F>(
        &self,
        data: (),
        f: F,
        policy: TransactionPolicy,
    ) -> Result<T>
    where
        T: Send,
        F: for<'a> FnMut(
                &'a Transaction,
                &'a mut (),
            ) -> Pin<Box<dyn Future<Output = TxnResult<T>> + Send + 'a>>
            + Send,
    {
        let options = transaction_options(self.limits, policy);
        self.db
            .transact_boxed(data, f, options)
            .await
            .map_err(TxnError::into_fs)
    }

    async fn transact_idempotent<T, F>(&self, data: (), f: F) -> Result<T>
    where
        T: Send,
        F: for<'a> FnMut(
                &'a Transaction,
                &'a mut (),
            ) -> Pin<Box<dyn Future<Output = TxnResult<T>> + Send + 'a>>
            + Send,
    {
        self.transact_with_policy(data, f, TransactionPolicy::Idempotent)
            .await
    }

    async fn transact_metadata<T, F>(&self, data: (), f: F) -> Result<T>
    where
        T: Send,
        F: for<'a> FnMut(
                &'a Transaction,
                &'a mut (),
            ) -> Pin<Box<dyn Future<Output = TxnResult<T>> + Send + 'a>>
            + Send,
    {
        self.transact_with_policy(data, f, TransactionPolicy::FailClosedOnMaybeCommitted)
            .await
    }
}

fn configure_transaction(trx: &Transaction, limits: FoundationDbLimits) -> TxnResult<()> {
    let timeout_ms = i32::try_from(limits.transaction_timeout.as_millis())
        .map_err(|_| TxnError::Fs(FsError::new(ErrorCode::Einval)))?;
    trx.set_option(TransactionOption::Timeout(timeout_ms))?;
    trx.set_option(TransactionOption::RetryLimit(
        limits.transaction_retry_limit,
    ))?;
    Ok(())
}

#[derive(Debug)]
enum TxnError {
    Fdb(FdbError),
    Fs(FsError),
}

type TxnResult<T> = std::result::Result<T, TxnError>;

impl From<FdbError> for TxnError {
    fn from(error: FdbError) -> Self {
        Self::Fdb(error)
    }
}

impl TryFrom<TxnError> for FdbError {
    type Error = TxnError;

    fn try_from(error: TxnError) -> std::result::Result<Self, Self::Error> {
        match error {
            TxnError::Fdb(error) => Ok(error),
            other => Err(other),
        }
    }
}

impl std::fmt::Display for TxnError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Fdb(error) => write!(formatter, "FoundationDB error {}: {error}", error.code()),
            Self::Fs(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for TxnError {}

impl TxnError {
    fn into_fs(self) -> FsError {
        match self {
            Self::Fdb(error) => fdb_error(error),
            Self::Fs(error) => error,
        }
    }
}

fn fdb_error(error: FdbError) -> FsError {
    let code = error.code();
    let message = error.message();
    if error.is_maybe_committed() {
        ambiguous_commit_error(code, message)
    } else {
        FsError::backend(format!("FoundationDB error {code}: {message}"))
    }
}

enum ClientNetworkState {
    Uninitialized,
    Running(Arc<NetworkAutoStop>),
    Failed(String),
    Stopping,
    Stopped,
}

static CLIENT_NETWORK: Mutex<ClientNetworkState> = Mutex::new(ClientNetworkState::Uninitialized);
static CLIENT_NETWORK_STOPPED: Condvar = Condvar::new();

fn client_network_lock() -> Result<std::sync::MutexGuard<'static, ClientNetworkState>> {
    CLIENT_NETWORK
        .lock()
        .map_err(|_| FsError::backend("FoundationDB client network state lock was poisoned"))
}

fn client_network() -> Result<Arc<NetworkAutoStop>> {
    let mut state = client_network_lock()?;
    match &*state {
        ClientNetworkState::Running(network) => return Ok(Arc::clone(network)),
        ClientNetworkState::Failed(message) => return Err(FsError::backend(message.clone())),
        ClientNetworkState::Stopping | ClientNetworkState::Stopped => {
            return Err(FsError::new(ErrorCode::Eio)
                .with_message("FoundationDB client network has been shut down"));
        }
        ClientNetworkState::Uninitialized => {}
    }

    let network = FdbApiBuilder::default()
        .build()
        .map_err(|error| format!("FoundationDB client API initialization failed: {error}"))
        .and_then(|builder| {
            unsafe { builder.boot() }.map_err(|error| {
                format!("FoundationDB client network initialization failed: {error}")
            })
        });
    match network {
        Ok(network) => {
            let network = Arc::new(network);
            *state = ClientNetworkState::Running(Arc::clone(&network));
            Ok(network)
        }
        Err(message) => {
            *state = ClientNetworkState::Failed(message.clone());
            Err(FsError::backend(message))
        }
    }
}

/// Stop and join the process-wide FoundationDB client network at the
/// application's terminal boundary.
///
/// All providers, databases, in-flight operations, and application runtimes
/// using this network must be dropped first. An active managed provider guard
/// returns `Ebusy` and leaves the network running, so the caller can finish
/// draining and retry. Once shutdown begins, managed connections cannot be
/// opened again in this process. Concurrent shutdown callers wait until the
/// native network thread has joined.
pub fn shutdown_client_network() -> Result<()> {
    let network = {
        let mut state = client_network_lock()?;
        loop {
            match &*state {
                ClientNetworkState::Stopped => return Ok(()),
                ClientNetworkState::Stopping => {
                    state = CLIENT_NETWORK_STOPPED.wait(state).map_err(|_| {
                        FsError::backend("FoundationDB client network state lock was poisoned")
                    })?;
                }
                ClientNetworkState::Running(network) if Arc::strong_count(network) > 1 => {
                    return Err(FsError::new(ErrorCode::Ebusy).with_message(
                        "FoundationDB client network still has active provider handles",
                    ));
                }
                ClientNetworkState::Running(_) => {
                    let ClientNetworkState::Running(network) =
                        std::mem::replace(&mut *state, ClientNetworkState::Stopping)
                    else {
                        unreachable!("running client network changed while locked")
                    };
                    break network;
                }
                ClientNetworkState::Uninitialized | ClientNetworkState::Failed(_) => {
                    *state = ClientNetworkState::Stopped;
                    CLIENT_NETWORK_STOPPED.notify_all();
                    return Ok(());
                }
            }
        }
    };

    // The FoundationDB wrapper aborts on stop failure and panics if joining
    // its network thread fails. Never unwind while the shared state remains
    // Stopping: another terminal caller would otherwise wait forever.
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(network))).is_err() {
        std::process::abort();
    }

    let mut state = client_network_lock()?;
    *state = ClientNetworkState::Stopped;
    CLIENT_NETWORK_STOPPED.notify_all();
    Ok(())
}

fn ambiguous_commit_error(code: i32, message: impl std::fmt::Display) -> FsError {
    FsError::new(ErrorCode::Eio).with_message(format!(
        "FoundationDB transaction outcome is ambiguous after commit attempt (error {code}: {message}); reconcile state before retrying"
    ))
}

async fn get_owned(trx: &Transaction, key: &[u8]) -> TxnResult<Option<Vec<u8>>> {
    Ok(trx.get(key, false).await?.map(|value| value.to_vec()))
}

async fn read_shared_authority_time(trx: &Transaction, key: &[u8]) -> TxnResult<u64> {
    get_owned(trx, key)
        .await?
        .map(|bytes| decode_oracle_time(&bytes).map_err(TxnError::Fs))
        .transpose()?
        .ok_or_else(|| {
            TxnError::Fs(
                FsError::enotsup("FoundationDB shared lease authority")
                    .with_message("the shared lease authority has not published a time sample"),
            )
        })
}

#[derive(Debug, Clone)]
struct LeaseRecord {
    owner: String,
    fence: u64,
    expires_at_ms: u64,
}

fn encode_lease(lease: &LeaseRecord) -> Result<Vec<u8>> {
    let owner = lease.owner.as_bytes();
    let owner_len = u32::try_from(owner.len()).map_err(|_| {
        FsError::new(ErrorCode::Enametoolong).with_message("lease owner is too long")
    })?;
    let mut bytes = Vec::with_capacity(LEASE_HEADER_BYTES + owner.len());
    bytes.extend_from_slice(LEASE_MAGIC);
    bytes.extend_from_slice(&lease.fence.to_be_bytes());
    bytes.extend_from_slice(&lease.expires_at_ms.to_be_bytes());
    bytes.extend_from_slice(&owner_len.to_be_bytes());
    bytes.extend_from_slice(owner);
    Ok(bytes)
}

fn decode_lease(bytes: &[u8]) -> Result<LeaseRecord> {
    if bytes.len() < LEASE_HEADER_BYTES || &bytes[..4] != LEASE_MAGIC {
        return Err(backend_error("malformed FoundationDB lease record"));
    }
    let fence = u64::from_be_bytes(bytes[4..12].try_into().unwrap());
    let expires_at_ms = u64::from_be_bytes(bytes[12..20].try_into().unwrap());
    let owner_len = u32::from_be_bytes(bytes[20..24].try_into().unwrap()) as usize;
    if owner_len != bytes.len() - LEASE_HEADER_BYTES {
        return Err(backend_error("malformed FoundationDB lease owner"));
    }
    let owner = String::from_utf8(bytes[24..].to_vec())
        .map_err(|_| backend_error("FoundationDB lease owner is not UTF-8"))?;
    if owner.is_empty() || fence == 0 || expires_at_ms == 0 {
        return Err(backend_error("invalid FoundationDB lease record"));
    }
    Ok(LeaseRecord {
        owner,
        fence,
        expires_at_ms,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Manifest {
    revision: u64,
    chunk_count: u32,
    payload_len: u64,
}

fn encode_manifest(manifest: Manifest) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MANIFEST_BYTES);
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.extend_from_slice(&manifest.revision.to_be_bytes());
    bytes.extend_from_slice(&manifest.chunk_count.to_be_bytes());
    bytes.extend_from_slice(&manifest.payload_len.to_be_bytes());
    bytes
}

fn decode_manifest(bytes: &[u8]) -> Result<Manifest> {
    if bytes.len() != MANIFEST_BYTES || &bytes[..4] != MANIFEST_MAGIC {
        return Err(backend_error("malformed FoundationDB metadata manifest"));
    }
    let manifest = Manifest {
        revision: u64::from_be_bytes(bytes[4..12].try_into().unwrap()),
        chunk_count: u32::from_be_bytes(bytes[12..16].try_into().unwrap()),
        payload_len: u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
    };
    if manifest.revision == 0 || manifest.chunk_count == 0 || manifest.payload_len == 0 {
        return Err(backend_error("invalid FoundationDB metadata manifest"));
    }
    Ok(manifest)
}

fn encode_oracle_time(now_ms: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(ORACLE_BYTES);
    bytes.extend_from_slice(ORACLE_MAGIC);
    bytes.extend_from_slice(&now_ms.to_be_bytes());
    bytes
}

fn decode_oracle_time(bytes: &[u8]) -> Result<u64> {
    if bytes.len() != ORACLE_BYTES || &bytes[..4] != ORACLE_MAGIC {
        return Err(backend_error("malformed FoundationDB lease oracle record"));
    }
    let now_ms = u64::from_be_bytes(bytes[4..12].try_into().unwrap());
    if now_ms == 0 {
        return Err(backend_error("invalid FoundationDB lease oracle time"));
    }
    Ok(now_ms)
}

fn authority_time_sample(
    current: Option<u64>,
    proposed: u64,
    max_forward_jump_ms: Option<u64>,
) -> Result<u64> {
    if proposed == 0 {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("FoundationDB lease authority time must be non-zero"));
    }
    let current = current.unwrap_or(0);
    if let Some(max_forward_jump_ms) = max_forward_jump_ms
        && current != 0
        && proposed > current
        && proposed - current > max_forward_jump_ms
    {
        return Err(FsError::new(ErrorCode::Eio).with_message(
            "FoundationDB lease authority clock advanced beyond the configured safety bound",
        ));
    }
    Ok(current.max(proposed))
}

fn encode_last_fence(fence: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(FENCE_BYTES);
    bytes.extend_from_slice(FENCE_MAGIC);
    bytes.extend_from_slice(&fence.to_be_bytes());
    bytes
}

fn decode_last_fence(bytes: &[u8]) -> Result<u64> {
    if bytes.len() != FENCE_BYTES || &bytes[..4] != FENCE_MAGIC {
        return Err(backend_error("malformed FoundationDB fence record"));
    }
    let fence = u64::from_be_bytes(bytes[4..12].try_into().unwrap());
    if fence == 0 {
        return Err(backend_error("invalid FoundationDB fence record"));
    }
    Ok(fence)
}

fn validate_prefix(prefix: &[u8]) -> Result<()> {
    if prefix.is_empty() {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("FoundationDB volume prefix must not be empty"));
    }
    if prefix.contains(&0) {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("FoundationDB volume prefix must not contain NUL"));
    }
    let block_key_bytes = KEY_SEPARATOR.len() + BLOCK_SUFFIX.len() + BLOCK_ID_TEXT_BYTES;
    if prefix.len() + block_key_bytes > FOUNDATIONDB_MAX_KEY_BYTES {
        return Err(FsError::new(ErrorCode::Enametoolong)
            .with_message("FoundationDB volume prefix is too long"));
    }
    Ok(())
}

fn validate_lease_authority_prefix(prefix: &[u8]) -> Result<()> {
    if prefix.is_empty() || prefix.contains(&0) {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("FoundationDB lease authority prefix is invalid"));
    }
    let key_bytes = prefix
        .len()
        .checked_add(KEY_SEPARATOR.len())
        .and_then(|bytes| bytes.checked_add(LEASE_ORACLE_SUFFIX.len()))
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
    if key_bytes > FOUNDATIONDB_MAX_KEY_BYTES {
        return Err(FsError::new(ErrorCode::Enametoolong)
            .with_message("FoundationDB lease authority prefix is too long"));
    }
    Ok(())
}

fn validate_owner(owner: &str) -> Result<()> {
    if owner.is_empty() {
        return Err(FsError::new(ErrorCode::Einval));
    }
    if owner.len() > FOUNDATIONDB_MAX_VALUE_BYTES - LEASE_HEADER_BYTES {
        return Err(FsError::new(ErrorCode::Enametoolong)
            .with_message("FoundationDB lease owner is too long"));
    }
    Ok(())
}

fn ttl_ms(ttl: Duration) -> Result<u64> {
    let ttl = u64::try_from(ttl.as_millis())
        .map_err(|_| FsError::new(ErrorCode::Eoverflow).with_message("lease TTL overflows"))?;
    if ttl == 0 {
        return Err(FsError::new(ErrorCode::Einval));
    }
    Ok(ttl)
}

fn lease_from_input(lease: &WriterLease) -> Result<LeaseRecord> {
    validate_owner(&lease.owner)?;
    if lease.fence == 0 || lease.expires_at_ms == 0 {
        return Err(stale());
    }
    Ok(LeaseRecord {
        owner: lease.owner.clone(),
        fence: lease.fence,
        expires_at_ms: lease.expires_at_ms,
    })
}

fn stale() -> FsError {
    FsError::new(ErrorCode::Estale).with_syscall("FoundationDB metadata lease")
}

fn stale_backing() -> FsError {
    FsError::new(ErrorCode::Estale).with_syscall("FoundationDB concurrent backing authority")
}

fn parse_backing_bytes(raw: &[u8]) -> Option<ConcurrentBackingId> {
    let bytes: [u8; 16] = raw.try_into().ok()?;
    ConcurrentBackingId::from_bytes(bytes).ok()
}

fn decode_concurrent_mode_state(
    raw_mode: Option<Vec<u8>>,
    raw_backing: Option<Vec<u8>>,
) -> TxnResult<ConcurrentModeState> {
    match (raw_mode.as_deref(), raw_backing.as_deref()) {
        (None, None) => Ok(ConcurrentModeState::Legacy),
        (Some(mode), None) if mode == CONCURRENT_WRITE_MODE => Ok(ConcurrentModeState::Mrc1),
        (Some(mode), Some(id)) if mode == BOUND_CONCURRENT_WRITE_MODE => parse_backing_bytes(id)
            .map(ConcurrentModeState::Mrc2)
            .ok_or_else(|| {
                TxnError::Fs(backend_error(
                    "FoundationDB concurrent metadata backing ID is invalid",
                ))
            }),
        _ => Err(TxnError::Fs(backend_error(
            "FoundationDB metadata write mode and backing ID disagree",
        ))),
    }
}

// The removed MRC1 publisher's exact mode decoder remains as a regression
// oracle for old-client rejection after migration.
#[cfg(test)]
fn decode_concurrent_write_mode(raw: Option<Vec<u8>>) -> TxnResult<bool> {
    match raw {
        None => Ok(false),
        Some(value) if value == CONCURRENT_WRITE_MODE => Ok(true),
        Some(_) => Err(TxnError::Fs(backend_error(
            "FoundationDB metadata write mode marker is invalid",
        ))),
    }
}

fn require_legacy_write_mode(raw: Option<Vec<u8>>) -> TxnResult<()> {
    match raw.as_deref() {
        None => Ok(()),
        Some(mode) if mode == CONCURRENT_WRITE_MODE || mode == BOUND_CONCURRENT_WRITE_MODE => {
            Err(TxnError::Fs(
                FsError::enotsup("FoundationDB fenced metadata writer")
                    .with_message("this volume uses concurrent revision publication"),
            ))
        }
        Some(_) => Err(TxnError::Fs(backend_error(
            "FoundationDB metadata write mode marker is invalid",
        ))),
    }
}

fn require_oracle(inner: &Inner) -> Result<Arc<dyn LeaseOracle>> {
    inner.oracle.clone().ok_or_else(|| {
        FsError::enotsup("FoundationDB metadata lease clock")
            .with_message("a shared FoundationDB lease oracle is required for writer leases")
    })
}

fn lease_matches(current: &LeaseRecord, requested: &LeaseRecord, now_ms: u64) -> bool {
    current.owner == requested.owner
        && current.fence == requested.fence
        && current.expires_at_ms == requested.expires_at_ms
        && current.expires_at_ms > now_ms
}

/// Decide a fenced writer takeover using only persisted fence/expiry values.
/// The transaction still owns the read/write conflict boundary around this decision.
fn plan_writer_acquisition(
    current: Option<(u64, u64)>,
    last_fence: Option<u64>,
    now_ms: u64,
    ttl_ms: u64,
) -> Result<(u64, u64)> {
    if current.is_some_and(|(_, expires_at_ms)| expires_at_ms > now_ms) {
        return Err(FsError::new(ErrorCode::Eagain).with_syscall("acquire writer"));
    }
    let previous_fence = last_fence
        .unwrap_or(0)
        .max(current.map_or(0, |(fence, _)| fence));
    let fence = previous_fence
        .checked_add(1)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
    let expires_at_ms = now_ms
        .checked_add(ttl_ms)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
    Ok((fence, expires_at_ms))
}

/// Renew only the exact live persisted token, without changing its fence.
/// The surrounding FoundationDB transaction owns the record read and write.
fn plan_writer_renewal(
    current: &LeaseRecord,
    requested: &LeaseRecord,
    now_ms: u64,
    ttl_ms: u64,
) -> Result<(u64, u64)> {
    if !lease_matches(current, requested, now_ms) {
        return Err(stale());
    }
    let expires_at_ms = now_ms
        .checked_add(ttl_ms)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
    Ok((requested.fence, expires_at_ms))
}

#[cfg(kani)]
mod verification {
    use super::*;

    /// Prove the scalar decision made inside the authority's publication
    /// transaction. This does not establish the transaction's atomicity,
    /// clock trust, or the deployment's publication cadence.
    #[kani::proof]
    #[kani::unwind(12)]
    fn authority_time_sample_is_monotonic_and_bounds_forward_jumps() {
        let current_ms: u64 = kani::any();
        let proposed_ms: u64 = kani::any();
        let jump_bound_ms: u64 = kani::any();
        let has_current: bool = kani::any();
        let has_jump_bound: bool = kani::any();

        let current = has_current.then_some(current_ms);
        let jump_bound = has_jump_bound.then_some(jump_bound_ms);
        let result = authority_time_sample(current, proposed_ms, jump_bound);

        let previous = if has_current {
            u128::from(current_ms)
        } else {
            0
        };
        let proposed = u128::from(proposed_ms);
        let excessive_jump = has_jump_bound
            && previous != 0
            && proposed > previous
            && proposed - previous > u128::from(jump_bound_ms);
        let expected = if previous > proposed {
            previous
        } else {
            proposed
        };

        kani::cover!(proposed_ms == 0);
        kani::cover!(result.is_ok() && !has_current && has_jump_bound);
        kani::cover!(result.is_ok() && has_current && previous > proposed);
        kani::cover!(
            result.is_ok()
                && has_current
                && has_jump_bound
                && previous > 0
                && proposed > previous
                && proposed - previous == u128::from(jump_bound_ms)
        );
        kani::cover!(excessive_jump);
        kani::cover!(result.is_ok() && has_current && !has_jump_bound && proposed > previous);
        kani::cover!(result.is_ok() && proposed_ms == u64::MAX);

        match result {
            Err(error) if proposed_ms == 0 => assert_eq!(error.code, ErrorCode::Einval),
            Err(error) => {
                assert!(excessive_jump);
                assert_eq!(error.code, ErrorCode::Eio);
            }
            Ok(published) => {
                assert!(proposed_ms != 0 && !excessive_jump);
                assert_eq!(u128::from(published), expected);
                assert!(u128::from(published) >= previous);
                assert!(published > 0);
            }
        }
    }

    /// One numeric takeover decision, with arbitrary full-range persisted values.
    /// It does not model the FoundationDB transaction or lease-oracle trust boundary.
    #[kani::proof]
    #[kani::unwind(12)]
    fn writer_takeover_is_fenced_and_checked() {
        let now_ms: u64 = kani::any();
        let ttl_ms: u64 = kani::any();
        let current_fence: u64 = kani::any();
        let current_expiry: u64 = kani::any();
        let last_fence: u64 = kani::any();
        let has_current: bool = kani::any();
        let has_last: bool = kani::any();
        kani::assume(now_ms > 0 && ttl_ms > 0);
        kani::assume(!has_current || (current_fence > 0 && current_expiry > 0));
        kani::assume(!has_last || last_fence > 0);

        let current = has_current.then_some((current_fence, current_expiry));
        let last = has_last.then_some(last_fence);
        let planned = plan_writer_acquisition(current, last, now_ms, ttl_ms);
        let live = has_current && current_expiry > now_ms;
        let previous_fence = last.unwrap_or(0).max(current.map_or(0, |value| value.0));
        let grantable = !live
            && previous_fence.checked_add(1).is_some()
            && now_ms.checked_add(ttl_ms).is_some();

        kani::cover!(planned.is_ok());
        kani::cover!(live);
        kani::cover!(!live && previous_fence == u64::MAX);
        kani::cover!(!live && previous_fence < u64::MAX && now_ms.checked_add(ttl_ms).is_none());
        assert_eq!(planned.is_ok(), grantable);
        if let Ok((fence, expiry)) = planned {
            assert!(fence > 0 && fence > last.unwrap_or(0));
            assert!(!has_current || fence > current_fence);
            assert_eq!(expiry, now_ms.checked_add(ttl_ms).unwrap());
            assert!(expiry > now_ms);
        }
    }

    /// The numeric decision called by writer renewal also uses the exact
    /// lease-token predicate shared with release and fenced publication.
    /// The FoundationDB transaction, oracle trust, and commit acknowledgement
    /// remain outside this harness.
    #[kani::proof]
    #[kani::unwind(16)]
    fn writer_renewal_requires_exact_live_fence_and_checked_expiry() {
        let now_ms: u64 = kani::any();
        let ttl_ms: u64 = kani::any();
        let current_fence: u64 = kani::any();
        let current_expiry: u64 = kani::any();
        let requested_fence: u64 = kani::any();
        let requested_expiry: u64 = kani::any();
        let same_owner: bool = kani::any();
        // Custom lease oracles may return zero. The TTL validator admits only
        // positive values; decoded leases and input tokens have positive fields.
        kani::assume(ttl_ms > 0);
        kani::assume(current_fence > 0 && current_expiry > 0);
        kani::assume(requested_fence > 0 && requested_expiry > 0);

        let current = LeaseRecord {
            owner: "writer".to_owned(),
            fence: current_fence,
            expires_at_ms: current_expiry,
        };
        let requested = LeaseRecord {
            owner: if same_owner { "writer" } else { "other" }.to_owned(),
            fence: requested_fence,
            expires_at_ms: requested_expiry,
        };
        let exact_live = same_owner
            && current_fence == requested_fence
            && current_expiry == requested_expiry
            && current_expiry > now_ms;
        assert_eq!(lease_matches(&current, &requested, now_ms), exact_live);

        let planned = plan_writer_renewal(&current, &requested, now_ms, ttl_ms);
        let grantable = exact_live && now_ms.checked_add(ttl_ms).is_some();
        kani::cover!(grantable);
        kani::cover!(now_ms == 0 && grantable);
        kani::cover!(
            !same_owner
                && current_fence == requested_fence
                && current_expiry == requested_expiry
                && current_expiry > now_ms
        );
        kani::cover!(
            same_owner
                && current_fence != requested_fence
                && current_expiry == requested_expiry
                && current_expiry > now_ms
        );
        kani::cover!(
            same_owner
                && current_fence == requested_fence
                && current_expiry != requested_expiry
                && current_expiry > now_ms
        );
        kani::cover!(
            same_owner
                && current_fence == requested_fence
                && current_expiry == requested_expiry
                && current_expiry == now_ms
        );
        kani::cover!(
            same_owner
                && current_fence == requested_fence
                && current_expiry == requested_expiry
                && current_expiry < now_ms
        );
        kani::cover!(exact_live && now_ms.checked_add(ttl_ms).is_none());

        assert_eq!(planned.is_ok(), grantable);
        match planned {
            Ok((fence, expiry)) => {
                assert_eq!(fence, current_fence);
                assert_eq!(fence, requested_fence);
                assert_eq!(expiry, now_ms.checked_add(ttl_ms).unwrap());
                assert!(expiry > now_ms);
            }
            Err(error) if !exact_live => assert_eq!(error.code, ErrorCode::Estale),
            Err(error) => assert_eq!(error.code, ErrorCode::Eoverflow),
        }
    }
}

struct Keyspace {
    prefix: Vec<u8>,
}

impl Keyspace {
    fn new(prefix: &[u8]) -> Self {
        Self {
            prefix: prefix.to_vec(),
        }
    }

    fn key(&self, suffix: &[u8]) -> Vec<u8> {
        let mut key = Vec::with_capacity(self.prefix.len() + 1 + suffix.len());
        key.extend_from_slice(&self.prefix);
        key.extend_from_slice(KEY_SEPARATOR);
        key.extend_from_slice(suffix);
        key
    }

    fn manifest(&self) -> Vec<u8> {
        self.key(METADATA_MANIFEST_SUFFIX)
    }

    fn chunks(&self) -> Vec<u8> {
        self.key(METADATA_CHUNK_SUFFIX)
    }

    fn lease(&self) -> Vec<u8> {
        self.key(METADATA_LEASE_SUFFIX)
    }

    fn fence(&self) -> Vec<u8> {
        self.key(METADATA_FENCE_SUFFIX)
    }

    fn write_mode(&self) -> Vec<u8> {
        self.key(METADATA_WRITE_MODE_SUFFIX)
    }

    fn metadata_backing(&self) -> Vec<u8> {
        self.key(METADATA_BACKING_SUFFIX)
    }

    fn lease_oracle(&self) -> Vec<u8> {
        self.key(LEASE_ORACLE_SUFFIX)
    }

    fn flush(&self) -> Vec<u8> {
        self.key(FLUSH_SUFFIX)
    }

    fn block(&self, id: &BlockId) -> Vec<u8> {
        let mut key = self.key(BLOCK_SUFFIX);
        key.extend_from_slice(id.0.as_bytes());
        key
    }

    fn block_authority(&self) -> Vec<u8> {
        self.key(BLOCK_AUTHORITY_SUFFIX)
    }
}

fn range_end(prefix: &[u8]) -> Result<Vec<u8>> {
    let mut end = prefix.to_vec();
    for index in (0..end.len()).rev() {
        if end[index] != u8::MAX {
            end[index] += 1;
            end.truncate(index + 1);
            return Ok(end);
        }
    }
    Err(FsError::new(ErrorCode::Eoverflow).with_message("FoundationDB key prefix has no range end"))
}

fn add_affected_bytes(total: &mut usize, bytes: usize) -> Result<()> {
    *total = total
        .checked_add(bytes)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
    Ok(())
}

fn key_conflict_range_bytes(key_bytes: usize) -> Result<usize> {
    key_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(1))
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))
}

fn range_conflict_bytes(begin_key_bytes: usize, end_key_bytes: usize) -> Result<usize> {
    begin_key_bytes
        .checked_add(end_key_bytes)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))
}

fn metadata_chunk_count(payload_len: usize, metadata_chunk_bytes: usize) -> Result<usize> {
    if metadata_chunk_bytes == 0 {
        return Err(FsError::new(ErrorCode::Einval));
    }
    let chunk_count = payload_len.div_ceil(metadata_chunk_bytes);
    u32::try_from(chunk_count)
        .map_err(|_| FsError::new(ErrorCode::Efbig).with_message("too many metadata chunks"))?;
    Ok(chunk_count)
}

fn metadata_chunk_key(prefix: &[u8], index: u32) -> Vec<u8> {
    let mut key = Vec::with_capacity(prefix.len() + std::mem::size_of::<u32>());
    key.extend_from_slice(prefix);
    key.extend_from_slice(&index.to_be_bytes());
    key
}

/// Return the smallest chunk key that must be cleared before a publication.
///
/// Existing chunks covered by the new manifest are overwritten by the same
/// transaction. Clearing the whole range on every publication adds an
/// unnecessary range mutation and conflict range to the common case where the
/// chunk count is unchanged. A missing manifest still clears the full range
/// so a pre-existing orphan cannot survive initialization; when a manifest
/// shrinks, only its trailing chunks are stale.
fn metadata_chunk_clear_start(
    prefix: &[u8],
    current_manifest: Option<Manifest>,
    next_chunk_count: u32,
) -> Option<Vec<u8>> {
    match current_manifest {
        None => Some(prefix.to_vec()),
        Some(manifest) if manifest.chunk_count > next_chunk_count => {
            Some(metadata_chunk_key(prefix, next_chunk_count))
        }
        Some(_) => None,
    }
}

/// Conservatively estimate the affected bytes for a metadata publication.
///
/// FoundationDB counts mutation bytes and read/write conflict-range endpoints.
/// The data currently stored in a cleared range is not charged, but the clear
/// mutation and its write-conflict range still have key overhead. Count read
/// and write occurrences separately so this remains an upper bound even when
/// the native client can coalesce them.
fn metadata_publication_affected_bytes(
    prefix: &[u8],
    metadata_chunk_bytes: usize,
    payload_len: usize,
) -> Result<usize> {
    let keyspace = Keyspace::new(prefix);
    let lease_key = keyspace.lease();
    let fence_key = keyspace.fence();
    let write_mode_key = keyspace.write_mode();
    let metadata_backing_key = keyspace.metadata_backing();
    let manifest_key = keyspace.manifest();
    let chunk_prefix = keyspace.chunks();
    let chunk_end = range_end(&chunk_prefix)?;
    let chunk_count = metadata_chunk_count(payload_len, metadata_chunk_bytes)?;
    let chunk_key_bytes = chunk_prefix
        .len()
        .checked_add(std::mem::size_of::<u32>())
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;

    let mut affected = 0;
    // The legacy publication reads the lease, mode, and manifest; the CAS
    // publication reads mode and manifest. Bound both with one estimate.
    add_affected_bytes(&mut affected, key_conflict_range_bytes(lease_key.len())?)?;
    add_affected_bytes(&mut affected, key_conflict_range_bytes(fence_key.len())?)?;
    add_affected_bytes(
        &mut affected,
        key_conflict_range_bytes(write_mode_key.len())?,
    )?;
    add_affected_bytes(
        &mut affected,
        key_conflict_range_bytes(metadata_backing_key.len())?,
    )?;
    add_affected_bytes(&mut affected, key_conflict_range_bytes(manifest_key.len())?)?;
    // clear_range(chunk_prefix, chunk_end) contributes both its mutation
    // endpoints and its write-conflict range endpoints.
    let clear_range_bytes = range_conflict_bytes(chunk_prefix.len(), chunk_end.len())?;
    add_affected_bytes(&mut affected, clear_range_bytes)?;
    add_affected_bytes(&mut affected, clear_range_bytes)?;
    // Manifest write and all shard key/value writes. A set contributes its
    // mutation bytes and a write-conflict range for the key.
    add_affected_bytes(&mut affected, manifest_key.len())?;
    add_affected_bytes(&mut affected, MANIFEST_BYTES)?;
    add_affected_bytes(&mut affected, key_conflict_range_bytes(manifest_key.len())?)?;
    add_affected_bytes(&mut affected, payload_len)?;
    add_affected_bytes(
        &mut affected,
        chunk_key_bytes
            .checked_mul(chunk_count)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?,
    )?;
    add_affected_bytes(
        &mut affected,
        key_conflict_range_bytes(chunk_key_bytes)?
            .checked_mul(chunk_count)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?,
    )?;
    Ok(affected)
}

fn metadata_load_affected_bytes(
    prefix: &[u8],
    metadata_chunk_bytes: usize,
    payload_len: usize,
) -> Result<usize> {
    let keyspace = Keyspace::new(prefix);
    let manifest_key = keyspace.manifest();
    let chunk_prefix = keyspace.chunks();
    let chunk_count = metadata_chunk_count(payload_len, metadata_chunk_bytes)?;
    let chunk_key_bytes = chunk_prefix
        .len()
        .checked_add(std::mem::size_of::<u32>())
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;

    let mut affected = 0;
    add_affected_bytes(&mut affected, key_conflict_range_bytes(manifest_key.len())?)?;
    add_affected_bytes(
        &mut affected,
        key_conflict_range_bytes(chunk_key_bytes)?
            .checked_mul(chunk_count)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?,
    )?;
    Ok(affected)
}

fn metadata_transaction_too_large(affected_bytes: usize) -> FsError {
    FsError::new(ErrorCode::Efbig).with_message(format!(
        "FoundationDB metadata transaction affects {affected_bytes} bytes, above the {FOUNDATIONDB_MAX_TRANSACTION_BYTES}-byte limit"
    ))
}

fn block_id(bytes: &[u8]) -> BlockId {
    let digest = Sha256::digest(bytes);
    let mut id = String::with_capacity(7 + digest.len() * 2);
    id.push_str("sha256:");
    for byte in digest {
        id.push_str(&format!("{byte:02x}"));
    }
    BlockId(id)
}

fn validate_block_id(id: &BlockId) -> Result<()> {
    let Some(hex) = id.0.strip_prefix("sha256:") else {
        return Err(FsError::new(ErrorCode::Einval).with_message("invalid FoundationDB block id"));
    };
    if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(FsError::new(ErrorCode::Einval).with_message("invalid FoundationDB block id"));
    }
    Ok(())
}

async fn flush_inner(inner: &Inner) -> Result<()> {
    let key = Keyspace::new(&inner.prefix).flush();
    let limits = inner.limits;
    inner
        .transact_idempotent((), move |trx, _| {
            let key = key.clone();
            Box::pin(async move {
                configure_transaction(trx, limits)?;
                let _ = get_owned(trx, &key).await?;
                Ok(())
            })
        })
        .await
}

/// Metadata stored in FoundationDB, supporting fenced single-writer leases
/// and opt-in concurrent revision-CAS publication.
#[derive(Clone)]
pub struct FoundationDbMetadataStore(Arc<Inner>);

impl FoundationDbMetadataStore {
    pub fn new(storage: &FoundationDbStorage) -> Self {
        storage.metadata()
    }
}

#[async_trait]
impl MetadataStore for FoundationDbMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    fn publish_includes_flush_barrier(&self) -> bool {
        // FoundationDB publish awaits the transaction commit future. The
        // current flush transaction is an explicit post-commit barrier and
        // remains available through syncfs.
        true
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let inner = Arc::clone(&self.0);
        let keyspace = Keyspace::new(&inner.prefix);
        let manifest_key = keyspace.manifest();
        let chunk_prefix = keyspace.chunks();
        let metadata_prefix = inner.prefix.clone();
        let limits = inner.limits;
        inner
            .transact_idempotent((), move |trx, _| {
                let manifest_key = manifest_key.clone();
                let chunk_prefix = chunk_prefix.clone();
                let metadata_prefix = metadata_prefix.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    let Some(raw_manifest) = get_owned(trx, &manifest_key).await? else {
                        return Ok(LoadedMetadata {
                            revision: 0,
                            namespace: None,
                        });
                    };
                    let manifest = decode_manifest(&raw_manifest).map_err(TxnError::Fs)?;
                    let payload_len = usize::try_from(manifest.payload_len)
                        .map_err(|_| TxnError::Fs(FsError::new(ErrorCode::Eoverflow)))?;
                    if payload_len > limits.max_metadata_bytes {
                        return Err(TxnError::Fs(backend_error(
                            "FoundationDB metadata exceeds configured limit",
                        )));
                    }
                    let expected_chunks = payload_len.div_ceil(limits.metadata_chunk_bytes);
                    if expected_chunks != manifest.chunk_count as usize {
                        return Err(TxnError::Fs(backend_error(
                            "FoundationDB metadata manifest chunk count is invalid",
                        )));
                    }
                    let affected_bytes = metadata_load_affected_bytes(
                        &metadata_prefix,
                        limits.metadata_chunk_bytes,
                        payload_len,
                    )
                    .map_err(TxnError::Fs)?;
                    if affected_bytes > FOUNDATIONDB_MAX_TRANSACTION_BYTES {
                        return Err(TxnError::Fs(metadata_transaction_too_large(affected_bytes)));
                    }
                    let mut payload = Vec::with_capacity(payload_len);
                    for index in 0..manifest.chunk_count {
                        let key = metadata_chunk_key(&chunk_prefix, index);
                        let Some(chunk) = get_owned(trx, &key).await? else {
                            return Err(TxnError::Fs(backend_error(
                                "FoundationDB metadata chunk is missing",
                            )));
                        };
                        if chunk.len() > limits.metadata_chunk_bytes {
                            return Err(TxnError::Fs(backend_error(
                                "FoundationDB metadata chunk exceeds configured limit",
                            )));
                        }
                        payload.extend_from_slice(&chunk);
                    }
                    if payload.len() != payload_len {
                        return Err(TxnError::Fs(backend_error(
                            "FoundationDB metadata payload length is invalid",
                        )));
                    }
                    let namespace = serde_json::from_slice(&payload)
                        .map_err(backend_error)
                        .map_err(TxnError::Fs)?;
                    let loaded = LoadedMetadata {
                        revision: manifest.revision,
                        namespace: Some(namespace),
                    };
                    loaded.validate().map_err(TxnError::Fs)?;
                    if let Some(namespace) = loaded.namespace.as_ref() {
                        validate_namespace_chunkers(namespace, limits).map_err(TxnError::Fs)?;
                    }
                    Ok(loaded)
                })
            })
            .await
    }

    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        let inner = Arc::clone(&self.0);
        let keyspace = Keyspace::new(&inner.prefix);
        let mode_key = keyspace.write_mode();
        let backing_key = keyspace.metadata_backing();
        let lease_key = keyspace.lease();
        let fence_key = keyspace.fence();
        let limits = inner.limits;
        inner
            .transact_idempotent((), move |trx, _| {
                let mode_key = mode_key.clone();
                let backing_key = backing_key.clone();
                let lease_key = lease_key.clone();
                let fence_key = fence_key.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    let (raw_mode, raw_backing, raw_lease, raw_fence) =
                        futures_util::future::try_join4(
                            get_owned(trx, &mode_key),
                            get_owned(trx, &backing_key),
                            get_owned(trx, &lease_key),
                            get_owned(trx, &fence_key),
                        )
                        .await?;
                    let mode = decode_concurrent_mode_state(raw_mode, raw_backing)?;
                    if mode != ConcurrentModeState::Legacy
                        && (raw_lease.is_some()
                            || raw_fence.as_deref() != Some(CONCURRENT_FENCE_SENTINEL))
                    {
                        return Err(TxnError::Fs(backend_error(
                            "FoundationDB concurrent mode and legacy fence disagree",
                        )));
                    }
                    Ok(mode)
                })
            })
            .await
    }

    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        let inner = Arc::clone(&self.0);
        let keyspace = Keyspace::new(&inner.prefix);
        let mode_key = keyspace.write_mode();
        let backing_key = keyspace.metadata_backing();
        let lease_key = keyspace.lease();
        let fence_key = keyspace.fence();
        let limits = inner.limits;
        inner
            .transact_metadata((), move |trx, _| {
                let mode_key = mode_key.clone();
                let backing_key = backing_key.clone();
                let lease_key = lease_key.clone();
                let fence_key = fence_key.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    // Mode, backing, lease, and fence share one read version.
                    // A racing legacy lease claimant reads this mode/fence pair
                    // and conflicts with the fresh MRC2 claim.
                    let (raw_mode, raw_backing, raw_lease, raw_fence) =
                        futures_util::future::try_join4(
                            get_owned(trx, &mode_key),
                            get_owned(trx, &backing_key),
                            get_owned(trx, &lease_key),
                            get_owned(trx, &fence_key),
                        )
                        .await?;
                    match decode_concurrent_mode_state(raw_mode, raw_backing)? {
                        ConcurrentModeState::Mrc2(current) if current != backing => {
                            Err(TxnError::Fs(stale_backing()))
                        }
                        ConcurrentModeState::Mrc2(_) => {
                            if raw_lease.is_none()
                                && raw_fence.as_deref() == Some(CONCURRENT_FENCE_SENTINEL)
                            {
                                Ok(())
                            } else {
                                Err(TxnError::Fs(backend_error(
                                    "FoundationDB bound mode and legacy fence disagree",
                                )))
                            }
                        }
                        ConcurrentModeState::Mrc1 => Err(TxnError::Fs(
                            FsError::new(ErrorCode::Ebusy)
                                .with_syscall("prepare bound concurrent FoundationDB volume")
                                .with_message("MRC1 needs offline migrate-concurrent-backing"),
                        )),
                        ConcurrentModeState::Legacy => {
                            if raw_lease.is_some() || raw_fence.is_some() {
                                return Err(TxnError::Fs(
                                    FsError::new(ErrorCode::Ebusy)
                                        .with_syscall(
                                            "prepare bound concurrent FoundationDB volume",
                                        )
                                        .with_message(
                                            "a fenced legacy volume needs an offline migration",
                                        ),
                                ));
                            }
                            trx.set(&mode_key, BOUND_CONCURRENT_WRITE_MODE);
                            trx.set(&backing_key, &backing.as_bytes());
                            trx.set(&fence_key, CONCURRENT_FENCE_SENTINEL);
                            Ok(())
                        }
                    }
                })
            })
            .await
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        validate_owner(owner)?;
        let ttl = ttl_ms(ttl)?;
        let owner = owner.to_owned();
        let inner = Arc::clone(&self.0);
        let keyspace = Keyspace::new(&inner.prefix);
        let lease_key = keyspace.lease();
        let fence_key = keyspace.fence();
        let mode_key = keyspace.write_mode();
        let oracle = require_oracle(&inner)?;
        let limits = inner.limits;
        inner
            .transact_metadata((), move |trx, _| {
                let lease_key = lease_key.clone();
                let fence_key = fence_key.clone();
                let mode_key = mode_key.clone();
                let owner = owner.clone();
                let oracle = Arc::clone(&oracle);
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    require_legacy_write_mode(get_owned(trx, &mode_key).await?)?;
                    let current = get_owned(trx, &lease_key)
                        .await?
                        .map(|bytes| decode_lease(&bytes).map_err(TxnError::Fs))
                        .transpose()?;
                    let last_fence = get_owned(trx, &fence_key)
                        .await?
                        .map(|bytes| decode_last_fence(&bytes).map_err(TxnError::Fs))
                        .transpose()?;
                    let now_ms = oracle
                        .now_ms_in_transaction(trx)
                        .await
                        .map_err(TxnError::Fs)?;
                    let (fence, expires_at_ms) = plan_writer_acquisition(
                        current
                            .as_ref()
                            .map(|lease| (lease.fence, lease.expires_at_ms)),
                        last_fence,
                        now_ms,
                        ttl,
                    )
                    .map_err(TxnError::Fs)?;
                    let record = LeaseRecord {
                        owner: owner.clone(),
                        fence,
                        expires_at_ms,
                    };
                    let bytes = encode_lease(&record).map_err(TxnError::Fs)?;
                    trx.set(&lease_key, &bytes);
                    trx.set(&fence_key, &encode_last_fence(fence));
                    Ok(WriterLease {
                        owner,
                        fence,
                        expires_at_ms,
                    })
                })
            })
            .await
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        let requested = lease_from_input(lease)?;
        let ttl = ttl_ms(ttl)?;
        let inner = Arc::clone(&self.0);
        let lease_key = Keyspace::new(&inner.prefix).lease();
        let mode_key = Keyspace::new(&inner.prefix).write_mode();
        let oracle = require_oracle(&inner)?;
        let limits = inner.limits;
        inner
            .transact_metadata((), move |trx, _| {
                let lease_key = lease_key.clone();
                let mode_key = mode_key.clone();
                let requested = requested.clone();
                let oracle = Arc::clone(&oracle);
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    require_legacy_write_mode(get_owned(trx, &mode_key).await?)?;
                    let current = get_owned(trx, &lease_key)
                        .await?
                        .ok_or_else(|| TxnError::Fs(stale()))
                        .and_then(|bytes| decode_lease(&bytes).map_err(TxnError::Fs))?;
                    let now_ms = oracle
                        .now_ms_in_transaction(trx)
                        .await
                        .map_err(TxnError::Fs)?;
                    let (fence, expires_at_ms) =
                        plan_writer_renewal(&current, &requested, now_ms, ttl)
                            .map_err(TxnError::Fs)?;
                    let renewed = LeaseRecord {
                        owner: requested.owner.clone(),
                        fence,
                        expires_at_ms,
                    };
                    let bytes = encode_lease(&renewed).map_err(TxnError::Fs)?;
                    trx.set(&lease_key, &bytes);
                    Ok(WriterLease {
                        owner: renewed.owner,
                        fence: renewed.fence,
                        expires_at_ms,
                    })
                })
            })
            .await
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        let requested = lease_from_input(lease)?;
        let inner = Arc::clone(&self.0);
        let lease_key = Keyspace::new(&inner.prefix).lease();
        let mode_key = Keyspace::new(&inner.prefix).write_mode();
        let oracle = require_oracle(&inner)?;
        let limits = inner.limits;
        inner
            .transact_metadata((), move |trx, _| {
                let lease_key = lease_key.clone();
                let mode_key = mode_key.clone();
                let requested = requested.clone();
                let oracle = Arc::clone(&oracle);
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    require_legacy_write_mode(get_owned(trx, &mode_key).await?)?;
                    let current = get_owned(trx, &lease_key)
                        .await?
                        .ok_or_else(|| TxnError::Fs(stale()))
                        .and_then(|bytes| decode_lease(&bytes).map_err(TxnError::Fs))?;
                    let now_ms = oracle
                        .now_ms_in_transaction(trx)
                        .await
                        .map_err(TxnError::Fs)?;
                    if !lease_matches(&current, &requested, now_ms) {
                        return Err(TxnError::Fs(stale()));
                    }
                    trx.clear(&lease_key);
                    Ok(())
                })
            })
            .await
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        namespace.validate()?;
        validate_namespace_chunkers(&namespace, self.0.limits)?;
        let payload = serde_json::to_vec(&namespace).map_err(backend_error)?;
        let inner = Arc::clone(&self.0);
        if payload.len() > inner.limits.max_metadata_bytes {
            return Err(FsError::new(ErrorCode::Efbig)
                .with_message("FoundationDB namespace exceeds configured limit"));
        }
        let affected_bytes = metadata_publication_affected_bytes(
            &inner.prefix,
            inner.limits.metadata_chunk_bytes,
            payload.len(),
        )?;
        if affected_bytes > FOUNDATIONDB_MAX_TRANSACTION_BYTES {
            return Err(metadata_transaction_too_large(affected_bytes));
        }
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let chunk_count = payload.len().div_ceil(inner.limits.metadata_chunk_bytes);
        let chunk_count = u32::try_from(chunk_count)
            .map_err(|_| FsError::new(ErrorCode::Efbig).with_message("too many metadata chunks"))?;
        let manifest = encode_manifest(Manifest {
            revision: next_revision,
            chunk_count,
            payload_len: payload.len() as u64,
        });
        let requested = lease_from_input(lease)?;
        let keyspace = Keyspace::new(&inner.prefix);
        let lease_key = keyspace.lease();
        let mode_key = keyspace.write_mode();
        let manifest_key = keyspace.manifest();
        let chunk_prefix = keyspace.chunks();
        let chunk_end = range_end(&chunk_prefix)?;
        let oracle = require_oracle(&inner)?;
        let limits = inner.limits;
        inner
            .transact_metadata((), move |trx, _| {
                let lease_key = lease_key.clone();
                let mode_key = mode_key.clone();
                let manifest_key = manifest_key.clone();
                let chunk_prefix = chunk_prefix.clone();
                let chunk_end = chunk_end.clone();
                let payload = payload.clone();
                let manifest = manifest.clone();
                let requested = requested.clone();
                let oracle = Arc::clone(&oracle);
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    // These reads share one transaction and do not depend on
                    // one another. Poll them together so a publication pays
                    // one provider read window instead of three serial ones;
                    // the transaction still supplies one read version and
                    // the same lease/revision/authority validation boundary.
                    let authority_time = async {
                        oracle
                            .now_ms_in_transaction(trx)
                            .await
                            .map_err(TxnError::Fs)
                    };
                    let (raw_mode, raw_lease, raw_manifest, now_ms) =
                        futures_util::future::try_join4(
                            get_owned(trx, &mode_key),
                            get_owned(trx, &lease_key),
                            get_owned(trx, &manifest_key),
                            authority_time,
                        )
                        .await?;
                    require_legacy_write_mode(raw_mode)?;
                    let current_lease = raw_lease
                        .ok_or_else(|| TxnError::Fs(stale()))
                        .and_then(|bytes| decode_lease(&bytes).map_err(TxnError::Fs))?;
                    let current_manifest = raw_manifest
                        .map(|bytes| decode_manifest(&bytes).map_err(TxnError::Fs))
                        .transpose()?;
                    if !lease_matches(&current_lease, &requested, now_ms) {
                        return Err(TxnError::Fs(stale()));
                    }
                    let current_revision = current_manifest.map_or(0, |value| value.revision);
                    if current_revision != expected_revision {
                        return Err(TxnError::Fs(
                            FsError::new(ErrorCode::Eagain).with_syscall("publish metadata"),
                        ));
                    }
                    if let Some(clear_start) =
                        metadata_chunk_clear_start(&chunk_prefix, current_manifest, chunk_count)
                    {
                        trx.clear_range(&clear_start, &chunk_end);
                    }
                    for index in 0..chunk_count {
                        let start = index as usize * limits.metadata_chunk_bytes;
                        let end = (start + limits.metadata_chunk_bytes).min(payload.len());
                        let key = metadata_chunk_key(&chunk_prefix, index);
                        trx.set(&key, &payload[start..end]);
                    }
                    trx.set(&manifest_key, &manifest);
                    Ok(next_revision)
                })
            })
            .await
    }

    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        namespace.validate()?;
        validate_namespace_chunkers(&namespace, self.0.limits)?;
        let payload = serde_json::to_vec(&namespace).map_err(backend_error)?;
        let inner = Arc::clone(&self.0);
        if payload.len() > inner.limits.max_metadata_bytes {
            return Err(FsError::new(ErrorCode::Efbig)
                .with_message("FoundationDB namespace exceeds configured limit"));
        }
        let affected_bytes = metadata_publication_affected_bytes(
            &inner.prefix,
            inner.limits.metadata_chunk_bytes,
            payload.len(),
        )?;
        if affected_bytes > FOUNDATIONDB_MAX_TRANSACTION_BYTES {
            return Err(metadata_transaction_too_large(affected_bytes));
        }
        let next_revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let chunk_count = payload.len().div_ceil(inner.limits.metadata_chunk_bytes);
        let chunk_count = u32::try_from(chunk_count)
            .map_err(|_| FsError::new(ErrorCode::Efbig).with_message("too many metadata chunks"))?;
        let manifest = encode_manifest(Manifest {
            revision: next_revision,
            chunk_count,
            payload_len: payload.len() as u64,
        });
        let keyspace = Keyspace::new(&inner.prefix);
        let mode_key = keyspace.write_mode();
        let backing_key = keyspace.metadata_backing();
        let lease_key = keyspace.lease();
        let fence_key = keyspace.fence();
        let manifest_key = keyspace.manifest();
        let chunk_prefix = keyspace.chunks();
        let chunk_end = range_end(&chunk_prefix)?;
        let limits = inner.limits;
        inner
            .transact_metadata((), move |trx, _| {
                let mode_key = mode_key.clone();
                let backing_key = backing_key.clone();
                let lease_key = lease_key.clone();
                let fence_key = fence_key.clone();
                let manifest_key = manifest_key.clone();
                let chunk_prefix = chunk_prefix.clone();
                let chunk_end = chunk_end.clone();
                let payload = payload.clone();
                let manifest = manifest.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    // The authority, legacy fence, and manifest CAS all use
                    // one transaction read version. Compare backing before
                    // revision so a wrong client cannot retry an EAGAIN.
                    let (raw_mode, raw_backing, raw_lease, raw_fence, raw_manifest) =
                        futures_util::future::try_join5(
                            get_owned(trx, &mode_key),
                            get_owned(trx, &backing_key),
                            get_owned(trx, &lease_key),
                            get_owned(trx, &fence_key),
                            get_owned(trx, &manifest_key),
                        )
                        .await?;
                    match decode_concurrent_mode_state(raw_mode, raw_backing)? {
                        ConcurrentModeState::Mrc2(current) if current != backing => {
                            return Err(TxnError::Fs(stale_backing()));
                        }
                        ConcurrentModeState::Mrc2(_) => {}
                        ConcurrentModeState::Mrc1 => {
                            return Err(TxnError::Fs(
                                FsError::new(ErrorCode::Ebusy)
                                    .with_syscall("publish bound FoundationDB metadata")
                                    .with_message("MRC1 needs offline migrate-concurrent-backing"),
                            ));
                        }
                        ConcurrentModeState::Legacy => {
                            return Err(TxnError::Fs(
                                FsError::enotsup("publish bound FoundationDB metadata")
                                    .with_message("prepare the volume for bound writers first"),
                            ));
                        }
                    }
                    if raw_lease.is_some()
                        || raw_fence.as_deref() != Some(CONCURRENT_FENCE_SENTINEL)
                    {
                        return Err(TxnError::Fs(
                            FsError::new(ErrorCode::Ebusy)
                                .with_syscall("publish bound FoundationDB metadata")
                                .with_message("the concurrent fence sentinel is missing or a legacy writer has claimed this volume"),
                        ));
                    }
                    let current_manifest = raw_manifest
                        .map(|bytes| decode_manifest(&bytes).map_err(TxnError::Fs))
                        .transpose()?;
                    let current_revision = current_manifest.map_or(0, |value| value.revision);
                    if current_revision != expected_revision {
                        return Err(TxnError::Fs(
                            FsError::new(ErrorCode::Eagain)
                                .with_syscall("publish bound FoundationDB metadata"),
                        ));
                    }
                    if let Some(clear_start) =
                        metadata_chunk_clear_start(&chunk_prefix, current_manifest, chunk_count)
                    {
                        trx.clear_range(&clear_start, &chunk_end);
                    }
                    for index in 0..chunk_count {
                        let start = index as usize * limits.metadata_chunk_bytes;
                        let end = (start + limits.metadata_chunk_bytes).min(payload.len());
                        let key = metadata_chunk_key(&chunk_prefix, index);
                        trx.set(&key, &payload[start..end]);
                    }
                    trx.set(&manifest_key, &manifest);
                    Ok(next_revision)
                })
            })
            .await
    }

    async fn migrate_mrc1_to_bound_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        let inner = Arc::clone(&self.0);
        let keyspace = Keyspace::new(&inner.prefix);
        let mode_key = keyspace.write_mode();
        let backing_key = keyspace.metadata_backing();
        let lease_key = keyspace.lease();
        let fence_key = keyspace.fence();
        let manifest_key = keyspace.manifest();
        let limits = inner.limits;
        inner
            .transact_metadata((), move |trx, _| {
                let mode_key = mode_key.clone();
                let backing_key = backing_key.clone();
                let lease_key = lease_key.clone();
                let fence_key = fence_key.clone();
                let manifest_key = manifest_key.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    let (raw_mode, raw_backing, raw_lease, raw_fence, raw_manifest) =
                        futures_util::future::try_join5(
                            get_owned(trx, &mode_key),
                            get_owned(trx, &backing_key),
                            get_owned(trx, &lease_key),
                            get_owned(trx, &fence_key),
                            get_owned(trx, &manifest_key),
                        )
                        .await?;
                    if decode_concurrent_mode_state(raw_mode, raw_backing)?
                        != ConcurrentModeState::Mrc1
                    {
                        return Err(TxnError::Fs(
                            FsError::new(ErrorCode::Ebusy)
                                .with_syscall("migrate MRC1 FoundationDB metadata")
                                .with_message("only a fenced MRC1 volume can be migrated"),
                        ));
                    }
                    if raw_lease.is_some()
                        || raw_fence.as_deref() != Some(CONCURRENT_FENCE_SENTINEL)
                    {
                        return Err(TxnError::Fs(backend_error(
                            "FoundationDB MRC1 mode and legacy fence disagree",
                        )));
                    }
                    let current_manifest = raw_manifest
                        .map(|bytes| decode_manifest(&bytes).map_err(TxnError::Fs))
                        .transpose()?;
                    if current_manifest.map_or(0, |value| value.revision) != expected_revision {
                        return Err(TxnError::Fs(
                            FsError::new(ErrorCode::Eagain)
                                .with_syscall("migrate MRC1 FoundationDB metadata"),
                        ));
                    }
                    // The mode and backing are one commit. Keep the manifest,
                    // namespace chunks, revision, and fence unchanged.
                    trx.set(&mode_key, BOUND_CONCURRENT_WRITE_MODE);
                    trx.set(&backing_key, &backing.as_bytes());
                    Ok(())
                })
            })
            .await
    }

    async fn flush(&self) -> Result<()> {
        flush_inner(&self.0).await
    }
}

/// Immutable content-addressed blocks stored in FoundationDB.
#[derive(Clone)]
pub struct FoundationDbBlockStore(Arc<Inner>);

impl FoundationDbBlockStore {
    pub fn new(storage: &FoundationDbStorage) -> Self {
        storage.blocks()
    }

    async fn probe_concurrent_keyspace(&self) -> Result<()> {
        // Opening Database::from_path creates only a client handle. Probe
        // this exact keyspace without mutation before claiming its marker.
        let inner = Arc::clone(&self.0);
        let key = Keyspace::new(&inner.prefix).block(&block_id(
            b"mount-rs FoundationDB concurrent block preflight",
        ));
        let limits = FoundationDbLimits {
            transaction_timeout: inner
                .limits
                .transaction_timeout
                .min(CONCURRENT_BLOCK_PREFLIGHT_TIMEOUT),
            transaction_retry_limit: inner
                .limits
                .transaction_retry_limit
                .min(CONCURRENT_BLOCK_PREFLIGHT_RETRY_LIMIT),
            ..inner.limits
        };
        inner
            .db
            .transact_boxed(
                (),
                move |trx, _| {
                    let key = key.clone();
                    Box::pin(async move {
                        configure_transaction(trx, limits)?;
                        let _ = get_owned(trx, &key).await?;
                        Ok(())
                    })
                },
                transaction_options(limits, TransactionPolicy::Idempotent),
            )
            .await
            .map_err(TxnError::into_fs)
            .map_err(|error| error.with_syscall("probe concurrent FoundationDB blocks"))
    }
}

#[async_trait]
impl BlockStore for FoundationDbBlockStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        // A Database handle is not proof that its coordinator is reachable.
        // Preserve the bounded, read-only keyspace probe before creating an
        // authority marker.
        self.probe_concurrent_keyspace().await?;
        let candidate = ConcurrentBackingId::from_bytes(*Uuid::new_v4().as_bytes())?;
        let inner = Arc::clone(&self.0);
        let key = Keyspace::new(&inner.prefix).block_authority();
        let limits = inner.limits;
        inner
            .transact_idempotent((), move |trx, _| {
                let key = key.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    if let Some(raw) = get_owned(trx, &key).await? {
                        // The normal read adds a conflict range. Concurrent
                        // creators cannot both commit distinct IDs; a loser
                        // retries by reading the winner from this same key.
                        parse_backing_bytes(&raw).ok_or_else(|| TxnError::Fs(stale_backing()))
                    } else {
                        trx.set(&key, &candidate.as_bytes());
                        Ok(candidate)
                    }
                })
            })
            .await
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        let inner = Arc::clone(&self.0);
        let key = Keyspace::new(&inner.prefix).block_authority();
        let limits = FoundationDbLimits {
            transaction_timeout: inner
                .limits
                .transaction_timeout
                .min(CONCURRENT_BLOCK_PREFLIGHT_TIMEOUT),
            transaction_retry_limit: inner
                .limits
                .transaction_retry_limit
                .min(CONCURRENT_BLOCK_PREFLIGHT_RETRY_LIMIT),
            ..inner.limits
        };
        inner
            .db
            .transact_boxed(
                (),
                move |trx, _| {
                    let key = key.clone();
                    Box::pin(async move {
                        configure_transaction(trx, limits)?;
                        let raw = get_owned(trx, &key).await?;
                        let actual = raw
                            .as_deref()
                            .and_then(parse_backing_bytes)
                            .ok_or_else(|| TxnError::Fs(stale_backing()))?;
                        if actual != expected {
                            return Err(TxnError::Fs(stale_backing()));
                        }
                        Ok(())
                    })
                },
                transaction_options(limits, TransactionPolicy::Idempotent),
            )
            .await
            .map_err(TxnError::into_fs)
    }

    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        validate_block_id(id)?;
        let inner = Arc::clone(&self.0);
        let key = Keyspace::new(&inner.prefix).block(id);
        let requested = id.clone();
        let limits = inner.limits;
        inner
            .transact_idempotent((), move |trx, _| {
                let key = key.clone();
                let requested = requested.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    let bytes = get_owned(trx, &key).await?.ok_or_else(|| {
                        TxnError::Fs(
                            FsError::new(ErrorCode::Enoent)
                                .with_syscall("read FoundationDB block for migration"),
                        )
                    })?;
                    if block_id(&bytes) != requested {
                        return Err(TxnError::Fs(
                            FsError::new(ErrorCode::Eio)
                                .with_syscall("verify FoundationDB migration block digest"),
                        ));
                    }
                    Ok(bytes)
                })
            })
            .await
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        if bytes.len() > self.0.limits.max_block_bytes {
            return Err(FsError::new(ErrorCode::Efbig)
                .with_message("FoundationDB block exceeds configured limit"));
        }
        let id = block_id(bytes);
        let inner = Arc::clone(&self.0);
        let key = Keyspace::new(&inner.prefix).block(&id);
        let expected = bytes.to_vec();
        let limits = inner.limits;
        inner
            .transact_idempotent((), move |trx, _| {
                let key = key.clone();
                let expected = expected.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    if let Some(existing) = get_owned(trx, &key).await? {
                        if existing != expected {
                            return Err(TxnError::Fs(backend_error(
                                "FoundationDB content-addressed block collision",
                            )));
                        }
                    } else {
                        trx.set(&key, &expected);
                    }
                    Ok(())
                })
            })
            .await?;
        Ok(id)
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        validate_block_id(id)?;
        let inner = Arc::clone(&self.0);
        let key = Keyspace::new(&inner.prefix).block(id);
        let limits = inner.limits;
        inner
            .transact_idempotent((), move |trx, _| {
                let key = key.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    get_owned(trx, &key).await?.ok_or_else(|| {
                        TxnError::Fs(FsError::new(ErrorCode::Enoent).with_syscall("get block"))
                    })
                })
            })
            .await
    }

    async fn flush(&self) -> Result<()> {
        flush_inner(&self.0).await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        validate_block_id(id)?;
        let inner = Arc::clone(&self.0);
        let key = Keyspace::new(&inner.prefix).block(id);
        let limits = inner.limits;
        inner
            .transact_idempotent((), move |trx, _| {
                let key = key.clone();
                Box::pin(async move {
                    configure_transaction(trx, limits)?;
                    trx.clear(&key);
                    Ok(())
                })
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::storage::{ConcurrentBackingId, ConcurrentModeState};
    use std::io::{self, Write};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct EventWriter(Arc<Mutex<Vec<u8>>>);

    impl Write for EventWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .map_err(|_| io::Error::other("event writer mutex poisoned"))?
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn captured_event(render: impl FnOnce()) -> String {
        let output = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::fmt()
            .with_ansi(false)
            .with_writer({
                let output = Arc::clone(&output);
                move || EventWriter(Arc::clone(&output))
            })
            .finish();
        tracing::subscriber::with_default(subscriber, render);
        String::from_utf8(output.lock().unwrap().clone()).unwrap()
    }

    #[test]
    fn authority_telemetry_event_has_stable_bounded_fields() {
        let rendered = captured_event(|| {
            emit_lease_authority_telemetry(
                "error",
                &LeaseAuthorityStats {
                    publication_attempts: 2,
                    publication_successes: 1,
                    publication_failures: 1,
                    last_published_time_ms: Some(2_000_000),
                    last_success_at_ms: Some(3_000_000),
                    last_failure_at_ms: Some(3_000_001),
                },
            );
        });
        assert!(rendered.contains("mount_rs.foundationdb.lease_authority"));
        assert!(rendered.contains("publication_attempts=2"));
        assert!(rendered.contains("publication_failures=1"));
        assert!(rendered.contains("outcome=error"), "{rendered}");
        assert!(!rendered.contains("cluster_file"));
        assert!(!rendered.contains("credentials"));
    }

    #[test]
    fn reader_telemetry_event_has_stable_bounded_fields() {
        let rendered = captured_event(|| {
            emit_lease_oracle_telemetry(
                "ok",
                &LeaseOracleStats {
                    read_attempts: 5,
                    read_successes: 4,
                    read_failures: 1,
                    last_observed_time_ms: Some(2_000_001),
                    last_success_at_ms: Some(3_000_000),
                    last_failure_at_ms: Some(3_000_001),
                },
            );
        });
        assert!(rendered.contains("mount_rs.foundationdb.lease_oracle"));
        assert!(rendered.contains("reader_attempts=5"));
        assert!(rendered.contains("reader_failures=1"));
        assert!(rendered.contains("outcome=ok"), "{rendered}");
        assert!(!rendered.contains("authority_prefix"));
        assert!(!rendered.contains("provider_error"));
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum InjectedCommitOutcome {
        Committed,
        DefinitelyNotCommitted,
        /// The commit may have applied, but the acknowledgement was lost.
        MaybeCommittedAfterAckLoss,
    }

    /// Deterministic seam for the foundationdb-rs commit-unknown contract.
    ///
    /// A native service fault-injection test cannot safely manufacture a
    /// lost acknowledgement without a network proxy or process failure. This
    /// seam keeps the dangerous replay decision executable in unit tests: a
    /// metadata closure must stop on `MaybeCommittedAfterAckLoss`, while an
    /// idempotent content-addressed block closure may be replayed.
    fn run_injected_transaction(
        policy: TransactionPolicy,
        outcomes: &[InjectedCommitOutcome],
        closure_runs: &mut usize,
    ) -> Result<()> {
        for outcome in outcomes {
            *closure_runs += 1;
            match outcome {
                InjectedCommitOutcome::Committed => return Ok(()),
                InjectedCommitOutcome::DefinitelyNotCommitted => continue,
                InjectedCommitOutcome::MaybeCommittedAfterAckLoss if policy.is_idempotent() => {
                    continue;
                }
                InjectedCommitOutcome::MaybeCommittedAfterAckLoss => {
                    return Err(ambiguous_commit_error(
                        1021,
                        "injected commit_unknown_result (committed-but-lost-ack)",
                    ));
                }
            }
        }
        Err(FsError::new(ErrorCode::Eio)
            .with_message("injected transaction exhausted before a committed result"))
    }

    #[derive(Default)]
    struct TestClock(AtomicU64);

    #[async_trait]
    impl LeaseOracle for TestClock {
        async fn now_ms(&self) -> Result<u64> {
            Ok(self.0.load(Ordering::Relaxed))
        }
    }

    #[derive(Default)]
    struct SharedAuthorityTestClock(AtomicU64);

    #[async_trait]
    impl LeaseOracle for SharedAuthorityTestClock {
        async fn now_ms(&self) -> Result<u64> {
            Ok(self.0.load(Ordering::Relaxed))
        }

        fn authority_kind(&self) -> LeaseAuthorityKind {
            LeaseAuthorityKind::SharedProvider
        }
    }

    #[test]
    fn limits_reject_values_that_would_break_fdb_contract() {
        assert!(
            FoundationDbLimits {
                max_block_bytes: FOUNDATIONDB_MAX_VALUE_BYTES + 1,
                ..FoundationDbLimits::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            FoundationDbLimits {
                metadata_chunk_bytes: 0,
                ..FoundationDbLimits::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            FoundationDbLimits {
                max_metadata_bytes: FOUNDATIONDB_MAX_TRANSACTION_BYTES + 1,
                ..FoundationDbLimits::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            FoundationDbStorageOptions::new("volume")
                .with_limits(FoundationDbLimits {
                    max_metadata_bytes: FOUNDATIONDB_MAX_TRANSACTION_BYTES,
                    ..FoundationDbLimits::default()
                })
                .validate()
                .is_err()
        );
    }

    #[test]
    fn chunker_preflight_surfaces_the_selected_block_bound() {
        let chunker = |size| ChunkerConfig {
            algorithm: "fixed-size".to_owned(),
            version: 1,
            parameters: [("chunk_size".to_owned(), size)].into_iter().collect(),
        };

        assert!(
            validate_chunker_config(
                &chunker(DEFAULT_MAX_BLOCK_BYTES as u64),
                FoundationDbLimits::default(),
            )
            .is_ok()
        );
        let error = validate_chunker_config(
            &chunker((DEFAULT_MAX_BLOCK_BYTES + 1) as u64),
            FoundationDbLimits::default(),
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Efbig);

        let larger_limits = FoundationDbLimits {
            max_block_bytes: FOUNDATIONDB_MAX_VALUE_BYTES,
            ..FoundationDbLimits::default()
        };
        assert!(
            validate_chunker_config(&chunker(FOUNDATIONDB_MAX_VALUE_BYTES as u64), larger_limits,)
                .is_ok()
        );
    }

    #[test]
    fn manifest_and_lease_codecs_round_trip() {
        let manifest = Manifest {
            revision: 7,
            chunk_count: 3,
            payload_len: 19,
        };
        assert_eq!(
            decode_manifest(&encode_manifest(manifest)).unwrap(),
            manifest
        );
        let lease = LeaseRecord {
            owner: "writer".to_owned(),
            fence: 9,
            expires_at_ms: 123,
        };
        assert_eq!(
            decode_lease(&encode_lease(&lease).unwrap()).unwrap().owner,
            "writer"
        );
        assert_eq!(decode_oracle_time(&encode_oracle_time(123)).unwrap(), 123);
        assert_eq!(decode_last_fence(&encode_last_fence(17)).unwrap(), 17);
    }

    #[test]
    fn writer_acquisition_numbers_fence_expired_leases_and_reject_overflow() {
        assert_eq!(
            plan_writer_acquisition(None, None, 1_000, 30).unwrap(),
            (1, 1_030)
        );
        assert_eq!(
            plan_writer_acquisition(Some((7, 1_000)), Some(5), 1_000, 30).unwrap(),
            (8, 1_030)
        );
        assert_eq!(
            plan_writer_acquisition(Some((3, 999)), Some(9), 1_000, 30).unwrap(),
            (10, 1_030)
        );
        assert_eq!(
            plan_writer_acquisition(Some((7, 1_001)), Some(5), 1_000, 30)
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        assert_eq!(
            plan_writer_acquisition(Some((u64::MAX, 1_000)), None, 1_000, 30)
                .unwrap_err()
                .code,
            ErrorCode::Eoverflow
        );
        assert_eq!(
            plan_writer_acquisition(None, Some(u64::MAX), 1_000, 30)
                .unwrap_err()
                .code,
            ErrorCode::Eoverflow
        );
        assert_eq!(
            plan_writer_acquisition(None, None, u64::MAX, 1)
                .unwrap_err()
                .code,
            ErrorCode::Eoverflow
        );
    }

    #[test]
    fn writer_renewal_numbers_require_exact_live_token_and_checked_expiry() {
        let current = LeaseRecord {
            owner: "writer".to_owned(),
            fence: 9,
            expires_at_ms: 1_000,
        };
        assert_eq!(
            plan_writer_renewal(&current, &current, 900, 200).unwrap(),
            (9, 1_100)
        );
        assert_eq!(
            plan_writer_renewal(&current, &current, 999, 1).unwrap(),
            (9, 1_000)
        );
        assert_eq!(
            plan_writer_renewal(&current, &current, 0, 200).unwrap(),
            (9, 200)
        );
        let wrong_owner = LeaseRecord {
            owner: "other".to_owned(),
            ..current.clone()
        };
        let wrong_fence = LeaseRecord {
            fence: 8,
            ..current.clone()
        };
        let wrong_expiry = LeaseRecord {
            expires_at_ms: 999,
            ..current.clone()
        };
        for requested in [&wrong_owner, &wrong_fence, &wrong_expiry] {
            assert_eq!(
                plan_writer_renewal(&current, requested, 900, 200)
                    .unwrap_err()
                    .code,
                ErrorCode::Estale
            );
        }
        for now in [1_000, 1_001] {
            assert_eq!(
                plan_writer_renewal(&current, &current, now, 1)
                    .unwrap_err()
                    .code,
                ErrorCode::Estale
            );
        }
        let max_expiry = LeaseRecord {
            expires_at_ms: u64::MAX,
            ..current
        };
        assert_eq!(
            plan_writer_renewal(&max_expiry, &max_expiry, u64::MAX - 1, 2)
                .unwrap_err()
                .code,
            ErrorCode::Eoverflow
        );
    }

    #[test]
    fn concurrent_fence_sentinel_rejects_the_legacy_decoder() {
        assert_eq!(CONCURRENT_FENCE_SENTINEL.len(), FENCE_BYTES);
        assert_ne!(&CONCURRENT_FENCE_SENTINEL[..4], FENCE_MAGIC);
        // This is the unchanged decoder that origin/main's acquire_writer
        // uses before it can write a lease or advance the fence.
        assert!(decode_last_fence(CONCURRENT_FENCE_SENTINEL).is_err());
        assert!(decode_concurrent_write_mode(Some(CONCURRENT_WRITE_MODE.to_vec())).unwrap());
    }

    #[test]
    fn concurrent_backing_mrc2_fences_legacy_writers() {
        let error = require_legacy_write_mode(Some(b"MRC2".to_vec()))
            .expect_err("MRC2 must fence the legacy publisher")
            .into_fs();
        assert_eq!(error.code, ErrorCode::Enotsup);
        assert!(decode_concurrent_write_mode(Some(b"MRC2".to_vec())).is_err());
    }

    #[test]
    fn concurrent_backing_mode_and_id_pairs_require_one_valid_authority() {
        let backing = ConcurrentBackingId::from_bytes([7; 16]).unwrap();
        assert_eq!(
            decode_concurrent_mode_state(None, None).unwrap(),
            ConcurrentModeState::Legacy
        );
        assert_eq!(
            decode_concurrent_mode_state(Some(CONCURRENT_WRITE_MODE.to_vec()), None).unwrap(),
            ConcurrentModeState::Mrc1
        );
        assert_eq!(
            decode_concurrent_mode_state(Some(b"MRC2".to_vec()), Some(backing.as_bytes().to_vec()))
                .unwrap(),
            ConcurrentModeState::Mrc2(backing)
        );
        for (mode, id) in [
            (None, Some(backing.as_bytes().to_vec())),
            (
                Some(CONCURRENT_WRITE_MODE.to_vec()),
                Some(backing.as_bytes().to_vec()),
            ),
            (Some(b"MRC2".to_vec()), None),
            (Some(b"MRC2".to_vec()), Some(vec![0; 16])),
            (Some(b"MRC2".to_vec()), Some(vec![7; 15])),
            (Some(b"BAD!".to_vec()), Some(backing.as_bytes().to_vec())),
        ] {
            assert!(
                decode_concurrent_mode_state(mode, id).is_err(),
                "malformed concurrent mode and ID pair must fail closed"
            );
        }
    }

    #[test]
    fn authority_clock_policy_clamps_backward_and_rejects_large_forward_jumps() {
        assert_eq!(
            authority_time_sample(Some(2_000), 1_000, Some(60_000)).unwrap(),
            2_000
        );
        assert_eq!(
            authority_time_sample(Some(2_000), 62_000, Some(60_000)).unwrap(),
            62_000
        );
        let error = authority_time_sample(Some(2_000), 62_001, Some(60_000)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        assert!(error.to_string().contains("safety bound"));
        assert_eq!(
            authority_time_sample(None, 62_001, Some(1)).unwrap(),
            62_001
        );
    }

    #[test]
    fn authority_clock_policy_checks_zero_and_maximum_time() {
        assert_eq!(
            authority_time_sample(None, 0, None).unwrap_err().code,
            ErrorCode::Einval
        );
        assert_eq!(
            authority_time_sample(Some(u64::MAX), u64::MAX - 1, Some(1)).unwrap(),
            u64::MAX
        );
        assert_eq!(
            authority_time_sample(Some(u64::MAX - 1), u64::MAX, Some(1)).unwrap(),
            u64::MAX
        );
        assert_eq!(
            authority_time_sample(Some(u64::MAX - 1), u64::MAX, Some(0))
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        assert_eq!(
            authority_time_sample(Some(1), u64::MAX, None).unwrap(),
            u64::MAX
        );
    }

    #[test]
    fn authority_clock_policy_requires_a_positive_duration() {
        assert!(duration_millis(Duration::ZERO, "authority bound must be positive").is_err());
        assert_eq!(
            duration_millis(Duration::from_millis(1), "authority bound").unwrap(),
            1
        );
    }

    #[test]
    fn lease_publication_policy_requires_cadence_and_jump_bounds() {
        let policy = LeasePublicationPolicy::new(
            Duration::from_secs(30),
            Duration::from_secs(5),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(policy.lease_ttl, Duration::from_secs(30));

        let equal_cadence = LeasePublicationPolicy::new(
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(equal_cadence.code, ErrorCode::Einval);
        assert!(equal_cadence.to_string().contains("shorter"));

        let oversized_jump = LeasePublicationPolicy::new(
            Duration::from_secs(30),
            Duration::from_secs(5),
            Duration::from_secs(31),
        )
        .unwrap_err();
        assert_eq!(oversized_jump.code, ErrorCode::Einval);
        assert!(oversized_jump.to_string().contains("lease TTL"));

        let zero_interval = LeasePublicationPolicy {
            lease_ttl: Duration::from_secs(30),
            publication_interval: Duration::ZERO,
            max_forward_jump: Duration::from_secs(5),
        }
        .validate()
        .unwrap_err();
        assert_eq!(zero_interval.code, ErrorCode::Einval);
    }

    #[test]
    fn authority_stats_preserve_success_and_failure_boundaries() {
        let stats = LeaseAuthorityStatsInner::default();
        stats.record_attempt();
        stats.record_success(2_000_000);
        stats.record_attempt();
        stats.record_failure();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.publication_attempts, 2);
        assert_eq!(snapshot.publication_successes, 1);
        assert_eq!(snapshot.publication_failures, 1);
        assert_eq!(snapshot.last_published_time_ms, Some(2_000_000));
        assert!(snapshot.last_success_at_ms.is_some());
        assert!(snapshot.last_failure_at_ms.is_some());
    }

    #[test]
    fn oracle_stats_preserve_success_and_failure_boundaries() {
        let stats = LeaseOracleStatsInner::default();
        stats.record_attempt();
        stats.record_success(2_000_000);
        stats.record_attempt();
        stats.record_failure();

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.read_attempts, 2);
        assert_eq!(snapshot.read_successes, 1);
        assert_eq!(snapshot.read_failures, 1);
        assert_eq!(snapshot.last_observed_time_ms, Some(2_000_000));
        assert!(snapshot.last_success_at_ms.is_some());
        assert!(snapshot.last_failure_at_ms.is_some());
    }

    #[test]
    fn blocks_are_deterministic_and_prefix_ranges_are_bounded() {
        assert_eq!(block_id(b"a"), block_id(b"a"));
        assert_ne!(block_id(b"a"), block_id(b"b"));
        let end = range_end(b"prefix/").unwrap();
        assert_eq!(end, b"prefix0");
    }

    #[test]
    fn metadata_publication_clears_only_missing_trailing_chunks() {
        let prefix = b"volume/meta/chunk/";
        assert_eq!(
            metadata_chunk_clear_start(prefix, None, 2),
            Some(prefix.to_vec())
        );
        assert_eq!(
            metadata_chunk_clear_start(
                prefix,
                Some(Manifest {
                    revision: 1,
                    chunk_count: 4,
                    payload_len: 32,
                }),
                2,
            ),
            Some(metadata_chunk_key(prefix, 2))
        );
        assert_eq!(
            metadata_chunk_clear_start(
                prefix,
                Some(Manifest {
                    revision: 2,
                    chunk_count: 2,
                    payload_len: 16,
                }),
                2,
            ),
            None
        );
        assert_eq!(metadata_chunk_key(prefix, 0), b"volume/meta/chunk/\0\0\0\0");
    }

    #[test]
    fn metadata_publication_budget_includes_shard_and_compaction_overhead() {
        let affected = metadata_publication_affected_bytes(
            b"volume",
            DEFAULT_METADATA_CHUNK_BYTES,
            FOUNDATIONDB_MAX_TRANSACTION_BYTES,
        )
        .unwrap();
        assert!(affected > FOUNDATIONDB_MAX_TRANSACTION_BYTES);

        let normal = metadata_publication_affected_bytes(
            b"volume",
            DEFAULT_METADATA_CHUNK_BYTES,
            DEFAULT_MAX_METADATA_BYTES,
        )
        .unwrap();
        assert!(normal < FOUNDATIONDB_MAX_TRANSACTION_BYTES);
    }

    #[test]
    fn lease_oracle_is_never_selected_by_default() {
        let default = FoundationDbStorageOptions::new("test");
        assert!(default.oracle.is_none());
        assert!(!default.auto_oracle);
        let persisted = FoundationDbStorageOptions::new("test").with_persisted_lease_oracle();
        assert!(persisted.oracle.is_none());
        assert!(persisted.auto_oracle);
        let options = FoundationDbStorageOptions::new("test").with_clock(TestClock::default());
        assert!(options.oracle.is_some());
        assert!(!options.auto_oracle);
        assert!(options.validate().is_ok());
    }

    #[test]
    fn production_lease_authority_rejects_untrusted_time_sources() {
        let unverified = FoundationDbStorageOptions::new("test")
            .with_production_lease_oracle(TestClock::default())
            .validate()
            .err()
            .expect("unverified clock must not open in production mode");
        assert_eq!(unverified.code, ErrorCode::Enotsup);

        let development = FoundationDbStorageOptions::new("test")
            .with_production_lease_oracle(SystemLeaseClock)
            .validate()
            .err()
            .expect("development clock must not open in production mode");
        assert_eq!(development.code, ErrorCode::Enotsup);

        assert!(
            FoundationDbStorageOptions::new("test")
                .with_production_lease_oracle(SharedAuthorityTestClock::default())
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn prefix_limit_accounts_for_the_full_block_key() {
        let overhead = KEY_SEPARATOR.len() + BLOCK_SUFFIX.len() + BLOCK_ID_TEXT_BYTES;
        let accepted = "p".repeat(FOUNDATIONDB_MAX_KEY_BYTES - overhead);
        assert!(FoundationDbStorageOptions::new(accepted).validate().is_ok());
        let rejected = "p".repeat(FOUNDATIONDB_MAX_KEY_BYTES - overhead + 1);
        assert!(
            FoundationDbStorageOptions::new(rejected)
                .validate()
                .is_err()
        );
        assert!(
            FoundationDbStorageOptions::new(b"volume\0suffix")
                .validate()
                .is_err()
        );
    }

    #[test]
    fn authority_prefix_limit_accounts_for_the_oracle_key() {
        let overhead = KEY_SEPARATOR.len() + LEASE_ORACLE_SUFFIX.len();
        let accepted = "p".repeat(FOUNDATIONDB_MAX_KEY_BYTES - overhead);
        assert!(validate_lease_authority_prefix(accepted.as_bytes()).is_ok());
        let rejected = "p".repeat(FOUNDATIONDB_MAX_KEY_BYTES - overhead + 1);
        let error = validate_lease_authority_prefix(rejected.as_bytes()).unwrap_err();
        assert_eq!(error.code, ErrorCode::Enametoolong);
        assert!(validate_lease_authority_prefix(b"authority\0suffix").is_err());
    }

    #[test]
    fn production_transaction_options_preserve_retry_policy_flags() {
        let limits = FoundationDbLimits {
            transaction_timeout: Duration::from_millis(1750),
            transaction_retry_limit: 13,
            ..FoundationDbLimits::default()
        };

        let metadata = transaction_options(limits, TransactionPolicy::FailClosedOnMaybeCommitted);
        assert_eq!(metadata.retry_limit, Some(13));
        assert_eq!(metadata.time_out, Some(Duration::from_millis(1750)));
        assert!(!metadata.is_idempotent);

        let idempotent = transaction_options(limits, TransactionPolicy::Idempotent);
        assert_eq!(idempotent.retry_limit, Some(13));
        assert_eq!(idempotent.time_out, Some(Duration::from_millis(1750)));
        assert!(idempotent.is_idempotent);
    }

    #[test]
    fn commit_unknown_result_1021_maps_to_fail_closed_io_error() {
        // foundationdb-rs exposes the native constructor, so this checks the
        // production error predicate and mapping rather than the injected
        // retry-loop model below.
        let error = FdbError::from_code(1021);
        assert!(error.is_maybe_committed());

        let mapped = fdb_error(error);
        assert_eq!(mapped.code, ErrorCode::Eio);
        assert!(mapped.to_string().contains("error 1021"));
        assert!(mapped.to_string().contains("reconcile state"));
    }

    #[test]
    fn metadata_unknown_commit_fails_closed_without_replaying_closure() {
        let mut closure_runs = 0;
        let error = run_injected_transaction(
            TransactionPolicy::FailClosedOnMaybeCommitted,
            &[InjectedCommitOutcome::MaybeCommittedAfterAckLoss],
            &mut closure_runs,
        )
        .unwrap_err();

        assert_eq!(error.code, ErrorCode::Eio);
        assert_eq!(closure_runs, 1);
        assert!(error.to_string().contains("committed-but-lost-ack"));
    }

    #[test]
    fn idempotent_block_unknown_commit_can_retry_after_ack_loss() {
        let mut closure_runs = 0;
        run_injected_transaction(
            TransactionPolicy::Idempotent,
            &[
                InjectedCommitOutcome::MaybeCommittedAfterAckLoss,
                InjectedCommitOutcome::Committed,
            ],
            &mut closure_runs,
        )
        .unwrap();
        assert_eq!(closure_runs, 2);
    }

    #[test]
    fn metadata_retries_errors_proven_not_committed() {
        let mut closure_runs = 0;
        run_injected_transaction(
            TransactionPolicy::FailClosedOnMaybeCommitted,
            &[
                InjectedCommitOutcome::DefinitelyNotCommitted,
                InjectedCommitOutcome::Committed,
            ],
            &mut closure_runs,
        )
        .unwrap();
        assert_eq!(closure_runs, 2);
    }
}
