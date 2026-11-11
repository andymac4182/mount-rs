//! Versioned, side-effect-free CLI configuration.
//!
//! The config parser deliberately uses serde_json::Value instead of derive
//! support. That keeps the CLI's direct dependency surface small while still
//! allowing every object in the public schema to reject unknown fields.

use std::collections::BTreeSet;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use mount_rs_sdk::{AtimePolicy, BlockCacheOptions, DiskCacheOptions, SplitRuntimeOptions};
use serde_json::{Map, Value};

use crate::parser::{CliOptions, CliOverrides, DriverChoice, TransportChoice};

pub const CONFIG_VERSION: u64 = 1;
pub const DEFAULT_CHUNK_SIZE_BYTES: usize = 64 * 1024;
const DEFAULT_HTTP_HOST: &str = "127.0.0.1";
const DEFAULT_HTTP_PORT: u16 = 0;
const DEFAULT_HTTP_MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_HTTP_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_HTTP_MAX_DIRECTORY_ENTRIES: usize = 4096;
const DEFAULT_HTTP_READ_CHUNK_BYTES: usize = 64 * 1024;
// Longer than the request timeout: a graceful stop lets every accepted
// request finish or time out by itself before anything is aborted.
const DEFAULT_HTTP_DRAIN_TIMEOUT_MS: u64 = 35_000;
// Upload memory budget, as a multiple of `max_request_bytes`.
const DEFAULT_HTTP_BUFFERED_UPLOAD_REQUESTS: usize = 4;
const DEFAULT_HTTP_MAX_CONNECTIONS: usize = 256;
const DEFAULT_HTTP_REQUEST_TIMEOUT_MS: u64 = 30_000;

/// Generate a process-and-instance-specific owner for writer fencing. A
/// constant or PID-only default would let a restarted process, or a later
/// process after PID reuse, impersonate an older writer. `RandomState` is the
/// standard library's OS-seeded hash-key source; two independent 64-bit
/// outputs provide a compact process nonce without another dependency.
pub fn unique_default_owner() -> String {
    static NEXT_INSTANCE: AtomicU64 = AtomicU64::new(1);
    static PROCESS_NONCE: OnceLock<String> = OnceLock::new();
    let instance = NEXT_INSTANCE.fetch_add(1, Ordering::Relaxed);
    let nonce = PROCESS_NONCE.get_or_init(process_nonce);
    format!("mount-rs-cli-{nonce}-{instance}")
}

