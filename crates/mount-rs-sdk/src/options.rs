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
    /// Owner-local immutable blocks on a host filesystem (block-only).
    Local {
        path: PathBuf,
    },
    /// Local blocks with shared server-local mutation admission; equality
    /// compares budget identity.
    LocalWithWriteBudget {
        path: PathBuf,
        budget: mount_rs_host::LocalWriteBudget,
    },
    Sqlite {
        path: PathBuf,
    },
    /// TiDB stores borrowed from an application-owned connection pool.
    /// Closing the filesystem does not disconnect the pool.
    TidbShared {
        pool: mount_rs_tidb::TidbStorePool,
        volume_key: String,
        durable: bool,
    },
    /// R2/S3-compatible blocks from an application-owned shared client.
    R2Shared {
        client: mount_rs_r2::SharedR2Client,
        prefix: String,
        durable: bool,
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
    /// SlateDB metadata on an S3-compatible service such as RustFS.
    /// SlateDB owns `path`; keep it separate from the block-store prefix.
    SlateDb {
        endpoint: String,
        bucket: String,
        region: String,
        path: String,
        access_key_id: String,
        secret_access_key: String,
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
    /// RustFS S3-compatible immutable blocks; metadata stays independent.
    RustFs {
        endpoint: String,
        bucket: String,
        region: String,
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
            Self::Local { path } => formatter.debug_struct("Local").field("path", path).finish(),
            Self::LocalWithWriteBudget { path, budget } => formatter
                .debug_struct("LocalWithWriteBudget")
                .field("path", path)
                .field("budget", budget)
                .finish(),
            Self::TidbShared {
                pool: _,
                volume_key,
                durable,
            } => formatter
                .debug_struct("TidbShared")
                .field("pool", &"<explicit shared owner>")
                .field("volume_key", volume_key)
                .field("durable", durable)
                .finish(),
            Self::R2Shared {
                client,
                prefix,
                durable,
            } => formatter
                .debug_struct("R2Shared")
                .field("client", client)
                .field("prefix", prefix)
                .field("durable", durable)
                .finish(),
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
            Self::SlateDb {
                endpoint,
                bucket,
                region,
                path,
                durable,
                ..
            } => formatter
                .debug_struct("SlateDb")
                .field("endpoint", &rustfs_debug_endpoint_authority(endpoint))
                .field("bucket", bucket)
                .field("region", region)
                .field("path", path)
                .field("access_key_id", &"<redacted>")
                .field("secret_access_key", &"<redacted>")
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
            Self::RustFs {
                endpoint,
                bucket,
                region,
                prefix,
                durable,
                ..
            } => formatter
                .debug_struct("RustFs")
                .field("endpoint", &rustfs_debug_endpoint_authority(endpoint))
                .field("bucket", bucket)
                .field("region", region)
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

fn rustfs_debug_endpoint_authority(endpoint: &str) -> String {
    let Some((scheme, remainder)) = endpoint.split_once("://") else {
        return "<redacted>".to_owned();
    };
    let authority = remainder.split('/').next().unwrap_or_default();
    if !matches!(scheme, "http" | "https")
        || endpoint.contains('?')
        || endpoint.contains('#')
        || !rustfs_valid_debug_authority(authority)
    {
        return "<redacted>".to_owned();
    }
    format!("{scheme}://{authority}")
}

fn rustfs_valid_debug_authority(authority: &str) -> bool {
    if authority.is_empty()
        || !authority.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':' | b'[' | b']')
        })
    {
        return false;
    }
    let port = if let Some(bracketed) = authority.strip_prefix('[') {
        let Some((host, suffix)) = bracketed.split_once(']') else {
            return false;
        };
        if host.parse::<std::net::Ipv6Addr>().is_err() {
            return false;
        }
        if suffix.is_empty() {
            None
        } else {
            let Some(port) = suffix.strip_prefix(':') else {
                return false;
            };
            Some(port)
        }
    } else {
        if authority.contains(['[', ']']) {
            return false;
        }
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        if host.is_empty()
            || !host
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        {
            return false;
        }
        port
    };
    port.is_none_or(|port| {
        !port.is_empty()
            && port.bytes().all(|byte| byte.is_ascii_digit())
            && port.parse::<u16>().is_ok_and(|value| value > 0)
    })
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

/// Limits for the process-local immutable-block write-through cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockCacheOptions {
    pub max_bytes: usize,
    pub max_entries: usize,
}

