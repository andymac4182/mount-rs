//! Runtime driver selection and native mount lifecycle for the mount-rs CLI.

use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use mount_rs_auto::{AutoMount, AutoMountError, AutoMountOptions, AutoTransport};
use mount_rs_core::{ErrorCode, FsDriver, FsError, Loopback, Result as FsResult};
use mount_rs_http::{
    DriveConfig as HttpDriveConfig, DriveRegistry, HttpServer, HttpServerError, HttpServerOptions,
};
#[cfg(feature = "observability")]
use mount_rs_observability::{Telemetry, set_global as set_global_telemetry};
use mount_rs_sdk::{
    Filesystem, FilesystemKind, HostOptions, MemoryOptions, SplitOptions, StoreConfig,
};

use crate::color::Color;
use crate::config::{
    ConfigError, DEFAULT_CHUNK_SIZE_BYTES, EnvReference, HttpServiceConfig, StorageProvider,
    load_config, redact_diagnostic, resolve_cli_options, unique_default_owner,
    validate_config_file, validate_owner,
};
use crate::parser::{
    CliOptions, Command, DriverChoice, ParseError, TransportChoice, help_text, parse_args,
    version_text,
};
use crate::stale::{stale_command_line, unmount_stale};
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

/// A Ctrl-C listener whose registration is completed before native mount
/// readiness can be reported. Tokio installs the process signal handler on the
/// first poll of `signal::ctrl_c`; merely constructing the future is not
/// sufficient for a caller that may receive SIGINT during mount startup.
struct CtrlCHandler {
    signal: Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>,
}

impl CtrlCHandler {
    async fn install() -> Result<Self, CliError> {
        let mut signal: Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>> =
            Box::pin(tokio::signal::ctrl_c());
        let received = poll_signal_registration(signal.as_mut()).await?;
        if received {
            return Err(CliError::runtime(
                "received SIGINT before native mount startup completed",
            ));
        }
        Ok(Self { signal })
    }

    async fn wait(&mut self) -> Result<(), CliError> {
        self.signal.as_mut().await.map_err(io_error)
    }
}

/// Poll a signal future exactly once. `false` means the listener returned
/// `Pending`, which is the registration acknowledgement; `true` means a
/// signal arrived during the initial poll and startup should stop.
async fn poll_signal_registration<F>(mut signal: Pin<&mut F>) -> Result<bool, CliError>
where
    F: Future<Output = std::io::Result<()>> + ?Sized,
{
    std::future::poll_fn(|context| {
        Poll::Ready(match signal.as_mut().poll(context) {
            Poll::Pending => Ok(false),
            Poll::Ready(result) => result.map(|()| true).map_err(io_error),
        })
    })
    .await
}

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
struct DriverRuntime {
    filesystem: Filesystem,
    #[cfg(feature = "observability")]
    telemetry: Telemetry,
}

impl DriverRuntime {
    async fn open(options: &CliOptions, uid: u32, gid: u32) -> Result<Self, CliError> {
        #[cfg(feature = "observability")]
        let telemetry = mount_rs_observability::global();
        let filesystem = match options.driver {
            DriverChoice::Memory => Filesystem::memory(MemoryOptions {
                uid,
                gid,
                ..MemoryOptions::default()
            }),
            DriverChoice::Host => {
                let root = options
                    .root
                    .as_deref()
                    .map(expand_path)
                    .unwrap_or(std::env::current_dir().map_err(io_error)?);
                Filesystem::host(
                    root,
                    HostOptions {
                        read_only: options.read_only,
                    },
                )
            }
            DriverChoice::Sqlite => {
                let database = options
                    .database
                    .as_deref()
                    .ok_or_else(|| CliError::usage("--driver sqlite requires --database <path>"))?;
                Filesystem::sqlite(expand_path(database)).await?
            }
            DriverChoice::SplitStore => {
                let split = split_options(options, uid, gid)?;
                Filesystem::split(split).await?
            }
        };

        if options.driver == DriverChoice::Sqlite {
            let driver = {
                #[cfg(feature = "observability")]
                {
                    filesystem.driver_with_telemetry(telemetry.clone())
                }
                #[cfg(not(feature = "observability"))]
                {
                    filesystem.driver()
                }
            };
            configure_sqlite_root_owner(&driver, options, uid, gid).await?;
        }
        Ok(Self {
            filesystem,
            #[cfg(feature = "observability")]
            telemetry,
        })
    }

