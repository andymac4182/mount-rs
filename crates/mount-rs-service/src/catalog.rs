//! Versioned service metadata, separate from filesystem namespace metadata.

#[cfg(feature = "io-profiling")]
use mount_rs_core::diagnostics::profile;
use mount_rs_core::diagnostics::profile::{Event, Span};

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
#[cfg(test)]
use std::sync::Mutex;
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
#[cfg(unix)]
const CONNECTION_POOL_SIZE: usize = 8;

#[cfg(test)]
static TEST_CONNECTION_OPENS: std::sync::LazyLock<Mutex<BTreeMap<PathBuf, usize>>> =
    std::sync::LazyLock::new(|| Mutex::new(BTreeMap::new()));

#[derive(Debug)]
pub enum CatalogError {
    Storage(rusqlite::Error),
    Task(tokio::task::JoinError),
    Invalid(&'static str),
    Conflict,
}

impl fmt::Display for CatalogError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(_) | Self::Task(_) => output.write_str("catalog storage unavailable"),
            Self::Invalid(reason) => write!(output, "invalid catalog: {reason}"),
            Self::Conflict => output.write_str("catalog revision conflict"),
        }
    }
}

impl std::error::Error for CatalogError {}

impl From<rusqlite::Error> for CatalogError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Storage(error)
    }
}

