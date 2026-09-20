//! Runtime driver selection and native mount lifecycle for the mount-rs CLI.

use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mount_rs_auto::{AutoMount, AutoMountError, AutoMountOptions, AutoTransport};
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::{ErrorCode, FsDriver, FsError, MemoryFs, MemoryOptions, Result as FsResult};
use mount_rs_host::{HostFs, HostFsOptions};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_sqlite::{SqliteBlockStore, SqliteFs, SqliteMetadataStore, open_sqlite};

use crate::color::Color;
use crate::config::{
    ConfigError, redact_diagnostic, resolve_cli_options, unique_default_owner,
    validate_config_file, validate_owner,
};
use crate::parser::{
    CliOptions, Command, DriverChoice, ParseError, TransportChoice, help_text, parse_args,
    version_text,
};
use crate::stale::{stale_command_line, unmount_stale};
use crate::storage::{ErasedBlockStore, ErasedMetadataStore, StorageResources, open_storage};
use crate::watch::{WatchOptions, watch_driver};

#[derive(Debug, Clone)]
pub struct CliError {
    code: u8,
    message: String,
}

impl CliError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: redact_diagnostic(&message.into()),
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: redact_diagnostic(&message.into()),
        }
    }

    pub const fn exit_code(&self) -> i32 {
        self.code as i32
    }
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

impl From<ParseError> for CliError {
    fn from(error: ParseError) -> Self {
        Self::usage(format!("{}\ntry --help", error.message()))
    }
}

impl From<ConfigError> for CliError {
    fn from(error: ConfigError) -> Self {
        Self::usage(format!("{}\ntry --help", error.message()))
    }
}

impl From<FsError> for CliError {
    fn from(error: FsError) -> Self {
        Self::runtime(error.to_string())
    }
}

#[derive(Clone)]
enum DriverRuntime {
    Memory(MemoryFs),
    Host(HostFs),
    Sqlite(SqliteFs),
    SplitMemory(ChunkedFs<MemoryMetadataStore, MemoryBlockStore>),
    SplitSqlite(ChunkedFs<SqliteMetadataStore, SqliteBlockStore>),
    SplitDynamic(
        ChunkedFs<ErasedMetadataStore, ErasedBlockStore>,
        StorageResources,
    ),
}

impl DriverRuntime {
    async fn open(options: &CliOptions, uid: u32, gid: u32) -> Result<Self, CliError> {
        match options.driver {
            DriverChoice::Memory => Ok(Self::Memory(MemoryFs::new(MemoryOptions {
                uid,
                gid,
                ..MemoryOptions::default()
            }))),
            DriverChoice::Host => {
                let root = options
                    .root
                    .as_deref()
                    .map(expand_path)
                    .unwrap_or(std::env::current_dir().map_err(io_error)?);
                Ok(Self::Host(HostFs::with_options(
                    root,
                    HostFsOptions {
                        read_only: options.read_only,
                    },
                )))
            }
            DriverChoice::Sqlite => {
                let database = options
                    .database
                    .as_deref()
                    .ok_or_else(|| CliError::usage("--driver sqlite requires --database <path>"))?;
                Ok(Self::Sqlite(open_sqlite(expand_path(database)).await?))
            }
            DriverChoice::SplitStore => {
                if let Some(storage) = &options.storage {
                    if let Some(owner) = &storage.owner {
                        validate_owner(owner)?;
                    }
                    let owner = storage.owner.clone().unwrap_or_else(unique_default_owner);
                    let chunk_options = ChunkedOptions::fixed(owner, storage.chunk_size_bytes)
                        .map_err(CliError::from)?
                        .with_identity(uid, gid, 0);
                    let opened = open_storage(storage).await.map_err(CliError::from)?;
                    let resources = opened.resources.clone();
                    return match ChunkedFs::open(opened.metadata, opened.blocks, chunk_options)
                        .await
                    {
                        Ok(driver) => Ok(Self::SplitDynamic(driver, resources)),
                        Err(error) => {
                            let _ = resources.close().await;
                            Err(CliError::from(error))
                        }
                    };
                }
                let chunk_options = ChunkedOptions::default().with_identity(uid, gid, 0);
                match (&options.database, &options.blocks) {
                    (None, None) => Ok(Self::SplitMemory(
                        ChunkedFs::open(
                            MemoryMetadataStore::new(),
                            MemoryBlockStore::new(),
                            chunk_options,
                        )
                        .await?,
                    )),
                    (Some(metadata), Some(blocks)) => Ok(Self::SplitSqlite(
                        ChunkedFs::open(
                            SqliteMetadataStore::open(expand_path(metadata))?,
                            SqliteBlockStore::open(expand_path(blocks))?,
                            chunk_options,
                        )
                        .await?,
                    )),
                    _ => Err(CliError::usage(
                        "splitstore needs both --database <metadata.db> and --blocks <blocks.db>, or neither for volatile storage",
                    )),
                }
            }
        }
    }

