//! Manual argument parsing for the small `mount-rs` binary.
//!
//! Keeping this parser local avoids a command-line framework dependency and
//! makes the error/help/version/probe paths completely mount-free. The long
//! option spellings intentionally follow the pinned mountx CLI; the driver
//! options are mount-rs additions for selecting the built-in integrations.

use std::ffi::OsString;
use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;

use crate::color::Color;

const TRANSPORT_NAMES: [&str; 4] = ["auto", "fuse", "9p", "nfs"];
const DRIVER_NAMES: [&str; 4] = ["memory", "host", "sqlite", "splitstore"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportChoice {
    Auto,
    Fuse,
    P9,
    Nfs,
}

impl TransportChoice {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Fuse => "fuse",
            Self::P9 => "9p",
            Self::Nfs => "nfs",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, ParseError> {
        match value {
            "auto" => Ok(Self::Auto),
            "fuse" => Ok(Self::Fuse),
            "9p" => Ok(Self::P9),
            "nfs" => Ok(Self::Nfs),
            _ => Err(ParseError::new(format!(
                "unknown transport '{value}' (expected {})",
                TRANSPORT_NAMES.join(", ")
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverChoice {
    Memory,
    Host,
    Sqlite,
    SplitStore,
}

impl DriverChoice {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Host => "host",
            Self::Sqlite => "sqlite",
            Self::SplitStore => "splitstore",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, ParseError> {
        match value {
            "memory" => Ok(Self::Memory),
            "host" => Ok(Self::Host),
            "sqlite" => Ok(Self::Sqlite),
            "splitstore" => Ok(Self::SplitStore),
            _ => Err(ParseError::new(format!(
                "unknown driver '{value}' (expected {})",
                DRIVER_NAMES.join(", ")
            ))),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CliOptions {
    pub mountpoint: Option<PathBuf>,
    /// Additional mountpoints served by the same opened filesystem in this
    /// CLI process. Each gets its own native NFS client/server lifecycle.
    pub also_mountpoints: Vec<PathBuf>,
    pub transport: TransportChoice,
    pub quiet: bool,
    pub verbose: bool,
    pub read_only: bool,
    pub empty: bool,
    pub allow_other: bool,
    pub sqlite_single_host: bool,
    pub driver: DriverChoice,
    pub root: Option<PathBuf>,
    pub database: Option<PathBuf>,
    pub blocks: Option<PathBuf>,
    /// Optional virtual-root ownership configured by a SQLite JSON config.
    /// These values are metadata for the mounted filesystem, not host-file
    /// ownership or forged NFS credentials.
    pub root_uid: Option<u32>,
    pub root_gid: Option<u32>,
    /// Optional versioned JSON configuration. Runtime resolves this before a
    /// mount; the parser itself remains filesystem-free.
    pub config: Option<PathBuf>,
    /// Explicit storage composition supplied by a config file.
    pub storage: Option<Box<crate::config::SplitStorageConfig>>,
    /// Flags that were actually present on the command line. Defaults must
    /// never silently override values from a config file.
    pub overrides: CliOverrides,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliOverrides {
    pub mountpoint: bool,
    pub transport: bool,
    pub quiet: bool,
    pub verbose: bool,
    pub read_only: bool,
    pub empty: bool,
    pub allow_other: bool,
    pub sqlite_single_host: bool,
    pub driver: bool,
    pub root: bool,
    pub database: bool,
    pub blocks: bool,
}

// Provenance is an implementation detail used during config merging. Parsed
// options with the same effective values remain semantically equal.
impl PartialEq for CliOptions {
    fn eq(&self, other: &Self) -> bool {
        self.mountpoint == other.mountpoint
            && self.also_mountpoints == other.also_mountpoints
            && self.transport == other.transport
            && self.quiet == other.quiet
            && self.verbose == other.verbose
            && self.read_only == other.read_only
            && self.empty == other.empty
            && self.allow_other == other.allow_other
            && self.sqlite_single_host == other.sqlite_single_host
            && self.driver == other.driver
            && self.root == other.root
            && self.database == other.database
            && self.blocks == other.blocks
            && self.root_uid == other.root_uid
            && self.root_gid == other.root_gid
            && self.config == other.config
            && self.storage == other.storage
    }
}

impl Eq for CliOptions {}

impl Default for CliOptions {
    fn default() -> Self {
        Self {
            mountpoint: None,
            also_mountpoints: Vec::new(),
            transport: TransportChoice::Auto,
            quiet: false,
            verbose: false,
            read_only: false,
            empty: false,
            allow_other: false,
            sqlite_single_host: false,
            driver: DriverChoice::Memory,
            root: None,
            database: None,
            blocks: None,
            root_uid: None,
            root_gid: None,
            config: None,
            storage: None,
            overrides: CliOverrides::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Mount(CliOptions),
    ServeHttp(PathBuf),
    ServeRemote(PathBuf),
    MountRemote(PathBuf),
    CatalogApply(PathBuf),
    Probe,
    ValidateConfig(PathBuf),
    EnrollDirectoryOwnership {
        config: PathBuf,
        expected_revision: u64,
    },
    DirectoryOwnershipStatus {
        config: PathBuf,
    },
    RecoverDirectoryOwnership {
        config: PathBuf,
        root: u64,
        expected_fence: u64,
    },
    MigrateConcurrentBacking {
        config: PathBuf,
        expected_revision: u64,
    },
    ReenrollSqliteConcurrentBacking {
        config: PathBuf,
        expected_revision: u64,
        expected_volume_id: String,
    },
    SdkSelfTest {
        config: Option<PathBuf>,
        reopen: bool,
    },
    Help,
    Version,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    message: String,
}

impl ParseError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl Display for ParseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ParseError {}

pub fn parse_args<I, S>(args: I) -> Result<Command, ParseError>
where
    I: IntoIterator<Item = S>,
    S: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    let _program = args.next();
    let mut values = CliOptions::default();
    let mut positionals = Vec::new();
    let mut after_double_dash = false;
    let mut first_argument = true;

    while let Some(raw) = args.next() {
        let raw = raw.to_string_lossy().into_owned();
        if after_double_dash {
            positionals.push(raw);
            continue;
        }
        if raw == "--" {
            after_double_dash = true;
            continue;
        }
        if first_argument && raw == "probe" {
            if args.next().is_some() {
                return Err(ParseError::new(
                    "probe does not accept mount options or a mountpoint",
                ));
            }
            return Ok(Command::Probe);
        }
        if first_argument && raw == "validate-config" {
            let mut config_path = None;
            while let Some(raw) = args.next() {
                let raw = raw.to_string_lossy().into_owned();
                if raw == "--help" || raw == "-h" {
                    return Ok(Command::Help);
                }
                let Some(name) = raw.strip_prefix("--") else {
                    return Err(ParseError::new(
                        "validate-config accepts only --config <path>",
                    ));
                };
                let (name, inline_value) = match name.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_owned())),
                    None => (name, None),
                };
                if name != "config" {
                    return Err(ParseError::new(
                        "validate-config accepts only --config <path>",
                    ));
                }
                if config_path.is_some() {
                    return Err(ParseError::new(
                        "validate-config accepts only one --config <path>",
                    ));
                }
                config_path = Some(PathBuf::from(value("config", inline_value, &mut args)?));
            }
            return config_path
                .map(Command::ValidateConfig)
                .ok_or_else(|| ParseError::new("validate-config requires --config <path>"));
        }
        if first_argument
            && matches!(
                raw.as_str(),
                "enroll-directory-ownership"
                    | "directory-ownership-status"
                    | "recover-directory-ownership"
            )
        {
            let mut config = None;
            let mut expected_revision = None;
            let mut root = None;
            let mut expected_fence = None;
            while let Some(argument) = args.next() {
                let argument = argument.to_string_lossy().into_owned();
                let Some(name) = argument.strip_prefix("--") else {
                    return Err(ParseError::new(format!("{raw} accepts only named options")));
                };
                let (name, inline) = match name.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_owned())),
                    None => (name, None),
                };
                match name {
                    "config" if config.is_none() => {
                        config = Some(PathBuf::from(value(name, inline, &mut args)?))
                    }
                    "expected-revision"
                        if raw == "enroll-directory-ownership" && expected_revision.is_none() =>
                    {
                        expected_revision = Some(
                            value(name, inline, &mut args)?
                                .parse::<u64>()
                                .map_err(|_| {
                                    ParseError::new(
                                        "--expected-revision must be an unsigned 64-bit integer",
                                    )
                                })?,
                        );
                    }
                    "root-inode" if raw == "recover-directory-ownership" && root.is_none() => {
                        root = Some(value(name, inline, &mut args)?.parse::<u64>().map_err(
                            |_| ParseError::new("--root-inode must be an unsigned 64-bit integer"),
                        )?)
                    }
                    "expected-fence"
                        if raw == "recover-directory-ownership" && expected_fence.is_none() =>
                    {
                        expected_fence = Some(
                            value(name, inline, &mut args)?
                                .parse::<u64>()
                                .map_err(|_| {
                                    ParseError::new(
                                        "--expected-fence must be an unsigned 64-bit integer",
                                    )
                                })?,
                        );
                    }
                    _ => {
                        return Err(ParseError::new(format!(
                            "unexpected or duplicate --{name} for {raw}"
                        )));
                    }
                }
            }
            let config =
                config.ok_or_else(|| ParseError::new(format!("{raw} requires --config <path>")))?;
            return Ok(match raw.as_str() {
                "enroll-directory-ownership" => Command::EnrollDirectoryOwnership {
                    config,
                    expected_revision: expected_revision.ok_or_else(|| {
                        ParseError::new(
                            "enroll-directory-ownership requires --expected-revision <u64>",
                        )
                    })?,
                },
                "directory-ownership-status" => Command::DirectoryOwnershipStatus { config },
                _ => Command::RecoverDirectoryOwnership {
                    config,
                    root: root.ok_or_else(|| {
                        ParseError::new("recover-directory-ownership requires --root-inode <u64>")
                    })?,
                    expected_fence: expected_fence.ok_or_else(|| {
                        ParseError::new(
                            "recover-directory-ownership requires --expected-fence <u64>",
                        )
                    })?,
                },
            });
        }
        if first_argument && raw == "migrate-concurrent-backing" {
            let mut config_path = None;
            let mut expected_revision = None;
            while let Some(raw) = args.next() {
                let raw = raw.to_string_lossy().into_owned();
                if raw == "--help" || raw == "-h" {
                    return Ok(Command::Help);
                }
                let Some(name) = raw.strip_prefix("--") else {
                    return Err(ParseError::new(
                        "migrate-concurrent-backing accepts only --config <path> and --expected-revision <u64>",
                    ));
                };
                let (name, inline_value) = match name.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_owned())),
                    None => (name, None),
                };
                match name {
                    "config" => {
                        if config_path.is_some() {
                            return Err(ParseError::new(
                                "migrate-concurrent-backing accepts only one --config <path>",
                            ));
                        }
                        config_path =
                            Some(PathBuf::from(value("config", inline_value, &mut args)?));
                    }
                    "expected-revision" => {
                        if expected_revision.is_some() {
                            return Err(ParseError::new(
                                "migrate-concurrent-backing accepts only one --expected-revision <u64>",
                            ));
                        }
                        let raw = value("expected-revision", inline_value, &mut args)?;
                        expected_revision = Some(raw.parse::<u64>().map_err(|_| {
                            ParseError::new(
                                "--expected-revision must be an unsigned 64-bit integer",
                            )
                        })?);
                    }
                    _ => {
                        return Err(ParseError::new(
                            "migrate-concurrent-backing accepts only --config <path> and --expected-revision <u64>",
                        ));
                    }
                }
            }
            let config = config_path.ok_or_else(|| {
                ParseError::new("migrate-concurrent-backing requires --config <path>")
            })?;
            let expected_revision = expected_revision.ok_or_else(|| {
                ParseError::new("migrate-concurrent-backing requires --expected-revision <u64>")
            })?;
            return Ok(Command::MigrateConcurrentBacking {
                config,
                expected_revision,
            });
        }
        if first_argument && raw == "reenroll-sqlite-concurrent-backing" {
            let mut config_path = None;
            let mut expected_revision = None;
            let mut expected_volume_id = None;
            let mut assertion = false;
            while let Some(raw) = args.next() {
                let raw = raw.to_string_lossy().into_owned();
                if raw == "--help" || raw == "-h" {
                    return Ok(Command::Help);
                }
                let Some(name) = raw.strip_prefix("--") else {
                    return Err(ParseError::new(
                        "reenroll-sqlite-concurrent-backing accepts only its required options",
                    ));
                };
                let (name, inline_value) = match name.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_owned())),
                    None => (name, None),
                };
                match name {
                    "config" if config_path.is_none() => {
                        config_path =
                            Some(PathBuf::from(value("config", inline_value, &mut args)?));
                    }
                    "expected-revision" if expected_revision.is_none() => {
                        let raw = value("expected-revision", inline_value, &mut args)?;
                        expected_revision = Some(raw.parse::<u64>().map_err(|_| {
                            ParseError::new(
                                "--expected-revision must be an unsigned 64-bit integer",
                            )
                        })?);
                    }
                    "expected-volume-id" if expected_volume_id.is_none() => {
                        expected_volume_id =
                            Some(value("expected-volume-id", inline_value, &mut args)?);
                    }
                    "assert-all-writers-stopped-and-sole-metadata-copy"
                        if !assertion && inline_value.is_none() =>
                    {
                        assertion = true;
                    }
                    _ => {
                        return Err(ParseError::new(
                            "reenroll-sqlite-concurrent-backing has an unknown or repeated option",
                        ));
                    }
                }
            }
            let config = config_path.ok_or_else(|| {
                ParseError::new("reenroll-sqlite-concurrent-backing requires --config <path>")
            })?;
            let expected_revision = expected_revision.ok_or_else(|| {
                ParseError::new(
                    "reenroll-sqlite-concurrent-backing requires --expected-revision <u64>",
                )
            })?;
            let expected_volume_id = expected_volume_id.ok_or_else(|| {
                ParseError::new(
                    "reenroll-sqlite-concurrent-backing requires --expected-volume-id <id>",
                )
            })?;
            if !assertion {
                return Err(ParseError::new(
                    "reenroll-sqlite-concurrent-backing requires --assert-all-writers-stopped-and-sole-metadata-copy",
                ));
            }
            return Ok(Command::ReenrollSqliteConcurrentBacking {
                config,
                expected_revision,
                expected_volume_id,
            });
        }
        if first_argument && raw == "sdk-self-test" {
            let mut config_path = None;
            let mut reopen = false;
            while let Some(raw) = args.next() {
                let raw = raw.to_string_lossy().into_owned();
                if raw == "--help" || raw == "-h" {
                    return Ok(Command::Help);
                }
                if raw == "--reopen" {
                    if reopen {
                        return Err(ParseError::new(
                            "sdk-self-test accepts --reopen at most once",
                        ));
                    }
                    reopen = true;
                    continue;
                }
                let Some(name) = raw.strip_prefix("--") else {
                    return Err(ParseError::new(
                        "sdk-self-test accepts only --config <path> and --reopen",
                    ));
                };
                let (name, inline_value) = match name.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_owned())),
                    None => (name, None),
                };
                if name != "config" {
                    return Err(ParseError::new(
                        "sdk-self-test accepts only --config <path> and --reopen",
                    ));
                }
                if config_path.is_some() {
                    return Err(ParseError::new(
                        "sdk-self-test accepts only one --config <path>",
                    ));
                }
                config_path = Some(PathBuf::from(value("config", inline_value, &mut args)?));
            }
            if reopen && config_path.is_none() {
                return Err(ParseError::new(
                    "sdk-self-test --reopen requires --config <path>",
                ));
            }
            return Ok(Command::SdkSelfTest {
                config: config_path,
                reopen,
            });
        }
        if first_argument
            && matches!(
                raw.as_str(),
                "serve-http" | "serve-remote" | "mount-remote" | "catalog-apply"
            )
        {
            let service_command = raw.clone();
            let mut config_path = None;
            while let Some(raw) = args.next() {
                let raw = raw.to_string_lossy().into_owned();
                if raw == "--help" || raw == "-h" {
                    return Ok(Command::Help);
                }
                let Some(name) = raw.strip_prefix("--") else {
                    return Err(ParseError::new("serve-http accepts only --config <path>"));
                };
                let (name, inline_value) = match name.split_once('=') {
                    Some((name, value)) => (name, Some(value.to_owned())),
                    None => (name, None),
                };
                if name != "config" {
                    return Err(ParseError::new("serve-http accepts only --config <path>"));
                }
                if config_path.is_some() {
                    return Err(ParseError::new(
                        "serve-http accepts only one --config <path>",
                    ));
                }
                config_path = Some(PathBuf::from(value("config", inline_value, &mut args)?));
            }
            return config_path
                .map(|path| match service_command.as_str() {
                    "serve-remote" => Command::ServeRemote(path),
                    "mount-remote" => Command::MountRemote(path),
                    "catalog-apply" => Command::CatalogApply(path),
                    _ => Command::ServeHttp(path),
                })
                .ok_or_else(|| ParseError::new("serve-http requires --config <path>"));
        }
        if first_argument && raw == "mount" {
            first_argument = false;
            continue;
        }
        first_argument = false;

        if raw == "--help" || raw == "-h" {
            return Ok(Command::Help);
        }
        if raw == "--version" || raw == "-V" {
            return Ok(Command::Version);
        }
        if raw == "--probe" {
            if args.next().is_some() || !positionals.is_empty() {
                return Err(ParseError::new(
                    "--probe does not accept mount options or a mountpoint",
                ));
            }
            return Ok(Command::Probe);
        }
        if let Some(name) = raw.strip_prefix("--") {
            let (name, inline_value) = match name.split_once('=') {
                Some((name, value)) => (name, Some(value.to_owned())),
                None => (name, None),
            };
            match name {
                "quiet" => {
                    values.quiet = true;
                    values.overrides.quiet = true;
                }
                "verbose" => {
                    values.verbose = true;
                    values.overrides.verbose = true;
                }
                "read-only" => {
                    values.read_only = true;
                    values.overrides.read_only = true;
                }
                "empty" => {
                    values.empty = true;
                    values.overrides.empty = true;
                }
                "allow-other" => {
                    values.allow_other = true;
                    values.overrides.allow_other = true;
                }
                "sqlite-single-host" => {
                    values.sqlite_single_host = true;
                    values.overrides.sqlite_single_host = true;
                }
                "mountpoint" => {
                    if values.overrides.mountpoint {
                        return Err(ParseError::new(
                            "option '--mountpoint' was supplied more than once",
                        ));
                    }
                    values.mountpoint = Some(PathBuf::from(value(name, inline_value, &mut args)?));
                    values.overrides.mountpoint = true;
                }
                "also-mountpoint" => {
                    values.also_mountpoints.push(PathBuf::from(value(
                        name,
                        inline_value,
                        &mut args,
                    )?));
                }
                "config" => {
                    if values.config.is_some() {
                        return Err(ParseError::new(
                            "option '--config' was supplied more than once",
                        ));
                    }
                    values.config = Some(PathBuf::from(value(name, inline_value, &mut args)?));
                }
                "transport" => {
                    values.transport =
                        TransportChoice::parse(&value(name, inline_value, &mut args)?)?;
                    values.overrides.transport = true;
                }
                "driver" => {
                    values.driver = DriverChoice::parse(&value(name, inline_value, &mut args)?)?;
                    values.overrides.driver = true;
                }
                "root" => {
                    values.root = Some(PathBuf::from(value(name, inline_value, &mut args)?));
                    values.overrides.root = true;
                }
                "database" | "db" => {
                    values.database = Some(PathBuf::from(value(name, inline_value, &mut args)?));
                    values.overrides.database = true;
                }
                "blocks" => {
                    values.blocks = Some(PathBuf::from(value(name, inline_value, &mut args)?));
                    values.overrides.blocks = true;
                }
                other => return Err(ParseError::new(format!("unknown option '--{other}'"))),
            }
            continue;
        }
        if let Some(shorts) = raw.strip_prefix('-') {
            if shorts.is_empty() {
                positionals.push(raw);
                continue;
            }
            parse_short_options(shorts, &mut values, &mut args)?;
            continue;
        }
        positionals.push(raw);
    }

    if positionals.len() > 1 {
        return Err(ParseError::new(format!(
            "one mountpoint at a time, got {}",
            positionals.len()
        )));
    }
    if let Some(mountpoint) = positionals.into_iter().next() {
        if values.mountpoint.is_some() {
            return Err(ParseError::new(
                "mountpoint was supplied both positionally and with --mountpoint",
            ));
        }
        values.mountpoint = Some(PathBuf::from(mountpoint));
        values.overrides.mountpoint = true;
    }
    if values.config.is_none() {
        validate_driver_options(&values)?;
    }
    validate_transport_options(&values)?;
    Ok(Command::Mount(values))
}