impl From<tokio::task::JoinError> for CatalogError {
    fn from(error: tokio::task::JoinError) -> Self {
        Self::Task(error)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    Read,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct DriveKey {
    pub partition_id: String,
    pub drive_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DriveDefinition {
    pub driver: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PartitionDefinition {
    #[serde(deserialize_with = "unique_map")]
    pub drives: BTreeMap<String, DriveDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GrantDefinition {
    pub partition_id: String,
    pub policy_id: String,
    #[serde(deserialize_with = "unique_map")]
    pub drives: BTreeMap<String, Permission>,
    #[serde(deserialize_with = "unique_map")]
    pub claim_conditions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IssuerPolicyDefinition {
    pub issuer: String,
    pub audiences: Vec<String>,
    #[serde(default = "default_algorithms")]
    pub algorithms: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSnapshot {
    pub revision: u64,
    #[serde(deserialize_with = "unique_map")]
    pub partitions: BTreeMap<String, PartitionDefinition>,
    #[serde(deserialize_with = "unique_map")]
    pub issuer_policies: BTreeMap<String, serde_json::Value>,
    #[serde(deserialize_with = "unique_map")]
    pub grants: BTreeMap<String, GrantDefinition>,
}

fn default_algorithms() -> Vec<String> {
    vec!["RS256".into(), "ES256".into()]
}

impl CatalogSnapshot {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            revision: 0,
            partitions: BTreeMap::new(),
            issuer_policies: BTreeMap::new(),
            grants: BTreeMap::new(),
        }
    }

    pub fn validate(&self) -> Result<(), CatalogError> {
        if self.partitions.len() > 1024
            || self.issuer_policies.len() > 64
            || self.grants.len() > 4096
            || self
                .partitions
                .values()
                .map(|p| p.drives.len())
                .sum::<usize>()
                > 4096
        {
            return Err(CatalogError::Invalid("catalog count limit exceeded"));
        }
        for (partition_id, partition) in &self.partitions {
            validate_id(partition_id)?;
            for (drive_id, drive) in &partition.drives {
                validate_id(drive_id)?;
                if !drive.driver.is_object() {
                    return Err(CatalogError::Invalid("Drive driver must be an object"));
                }
            }
        }
        for (policy_id, policy) in &self.issuer_policies {
            validate_id(policy_id)?;
            let policy: IssuerPolicyDefinition = serde_json::from_value(policy.clone())
                .map_err(|_| CatalogError::Invalid("invalid issuer policy shape"))?;
            if !crate::auth::is_safe_public_https_url(&policy.issuer)
                || policy.algorithms.is_empty()
                || policy.algorithms.len() > 2
                || policy
                    .algorithms
                    .iter()
                    .any(|a| !matches!(a.as_str(), "RS256" | "ES256"))
                || policy.audiences.is_empty()
                || policy.audiences.len() > 16
                || policy
                    .audiences
                    .iter()
                    .any(|aud| aud.is_empty() || aud == "*" || aud.len() > 512)
            {
                return Err(CatalogError::Invalid("unsafe issuer policy"));
            }
        }
        for (grant_id, grant) in &self.grants {
            validate_id(grant_id)?;
            let partition = self
                .partitions
                .get(&grant.partition_id)
                .ok_or(CatalogError::Invalid("grant Partition does not exist"))?;
            if !self.issuer_policies.contains_key(&grant.policy_id) {
                return Err(CatalogError::Invalid("grant issuer policy does not exist"));
            }
            if grant.drives.is_empty() {
                return Err(CatalogError::Invalid("grant must name a Drive"));
            }
            if grant.claim_conditions.len() > 32
                || grant.claim_conditions.iter().any(|(pointer, value)| {
                    !pointer.starts_with('/')
                        || pointer.len() > 512
                        || value.is_empty()
                        || value.len() > 2048
                })
            {
                return Err(CatalogError::Invalid("invalid claim conditions"));
            }
            if !grant.claim_conditions.iter().any(|(path, value)| {
                !value.is_empty()
                    && path.starts_with('/')
                    && !matches!(path.as_str(), "/iss" | "/aud" | "/exp" | "/iat" | "/nbf")
            }) {
                return Err(CatalogError::Invalid(
                    "grant requires a stable workload identity condition",
                ));
            }
            for drive_id in grant.drives.keys() {
                if !partition.drives.contains_key(drive_id) {
                    return Err(CatalogError::Invalid("grant Drive does not exist"));
                }
            }
        }
        Ok(())
    }
}

fn validate_id(value: &str) -> Result<(), CatalogError> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(CatalogError::Invalid(
            "invalid Partition, Drive, or policy ID",
        ));
    }
    Ok(())
}

#[async_trait]
pub trait CatalogStore: Send + Sync {
    async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError>;

    /// Load an immutable snapshot. Providers may override this to share decoded metadata.
    async fn load_shared_current(&self) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        self.load_current().await.map(Arc::new)
    }
    async fn compare_and_swap(
        &self,
        expected_revision: u64,
        next: CatalogSnapshot,
    ) -> Result<u64, CatalogError>;
}

#[derive(Clone)]
pub struct SqliteCatalog {
    shared: Arc<CatalogConnections>,
}

struct CatalogConnections {
    state: Arc<CatalogState>,
    #[cfg(unix)]
    owners: ConnectionOwners,
}

/// What a catalog job reads besides its connection. Jobs hold this and never
/// the owners, so the last handle to the owners is never dropped by a job.
struct CatalogState {
    path: PathBuf,
    current: ArcSwapOption<CachedSnapshot>,
    #[cfg(unix)]
    identity: FileIdentity,
}

/// One unit of work for an owner thread: a whole load or a whole
/// compare-and-swap transaction, so a transaction never spans two jobs.
#[cfg(unix)]
type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// A thread that owns one SQLite connection and runs the jobs it is sent, one
/// at a time and in arrival order.
#[cfg(unix)]
struct ConnectionOwner {
    jobs: Option<std::sync::mpsc::Sender<Job>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// The pooled connections, each on its own thread. A caller picks one
/// round-robin, sends it a job, and waits for the reply without holding a
/// runtime thread.
#[cfg(unix)]
struct ConnectionOwners {
    owners: Vec<ConnectionOwner>,
    next: AtomicUsize,
}

#[cfg(unix)]
impl ConnectionOwners {
    fn start(connections: Vec<Connection>) -> Result<Self, CatalogError> {
        let mut owners = Vec::with_capacity(connections.len());
        for connection in connections {
            let (jobs, queue) = std::sync::mpsc::channel::<Job>();
            // On failure the owners already started are dropped, which joins them.
            let thread = std::thread::Builder::new()
                .name("mount-rs-catalog".into())
                .spawn(move || {
                    let mut connection = connection;
                    for job in queue {
                        job(&mut connection);
                    }
                    // Every sender is gone: close the connection here.
                    let _profile = Span::new(Event::CatalogClose);
                    drop(connection);
                })
                .map_err(|_| CatalogError::Invalid("cannot start catalog connection thread"))?;
            owners.push(ConnectionOwner {
                jobs: Some(jobs),
                thread: Some(thread),
            });
        }
        Ok(Self {
            owners,
            next: AtomicUsize::new(0),
        })
    }

    /// Runs `work` on the next connection and returns its result.
    ///
    /// A caller that goes away before its job starts cancels it: the job is
    /// skipped. Once started, a job runs to its end. A panic in `work` is
    /// reported to its caller as the task failure a blocking task would have
    /// produced, and the connection keeps serving later jobs (a transaction
    /// open in `work` rolls back as the panic unwinds through it).
    async fn run<R, F>(&self, work: F) -> Result<R, CatalogError>
    where
        R: Send + 'static,
        F: FnOnce(&mut Connection) -> R + Send + 'static,
    {
        let index = self.next.fetch_add(1, Ordering::Relaxed) % self.owners.len();
        let wait_profile = Span::new(Event::CatalogPoolWait);
        let (reply, outcome) = tokio::sync::oneshot::channel::<std::thread::Result<R>>();
        let job: Job = Box::new(move |connection| {
            drop(wait_profile);
            if reply.is_closed() {
                return;
            }
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(connection)));
            let _ = reply.send(outcome);
        });
        if let Some(jobs) = &self.owners[index].jobs {
            // A failed send drops the job and with it the reply sender.
            let _ = jobs.send(job);
        }
        match outcome.await {
            Ok(Ok(value)) => Ok(value),
            // Re-raise on a blocking thread to report the same error value as
            // when the work itself ran there. This does not print the panic again.
            Ok(Err(panic)) => {
                match tokio::task::spawn_blocking(move || std::panic::resume_unwind(panic)).await {
                    Err(error) => Err(CatalogError::Task(error)),
                    Ok(never) => match never {},
                }
            }
            Err(_) => Err(CatalogError::Invalid("catalog connection closed")),
        }
    }
}

#[cfg(unix)]
impl Drop for ConnectionOwners {
    fn drop(&mut self) {
        // One connection at a time, as when the pool was a list of
        // connections: two that close together each see the other still open
        // and neither checkpoints and removes the write-ahead log. When this
        // returns every connection is closed.
        let current = std::thread::current().id();
        for owner in &mut self.owners {
            owner.jobs = None;
            if let Some(thread) = owner.thread.take()
                && thread.thread().id() != current
            {
                let _ = thread.join();
            }
        }
    }
}

struct CachedSnapshot {
    revision: i64,
    document: Box<[u8]>,
    snapshot: Arc<CatalogSnapshot>,
}

// Reuse requires exact document equality as well as the SQL revision: out-of-band
// edits at an unchanged revision must still be decoded and validated.
//
// Readers compare against a loaded pointer with no exclusion, and nothing is
// held across the decode. Concurrent reads of a new revision each decode it;
// an older read never replaces a newer cached document.
fn shared_snapshot(
    current: &ArcSwapOption<CachedSnapshot>,
    revision: i64,
    document: &[u8],
) -> Result<Arc<CatalogSnapshot>, CatalogError> {
    if let Some(cached) = current.load().as_ref()
        && cached.revision == revision
        && cached.document.as_ref() == document
    {
        return Ok(Arc::clone(&cached.snapshot));
    }
    let decode_profile = Span::new(Event::CatalogDecode).units(document.len() as u64);
    let snapshot = Arc::new(decode_snapshot(revision, document)?);
    drop(decode_profile);
    let cached = Arc::new(CachedSnapshot {
        revision,
        document: document.into(),
        snapshot: Arc::clone(&snapshot),
    });
    // `rcu` re-runs the revision check against whatever a racing publisher
    // stored, so the check and the store stay one step as they were under the cell.
    current.rcu(|newest| {
        if newest
            .as_ref()
            .is_none_or(|newest| newest.revision <= revision)
        {
            Some(Arc::clone(&cached))
        } else {
            newest.clone()
        }
    });
    Ok(snapshot)
}

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
}