    fn driver(&self) -> Arc<dyn FsDriver> {
        match self {
            Self::Memory(driver) => Arc::new(driver.clone()),
            Self::Host(driver) => Arc::new(driver.clone()),
            Self::Sqlite(driver) => Arc::new(driver.clone()),
            Self::SplitMemory(driver) => Arc::new(driver.clone()),
            Self::SplitSqlite(driver) => Arc::new(driver.clone()),
            Self::SplitDynamic(driver, _) => Arc::new(driver.clone()),
        }
    }

    async fn shutdown(&self) -> FsResult<()> {
        match self {
            Self::SplitMemory(driver) => driver.shutdown().await,
            Self::SplitSqlite(driver) => driver.shutdown().await,
            Self::SplitDynamic(driver, resources) => {
                let driver_result = driver.shutdown().await;
                let resources_result = resources.close().await;
                driver_result.and(resources_result)
            }
            Self::Memory(_) | Self::Host(_) | Self::Sqlite(_) => Ok(()),
        }
    }

    fn is_memory(&self) -> bool {
        matches!(self, Self::Memory(_))
    }
}

/// Parse and execute the CLI. Only the mount command reaches native
/// transport code; help, version, and probe remain safe in ordinary tests.
pub async fn run<I, S>(args: I) -> Result<(), CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString>,
{
    match parse_args(args)? {
        Command::Help => {
            println!("{}", help_text(Color::from_env()));
            Ok(())
        }
        Command::Version => {
            println!("{}", version_text());
            Ok(())
        }
        Command::Probe => {
            println!(
                "{}",
                render_probe(&mount_rs_auto::probe_transports(), Color::from_env())
            );
            Ok(())
        }
        Command::ValidateConfig(path) => {
            validate_config_file(&path)?;
            println!("valid config: {}", path.display());
            Ok(())
        }
        Command::Mount(options) => {
            let options = resolve_cli_options(options)?;
            mount_command(options).await
        }
    }
}

async fn mount_command(options: CliOptions) -> Result<(), CliError> {
    if options.transport == TransportChoice::Auto {
        let probe = mount_rs_auto::probe_transports();
        if probe.chosen.is_none() {
            return Err(CliError::runtime(
                probe
                    .reason
                    .unwrap_or_else(|| "no native transport is usable".to_owned()),
            ));
        }
    }
    let mountpoint = resolve_mountpoint(options.mountpoint.as_deref())?;
    let (uid, gid) = effective_identity();
    let runtime = DriverRuntime::open(&options, uid, gid).await?;

    let bare_driver = runtime.driver();
    if !options.empty && runtime.is_memory() {
        seed_readme(Arc::clone(&bare_driver)).await?;
    }

    let color = Color::from_env();
    let watched = watch_driver(
        bare_driver,
        WatchOptions::new(
            !options.quiet && options.verbose,
            Color::from_env(),
            !options.quiet,
        ),
    );
    // Match mountx: stale cleanup is scoped to the one mountpoint this start
    // command resolved, and runs immediately before creating that directory.
    unmount_stale(&mountpoint, current_uid(), color).await;
    std::fs::create_dir_all(&mountpoint).map_err(io_error)?;
    let mount_options = AutoMountOptions {
        transport: options.transport.into(),
        read_only: Some(options.read_only),
        nfs: sqlite_single_host_nfs_options(&options),
        fuse: Some(mount_rs_auto::MountOptions {
            allow_other: options.allow_other || uid == 0,
            ..mount_rs_auto::MountOptions::default()
        }),
        ..AutoMountOptions::default()
    };

    let mounted = match mount_rs_auto::mount(watched, &mountpoint, mount_options).await {
        Ok(mounted) => mounted,
        Err(error) => {
            let _ = runtime.shutdown().await;
            return Err(auto_error(error));
        }
    };

    println!(
        "{} {} at {} (source: {})",
        color.green("mounted"),
        transport_name(mounted.transport()),
        mounted.mountpoint().display(),
        mounted.source().unwrap_or("transport-managed")
    );
    println!(
        "From another terminal: ls -l {}",
        mounted.mountpoint().display()
    );
    if !options.read_only {
        println!(
            "Press Ctrl-C to unmount; requests are {}.",
            if options.quiet {
                "not logged"
            } else {
                "logged below"
            }
        );
    } else {
        println!("Press Ctrl-C to unmount; the mounted view is read-only.");
    }
    if let Some(command) = stale_command_line(
        mounted.mountpoint(),
        transport_name(mounted.transport()),
        current_uid(),
    ) {
        println!(
            "If this process exits without unmounting, clear it with {}.",
            color.bold(command)
        );
    }

    let lifecycle = wait_for_shutdown(&mounted).await;
    let shutdown = runtime.shutdown().await.map_err(CliError::from);
    lifecycle?;
    shutdown?;
    println!("{}", color.yellow("unmounted"));
    if let Some(stats) = session_stats(&mounted) {
        println!("  {}", color.dim(stats));
    }
    Ok(())
}