fn parse_short_options<I>(
    shorts: &str,
    values: &mut CliOptions,
    args: &mut I,
) -> Result<(), ParseError>
where
    I: Iterator,
    I::Item: Into<OsString>,
{
    for (index, character) in shorts.char_indices() {
        match character {
            'q' => {
                values.quiet = true;
                values.overrides.quiet = true;
            }
            'v' => {
                values.verbose = true;
                values.overrides.verbose = true;
            }
            'r' => {
                values.read_only = true;
                values.overrides.read_only = true;
            }
            'h' => return Err(ParseError::new("--help must be used as '-h'")),
            'V' => return Err(ParseError::new("--version must be used as '-V'")),
            'm' | 't' => {
                let inline = shorts
                    .get(index + character.len_utf8()..)
                    .filter(|value| !value.is_empty());
                let value = inline.map(ToOwned::to_owned).map(Ok).unwrap_or_else(|| {
                    next_value(
                        if character == 'm' {
                            "mountpoint"
                        } else {
                            "transport"
                        },
                        args,
                    )
                })?;
                if character == 'm' {
                    values.mountpoint = Some(PathBuf::from(value));
                    values.overrides.mountpoint = true;
                } else {
                    values.transport = TransportChoice::parse(&value)?;
                    values.overrides.transport = true;
                }
                break;
            }
            other => return Err(ParseError::new(format!("unknown option '-{other}'"))),
        }
    }
    Ok(())
}