impl fmt::Debug for SqliteCatalog {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output
            .debug_struct("SqliteCatalog")
            .field("path", &self.shared.state.path)
            .finish()
    }
}

#[cfg(unix)]
impl CatalogState {
    fn verify_backing(&self) -> Result<(), CatalogError> {
        let _profile = Span::new(Event::CatalogBackingVerify);
        if file_identity(&self.path)? != self.identity {
            return Err(CatalogError::Invalid("catalog backing file changed"));
        }
        Ok(())
    }
}

impl CatalogConnections {
    /// Runs `work` with the shared state and a connection, off the caller's
    /// runtime thread: on a pooled connection's own thread on Unix, and on a
    /// blocking thread with a fresh connection elsewhere.
    async fn run<R, F>(&self, work: F) -> Result<R, CatalogError>
    where
        R: Send + 'static,
        F: FnOnce(&CatalogState, &mut Connection) -> Result<R, CatalogError> + Send + 'static,
    {
        let state = Arc::clone(&self.state);
        #[cfg(unix)]
        {
            self.owners
                .run(move |connection| work(&state, connection))
                .await?
        }
        #[cfg(not(unix))]
        {
            tokio::task::spawn_blocking(move || {
                let mut connection = connect(&state.path)?;
                let result = work(&state, &mut connection);
                let _close_profile = Span::new(Event::CatalogClose);
                drop(connection);
                result
            })
            .await?
        }
    }
}