fn sqlite_single_host_nfs_options(options: &CliOptions) -> Option<mount_rs_nfs::NfsMountOptions> {
    if !options.sqlite_single_host {
        return None;
    }

    let mut nfs = mount_rs_nfs::NfsMountOptions::sqlite_single_host();
    // A transport-specific override is complete, so carry the CLI's common
    // read-only setting into the profile instead of relying on AutoMount's
    // shared-field merge.
    nfs.read_only = options.read_only;
    Some(nfs)
}

fn session_stats(mounted: &AutoMount) -> Option<String> {
    match mounted {
        AutoMount::Fuse { .. } => Some(
            "session stats unavailable: mount-rs-fuse does not expose FUSE counters".to_owned(),
        ),
        AutoMount::P9 { mount, .. } => {
            let stats = mount.connection.session.stats();
            Some(format!(
                "session stats: requests={} replies={} errors={} dropped={} flushed={}",
                stats.requests, stats.replies, stats.errors, stats.dropped, stats.flushed
            ))
        }
        // This follows the upstream CLI: NFS has no client-session close
        // equivalent to the FUSE/9P connection lifecycle.
        AutoMount::Nfs { .. } => None,
    }
}

async fn seed_readme(driver: Arc<dyn FsDriver>) -> Result<(), CliError> {
    const README: &str = include_str!("../../../README.md");
    let handle = driver.open("/README.md", "w", 0o644).await?;
    let result = async {
        let mut written = 0_usize;
        while written < README.len() {
            let count = handle
                .write(
                    README.as_bytes().get(written..).unwrap_or_default(),
                    Some(written as u64),
                )
                .await?;
            if count == 0 || count > README.len() - written {
                return Err(FsError::new(ErrorCode::Eio).with_syscall("write"));
            }
            written += count;
        }
        Ok(())
    }
    .await;
    let close = handle.close().await;
    result.and(close).map_err(CliError::from)
}

async fn wait_for_shutdown(mounted: &AutoMount) -> Result<(), CliError> {
    match mounted {
        AutoMount::Fuse { mount, .. } => {
            tokio::select! {
                result = wait_fuse_closed(mount.as_ref()) => result,
                signal = tokio::signal::ctrl_c() => {
                    signal.map_err(io_error)?;
                    mounted.unmount().await.map_err(auto_error)
                }
            }
        }
        AutoMount::P9 { mount, .. } => {
            tokio::select! {
                () = mount.wait_closed() => Ok(()),
                signal = tokio::signal::ctrl_c() => {
                    signal.map_err(io_error)?;
                    mounted.unmount().await.map_err(auto_error)
                }
            }
        }
        AutoMount::Nfs { .. } => loop {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => {
                    signal.map_err(io_error)?;
                    mounted.unmount().await.map_err(auto_error)?;
                    return Ok(())
                }
                () = tokio::time::sleep(Duration::from_millis(250)) => {
                    if !mounted.active() {
                        return Ok(())
                    }
                }
            }
        },
    }
}

async fn wait_fuse_closed(mount: &mount_rs_auto::FuseMount) -> Result<(), CliError> {
    #[cfg(target_os = "linux")]
    {
        mount.wait_closed().await;
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = mount;
        Err(CliError::runtime(
            "the FUSE lifecycle is unavailable on this platform",
        ))
    }
}

