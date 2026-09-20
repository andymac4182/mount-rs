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

    fn parse(value: &str) -> Result<Self, ParseError> {
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

    fn parse(value: &str) -> Result<Self, ParseError> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliOptions {
    pub mountpoint: Option<PathBuf>,
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
}

impl Default for CliOptions {
    fn default() -> Self {
        Self {
            mountpoint: None,
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Mount(CliOptions),
    Probe,
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
                "quiet" => values.quiet = true,
                "verbose" => values.verbose = true,
                "read-only" => values.read_only = true,
                "empty" => values.empty = true,
                "allow-other" => values.allow_other = true,
                "sqlite-single-host" => values.sqlite_single_host = true,
                "mountpoint" => {
                    values.mountpoint = Some(PathBuf::from(value(name, inline_value, &mut args)?))
                }
                "transport" => {
                    values.transport =
                        TransportChoice::parse(&value(name, inline_value, &mut args)?)?
                }
                "driver" => {
                    values.driver = DriverChoice::parse(&value(name, inline_value, &mut args)?)?
                }
                "root" => values.root = Some(PathBuf::from(value(name, inline_value, &mut args)?)),
                "database" | "db" => {
                    values.database = Some(PathBuf::from(value(name, inline_value, &mut args)?))
                }
                "blocks" => {
                    values.blocks = Some(PathBuf::from(value(name, inline_value, &mut args)?))
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
    }
    validate_driver_options(&values)?;
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
            'q' => values.quiet = true,
            'v' => values.verbose = true,
            'r' => values.read_only = true,
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
                } else {
                    values.transport = TransportChoice::parse(&value)?;
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
    format!(
        "\n{} {}\n\n{}  mount-rs [mountpoint] [options]\n       mount-rs mount [mountpoint] [options]\n\n{}\n  -m, --mountpoint {}  where to mount {}\n  -t, --transport {}   auto | fuse | 9p | nfs {}\n      --sqlite-single-host  use the single-host SQLite NFS profile (nfs or auto)\n  -q, --quiet              do not log filesystem requests\n  -v, --verbose            log metadata polls too {}\n  -r, --read-only          mount read-only\n      --empty              start without the memory README\n      --allow-other        let other users see the FUSE mount\n      --driver {}    memory | host | sqlite | splitstore {}\n      --root {}      host driver root {}\n      --database {}  SQLite state/metadata database\n      --blocks {}    splitstore block database\n      --probe              print transport availability without mounting\n  -h, --help               this\n  -V, --version            print the version\n\n{}\n{}\n",
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
