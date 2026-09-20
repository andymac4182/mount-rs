//! Versioned, side-effect-free CLI configuration.
//!
//! The config parser deliberately uses serde_json::Value instead of derive
//! support. That keeps the CLI's direct dependency surface small while still
//! allowing every object in the public schema to reject unknown fields.

use std::fmt::{self, Display, Formatter};
use std::fs;
use std::hash::{BuildHasher, RandomState};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

use crate::parser::{CliOptions, CliOverrides, DriverChoice, TransportChoice};

pub const CONFIG_VERSION: u64 = 1;
pub const DEFAULT_CHUNK_SIZE_BYTES: usize = 64 * 1024;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StorageProvider {
    Memory,
    Sqlite {
        path: PathBuf,
    },
    Pglite {
        connection: EnvReference,
        volume_key: String,
        durable: bool,
    },
    R2 {
        endpoint: String,
        bucket: String,
        prefix: String,
        access_key_id: EnvReference,
        secret_access_key: EnvReference,
        durable: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitStorageConfig {
    pub metadata: StorageProvider,
    pub blocks: StorageProvider,
    pub chunk_size_bytes: usize,
    pub owner: Option<String>,
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
            "sqlite_single_host",
            "driver",
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
        sqlite_single_host: optional_bool(object, "sqlite_single_host", "config")?,
        ..ConfigSpec::default()
    };

    if let Some(driver) = object.get("driver") {
        parse_driver(driver, base_dir, &mut spec)?;
    }
    validate_spec(&spec)
}

fn parse_driver(value: &Value, base_dir: &Path, spec: &mut ConfigSpec) -> Result<(), ConfigError> {
    let object = object(value, "config.driver")?;
    let kind = required_string(object, "kind", "config.driver")?;
    match kind {
        "memory" => {
            reject_unknown(object, &["kind"], "config.driver")?;
            spec.driver = Some(DriverChoice::Memory);
        }
        "host" => {
            reject_unknown(object, &["kind", "root"], "config.driver")?;
            spec.driver = Some(DriverChoice::Host);
            spec.root = Some(required_path(object, "root", "config.driver", base_dir)?);
        }
        "sqlite" => {
            reject_unknown(object, &["kind", "database", "uid", "gid"], "config.driver")?;
            spec.driver = Some(DriverChoice::Sqlite);
            spec.database = Some(required_path(
                object,
                "database",
                "config.driver",
                base_dir,
            )?);
            spec.root_uid = optional_u32(object, "uid", "config.driver")?;
            spec.root_gid = optional_u32(object, "gid", "config.driver")?;
            if spec.root_uid.is_some() != spec.root_gid.is_some() {
                return Err(ConfigError::at(
                    "config.driver",
                    "uid and gid must be supplied together",
                ));
            }
        }
        "splitstore" => {
            spec.driver = Some(DriverChoice::SplitStore);
            if object.contains_key("storage") {
                // The structured form cannot be mixed with legacy SQLite
                // path fields or irrelevant host-driver fields.
                reject_unknown(object, &["kind", "storage"], "config.driver")?;
                spec.storage = Some(parse_storage(
                    object
                        .get("storage")
                        .ok_or_else(|| ConfigError::at("config.driver.storage", "is missing"))?,
                    base_dir,
                )?);
            } else {
                reject_unknown(object, &["kind", "database", "blocks"], "config.driver")?;
                spec.database = optional_path(object, "database", "config.driver", base_dir)?;
                spec.blocks = optional_path(object, "blocks", "config.driver", base_dir)?;
                if spec.database.is_some() != spec.blocks.is_some() {
                    return Err(ConfigError::at(
                        "config.driver",
                        "legacy splitstore database and blocks must be supplied together",
                    ));
                }
            }
        }
        _ => {
            return Err(ConfigError::at(
                "config.driver.kind",
                format!("unknown driver '{kind}'"),
            ));
        }
    }
    Ok(())
}

fn parse_storage(value: &Value, base_dir: &Path) -> Result<SplitStorageConfig, ConfigError> {
    let object = object(value, "config.driver.storage")?;
    reject_unknown(
        object,
        &["metadata", "blocks", "chunk_size_bytes", "owner"],
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
    let owner = object
        .get("owner")
        .map(|value| required_value_string(value, "config.driver.storage.owner"))
        .transpose()?;
    if let Some(owner) = &owner {
        validate_owner(owner)?;
    }
    Ok(SplitStorageConfig {
        metadata,
        blocks,
        chunk_size_bytes,
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
            StorageProvider::R2 {
                endpoint: required_nonempty_string(object, "endpoint", path)?,
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

fn apply_explicit_overrides(resolved: &mut CliOptions, raw: &CliOptions) {
    let overrides = &raw.overrides;
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
    Ok(())
}

impl ConfigSpec {
    fn to_options(&self) -> CliOptions {
        CliOptions {
            mountpoint: self.mountpoint.clone(),
            transport: self.transport.unwrap_or(TransportChoice::Auto),
            quiet: self.quiet.unwrap_or(false),
            verbose: self.verbose.unwrap_or(false),
            read_only: self.read_only.unwrap_or(false),
            empty: self.empty.unwrap_or(false),
            allow_other: self.allow_other.unwrap_or(false),
            sqlite_single_host: self.sqlite_single_host.unwrap_or(false),
            driver: self.driver.unwrap_or(DriverChoice::Memory),
            root: self.root.clone(),
            database: self.database.clone(),
            blocks: self.blocks.clone(),
            root_uid: self.root_uid,
            root_gid: self.root_gid,
            config: None,
            storage: self.storage.clone().map(Box::new),
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
    fn static_validation_does_not_require_provider_credentials() {
        let spec = parse_config_str(SPLIT_MEMORY, Path::new("/tmp")).unwrap();
        assert_eq!(spec.storage.as_ref().unwrap().chunk_size_bytes, 4096);
        assert!(validate_resolved_options(&spec.to_options()).is_ok());
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
}