    fn driver(&self) -> Arc<dyn FsDriver> {
        #[cfg(feature = "observability")]
        {
            self.filesystem
                .driver_with_telemetry(self.telemetry.clone())
        }
        #[cfg(not(feature = "observability"))]
        {
            self.filesystem.driver()
        }
    }

    async fn shutdown(&self) -> FsResult<()> {
        self.filesystem.shutdown().await
    }

    fn is_memory(&self) -> bool {
        self.filesystem.kind() == FilesystemKind::Memory
    }

    #[cfg(feature = "observability")]
    fn telemetry(&self) -> Telemetry {
        self.telemetry.clone()
    }
}

fn split_options(options: &CliOptions, uid: u32, gid: u32) -> Result<SplitOptions, CliError> {
    if let Some(storage) = &options.storage {
        if let Some(owner) = &storage.owner {
            validate_owner(owner)?;
        }
        return Ok(SplitOptions {
            metadata: sdk_store_config(&storage.metadata)?,
            blocks: sdk_store_config(&storage.blocks)?,
            chunk_size_bytes: storage.chunk_size_bytes,
            owner: storage.owner.clone().unwrap_or_else(unique_default_owner),
            uid,
            gid,
            umask: 0,
        });
    }

    let (metadata, blocks) = match (&options.database, &options.blocks) {
        (None, None) => (StoreConfig::Memory, StoreConfig::Memory),
        (Some(metadata), Some(blocks)) => (
            StoreConfig::Sqlite {
                path: expand_path(metadata),
            },
            StoreConfig::Sqlite {
                path: expand_path(blocks),
            },
        ),
        _ => {
            return Err(CliError::usage(
                "splitstore needs both --database <metadata.db> and --blocks <blocks.db>, or neither for volatile storage",
            ));
        }
    };
    Ok(SplitOptions {
        metadata,
        blocks,
        chunk_size_bytes: DEFAULT_CHUNK_SIZE_BYTES,
        owner: unique_default_owner(),
        uid,
        gid,
        umask: 0,
    })
}

fn sdk_store_config(provider: &StorageProvider) -> Result<StoreConfig, CliError> {
    match provider {
        StorageProvider::Memory => Ok(StoreConfig::Memory),
        StorageProvider::Sqlite { path } => Ok(StoreConfig::Sqlite { path: path.clone() }),
        StorageProvider::Pglite {
            connection,
            volume_key,
            durable,
        } => Ok(StoreConfig::Pglite {
            connection: resolve_storage_env(connection)?,
            volume_key: volume_key.clone(),
            durable: *durable,
        }),
        StorageProvider::Tidb {
            connection,
            volume_key,
            durable,
        } => Ok(StoreConfig::Tidb {
            connection: resolve_storage_env(connection)?,
            volume_key: volume_key.clone(),
            durable: *durable,
        }),
        StorageProvider::FoundationDb {
            cluster_file,
            volume_key,
            durable,
            lease_authority,
        } => Ok(StoreConfig::FoundationDb {
            cluster_file: cluster_file.clone(),
            volume_key: volume_key.clone(),
            durable: *durable,
            lease_authority: lease_authority.clone(),
        }),
        StorageProvider::R2 {
            endpoint,
            bucket,
            prefix,
            access_key_id,
            secret_access_key,
            durable,
        } => Ok(StoreConfig::R2 {
            endpoint: endpoint.clone(),
            bucket: bucket.clone(),
            prefix: prefix.clone(),
            access_key_id: resolve_storage_env(access_key_id)?,
            secret_access_key: resolve_storage_env(secret_access_key)?,
            durable: *durable,
        }),
        StorageProvider::AwsS3 {
            bucket,
            region,
            prefix,
            durable,
        } => Ok(StoreConfig::AwsS3 {
            bucket: bucket.clone(),
            region: region.clone(),
            prefix: prefix.clone(),
            durable: *durable,
        }),
    }
}

