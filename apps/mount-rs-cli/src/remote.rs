//! Remote service bootstrap and provider configuration.
#[path = "remote_diagnostics.rs"]
mod diagnostics;

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
            resolve_backend_paths(&mut drive.driver, &document_base);
            let spec = parse_config_str(
                &serde_json::json!({"version":1,"driver":drive.driver}).to_string(),
                document_path.parent().unwrap_or(Path::new(".")),
            )?;
            if spec.driver.is_none() {
                return Err(CliError::usage("Drive requires a storage driver"));
            }
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
    let config: ServiceConfig = read(path)?;
    version(config.version)?;
    let server_options = mount_rs_service::server::RemoteServerOptions {
        max_connections: config.max_connections,
    };
    server_options.validate().map_err(CliError::usage)?;
    if let Some(cache) = &config.cache {
        cache.validate()?;
    }
    #[cfg(all(feature = "local-oidc-fixture", debug_assertions))]
    let local_oidc_fixture = local_oidc_fixture_key_source(&config, path)?;
    let context = mount_rs_sdk::StorageContext::new(config.tidb_pool_max_connections)?;
    let catalog = Arc::new(
        SqliteCatalog::open(relative(path, &config.catalog))
            .await
            .map_err(|_| CliError::runtime("cannot open service catalog"))?,
    );
    let snapshot = catalog
        .load_current()
        .await
        .map_err(|_| CliError::runtime("cannot load service catalog"))?;
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
    let (uid, gid) = effective_identity();
    let cache = config
        .cache
        .as_ref()
        .map(|cache| crate::server_cache::ServerCache::start(cache, path))
        .transpose()?;
    let mut runtimes = Vec::new();
    let mut observer = None;
    let result = async {
        let mut dispatcher = mount_rs_service::dispatch::DriveDispatcher::new(catalog.clone());
        for (partition_id, partition) in &snapshot.partitions {
            for (drive_id, drive) in &partition.drives {
                let spec = parse_config_str(
                    &serde_json::json!({"version":1,"driver":drive.driver}).to_string(),
                    path.parent().unwrap_or(Path::new(".")),
                )?;
                let options = spec.to_options();
                let decorator = cache
                    .as_ref()
                    .map(|cache| cache.decorator(partition_id, drive_id));
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
                let runtime = DriverRuntime::open_with_storage_context(
                    &options,
                    uid,
                    gid,
                    decorator
                        .as_ref()
                        .map(|d| d as &dyn mount_rs_sdk::BlockStoreDecorator),
                    Some(&context),
                )
                .await?;
                runtimes.push(runtime);
                dispatcher
                    .register_definition(
                        partition_id,
                        drive_id,
                        drive.driver.clone(),
                        runtimes.last().expect("just opened runtime").driver(),
                    )
                    .map_err(CliError::usage)?;
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
        let mut signal = CtrlCHandler::install().await?;
        let dispatcher = Arc::new(dispatcher);
        let websocket = if let Some(address) = config.websocket_listen {
            Some(
                mount_rs_service::websocket::WebSocketServer::bind_with_options(
                    address,
                    certs.clone(),
                    key.clone_key(),
                    dispatcher.clone(),
                    authenticator.clone(),
                    server_options,
                )
                .await
                .map_err(|_| CliError::runtime("cannot start TLS websocket service"))?,
            )
        } else {
            None
        };
        let server = match mount_rs_service::server::RemoteServer::bind_with_diagnostics(
            config.listen,
            certs,
            key,
            dispatcher,
            authenticator,
            server_options,
            mount_rs_service::server::RemoteTransferLimits::default(),
            diagnostics::enabled(std::env::var_os("MOUNT_RS_PROFILE_IO").as_deref()),
        )
        .await
        {
            Ok(server) => server,
            Err(_) => {
                if let Some(websocket) = websocket {
                    websocket.close().await;
                }
                return Err(CliError::runtime("cannot start remote service"));
            }
        };
        observer = server.diagnostics();
        if let Some(websocket) = &websocket {
            println!(
                "remote TLS websocket listening at {}",
                websocket.local_addr()
            );
        }
        println!("remote listening at {}", server.local_addr());
        let result = signal.wait().await;
        server.close().await;
        if let Some(websocket) = websocket {
            websocket.close().await;
        }
        result
    }
    .await;
    let mut shutdown_error = None;
    for runtime in runtimes.iter().rev() {
        if let Err(error) = runtime.shutdown().await {
            shutdown_error.get_or_insert_with(|| CliError::from(error));
        }
    }
    if let Err(error) = context.close().await {
        shutdown_error.get_or_insert_with(|| CliError::from(error));
    }
    if let Some(cache) = cache {
        cache.shutdown().await;
    }
    let outcome = result.and(shutdown_error.map_or(Ok(()), Err));
    if let Some(observer) = observer {
        diagnostics::emit(&observer);
    }
    outcome
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
        assert_eq!(config.tidb_pool_max_connections, 16);
        value["max_connections"] = serde_json::json!(1024);
        let config: ServiceConfig = serde_json::from_value(value).unwrap();
        assert_eq!(config.max_connections, 1024);
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