fn value<I>(name: &str, inline: Option<String>, args: &mut I) -> Result<String, ParseError>
where
    I: Iterator,
    I::Item: Into<OsString>,
{
    inline.map(Ok).unwrap_or_else(|| next_value(name, args))
}

fn next_value<I>(name: &str, args: &mut I) -> Result<String, ParseError>
where
    I: Iterator,
    I::Item: Into<OsString>,
{
    args.next()
        .map(Into::into)
        .map(|value| value.to_string_lossy().into_owned())
        .filter(|value| !value.starts_with('-') || value == "-")
        .ok_or_else(|| ParseError::new(format!("option '--{name}' needs a value")))
}

fn validate_driver_options(values: &CliOptions) -> Result<(), ParseError> {
    match values.driver {
        DriverChoice::Memory
            if values.root.is_some() || values.database.is_some() || values.blocks.is_some() =>
        {
            Err(ParseError::new(
                "--root, --database, and --blocks require --driver host, sqlite, or splitstore",
            ))
        }
        DriverChoice::Host if values.database.is_some() || values.blocks.is_some() => Err(
            ParseError::new("--database and --blocks require --driver sqlite or splitstore"),
        ),
        DriverChoice::Sqlite if values.database.is_none() => Err(ParseError::new(
            "--driver sqlite requires --database <path>",
        )),
        DriverChoice::Sqlite if values.root.is_some() || values.blocks.is_some() => {
            Err(ParseError::new(
                "--root is only valid for --driver host; --blocks is only valid for --driver splitstore",
            ))
        }
        DriverChoice::SplitStore
            if values.root.is_some() || values.database.is_some() != values.blocks.is_some() =>
        {
            if values.root.is_some() {
                Err(ParseError::new("--root is only valid for --driver host"))
            } else {
                Err(ParseError::new(
                    "splitstore needs both --database <metadata.db> and --blocks <blocks.db>, or neither for volatile storage",
                ))
            }
        }
        _ => Ok(()),
    }
}