impl SqliteCatalog {
    pub async fn open(path: impl AsRef<Path>) -> Result<Self, CatalogError> {
        let path = path.as_ref().to_path_buf();
        validate_catalog_path(&path)?;
        let setup_path = path.clone();
        let shared = tokio::task::spawn_blocking(move || -> Result<CatalogConnections, CatalogError> {
            #[cfg(unix)]
            let initial_identity = prepare_file_identity(&setup_path)?;
            let connection = connect(&setup_path)?;
            connection.execute_batch(
                "CREATE TABLE IF NOT EXISTS service_catalog (\
                   singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
                   revision INTEGER NOT NULL CHECK (revision >= 0), \
                   document BLOB NOT NULL);",
            )?;
            let empty = serde_json::to_vec(&CatalogSnapshot::empty())
                .map_err(|_| CatalogError::Invalid("cannot encode empty catalog"))?;
            connection.execute(
                "INSERT OR IGNORE INTO service_catalog (singleton, revision, document) VALUES (1, 0, ?1)",
                params![empty],
            )?;
            #[cfg(unix)]
            {
            let identity = file_identity(&setup_path)?;
            if initial_identity != identity {
                return Err(CatalogError::Invalid("catalog backing file changed"));
            }
            let mut connections = Vec::with_capacity(CONNECTION_POOL_SIZE);
            connections.push(connection);
            for _ in 1..CONNECTION_POOL_SIZE {
                if file_identity(&setup_path)? != identity {
                    return Err(CatalogError::Invalid("catalog backing file changed"));
                }
                let connection = connect(&setup_path)?;
                if file_identity(&setup_path)? != identity {
                    return Err(CatalogError::Invalid("catalog backing file changed"));
                }
                connections.push(connection);
            }
            Ok(CatalogConnections {
                state: Arc::new(CatalogState {
                    path: setup_path,
                    current: ArcSwapOption::empty(),
                    identity,
                }),
                owners: ConnectionOwners::start(connections)?,
            })
            }
            #[cfg(not(unix))]
            {
                drop(connection);
                Ok(CatalogConnections {
                    state: Arc::new(CatalogState {
                        path: setup_path,
                        current: ArcSwapOption::empty(),
                    }),
                })
            }
        })
        .await??;
        Ok(Self {
            shared: Arc::new(shared),
        })
    }

    pub async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
        <Self as CatalogStore>::load_current(self).await
    }

    pub async fn load_shared_current(&self) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        <Self as CatalogStore>::load_shared_current(self).await
    }

    pub async fn compare_and_swap(
        &self,
        expected_revision: u64,
        next: CatalogSnapshot,
    ) -> Result<u64, CatalogError> {
        <Self as CatalogStore>::compare_and_swap(self, expected_revision, next).await
    }
}