fn process_nonce() -> String {
    let first = RandomState::new();
    let second = RandomState::new();
    let first_hash = first.hash_one("mount-rs-cli-process");
    let second_hash = second.hash_one("mount-rs-cli-process");
    // Hashing a fixed marker is intentional: the entropy comes from the
    // independent OS-seeded RandomState keys, not from a predictable PID or
    // wall-clock value.
    format!("{:016x}{:016x}", first_hash, second_hash)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvReference {
    pub name: String,
}

#[derive(Clone, PartialEq, Eq)]
pub enum StorageProvider {
    Memory,
    Local {
        path: PathBuf,
    },
    Sqlite {
        path: PathBuf,
    },
    Pglite {
        connection: EnvReference,
        volume_key: String,
        durable: bool,
    },
    Tidb {
        connection: EnvReference,
        volume_key: String,
        durable: bool,
    },
    FoundationDb {
        cluster_file: PathBuf,
        volume_key: String,
        durable: bool,
        lease_authority: mount_rs_sdk::FoundationDbLeaseAuthority,
    },
    R2 {
        endpoint: String,
        bucket: String,
        prefix: String,
        access_key_id: EnvReference,
        secret_access_key: EnvReference,
        durable: bool,
    },
    RustFs {
        endpoint: String,
        bucket: String,
        region: String,
        prefix: String,
        access_key_id: EnvReference,
        secret_access_key: EnvReference,
        durable: bool,
    },
    AwsS3 {
        bucket: String,
        region: String,
        prefix: String,
        durable: bool,
    },
}

impl fmt::Debug for StorageProvider {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory => formatter.write_str("Memory"),
            Self::Local { path } => formatter.debug_struct("Local").field("path", path).finish(),
            Self::Sqlite { path } => formatter
                .debug_struct("Sqlite")
                .field("path", path)
                .finish(),
            Self::Pglite {
                connection,
                volume_key,
                durable,
            } => formatter
                .debug_struct("Pglite")
                .field("connection", connection)
                .field("volume_key", volume_key)
                .field("durable", durable)
                .finish(),
            Self::Tidb {
                connection,
                volume_key,
                durable,
            } => formatter
                .debug_struct("Tidb")
                .field("connection", connection)
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
                access_key_id,
                secret_access_key,
                durable,
            } => formatter
                .debug_struct("R2")
                .field("endpoint", endpoint)
                .field("bucket", bucket)
                .field("prefix", prefix)
                .field("access_key_id", access_key_id)
                .field("secret_access_key", secret_access_key)
                .field("durable", durable)
                .finish(),
            Self::RustFs {
                endpoint,
                bucket,
                region,
                prefix,
                access_key_id,
                secret_access_key,
                durable,
            } => formatter
                .debug_struct("RustFs")
                .field("endpoint", &rustfs_debug_endpoint_authority(endpoint))
                .field("bucket", bucket)
                .field("region", region)
                .field("prefix", prefix)
                .field("access_key_id", access_key_id)
                .field("secret_access_key", secret_access_key)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitStorageConfig {
    pub verify_backing_blocks: bool,
    pub disk_cache: Option<DiskCacheOptions>,
    /// Owner-serving policies; present only for exclusive ownership.
    pub runtime: Option<SplitRuntimeOptions>,
    pub metadata: StorageProvider,
    pub blocks: StorageProvider,
    pub chunk_size_bytes: usize,
    pub lease_ttl_ms: Option<u64>,
    pub concurrent_writes: bool,
    pub inode_updates: bool,
    pub writeback: bool,
    pub delegated: bool,
    pub checkout_path: Option<String>,
    pub owner: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteConfig {
    pub server: String,
    pub volume: String,
    pub token: EnvReference,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConfigSpec {
    pub mountpoint: Option<PathBuf>,
    pub transport: Option<TransportChoice>,
    pub quiet: Option<bool>,
    pub verbose: Option<bool>,
    pub read_only: Option<bool>,
    pub empty: Option<bool>,
    pub allow_other: Option<bool>,
    pub fuse_uncached: Option<bool>,
    pub sqlite_single_host: Option<bool>,
    pub driver: Option<DriverChoice>,
    pub root: Option<PathBuf>,
    pub database: Option<PathBuf>,
    pub blocks: Option<PathBuf>,
    /// Desired ownership of the virtual SQLite filesystem root. The JSON
    /// spelling is driver.uid/driver.gid; the internal names avoid
    /// confusing this metadata with the host process identity.
    pub root_uid: Option<u32>,
    pub root_gid: Option<u32>,
    pub storage: Option<SplitStorageConfig>,
    pub remote: Option<RemoteConfig>,
    pub(crate) http: Option<HttpServiceConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpServiceConfig {
    pub remote_sessions: bool,
    pub forwarding: Option<ForwardingConfig>,
    pub host: String,
    pub port: u16,
    pub max_request_bytes: usize,
    /// Memory that uploads being received may hold in total.
    pub max_buffered_upload_bytes: usize,
    pub max_response_bytes: usize,
    pub max_directory_entries: usize,
    pub read_chunk_bytes: usize,
    pub tcp_nodelay: bool,
    pub http2_prior_knowledge: bool,
    pub drain_timeout_ms: u64,
    pub max_connections: usize,
    pub request_timeout_ms: u64,
    pub drives: Vec<HttpDriveConfig>,
    pub disk_cache_budget: Option<DiskCacheBudgetConfig>,
    pub tidb_metadata_pool_limit: Option<usize>,
    pub cache_report_interval_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ForwardingConfig {
    pub peers: PathBuf,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub ca: PathBuf,
    pub redis_url: EnvReference,
    pub namespace: String,
    pub bind: std::net::SocketAddr,
    pub owner: bool,
    pub refresh_active_routes: bool,
    pub route_refresh_idle_ttls: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiskCacheBudgetConfig {
    pub workers: usize,
    pub fill_copies: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HttpDriveConfig {
    pub id: String,
    pub token: EnvReference,
    pub driver: ConfigSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    message: String,
}

impl ConfigError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: redact_diagnostic(&message.into()),
        }
    }

    fn at(path: &str, message: impl Into<String>) -> Self {
        Self::new(format!("{path}: {}", message.into()))
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for ConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Parse and statically validate a config file. This function reads only the
/// JSON file; it never opens a database, resolves a credential, or probes a
/// network provider.
pub fn load_config(path: &Path) -> Result<ConfigSpec, ConfigError> {
    let source = fs::read_to_string(path).map_err(|error| {
        ConfigError::new(format!("cannot read config {}: {error}", path.display()))
    })?;
    let base = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    parse_config_str(&source, base.unwrap_or_else(|| Path::new(".")))
}

/// Parse and statically validate JSON using base_dir for relative storage
/// paths. This is public so tests can prove validation is mount-free without
/// creating temporary providers.
pub fn parse_config_str(source: &str, base_dir: &Path) -> Result<ConfigSpec, ConfigError> {
    let value: Value = serde_json::from_str(source)
        .map_err(|error| ConfigError::new(format!("invalid JSON configuration: {error}")))?;
    parse_config_value(&value, base_dir)
}

/// Resolve a parsed CLI command. Only flags that were explicitly supplied on
/// the command line override config values; parser defaults do not.
pub fn resolve_cli_options(raw: CliOptions) -> Result<CliOptions, ConfigError> {
    let Some(config_path) = raw.config.clone() else {
        validate_resolved_options(&raw)?;
        return Ok(raw);
    };

    let spec = load_config(&config_path)?;
    if spec.http.is_some() {
        return Err(ConfigError::at(
            "config.http",
            "is only valid with 'serve-http --config <path>'",
        ));
    }
    let mut resolved = spec.to_options();
    resolved.config = Some(config_path);
    apply_explicit_overrides(&mut resolved, &raw);
    resolved.overrides = raw.overrides;
    validate_resolved_options(&resolved)?;
    Ok(resolved)
}

/// The validate-config command intentionally stops at this static boundary.
pub fn validate_config_file(path: &Path) -> Result<(), ConfigError> {
    let spec = load_config(path)?;
    let options = spec.to_options();
    validate_resolved_options(&options)
}

fn parse_config_value(value: &Value, base_dir: &Path) -> Result<ConfigSpec, ConfigError> {
    let object = object(value, "config")?;
    reject_unknown(
        object,
        &[
            "version",
            "mountpoint",
            "transport",
            "quiet",
            "verbose",
            "read_only",
            "empty",
            "allow_other",
            "fuse_uncached",
            "sqlite_single_host",
            "driver",
            "http",
        ],
        "config",
    )?;

    let version = required_u64(object, "version", "config")?;
    if version != CONFIG_VERSION {
        return Err(ConfigError::at(
            "config.version",
            format!("unsupported version {version}; expected {CONFIG_VERSION}"),
        ));
    }

    let mut spec = ConfigSpec {
        mountpoint: optional_path(object, "mountpoint", "config", base_dir)?,
        transport: optional_transport(object, "transport")?,
        quiet: optional_bool(object, "quiet", "config")?,
        verbose: optional_bool(object, "verbose", "config")?,
        read_only: optional_bool(object, "read_only", "config")?,
        empty: optional_bool(object, "empty", "config")?,
        allow_other: optional_bool(object, "allow_other", "config")?,
        fuse_uncached: optional_bool(object, "fuse_uncached", "config")?,
        sqlite_single_host: optional_bool(object, "sqlite_single_host", "config")?,
        ..ConfigSpec::default()
    };

    if let Some(driver) = object.get("driver") {
        parse_driver(driver, base_dir, &mut spec, "config.driver")?;
    }
    if let Some(http) = object.get("http") {
        spec.http = Some(parse_http(http, base_dir)?);
        if has_native_fields(&spec) {
            return Err(ConfigError::at(
                "config.http",
                "cannot be combined with native mount fields",
            ));
        }
    }
    validate_spec(&spec)
}

fn parse_driver(
    value: &Value,
    base_dir: &Path,
    spec: &mut ConfigSpec,
    path: &str,
) -> Result<(), ConfigError> {
    let object = object(value, path)?;
    let kind = required_string(object, "kind", path)?;
    match kind {
        "remote" => {
            reject_unknown(
                object,
                &["kind", "server", "volume", "token", "timeout_ms"],
                path,
            )?;
            let server = required_string(object, "server", path)?.to_owned();
            let volume = required_string(object, "volume", path)?.to_owned();
            mount_rs_remote::RemoteSession::validate_endpoint(&server, &volume)
                .map_err(|_| ConfigError::at(path, "invalid remote server origin or volume"))?;
            let timeout_ms = match object.get("timeout_ms") {
                None => 30_000,
                Some(value) => value
                    .as_u64()
                    .filter(|n| (1..=300_000).contains(n))
                    .ok_or_else(|| ConfigError::at(path, "timeout_ms must be 1..300000"))?,
            };
            spec.remote = Some(RemoteConfig {
                server,
                volume,
                timeout_ms,
                token: required_env_reference(object, "token", &format!("{path}.token"))?,
            });
            spec.driver = Some(DriverChoice::Remote);
        }
        "memory" => {
            reject_unknown(object, &["kind"], path)?;
            spec.driver = Some(DriverChoice::Memory);
        }
        "host" => {
            reject_unknown(object, &["kind", "root"], path)?;
            spec.driver = Some(DriverChoice::Host);
            spec.root = Some(required_path(object, "root", path, base_dir)?);
        }
        "sqlite" => {
            reject_unknown(object, &["kind", "database", "uid", "gid"], path)?;
            spec.driver = Some(DriverChoice::Sqlite);
            spec.database = Some(required_path(object, "database", path, base_dir)?);
            spec.root_uid = optional_u32(object, "uid", path)?;
            spec.root_gid = optional_u32(object, "gid", path)?;
            if spec.root_uid.is_some() != spec.root_gid.is_some() {
                return Err(ConfigError::at(
                    path,
                    "uid and gid must be supplied together",
                ));
            }
        }
        "splitstore" => {
            spec.driver = Some(DriverChoice::SplitStore);
            if object.contains_key("storage") {
                // The structured form cannot be mixed with legacy SQLite
                // path fields or irrelevant host-driver fields.
                reject_unknown(object, &["kind", "storage"], path)?;
                spec.storage = Some(parse_storage(
                    object
                        .get("storage")
                        .ok_or_else(|| ConfigError::at(&format!("{path}.storage"), "is missing"))?,
                    base_dir,
                )?);
            } else {
                reject_unknown(object, &["kind", "database", "blocks"], path)?;
                spec.database = optional_path(object, "database", path, base_dir)?;
                spec.blocks = optional_path(object, "blocks", path, base_dir)?;
                if spec.database.is_some() != spec.blocks.is_some() {
                    return Err(ConfigError::at(
                        path,
                        "legacy splitstore database and blocks must be supplied together",
                    ));
                }
            }
        }
        _ => {
            return Err(ConfigError::at(
                &format!("{path}.kind"),
                format!("unknown driver '{kind}'"),
            ));
        }
    }
    Ok(())
}

fn parse_disk_budget(value: &Value) -> Result<DiskCacheBudgetConfig, ConfigError> {
    let path = "config.http.disk_cache_budget";
    let fields = object(value, path)?;
    reject_unknown(fields, &["workers", "fill_copies"], path)?;
    let limit = |name: &str| -> Result<usize, ConfigError> {
        let field = format!("{path}.{name}");
        let value = fields
            .get(name)
            .ok_or_else(|| ConfigError::at(&field, "is required"))?;
        let value = positive_usize(value, &field)?;
        if value > 512 {
            return Err(ConfigError::at(&field, "must be 1..512"));
        }
        Ok(value)
    };
    Ok(DiskCacheBudgetConfig {
        workers: limit("workers")?,
        fill_copies: limit("fill_copies")?,
    })
}

fn validate_cache_capacity(value: &Value, drives: &[HttpDriveConfig]) -> Result<(), ConfigError> {
    let path = "config.http.cache_capacity";
    let fields = object(value, path)?;
    reject_unknown(fields, &["ram_bytes", "disk_bytes"], path)?;
    let limit = |name: &str| -> Result<u64, ConfigError> {
        fields.get(name).and_then(Value::as_u64).ok_or_else(|| {
            ConfigError::at(&format!("{path}.{name}"), "must be a non-negative integer")
        })
    };
    let ram_limit = limit("ram_bytes")?;
    let disk_limit = limit("disk_bytes")?;
    let mut ram = 0u64;
    let mut disk = 0u64;
    for drive in drives {
        if let Some(storage) = &drive.driver.storage {
            let ram_bytes = storage
                .runtime
                .and_then(|runtime| runtime.block_cache)
                .map_or(0, |cache| cache.max_bytes);
            let ram_bytes = u64::try_from(ram_bytes)
                .map_err(|_| ConfigError::at(path, "RAM capacity overflow"))?;
            ram = ram
                .checked_add(ram_bytes)
                .ok_or_else(|| ConfigError::at(path, "RAM capacity overflow"))?;
            disk = disk
                .checked_add(
                    storage
                        .disk_cache
                        .as_ref()
                        .map_or(0, |cache| cache.max_bytes),
                )
                .ok_or_else(|| ConfigError::at(path, "disk capacity overflow"))?;
        }
    }
    if ram > ram_limit || disk > disk_limit {
        return Err(ConfigError::at(
            path,
            format!(
                "configured cache reservations exceed capacity: RAM {ram}/{ram_limit} bytes, disk {disk}/{disk_limit} bytes"
            ),
        ));
    }
    Ok(())
}

fn parse_http(value: &Value, base_dir: &Path) -> Result<HttpServiceConfig, ConfigError> {
    let object = object(value, "config.http")?;
    reject_unknown(
        object,
        &[
            "host",
            "port",
            "max_request_bytes",
            "max_buffered_upload_bytes",
            "max_response_bytes",
            "max_directory_entries",
            "read_chunk_bytes",
            "tcp_nodelay",
            "http2_prior_knowledge",
            "drain_timeout_ms",
            "max_connections",
            "request_timeout_ms",
            "disk_cache_budget",
            "tidb_metadata_pool_limit",
            "cache_report_interval_seconds",
            "remote_sessions",
            "forwarding",
            "cache_capacity",
            "drives",
        ],
        "config.http",
    )?;
    let host = object
        .get("host")
        .map(|value| required_value_string(value, "config.http.host"))
        .transpose()?
        .unwrap_or_else(|| DEFAULT_HTTP_HOST.to_owned());
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err(ConfigError::at(
            "config.http.host",
            "must be non-empty and contain no whitespace",
        ));
    }
    if !is_loopback_http_host(&host) {
        return Err(ConfigError::at(
            "config.http.host",
            "must be a loopback host; use a TLS reverse proxy for remote clients",
        ));
    }
    let port = optional_u16(object, "port", "config.http")?.unwrap_or(DEFAULT_HTTP_PORT);
    let max_request_bytes = object
        .get("max_request_bytes")
        .map(|value| positive_usize(value, "config.http.max_request_bytes"))
        .transpose()?
        .unwrap_or(DEFAULT_HTTP_MAX_REQUEST_BYTES);
    let max_buffered_upload_bytes = match object.get("max_buffered_upload_bytes") {
        Some(value) => {
            let budget = positive_usize(value, "config.http.max_buffered_upload_bytes")?;
            if budget < max_request_bytes {
                return Err(ConfigError::at(
                    "config.http.max_buffered_upload_bytes",
                    "must be at least max_request_bytes, or no upload of that size could start",
                ));
            }
            budget
        }
        None => max_request_bytes.saturating_mul(DEFAULT_HTTP_BUFFERED_UPLOAD_REQUESTS),
    };
    let max_response_bytes = object
        .get("max_response_bytes")
        .map(|value| positive_usize(value, "config.http.max_response_bytes"))
        .transpose()?
        .unwrap_or(DEFAULT_HTTP_MAX_RESPONSE_BYTES);
    let max_directory_entries = object
        .get("max_directory_entries")
        .map(|value| positive_usize(value, "config.http.max_directory_entries"))
        .transpose()?
        .unwrap_or(DEFAULT_HTTP_MAX_DIRECTORY_ENTRIES);
    let read_chunk_bytes = object
        .get("read_chunk_bytes")
        .map(|value| positive_usize(value, "config.http.read_chunk_bytes"))
        .transpose()?
        .unwrap_or(DEFAULT_HTTP_READ_CHUNK_BYTES);
    let drain_timeout_ms = object
        .get("drain_timeout_ms")
        .map(|value| positive_u64(value, "config.http.drain_timeout_ms"))
        .transpose()?
        .unwrap_or(DEFAULT_HTTP_DRAIN_TIMEOUT_MS);
    let max_connections = object
        .get("max_connections")
        .map(|value| positive_usize(value, "config.http.max_connections"))
        .transpose()?
        .unwrap_or(DEFAULT_HTTP_MAX_CONNECTIONS);
    let request_timeout_ms = object
        .get("request_timeout_ms")
        .map(|value| positive_u64(value, "config.http.request_timeout_ms"))
        .transpose()?
        .unwrap_or(DEFAULT_HTTP_REQUEST_TIMEOUT_MS);
    let tcp_nodelay = optional_bool(object, "tcp_nodelay", "config.http")?.unwrap_or(true);
    let drives = object
        .get("drives")
        .ok_or_else(|| ConfigError::at("config.http.drives", "is required"))?
        .as_array()
        .ok_or_else(|| ConfigError::at("config.http.drives", "must be an array"))?;
    let forwarding = object
        .get("forwarding")
        .map(|value| parse_forwarding(value, base_dir))
        .transpose()?;
    if drives.is_empty() && forwarding.is_none() {
        return Err(ConfigError::at(
            "config.http.drives",
            "must contain at least one drive",
        ));
    }
    let mut ids = BTreeSet::new();
    let mut parsed_drives = Vec::with_capacity(drives.len());
    for (index, drive) in drives.iter().enumerate() {
        let path = format!("config.http.drives[{index}]");
        let parsed = parse_http_drive(drive, base_dir, &path)?;
        if !ids.insert(parsed.id.clone()) {
            return Err(ConfigError::at(
                &format!("{path}.id"),
                "duplicates another configured drive id",
            ));
        }
        parsed_drives.push(parsed);
    }
    if let Some(capacity) = object.get("cache_capacity") {
        validate_cache_capacity(capacity, &parsed_drives)?;
    }
    let remote_sessions = optional_bool(object, "remote_sessions", "config.http")?.unwrap_or(false);
    if parsed_drives
        .iter()
        .any(|drive| drive.driver.driver != Some(DriverChoice::SplitStore))
    {
        let option = if remote_sessions {
            Some("config.http.remote_sessions")
        } else if forwarding.as_ref().is_some_and(|config| config.owner) {
            Some("config.http.forwarding.owner")
        } else {
            None
        };
        if let Some(option) = option {
            return Err(ConfigError::at(
                option,
                "requires splitstore drivers for every drive",
            ));
        }
    }
    Ok(HttpServiceConfig {
        forwarding,
        remote_sessions,
        host,
        port,
        max_request_bytes,
        max_buffered_upload_bytes,
        max_response_bytes,
        max_directory_entries,
        read_chunk_bytes,
        tcp_nodelay,
        http2_prior_knowledge: optional_bool(object, "http2_prior_knowledge", "config.http")?
            .unwrap_or(false),
        drain_timeout_ms,
        max_connections,
        request_timeout_ms,
        drives: parsed_drives,
        tidb_metadata_pool_limit: object
            .get("tidb_metadata_pool_limit")
            .map(|v| positive_usize(v, "config.http.tidb_metadata_pool_limit"))
            .transpose()?,
        cache_report_interval_seconds: object
            .get("cache_report_interval_seconds")
            .map(|value| positive_u64(value, "config.http.cache_report_interval_seconds"))
            .transpose()?,
        disk_cache_budget: object
            .get("disk_cache_budget")
            .map(parse_disk_budget)
            .transpose()?,
    })
}

fn parse_forwarding(value: &Value, base: &Path) -> Result<ForwardingConfig, ConfigError> {
    let path = "config.http.forwarding";
    let fields = object(value, path)?;
    reject_unknown(
        fields,
        &[
            "peers",
            "certificate",
            "private_key",
            "ca",
            "redis_url",
            "namespace",
            "bind",
            "owner",
            "refresh_active_routes",
            "route_refresh_idle_ttls",
        ],
        path,
    )?;
    let namespace = required_string(fields, "namespace", path)?;
    if namespace.is_empty()
        || namespace.len() > 256
        || !namespace
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
    {
        return Err(ConfigError::at(
            "config.http.forwarding.namespace",
            "invalid registry namespace",
        ));
    }
    let route_refresh_idle_ttls =
        optional_u32(fields, "route_refresh_idle_ttls", path)?.unwrap_or(5);
    if !(1..=60).contains(&route_refresh_idle_ttls) {
        return Err(ConfigError::at(
            "config.http.forwarding.route_refresh_idle_ttls",
            "must be between 1 and 60",
        ));
    }
    Ok(ForwardingConfig {
        route_refresh_idle_ttls,
        refresh_active_routes: optional_bool(fields, "refresh_active_routes", path)?
            .unwrap_or(false),
        owner: optional_bool(fields, "owner", path)?.unwrap_or(false),
        peers: required_path(fields, "peers", path, base)?,
        certificate: required_path(fields, "certificate", path, base)?,
        private_key: required_path(fields, "private_key", path, base)?,
        ca: required_path(fields, "ca", path, base)?,
        redis_url: required_env_reference(fields, "redis_url", path)?,
        namespace: namespace.to_owned(),
        bind: required_string(fields, "bind", path)?
            .parse()
            .map_err(|_| {
                ConfigError::at("config.http.forwarding.bind", "invalid UDP socket address")
            })?,
    })
}

fn is_loopback_http_host(host: &str) -> bool {
    let normalized = match (host.starts_with('['), host.ends_with(']')) {
        (true, true) if host.len() > 2 => &host[1..host.len() - 1],
        (false, false) if !host.contains(['[', ']']) => host,
        _ => return false,
    };
    normalized.eq_ignore_ascii_case("localhost")
        || normalized
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn parse_http_drive(
    value: &Value,
    base_dir: &Path,
    path: &str,
) -> Result<HttpDriveConfig, ConfigError> {
    let object = object(value, path)?;
    reject_unknown(object, &["id", "token", "driver"], path)?;
    let id = required_value_string(
        object
            .get("id")
            .ok_or_else(|| ConfigError::at(&format!("{path}.id"), "is required"))?,
        &format!("{path}.id"),
    )?;
    mount_rs_http::DriveConfig::validate_id(&id)
        .map_err(|error| ConfigError::at(&format!("{path}.id"), error.to_string()))?;
    let token = required_env_reference(object, "token", &format!("{path}.token"))?;
    let driver_value = object
        .get("driver")
        .ok_or_else(|| ConfigError::at(&format!("{path}.driver"), "is required"))?;
    let mut driver = ConfigSpec::default();
    parse_driver(
        driver_value,
        base_dir,
        &mut driver,
        &format!("{path}.driver"),
    )?;
    if driver.remote.is_some() {
        return Err(ConfigError::at(
            path,
            "remote drivers are mount clients, not HTTP service providers",
        ));
    }
    validate_spec(&driver)?;
    Ok(HttpDriveConfig { id, token, driver })
}

fn parse_storage(value: &Value, base_dir: &Path) -> Result<SplitStorageConfig, ConfigError> {
    let object = object(value, "config.driver.storage")?;
    reject_unknown(
        object,
        &[
            "metadata",
            "blocks",
            "chunk_size_bytes",
            "lease_ttl_ms",
            "concurrent_writes",
            "inode_updates",
            "ownership_mode",
            "checkout_path",
            "owner",
            "atime",
            "verify_backing_blocks",
            "block_cache",
            "disk_cache",
            "read_parallelism",
            "read_lease_cache_window_ms",
        ],
        "config.driver.storage",
    )?;
    let metadata = parse_provider(
        object
            .get("metadata")
            .ok_or_else(|| ConfigError::at("config.driver.storage.metadata", "is missing"))?,
        "config.driver.storage.metadata",
        base_dir,
        false,
    )?;
    let blocks = parse_provider(
        object
            .get("blocks")
            .ok_or_else(|| ConfigError::at("config.driver.storage.blocks", "is missing"))?,
        "config.driver.storage.blocks",
        base_dir,
        true,
    )?;
    let chunk_size_bytes = object
        .get("chunk_size_bytes")
        .map(|value| positive_usize(value, "config.driver.storage.chunk_size_bytes"))
        .transpose()?
        .unwrap_or(DEFAULT_CHUNK_SIZE_BYTES);
    let lease_ttl_ms = object
        .get("lease_ttl_ms")
        .map(|value| positive_u64(value, "config.driver.storage.lease_ttl_ms"))
        .transpose()?;
    let legacy_concurrent = object
        .get("concurrent_writes")
        .map(|value| required_value_bool(value, "config.driver.storage.concurrent_writes"))
        .transpose()?;
    let inode_updates = object
        .get("inode_updates")
        .map(|value| required_value_bool(value, "config.driver.storage.inode_updates"))
        .transpose()?
        .unwrap_or(false);
    let ownership = object
        .get("ownership_mode")
        .map(|value| required_value_string(value, "config.driver.storage.ownership_mode"))
        .transpose()?;
    let (concurrent_writes, writeback) = match ownership.as_deref() {
        Some("exclusive") => (false, true),
        Some("shared") => (true, false),
        None => (legacy_concurrent.unwrap_or(inode_updates), false),
        Some(_) => {
            return Err(ConfigError::at(
                "config.driver.storage.ownership_mode",
                "must be 'exclusive' or 'shared'",
            ));
        }
    };
    if ownership.is_some() && legacy_concurrent.is_some_and(|legacy| legacy != concurrent_writes) {
        return Err(ConfigError::at(
            "config.driver.storage.concurrent_writes",
            "conflicts with ownership_mode",
        ));
    }
    let delegated = ownership.as_deref() == Some("shared");
    if inode_updates && (!concurrent_writes || delegated || writeback) {
        return Err(ConfigError::at(
            "config.driver.storage.inode_updates",
            "requires concurrent writes without directory ownership or writeback",
        ));
    }
    let checkout_path = object
        .get("checkout_path")
        .map(|value| required_value_string(value, "config.driver.storage.checkout_path"))
        .transpose()?;
    if delegated && checkout_path.is_none() {
        return Err(ConfigError::at(
            "config.driver.storage.checkout_path",
            "is required with ownership_mode 'shared' to claim the subtree before mounting",
        ));
    }
    if let Some(path) = &checkout_path {
        if !delegated {
            return Err(ConfigError::at(
                "config.driver.storage.checkout_path",
                "is only valid with ownership_mode 'shared'",
            ));
        }
        if !path.starts_with('/')
            || path.contains('\0')
            || (path != "/"
                && path
                    .split('/')
                    .skip(1)
                    .any(|part| part.is_empty() || part == "." || part == ".."))
        {
            return Err(ConfigError::at(
                "config.driver.storage.checkout_path",
                "must be a canonical absolute virtual path",
            ));
        }
    }
    if concurrent_writes {
        for role in ["metadata", "blocks"] {
            let raw = object.get(role).and_then(Value::as_object);
            if raw
                .and_then(|provider| provider.get("kind"))
                .and_then(Value::as_str)
                == Some("sqlite")
                && raw
                    .and_then(|provider| provider.get("path"))
                    .and_then(Value::as_str)
                    .is_some_and(|path| path == ":memory:" || path.starts_with("file:"))
            {
                return Err(ConfigError::at(
                    &format!("config.driver.storage.{role}.path"),
                    "concurrent SQLite storage requires a durable local database file path",
                ));
            }
        }
        if lease_ttl_ms.is_some() {
            return Err(ConfigError::at(
                "config.driver.storage.lease_ttl_ms",
                "is unused with concurrent_writes; omit the writer lease TTL",
            ));
        }
        match &metadata {
            StorageProvider::FoundationDb {
                lease_authority: mount_rs_sdk::FoundationDbLeaseAuthority::RevisionCas,
                ..
            }
            | StorageProvider::Pglite { .. }
            | StorageProvider::Tidb { .. } => {}
            StorageProvider::Sqlite { path } if sqlite_durable_path(path) => {}
            StorageProvider::Sqlite { .. } => {
                return Err(ConfigError::at(
                    "config.driver.storage.metadata.path",
                    "concurrent_writes SQLite metadata requires a durable local database path",
                ));
            }
            _ => {
                return Err(ConfigError::at(
                    "config.driver.storage.metadata",
                    "concurrent_writes requires SQLite, PGlite, TiDB, or FoundationDB metadata with lease_authority 'revision-cas'",
                ));
            }
        }
        match &blocks {
            StorageProvider::Memory => {
                return Err(ConfigError::at(
                    "config.driver.storage.blocks",
                    "concurrent_writes requires a shared block provider; memory blocks cannot serve independent mounts",
                ));
            }
            StorageProvider::Sqlite { path }
                if !matches!(&metadata, StorageProvider::Sqlite { .. })
                    || !sqlite_durable_path(path) =>
            {
                return Err(ConfigError::at(
                    "config.driver.storage.blocks",
                    "concurrent_writes requires a shared block provider; local SQLite blocks require local SQLite metadata and a durable path on the same host",
                ));
            }
            _ => {}
        }
    } else if !concurrent_writes
        && matches!(
            metadata,
            StorageProvider::FoundationDb {
                lease_authority: mount_rs_sdk::FoundationDbLeaseAuthority::RevisionCas,
                ..
            }
        )
    {
        return Err(ConfigError::at(
            "config.driver.storage.concurrent_writes",
            "must be true with FoundationDB lease_authority 'revision-cas'",
        ));
    }
    let owner = object
        .get("owner")
        .map(|value| required_value_string(value, "config.driver.storage.owner"))
        .transpose()?;
    if let Some(owner) = &owner {
        validate_owner(owner)?;
    }
    let atime_policy = match object
        .get("atime")
        .map(|value| required_value_string(value, "config.driver.storage.atime"))
        .transpose()?
        .as_deref()
    {
        Some("strict") => AtimePolicy::Strict,
        None | Some("noatime") => AtimePolicy::NoAtime,
        Some(_) => {
            return Err(ConfigError::at(
                "config.driver.storage.atime",
                "must be strict or noatime",
            ));
        }
    };
    // Owner-serving runtime policies are exclusive-ownership only. Shared
    // and concurrent modes keep their own lease and atime semantics.
    let exclusive = !concurrent_writes && !delegated;
    if !exclusive {
        for key in [
            "atime",
            "verify_backing_blocks",
            "block_cache",
            "disk_cache",
            "read_parallelism",
            "read_lease_cache_window_ms",
        ] {
            if object.contains_key(key) {
                return Err(ConfigError::at(
                    &format!("config.driver.storage.{key}"),
                    "requires exclusive ownership (no concurrent_writes or shared ownership_mode)",
                ));
            }
        }
    }
    let runtime_lease_ttl_ms = lease_ttl_ms.unwrap_or(240_000);
    let lease_ttl = std::time::Duration::from_millis(runtime_lease_ttl_ms);
    let window_ms = match object.get("read_lease_cache_window_ms") {
        // Default to 60 s, capped at a quarter of the lease; leases too short
        // for a 20 ms window fall back to per-operation renewal.
        None => match (runtime_lease_ttl_ms / 4).min(60_000) {
            window if window < 20 => 0,
            window => window,
        },
        Some(value) => value.as_u64().ok_or_else(|| {
            ConfigError::at(
                "config.driver.storage.read_lease_cache_window_ms",
                "must be a nonnegative integer",
            )
        })?,
    };
    if window_ms != 0 && (window_ms < 20 || window_ms > runtime_lease_ttl_ms / 4) {
        return Err(ConfigError::at(
            "config.driver.storage.read_lease_cache_window_ms",
            "must be zero or at least 20ms and at most one quarter of lease_ttl_ms",
        ));
    }
    let read_lease_cache_window = std::time::Duration::from_millis(window_ms);
    let block_cache = object
        .get("block_cache")
        .map(|value| {
            let path = "config.driver.storage.block_cache";
            let fields = value
                .as_object()
                .ok_or_else(|| ConfigError::at(path, "must be an object"))?;
            reject_unknown(fields, &["max_bytes", "max_entries"], path)?;
            let limit = |name: &str| {
                positive_usize(
                    fields
                        .get(name)
                        .ok_or_else(|| ConfigError::at(path, format!("{name} is missing")))?,
                    &format!("{path}.{name}"),
                )
            };
            Ok::<_, ConfigError>(BlockCacheOptions {
                max_bytes: limit("max_bytes")?,
                max_entries: limit("max_entries")?,
            })
        })
        .transpose()?;
    let disk_cache = object
        .get("disk_cache")
        .map(|value| {
            let path = "config.driver.storage.disk_cache";
            let fields = value
                .as_object()
                .ok_or_else(|| ConfigError::at(path, "must be an object"))?;
            reject_unknown(fields, &["root", "scope", "max_bytes", "max_entries"], path)?;
            let scope = required_string(fields, "scope", path)?;
            if scope.is_empty() {
                return Err(ConfigError::at(path, "scope must not be empty"));
            }
            let limit = |key: &str| {
                positive_usize(
                    fields
                        .get(key)
                        .ok_or_else(|| ConfigError::at(path, format!("{key} is missing")))?,
                    &format!("{path}.{key}"),
                )
            };
            let max_bytes = limit("max_bytes")? as u64;
            if max_bytes < 80 {
                return Err(ConfigError::at(path, "max_bytes must be at least 80"));
            }
            Ok(DiskCacheOptions {
                root: required_path(fields, "root", path, base_dir)?,
                scope: scope.to_owned(),
                max_bytes,
                max_entries: limit("max_entries")?,
            })
        })
        .transpose()?;
    Ok(SplitStorageConfig {
        verify_backing_blocks: optional_bool(
            object,
            "verify_backing_blocks",
            "config.driver.storage",
        )?
        .unwrap_or(false),
        disk_cache,
        runtime: exclusive.then_some(SplitRuntimeOptions {
            read_parallelism: {
                let value = object
                    .get("read_parallelism")
                    .map(|value| positive_usize(value, "config.driver.storage.read_parallelism"))
                    .transpose()?
                    .unwrap_or(1);
                if value != 1 {
                    return Err(ConfigError::at(
                        "config.driver.storage.read_parallelism",
                        "must be 1; parallel block reads are not supported",
                    ));
                }
                value
            },
            lease_ttl,
            atime_policy,
            read_lease_cache_window,
            block_cache,
        }),
        metadata,
        blocks,
        chunk_size_bytes,
        lease_ttl_ms,
        concurrent_writes,
        inode_updates,
        writeback,
        delegated,
        checkout_path,
        owner,
    })
}

fn parse_provider(
    value: &Value,
    path: &str,
    base_dir: &Path,
    block_role: bool,
) -> Result<StorageProvider, ConfigError> {
    let object = object(value, path)?;
    let kind = required_string(object, "kind", path)?;
    let provider = match kind {
        "memory" => {
            reject_unknown(object, &["kind"], path)?;
            StorageProvider::Memory
        }
        "local" => {
            if !block_role {
                return Err(ConfigError::at(path, "provider 'local' is block-only"));
            }
            reject_unknown(object, &["kind", "path"], path)?;
            StorageProvider::Local {
                path: required_path(object, "path", path, base_dir)?,
            }
        }
        "sqlite" => {
            reject_unknown(object, &["kind", "path"], path)?;
            StorageProvider::Sqlite {
                path: required_path(object, "path", path, base_dir)?,
            }
        }
        "pglite" => {
            reject_unknown(
                object,
                &["kind", "connection", "volume_key", "durable"],
                path,
            )?;
            StorageProvider::Pglite {
                connection: required_env_reference(
                    object,
                    "connection",
                    &format!("{path}.connection"),
                )?,
                volume_key: optional_nonempty_string(object, "volume_key", path)?
                    .unwrap_or_else(|| "mount-rs".to_owned()),
                durable: object
                    .get("durable")
                    .map(|value| required_value_bool(value, &format!("{path}.durable")))
                    .transpose()?
                    .unwrap_or(false),
            }
        }
        "tidb" => {
            reject_unknown(
                object,
                &["kind", "connection", "volume_key", "durable"],
                path,
            )?;
            StorageProvider::Tidb {
                connection: required_env_reference(
                    object,
                    "connection",
                    &format!("{path}.connection"),
                )?,
                volume_key: optional_nonempty_string(object, "volume_key", path)?
                    .unwrap_or_else(|| "mount-rs".to_owned()),
                durable: object
                    .get("durable")
                    .map(|value| required_value_bool(value, &format!("{path}.durable")))
                    .transpose()?
                    .unwrap_or(false),
            }
        }
        "foundationdb" => {
            reject_unknown(
                object,
                &[
                    "kind",
                    "cluster_file",
                    "volume_key",
                    "durable",
                    "lease_authority",
                    "authority_prefix",
                ],
                path,
            )?;
            let lease_authority = required_nonempty_string(object, "lease_authority", path)?;
            let lease_authority = match lease_authority.as_str() {
                "persisted-single-authority" => {
                    if object.get("authority_prefix").is_some() {
                        return Err(ConfigError::at(
                            &format!("{path}.authority_prefix"),
                            "is only valid with lease_authority 'shared-provider'",
                        ));
                    }
                    mount_rs_sdk::FoundationDbLeaseAuthority::PersistedSingleAuthority
                }
                "shared-provider" => mount_rs_sdk::FoundationDbLeaseAuthority::SharedProvider {
                    authority_prefix: required_nonempty_string(object, "authority_prefix", path)?,
                },
                "revision-cas" => {
                    if object.get("authority_prefix").is_some() {
                        return Err(ConfigError::at(
                            &format!("{path}.authority_prefix"),
                            "is only valid with lease_authority 'shared-provider'",
                        ));
                    }
                    mount_rs_sdk::FoundationDbLeaseAuthority::RevisionCas
                }
                _ => {
                    return Err(ConfigError::at(
                        &format!("{path}.lease_authority"),
                        "expected 'persisted-single-authority', 'shared-provider', or 'revision-cas'",
                    ));
                }
            };
            StorageProvider::FoundationDb {
                cluster_file: required_path(object, "cluster_file", path, base_dir)?,
                volume_key: optional_nonempty_string(object, "volume_key", path)?
                    .unwrap_or_else(|| "mount-rs".to_owned()),
                durable: object
                    .get("durable")
                    .map(|value| required_value_bool(value, &format!("{path}.durable")))
                    .transpose()?
                    .unwrap_or(false),
                lease_authority,
            }
        }
        "r2" => {
            if !block_role {
                return Err(ConfigError::at(
                    path,
                    "provider 'r2' is block-only; metadata R2 is unsupported by mount-rs",
                ));
            }
            reject_unknown(
                object,
                &[
                    "kind",
                    "endpoint",
                    "bucket",
                    "prefix",
                    "access_key_id",
                    "secret_access_key",
                    "durable",
                ],
                path,
            )?;
            let endpoint = required_nonempty_string(object, "endpoint", path)?;
            validate_r2_endpoint(&endpoint, &format!("{path}.endpoint"))?;
            StorageProvider::R2 {
                endpoint,
                bucket: required_nonempty_string(object, "bucket", path)?,
                prefix: required_nonempty_string(object, "prefix", path)?,
                access_key_id: required_env_reference(
                    object,
                    "access_key_id",
                    &format!("{path}.access_key_id"),
                )?,
                secret_access_key: required_env_reference(
                    object,
                    "secret_access_key",
                    &format!("{path}.secret_access_key"),
                )?,
                durable: object
                    .get("durable")
                    .map(|value| required_value_bool(value, &format!("{path}.durable")))
                    .transpose()?
                    .unwrap_or(true),
            }
        }
        "rustfs" => {
            if !block_role {
                return Err(ConfigError::at(
                    path,
                    "provider 'rustfs' is block-only; metadata RustFS is unsupported by mount-rs",
                ));
            }
            reject_unknown(
                object,
                &[
                    "kind",
                    "endpoint",
                    "bucket",
                    "region",
                    "prefix",
                    "access_key_id",
                    "secret_access_key",
                    "durable",
                ],
                path,
            )?;
            let endpoint = required_nonempty_string(object, "endpoint", path)?;
            validate_rustfs_endpoint(&endpoint, &format!("{path}.endpoint"))?;
            StorageProvider::RustFs {
                endpoint,
                bucket: required_nonempty_string(object, "bucket", path)?,
                region: required_nonempty_string(object, "region", path)?,
                prefix: required_nonempty_string(object, "prefix", path)?,
                access_key_id: required_env_reference(
                    object,
                    "access_key_id",
                    &format!("{path}.access_key_id"),
                )?,
                secret_access_key: required_env_reference(
                    object,
                    "secret_access_key",
                    &format!("{path}.secret_access_key"),
                )?,
                durable: object
                    .get("durable")
                    .map(|value| required_value_bool(value, &format!("{path}.durable")))
                    .transpose()?
                    .unwrap_or(false),
            }
        }
        "aws-s3" => {
            if !block_role {
                return Err(ConfigError::at(
                    path,
                    "provider 'aws-s3' is block-only; metadata AWS S3 is unsupported by mount-rs",
                ));
            }
            reject_unknown(
                object,
                &["kind", "bucket", "region", "prefix", "durable"],
                path,
            )?;
            StorageProvider::AwsS3 {
                bucket: required_nonempty_string(object, "bucket", path)?,
                region: required_nonempty_string(object, "region", path)?,
                prefix: required_nonempty_string(object, "prefix", path)?,
                durable: object
                    .get("durable")
                    .map(|value| required_value_bool(value, &format!("{path}.durable")))
                    .transpose()?
                    .unwrap_or(true),
            }
        }
        _ => {
            return Err(ConfigError::at(
                &format!("{path}.kind"),
                format!("unknown provider '{kind}'"),
            ));
        }
    };
    Ok(provider)
}

fn validate_spec(spec: &ConfigSpec) -> Result<ConfigSpec, ConfigError> {
    if let Some(owner) = spec
        .storage
        .as_ref()
        .and_then(|storage| storage.owner.as_ref())
    {
        validate_owner(owner)?;
    }
    if spec.storage.is_some()
        && (spec.driver != Some(DriverChoice::SplitStore)
            || spec.database.is_some()
            || spec.blocks.is_some()
            || spec.root.is_some())
    {
        return Err(ConfigError::at(
            "config.driver.storage",
            "structured storage is only valid for splitstore and cannot be mixed with legacy paths",
        ));
    }
    if spec.root_uid.is_some() != spec.root_gid.is_some() {
        return Err(ConfigError::at(
            "config.driver",
            "uid and gid must be supplied together",
        ));
    }
    if (spec.root_uid.is_some() || spec.root_gid.is_some())
        && spec.driver != Some(DriverChoice::Sqlite)
    {
        return Err(ConfigError::at(
            "config.driver",
            "uid and gid are only valid for the sqlite driver",
        ));
    }
    Ok(spec.clone())
}

fn has_native_fields(spec: &ConfigSpec) -> bool {
    spec.mountpoint.is_some()
        || spec.transport.is_some()
        || spec.quiet.is_some()
        || spec.verbose.is_some()
        || spec.read_only.is_some()
        || spec.empty.is_some()
        || spec.allow_other.is_some()
        || spec.fuse_uncached.is_some()
        || spec.sqlite_single_host.is_some()
        || spec.driver.is_some()
        || spec.root.is_some()
        || spec.database.is_some()
        || spec.blocks.is_some()
        || spec.root_uid.is_some()
        || spec.root_gid.is_some()
        || spec.storage.is_some()
}

fn apply_explicit_overrides(resolved: &mut CliOptions, raw: &CliOptions) {
    let overrides = &raw.overrides;
    // Extra mountpoints exist only on the CLI; a config describes one primary
    // view and the storage backing shared by every view in this process.
    resolved.also_mountpoints = raw.also_mountpoints.clone();
    if overrides.mountpoint {
        resolved.mountpoint = raw.mountpoint.clone();
    }
    if overrides.transport {
        resolved.transport = raw.transport;
    }
    if overrides.quiet {
        resolved.quiet = raw.quiet;
    }
    if overrides.verbose {
        resolved.verbose = raw.verbose;
    }
    if overrides.read_only {
        resolved.read_only = raw.read_only;
    }
    if overrides.empty {
        resolved.empty = raw.empty;
    }
    if overrides.fuse_uncached {
        resolved.fuse_uncached = raw.fuse_uncached;
    }
    if overrides.allow_other {
        resolved.allow_other = raw.allow_other;
    }
    if overrides.sqlite_single_host {
        resolved.sqlite_single_host = raw.sqlite_single_host;
    }
    if overrides.driver {
        resolved.driver = raw.driver;
    }
    if overrides.root {
        resolved.root = raw.root.clone();
    }
    if overrides.database {
        resolved.database = raw.database.clone();
    }
    if overrides.blocks {
        resolved.blocks = raw.blocks.clone();
    }
}

pub(crate) fn validate_resolved_options(options: &CliOptions) -> Result<(), ConfigError> {
    if (options.driver == DriverChoice::Remote) != options.remote.is_some() {
        return Err(ConfigError::new(
            "remote driver requires remote configuration and cannot be overridden with another driver",
        ));
    }
    if options.remote.is_some()
        && (options.root.is_some()
            || options.database.is_some()
            || options.blocks.is_some()
            || options.storage.is_some()
            || options.sqlite_single_host)
    {
        return Err(ConfigError::new(
            "remote driver cannot include local storage or SQLite settings",
        ));
    }
    if options
        .storage
        .as_ref()
        .is_some_and(|storage| storage.delegated)
        && (matches!(
            options.transport,
            TransportChoice::Nfs | TransportChoice::P9
        ) || !options.also_mountpoints.is_empty()
            || options.sqlite_single_host)
    {
        return Err(ConfigError::new(
            "shared directory ownership native mounts require FUSE; NFS, 9p, multiple mountpoints, and the NFS SQLite profile are not qualified for cache handoff",
        ));
    }
    match options.driver {
        DriverChoice::Memory
            if options.root.is_some()
                || options.database.is_some()
                || options.blocks.is_some()
                || options.root_uid.is_some()
                || options.root_gid.is_some()
                || options.storage.is_some() =>
        {
            Err(ConfigError::new(
                "--root, --database, --blocks, and structured storage require --driver host, sqlite, or splitstore",
            ))
        }
        DriverChoice::Host
            if options.database.is_some()
                || options.blocks.is_some()
                || options.root_uid.is_some()
                || options.root_gid.is_some()
                || options.storage.is_some() =>
        {
            Err(ConfigError::new(
                "--database, --blocks, and structured storage require --driver sqlite or splitstore",
            ))
        }
        DriverChoice::Sqlite
            if options.database.is_none()
                || options.root.is_some()
                || options.blocks.is_some()
                || options.storage.is_some() =>
        {
            Err(ConfigError::new(
                "sqlite requires database and does not accept root, blocks, or structured storage",
            ))
        }
        DriverChoice::SplitStore
            if options.root.is_some()
                || (options.storage.is_none()
                    && (options.database.is_some() != options.blocks.is_some()))
                || (options.storage.is_some()
                    && (options.database.is_some() || options.blocks.is_some())) =>
        {
            if options.root.is_some() {
                Err(ConfigError::new("--root is only valid for --driver host"))
            } else if options.storage.is_some() {
                Err(ConfigError::new(
                    "structured splitstore storage cannot be mixed with legacy database/blocks paths",
                ))
            } else {
                Err(ConfigError::new(
                    "splitstore needs both database and blocks, or neither for volatile storage",
                ))
            }
        }
        _ => Ok(()),
    }?;

    if options.root_uid.is_some() != options.root_gid.is_some() {
        return Err(ConfigError::new(
            "sqlite root ownership requires both uid and gid",
        ));
    }
    if (options.root_uid.is_some() || options.root_gid.is_some())
        && options.driver != DriverChoice::Sqlite
    {
        return Err(ConfigError::new(
            "sqlite root uid/gid are only valid with --driver sqlite",
        ));
    }

    if options.fuse_uncached && options.transport != TransportChoice::Fuse {
        return Err(ConfigError::new(
            "--fuse-uncached requires --transport fuse",
        ));
    }

    if options.sqlite_single_host
        && matches!(
            options.transport,
            TransportChoice::Fuse | TransportChoice::P9
        )
    {
        return Err(ConfigError::new(
            "--sqlite-single-host is only valid with --transport nfs or auto",
        ));
    }
    if options.sqlite_single_host
        && (!options.also_mountpoints.is_empty()
            || options
                .storage
                .as_ref()
                .is_some_and(|storage| storage.concurrent_writes))
    {
        return Err(ConfigError::new(
            "--sqlite-single-host cannot be combined with shared NFS mounts",
        ));
    }
    Ok(())
}

impl ConfigSpec {
    pub(crate) fn to_options(&self) -> CliOptions {
        CliOptions {
            mountpoint: self.mountpoint.clone(),
            also_mountpoints: Vec::new(),
            transport: self.transport.unwrap_or(TransportChoice::Auto),
            quiet: self.quiet.unwrap_or(false),
            verbose: self.verbose.unwrap_or(false),
            read_only: self.read_only.unwrap_or(false),
            empty: self.empty.unwrap_or(false),
            allow_other: self.allow_other.unwrap_or(false),
            fuse_uncached: self.fuse_uncached.unwrap_or(false),
            sqlite_single_host: self.sqlite_single_host.unwrap_or(false),
            driver: self.driver.unwrap_or(DriverChoice::Memory),
            root: self.root.clone(),
            database: self.database.clone(),
            blocks: self.blocks.clone(),
            root_uid: self.root_uid,
            root_gid: self.root_gid,
            config: None,
            storage: self.storage.clone().map(Box::new),
            remote: self.remote.clone().map(Box::new),
            overrides: CliOverrides::default(),
        }
    }
}

fn object<'a>(value: &'a Value, path: &str) -> Result<&'a Map<String, Value>, ConfigError> {
    value
        .as_object()
        .ok_or_else(|| ConfigError::at(path, "must be an object"))
}

fn reject_unknown(
    object: &Map<String, Value>,
    allowed: &[&str],
    path: &str,
) -> Result<(), ConfigError> {
    if let Some(unknown) = object
        .keys()
        .find(|key| !allowed.iter().any(|allowed| allowed == key))
    {
        return Err(ConfigError::at(
            &format!("{path}.{unknown}"),
            "unknown field",
        ));
    }
    Ok(())
}

fn required_u64(object: &Map<String, Value>, key: &str, path: &str) -> Result<u64, ConfigError> {
    object
        .get(key)
        .ok_or_else(|| ConfigError::at(&format!("{path}.{key}"), "is required"))?
        .as_u64()
        .ok_or_else(|| ConfigError::at(&format!("{path}.{key}"), "must be an unsigned integer"))
}

fn optional_u32(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<u32>, ConfigError> {
    object
        .get(key)
        .map(|value| {
            let value = value.as_u64().ok_or_else(|| {
                ConfigError::at(
                    &format!("{path}.{key}"),
                    "must be an unsigned integer in the u32 range",
                )
            })?;
            u32::try_from(value).map_err(|_| {
                ConfigError::at(
                    &format!("{path}.{key}"),
                    "must be an unsigned integer in the u32 range",
                )
            })
        })
        .transpose()
}

fn required_string<'a>(
    object: &'a Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<&'a str, ConfigError> {
    object
        .get(key)
        .ok_or_else(|| ConfigError::at(&format!("{path}.{key}"), "is required"))?
        .as_str()
        .ok_or_else(|| ConfigError::at(&format!("{path}.{key}"), "must be a string"))
}

fn required_value_string(value: &Value, path: &str) -> Result<String, ConfigError> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| ConfigError::at(path, "must be a string"))
}

fn required_nonempty_string(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<String, ConfigError> {
    let value = required_string(object, key, path)?;
    if value.is_empty() {
        return Err(ConfigError::at(
            &format!("{path}.{key}"),
            "must not be empty",
        ));
    }
    Ok(value.to_owned())
}

fn validate_r2_endpoint(endpoint: &str, path: &str) -> Result<(), ConfigError> {
    let Some((scheme, remainder)) = endpoint.split_once("://") else {
        return Err(ConfigError::at(
            path,
            "must use an http:// or https:// endpoint",
        ));
    };
    let authority = remainder.split('/').next().unwrap_or_default();
    if !matches!(scheme, "http" | "https")
        || endpoint.chars().any(|character| {
            character == '\0'
                || character.is_ascii_whitespace()
                || character == '?'
                || character == '#'
        })
        || authority.is_empty()
        || authority.contains('@')
        || remainder.starts_with('@')
    {
        return Err(ConfigError::at(
            path,
            "must be an absolute HTTP(S) endpoint without credentials, query, or fragment",
        ));
    }
    if scheme == "http" && !is_local_http_authority(authority) {
        return Err(ConfigError::at(
            path,
            "HTTP is only allowed for loopback or Docker test gateways; use HTTPS for remote services",
        ));
    }
    Ok(())
}

fn validate_rustfs_endpoint(endpoint: &str, path: &str) -> Result<(), ConfigError> {
    validate_r2_endpoint(endpoint, path)?;
    let (_, remainder) = endpoint.split_once("://").unwrap_or_default();
    let authority = remainder.split('/').next().unwrap_or_default();
    let suffix = remainder.strip_prefix(authority).unwrap_or_default();
    if (!suffix.is_empty() && suffix != "/") || !rustfs_valid_authority(authority) {
        return Err(ConfigError::at(
            path,
            "RustFS endpoint must be an HTTP(S) authority with optional trailing slash; paths, credentials, query, and fragment are forbidden",
        ));
    }
    Ok(())
}

fn rustfs_debug_endpoint_authority(endpoint: &str) -> String {
    let Some((scheme, remainder)) = endpoint.split_once("://") else {
        return "<redacted>".to_owned();
    };
    let authority = remainder.split('/').next().unwrap_or_default();
    if !matches!(scheme, "http" | "https")
        || endpoint.contains('?')
        || endpoint.contains('#')
        || !rustfs_valid_authority(authority)
    {
        return "<redacted>".to_owned();
    }
    format!("{scheme}://{authority}")
}

fn rustfs_valid_authority(authority: &str) -> bool {
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

fn sqlite_durable_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    !value.is_empty() && value != ":memory:" && !value.starts_with("file:")
}

fn is_local_http_authority(authority: &str) -> bool {
    let host = if let Some(bracketed) = authority.strip_prefix('[') {
        bracketed
            .split_once(']')
            .map(|(host, _)| host)
            .unwrap_or_default()
    } else {
        authority
            .rsplit_once(':')
            .map(|(host, port)| {
                if port.chars().all(|character| character.is_ascii_digit()) {
                    host
                } else {
                    authority
                }
            })
            .unwrap_or(authority)
    };
    matches!(
        host,
        "127.0.0.1" | "localhost" | "::1" | "host.docker.internal" | "mount-rs-rustfs"
    )
}

fn optional_nonempty_string(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<String>, ConfigError> {
    object
        .get(key)
        .map(|value| {
            let value = required_value_string(value, &format!("{path}.{key}"))?;
            if value.is_empty() {
                return Err(ConfigError::at(
                    &format!("{path}.{key}"),
                    "must not be empty",
                ));
            }
            Ok(value)
        })
        .transpose()
}

fn required_value_bool(value: &Value, path: &str) -> Result<bool, ConfigError> {
    value
        .as_bool()
        .ok_or_else(|| ConfigError::at(path, "must be a boolean"))
}

fn optional_bool(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<bool>, ConfigError> {
    object
        .get(key)
        .map(|value| required_value_bool(value, &format!("{path}.{key}")))
        .transpose()
}

fn optional_transport(
    object: &Map<String, Value>,
    key: &str,
) -> Result<Option<TransportChoice>, ConfigError> {
    object
        .get(key)
        .map(|value| {
            let value = required_value_string(value, &format!("config.{key}"))?;
            TransportChoice::parse(&value)
                .map_err(|error| ConfigError::at(&format!("config.{key}"), error.message()))
        })
        .transpose()
}

fn optional_path(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
    base_dir: &Path,
) -> Result<Option<PathBuf>, ConfigError> {
    object
        .get(key)
        .map(|value| parse_path(value, &format!("{path}.{key}"), base_dir))
        .transpose()
}

fn required_path(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
    base_dir: &Path,
) -> Result<PathBuf, ConfigError> {
    object
        .get(key)
        .ok_or_else(|| ConfigError::at(&format!("{path}.{key}"), "is required"))
        .and_then(|value| parse_path(value, &format!("{path}.{key}"), base_dir))
}

fn parse_path(value: &Value, path: &str, base_dir: &Path) -> Result<PathBuf, ConfigError> {
    let value = required_value_string(value, path)?;
    if value.is_empty() {
        return Err(ConfigError::at(path, "must not be empty"));
    }
    let path_value = PathBuf::from(value);
    Ok(if path_value.is_absolute() {
        path_value
    } else {
        base_dir.join(path_value)
    })
}

fn positive_usize(value: &Value, path: &str) -> Result<usize, ConfigError> {
    let value = value
        .as_u64()
        .ok_or_else(|| ConfigError::at(path, "must be a positive unsigned integer"))?;
    let value = usize::try_from(value)
        .map_err(|_| ConfigError::at(path, "does not fit this platform's usize"))?;
    if value == 0 {
        return Err(ConfigError::at(path, "must be greater than zero"));
    }
    Ok(value)
}

fn positive_u64(value: &Value, path: &str) -> Result<u64, ConfigError> {
    let value = value
        .as_u64()
        .ok_or_else(|| ConfigError::at(path, "must be a positive unsigned integer"))?;
    if value == 0 {
        return Err(ConfigError::at(path, "must be greater than zero"));
    }
    Ok(value)
}

fn optional_u16(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<Option<u16>, ConfigError> {
    object
        .get(key)
        .map(|value| {
            let value = value.as_u64().ok_or_else(|| {
                ConfigError::at(
                    &format!("{path}.{key}"),
                    "must be an unsigned integer in the u16 range",
                )
            })?;
            u16::try_from(value).map_err(|_| {
                ConfigError::at(
                    &format!("{path}.{key}"),
                    "must be an unsigned integer in the u16 range",
                )
            })
        })
        .transpose()
}

fn required_env_reference(
    fields: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<EnvReference, ConfigError> {
    let value = fields
        .get(key)
        .ok_or_else(|| ConfigError::at(path, "is required"))?;
    let value = object(value, path)?;
    reject_unknown(value, &["env"], path)?;
    let name = required_string(value, "env", path)?.to_owned();
    if !valid_env_name(&name) {
        return Err(ConfigError::at(
            &format!("{path}.env"),
            "must be a valid environment variable name",
        ));
    }
    Ok(EnvReference { name })
}

fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|character| character == '_' || character.is_ascii_alphanumeric())
}

pub fn validate_owner(owner: &str) -> Result<(), ConfigError> {
    if owner.is_empty() || owner.len() > 128 {
        return Err(ConfigError::at(
            "config.driver.storage.owner",
            "must contain 1..=128 bytes",
        ));
    }
    if owner.chars().any(|character| {
        character.is_control() || !(character.is_ascii_alphanumeric() || ".-_".contains(character))
    }) {
        return Err(ConfigError::at(
            "config.driver.storage.owner",
            "may contain only ASCII letters, digits, '.', '_' and '-'",
        ));
    }
    Ok(())
}

/// Redact values commonly carried by URLs and provider diagnostics before
/// they reach stderr. The CLI never prints resolved environment values.
pub fn redact_diagnostic(input: &str) -> String {
    let mut output = input.to_owned();
    redact_url_userinfo(&mut output);
    for key in [
        "password",
        "token",
        "secret_access_key",
        "access_key_id",
        "access_key",
        "signature",
        "authorization",
        "secret",
    ] {
        redact_assignment_values(&mut output, key);
    }
    output
}

fn redact_url_userinfo(value: &mut String) {
    let mut search = 0;
    while let Some(relative) = value[search..].find("://") {
        let scheme_end = search + relative + 3;
        let end = value[scheme_end..]
            .find(|character: char| character.is_whitespace() || character == ')')
            .map(|offset| scheme_end + offset)
            .unwrap_or(value.len());
        let Some(at_offset) = value[scheme_end..end].find('@') else {
            search = scheme_end;
            continue;
        };
        let at = scheme_end + at_offset;
        let Some(colon_offset) = value[scheme_end..at].find(':') else {
            search = at + 1;
            continue;
        };
        let colon = scheme_end + colon_offset;
        value.replace_range(colon + 1..at, "[REDACTED]");
        search = colon + 1 + "[REDACTED]".len();
    }
}

fn redact_assignment_values(value: &mut String, key: &str) {
    let mut search = 0;
    while let Some(relative) = value[search..].to_ascii_lowercase().find(key) {
        let start = search + relative;
        let lower = value.to_ascii_lowercase();
        let before_ok = start == 0
            || (!lower.as_bytes()[start - 1].is_ascii_alphanumeric()
                && lower.as_bytes()[start - 1] != b'_');
        let after_key = start + key.len();
        if !before_ok || !lower[after_key..].starts_with('=') {
            search = after_key.min(lower.len());
            continue;
        }
        let value_start = after_key + 1;
        let value_end = value[value_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '&' | ',' | ')' | ';')
            })
            .map(|offset| value_start + offset)
            .unwrap_or(value.len());
        value.replace_range(value_start..value_end, "[REDACTED]");
        search = (value_start + "[REDACTED]".len()).min(value.len());
        if search >= value.len() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::{Command, parse_args};

    const SPLIT_MEMORY: &str = r#"{
        "version": 1,
        "driver": {
            "kind": "splitstore",
            "storage": {
                "metadata": {"kind": "memory"},
                "blocks": {"kind": "memory"},
                "chunk_size_bytes": 4096,
                "owner": "test-owner"
            }
        }
    }"#;

    #[test]
    fn inode_updates_are_explicit_and_validate_ownership() {
        let driver = serde_json::json!({
            "version": 1,
            "driver": { "kind": "splitstore", "storage": {
                "metadata": { "kind": "sqlite", "path": "metadata.db" },
                "blocks": { "kind": "sqlite", "path": "blocks.db" }
            }}
        });
        let parse = |value: &Value| parse_config_str(&value.to_string(), Path::new("/tmp/config"));
        assert!(!parse(&driver).unwrap().storage.unwrap().inode_updates);
        let mut enabled = driver.clone();
        enabled["driver"]["storage"]["inode_updates"] = Value::Bool(true);
        let storage = parse(&enabled).unwrap().storage.unwrap();
        assert!(storage.inode_updates);
        assert!(storage.concurrent_writes);
        for (field, value) in [
            ("concurrent_writes", Value::Bool(false)),
            ("ownership_mode", Value::String("exclusive".into())),
            ("ownership_mode", Value::String("shared".into())),
        ] {
            let mut invalid = enabled.clone();
            invalid["driver"]["storage"][field] = value;
            assert!(
                parse(&invalid)
                    .unwrap_err()
                    .message()
                    .contains("inode_updates")
            );
        }
    }

    #[test]
    fn split_defaults_to_noatime_with_explicit_strict_override() {
        let config = parse_config_str(SPLIT_MEMORY, Path::new(".")).unwrap();
        assert_eq!(
            config.storage.unwrap().runtime.unwrap().atime_policy,
            AtimePolicy::NoAtime
        );
        let mut source: serde_json::Value = serde_json::from_str(SPLIT_MEMORY).unwrap();
        source["driver"]["storage"]["atime"] = serde_json::json!("strict");
        let config = parse_config_str(&source.to_string(), Path::new(".")).unwrap();
        assert_eq!(
            config.storage.unwrap().runtime.unwrap().atime_policy,
            AtimePolicy::Strict
        );
    }

    #[test]
    fn tidb_cache_config_is_static_and_strictly_validated() {
        let source = serde_json::json!({"version": 1, "driver": {"kind": "splitstore", "storage": {
            "metadata": {"kind": "tidb", "connection": {"env": "MOUNT_RS_TIDB_URL"}, "volume_key": "audit", "durable": true},
            "blocks": {"kind": "memory"}, "atime": "noatime",
            "block_cache": {"max_bytes": 1048576, "max_entries": 128}
        }}});
        let config = parse_config_str(&source.to_string(), Path::new(".")).unwrap();
        let storage = config.storage.unwrap();
        assert!(matches!(
            storage.metadata,
            StorageProvider::Tidb { durable: true, .. }
        ));
        let mut normalized = source.clone();
        normalized["driver"]["storage"]["metadata"]["normalized"] = serde_json::json!(true);
        assert!(parse_config_str(&normalized.to_string(), Path::new(".")).is_err());
        assert_eq!(storage.runtime.unwrap().atime_policy, AtimePolicy::NoAtime);
        assert_eq!(
            storage.runtime.unwrap().block_cache.unwrap().max_entries,
            128
        );
        let mut invalid = source.clone();
        invalid["driver"]["storage"]["block_cache"]["max_bytes"] = serde_json::json!(0);
        assert!(parse_config_str(&invalid.to_string(), Path::new(".")).is_err());
        invalid = source.clone();
        invalid["driver"]["storage"]["atime"] = serde_json::json!("lazy");
        assert!(parse_config_str(&invalid.to_string(), Path::new(".")).is_err());
        invalid = source;
        invalid["driver"]["storage"]["metadata"]["connection"] =
            serde_json::json!("mysql://secret");
        let error = parse_config_str(&invalid.to_string(), Path::new(".")).unwrap_err();
        assert!(!error.to_string().contains("mysql://secret"));
    }

    #[test]
    fn disk_cache_has_explicit_scope_bounds_and_config_relative_root() {
        let mut source: Value = serde_json::from_str(SPLIT_MEMORY).unwrap();
        source["driver"]["storage"]["disk_cache"] = serde_json::json!({"root":"cache","scope":"volume-generation","max_bytes":4096,"max_entries":8});
        let parsed = parse_config_str(&source.to_string(), Path::new("/config-root")).unwrap();
        let disk = parsed.storage.unwrap().disk_cache.unwrap();
        assert_eq!(disk.root, Path::new("/config-root/cache"));
        assert_eq!(disk.scope, "volume-generation");
        for (field, value) in [
            ("max_bytes", serde_json::json!(79)),
            ("max_entries", serde_json::json!(0)),
            ("scope", serde_json::json!("")),
            ("extra", serde_json::json!(true)),
        ] {
            let mut invalid = source.clone();
            invalid["driver"]["storage"]["disk_cache"][field] = value;
            assert!(parse_config_str(&invalid.to_string(), Path::new(".")).is_err());
        }
    }

    #[test]
    fn owner_serving_window_has_explicit_safe_bounds() {
        let source: Value = serde_json::from_str(SPLIT_MEMORY).unwrap();
        let runtime = parse_config_str(&source.to_string(), Path::new("."))
            .unwrap()
            .storage
            .unwrap()
            .runtime
            .unwrap();
        assert_eq!(runtime.lease_ttl, std::time::Duration::from_secs(240));
        assert_eq!(runtime.read_parallelism, 1);
        assert_eq!(
            runtime.read_lease_cache_window,
            std::time::Duration::from_secs(60)
        );
        for ms in [0, 20, 5000, 7500, 60000] {
            let mut config = source.clone();
            config["driver"]["storage"]["read_lease_cache_window_ms"] = serde_json::json!(ms);
            assert_eq!(
                parse_config_str(&config.to_string(), Path::new("."))
                    .unwrap()
                    .storage
                    .unwrap()
                    .runtime
                    .unwrap()
                    .read_lease_cache_window,
                std::time::Duration::from_millis(ms)
            );
        }
        for (ttl_ms, window_ms, valid) in [
            (240_000, 60_000, true),
            (239_999, 60_000, false),
            (30_000, 7_500, true),
            (30_000, 7_501, false),
            (0, 0, false),
        ] {
            let mut config = source.clone();
            config["driver"]["storage"]["lease_ttl_ms"] = serde_json::json!(ttl_ms);
            config["driver"]["storage"]["read_lease_cache_window_ms"] =
                serde_json::json!(window_ms);
            assert_eq!(
                parse_config_str(&config.to_string(), Path::new(".")).is_ok(),
                valid
            );
        }
        for invalid in [
            serde_json::json!(1),
            serde_json::json!(19),
            serde_json::json!(60001),
            serde_json::json!(-1),
            serde_json::json!(20.5),
            serde_json::json!("5000"),
        ] {
            let mut config = source.clone();
            config["driver"]["storage"]["read_lease_cache_window_ms"] = invalid;
            assert!(parse_config_str(&config.to_string(), Path::new(".")).is_err());
        }
    }

    #[test]
    fn read_parallelism_is_explicit_and_bounded() {
        let source: Value = serde_json::from_str(SPLIT_MEMORY).unwrap();
        for value in [
            serde_json::json!(1),
            serde_json::json!(4),
            serde_json::json!(0),
            serde_json::json!(5),
            serde_json::json!("4"),
        ] {
            let mut config = source.clone();
            config["driver"]["storage"]["read_parallelism"] = value.clone();
            let result = parse_config_str(&config.to_string(), Path::new("."));
            match value.as_u64() {
                Some(1) => assert_eq!(
                    result
                        .unwrap()
                        .storage
                        .unwrap()
                        .runtime
                        .unwrap()
                        .read_parallelism,
                    1
                ),
                _ => assert!(result.is_err()),
            }
        }
    }

    #[test]
    fn structured_splitstore_has_no_legacy_or_host_fields() {
        let error = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "root": "/tmp/irrelevant",
                    "storage": {
                        "metadata": {"kind": "memory"},
                        "blocks": {"kind": "memory"}
                    }
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(error.message().contains("config.driver.root"));
        assert!(error.message().contains("unknown field"));
    }

    #[test]
    fn provider_objects_are_discriminated_and_strict() {
        let error = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "memory",
                    "storage": {}
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(error.message().contains("config.driver.storage"));

        let error = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {"kind": "r2"},
                        "blocks": {"kind": "memory"}
                    }
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(error.message().contains("block-only"));
    }

    #[test]
    fn sqlite_root_owner_is_an_unsigned_pair() {
        let spec = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "sqlite",
                    "database": "state.db",
                    "uid": 501,
                    "gid": 20
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap();
        assert_eq!(spec.root_uid, Some(501));
        assert_eq!(spec.root_gid, Some(20));
        let options = spec.to_options();
        assert_eq!(options.root_uid, Some(501));
        assert_eq!(options.root_gid, Some(20));
    }

    #[test]
    fn sqlite_root_owner_rejects_partial_or_out_of_range_values() {
        let partial = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "sqlite",
                    "database": "state.db",
                    "uid": 501
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap_err();
        assert!(partial.message().contains("uid and gid"));

        let out_of_range = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "sqlite",
                    "database": "state.db",
                    "uid": 4294967296,
                    "gid": 20
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap_err();
        assert!(out_of_range.message().contains("u32 range"));

        let negative = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "sqlite",
                    "database": "state.db",
                    "uid": -1,
                    "gid": 20
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap_err();
        assert!(negative.message().contains("unsigned integer"));
    }

    #[test]
    fn sqlite_root_owner_is_rejected_for_other_drivers() {
        let error = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "memory",
                    "uid": 501,
                    "gid": 20
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap_err();
        assert!(error.message().contains("config.driver."));
        assert!(error.message().contains("unknown field"));
    }

    #[test]
    fn pglite_and_r2_credentials_are_env_references_only() {
        let spec = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {
                            "kind": "pglite",
                            "connection": {"env": "PGLITE_URL"}
                        },
                        "blocks": {
                            "kind": "r2",
                            "endpoint": "https://account.example",
                            "bucket": "bucket",
                            "prefix": "blocks",
                            "access_key_id": {"env": "R2_ACCESS_KEY_ID"},
                            "secret_access_key": {"env": "R2_SECRET_ACCESS_KEY"}
                        }
                    }
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap();
        let Some(SplitStorageConfig {
            metadata: StorageProvider::Pglite { connection, .. },
            blocks:
                StorageProvider::R2 {
                    access_key_id,
                    secret_access_key,
                    ..
                },
            ..
        }) = spec.storage
        else {
            panic!("expected independent PGlite/R2 providers");
        };
        assert_eq!(connection.name, "PGLITE_URL");
        assert_eq!(access_key_id.name, "R2_ACCESS_KEY_ID");
        assert_eq!(secret_access_key.name, "R2_SECRET_ACCESS_KEY");
    }

    #[test]
    fn r2_endpoint_validation_rejects_unsafe_url_shapes() {
        for endpoint in [
            "ftp://account.example",
            "https://user:password@account.example",
            "https://account.example?signature=secret",
            "https://account.example#fragment",
            "https:///missing-authority",
            "http://object-store.example",
        ] {
            let config = format!(
                r#"{{
                    "version": 1,
                    "driver": {{
                        "kind": "splitstore",
                        "storage": {{
                            "metadata": {{"kind": "memory"}},
                            "blocks": {{
                                "kind": "r2",
                                "endpoint": "{endpoint}",
                                "bucket": "mount-rs-tests",
                                "prefix": "blocks",
                                "access_key_id": {{"env": "R2_ACCESS_KEY_ID"}},
                                "secret_access_key": {{"env": "R2_SECRET_ACCESS_KEY"}}
                            }}
                        }}
                    }}
                }}"#
            );
            let error = parse_config_str(&config, Path::new("/tmp/config")).unwrap_err();
            assert!(error.message().contains("endpoint"), "{endpoint}: {error}");
            assert!(!error.message().contains("password"), "{endpoint}: {error}");
        }

        for endpoint in [
            "http://127.0.0.1:9878",
            "http://host.docker.internal:9878",
            "http://mount-rs-rustfs:9000",
        ] {
            let config = format!(
                r#"{{
                    "version": 1,
                    "driver": {{
                        "kind": "splitstore",
                        "storage": {{
                            "metadata": {{"kind": "memory"}},
                            "blocks": {{
                                "kind": "r2",
                                "endpoint": "{endpoint}",
                                "bucket": "mount-rs-tests",
                                "prefix": "blocks",
                                "access_key_id": {{"env": "R2_ACCESS_KEY_ID"}},
                                "secret_access_key": {{"env": "R2_SECRET_ACCESS_KEY"}}
                            }}
                        }}
                    }}
                }}"#
            );
            parse_config_str(&config, Path::new("/tmp/config"))
                .unwrap_or_else(|error| panic!("{endpoint}: {error}"));
        }
    }

    #[test]
    fn aws_s3_provider_uses_region_and_rejects_s3_compatible_fields() {
        let spec = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {"kind": "memory"},
                        "blocks": {
                            "kind": "aws-s3",
                            "bucket": "mount-rs-integration",
                            "region": "ap-southeast-2",
                            "prefix": "mount-rs-tests/aws-s3"
                        }
                    }
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap();
        let Some(SplitStorageConfig {
            blocks:
                StorageProvider::AwsS3 {
                    bucket,
                    region,
                    prefix,
                    durable,
                },
            ..
        }) = spec.storage
        else {
            panic!("expected AWS S3 block provider");
        };
        assert_eq!(bucket, "mount-rs-integration");
        assert_eq!(region, "ap-southeast-2");
        assert_eq!(prefix, "mount-rs-tests/aws-s3");
        assert!(durable);

        let error = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {"kind": "memory"},
                        "blocks": {
                            "kind": "aws-s3",
                            "bucket": "mount-rs-integration",
                            "region": "ap-southeast-2",
                            "prefix": "mount-rs-tests/aws-s3",
                            "endpoint": "https://example.invalid"
                        }
                    }
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(error.message().contains("endpoint"));
        assert!(error.message().contains("unknown field"));

        let error = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {"kind": "aws-s3"},
                        "blocks": {"kind": "memory"}
                    }
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(error.message().contains("block-only"));
    }

    #[test]
    fn tidb_and_rustfs_storage_is_strict_and_uses_env_references() {
        let spec = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {
                            "kind": "tidb",
                            "connection": {"env": "MOUNT_RS_TIDB_URL"},
                            "volume_key": "cli-tidb-rustfs",
                            "durable": true
                        },
                        "blocks": {
                            "kind": "rustfs",
                            "endpoint": "http://127.0.0.1:9878",
                            "bucket": "mount-rs-rustfs",
                            "region": "us-east-1",
                            "prefix": "mount-rs/tidb-rustfs",
                            "access_key_id": {"env": "RUSTFS_ACCESS_KEY_ID"},
                            "secret_access_key": {"env": "RUSTFS_SECRET_ACCESS_KEY"}
                        }
                    }
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap();
        let Some(SplitStorageConfig {
            metadata:
                StorageProvider::Tidb {
                    connection,
                    volume_key,
                    durable,
                },
            blocks: StorageProvider::RustFs { .. },
            ..
        }) = spec.storage
        else {
            panic!("expected TiDB metadata and RustFS blocks");
        };
        assert_eq!(connection.name, "MOUNT_RS_TIDB_URL");
        assert_eq!(volume_key, "cli-tidb-rustfs");
        assert!(durable);
    }

    #[test]
    fn foundationdb_storage_requires_an_explicit_supported_authority_mode() {
        let spec = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {
                            "kind": "foundationdb",
                            "cluster_file": "fdb.cluster",
                            "volume_key": "cli-fdb-rustfs",
                            "lease_authority": "persisted-single-authority"
                        },
                        "blocks": {
                            "kind": "rustfs",
                            "endpoint": "http://127.0.0.1:9878",
                            "bucket": "mount-rs-rustfs",
                            "region": "us-east-1",
                            "prefix": "mount-rs/fdb-rustfs",
                            "access_key_id": {"env": "RUSTFS_ACCESS_KEY_ID"},
                            "secret_access_key": {"env": "RUSTFS_SECRET_ACCESS_KEY"}
                        }
                    }
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap();
        let Some(SplitStorageConfig {
            metadata:
                StorageProvider::FoundationDb {
                    cluster_file,
                    volume_key,
                    lease_authority,
                    ..
                },
            blocks: StorageProvider::RustFs { .. },
            ..
        }) = spec.storage
        else {
            panic!("expected FoundationDB metadata and RustFS blocks");
        };
        assert_eq!(cluster_file, Path::new("/tmp/config/fdb.cluster"));
        assert_eq!(volume_key, "cli-fdb-rustfs");
        assert_eq!(
            lease_authority,
            mount_rs_sdk::FoundationDbLeaseAuthority::PersistedSingleAuthority
        );

        let shared_spec = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {
                            "kind": "foundationdb",
                            "cluster_file": "fdb.cluster",
                            "volume_key": "cli-fdb-rustfs",
                            "lease_authority": "shared-provider",
                            "authority_prefix": "mount-rs/lease-authority"
                        },
                        "blocks": {"kind": "memory"}
                    }
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap();
        let Some(SplitStorageConfig {
            metadata:
                StorageProvider::FoundationDb {
                    lease_authority, ..
                },
            blocks: StorageProvider::Memory,
            ..
        }) = shared_spec.storage
        else {
            panic!("expected shared-provider FoundationDB metadata and memory blocks");
        };
        assert_eq!(
            lease_authority,
            mount_rs_sdk::FoundationDbLeaseAuthority::SharedProvider {
                authority_prefix: "mount-rs/lease-authority".to_owned(),
            }
        );

        let missing_authority = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {
                            "kind": "foundationdb",
                            "cluster_file": "fdb.cluster"
                        },
                        "blocks": {"kind": "memory"}
                    }
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap_err();
        assert!(missing_authority.message().contains("lease_authority"));

        let invalid_authority = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {
                            "kind": "foundationdb",
                            "cluster_file": "fdb.cluster",
                            "lease_authority": "host-clock"
                        },
                        "blocks": {"kind": "memory"}
                    }
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap_err();
        assert!(
            invalid_authority
                .message()
                .contains("persisted-single-authority")
        );
    }

    #[test]
    fn explicit_exclusive_ownership_is_accepted() {
        let mut value: Value = serde_json::from_str(SPLIT_MEMORY).unwrap();
        value["driver"]["storage"]["ownership_mode"] = serde_json::json!("exclusive");
        let spec = parse_config_str(&value.to_string(), Path::new("/tmp")).unwrap();
        assert!(!spec.storage.as_ref().unwrap().concurrent_writes);
        assert!(spec.storage.as_ref().unwrap().writeback);
    }

    #[test]
    fn explicit_shared_requires_a_checkout_path() {
        let mut value: Value = serde_json::from_str(SPLIT_MEMORY).unwrap();
        value["driver"]["storage"]["ownership_mode"] = serde_json::json!("shared");
        let error = parse_config_str(&value.to_string(), Path::new("/tmp")).unwrap_err();
        assert!(error.message().contains("checkout_path"));
    }

    #[test]
    fn shared_checkout_requires_supported_providers_and_rejects_invalid_paths() {
        let mut value: Value = serde_json::from_str(SPLIT_MEMORY).unwrap();
        value["driver"]["storage"]["ownership_mode"] = serde_json::json!("shared");
        value["driver"]["storage"]["checkout_path"] = serde_json::json!("/project");
        assert!(parse_config_str(&value.to_string(), Path::new("/tmp")).is_err());
        value["driver"]["storage"]["metadata"] =
            serde_json::json!({"kind":"sqlite","path":"/tmp/shared-meta.sqlite"});
        value["driver"]["storage"]["blocks"] =
            serde_json::json!({"kind":"sqlite","path":"/tmp/shared-blocks.sqlite"});
        assert!(parse_config_str(&value.to_string(), Path::new("/tmp")).is_ok());
        for invalid in ["", "relative", "/project/../other", "/project//child"] {
            value["driver"]["storage"]["checkout_path"] = serde_json::json!(invalid);
            assert!(parse_config_str(&value.to_string(), Path::new("/tmp")).is_err());
        }
        value["driver"]["storage"]["checkout_path"] = serde_json::json!("/project");
        value["driver"]["storage"]["ownership_mode"] = serde_json::json!("exclusive");
        assert!(parse_config_str(&value.to_string(), Path::new("/tmp")).is_err());
        value["driver"]["storage"]
            .as_object_mut()
            .unwrap()
            .remove("ownership_mode");
        assert!(parse_config_str(&value.to_string(), Path::new("/tmp")).is_err());
    }

    #[test]
    fn ownership_mode_rejects_conflicting_legacy_option() {
        let mut value: Value = serde_json::from_str(SPLIT_MEMORY).unwrap();
        value["driver"]["storage"]["ownership_mode"] = serde_json::json!("exclusive");
        value["driver"]["storage"]["concurrent_writes"] = serde_json::json!(true);
        let error = parse_config_str(&value.to_string(), Path::new("/tmp")).unwrap_err();
        assert!(error.message().contains("conflicts"));
    }

    #[test]
    fn shared_ownership_accepts_supported_providers_and_agreeing_legacy_option() {
        let mut value = serde_json::json!({"version": 1, "driver": {
            "kind": "splitstore", "storage": {
                "ownership_mode": "shared",
                "checkout_path": "/project",
                "metadata": {"kind": "sqlite", "path": "/tmp/meta.sqlite"},
                "blocks": {"kind": "sqlite", "path": "/tmp/blocks.sqlite"}
            }
        }});
        for legacy in [None, Some(true)] {
            if let Some(legacy) = legacy {
                value["driver"]["storage"]["concurrent_writes"] = serde_json::json!(legacy);
            }
            let spec = parse_config_str(&value.to_string(), Path::new("/tmp")).unwrap();
            let storage = spec.storage.unwrap();
            assert!(storage.concurrent_writes);
            assert!(storage.delegated);
            assert_eq!(storage.checkout_path.as_deref(), Some("/project"));
            assert!(!storage.writeback);
        }
    }

    #[test]
    fn ownership_rejects_invalid_names_and_retains_legacy_write_through() {
        let mut value: Value = serde_json::from_str(SPLIT_MEMORY).unwrap();
        let legacy = parse_config_str(&value.to_string(), Path::new("/tmp")).unwrap();
        assert!(!legacy.storage.unwrap().writeback);
        for invalid in [
            serde_json::json!("single-host"),
            serde_json::json!(true),
            serde_json::json!(null),
        ] {
            value["driver"]["storage"]["ownership_mode"] = invalid;
            assert!(parse_config_str(&value.to_string(), Path::new("/tmp")).is_err());
        }
        value["driver"]["storage"]["ownership_mode"] = serde_json::json!("exclusive");
        value["driver"]["storage"]["concurrent_writes"] = serde_json::json!(false);
        assert!(
            parse_config_str(&value.to_string(), Path::new("/tmp"))
                .unwrap()
                .storage
                .unwrap()
                .writeback
        );
    }

    #[test]
    fn static_validation_does_not_require_provider_credentials() {
        let spec = parse_config_str(SPLIT_MEMORY, Path::new("/tmp")).unwrap();
        assert_eq!(spec.storage.as_ref().unwrap().chunk_size_bytes, 4096);
        assert!(validate_resolved_options(&spec.to_options()).is_ok());
    }

    #[test]
    fn local_sqlite_nfs_lock_profile_cannot_be_used_for_shared_views() {
        let options = CliOptions {
            sqlite_single_host: true,
            also_mountpoints: vec![PathBuf::from("/tmp/second-view")],
            ..CliOptions::default()
        };
        let error = validate_resolved_options(&options)
            .expect_err("the local SQLite profile is incompatible with shared views");
        assert!(error.message().contains("shared NFS mounts"));
    }

    #[test]
    fn concurrent_tidb_config_accepts_shared_blocks_and_rejects_local_blocks() {
        let config = |blocks: Value| {
            serde_json::json!({
                "version": 1,
                "driver": {"kind": "splitstore", "storage": {
                    "concurrent_writes": true,
                    "metadata": {"kind": "tidb", "connection": {"env": "MOUNT_RS_TIDB_URL"},
                                 "volume_key": "shared-tidb", "durable": true},
                    "blocks": blocks
                }}
            })
        };
        for blocks in [
            serde_json::json!({"kind": "tidb", "connection": {"env": "MOUNT_RS_TIDB_URL"},
                               "volume_key": "shared-tidb-blocks", "durable": true}),
            serde_json::json!({"kind": "rustfs", "endpoint": "http://127.0.0.1:9000",
                               "bucket": "test", "prefix": "shared-tidb", "region": "us-east-1",
                               "access_key_id": {"env": "RUSTFS_ACCESS_KEY_ID"},
                               "secret_access_key": {"env": "RUSTFS_SECRET_ACCESS_KEY"}}),
        ] {
            let valid = config(blocks);
            parse_config_str(&valid.to_string(), Path::new("/tmp"))
                .expect("TiDB supports independent clients with shared blocks");
            let mut unused_ttl = valid;
            unused_ttl["driver"]["storage"]["lease_ttl_ms"] = serde_json::json!(1000);
            assert!(parse_config_str(&unused_ttl.to_string(), Path::new("/tmp")).is_err());
        }
        for blocks in [
            serde_json::json!({"kind": "memory"}),
            serde_json::json!({"kind": "sqlite", "path": "blocks.sqlite"}),
        ] {
            let error = parse_config_str(&config(blocks).to_string(), Path::new("/tmp"))
                .expect_err("TiDB requires blocks shared across hosts");
            assert!(error.message().contains("shared block"));
        }
    }

    #[test]
    fn concurrent_fdb_config_rejects_blocks_local_to_one_process_or_host() {
        for blocks in [
            serde_json::json!({"kind": "memory"}),
            serde_json::json!({"kind": "sqlite", "path": "local-blocks.sqlite"}),
        ] {
            let config = serde_json::json!({
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "concurrent_writes": true,
                        "metadata": {
                            "kind": "foundationdb",
                            "cluster_file": "/nonexistent/fdb.cluster",
                            "volume_key": "shared-test",
                            "durable": true,
                            "lease_authority": "revision-cas"
                        },
                        "blocks": blocks
                    }
                }
            });
            let error = parse_config_str(&config.to_string(), Path::new("/tmp"))
                .expect_err("process-local blocks must fail static validation");
            assert!(error.message().contains("config.driver.storage.blocks"));
            assert!(error.message().contains("shared block"));
        }
    }

    #[test]
    fn concurrent_sqlite_config_requires_local_durable_metadata_and_blocks() {
        for (metadata, blocks) in [
            (
                serde_json::json!({"kind": "sqlite", "path": ":memory:"}),
                serde_json::json!({"kind": "sqlite", "path": "blocks.sqlite"}),
            ),
            (
                serde_json::json!({"kind": "sqlite", "path": "metadata.sqlite"}),
                serde_json::json!({"kind": "memory"}),
            ),
            (
                serde_json::json!({"kind": "pglite", "connection": {"env": "PGLITE_DATABASE_URL"}}),
                serde_json::json!({"kind": "sqlite", "path": "blocks.sqlite"}),
            ),
        ] {
            let config = serde_json::json!({
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "concurrent_writes": true,
                        "metadata": metadata,
                        "blocks": blocks
                    }
                }
            });
            assert!(parse_config_str(&config.to_string(), Path::new("/tmp")).is_err());
        }
        let valid = serde_json::json!({
            "version": 1,
            "driver": {
                "kind": "splitstore",
                "storage": {
                    "concurrent_writes": true,
                    "metadata": {"kind": "sqlite", "path": "metadata.sqlite"},
                    "blocks": {"kind": "sqlite", "path": "blocks.sqlite"}
                }
            }
        });
        parse_config_str(&valid.to_string(), Path::new("/tmp"))
            .expect("two local SQLite files support same-host concurrent mode");
        let mut legacy = valid;
        legacy["driver"]["storage"]["concurrent_writes"] = serde_json::json!(false);
        legacy["driver"]["storage"]["metadata"]["path"] = serde_json::json!(":memory:");
        parse_config_str(&legacy.to_string(), Path::new("/tmp"))
            .expect("nonconcurrent SQLite path parsing remains compatible");
    }

    #[test]
    fn rustfs_is_named_block_only_provider_with_environment_credentials() {
        let config = serde_json::json!({
            "version": 1,
            "driver": {
                "kind": "splitstore",
                "storage": {
                    "metadata": {"kind": "sqlite", "path": "metadata.sqlite"},
                    "blocks": {
                        "kind": "rustfs",
                        "endpoint": "http://127.0.0.1:9878",
                        "bucket": "mount-rs-test",
                        "region": "us-east-1",
                        "prefix": "mount-rs/test",
                        "access_key_id": {"env": "RUSTFS_ACCESS_KEY_ID"},
                        "secret_access_key": {"env": "RUSTFS_SECRET_ACCESS_KEY"}
                    }
                }
            }
        });
        let spec = parse_config_str(&config.to_string(), Path::new("/tmp"))
            .expect("RustFS block config should parse without opening backend");
        assert!(matches!(
            spec.storage.unwrap().blocks,
            StorageProvider::RustFs { .. }
        ));
        let metadata = serde_json::json!({
            "version": 1,
            "driver": {
                "kind": "splitstore",
                "storage": {
                    "metadata": config["driver"]["storage"]["blocks"],
                    "blocks": {"kind": "memory"}
                }
            }
        });
        let error = parse_config_str(&metadata.to_string(), Path::new("/tmp"))
            .expect_err("RustFS does not publish metadata revision CAS");
        assert!(error.message().contains("block-only"));
    }

    #[test]
    fn rustfs_durability_requires_an_explicit_caller_assertion() {
        let mut config = serde_json::json!({
            "version": 1,
            "driver": {
                "kind": "splitstore",
                "storage": {
                    "metadata": {"kind": "sqlite", "path": "metadata.sqlite"},
                    "blocks": {
                        "kind": "rustfs",
                        "endpoint": "http://127.0.0.1:9878",
                        "bucket": "mount-rs-test",
                        "region": "us-east-1",
                        "prefix": "mount-rs/test",
                        "access_key_id": {"env": "RUSTFS_ACCESS_KEY_ID"},
                        "secret_access_key": {"env": "RUSTFS_SECRET_ACCESS_KEY"}
                    }
                }
            }
        });
        let spec = parse_config_str(&config.to_string(), Path::new("/tmp"))
            .expect("RustFS durability defaults to caller-unasserted");
        assert!(matches!(
            spec.storage.unwrap().blocks,
            StorageProvider::RustFs { durable: false, .. }
        ));
        config["driver"]["storage"]["blocks"]["durable"] = serde_json::json!(true);
        let spec = parse_config_str(&config.to_string(), Path::new("/tmp"))
            .expect("explicit RustFS durability assertion is accepted");
        assert!(matches!(
            spec.storage.unwrap().blocks,
            StorageProvider::RustFs { durable: true, .. }
        ));
    }

    #[test]
    fn rustfs_endpoint_paths_fail_static_validation_without_echoing_them() {
        let mut config = serde_json::json!({
            "version": 1,
            "driver": {
                "kind": "splitstore",
                "storage": {
                    "metadata": {"kind": "sqlite", "path": "metadata.sqlite"},
                    "blocks": {
                        "kind": "rustfs",
                        "endpoint": "https://example.invalid",
                        "bucket": "mount-rs-test",
                        "region": "us-east-1",
                        "prefix": "mount-rs/test",
                        "access_key_id": {"env": "RUSTFS_ACCESS_KEY_ID"},
                        "secret_access_key": {"env": "RUSTFS_SECRET_ACCESS_KEY"}
                    }
                }
            }
        });
        for endpoint in [
            "https://example.invalid/tenant-secret",
            "https://example.invalid//",
            "https://example.invalid/%2F",
            "https://example.invalid:tenant-secret",
            "https://example.invalid:0",
            "https://example.invalid:65536",
        ] {
            config["driver"]["storage"]["blocks"]["endpoint"] = serde_json::json!(endpoint);
            let error = parse_config_str(&config.to_string(), Path::new("/tmp"))
                .expect_err("RustFS endpoint paths must be rejected before provider open");
            assert!(error.message().contains("endpoint"), "{error}");
            assert!(!error.message().contains("tenant-secret"), "{error}");
        }
        config["driver"]["storage"]["blocks"]["endpoint"] =
            serde_json::json!("https://example.invalid/");
        parse_config_str(&config.to_string(), Path::new("/tmp"))
            .expect("one optional trailing slash remains valid");
        config["driver"]["storage"]["blocks"]["endpoint"] = serde_json::json!("https://[::1]:443/");
        parse_config_str(&config.to_string(), Path::new("/tmp"))
            .expect("bracketed IPv6 with a numeric port remains valid");
    }

    #[test]
    fn rustfs_provider_debug_omits_endpoint_path_values() {
        let mut provider = StorageProvider::RustFs {
            endpoint: "https://example.invalid/tenant-secret".to_owned(),
            bucket: "mount-rs-test".to_owned(),
            region: "us-east-1".to_owned(),
            prefix: "mount-rs/test".to_owned(),
            access_key_id: EnvReference {
                name: "RUSTFS_ACCESS_KEY_ID".to_owned(),
            },
            secret_access_key: EnvReference {
                name: "RUSTFS_SECRET_ACCESS_KEY".to_owned(),
            },
            durable: false,
        };
        let debug = format!("{provider:?}");
        assert!(debug.contains("https://example.invalid"));
        assert!(!debug.contains("tenant-secret"), "{debug}");
        if let StorageProvider::RustFs { endpoint, .. } = &mut provider {
            *endpoint = "https://example.invalid:tenant-secret".to_owned();
        }
        let debug = format!("{provider:?}");
        assert!(!debug.contains("tenant-secret"), "{debug}");
    }

    #[test]
    fn explicit_lease_ttl_is_positive_milliseconds() {
        let spec = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {"kind": "memory"},
                        "blocks": {"kind": "memory"},
                        "lease_ttl_ms": 120000
                    }
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap();
        assert_eq!(spec.storage.as_ref().unwrap().lease_ttl_ms, Some(120_000));
        assert_eq!(
            spec.storage
                .as_ref()
                .unwrap()
                .runtime
                .unwrap()
                .read_lease_cache_window,
            std::time::Duration::from_secs(30)
        );

        let error = parse_config_str(
            r#"{
                "version": 1,
                "driver": {
                    "kind": "splitstore",
                    "storage": {
                        "metadata": {"kind": "memory"},
                        "blocks": {"kind": "memory"},
                        "lease_ttl_ms": 0
                    }
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(error.message().contains("lease_ttl_ms"));
        assert!(error.message().contains("greater than zero"));
    }

    #[test]
    fn default_owner_is_unique_and_explicit_owner_is_validated() {
        let first = unique_default_owner();
        let second = unique_default_owner();
        assert_ne!(first, second);
        let first_parts: Vec<_> = first.split('-').collect();
        let second_parts: Vec<_> = second.split('-').collect();
        assert_eq!(first_parts.len(), 5);
        assert_eq!(second_parts.len(), 5);
        assert_eq!(&first_parts[..3], &["mount", "rs", "cli"]);
        assert_eq!(first_parts[3].len(), 32);
        assert_eq!(first_parts[3], second_parts[3]);
        assert!(validate_owner("writer-a_1").is_ok());
        assert!(validate_owner("writer with spaces").is_err());
    }

    #[test]
    fn explicit_cli_flags_override_config_but_defaults_do_not() {
        let path = std::env::temp_dir().join(format!(
            "mount-rs-cli-config-{}-{}.json",
            std::process::id(),
            unique_default_owner()
        ));
        std::fs::write(
            &path,
            r#"{
                "version": 1,
                "transport": "nfs",
                "quiet": true,
                "driver": {"kind": "memory"}
            }"#,
        )
        .unwrap();

        let raw = match parse_args([
            "mount-rs",
            "--config",
            path.to_str().unwrap(),
            "--transport",
            "fuse",
        ])
        .unwrap()
        {
            Command::Mount(options) => options,
            other => panic!("expected mount, got {other:?}"),
        };
        let resolved = resolve_cli_options(raw).unwrap();
        assert_eq!(resolved.transport, TransportChoice::Fuse);
        assert!(resolved.quiet);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn redaction_removes_url_and_query_credentials() {
        let message = redact_diagnostic(
            "connect postgres://user:secret@host/db?password=another&token=third",
        );
        assert!(!message.contains("secret"));
        assert!(!message.contains("another"));
        assert!(!message.contains("third"));
        assert!(message.contains("[REDACTED]"));
    }

    #[test]
    fn aggregate_cache_capacity_reserves_all_drives_before_opening() {
        let drive = |id: &str| {
            serde_json::json!({"id":id,"token":{"env":"TEST_TOKEN"},
            "driver":{"kind":"splitstore","storage":{
                "metadata":{"kind":"memory"},"blocks":{"kind":"memory"},
                "block_cache":{"max_bytes":1024,"max_entries":8},
                "disk_cache":{"root":".","scope":id,"max_bytes":2048,"max_entries":8}
            }}})
        };
        let mut source = serde_json::json!({"version":1,"http":{
            "cache_capacity":{"ram_bytes":2048,"disk_bytes":4096},
            "drives":[drive("a"),drive("b")]
        }});
        assert!(parse_config_str(&source.to_string(), Path::new(".")).is_ok());
        for limits in [
            serde_json::json!({"ram_bytes":2047,"disk_bytes":4096}),
            serde_json::json!({"ram_bytes":2048,"disk_bytes":4095}),
            serde_json::json!({"ram_bytes":2048}),
            serde_json::json!({"ram_bytes":-1,"disk_bytes":4096}),
            serde_json::json!({"ram_bytes":2048,"disk_bytes":4096,"extra":1}),
        ] {
            source["http"]["cache_capacity"] = limits;
            assert!(parse_config_str(&source.to_string(), Path::new(".")).is_err());
        }
        source["http"]["cache_capacity"] =
            serde_json::json!({"ram_bytes":u64::MAX,"disk_bytes":u64::MAX});
        for index in 0..2 {
            source["http"]["drives"][index]["driver"]["storage"]["disk_cache"]["max_bytes"] =
                serde_json::json!(u64::MAX);
        }
        assert!(parse_config_str(&source.to_string(), Path::new(".")).is_err());
    }

    #[test]
    fn ingress_only_config_resolves_peer_files_and_requires_redis_env_reference() {
        let mut source = serde_json::json!({"version":1,"http":{"drives":[],"forwarding":{
            "peers":"peers.json","certificate":"cert.pem","private_key":"key.pem","ca":"ca.pem",
            "bind":"0.0.0.0:0","redis_url":{"env":"MOUNT_REGISTRY"},"namespace":"prod"
        }}});
        let base = Path::new("/etc/mount-rs");
        let config = parse_config_str(&source.to_string(), base)
            .unwrap()
            .http
            .unwrap();
        assert!(config.drives.is_empty());
        assert_eq!(config.forwarding.unwrap().peers, base.join("peers.json"));
        source["http"]["forwarding"]["redis_url"] = serde_json::json!("redis://localhost/");
        assert!(parse_config_str(&source.to_string(), base).is_err());
        source["http"].as_object_mut().unwrap().remove("forwarding");
        assert!(parse_config_str(&source.to_string(), base).is_err());
    }

    #[test]
    fn route_refresh_idle_window_defaults_to_five_ttls_and_is_bounded() {
        let mut source = serde_json::json!({"version":1,"http":{"drives":[],"forwarding":{
            "peers":"peers.json","certificate":"cert.pem","private_key":"key.pem","ca":"ca.pem",
            "bind":"0.0.0.0:0","redis_url":{"env":"MOUNT_REGISTRY"},"namespace":"prod",
            "refresh_active_routes":true
        }}});
        let parse = |source: &serde_json::Value| {
            parse_config_str(&source.to_string(), Path::new(".")).map(|config| {
                config
                    .http
                    .unwrap()
                    .forwarding
                    .unwrap()
                    .route_refresh_idle_ttls
            })
        };
        assert_eq!(parse(&source).unwrap(), 5);
        source["http"]["forwarding"]["route_refresh_idle_ttls"] = 25.into();
        assert_eq!(parse(&source).unwrap(), 25);
        for invalid in [
            serde_json::json!(0),
            serde_json::json!(61),
            serde_json::json!("25"),
        ] {
            source["http"]["forwarding"]["route_refresh_idle_ttls"] = invalid;
            assert!(parse(&source).is_err());
        }
    }

    #[test]
    fn shared_disk_budget_is_explicit_and_bounded() {
        let mut source = serde_json::json!({"version":1,"http":{"drives":[{
            "id":"memory","token":{"env":"TEST_TOKEN"},"driver":{"kind":"memory"}
        }]}});
        assert!(
            parse_config_str(&source.to_string(), Path::new("."))
                .unwrap()
                .http
                .unwrap()
                .disk_cache_budget
                .is_none()
        );
        source["http"]["disk_cache_budget"] = serde_json::json!({"workers":8,"fill_copies":4});
        let budget = parse_config_str(&source.to_string(), Path::new("."))
            .unwrap()
            .http
            .unwrap()
            .disk_cache_budget
            .unwrap();
        assert_eq!((budget.workers, budget.fill_copies), (8, 4));
        for invalid in [
            serde_json::json!({"workers":0,"fill_copies":4}),
            serde_json::json!({"workers":513,"fill_copies":4}),
            serde_json::json!({"workers":8}),
            serde_json::json!({"workers":8,"fill_copies":4,"extra":1}),
        ] {
            source["http"]["disk_cache_budget"] = invalid;
            assert!(parse_config_str(&source.to_string(), Path::new(".")).is_err());
        }
    }

    #[test]
    fn http_upload_budget_and_drain_defaults() {
        let config = |http: serde_json::Value| {
            let mut http = http;
            http["drives"] = serde_json::json!([{
                "id": "memory",
                "token": {"env": "MOUNT_RS_MEMORY_TOKEN"},
                "driver": {"kind": "memory"}
            }]);
            parse_config_str(
                &serde_json::json!({"version": 1, "http": http}).to_string(),
                Path::new("."),
            )
            .map(|spec| spec.http.expect("HTTP config"))
        };

        let defaults = config(serde_json::json!({})).unwrap();
        assert_eq!(defaults.max_request_bytes, 8 * 1024 * 1024);
        assert_eq!(defaults.max_buffered_upload_bytes, 32 * 1024 * 1024);
        // A graceful stop outlasts the slowest request it accepted.
        assert_eq!(defaults.request_timeout_ms, 30_000);
        assert_eq!(defaults.drain_timeout_ms, 35_000);
        assert!(defaults.drain_timeout_ms >= defaults.request_timeout_ms);

        let explicit = config(serde_json::json!({
            "max_request_bytes": 4096,
            "max_buffered_upload_bytes": 4096
        }))
        .unwrap();
        assert_eq!(explicit.max_buffered_upload_bytes, 4096);

        for invalid in [
            serde_json::json!({"max_request_bytes": 4096, "max_buffered_upload_bytes": 4095}),
            serde_json::json!({"max_buffered_upload_bytes": 0}),
            serde_json::json!({"max_buffered_upload_bytes": "32MiB"}),
            serde_json::json!({"max_buffered_upload_bytes": -1}),
        ] {
            let error = config(invalid).unwrap_err();
            assert!(
                error.to_string().contains("max_buffered_upload_bytes"),
                "{error}"
            );
        }
    }

    #[test]
    fn http_config_reuses_driver_shapes_for_multiple_named_drives() {
        let spec = parse_config_str(
            r#"{
                "version": 1,
                "http": {
                    "port": 0,
                    "max_request_bytes": 4096,
                    "max_response_bytes": 8192,
                    "max_directory_entries": 12,
                    "read_chunk_bytes": 1024,
                    "drain_timeout_ms": 250,
                    "max_connections": 12,
                    "request_timeout_ms": 750,
                    "drives": [
                        {
                            "id": "memory",
                            "token": {"env": "MOUNT_RS_MEMORY_TOKEN"},
                            "driver": {"kind": "memory"}
                        },
                        {
                            "id": "sqlite",
                            "token": {"env": "MOUNT_RS_SQLITE_TOKEN"},
                            "driver": {
                                "kind": "sqlite",
                                "database": "state/sqlite.db",
                                "uid": 501,
                                "gid": 20
                            }
                        }
                    ]
                }
            }"#,
            Path::new("/tmp/config"),
        )
        .unwrap();
        let http = spec.http.as_ref().expect("HTTP config");
        assert_eq!(http.host, "127.0.0.1");
        assert_eq!(http.port, 0);
        assert_eq!(http.max_request_bytes, 4096);
        // Four uploads of the largest size, unless configured.
        assert_eq!(http.max_buffered_upload_bytes, 4 * 4096);
        assert_eq!(http.max_response_bytes, 8192);
        assert_eq!(http.max_directory_entries, 12);
        assert_eq!(http.read_chunk_bytes, 1024);
        assert!(http.tcp_nodelay);
        assert!(!http.http2_prior_knowledge);
        assert_eq!(http.drain_timeout_ms, 250);
        assert_eq!(http.max_connections, 12);
        assert_eq!(http.request_timeout_ms, 750);
        assert_eq!(http.drives.len(), 2);
        assert_eq!(http.drives[0].token.name, "MOUNT_RS_MEMORY_TOKEN");
        assert_eq!(
            http.drives[1].driver.database,
            Some("/tmp/config/state/sqlite.db".into())
        );
        assert_eq!(http.drives[1].driver.root_uid, Some(501));
    }

    #[test]
    fn http_owner_requires_splitstore_before_opening_any_drives() {
        let mut value = serde_json::json!({
            "forwarding": {"peers":"peers.json", "certificate":"peer.pem", "private_key":"peer.key",
                "ca":"ca.pem", "redis_url":{"env":"TEST_REDIS"}, "namespace":"test",
                "bind":"127.0.0.1:0", "owner":true},
            "drives":[{"id":"volume", "token":{"env":"TEST_TOKEN"}, "driver":{"kind":"memory"}}]
        });
        let error = parse_http(&value, Path::new("/tmp")).unwrap_err();
        assert!(error.to_string().contains("config.http.forwarding.owner"));
        value["forwarding"]["owner"] = false.into();
        assert!(parse_http(&value, Path::new("/tmp")).is_ok());
    }

    #[test]
    fn http_tcp_nodelay_override_requires_boolean() {
        let mut value = serde_json::json!({
            "tcp_nodelay": false,
            "drives": [{"id":"memory", "token":{"env":"TEST_TOKEN"},
                "driver":{"kind":"memory"}}]
        });
        assert!(!parse_http(&value, Path::new("/tmp")).unwrap().tcp_nodelay);
        value["tcp_nodelay"] = serde_json::json!("false");
        assert!(parse_http(&value, Path::new("/tmp")).is_err());
    }

    #[test]
    fn http_config_rejects_duplicate_or_unsafe_drive_ids() {
        let duplicate = parse_config_str(
            r#"{
                "version": 1,
                "http": {
                    "drives": [
                        {"id": "same", "token": {"env": "TOKEN_A"}, "driver": {"kind": "memory"}},
                        {"id": "same", "token": {"env": "TOKEN_B"}, "driver": {"kind": "memory"}}
                    ]
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(duplicate.message().contains("duplicates another"));

        for id in [".", "..", "../escape"] {
            let config = serde_json::json!({"version": 1, "http": {"drives": [{
                "id": id, "token": {"env": "TOKEN"}, "driver": {"kind": "memory"}
            }]}});
            let error = parse_config_str(&config.to_string(), Path::new("/tmp")).unwrap_err();
            assert!(error.message().contains("1-64 ASCII"));
        }
    }

    #[test]
    fn http_config_rejects_non_loopback_hosts() {
        let error = parse_config_str(
            r#"{
                "version": 1,
                "http": {
                    "host": "0.0.0.0",
                    "drives": [
                        {"id": "memory", "token": {"env": "TOKEN"}, "driver": {"kind": "memory"}}
                    ]
                }
            }"#,
            Path::new("/tmp"),
        )
        .unwrap_err();
        assert!(error.message().contains("must be a loopback host"));
    }

    #[test]
    fn native_mount_config_cannot_silently_consume_http_settings() {
        let path = std::env::temp_dir().join(format!(
            "mount-rs-cli-http-config-{}-{}.json",
            std::process::id(),
            unique_default_owner()
        ));
        std::fs::write(
            &path,
            r#"{
                "version": 1,
                "http": {
                    "drives": [
                        {"id": "memory", "token": {"env": "TOKEN"}, "driver": {"kind": "memory"}}
                    ]
                }
            }"#,
        )
        .unwrap();
        let raw = match parse_args(["mount-rs", "--config", path.to_str().unwrap()]).unwrap() {
            Command::Mount(options) => options,
            other => panic!("expected mount, got {other:?}"),
        };
        let error = resolve_cli_options(raw).unwrap_err();
        assert!(error.message().contains("serve-http"));
        let _ = std::fs::remove_file(path);
    }
    #[test]
    fn uncached_fuse_requires_explicit_fuse_and_preserves_config_merge() {
        for transport in ["auto", "nfs", "9p"] {
            assert!(parse_args(["mount-rs", "--fuse-uncached", "--transport", transport]).is_err());
        }
        let parsed = parse_args(["mount-rs", "--fuse-uncached", "--transport", "fuse"]).unwrap();
        let Command::Mount(options) = parsed else {
            panic!("expected mount")
        };
        assert!(options.fuse_uncached);
        assert!(
            parse_config_str(
                r#"{"version":1,"transport":"fuse","fuse_uncached":"yes"}"#,
                Path::new("/tmp")
            )
            .is_err()
        );
        let incompatible = parse_config_str(
            r#"{"version":1,"transport":"nfs","fuse_uncached":true}"#,
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(validate_resolved_options(&incompatible.to_options()).is_err());
        let spec = parse_config_str(
            r#"{"version":1,"transport":"fuse","fuse_uncached":true}"#,
            Path::new("/tmp"),
        )
        .unwrap();
        assert!(spec.to_options().fuse_uncached);
        let path =
            std::env::temp_dir().join(format!("fuse-uncached-{}.json", unique_default_owner()));
        std::fs::write(
            &path,
            r#"{"version":1,"transport":"fuse","fuse_uncached":false}"#,
        )
        .unwrap();
        let parsed = parse_args([
            "mount-rs",
            "--config",
            path.to_str().unwrap(),
            "--fuse-uncached",
        ])
        .unwrap();
        let Command::Mount(options) = parsed else {
            panic!("expected mount")
        };
        let result = resolve_cli_options(options);
        std::fs::remove_file(path).unwrap();
        assert!(result.unwrap().fuse_uncached);
    }
    #[test]
    fn remote_configuration_is_static_and_credential_reference_only() {
        let spec = parse_config_str(r#"{"version":1,"driver":{"kind":"remote","server":"https://mount.example","volume":"drive-1","token":{"env":"MOUNT_CLIENT_TOKEN"}}}"#, Path::new(".")).unwrap();
        let options = spec.to_options();
        validate_resolved_options(&options).unwrap();
        assert_eq!(options.driver, DriverChoice::Remote);
        assert_eq!(options.remote.as_ref().unwrap().timeout_ms, 30_000);
        let mut mixed = options.clone();
        mixed.database = Some("local.db".into());
        assert!(validate_resolved_options(&mixed).is_err());
        mixed = options;
        mixed.driver = DriverChoice::Memory;
        assert!(validate_resolved_options(&mixed).is_err());
        for driver in [
            r#"{"kind":"remote","server":"https://mount.example","volume":"drive","token":"secret"}"#,
            r#"{"kind":"remote","server":"http://mount.example","volume":"drive","token":{"env":"TOKEN"}}"#,
            r#"{"kind":"remote","server":"https://mount.example","volume":"../drive","token":{"env":"TOKEN"}}"#,
            r#"{"kind":"remote","server":"https://mount.example","volume":"drive","token":{"env":"TOKEN"},"timeout_ms":0}"#,
            r#"{"kind":"remote","server":"https://mount.example","volume":"drive","token":{"env":"TOKEN"},"storage":{}}"#,
        ] {
            assert!(
                parse_config_str(
                    &format!("{{\"version\":1,\"driver\":{driver}}}"),
                    Path::new(".")
                )
                .is_err()
            );
        }
    }
    #[test]
    fn remote_http_sessions_require_explicit_splitstore_configuration() {
        let mut value = serde_json::json!({
            "remote_sessions": true,
            "drives": [{"id":"volume", "token":{"env":"TOKEN"}, "driver":{"kind":"memory"}}]
        });
        assert!(parse_http(&value, Path::new("/tmp")).is_err());
        value["drives"][0]["driver"] = serde_json::json!({"kind":"splitstore", "storage":{
            "metadata":{"kind":"memory"},"blocks":{"kind":"memory"}
        }});
        assert!(
            parse_http(&value, Path::new("/tmp"))
                .unwrap()
                .remote_sessions
        );
        value["remote_sessions"] = serde_json::json!("true");
        assert!(parse_http(&value, Path::new("/tmp")).is_err());
        value.as_object_mut().unwrap().remove("remote_sessions");
        assert!(
            !parse_http(&value, Path::new("/tmp"))
                .unwrap()
                .remote_sessions
        );
    }
}
