//! Remote service bootstrap and provider configuration.
#[path = "remote_diagnostics.rs"]
mod diagnostics;

use crate::remote_runtime::{CliRuntimeConstructor, RemoteRuntimeKeeper, RemoteRuntimeLifecycle};
use crate::{
    CliError,
    config::parse_config_str,
    runtime::{
        CtrlCHandler, DriverRuntime, effective_identity, prepare_mountpoints_before_driver,
        retry_unmount, wait_for_multiple_nfs_shutdown,
    },
};
use mount_rs_core::FsDriver;
use mount_rs_service::catalog::{CatalogSnapshot, SqliteCatalog};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    ffi::OsString,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ServiceConfig {
    version: u32,
    catalog: PathBuf,
    listen: SocketAddr,
    #[serde(default)]
    websocket_listen: Option<SocketAddr>,
    certificate: PathBuf,
    private_key: PathBuf,
    #[serde(default = "default_connection_limit")]
    max_connections: usize,
    #[serde(default)]
    max_active_drives: Option<usize>,
    #[serde(default = "default_tidb_pool_max_connections")]
    tidb_pool_max_connections: usize,
    #[serde(default)]
    cache: Option<crate::server_cache::CacheServiceConfig>,
    #[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
    #[serde(default)]
    local_oidc_fixture: Option<LocalOidcFixtureConfig>,
}

#[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LocalOidcFixtureConfig {
    issuer: String,
    audiences: Vec<String>,
    jwks: PathBuf,
}

#[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
struct LocalOidcFixtureKeySource {
    issuer: String,
    audiences: Vec<String>,
    verifier: mount_rs_service::auth::OidcVerifier,
    rejection: mount_rs_service::auth::AuthError,
}

#[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
#[async_trait::async_trait]
impl mount_rs_service::auth::OidcKeySource for LocalOidcFixtureKeySource {
    async fn fetch(
        &self,
        issuer: &str,
        audiences: &[String],
    ) -> Result<mount_rs_service::auth::OidcVerifier, mount_rs_service::auth::AuthError> {
        if issuer != self.issuer || audiences != self.audiences {
            return Err(self.rejection.clone());
        }
        Ok(self.verifier.clone())
    }
}
fn default_tidb_pool_max_connections() -> usize {
    16
}
fn default_connection_limit() -> usize {
    mount_rs_service::server::RemoteServerOptions::default().max_connections
}