fn render_probe(probe: &mount_rs_auto::AutoProbe, color: Color) -> String {
    let selected = probe.chosen.map(transport_name).unwrap_or("none");
    let mut output = format!(
        "platform: {}\npreference: {}\nchosen: {}\n",
        probe.platform,
        probe
            .preference
            .iter()
            .map(|transport| transport_name(*transport))
            .collect::<Vec<_>>()
            .join(", "),
        if probe.chosen.is_some() {
            color.green(selected).to_string()
        } else {
            color.red(selected).to_string()
        }
    );
    for (name, value) in [
        ("fuse", &probe.fuse),
        ("9p", &probe.p9),
        ("nfs", &probe.nfs),
    ] {
        let state = if value.usable {
            color.green("usable").to_string()
        } else {
            format!(
                "{} ({})",
                color.red("unavailable"),
                value.reason.as_deref().unwrap_or("no reason reported")
            )
        };
        output.push_str(&format!("{name}: {state}\n"));
    }
    if let Some(reason) = &probe.reason {
        output.push_str(&format!("reason: {}\n", color.red(reason)));
    }
    output
}

fn auto_error(error: AutoMountError) -> CliError {
    CliError::runtime(error.to_string())
}

fn io_error(error: std::io::Error) -> CliError {
    CliError::runtime(error.to_string())
}

fn resolve_mountpoint(requested: Option<&Path>) -> Result<PathBuf, CliError> {
    let path = requested
        .map(Path::to_owned)
        .or_else(|| std::env::var_os("MOUNT_RS_MOUNTPOINT").map(PathBuf::from))
        .or_else(|| std::env::var_os("MOUNTX_MOUNTPOINT").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("~/mountx"));
    Ok(expand_path(&path))
}