fn resolve_storage_env(reference: &EnvReference) -> Result<String, CliError> {
    std::env::var(&reference.name)
        .map_err(|_| CliError::runtime(format!("missing environment variable {}", reference.name)))
}

/// Configure the ownership metadata of the virtual SQLite root before a
/// native mount is created. This intentionally goes through FsDriver only:
/// the provider remains responsible for persistence, and the host SQLite
/// database file is never chmod/chowned by the CLI.
async fn configure_sqlite_root_owner(
    driver: &Arc<dyn FsDriver>,
    options: &CliOptions,
    uid: u32,
    gid: u32,
) -> Result<(), CliError> {
    let configured = match (options.root_uid, options.root_gid) {
        (Some(uid), Some(gid)) => Some((uid, gid)),
        (None, None) => None,
        _ => {
            return Err(CliError::usage(
                "sqlite root ownership requires both uid and gid",
            ));
        }
    };
    let stats = driver.stat("/").await?;
    let desired = configured.unwrap_or((uid, gid));
    if stats.uid == desired.0 && stats.gid == desired.1 {
        return Ok(());
    }

    if options.read_only {
        return Err(CliError::runtime(format!(
            "read-only SQLite mount would need virtual root ownership {}:{} -> {}:{}, refusing to mutate persisted metadata; use matching driver.uid and driver.gid or a writable explicit migration",
            stats.uid, stats.gid, desired.0, desired.1
        )));
    }
    if configured.is_none() {
        return Err(CliError::runtime(format!(
            "SQLite virtual root is owned by {}:{}, but this process requests {}:{}; set driver.uid and driver.gid for an explicit writable ownership migration",
            stats.uid, stats.gid, uid, gid
        )));
    }

    driver.chown("/", desired.0, desired.1).await?;
    Ok(())
}

/// Parse and execute the CLI. Only the mount command reaches native
/// transport code; help, version, and probe remain safe in ordinary tests.
pub async fn run<I, S>(args: I) -> Result<(), CliError>
where
    I: IntoIterator<Item = S>,
    S: Into<std::ffi::OsString>,
{
    #[cfg(feature = "observability")]
    initialize_telemetry();

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
        Command::SdkSelfTest { config, reopen } => {
            sdk_self_test_command(config.as_deref(), reopen).await
        }
        Command::ServeHttp(path) => serve_http_command(&path).await,
        Command::Mount(options) => {
            let options = resolve_cli_options(options)?;
            mount_command(options).await
        }
    }
}

#[cfg(feature = "observability")]
fn initialize_telemetry() {
    if mount_rs_observability::global().is_enabled() {
        return;
    }
    let telemetry = Telemetry::from_env("mount-rs-cli");
    if telemetry.is_enabled() {
        set_global_telemetry(telemetry);
    }
}