#[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
fn local_oidc_fixture_key_source(
    config: &ServiceConfig,
    path: &Path,
) -> Result<Option<Arc<dyn mount_rs_service::auth::OidcKeySource>>, CliError> {
    let Some(fixture) = &config.local_oidc_fixture else {
        return Ok(None);
    };
    if !config.listen.ip().is_loopback()
        || config
            .websocket_listen
            .is_some_and(|address| !address.ip().is_loopback())
    {
        return Err(CliError::usage(
            "local OIDC fixture requires every remote listener to use a loopback address",
        ));
    }
    let jwks_path = relative(path, &fixture.jwks);
    let metadata = std::fs::metadata(&jwks_path)
        .map_err(|_| CliError::runtime("cannot read local OIDC fixture JWKS"))?;
    if metadata.len() > 256 * 1024 {
        return Err(CliError::usage("local OIDC fixture JWKS is too large"));
    }
    let jwks = std::fs::read_to_string(jwks_path)
        .map_err(|_| CliError::runtime("cannot read local OIDC fixture JWKS"))?;
    let verifier = mount_rs_service::auth::OidcVerifier::from_jwks_json(
        &fixture.issuer,
        &fixture.audiences,
        &jwks,
    )
    .map_err(|_| CliError::usage("invalid local OIDC fixture policy or JWKS"))?;
    let rejection = mount_rs_service::auth::OidcVerifier::new("", &[], Vec::new())
        .expect_err("empty OIDC fixture policy is invalid");
    Ok(Some(Arc::new(LocalOidcFixtureKeySource {
        issuer: fixture.issuer.clone(),
        audiences: fixture.audiences.clone(),
        verifier,
        rejection,
    })))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyConfig {
    version: u32,
    catalog: PathBuf,
    document: PathBuf,
    expected_revision: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MountConfig {
    version: u32,
    driver: RemoteProvider,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    read_only: bool,
    #[serde(default)]
    allow_other: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RemoteProvider {
    kind: String,
    endpoint: SocketAddr,
    #[serde(default)]
    connection_transport: ConnectionMode,
    #[serde(default)]
    websocket_endpoint: Option<SocketAddr>,
    server_name: String,
    partition: String,
    credentials: Credentials,
    #[serde(default)]
    ca_certificate: Option<PathBuf>,
    mounts: Vec<DriveMount>,
}
#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConnectionMode {
    #[default]
    Quic,
    Websocket,
    Auto,
}
impl RemoteProvider {
    fn connection_selection(
        &self,
    ) -> Result<mount_rs_remote_client::connection::ConnectionTransport, CliError> {
        use mount_rs_remote_client::connection::ConnectionTransport;
        Ok(match self.connection_transport {
            ConnectionMode::Quic => ConnectionTransport::Quic,
            ConnectionMode::Websocket => ConnectionTransport::WebSocket(
                self.websocket_endpoint
                    .ok_or_else(|| CliError::usage("websocket_endpoint is required"))?,
            ),
            ConnectionMode::Auto => ConnectionTransport::Auto {
                websocket: self
                    .websocket_endpoint
                    .ok_or_else(|| CliError::usage("websocket_endpoint is required"))?,
            },
        })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DriveMount {
    drive: String,
    mountpoint: PathBuf,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Credentials {
    File(PathBuf),
    Command(Vec<String>),
}

fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T, CliError> {
    let bytes = std::fs::read(path).map_err(|_| CliError::runtime("cannot read configuration"))?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(CliError::usage("configuration too large"));
    }
    serde_json::from_slice(&bytes).map_err(|_| CliError::usage("invalid remote configuration"))
}
fn relative(config: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        config.parent().unwrap_or(Path::new(".")).join(path)
    }
}
fn version(value: u32) -> Result<(), CliError> {
    if value == 1 {
        Ok(())
    } else {
        Err(CliError::usage("unsupported configuration version"))
    }
}
fn id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_ .".contains(&b) && b != b' ')
}
fn validate_mount(config: &MountConfig) -> Result<(), CliError> {
    version(config.version)?;
    let provider = &config.driver;
    provider.connection_selection()?;
    if provider.kind != "remote"
        || !id(&provider.partition)
        || provider.server_name.is_empty()
        || provider.mounts.is_empty()
        || provider.mounts.len() > 64
    {
        return Err(CliError::usage("invalid remote provider"));
    }
    let mut drives = BTreeSet::new();
    let mut paths = BTreeSet::new();
    for mount in &provider.mounts {
        if !id(&mount.drive) || !drives.insert(&mount.drive) || !paths.insert(&mount.mountpoint) {
            return Err(CliError::usage("duplicate or invalid remote mount"));
        }
    }
    if let Credentials::Command(argv) = &provider.credentials
        && (argv.is_empty() || argv[0].is_empty())
    {
        return Err(CliError::usage("empty credential command"));
    }
    Ok(())
}

pub(crate) async fn apply(path: &Path) -> Result<(), CliError> {
    let config: ApplyConfig = read(path)?;
    version(config.version)?;
    let document_path = relative(path, &config.document);
    let mut document: CatalogSnapshot = read(&document_path)?;
    let document_base = std::fs::canonicalize(document_path.parent().unwrap_or(Path::new(".")))
        .map_err(|_| CliError::runtime("cannot resolve catalog document directory"))?;
    // Reuse the normal CLI backend parser for every independent Drive.
    for partition in document.partitions.values_mut() {
        for drive in partition.drives.values_mut() {
            resolve_catalog_driver(&mut drive.driver, &document_base)?;
        }
    }
    let catalog = SqliteCatalog::open(relative(path, &config.catalog))
        .await
        .map_err(|_| CliError::runtime("cannot open service catalog"))?;
    let revision = catalog
        .compare_and_swap(config.expected_revision, document)
        .await
        .map_err(|_| CliError::runtime("catalog validation or revision conflict"))?;
    println!("catalog revision {revision}");
    Ok(())
}

pub(crate) async fn serve(path: &Path) -> Result<(), CliError> {
    let startup = mount_rs_service::startup::Startup::new_lazy(
        diagnostics::enabled(std::env::var_os("MOUNT_RS_PROFILE_IO").as_deref()),
        mount_rs_service::startup::Identity::cli(),
    );
    #[cfg(not(test))]
    static KEEPER: std::sync::OnceLock<Arc<RemoteRuntimeKeeper>> = std::sync::OnceLock::new();
    #[cfg(not(test))]
    let keeper = KEEPER
        .get_or_init(|| Arc::new(RemoteRuntimeKeeper::default()))
        .clone();
    #[cfg(test)]
    let keeper = Arc::new(RemoteRuntimeKeeper::default());
    serve_observed(
        path,
        &startup,
        &mut |snapshot| {
            mount_rs_service::startup::Startup::write_record(
                &mut std::io::stderr().lock(),
                snapshot,
            )
        },
        &keeper,
    )
    .await
}

#[derive(Default)]
struct RemoteObservers {
    quic: Option<mount_rs_service::server::ServerDiagnostics>,
    websocket: Option<mount_rs_service::websocket::WebSocketDiagnostics>,
}

async fn serve_observed(
    path: &Path,
    startup: &mount_rs_service::startup::Startup,
    sink: &mut impl FnMut(&mount_rs_service::startup::Snapshot) -> std::io::Result<()>,
    keeper: &Arc<RemoteRuntimeKeeper>,
) -> Result<(), CliError> {
    let scope = keeper.reserve().map_err(CliError::from)?;
    let mut observers = RemoteObservers::default();
    startup.publish(sink);
    let result = serve_resources(path, startup, sink, &scope.0, &mut observers).await;
    startup.finish_startup(result.is_ok());
    if result.is_err()
        && startup
            .snapshot()
            .is_some_and(|snapshot| snapshot.cleanup_outcome.is_none())
    {
        let _ = startup.write_banks(&mut std::io::stderr().lock());
    }
    let cleanup = startup.begin(mount_rs_service::startup::Stage::Cleanup);
    let shutdown = scope.0.close().await;
    cleanup.finish(shutdown.is_ok());
    startup.finish_cleanup(shutdown.is_ok());
    startup.publish(sink);
    if let Some(observer) = observers.quic {
        diagnostics::emit(&observer);
    }
    if let Some(observer) = observers.websocket {
        diagnostics::emit_websocket(&observer);
    }
    result.and(shutdown.map_err(CliError::from))
}

async fn serve_resources(
    path: &Path,
    startup: &mount_rs_service::startup::Startup,
    sink: &mut impl FnMut(&mount_rs_service::startup::Snapshot) -> std::io::Result<()>,
    lifecycle: &Arc<RemoteRuntimeLifecycle>,
    observers: &mut RemoteObservers,
) -> Result<(), CliError> {
    use mount_rs_service::startup::Stage;
    let configuration = startup
        .observe(
            Stage::Configuration,
            async {
                let config: ServiceConfig = read(path)?;
                version(config.version)?;
                let server_options = mount_rs_service::server::RemoteServerOptions {
                    max_connections: config.max_connections,
                };
                server_options.validate().map_err(CliError::usage)?;
                if config.max_active_drives == Some(0) {
                    return Err(CliError::usage("max_active_drives must be positive"));
                }
                if let Some(cache) = &config.cache {
                    cache.validate()?;
                }
                let diagnostic_interval = diagnostics::diagnostic_interval(
                    startup.enabled(),
                    std::env::var_os("MOUNT_RS_DIAGNOSTIC_INTERVAL_MS").as_deref(),
                )?;
                Ok::<_, CliError>((config, server_options, diagnostic_interval))
            },
            sink,
        )
        .await?;
    let (config, server_options, diagnostic_interval) = configuration;
    #[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
    let local_oidc_fixture = startup
        .observe(
            Stage::Configuration,
            async { local_oidc_fixture_key_source(&config, path) },
            sink,
        )
        .await?;
    let context = startup
        .observe(
            Stage::Configuration,
            async {
                if config.cache.is_some() {
                    mount_rs_sdk::StorageContext::new_with_raw_cache_limits(
                        config.tidb_pool_max_connections,
                        0,
                        0,
                    )
                } else {
                    mount_rs_sdk::StorageContext::new(config.tidb_pool_max_connections)
                }
            },
            sink,
        )
        .await?;
    lifecycle
        .install_context(context.clone())
        .map_err(CliError::from)?;
    let catalog = Arc::new(
        startup
            .observe(
                Stage::CatalogOpen,
                async {
                    SqliteCatalog::open(relative(path, &config.catalog))
                        .await
                        .map_err(|_| CliError::runtime("cannot open service catalog"))
                },
                sink,
            )
            .await?,
    );
    let snapshot = startup
        .observe(
            Stage::CatalogLoad,
            async {
                catalog
                    .load_current()
                    .await
                    .map_err(|_| CliError::runtime("cannot load service catalog"))
            },
            sink,
        )
        .await?;
    let drive_count = snapshot
        .partitions
        .values()
        .try_fold(0usize, |total, partition| {
            total.checked_add(partition.drives.len())
        })
        .ok_or_else(|| CliError::usage("Drive registration count overflow"))?;
    let capacity = config.max_active_drives.unwrap_or(drive_count.max(1));
    startup
        .plan_lazy(
            snapshot.partitions.len() as u64,
            drive_count as u64,
            capacity as u64,
        )
        .map_err(|_| CliError::usage("invalid lazy Drive plan"))?;
    let runtime_diagnostics = if startup.enabled() {
        mount_rs_service::runtime_diagnostics::RuntimeDiagnostics::new(false)
    } else {
        mount_rs_service::runtime_diagnostics::RuntimeDiagnostics::default()
    };
    let pool = mount_rs_service::runtime_pool::RuntimePool::with_diagnostics(
        capacity,
        runtime_diagnostics,
    )
    .map_err(CliError::from)?;
    lifecycle.install_pool(pool.clone());
    let (certs, key) = startup
        .observe(
            Stage::TlsMaterial,
            async {
                let cert_bytes = std::fs::read(relative(path, &config.certificate))
                    .map_err(|_| CliError::runtime("cannot read TLS certificate"))?;
                let key_bytes = std::fs::read(relative(path, &config.private_key))
                    .map_err(|_| CliError::runtime("cannot read TLS private key"))?;
                let certs = rustls_pemfile::certs(&mut cert_bytes.as_slice())
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| CliError::usage("invalid TLS certificate"))?;
                let key = rustls_pemfile::private_key(&mut key_bytes.as_slice())
                    .map_err(|_| CliError::usage("invalid TLS private key"))?
                    .ok_or_else(|| CliError::usage("missing TLS private key"))?;
                Ok::<_, CliError>((certs, key))
            },
            sink,
        )
        .await?;
    let (uid, gid) = effective_identity();
    let cache = startup
        .observe(
            Stage::CacheStart,
            async {
                config
                    .cache
                    .as_ref()
                    .map(|cache| {
                        crate::server_cache::ServerCache::start_with_configured_holders(
                            cache,
                            path,
                            catalog.clone(),
                            context.clone(),
                            drive_count,
                        )
                    })
                    .transpose()
            },
            sink,
        )
        .await?;
    let cache = cache.map(Arc::new);
    if let Some(cache) = &cache {
        lifecycle.install_cache(cache.clone());
    }
    let result = async {
        let mut dispatcher = mount_rs_service::dispatch::DriveDispatcher::new(catalog.clone());
        for (partition_id, partition) in &snapshot.partitions {
            for (drive_id, drive) in &partition.drives {
                let (plan, decorator) = startup.observe(Stage::DriveConfig, async {
                let spec = parse_config_str(
                    &serde_json::json!({"version":1,"driver":drive.driver}).to_string(),
                    path.parent().unwrap_or(Path::new(".")),
                )?;
                let options = spec.to_options();
                let decorator = cache
                    .as_ref()
                    .map(|cache| cache.decorator(partition_id, drive_id))
                    .transpose()?;
                if decorator.is_some()
                    && options.driver == crate::DriverChoice::SplitStore
                    && !options
                        .storage
                        .as_ref()
                        .is_some_and(|storage| storage.concurrent_writes)
                {
                    return Err(CliError::usage(
                        "server blob cache requires concurrent_writes for split-storage drives",
                    ));
                }
                Ok::<_, CliError>((DriverRuntime::prepare(&options, uid, gid)?, decorator))
                }, sink).await?;
                let plan = Arc::new(plan);
                if let Some(cache) = &cache {
                    cache.register_holder_route(
                        partition_id,
                        drive_id,
                        drive.driver.clone(),
                        plan.clone(),
                    )?;
                }
                startup.construction_plan();
                let factory = mount_rs_service::filesystem_runtime::SdkRuntimeFactory::new(
                    Arc::new(CliRuntimeConstructor {
                        plan,
                        context: context.clone(),
                        decorator,
                    }),
                );
                lifecycle.retain_factory(factory.clone());
                let registration = pool.register(factory).map_err(CliError::from)?;
                startup
                    .observe(
                        Stage::DriveRegister,
                        async {
                            dispatcher
                                .register_lazy_definition(
                                    partition_id,
                                    drive_id,
                                    drive.driver.clone(),
                                    registration,
                                )
                                .map_err(CliError::usage)
                        },
                        sink,
                    )
                    .await?;
                startup.registered();
            }
        }
        #[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
        let authenticator = Arc::new(match &local_oidc_fixture {
            Some(source) => mount_rs_service::auth::CatalogAuthenticator::with_key_source(
                catalog.clone(),
                source.clone(),
            ),
            None => mount_rs_service::auth::CatalogAuthenticator::new(catalog.clone()),
        });
        #[cfg(not(all(feature = "local-oidc-fixture", debug_assertions)))]
        let authenticator = Arc::new(mount_rs_service::auth::CatalogAuthenticator::new(
            catalog.clone(),
        ));
        let mut signal = startup
            .observe(Stage::ListenerBind, CtrlCHandler::install(), sink)
            .await?;
        let dispatcher = Arc::new(dispatcher);
        let websocket = if let Some(address) = config.websocket_listen {
            Some(
                startup
                    .observe(
                        Stage::ListenerBind,
                        async {
                            mount_rs_service::websocket::WebSocketServer::bind_with_diagnostics(
                                address,
                                certs.clone(),
                                key.clone_key(),
                                dispatcher.clone(),
                                authenticator.clone(),
                                server_options,
                                mount_rs_service::server::RemoteTransferLimits::default(),
                                startup.enabled(),
                            )
                            .await
                            .map_err(|_| CliError::runtime("cannot start TLS websocket service"))
                        },
                        sink,
                    )
                    .await?,
            )
        } else {
            None
        };
        observers.websocket = websocket
            .as_ref()
            .and_then(mount_rs_service::websocket::WebSocketServer::diagnostics);
        let websocket_address = websocket.as_ref().map(|server| server.local_addr());
        if let Some(websocket) = websocket {
            lifecycle.install_websocket(websocket);
        }
        let server = match startup
            .observe(
                Stage::ListenerBind,
                mount_rs_service::server::RemoteServer::bind_with_diagnostics(
                    config.listen,
                    certs,
                    key,
                    dispatcher,
                    authenticator,
                    server_options,
                    mount_rs_service::server::RemoteTransferLimits::default(),
                    startup.enabled(),
                ),
                sink,
            )
            .await
        {
            Ok(server) => server,
            Err(_) => {
                return Err(CliError::runtime("cannot start remote service"));
            }
        };
        observers.quic = server.diagnostics();
        let quic_address = server.local_addr();
        lifecycle.install_quic(server);
        startup.finish_startup(true);
        startup.publish(sink);
        if let Some(websocket) = websocket_address {
            println!("remote TLS websocket listening at {}", websocket);
        }
        println!("remote listening at {}", quic_address);
        diagnostics::wait_with_periodic_capture(signal.wait(), diagnostic_interval, |capture| {
            diagnostics::emit_periodic(
                observers.quic.as_ref(),
                observers.websocket.as_ref(),
                capture,
            );
        })
        .await
    }
    .await;
    if result.is_err() {
        startup.finish_startup(false);
        startup.publish(sink);
        let _ = startup.write_banks(&mut std::io::stderr().lock());
    }
    result
}

pub(crate) async fn mount(path: &Path) -> Result<(), CliError> {
    let config: MountConfig = read(path)?;
    validate_mount(&config)?;
    let transport = match config.transport.as_deref().unwrap_or("auto") {
        "auto" => mount_rs_auto::AutoTransport::Auto,
        "fuse" => mount_rs_auto::AutoTransport::Fuse,
        "nfs" => mount_rs_auto::AutoTransport::Nfs,
        "9p" => mount_rs_auto::AutoTransport::P9,
        _ => return Err(CliError::usage("invalid mount transport")),
    };
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(ca) = &config.driver.ca_certificate {
        let bytes = std::fs::read(relative(path, ca))
            .map_err(|_| CliError::runtime("cannot read CA certificate"))?;
        for cert in rustls_pemfile::certs(&mut bytes.as_slice()) {
            roots
                .add(cert.map_err(|_| CliError::usage("invalid CA certificate"))?)
                .map_err(|_| CliError::usage("invalid CA certificate"))?;
        }
    }
    let credentials = match &config.driver.credentials {
        Credentials::File(file) => {
            mount_rs_remote_client::credentials::CredentialSource::File(relative(path, file))
        }
        Credentials::Command(argv) => {
            mount_rs_remote_client::credentials::CredentialSource::Command(
                argv.iter().map(OsString::from).collect(),
            )
        }
    };
    let connection = mount_rs_remote_client::connection::RemoteConnection::connect_with_transport(
        config.driver.endpoint,
        &config.driver.server_name,
        roots,
        config.driver.partition.clone(),
        credentials,
        config.driver.connection_selection()?,
    )
    .await
    .map_err(|_| CliError::runtime("remote connection failed"))?;
    let mut drivers = Vec::new();
    let mut paths = Vec::new();
    // Preflight every Drive before creating any native mounts.
    for mount in &config.driver.mounts {
        let driver = mount_rs_remote_client::driver::RemoteFsDriver::new(
            connection.clone(),
            mount.drive.clone(),
        )
        .await
        .map_err(|_| CliError::runtime("remote Drive authorization failed"))?;
        drivers.push(driver);
        paths.push(relative(path, &mount.mountpoint));
    }
    let options = crate::parser::CliOptions {
        read_only: config.read_only,
        allow_other: config.allow_other,
        ..Default::default()
    };
    prepare_mountpoints_before_driver(&options, &paths, false).await?;
    let mut signal = CtrlCHandler::install().await?;
    let mut mounted = Vec::new();
    for (driver, mountpoint) in drivers.into_iter().zip(&paths) {
        let options = mount_rs_auto::AutoMountOptions {
            transport,
            read_only: Some(config.read_only || driver.capabilities().read_only),
            fuse: Some(mount_rs_auto::MountOptions {
                allow_other: config.allow_other,
                ..Default::default()
            }),
            ..Default::default()
        };
        match mount_rs_auto::mount(driver, mountpoint, options).await {
            Ok(mount) => {
                println!("mounted {}", mountpoint.display());
                mounted.push(mount);
            }
            Err(_) => {
                for previous in mounted.iter().rev() {
                    retry_unmount(previous, &mut signal).await;
                }
                return Err(CliError::runtime("remote native mount failed"));
            }
        }
    }
    wait_for_multiple_nfs_shutdown(&mounted, &mut signal).await;
    connection.close();
    Ok(())
}

fn resolve_catalog_driver(value: &mut serde_json::Value, base: &Path) -> Result<(), CliError> {
    // Validate the original lexical paths before catalog rebasing can turn a
    // refused SQLite URI or :memory: selection into an ordinary filename.
    let spec = parse_config_str(
        &serde_json::json!({"version":1,"driver":value}).to_string(),
        base,
    )?;
    if spec.driver.is_none() {
        return Err(CliError::usage("Drive requires a storage driver"));
    }
    resolve_backend_paths(value, base);
    Ok(())
}

fn resolve_backend_paths(value: &mut serde_json::Value, base: &Path) {
    if let Some(object) = value.as_object_mut() {
        for (key, value) in object {
            if matches!(
                key.as_str(),
                "root" | "database" | "blocks" | "path" | "cluster_file"
            ) {
                if let Some(path) = value.as_str()
                    && !Path::new(path).is_absolute()
                {
                    *value =
                        serde_json::Value::String(base.join(path).to_string_lossy().into_owned());
                }
            } else {
                resolve_backend_paths(value, base);
            }
        }
    }
}

pub(crate) fn is_remote(path: &Path) -> Result<bool, CliError> {
    let value: serde_json::Value = read(path)?;
    Ok(value
        .pointer("/driver/kind")
        .and_then(serde_json::Value::as_str)
        == Some("remote"))
}
pub(crate) fn validate(path: &Path) -> Result<(), CliError> {
    let config: MountConfig = read(path)?;
    validate_mount(&config)?;
    match config.transport.as_deref().unwrap_or("auto") {
        "auto" | "fuse" | "nfs" | "9p" => Ok(()),
        _ => Err(CliError::usage("invalid mount transport")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn lazy_service_fixture(
        root: &Path,
        listen: SocketAddr,
        websocket: SocketAddr,
    ) -> PathBuf {
        use base64::Engine;
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let pem = |label: &str, bytes: &[u8]| {
            format!(
                "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        };
        std::fs::write(
            root.join("cert.pem"),
            pem("CERTIFICATE", certificate.cert.der()),
        )
        .unwrap();
        std::fs::write(
            root.join("key.pem"),
            pem("PRIVATE KEY", &certificate.signing_key.serialize_der()),
        )
        .unwrap();
        let catalog = SqliteCatalog::open(root.join("catalog.sqlite"))
            .await
            .unwrap();
        let document = serde_json::json!({"revision":0,"partitions":{"p":{"drives":{"d":{"driver":{
            "kind":"splitstore","storage":{
                "metadata":{"kind":"sqlite","path":root.join("cold-metadata.sqlite"),"journal_mode":"wal"},
                "blocks":{"kind":"sqlite","path":root.join("cold-blocks.sqlite"),"journal_mode":"wal"},
                "compact_inode_updates":true
            }
        }}}}},"issuer_policies":{},"grants":{}});
        catalog
            .compare_and_swap(0, serde_json::from_value(document).unwrap())
            .await
            .unwrap();
        let path = root.join("service.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version":1,"catalog":"catalog.sqlite","listen":listen,"websocket_listen":websocket,
                "certificate":"cert.pem","private_key":"key.pem","max_active_drives":1
            })
            .to_string(),
        )
        .unwrap();
        path
    }

    #[tokio::test]
    async fn lazy_cancelled_actual_ready_service_drains_both_retained_listeners_with_cold_providers()
     {
        use mount_rs_service::startup::{Identity, Outcome, Startup};
        let root = tempfile::tempdir().unwrap().keep();
        let path = lazy_service_fixture(
            &root,
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await;
        let keeper = Arc::new(RemoteRuntimeKeeper::default());
        let startup = Arc::new(Startup::new_lazy(true, Identity::cli()));
        let ready = Arc::new(tokio::sync::Notify::new());
        let serving = tokio::spawn({
            let keeper = keeper.clone();
            let startup = startup.clone();
            let ready = ready.clone();
            async move {
                serve_observed(
                    &path,
                    &startup,
                    &mut |snapshot| {
                        if snapshot.terminal_outcome == Outcome::Ready {
                            ready.notify_one();
                        }
                        Ok(())
                    },
                    &keeper,
                )
                .await
            }
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), ready.notified())
            .await
            .unwrap();
        let lifecycle = keeper.retained().unwrap();
        assert_eq!(lifecycle.listener_count(), 2);
        assert_eq!(startup.snapshot().unwrap().open_started, 0);
        assert_eq!(startup.snapshot().unwrap().construction_plans, Some(1));
        assert!(!root.join("cold-metadata.sqlite").exists());
        assert!(!root.join("cold-blocks.sqlite").exists());
        serving.abort();
        assert!(serving.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(5), lifecycle.close())
            .await
            .unwrap()
            .unwrap();
        assert!(keeper.is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn assert_lazy_ready_raw_cache_policy(
        cache_limits: Option<(usize, usize)>,
        block_cache_directory: bool,
    ) {
        use mount_rs_service::startup::{CleanupOutcome, Identity, Outcome, Stage, Startup};
        let root = tempfile::tempdir().unwrap().keep();
        let path = lazy_service_fixture(
            &root,
            "127.0.0.1:0".parse().unwrap(),
            "127.0.0.1:0".parse().unwrap(),
        )
        .await;
        if let Some((ram_bytes, disk_bytes)) = cache_limits {
            let catalog = SqliteCatalog::open(root.join("catalog.sqlite"))
                .await
                .unwrap();
            let mut snapshot = catalog.load_current().await.unwrap();
            snapshot
                .partitions
                .get_mut("p")
                .unwrap()
                .drives
                .get_mut("d")
                .unwrap()
                .driver["storage"]["concurrent_writes"] = serde_json::json!(true);
            catalog
                .compare_and_swap(snapshot.revision, snapshot)
                .await
                .unwrap();
            let mut config: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            config["cache"] = serde_json::json!({
                "cluster":"test", "node_id":"node", "disk_path":"cache",
                "ram_bytes":ram_bytes, "disk_bytes":disk_bytes, "max_entries":16,
                "peer_listen":"127.0.0.1:0", "ca_certificate":"cert.pem",
                "certificate":"cert.pem", "private_key":"key.pem",
                "discovery":"peer-query", "peers":[]
            });
            std::fs::write(&path, config.to_string()).unwrap();
        }
        if block_cache_directory {
            assert!(cache_limits.is_some());
            std::fs::write(root.join("cache"), b"owned non-directory cache fixture").unwrap();
        }
        let keeper = Arc::new(RemoteRuntimeKeeper::default());
        let startup = Arc::new(Startup::new_lazy(true, Identity::cli()));
        let ready = Arc::new(tokio::sync::Notify::new());
        let mut serving = tokio::spawn({
            let keeper = keeper.clone();
            let startup = startup.clone();
            let ready = ready.clone();
            async move {
                serve_observed(
                    &path,
                    &startup,
                    &mut |snapshot| {
                        if snapshot.terminal_outcome == Outcome::Ready {
                            ready.notify_one();
                        }
                        Ok(())
                    },
                    &keeper,
                )
                .await
            }
        });
        let early_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                result = &mut serving => Some(result.expect("service task panicked")),
                _ = ready.notified() => None,
            }
        })
        .await
        .expect("service neither became ready nor returned its startup error");
        let expected_refusal = block_cache_directory || (cache_limits.is_some() && cfg!(not(unix)));
        if let Some(result) = early_result {
            let error = result.expect_err("service completed before readiness without an error");
            let snapshot = startup.snapshot().unwrap();
            assert!(
                keeper.is_empty(),
                "startup refusal must drain retained owners"
            );
            assert!(!root.join("cold-metadata.sqlite").exists());
            assert!(!root.join("cold-blocks.sqlite").exists());
            if block_cache_directory {
                assert_eq!(
                    std::fs::read(root.join("cache")).unwrap(),
                    b"owned non-directory cache fixture",
                );
            } else {
                assert!(!root.join("cache").exists());
            }
            std::fs::remove_dir_all(&root).unwrap();
            assert!(expected_refusal, "unexpected startup failure: {error}");
            assert_eq!(error.exit_code(), 1);
            assert!(
                error
                    .to_string()
                    .starts_with(if cfg!(unix) { "EIO:" } else { "ENOTSUP:" }),
                "{error}",
            );
            assert_eq!(snapshot.terminal_outcome, Outcome::Error);
            assert_eq!(snapshot.cleanup_outcome, Some(CleanupOutcome::Success));
            assert_eq!(snapshot.open_started, 0);
            let cache = snapshot.stages[Stage::CacheStart as usize];
            assert_eq!(
                (cache.started, cache.error, cache.success, cache.in_flight),
                (1, 1, 0, 0)
            );
            assert_eq!(snapshot.stages[Stage::ListenerBind as usize].started, 0);
            assert_eq!(snapshot.stages[Stage::Ready as usize].started, 0);
            return;
        }
        let lifecycle = keeper.retained().unwrap();
        let retained_limits = lifecycle.raw_cache_limits_for_test().unwrap();
        assert_eq!(lifecycle.listener_count(), 2);
        assert_eq!(startup.snapshot().unwrap().open_started, 0);
        assert_eq!(startup.snapshot().unwrap().construction_plans, Some(1));
        assert!(!root.join("cold-metadata.sqlite").exists());
        assert!(!root.join("cold-blocks.sqlite").exists());
        serving.abort();
        assert!(serving.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(5), lifecycle.close())
            .await
            .unwrap()
            .unwrap();
        assert!(keeper.is_empty());
        assert!(!root.join("cold-metadata.sqlite").exists());
        assert!(!root.join("cold-blocks.sqlite").exists());
        std::fs::remove_dir_all(root).unwrap();
        // Check policy after the actual listeners and retained owners have drained,
        // so a behavioral RED cannot leave a running service behind.
        assert!(
            !expected_refusal,
            "refused cache configuration reported Ready"
        );
        assert_eq!(
            retained_limits,
            if cache_limits.is_some() {
                (0, 0)
            } else {
                (64 * 1024 * 1024, 4096)
            }
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lazy_ready_raw_cache_ram_tier_disables_inner_cache() {
        assert_lazy_ready_raw_cache_policy(Some((4096, 0)), false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lazy_ready_raw_cache_disk_tier_disables_inner_cache() {
        assert_lazy_ready_raw_cache_policy(Some((0, 4096)), false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lazy_ready_raw_cache_both_tiers_disable_inner_cache() {
        assert_lazy_ready_raw_cache_policy(Some((4096, 4096)), false).await;
    }

    #[tokio::test]
    async fn lazy_ready_raw_cache_without_server_cache_keeps_common_default() {
        assert_lazy_ready_raw_cache_policy(None, false).await;
    }

    #[tokio::test]
    async fn lazy_cache_start_refusal_closes_resources_without_opening_drives() {
        assert_lazy_ready_raw_cache_policy(Some((4096, 4096)), true).await;
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn lazy_cache_start_refuses_unsupported_platform_before_drives_or_listeners() {
        for limits in [(4096, 0), (0, 4096), (4096, 4096)] {
            assert_lazy_ready_raw_cache_policy(Some(limits), false).await;
        }
    }

    #[tokio::test]
    async fn lazy_actual_listener_bind_failures_close_created_resources_without_opening_drives() {
        use mount_rs_service::startup::{CleanupOutcome, Identity, Outcome, Startup};
        for fail_quic in [true, false] {
            let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let root = tempfile::tempdir().unwrap().keep();
            let path = lazy_service_fixture(
                &root,
                if fail_quic {
                    udp.local_addr().unwrap()
                } else {
                    "127.0.0.1:0".parse().unwrap()
                },
                if fail_quic {
                    "127.0.0.1:0".parse().unwrap()
                } else {
                    tcp.local_addr().unwrap()
                },
            )
            .await;
            let keeper = Arc::new(RemoteRuntimeKeeper::default());
            let startup = Startup::new_lazy(true, Identity::cli());
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                serve_observed(&path, &startup, &mut |_| Ok(()), &keeper),
            )
            .await
            .unwrap();
            assert!(result.is_err());
            let snapshot = startup.snapshot().unwrap();
            assert_eq!(snapshot.terminal_outcome, Outcome::Error);
            assert_eq!(snapshot.cleanup_outcome, Some(CleanupOutcome::Success));
            assert_eq!(snapshot.open_started, 0);
            assert_eq!(snapshot.registered_drives, 1);
            assert_eq!(snapshot.stages[10].started, if fail_quic { 3 } else { 2 });
            assert_eq!(snapshot.stages[10].error, 1);
            assert!(keeper.is_empty());
            assert!(!root.join("cold-metadata.sqlite").exists());
            assert!(!root.join("cold-blocks.sqlite").exists());
            drop(udp);
            drop(tcp);
            std::fs::remove_dir_all(root).unwrap();
        }
    }
    #[test]
    fn sqlite_wal_catalog_rejects_special_paths_before_rebasing_both_roles() {
        for role in ["metadata", "blocks"] {
            for path in ["", ":memory:", "file:memory?mode=memory"] {
                let mut value = serde_json::json!({"kind":"splitstore", "storage": {
                    "metadata":{"kind":"memory"}, "blocks":{"kind":"memory"}
                }});
                value["storage"][role] =
                    serde_json::json!({"kind":"sqlite", "path":path, "journal_mode":"wal"});
                assert!(resolve_catalog_driver(&mut value, Path::new("/tmp/catalog")).is_err());
                assert_eq!(value["storage"][role]["path"], path);
            }
        }
        let mut value = serde_json::json!({"kind":"splitstore", "storage": {
            "metadata":{"kind":"sqlite", "path":"meta.db", "journal_mode":"wal"},
            "blocks":{"kind":"sqlite", "path":"blocks.db", "journal_mode":"wal"}
        }});
        resolve_catalog_driver(&mut value, Path::new("/tmp/catalog")).unwrap();
        let expected_metadata_path = Path::new("/tmp/catalog").join("meta.db");
        assert_eq!(
            value["storage"]["metadata"]["path"],
            expected_metadata_path.to_string_lossy().as_ref()
        );
        let parsed = parse_config_str(
            &serde_json::json!({"version":1,"driver":value}).to_string(),
            Path::new("/tmp/catalog"),
        )
        .unwrap();
        assert!(
            matches!(parsed.storage.unwrap().blocks, crate::config::StorageProvider::SqliteWithOptions { path, .. } if path == Path::new("/tmp/catalog/blocks.db"))
        );
    }

    #[tokio::test]
    async fn lazy_invalid_drive_plan_preserves_cold_providers_and_reports_prepared_routes() {
        use base64::Engine;
        use mount_rs_service::startup::{Identity, Outcome, Startup};
        let directory = tempfile::tempdir().unwrap();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let pem = |label: &str, bytes: &[u8]| {
            format!(
                "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
                base64::engine::general_purpose::STANDARD.encode(bytes)
            )
        };
        std::fs::write(
            directory.path().join("cert.pem"),
            pem("CERTIFICATE", cert.cert.der()),
        )
        .unwrap();
        std::fs::write(
            directory.path().join("key.pem"),
            pem("PRIVATE KEY", &cert.signing_key.serialize_der()),
        )
        .unwrap();
        std::fs::create_dir(directory.path().join("blocked.sqlite")).unwrap();
        let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap();
        let (uid, gid) = effective_identity();
        let document = serde_json::json!({"revision":0,"partitions":{"private_partition":{"drives":{
            "a_private_memory":{"driver":{"kind":"memory"}},
            "b_private_sqlite":{"driver":{"kind":"sqlite","database":"good.sqlite","uid":uid,"gid":gid}},
            "c_private_failure":{"driver":{"kind":"unknown_fixture_driver"}}
        }}},"issuer_policies":{},"grants":{}});
        catalog
            .compare_and_swap(0, serde_json::from_value(document).unwrap())
            .await
            .unwrap();
        let path = directory.path().join("service.json");
        std::fs::write(&path, serde_json::json!({"version":1,"catalog":"catalog.sqlite","listen":"127.0.0.1:0","certificate":"cert.pem","private_key":"key.pem"}).to_string()).unwrap();
        let startup = Startup::new_lazy(true, Identity::cli());
        let keeper = Arc::new(RemoteRuntimeKeeper::default());
        let mut output = Vec::new();
        let result = serve_observed(
            &path,
            &startup,
            &mut |snapshot| Startup::write_record(&mut output, snapshot),
            &keeper,
        )
        .await;
        let error = result.unwrap_err();
        let snapshot = startup.snapshot().unwrap();
        assert_eq!(snapshot.open_started, 0, "{error}");
        assert_eq!(snapshot.open_success, 0);
        assert_eq!(snapshot.open_error, 0);
        assert_eq!(snapshot.construction_plans, Some(2));
        assert_eq!(snapshot.max_active_drives, Some(3));
        assert_eq!(snapshot.registered_drives, 2);
        assert_eq!(snapshot.terminal_outcome, Outcome::Error);
        assert_eq!(snapshot.stages[10].started, 0);
        assert!(!directory.path().join("good.sqlite").exists());
        assert!(
            keeper.is_empty(),
            "acknowledged cold cleanup retained the slot"
        );
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("startup_diagnostics "));
        for secret in [
            "private_partition",
            "a_private_memory",
            "b_private_sqlite",
            "c_private_failure",
            "good.sqlite",
            "blocked.sqlite",
        ] {
            assert!(!output.contains(secret));
        }
    }
    #[test]
    fn websocket_remote_configuration_is_explicit() {
        let mut value = valid();
        value["driver"]["connection_transport"] = serde_json::json!("websocket");
        value["driver"]["websocket_endpoint"] = serde_json::json!("127.0.0.1:4434");
        let config: MountConfig = serde_json::from_value(value).expect("TLS websocket config");
        assert!(validate_mount(&config).is_ok());
    }

    #[test]
    fn automatic_remote_selection_requires_explicit_tls_endpoint() {
        for mode in ["websocket", "auto"] {
            let mut value = valid();
            value["driver"]["connection_transport"] = serde_json::json!(mode);
            assert!(validate_mount(&serde_json::from_value(value.clone()).unwrap()).is_err());
            value["driver"]["websocket_endpoint"] = serde_json::json!("127.0.0.1:4434");
            assert!(validate_mount(&serde_json::from_value(value).unwrap()).is_ok());
        }
        let mut value = valid();
        value["driver"]["connection_transport"] = serde_json::json!("plaintext");
        assert!(serde_json::from_value::<MountConfig>(value).is_err());
    }

    #[test]
    fn server_connection_capacity_is_explicit_with_compatible_default() {
        let mut value = serde_json::json!({"version":1,"catalog":"catalog.sqlite","listen":"127.0.0.1:4433","certificate":"cert.pem","private_key":"key.pem"});
        let config: ServiceConfig = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(config.max_connections, 128);
        assert_eq!(config.max_active_drives, None);
        assert_eq!(config.tidb_pool_max_connections, 16);
        value["max_connections"] = serde_json::json!(1024);
        let config: ServiceConfig = serde_json::from_value(value).unwrap();
        assert_eq!(config.max_connections, 1024);
    }

    #[tokio::test]
    async fn lazy_zero_active_drive_capacity_fails_before_opening_any_provider() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("service.json");
        std::fs::write(
            &path,
            serde_json::json!({
                "version":1,"catalog":"absent/catalog.sqlite","listen":"127.0.0.1:0",
                "certificate":"absent.pem","private_key":"absent-key.pem","max_active_drives":0
            })
            .to_string(),
        )
        .unwrap();
        let error = serve(&path).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("max_active_drives must be positive")
        );
        assert!(!directory.path().join("absent").exists());
    }

    #[tokio::test]
    async fn invalid_pool_bound_fails_before_opening_server_resources() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("service.json");
        std::fs::write(&path,serde_json::json!({"version":1,"catalog":"absent/catalog.sqlite","listen":"127.0.0.1:0","certificate":"absent.pem","private_key":"absent-key.pem","tidb_pool_max_connections":0}).to_string()).unwrap();
        let error = serve(&path).await.unwrap_err();
        assert!(
            error.to_string().contains("maximum must be positive"),
            "{error}"
        );
        assert!(!directory.path().join("absent").exists());
    }

    #[cfg(not(all(feature = "local-oidc-fixture", debug_assertions)))]
    #[test]
    fn normal_build_rejects_local_oidc_fixture_configuration() {
        let value = serde_json::json!({
            "version":1,"catalog":"catalog.sqlite","listen":"127.0.0.1:4433",
            "certificate":"cert.pem","private_key":"key.pem",
            "local_oidc_fixture":{
                "issuer":"https://issuer.example.com","audiences":["mount-rs"],
                "jwks":"fixture.jwks.json"
            }
        });
        assert!(serde_json::from_value::<ServiceConfig>(value).is_err());
    }

    #[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
    #[test]
    fn debug_feature_accepts_explicit_local_oidc_fixture_configuration() {
        let value = serde_json::json!({
            "version":1,"catalog":"catalog.sqlite","listen":"127.0.0.1:4433",
            "certificate":"cert.pem","private_key":"key.pem",
            "local_oidc_fixture":{
                "issuer":"https://issuer.example.com","audiences":["mount-rs"],
                "jwks":"fixture.jwks.json"
            }
        });
        let config: ServiceConfig = serde_json::from_value(value).unwrap();
        let fixture = config.local_oidc_fixture.unwrap();
        assert_eq!(fixture.issuer, "https://issuer.example.com");
        assert_eq!(fixture.audiences, ["mount-rs"]);
        assert_eq!(fixture.jwks, Path::new("fixture.jwks.json"));
    }

    #[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
    #[tokio::test]
    async fn debug_fixture_rejects_public_listener_before_opening_resources() {
        for (listen, websocket) in [("0.0.0.0:0", "127.0.0.1:0"), ("127.0.0.1:0", "0.0.0.0:0")] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("service.json");
            std::fs::write(
                &path,
                serde_json::json!({
                    "version":1,"catalog":"absent/catalog.sqlite","listen":listen,
                    "websocket_listen":websocket,
                    "certificate":"absent.pem","private_key":"absent-key.pem",
                    "local_oidc_fixture":{
                        "issuer":"https://issuer.example.com","audiences":["mount-rs"],
                        "jwks":"absent.jwks.json"
                    }
                })
                .to_string(),
            )
            .unwrap();
            let error = serve(&path).await.unwrap_err();
            assert!(error.to_string().contains("loopback"), "{error}");
            assert!(!directory.path().join("absent").exists());
        }
    }

    #[test]
    fn server_cache_configuration_selects_compiled_discovery() {
        let value = serde_json::json!({
            "version":1,"catalog":"catalog.sqlite","listen":"127.0.0.1:4433",
            "certificate":"cert.pem","private_key":"key.pem",
            "cache":{
                "cluster":"production","node_id":"node-1","disk_path":"cache",
                "ram_bytes":1048576,"disk_bytes":8388608,"max_entries":1024,
                "peer_listen":"127.0.0.1:4434","ca_certificate":"ca.pem",
                "certificate":"peer.pem","private_key":"peer-key.pem",
                "discovery":"deterministic","peers":[]
            }
        });
        let config: ServiceConfig = serde_json::from_value(value).unwrap();
        assert!(config.cache.unwrap().validate().is_ok());
    }

    fn valid() -> serde_json::Value {
        serde_json::json!({"version":1,"driver":{"kind":"remote","endpoint":"127.0.0.1:4433","server_name":"localhost","partition":"red","credentials":{"file":"token.jwt"},"mounts":[{"drive":"data","mountpoint":"mnt"}]}})
    }
    #[test]
    fn one_partition_multiple_drives() {
        let mut value = valid();
        value["driver"]["mounts"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"drive":"logs","mountpoint":"logs"}));
        let config: MountConfig = serde_json::from_value(value).unwrap();
        assert!(validate_mount(&config).is_ok());
    }
    #[test]
    fn drive_cannot_override_partition() {
        let mut value = valid();
        value["driver"]["mounts"][0]["partition"] = serde_json::json!("blue");
        assert!(serde_json::from_value::<MountConfig>(value).is_err());
    }
    #[test]
    fn duplicates_and_empty_command_rejected() {
        let mut value = valid();
        let entry = value["driver"]["mounts"][0].clone();
        value["driver"]["mounts"]
            .as_array_mut()
            .unwrap()
            .push(entry);
        assert!(validate_mount(&serde_json::from_value(value).unwrap()).is_err());
        let mut value = valid();
        value["driver"]["credentials"] = serde_json::json!({"command":[]});
        assert!(validate_mount(&serde_json::from_value(value).unwrap()).is_err());
    }
    #[tokio::test]
    async fn operator_apply_resolves_independent_backends_and_rejects_stale_revision() {
        let directory = tempfile::tempdir().unwrap();
        let document = directory.path().join("catalog.json");
        let path = directory.path().join("apply.json");
        std::fs::write(&document,serde_json::json!({"revision":0,"partitions":{"red":{"drives":{"data":{"driver":{"kind":"sqlite","database":"red.sqlite"}}}},"blue":{"drives":{"data":{"driver":{"kind":"memory"}}}}},"issuer_policies":{},"grants":{}}).to_string()).unwrap();
        std::fs::write(&path,serde_json::json!({"version":1,"catalog":"service.sqlite","document":"catalog.json","expected_revision":0}).to_string()).unwrap();
        apply(&path).await.unwrap();
        assert!(apply(&path).await.is_err());
        let catalog = SqliteCatalog::open(directory.path().join("service.sqlite"))
            .await
            .unwrap();
        let snapshot = catalog.load_current().await.unwrap();
        assert_eq!(snapshot.revision, 1);
        assert!(
            Path::new(
                snapshot.partitions["red"].drives["data"].driver["database"]
                    .as_str()
                    .unwrap()
            )
            .is_absolute()
        );
        assert_eq!(
            snapshot.partitions["blue"].drives["data"].driver,
            serde_json::json!({"kind":"memory"})
        );
    }

    #[tokio::test]
    async fn operator_apply_rejects_compact_contradiction_before_catalog_open() {
        let directory = tempfile::tempdir().unwrap();
        let document = directory.path().join("catalog.json");
        let path = directory.path().join("apply.json");
        std::fs::write(
            &document,
            serde_json::json!({
                "revision":0,
                "partitions":{"red":{"drives":{"data":{"driver":{
                    "kind":"splitstore","storage":{
                        "metadata":{"kind":"sqlite","path":"metadata.sqlite"},
                        "blocks":{"kind":"sqlite","path":"blocks.sqlite"},
                        "compact_inode_updates":true,
                        "inode_updates":false
                    }
                }}}}},
                "issuer_policies":{},"grants":{}
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(
            &path,
            serde_json::json!({
                "version":1,"catalog":"absent/service.sqlite",
                "document":"catalog.json","expected_revision":0
            })
            .to_string(),
        )
        .unwrap();
        let error = apply(&path).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("config.driver.storage.inode_updates"),
            "{error}"
        );
        assert!(!directory.path().join("absent").exists());
    }
}