fn expand_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text == "~" {
        return home_directory();
    }
    if let Some(rest) = text.strip_prefix("~/") {
        return home_directory().join(rest);
    }
    if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn home_directory() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn effective_identity() -> (u32, u32) {
    let uid = std::env::var("SUDO_UID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(current_uid);
    let gid = std::env::var("SUDO_GID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(current_gid);
    (uid, gid)
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid has no pointer arguments or retained state.
    unsafe { libc::getuid() as u32 }
}

#[cfg(not(unix))]
const fn current_uid() -> u32 {
    0
}

#[cfg(unix)]
fn current_gid() -> u32 {
    // SAFETY: getgid has no pointer arguments or retained state.
    unsafe { libc::getgid() as u32 }
}

#[cfg(not(unix))]
const fn current_gid() -> u32 {
    0
}

fn transport_name(transport: mount_rs_auto::Transport) -> &'static str {
    match transport {
        mount_rs_auto::Transport::Fuse => "fuse",
        mount_rs_auto::Transport::P9 => "9p",
        mount_rs_auto::Transport::Nfs => "nfs",
    }
}

impl From<TransportChoice> for AutoTransport {
    fn from(value: TransportChoice) -> Self {
        match value {
            TransportChoice::Auto => Self::Auto,
            TransportChoice::Fuse => Self::Fuse,
            TransportChoice::P9 => Self::P9,
            TransportChoice::Nfs => Self::Nfs,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SplitStorageConfig, StorageProvider};
    use crate::parser::parse_args;

    #[test]
    fn mountpoint_expansion_is_cross_platform_and_shell_like() {
        let home = home_directory();
        assert_eq!(expand_path(Path::new("~")), home);
        assert_eq!(expand_path(Path::new("~/mount-rs")), home.join("mount-rs"));
        assert!(expand_path(Path::new("relative")).is_absolute());
    }

    #[test]
    fn probe_rendering_reports_each_transport_without_mounting() {
        let probe = mount_rs_auto::probe_transports_for("darwin");
        let rendered = render_probe(&probe, Color::disabled());
        assert!(rendered.contains("platform: darwin"));
        assert!(rendered.contains("fuse:"));
        assert!(rendered.contains("9p:"));
        assert!(rendered.contains("nfs:"));
        assert!(!rendered.contains('\u{1b}'));
    }

    #[test]
    fn probe_and_help_parse_before_any_driver_is_opened() {
        assert!(matches!(
            parse_args(["mount-rs", "probe"]),
            Ok(Command::Probe)
        ));
        assert!(matches!(
            parse_args(["mount-rs", "--help"]),
            Ok(Command::Help)
        ));
    }

    #[test]
    fn driver_labels_are_explicit() {
        assert_eq!(DriverChoice::SplitStore.as_str(), "splitstore");
        assert_eq!(TransportChoice::P9.as_str(), "9p");
    }

    #[test]
    fn sqlite_single_host_profile_is_an_nfs_only_override() {
        let options = CliOptions {
            read_only: true,
            sqlite_single_host: true,
            ..CliOptions::default()
        };
        let nfs = sqlite_single_host_nfs_options(&options).expect("profile override");
        assert!(nfs.hard);
        assert!(nfs.read_only);
        assert_eq!(nfs.version, mount_rs_nfs::NfsVersion::V3);
        let rendered =
            mount_rs_nfs::nfs_mount_options(2049, &nfs, mount_rs_nfs::NfsPlatform::Linux)
                .expect("profile renders");
        assert!(rendered.contains("local_lock=all"));
        assert!(rendered.contains("hard"));
    }

    #[test]
    fn sqlite_single_host_profile_is_omitted_by_default() {
        assert!(sqlite_single_host_nfs_options(&CliOptions::default()).is_none());
    }

    #[tokio::test]
    async fn memory_and_splitstore_driver_choices_are_live_filesystems() {
        let memory_options = CliOptions::default();
        let memory = DriverRuntime::open(&memory_options, 1000, 1000)
            .await
            .unwrap();
        let memory_driver = memory.driver();
        assert!(memory_driver.stat("/").await.unwrap().is_directory());
        seed_readme(Arc::clone(&memory_driver)).await.unwrap();
        assert!(memory_driver.stat("/README.md").await.unwrap().is_file());
        memory.shutdown().await.unwrap();

        let split_options = CliOptions {
            driver: DriverChoice::SplitStore,
            ..CliOptions::default()
        };
        let split = DriverRuntime::open(&split_options, 1000, 1000)
            .await
            .unwrap();
        let split_driver = split.driver();
        let handle = split_driver.open("/data", "w", 0o644).await.unwrap();
        assert_eq!(handle.write(b"split", Some(0)).await.unwrap(), 5);
        handle.close().await.unwrap();
        assert_eq!(split_driver.stat("/data").await.unwrap().size, 5);
        split.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn structured_memory_storage_is_a_live_provider_composition() {
        let options = CliOptions {
            driver: DriverChoice::SplitStore,
            storage: Some(Box::new(SplitStorageConfig {
                metadata: StorageProvider::Memory,
                blocks: StorageProvider::Memory,
                chunk_size_bytes: 4096,
                owner: Some("runtime-test-owner".to_owned()),
            })),
            ..CliOptions::default()
        };
        let runtime = DriverRuntime::open(&options, 1000, 1000).await.unwrap();
        let driver = runtime.driver();
        let handle = driver.open("/config", "w", 0o644).await.unwrap();
        assert_eq!(handle.write(b"config", Some(0)).await.unwrap(), 6);
        handle.close().await.unwrap();
        let handle = driver.open("/config", "r", 0).await.unwrap();
        let mut bytes = [0_u8; 6];
        assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 6);
        assert_eq!(&bytes, b"config");
        handle.close().await.unwrap();
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn structured_sqlite_storage_is_a_live_provider_composition() {
        let stem = format!("mount-rs-cli-storage-{}", unique_default_owner());
        let metadata_path = std::env::temp_dir().join(format!("{stem}-metadata.db"));
        let blocks_path = std::env::temp_dir().join(format!("{stem}-blocks.db"));
        let options = CliOptions {
            driver: DriverChoice::SplitStore,
            storage: Some(Box::new(SplitStorageConfig {
                metadata: StorageProvider::Sqlite {
                    path: metadata_path.clone(),
                },
                blocks: StorageProvider::Sqlite {
                    path: blocks_path.clone(),
                },
                chunk_size_bytes: 4096,
                owner: None,
            })),
            ..CliOptions::default()
        };
        let runtime = DriverRuntime::open(&options, 1000, 1000).await.unwrap();
        let driver = runtime.driver();
        let handle = driver.open("/sqlite", "w", 0o644).await.unwrap();
        assert_eq!(handle.write(b"sqlite", Some(0)).await.unwrap(), 6);
        handle.close().await.unwrap();
        runtime.shutdown().await.unwrap();
        let _ = std::fs::remove_file(metadata_path);
        let _ = std::fs::remove_file(blocks_path);
    }
}
