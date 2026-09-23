//! Provider selection and split-store construction options.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

/// A provider selected for the metadata or immutable block side of a
/// split-store filesystem.
///
/// Credentials are passed as values by the application. Configuration files
/// and CLIs should resolve environment references before constructing this
/// value so the SDK never needs to know about a configuration-file schema.
#[derive(Clone, PartialEq, Eq)]
pub enum StoreConfig {
    Memory,
    Sqlite {
        path: PathBuf,
    },
    Pglite {
        connection: String,
        volume_key: String,
        durable: bool,
    },
    Tidb {
        connection: String,
        volume_key: String,
        durable: bool,
    },
    FoundationDb {
        cluster_file: PathBuf,
        volume_key: String,
        durable: bool,
        lease_authority: FoundationDbLeaseAuthority,
    },
    R2 {
        endpoint: String,
        bucket: String,
        prefix: String,
        access_key_id: String,
        secret_access_key: String,
        durable: bool,
    },
    /// AWS S3 block storage using the standard AWS workload credential chain.
    /// The provider is block-only; metadata remains an independent store.
    AwsS3 {
        bucket: String,
        region: String,
        prefix: String,
        durable: bool,
    },
}

impl fmt::Debug for StoreConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory => formatter.write_str("Memory"),
            Self::Sqlite { path } => formatter
                .debug_struct("Sqlite")
                .field("path", path)
                .finish(),
            Self::Pglite {
                connection: _,
                volume_key,
                durable,
            } => formatter
                .debug_struct("Pglite")
                .field("connection", &"<redacted>")
                .field("volume_key", volume_key)
                .field("durable", durable)
                .finish(),
            Self::Tidb {
                connection: _,
                volume_key,
                durable,
            } => formatter
                .debug_struct("Tidb")
                .field("connection", &"<redacted>")
                .field("volume_key", volume_key)
                .field("durable", durable)
                .finish(),
            Self::FoundationDb {
                cluster_file,
                volume_key,
                durable,
                lease_authority,
            } => formatter
                .debug_struct("FoundationDb")
                .field("cluster_file", cluster_file)
                .field("volume_key", volume_key)
                .field("durable", durable)
                .field("lease_authority", lease_authority)
                .finish(),
            Self::R2 {
                endpoint,
                bucket,
                prefix,
                durable,
                ..
            } => formatter
                .debug_struct("R2")
                .field("endpoint", endpoint)
                .field("bucket", bucket)
                .field("prefix", prefix)
                .field("access_key_id", &"<redacted>")
                .field("secret_access_key", &"<redacted>")
                .field("durable", durable)
                .finish(),
            Self::AwsS3 {
                bucket,
                region,
                prefix,
                durable,
            } => formatter
                .debug_struct("AwsS3")
                .field("bucket", bucket)
                .field("region", region)
                .field("prefix", prefix)
                .field("durable", durable)
                .finish(),
        }
    }
}

/// Lease authority choices exposed by consumer configuration.
///
/// The persisted choice is intentionally named as a single-authority mode:
/// it is suitable for an owned test cluster or one trusted writer authority.
/// Production consumers should select [`Self::SharedProvider`] with the
/// authority prefix published by a protected shared time authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FoundationDbLeaseAuthority {
    PersistedSingleAuthority,
    SharedProvider {
        authority_prefix: String,
    },
    /// Concurrent-volume manifest revision CAS needs no lease-time oracle.
    /// Only choose this with `SplitOptions::concurrent_writes`.
    RevisionCas,
}

/// Options for a filesystem with independent metadata and block providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitOptions {
    pub metadata: StoreConfig,
    pub blocks: StoreConfig,
    pub chunk_size_bytes: usize,
    pub owner: String,
    pub lease_ttl: Duration,
    /// Persisted, opt-in concurrent metadata mode; the provider must support
    /// revision CAS and fence legacy lease clients on the same volume.
    pub concurrent_writes: bool,
    pub uid: u32,
    pub gid: u32,
    pub umask: u32,
}

impl SplitOptions {
    /// Create a volatile split store using the in-process memory providers.
    pub fn memory(owner: impl Into<String>, chunk_size_bytes: usize) -> Self {
        Self {
            metadata: StoreConfig::Memory,
            blocks: StoreConfig::Memory,
            chunk_size_bytes,
            owner: owner.into(),
            lease_ttl: Duration::from_secs(30),
            concurrent_writes: false,
            uid: 0,
            gid: 0,
            umask: 0,
        }
    }

    /// Set the writer-lease TTL used by metadata providers.
    ///
    /// The default remains 30 seconds. Consumers with remote provider
    /// latency should choose an explicit value that covers their observed
    /// operation duration without making stale-writer recovery unbounded.
    pub fn with_lease_ttl(mut self, lease_ttl: Duration) -> Self {
        self.lease_ttl = lease_ttl;
        self
    }

    pub fn with_concurrent_writes(mut self, concurrent_writes: bool) -> Self {
        self.concurrent_writes = concurrent_writes;
        self
    }

    pub fn with_identity(mut self, uid: u32, gid: u32, umask: u32) -> Self {
        self.uid = uid;
        self.gid = gid;
        self.umask = umask;
        self
    }
}