/// Exercise the public Rust SDK through the actual CLI binary without
/// requiring a native mount helper. This is intentionally a separate command
/// from the native mount lifecycle: it proves the CLI constructs a filesystem
/// through `mount-rs-sdk`, performs real driver I/O, and releases provider
/// resources before an optional durable reopen.
async fn sdk_self_test_command(config_path: Option<&Path>, reopen: bool) -> Result<(), CliError> {
    let raw = CliOptions {
        config: config_path.map(Path::to_owned),
        ..CliOptions::default()
    };
    let options = resolve_cli_options(raw)?;
    if reopen && !supports_sdk_reopen(&options) {
        return Err(CliError::usage(
            "sdk-self-test --reopen requires a durable host, sqlite, or splitstore provider",
        ));
    }

    let (uid, gid) = effective_identity();
    let runtime = DriverRuntime::open(&options, uid, gid).await?;
    let path = format!(
        "/.mount-rs-rust-sdk-self-test-{}-{}.bin",
        std::process::id(),
        unique_default_owner()
    );
    let expected = b"mount-rs Rust SDK CLI self-test payload\0";
    let patch = b"partial";
    let patch_offset = 3_u64;
    let final_length = expected.len() - 2;
    let mut expected_final = expected.to_vec();
    expected_final[patch_offset as usize..patch_offset as usize + patch.len()]
        .copy_from_slice(patch);
    expected_final.truncate(final_length);
    let view = Loopback::from_arc(runtime.driver());
    let result = async {
        view.write_file(&path, expected).await?;
        let handle = view.open(&path, "r+", 0o666).await?;
        let result = async {
            let written = handle.write(patch, Some(patch_offset)).await?;
            if written != patch.len() {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("sdk-self-test")
                    .with_message("Rust SDK CLI partial write length mismatch"));
            }
            handle.sync().await
        }
        .await;
        handle.close().await?;
        result?;
        view.truncate(&path, final_length as u64).await?;
        let actual = view.read_file(&path).await?;
        if actual != expected_final {
            return Err(FsError::new(ErrorCode::Eio)
                .with_syscall("sdk-self-test")
                .with_message("Rust SDK CLI partial/truncate readback mismatch"));
        }
        view.syncfs().await?;
        if !reopen {
            view.unlink(&path).await?;
            view.syncfs().await?;
        }
        Ok::<(), FsError>(())
    }
    .await;
    let shutdown = runtime.shutdown().await;
    result.map_err(CliError::from)?;
    shutdown.map_err(CliError::from)?;

    if reopen {
        let reopened = DriverRuntime::open(&options, uid, gid).await?;
        let reopened_view = Loopback::from_arc(reopened.driver());
        let result = async {
            let actual = reopened_view.read_file(&path).await?;
            if actual != expected_final {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("sdk-self-test")
                    .with_message("Rust SDK CLI reopen partial/truncate readback mismatch"));
            }
            reopened_view.unlink(&path).await?;
            reopened_view.syncfs().await
        }
        .await;
        let shutdown = reopened.shutdown().await;
        result.map_err(CliError::from)?;
        shutdown.map_err(CliError::from)?;
        println!(
            "sdk self-test passed: Rust SDK wrote, shut down, reopened, and read {} (driver={})",
            path,
            options.driver.as_str()
        );
    } else {
        println!(
            "sdk self-test passed: Rust SDK wrote and read {} (driver={})",
            path,
            options.driver.as_str()
        );
    }
    Ok(())
}

fn supports_sdk_reopen(options: &CliOptions) -> bool {
    match options.driver {
        DriverChoice::Memory => false,
        DriverChoice::Host | DriverChoice::Sqlite => true,
        DriverChoice::SplitStore => options.storage.as_deref().map_or(
            options.database.is_some() && options.blocks.is_some(),
            |storage| {
                !matches!(&storage.metadata, StorageProvider::Memory)
                    || !matches!(&storage.blocks, StorageProvider::Memory)
            },
        ),
    }
}

async fn serve_http_command(config_path: &Path) -> Result<(), CliError> {
    let spec = load_config(config_path)?;
    let http = spec.http.as_ref().ok_or_else(|| {
        CliError::usage(format!(
            "config {} does not contain an http section",
            config_path.display()
        ))
    })?;
    let (uid, gid) = effective_identity();
    #[cfg(feature = "observability")]
    let telemetry = mount_rs_observability::global();
    let mut runtimes = Vec::with_capacity(http.drives.len());
    let mut registry = DriveRegistry::new();

    for drive in &http.drives {
        let options = drive.driver.to_options();
        let runtime = match DriverRuntime::open(&options, uid, gid).await {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = shutdown_runtimes(&runtimes).await;
                return Err(error);
            }
        };
        runtimes.push(runtime);
        let token = match std::env::var(&drive.token.name) {
            Ok(token) => token,
            Err(_) => {
                let _ = shutdown_runtimes(&runtimes).await;
                return Err(CliError::runtime(format!(
                    "missing environment variable {}",
                    drive.token.name
                )));
            }
        };
        let configured = match HttpDriveConfig::new(
            drive.id.clone(),
            runtimes
                .last()
                .expect("HTTP runtime was pushed before configuration")
                .driver(),
            token,
        ) {
            Ok(configured) => configured,
            Err(error) => {
                let _ = shutdown_runtimes(&runtimes).await;
                return Err(CliError::usage(format!(
                    "invalid HTTP drive '{}': {error}",
                    drive.id
                )));
            }
        };
        if let Err(error) = registry.register(configured) {
            let _ = shutdown_runtimes(&runtimes).await;
            return Err(CliError::usage(format!(
                "invalid HTTP drive '{}': {error}",
                drive.id
            )));
        }
    }

    #[cfg(feature = "observability")]
    let server_options = http_server_options(http, telemetry);
    #[cfg(not(feature = "observability"))]
    let server_options = http_server_options(http);
    let mut ctrl_c = match CtrlCHandler::install().await {
        Ok(handler) => handler,
        Err(error) => {
            let _ = shutdown_runtimes(&runtimes).await;
            return Err(error);
        }
    };
    let server = match HttpServer::start(registry, server_options).await {
        Ok(server) => server,
        Err(error) => {
            let _ = shutdown_runtimes(&runtimes).await;
            return Err(http_error(error));
        }
    };

    let drive_ids = http
        .drives
        .iter()
        .map(|drive| drive.id.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    println!("http listening at {} (drives: {drive_ids})", server.url());
    println!("Press Ctrl-C to stop.");
    if let Err(error) = std::io::stdout().flush() {
        let _ = server.close().await;
        let _ = shutdown_runtimes(&runtimes).await;
        return Err(io_error(error));
    }

    let signal_result = ctrl_c.wait().await;
    let close_result = server.close().await.map_err(http_error);
    let shutdown_result = shutdown_runtimes(&runtimes).await.map_err(CliError::from);
    signal_result?;
    close_result?;
    shutdown_result?;
    println!("http stopped");
    Ok(())
}