fn validate_transport_options(values: &CliOptions) -> Result<(), ParseError> {
    if values.sqlite_single_host
        && matches!(
            values.transport,
            TransportChoice::Fuse | TransportChoice::P9
        )
    {
        return Err(ParseError::new(
            "--sqlite-single-host is only valid with --transport nfs or auto",
        ));
    }
    Ok(())
}

pub fn help_text(color: Color) -> String {
    let b = |text: &str| color.bold(text).to_string();
    let d = |text: &str| color.dim(text).to_string();
    let output = format!(
        "\n{} {}\n\n{}  mount-rs [mountpoint] [options]\n       mount-rs mount [mountpoint] [options]\n       mount-rs serve-http --config <path>\n       mount-rs serve-remote --config <path>\n       mount-rs mount-remote --config <path>\n       mount-rs catalog-apply --config <path>\n       mount-rs sdk-self-test [--config <path>] [--reopen]\n       mount-rs enroll-directory-ownership --config <path> --expected-revision <u64>\n       mount-rs directory-ownership-status --config <path>\n       mount-rs recover-directory-ownership --config <path> --root-inode <u64> --expected-fence <u64>\n\n{}\n  -m, --mountpoint {}  where to mount {}\n      --also-mountpoint <path>  add another NFS view of this filesystem (repeatable)\n  -t, --transport {}   auto | fuse | 9p | nfs {}\n      --sqlite-single-host  use the single-host SQLite NFS profile (nfs or auto)\n  -q, --quiet              do not log filesystem requests\n  -v, --verbose            log metadata polls too {}\n  -r, --read-only          mount read-only\n      --empty              start without the memory README\n      --allow-other        let other users see the FUSE mount\n      --driver {}    memory | host | sqlite | splitstore {}\n      --root {}      host driver root {}\n      --database {}  SQLite state/metadata database\n      --blocks {}    splitstore block database\n      --probe              print transport availability without mounting\n  -h, --help               this\n  -V, --version            print the version\n\n{}\n{}\n",
        b("mount-rs"),
        d("— mount a selected filesystem driver and watch kernel requests"),
        b("Usage:"),
        b("Options:"),
        d("<path>"),
        d("(default: ~/mountx, $MOUNT_RS_MOUNTPOINT or $MOUNTX_MOUNTPOINT)"),
        d("<name>"),
        d("(default: auto)"),
        d("(lstat, stat, statfs)"),
        d("<name>"),
        d("(default: memory)"),
        d("<path>"),
        d("(default: current directory)"),
        d("<path>"),
        d("<path>"),
        d("Ctrl-C unmounts and exits."),
        d("Use 'mount-rs probe' for the same probe command form."),
    );
    let config_help = format!(
        "      --config {}      read a versioned JSON config file; explicit flags override it\n      validate-config --config {}  validate JSON/schema without opening providers\n",
        d("<path>"),
        d("<path>"),
    );
    let output = output.replace(
        "       mount-rs sdk-self-test [--config <path>] [--reopen]",
        "       mount-rs sdk-self-test [--config <path>] [--reopen]\n       mount-rs migrate-concurrent-backing --config <path> --expected-revision <u64>\n       mount-rs reenroll-sqlite-concurrent-backing --config <path> --expected-revision <u64> --expected-volume-id <id> --assert-all-writers-stopped-and-sole-metadata-copy",
    );
    let output = output.replace(
        "      --sqlite-single-host",
        &format!("{config_help}      --sqlite-single-host"),
    );
    format!(
        "{output}{}\n",
        d(
            "Before MRC1 migration, stop all old mounts on every host and keep them stopped until all clients are upgraded."
        )
    )
}