/// Persistent owner-local content cache, below the optional RAM cache.
/// The scope must identify the backing volume/generation, not just its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskCacheOptions {
    pub root: PathBuf,
    pub scope: String,
    /// Encoded bytes, including entry headers; must be at least 80.
    pub max_bytes: u64,
    pub max_entries: usize,
}

/// Owner-serving runtime policies for exclusive split stores: a 240-second
/// lease, a 60-second idle-owner heartbeat window and noatime by default.
/// Backing durability is unchanged. These override `SplitOptions::lease_ttl`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitRuntimeOptions {
    /// Authoritative ownership lease; default 240 seconds.
    pub lease_ttl: Duration,
    pub atime_policy: mount_rs_chunked::AtimePolicy,
    /// Idle-owner heartbeat window; zero disables the heartbeat. Must be at
    /// least 20 ms and at most a quarter of `lease_ttl`.
    pub read_lease_cache_window: Duration,
    pub block_cache: Option<BlockCacheOptions>,
    /// Concurrent block gets per read. Only 1 (sequential) is supported.
    pub read_parallelism: usize,
}

impl Default for SplitRuntimeOptions {
    fn default() -> Self {
        Self {
            lease_ttl: Duration::from_secs(240),
            read_lease_cache_window: Duration::from_secs(60),
            atime_policy: mount_rs_chunked::AtimePolicy::NoAtime,
            block_cache: None,
            read_parallelism: 1,
        }
    }
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
    /// Explicitly enroll and use per-inode metadata publication (MRC4).
    pub inode_updates: bool,
    /// Defer exclusive namespace publication until synchronization.
    pub writeback: bool,
    /// Persisted directory checkout authority (MRC3), distinct from legacy CAS.
    pub delegated: bool,
    /// Claim this virtual directory before exposing a fresh mount.
    pub checkout_path: Option<String>,
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
            inode_updates: false,
            writeback: false,
            delegated: false,
            checkout_path: None,
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
        self.delegated = false;
        self.checkout_path = None;
        self
    }

    /// Opt in to the persisted MRC4 format and per-inode publication.
    /// Enabling this also enables concurrent writes; disabling it preserves
    /// the selected concurrency and ownership settings.
    pub fn with_inode_updates(mut self, inode_updates: bool) -> Self {
        self.inode_updates = inode_updates;
        if inode_updates {
            self.concurrent_writes = true;
        }
        self
    }

    /// Select the mount authority. Explicit exclusive ownership enables
    /// writeback; shared ownership requires persisted directory checkout authority.
    pub fn with_ownership_mode(mut self, mode: mount_rs_chunked::OwnershipMode) -> Self {
        self.concurrent_writes = mode == mount_rs_chunked::OwnershipMode::Shared;
        self.writeback = mode == mount_rs_chunked::OwnershipMode::Exclusive;
        self.delegated = mode == mount_rs_chunked::OwnershipMode::Shared;
        if !self.delegated {
            self.checkout_path = None;
        }
        self
    }

    pub fn with_checkout_path(self, path: impl Into<String>) -> Self {
        let mut options = self.with_ownership_mode(mount_rs_chunked::OwnershipMode::Shared);
        options.checkout_path = Some(path.into());
        options
    }

    pub fn with_writeback(mut self, writeback: bool) -> Self {
        self.writeback = writeback;
        self
    }

    pub fn with_identity(mut self, uid: u32, gid: u32, umask: u32) -> Self {
        self.uid = uid;
        self.gid = gid;
        self.umask = umask;
        self
    }
}