#[async_trait]
impl CatalogStore for SqliteCatalog {
    async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
        self.load_shared_current()
            .await
            .map(|snapshot| (*snapshot).clone())
    }

    async fn load_shared_current(&self) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        let _load_profile = Span::new(Event::CatalogLoad);
        let queue_profile = Span::new(Event::CatalogQueue);
        self.shared.run(move |shared, connection| {
            drop(queue_profile);
            #[cfg(unix)]
            shared.verify_backing()?;
            // The row itself is read every time.
            let current = &shared.current;
            let mut query_profile = Span::new(Event::CatalogQuery);
            let mut statement = connection.prepare_cached(
                "SELECT revision, length(document), document FROM service_catalog WHERE singleton = 1",
            )?;
            let row = statement.query_row([], |row| {
                let length: i64 = row.get(1)?;
                if length < 0 || length > MAX_DOCUMENT_BYTES as i64 {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                let document = match row.get_ref(2)? {
                    rusqlite::types::ValueRef::Blob(document) => document,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                if document.len() != length as usize {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                query_profile.set_units(document.len() as u64);
                Ok(shared_snapshot(current, row.get(0)?, document))
            }).optional()?;
            let result = row.ok_or(CatalogError::Invalid("catalog row missing"))?;
            drop(statement);
            drop(query_profile);
            #[cfg(feature = "io-profiling")]
            if profile::enabled() {
                let sample = mount_rs_sqlite::connection_page_diagnostics(connection, true);
                match sample.ok().and_then(|pages| {
                    Some((
                        pages["pager"]["cache_hits"].as_u64()?,
                        pages["pager"]["cache_misses"].as_u64()?,
                        pages["pager"]["page_writes"].as_u64()?,
                    ))
                }) {
                    Some((hits, misses, writes)) => {
                        profile::add(Event::CatalogPagerHits, hits);
                        profile::add(Event::CatalogPagerMisses, misses);
                        profile::add(Event::CatalogPagerWrites, writes);
                    }
                    None => profile::add(Event::CatalogPagerUnavailable, 1),
                }
            }
            #[cfg(unix)]
            shared.verify_backing()?;
            result
        })
        .await
    }

    async fn compare_and_swap(
        &self,
        expected_revision: u64,
        mut next: CatalogSnapshot,
    ) -> Result<u64, CatalogError> {
        next.validate()?;
        let revision = expected_revision
            .checked_add(1)
            .ok_or(CatalogError::Invalid("revision overflow"))?;
        next.revision = revision;
        let document = serde_json::to_vec(&next)
            .map_err(|_| CatalogError::Invalid("cannot encode catalog"))?;
        if document.len() > MAX_DOCUMENT_BYTES {
            return Err(CatalogError::Invalid("catalog document too large"));
        }
        self.shared
            .run(move |shared, connection| {
                #[cfg(unix)]
                shared.verify_backing()?;
                #[cfg(not(unix))]
                let _ = shared;
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let previous: i64 = transaction.query_row(
                    "SELECT revision FROM service_catalog WHERE singleton = 1",
                    [],
                    |row| row.get(0),
                )?;
                if u64::try_from(previous).ok() != Some(expected_revision) {
                    return Err(CatalogError::Conflict);
                }
                let prior_document: Vec<u8> = transaction.query_row(
                    "SELECT document FROM service_catalog WHERE singleton=1",
                    [],
                    |row| row.get(0),
                )?;
                let prior = decode_snapshot(previous, &prior_document)?;
                for (id, partition) in &prior.partitions {
                    if !next.partitions.contains_key(id)
                        && (!partition.drives.is_empty()
                            || prior.grants.values().any(|g| g.partition_id == *id))
                    {
                        return Err(CatalogError::Invalid(
                            "Partition must be empty before deletion",
                        ));
                    }
                }
                let revision_sql = i64::try_from(revision)
                    .map_err(|_| CatalogError::Invalid("revision overflow"))?;
                transaction.execute(
                    "UPDATE service_catalog SET revision = ?1, document = ?2 WHERE singleton = 1",
                    params![revision_sql, document],
                )?;
                transaction.commit()?;
                #[cfg(unix)]
                shared.verify_backing()?;
                Ok(revision)
            })
            .await
    }
}

fn connect(path: &Path) -> Result<Connection, CatalogError> {
    #[cfg(test)]
    {
        *TEST_CONNECTION_OPENS
            .lock()
            .unwrap()
            .entry(path.to_path_buf())
            .or_default() += 1;
    }
    let _profile = Span::new(Event::CatalogConnect);
    let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE
        | rusqlite::OpenFlags::SQLITE_OPEN_CREATE
        | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(path, flags)?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.pragma_update(None, "journal_mode", "WAL")?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    Ok(connection)
}

fn validate_catalog_path(path: &Path) -> Result<(), CatalogError> {
    if path
        .to_str()
        .is_some_and(|value| value == ":memory:" || value.starts_with("file:"))
    {
        return Err(CatalogError::Invalid(
            "catalog requires a durable file path",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn file_identity(path: &Path) -> Result<FileIdentity, CatalogError> {
    let metadata = std::fs::metadata(path)
        .map_err(|_| CatalogError::Invalid("catalog backing file unavailable"))?;
    if !metadata.is_file() {
        return Err(CatalogError::Invalid("catalog backing file is not regular"));
    }
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
fn prepare_file_identity(path: &Path) -> Result<FileIdentity, CatalogError> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(file) => {
            let metadata = file
                .metadata()
                .map_err(|_| CatalogError::Invalid("catalog backing file unavailable"))?;
            Ok(FileIdentity {
                device: metadata.dev(),
                inode: metadata.ino(),
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => file_identity(path),
        Err(_) => Err(CatalogError::Invalid("catalog backing file unavailable")),
    }
}

fn decode_snapshot(revision: i64, document: &[u8]) -> Result<CatalogSnapshot, CatalogError> {
    if document.len() > MAX_DOCUMENT_BYTES {
        return Err(CatalogError::Invalid("catalog document too large"));
    }
    let snapshot: CatalogSnapshot = serde_json::from_slice(document)
        .map_err(|_| CatalogError::Invalid("malformed catalog document"))?;
    if u64::try_from(revision).ok() != Some(snapshot.revision) {
        return Err(CatalogError::Invalid("catalog revision mismatch"));
    }
    snapshot.validate()?;
    Ok(snapshot)
}

fn unique_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Unique<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Unique<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("map with unique identifiers")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, T>()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate catalog identifier"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique(std::marker::PhantomData))
}

#[cfg(test)]
mod reuse_tests {
    use super::*;

    #[test]
    fn unchanged_document_selection_allocates_no_snapshot_or_document_data() {
        let mut snapshot = CatalogSnapshot::empty();
        snapshot.partitions.insert(
            "red".into(),
            PartitionDefinition {
                drives: BTreeMap::from([(
                    "data".into(),
                    DriveDefinition {
                        driver: serde_json::json!({"kind": "memory"}),
                    },
                )]),
            },
        );
        let document = serde_json::to_vec(&snapshot).unwrap();
        let current = ArcSwapOption::empty();
        let first = shared_snapshot(&current, 0, &document).unwrap();
        // The former owned-snapshot path is a positive control for this counter.
        let owned = crate::dispatch::allocation_tests::count(|| {
            std::hint::black_box((*first).clone());
        });
        assert!(owned.0 > 0 && owned.1 > 0);
        let shared = crate::dispatch::allocation_tests::count(|| {
            for _ in 0..32 {
                let loaded = shared_snapshot(&current, 0, &document).unwrap();
                assert!(Arc::ptr_eq(&first, &loaded));
                std::hint::black_box(loaded);
            }
        });
        assert_eq!(shared, (0, 0), "unchanged catalog data allocates");
    }

    #[tokio::test]
    async fn unchanged_documents_share_the_decoded_snapshot_across_clones() {
        let directory = tempfile::tempdir().unwrap();
        let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap();
        let first = catalog.load_shared_current().await.unwrap();
        for _ in 0..16 {
            let current = catalog.clone().load_shared_current().await.unwrap();
            assert!(Arc::ptr_eq(&first, &current));
        }
    }

    #[tokio::test]
    async fn shared_reads_validate_every_byte_even_when_revision_is_unchanged() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let catalog = SqliteCatalog::open(&path).await.unwrap();
        let first = catalog.load_shared_current().await.unwrap();
        let mut changed = CatalogSnapshot::empty();
        changed.partitions.insert(
            "new".into(),
            PartitionDefinition {
                drives: BTreeMap::new(),
            },
        );
        let connection = Connection::open(&path).unwrap();
        let document = serde_json::to_vec(&changed).unwrap();
        connection
            .execute("UPDATE service_catalog SET document=?1", params![document])
            .unwrap();
        let current = catalog.load_shared_current().await.unwrap();
        assert!(!Arc::ptr_eq(&first, &current));
        assert!(current.partitions.contains_key("new"));
        connection
            .execute(
                "UPDATE service_catalog SET document=?1",
                params![b"malformed".as_slice()],
            )
            .unwrap();
        assert!(catalog.load_shared_current().await.is_err());
        connection
            .execute(
                "UPDATE service_catalog SET document=?1",
                params![serde_json::to_vec(&changed).unwrap()],
            )
            .unwrap();
        assert!(
            catalog
                .load_shared_current()
                .await
                .unwrap()
                .partitions
                .contains_key("new")
        );
    }

    #[tokio::test]
    async fn cached_snapshot_never_masks_invalid_authoritative_rows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let catalog = SqliteCatalog::open(&path).await.unwrap();
        let snapshot = catalog.load_shared_current().await.unwrap();
        let document = serde_json::to_vec(snapshot.as_ref()).unwrap();
        let connection = Connection::open(&path).unwrap();
        // SQLite permits a text value in this BLOB column. Reject its type even
        // when the UTF-8 bytes exactly match a previously validated document.
        connection
            .execute(
                "UPDATE service_catalog SET document=?1",
                params![std::str::from_utf8(&document).unwrap()],
            )
            .unwrap();
        assert!(catalog.load_shared_current().await.is_err());
        connection
            .execute("UPDATE service_catalog SET document=?1", params![document])
            .unwrap();
        connection
            .execute("UPDATE service_catalog SET revision=1", [])
            .unwrap();
        assert!(catalog.load_shared_current().await.is_err());
        connection
            .execute(
                "UPDATE service_catalog SET revision=0, document=zeroblob(0)",
                [],
            )
            .unwrap();
        assert!(catalog.load_shared_current().await.is_err());
    }

    #[test]
    fn catalog_requires_a_durable_non_uri_path() {
        assert!(validate_catalog_path(Path::new(":memory:")).is_err());
        assert!(validate_catalog_path(Path::new("file:catalog?mode=memory")).is_err());
        assert!(validate_catalog_path(Path::new("catalog.sqlite")).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn repeated_loads_reuse_the_open_catalog_connection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let catalog = SqliteCatalog::open(&path).await.unwrap();
        let opened_after_setup = super::TEST_CONNECTION_OPENS.lock().unwrap()[&path];
        for _ in 0..8 {
            assert_eq!(catalog.load_current().await.unwrap().revision, 0);
        }
        assert_eq!(
            super::TEST_CONNECTION_OPENS.lock().unwrap()[&path],
            opened_after_setup,
            "catalog reads must not open a SQLite connection per request"
        );
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn unsupported_pool_platform_keeps_fresh_catalog_reads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let catalog = SqliteCatalog::open(&path).await.unwrap();
        let opened_after_setup = super::TEST_CONNECTION_OPENS.lock().unwrap()[&path];
        assert_eq!(catalog.load_current().await.unwrap().revision, 0);
        assert_eq!(
            super::TEST_CONNECTION_OPENS.lock().unwrap()[&path],
            opened_after_setup + 1
        );
    }

    #[tokio::test]
    async fn a_reused_reader_observes_an_external_grant_revocation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let reader = SqliteCatalog::open(&path).await.unwrap();
        let writer = SqliteCatalog::open(&path).await.unwrap();
        let mut initial = CatalogSnapshot::empty();
        initial.partitions.insert(
            "red".into(),
            PartitionDefinition {
                drives: BTreeMap::from([(
                    "data".into(),
                    DriveDefinition {
                        driver: serde_json::json!({"kind":"memory"}),
                    },
                )]),
            },
        );
        initial.issuer_policies.insert(
            "issuer".into(),
            serde_json::json!({
                "issuer":"https://issuer.example.com", "audiences":["mount-rs"]
            }),
        );
        initial.grants.insert(
            "grant".into(),
            GrantDefinition {
                partition_id: "red".into(),
                policy_id: "issuer".into(),
                drives: BTreeMap::from([("data".into(), Permission::Read)]),
                claim_conditions: BTreeMap::from([("/sub".into(), "workload".into())]),
            },
        );
        writer.compare_and_swap(0, initial).await.unwrap();
        assert!(
            reader
                .load_shared_current()
                .await
                .unwrap()
                .grants
                .contains_key("grant")
        );
        let mut revoked = writer.load_current().await.unwrap();
        revoked.grants.clear();
        writer.compare_and_swap(1, revoked).await.unwrap();
        let current = reader.load_shared_current().await.unwrap();
        assert_eq!(current.revision, 2);
        assert!(current.grants.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replacing_the_catalog_file_fails_closed_for_existing_connections() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let catalog = SqliteCatalog::open(&path).await.unwrap();
        assert_eq!(catalog.load_shared_current().await.unwrap().revision, 0);
        let replacement_path = directory.path().join("replacement.sqlite");
        let replacement = SqliteCatalog::open(&replacement_path).await.unwrap();
        replacement
            .compare_and_swap(0, CatalogSnapshot::empty())
            .await
            .unwrap();
        drop(replacement);
        std::fs::rename(&replacement_path, &path).unwrap();
        assert!(catalog.load_shared_current().await.is_err());
        assert!(
            catalog
                .compare_and_swap(0, CatalogSnapshot::empty())
                .await
                .is_err()
        );
    }
}

#[cfg(all(test, unix))]
mod owner_tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;

    fn pool(path: &Path, size: usize) -> ConnectionOwners {
        let connections = (0..size).map(|_| connect(path).unwrap()).collect();
        ConnectionOwners::start(connections).unwrap()
    }

    #[tokio::test]
    async fn each_connection_runs_its_jobs_one_at_a_time_in_arrival_order() {
        let directory = tempfile::tempdir().unwrap();
        let owners = Arc::new(pool(&directory.path().join("pool.sqlite"), 4));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let running = Arc::new(AtomicUsize::new(0));
        // A current-thread runtime polls these in spawn order, so job `n` is
        // sent `n`th and lands on connection `n % 4`.
        let tasks: Vec<_> = (0..256_usize)
            .map(|sequence| {
                let owners = Arc::clone(&owners);
                let seen = Arc::clone(&seen);
                let running = Arc::clone(&running);
                tokio::spawn(async move {
                    owners
                        .run(move |connection| {
                            // The connection's own address marks which one ran the job.
                            let connection = std::ptr::from_ref(connection) as usize;
                            let overlap = running.fetch_add(1, Ordering::SeqCst);
                            seen.lock().unwrap().push((
                                std::thread::current().id(),
                                connection,
                                sequence,
                            ));
                            std::thread::yield_now();
                            running.fetch_sub(1, Ordering::SeqCst);
                            overlap
                        })
                        .await
                        .unwrap()
                })
            })
            .collect();
        for task in tasks {
            assert!(task.await.unwrap() < 4);
        }
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 256);
        let mut by_thread = BTreeMap::new();
        for (thread, connection, sequence) in seen.iter() {
            by_thread
                .entry(format!("{thread:?}"))
                .or_insert_with(Vec::new)
                .push((*connection, *sequence));
        }
        assert_eq!(by_thread.len(), 4);
        for jobs in by_thread.values() {
            assert_eq!(jobs.len(), 64);
            assert!(jobs.iter().all(|job| job.0 == jobs[0].0));
            assert!(jobs.iter().all(|job| job.1 % 4 == jobs[0].1 % 4));
            assert!(jobs.windows(2).all(|pair| pair[0].1 < pair[1].1));
        }
    }

    #[tokio::test]
    async fn a_panicking_job_fails_its_caller_and_leaves_the_connection_usable() {
        let directory = tempfile::tempdir().unwrap();
        let owners = pool(&directory.path().join("pool.sqlite"), 1);
        owners
            .run(|connection| {
                connection
                    .execute_batch("CREATE TABLE t (v INTEGER); INSERT INTO t VALUES (1);")
                    .unwrap();
            })
            .await
            .unwrap();
        let failed = owners
            .run(|connection| {
                let transaction = connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)
                    .unwrap();
                transaction.execute("UPDATE t SET v = 2", []).unwrap();
                std::panic::resume_unwind(Box::new("catalog job failed"));
                #[allow(unreachable_code)]
                transaction.commit().unwrap()
            })
            .await;
        match failed {
            Err(CatalogError::Task(error)) => {
                assert!(error.is_panic());
                assert_eq!(
                    *error.into_panic().downcast::<&str>().unwrap(),
                    "catalog job failed"
                );
            }
            other => panic!("unexpected outcome: {other:?}"),
        }
        // The same connection serves the next job, with the transaction rolled back.
        let value: i64 = owners
            .run(|connection| {
                assert!(connection.is_autocommit());
                connection
                    .query_row("SELECT v FROM t", [], |row| row.get(0))
                    .unwrap()
            })
            .await
            .unwrap();
        assert_eq!(value, 1);
    }

    #[tokio::test]
    async fn a_job_whose_caller_left_before_it_started_is_skipped() {
        let directory = tempfile::tempdir().unwrap();
        let owners = Arc::new(pool(&directory.path().join("pool.sqlite"), 1));
        let (started, has_started) = tokio::sync::oneshot::channel();
        let (release, released) = mpsc::channel::<()>();
        let blocker = tokio::spawn({
            let owners = Arc::clone(&owners);
            async move {
                owners
                    .run(move |_| {
                        started.send(()).unwrap();
                        released.recv().unwrap();
                    })
                    .await
            }
        });
        has_started.await.unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let (sent, was_sent) = tokio::sync::oneshot::channel::<()>();
        let victim = tokio::spawn({
            let owners = Arc::clone(&owners);
            let ran = Arc::clone(&ran);
            async move {
                let job = owners.run(move |_| ran.store(true, Ordering::SeqCst));
                tokio::pin!(job);
                // Poll once so the job is queued, then report and keep waiting.
                assert!(
                    std::future::poll_fn(|context| {
                        std::task::Poll::Ready(job.as_mut().poll(context).is_pending())
                    })
                    .await
                );
                drop(sent);
                job.await
            }
        });
        assert!(was_sent.await.is_err());
        victim.abort();
        assert!(victim.await.unwrap_err().is_cancelled());
        release.send(()).unwrap();
        blocker.await.unwrap().unwrap();
        // The owner reached this job, so it has already passed the skipped one.
        owners.run(|_| ()).await.unwrap();
        assert!(!ran.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn dropping_the_pool_waits_until_every_connection_is_closed() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pool.sqlite");
        let owners = pool(&path, 3);
        // Every connection touches the database, so each has the write-ahead
        // log open. SQLite removes the log when the last one closes, and only
        // if no other is closing at that moment.
        for value in 0..3 {
            owners
                .run(move |connection| {
                    connection
                        .execute_batch("CREATE TABLE IF NOT EXISTS t (v INTEGER);")
                        .unwrap();
                    connection
                        .execute("INSERT INTO t VALUES (?1)", params![value])
                        .unwrap();
                })
                .await
                .unwrap();
        }
        let log = directory.path().join("pool.sqlite-wal");
        assert!(log.exists());
        drop(owners);
        // No wait: the drop above returned only after the three closes.
        assert!(!log.exists());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_swaps_and_loads_keep_one_winner_per_revision() {
        const WRITERS: usize = 12;
        const SWAPS: u64 = 8;
        let directory = tempfile::tempdir().unwrap();
        let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let readers: Vec<_> = (0..8)
            .map(|_| {
                let catalog = catalog.clone();
                let done = Arc::clone(&done);
                tokio::spawn(async move {
                    let mut last = 0;
                    while !done.load(Ordering::SeqCst) {
                        let snapshot = catalog.load_shared_current().await.unwrap();
                        assert!(snapshot.revision >= last, "a load went back in time");
                        last = snapshot.revision;
                    }
                })
            })
            .collect();
        let writers: Vec<_> = (0..WRITERS)
            .map(|writer| {
                let catalog = catalog.clone();
                tokio::spawn(async move {
                    let mut won = Vec::new();
                    while (won.len() as u64) < SWAPS {
                        let current = catalog.load_current().await.unwrap();
                        let mut next = current.clone();
                        next.partitions.insert(
                            format!("writer-{writer}-{}", won.len()),
                            PartitionDefinition {
                                drives: BTreeMap::new(),
                            },
                        );
                        match catalog.compare_and_swap(current.revision, next).await {
                            Ok(revision) => {
                                assert_eq!(revision, current.revision + 1);
                                won.push(revision);
                            }
                            Err(CatalogError::Conflict) => {}
                            Err(error) => panic!("swap failed: {error:?}"),
                        }
                    }
                    won
                })
            })
            .collect();
        let mut won = Vec::new();
        for writer in writers {
            won.extend(writer.await.unwrap());
        }
        done.store(true, Ordering::SeqCst);
        for reader in readers {
            reader.await.unwrap();
        }
        // Every revision was granted to exactly one writer, and no winning
        // swap was lost: each added one partition to the one before it.
        won.sort_unstable();
        let total = WRITERS as u64 * SWAPS;
        assert_eq!(won, (1..=total).collect::<Vec<_>>());
        let last = catalog.load_current().await.unwrap();
        assert_eq!(last.revision, total);
        assert_eq!(last.partitions.len() as u64, total);
    }

    #[tokio::test]
    async fn the_last_catalog_handle_can_be_dropped_while_calls_are_queued() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let catalog = SqliteCatalog::open(&path).await.unwrap();
        let calls: Vec<_> = (0..64)
            .map(|_| {
                let catalog = catalog.clone();
                tokio::spawn(async move { catalog.load_shared_current().await.map(|_| ()) })
            })
            .collect();
        drop(catalog);
        for call in calls {
            call.await.unwrap().unwrap();
        }
        // Every handle is gone, so every pooled connection is closed.
        let reopened = SqliteCatalog::open(&path).await.unwrap();
        assert_eq!(reopened.load_current().await.unwrap().revision, 0);
    }
}