pub fn version_text() -> String {
    format!("mount-rs {}", env!("CARGO_PKG_VERSION"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(values: &[&str]) -> Command {
        parse_args(values.iter().copied()).unwrap()
    }

    #[test]
    fn directory_ownership_offline_commands_require_exact_inputs() {
        assert!(
            parse_args([
                "mount-rs",
                "enroll-directory-ownership",
                "--config",
                "shared.json",
                "--expected-revision",
                "7"
            ])
            .is_ok()
        );
        assert!(
            parse_args([
                "mount-rs",
                "directory-ownership-status",
                "--config",
                "shared.json"
            ])
            .is_ok()
        );
        assert!(
            parse_args([
                "mount-rs",
                "recover-directory-ownership",
                "--config",
                "shared.json",
                "--root-inode",
                "7",
                "--expected-fence",
                "9"
            ])
            .is_ok()
        );
        for args in [
            vec![
                "mount-rs",
                "enroll-directory-ownership",
                "--config",
                "shared.json",
            ],
            vec![
                "mount-rs",
                "recover-directory-ownership",
                "--config",
                "shared.json",
                "--root-inode",
                "7",
            ],
            vec![
                "mount-rs",
                "recover-directory-ownership",
                "--config",
                "shared.json",
                "--expected-fence",
                "9",
            ],
            vec![
                "mount-rs",
                "directory-ownership-status",
                "--config",
                "shared.json",
                "--expected-fence",
                "9",
            ],
        ] {
            assert!(parse_args(args).is_err());
        }
    }

    #[test]
    fn defaults_match_mountx_for_a_mount_command() {
        assert_eq!(parse(&["mount-rs"]), Command::Mount(CliOptions::default()));
    }

    #[test]
    fn long_and_short_options_are_parsed_without_a_framework() {
        let command = parse(&[
            "mount-rs",
            "/tmp/mount",
            "-qv",
            "-r",
            "-t",
            "nfs",
            "--driver=splitstore",
            "--database",
            "meta.db",
            "--blocks=blocks.db",
        ]);
        assert_eq!(
            command,
            Command::Mount(CliOptions {
                mountpoint: Some(PathBuf::from("/tmp/mount")),
                transport: TransportChoice::Nfs,
                quiet: true,
                verbose: true,
                read_only: true,
                driver: DriverChoice::SplitStore,
                database: Some(PathBuf::from("meta.db")),
                blocks: Some(PathBuf::from("blocks.db")),
                ..CliOptions::default()
            })
        );
    }

    #[test]
    fn extra_mountpoints_are_repeatable_without_replacing_the_primary() {
        let command = parse(&[
            "mount-rs",
            "mount",
            "/tmp/first",
            "--transport=nfs",
            "--also-mountpoint",
            "/tmp/second",
            "--also-mountpoint=/tmp/third",
        ]);
        let Command::Mount(options) = command else {
            panic!("expected mount command");
        };
        assert_eq!(options.mountpoint, Some(PathBuf::from("/tmp/first")));
        assert_eq!(
            options.also_mountpoints,
            vec![PathBuf::from("/tmp/second"), PathBuf::from("/tmp/third")]
        );
    }

    #[test]
    fn help_version_and_probe_are_mount_free_commands() {
        assert_eq!(parse(&["mount-rs", "--help"]), Command::Help);
        assert_eq!(parse(&["mount-rs", "-V"]), Command::Version);
        assert_eq!(parse(&["mount-rs", "probe"]), Command::Probe);
        assert_eq!(parse(&["mount-rs", "--probe"]), Command::Probe);
        assert_eq!(
            parse(&["mount-rs", "mount"]),
            Command::Mount(CliOptions::default())
        );
    }

    #[test]
    fn serve_http_requires_only_a_config_path() {
        assert_eq!(
            parse(&["mount-rs", "serve-http", "--config", "http.json"]),
            Command::ServeHttp(PathBuf::from("http.json"))
        );
        assert_eq!(
            parse(&["mount-rs", "serve-http", "--config=http.json"]),
            Command::ServeHttp(PathBuf::from("http.json"))
        );
        assert_eq!(parse(&["mount-rs", "serve-http", "--help"]), Command::Help);
        let error = parse_args(["mount-rs", "serve-http"]).unwrap_err();
        assert!(error.message().contains("requires --config"));
    }

    #[test]
    fn sdk_self_test_accepts_an_optional_config_and_reopen() {
        assert_eq!(
            parse(&["mount-rs", "sdk-self-test"]),
            Command::SdkSelfTest {
                config: None,
                reopen: false,
            }
        );
        assert_eq!(
            parse(&[
                "mount-rs",
                "sdk-self-test",
                "--config=state.json",
                "--reopen",
            ]),
            Command::SdkSelfTest {
                config: Some(PathBuf::from("state.json")),
                reopen: true,
            }
        );
        assert_eq!(
            parse(&["mount-rs", "sdk-self-test", "--help"]),
            Command::Help
        );
        let error = parse_args(["mount-rs", "sdk-self-test", "--reopen"]).unwrap_err();
        assert!(error.message().contains("requires --config"));
    }

    #[test]
    fn migration_requires_exact_config_and_revision_options() {
        assert_eq!(
            parse(&[
                "mount-rs",
                "migrate-concurrent-backing",
                "--config",
                "shared.json",
                "--expected-revision",
                "7",
            ]),
            Command::MigrateConcurrentBacking {
                config: PathBuf::from("shared.json"),
                expected_revision: 7,
            }
        );
        for invalid in [
            &["mount-rs", "migrate-concurrent-backing"][..],
            &[
                "mount-rs",
                "migrate-concurrent-backing",
                "--config",
                "shared.json",
            ][..],
            &[
                "mount-rs",
                "migrate-concurrent-backing",
                "--config",
                "shared.json",
                "--expected-revision",
                "not-a-number",
            ][..],
            &[
                "mount-rs",
                "migrate-concurrent-backing",
                "--config",
                "shared.json",
                "--expected-revision",
                "7",
                "--mountpoint",
                "/tmp/view",
            ][..],
        ] {
            assert!(
                parse_args(invalid.iter().copied()).is_err(),
                "accepted {invalid:?}"
            );
        }
    }

    #[test]
    fn reenrollment_requires_the_operator_assertion_and_expected_timeline() {
        let valid = [
            "mount-rs",
            "reenroll-sqlite-concurrent-backing",
            "--config",
            "shared.json",
            "--expected-revision",
            "7",
            "--expected-volume-id",
            "selected-volume",
            "--assert-all-writers-stopped-and-sole-metadata-copy",
        ];
        assert_eq!(
            parse(&valid),
            Command::ReenrollSqliteConcurrentBacking {
                config: PathBuf::from("shared.json"),
                expected_revision: 7,
                expected_volume_id: "selected-volume".into(),
            }
        );
        for (start, count) in [(2, 2), (4, 2), (6, 2), (8, 1)] {
            let mut incomplete = valid.to_vec();
            incomplete.drain(start..start + count);
            assert!(
                parse_args(incomplete).is_err(),
                "accepted missing option at {start}"
            );
        }
        for extra in [
            "--mountpoint=/tmp/view",
            "--config=other.json",
            "--expected-revision=8",
            "--expected-volume-id=other-volume",
            "--assert-all-writers-stopped-and-sole-metadata-copy=false",
        ] {
            let mut invalid = valid.to_vec();
            invalid.push(extra);
            assert!(parse_args(invalid).is_err(), "accepted {extra}");
        }
    }

    #[test]
    fn invalid_options_report_actionable_errors() {
        let error = parse_args(["mount-rs", "--transport", "bogus"]).unwrap_err();
        assert!(error.message().contains("unknown transport"));
        let error = parse_args(["mount-rs", "--driver", "sqlite"]).unwrap_err();
        assert!(error.message().contains("--database"));
        let error = parse_args(["mount-rs", "a", "b"]).unwrap_err();
        assert!(error.message().contains("one mountpoint"));
    }

    #[test]
    fn a_double_dash_keeps_a_mountpoint_that_starts_with_a_dash() {
        let command = parse(&["mount-rs", "--", "-mount"]);
        assert_eq!(
            command,
            Command::Mount(CliOptions {
                mountpoint: Some(PathBuf::from("-mount")),
                ..CliOptions::default()
            })
        );
    }

    #[test]
    fn help_is_plain_when_color_is_disabled() {
        let help = help_text(Color::disabled());
        assert!(help.contains("mount-rs"));
        assert!(!help.contains('\u{1b}'));
    }
}