fn http_server_options(
    config: &HttpServiceConfig,
    #[cfg(feature = "observability")] telemetry: Telemetry,
) -> HttpServerOptions {
    HttpServerOptions {
        host: config.host.clone(),
        port: config.port,
        max_request_bytes: config.max_request_bytes,
        max_response_bytes: config.max_response_bytes,
        max_directory_entries: config.max_directory_entries,
        read_chunk_bytes: config.read_chunk_bytes,
        drain_timeout: Duration::from_millis(config.drain_timeout_ms),
        max_connections: config.max_connections,
        request_timeout: Duration::from_millis(config.request_timeout_ms),
        #[cfg(feature = "observability")]
        telemetry,
    }
}

async fn shutdown_runtimes(runtimes: &[DriverRuntime]) -> FsResult<()> {
    let mut first_error = None;
    for runtime in runtimes.iter().rev() {
        if let Err(error) = runtime.shutdown().await
            && first_error.is_none()
        {
            first_error = Some(error);
        }
    }
    first_error.map_or(Ok(()), Err)
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
    #[cfg(feature = "observability")]
    let telemetry = runtime.telemetry();

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

    let mut ctrl_c = match CtrlCHandler::install().await {
        Ok(handler) => handler,
        Err(error) => {
            let _ = runtime.shutdown().await;
            return Err(error);
        }
    };

    let mount_future = mount_rs_auto::mount(watched, &mountpoint, mount_options);
    #[cfg(feature = "observability")]
    let mount_result = {
        let mount_path = mountpoint.to_string_lossy().into_owned();
        telemetry
            .observe_result("mount", "mount", Some(&mount_path), mount_future, |_| {
                Some("mount_error")
            })
            .await
    };
    #[cfg(not(feature = "observability"))]
    let mount_result = mount_future.await;
    let mounted = match mount_result {
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

    let lifecycle = wait_for_shutdown(&mounted, &mut ctrl_c).await;
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

async fn wait_for_shutdown(mounted: &AutoMount, ctrl_c: &mut CtrlCHandler) -> Result<(), CliError> {
    match mounted {
        AutoMount::Fuse { mount, .. } => {
            tokio::select! {
                result = wait_fuse_closed(mount.as_ref()) => result,
                signal = ctrl_c.wait() => {
                    signal?;
                    mounted.unmount().await.map_err(auto_error)
                }
            }
        }
        AutoMount::P9 { mount, .. } => {
            tokio::select! {
                () = mount.wait_closed() => Ok(()),
                signal = ctrl_c.wait() => {
                    signal?;
                    mounted.unmount().await.map_err(auto_error)
                }
            }
        }
        AutoMount::Nfs { .. } => loop {
            tokio::select! {
                signal = ctrl_c.wait() => {
                    signal?;
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

fn http_error(error: HttpServerError) -> CliError {
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
    use std::sync::atomic::{AtomicBool, Ordering};

    struct RegistrationProbe {
        registered: Arc<AtomicBool>,
    }

    impl Future for RegistrationProbe {
        type Output = std::io::Result<()>;

        fn poll(self: Pin<&mut Self>, context: &mut std::task::Context<'_>) -> Poll<Self::Output> {
            self.registered.store(true, Ordering::Release);
            context.waker().wake_by_ref();
            Poll::Pending
        }
    }

    #[tokio::test]
    async fn signal_registration_is_acknowledged_before_mount_start() {
        let registered = Arc::new(AtomicBool::new(false));
        let mut signal = Box::pin(RegistrationProbe {
            registered: Arc::clone(&registered),
        });

        assert!(!poll_signal_registration(signal.as_mut()).await.unwrap());
        assert!(registered.load(Ordering::Acquire));
    }

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

    #[tokio::test]
    async fn sqlite_root_owner_is_metadata_and_persists_across_reopen() {
        let database = std::env::temp_dir().join(format!(
            "mount-rs-cli-root-owner-{}.db",
            unique_default_owner()
        ));
        let options = CliOptions {
            driver: DriverChoice::Sqlite,
            database: Some(database.clone()),
            root_uid: Some(501),
            root_gid: Some(20),
            ..CliOptions::default()
        };

        let runtime = DriverRuntime::open(&options, 1000, 1000).await.unwrap();
        let root = runtime.driver().stat("/").await.unwrap();
        assert_eq!((root.uid, root.gid), (501, 20));
        runtime.shutdown().await.unwrap();
        drop(runtime);

        let reopened = DriverRuntime::open(&options, 2000, 2000).await.unwrap();
        let root = reopened.driver().stat("/").await.unwrap();
        assert_eq!((root.uid, root.gid), (501, 20));
        reopened.shutdown().await.unwrap();
        drop(reopened);
        let _ = std::fs::remove_file(database);
    }

    #[tokio::test]
    async fn sqlite_persisted_zero_root_is_not_implicitly_taken_or_readonly_migrated() {
        let database = std::env::temp_dir().join(format!(
            "mount-rs-cli-default-root-owner-{}.db",
            unique_default_owner()
        ));
        let initial = CliOptions {
            driver: DriverChoice::Sqlite,
            database: Some(database.clone()),
            root_uid: Some(0),
            root_gid: Some(0),
            ..CliOptions::default()
        };

        let runtime = DriverRuntime::open(&initial, 1000, 1001).await.unwrap();
        let handle = runtime
            .driver()
            .open("/sentinel", "w", 0o644)
            .await
            .unwrap();
        assert_eq!(handle.write(b"root-owned", Some(0)).await.unwrap(), 10);
        handle.close().await.unwrap();
        runtime.shutdown().await.unwrap();
        drop(runtime);

        let implicit = CliOptions {
            root_uid: None,
            root_gid: None,
            ..initial.clone()
        };
        let error = match DriverRuntime::open(&implicit, 2000, 2001).await {
            Ok(_) => panic!("a persisted 0:0 root must not be implicitly taken over"),
            Err(error) => error,
        };
        assert!(
            error
                .to_string()
                .contains("explicit writable ownership migration")
        );

        let read_only_mismatch = CliOptions {
            read_only: true,
            root_uid: Some(2000),
            root_gid: Some(2001),
            ..initial.clone()
        };
        let error = match DriverRuntime::open(&read_only_mismatch, 2000, 2001).await {
            Ok(_) => panic!("read-only ownership mismatch must not mutate persisted metadata"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("refusing to mutate"));

        let preserved = CliOptions {
            read_only: true,
            ..initial
        };
        let reopened = DriverRuntime::open(&preserved, 2000, 2001).await.unwrap();
        let root = reopened.driver().stat("/").await.unwrap();
        assert_eq!((root.uid, root.gid), (0, 0));
        let handle = reopened.driver().open("/sentinel", "r", 0).await.unwrap();
        let mut bytes = [0_u8; 10];
        assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), bytes.len());
        assert_eq!(&bytes, b"root-owned");
        handle.close().await.unwrap();
        reopened.shutdown().await.unwrap();
        drop(reopened);
        let _ = std::fs::remove_file(database);
    }
}
