//! Server-owned cache configuration and SDK block decoration.
use crate::CliError;
use serde::Deserialize;
use std::{collections::BTreeSet, net::SocketAddr, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CacheServiceConfig {
    pub cluster: String,
    pub node_id: String,
    pub disk_path: PathBuf,
    pub ram_bytes: usize,
    pub disk_bytes: usize,
    pub max_entries: usize,
    pub peer_listen: SocketAddr,
    pub ca_certificate: PathBuf,
    pub certificate: PathBuf,
    pub private_key: PathBuf,
    pub discovery: DiscoverySelection,
    pub peers: Vec<PeerConfig>,
    #[serde(default)]
    pub redis: Option<DirectoryConfig>,
    #[serde(default = "default_blob_size")]
    pub max_blob_bytes: usize,
    #[serde(default = "default_inflight")]
    pub max_inflight: usize,
    #[serde(default = "default_deadline")]
    pub deadline_ms: u64,
    #[serde(default = "default_queries")]
    pub peer_query_limit: usize,
    #[serde(default = "default_maintenance")]
    pub maintenance_capacity: usize,
    #[serde(default = "default_placement_concurrency")]
    pub placement_concurrency: usize,
    #[serde(default = "default_hedge_delay")]
    pub hedge_delay_ms: u64,
    #[serde(default = "default_peer_transfer_bytes")]
    pub peer_transfer_bytes: usize,
}
fn default_placement_concurrency() -> usize {
    4
}
fn default_hedge_delay() -> u64 {
    25
}
fn default_peer_transfer_bytes() -> usize {
    128 * 1024 * 1024
}
fn default_blob_size() -> usize {
    1024 * 1024
}
fn default_inflight() -> usize {
    64
}
fn default_deadline() -> u64 {
    500
}
fn default_queries() -> usize {
    3
}
fn default_maintenance() -> usize {
    64
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum DiscoverySelection {
    Deterministic,
    PeerQuery,
    Directory,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PeerConfig {
    pub id: String,
    pub address: SocketAddr,
    pub server_name: String,
    /// SHA256 of the DER leaf certificate, not the PEM file.
    pub certificate_sha256: String,
    pub partitions: BTreeSet<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryConfig {
    pub address: String,
    pub namespace: String,
    #[serde(default)]
    pub username: Option<String>,
    /// Read the password from this file. Keep credentials out of service JSON.
    #[serde(default)]
    pub password_file: Option<PathBuf>,
    #[serde(default)]
    pub tls: Option<DirectoryTlsConfig>,
    #[serde(default = "default_ttl")]
    pub ttl_ms: u64,
}
fn default_ttl() -> u64 {
    30_000
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DirectoryTlsConfig {
    pub server_name: String,
    pub ca_certificate: PathBuf,
}
impl CacheServiceConfig {
    pub(crate) fn validate(&self) -> std::result::Result<(), CliError> {
        let name = |s: &str| !s.is_empty() && s.len() <= 256 && !s.chars().any(char::is_control);
        if !name(&self.cluster)
            || !name(&self.node_id)
            || self.disk_path.as_os_str().is_empty()
            || self.max_entries == 0
            || self.max_entries > 1_000_000
            || self.max_blob_bytes == 0
            || self.max_blob_bytes > 16 * 1024 * 1024
            || self.max_inflight == 0
            || self.max_inflight > 1024
            || self.deadline_ms == 0
            || self.deadline_ms > 30_000
            || self.peer_query_limit == 0
            || self.peer_query_limit > 256
            || self.maintenance_capacity == 0
            || self.maintenance_capacity > 4096
            || self.placement_concurrency == 0
            || self.placement_concurrency > 32
            || self.hedge_delay_ms > 30_000
            || self.peer_transfer_bytes > 1024 * 1024 * 1024
            || self.peer_transfer_bytes / 2 < 2 * (self.max_blob_bytes + 4121)
            || self.peers.len() > 256
            || (self.ram_bytes == 0 && self.disk_bytes == 0)
            || self.ram_bytes.checked_add(self.disk_bytes).is_none()
        {
            return Err(CliError::usage("invalid server cache limits or identity"));
        }
        let mut ids = BTreeSet::new();
        let mut pins = BTreeSet::new();
        for peer in &self.peers {
            if !name(&peer.id)
                || peer.id == self.node_id
                || !name(&peer.server_name)
                || !ids.insert(&peer.id)
                || !pins.insert(peer.certificate_sha256.to_ascii_lowercase())
                || peer.partitions.is_empty()
                || peer.partitions.len() > 1024
                || peer.partitions.iter().any(|p| !name(p))
                || peer.certificate_sha256.len() != 64
                || !peer
                    .certificate_sha256
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
            {
                return Err(CliError::usage("invalid or duplicate cache peer identity"));
            }
        }
        if matches!(self.discovery, DiscoverySelection::Directory) && self.redis.is_none() {
            return Err(CliError::usage(
                "directory discovery requires redis configuration",
            ));
        }
        if !matches!(self.discovery, DiscoverySelection::Directory) && self.redis.is_some() {
            return Err(CliError::usage(
                "redis configuration requires directory discovery",
            ));
        }
        if let Some(redis) = &self.redis {
            if !name(&redis.namespace)
                || redis.ttl_ms < 1000
                || redis.ttl_ms > 3_600_000
                || redis.address.is_empty()
                || redis.address.len() > 1024
                || redis.username.as_deref().is_some_and(|u| !name(u))
                || (redis.username.is_some() && redis.password_file.is_none())
                || redis
                    .tls
                    .as_ref()
                    .is_some_and(|tls| !name(&tls.server_name))
            {
                return Err(CliError::usage("invalid redis directory configuration"));
            }
            // Cleartext is restricted to loopback, including unauthenticated directories.
            if redis.tls.is_none()
                && !redis
                    .address
                    .parse::<SocketAddr>()
                    .is_ok_and(|a| a.ip().is_loopback())
            {
                return Err(CliError::usage("remote redis directory requires TLS"));
            }
        }
        Ok(())
    }
}

use mount_rs_blob_cache::{
    CachedBlockStore, DistributedRuntime, IntegrityPolicy, LocalCache, ScopeIdentity,
};
use mount_rs_core::{Result, storage::BlockStore};
use mount_rs_sdk::{BlockStoreDecorator, StoreConfig};
use std::sync::{Arc, Mutex};
type DriveMetrics = Arc<Mutex<BTreeMap<(String, String), Arc<mount_rs_blob_cache::CacheMetrics>>>>;

pub(crate) struct DriveCacheDecorator {
    pub cache: Arc<LocalCache>,
    pub runtime: Arc<DistributedRuntime>,
    pub identity: ScopeIdentity,
    metrics: DriveMetrics,
}
impl BlockStoreDecorator for DriveCacheDecorator {
    fn decorate(
        &self,
        config: &StoreConfig,
        store: Arc<dyn BlockStore>,
    ) -> Result<Arc<dyn BlockStore>> {
        let policy = match config {
            StoreConfig::Tidb { .. } => IntegrityPolicy::Sha256Prefixed,
            StoreConfig::FoundationDb { .. } => IntegrityPolicy::Sha256Colon,
            StoreConfig::R2 { .. } | StoreConfig::RustFs { .. } | StoreConfig::AwsS3 { .. } => {
                IntegrityPolicy::ObjectStoreSha256OrOpaque
            }
            StoreConfig::Pglite { .. } => IntegrityPolicy::PgliteMd5,
            StoreConfig::Memory | StoreConfig::Sqlite { .. } => IntegrityPolicy::Opaque,
        };
        let store = CachedBlockStore::new(store, self.cache.clone(), self.identity.clone(), policy)
            .with_runtime(self.runtime.clone());
        self.metrics
            .lock()
            .map_err(|_| mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio))?
            .insert(
                (self.identity.partition.clone(), self.identity.drive.clone()),
                store.metrics(),
            );
        Ok(Arc::new(store))
    }
}

use mount_rs_blob_cache::{
    Discovery, DiscoveryMode, DistributedConfig, FixedDiscovery, LocalCacheConfig, PeerEndpoint,
    PeerId, QuicPeerConfig, QuicPeerTransport, RedisConfig, RedisDiscovery, RedisTlsConfig,
};
use std::{collections::BTreeMap, path::Path, time::Duration};
pub(crate) struct ServerCache {
    pub local: Arc<LocalCache>,
    pub runtime: Arc<DistributedRuntime>,
    pub peers: Arc<QuicPeerTransport>,
    cluster: String,
    metrics: DriveMetrics,
}
fn relative(config: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        config.parent().unwrap_or(Path::new(".")).join(path)
    }
}
fn certificates(
    path: &Path,
) -> std::result::Result<Vec<rustls::pki_types::CertificateDer<'static>>, CliError> {
    let bytes =
        std::fs::read(path).map_err(|_| CliError::runtime("cannot read cache TLS certificate"))?;
    let certs = rustls_pemfile::certs(&mut bytes.as_slice())
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| CliError::usage("invalid cache TLS certificate"))?;
    if certs.is_empty() {
        return Err(CliError::usage("missing cache TLS certificate"));
    }
    Ok(certs)
}
fn roots(path: &Path) -> std::result::Result<rustls::RootCertStore, CliError> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in certificates(path)? {
        roots
            .add(cert)
            .map_err(|_| CliError::usage("invalid cache CA certificate"))?;
    }
    Ok(roots)
}
impl ServerCache {
    pub(crate) fn start(
        config: &CacheServiceConfig,
        path: &Path,
    ) -> std::result::Result<Self, CliError> {
        config.validate()?;
        let deadline = Duration::from_millis(config.deadline_ms);
        let local = LocalCache::new(LocalCacheConfig {
            directory: relative(path, &config.disk_path),
            memory_bytes: config.ram_bytes,
            disk_bytes: config.disk_bytes,
            max_entries: config.max_entries,
            max_blob_bytes: config.max_blob_bytes,
        })?;
        let key_bytes = std::fs::read(relative(path, &config.private_key))
            .map_err(|_| CliError::runtime("cannot read cache TLS key"))?;
        let key = rustls_pemfile::private_key(&mut key_bytes.as_slice())
            .map_err(|_| CliError::usage("invalid cache TLS key"))?
            .ok_or_else(|| CliError::usage("missing cache TLS key"))?;
        let mut trusted = BTreeMap::new();
        for peer in &config.peers {
            let mut pin = [0; 32];
            for (i, byte) in pin.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&peer.certificate_sha256[i * 2..i * 2 + 2], 16)
                    .map_err(|_| CliError::usage("invalid cache certificate pin"))?;
            }
            trusted.insert(
                PeerId(peer.id.clone()),
                PeerEndpoint {
                    address: peer.address,
                    server_name: peer.server_name.clone(),
                    certificate_sha256: pin,
                    partitions: peer.partitions.clone(),
                },
            );
        }
        let peers = QuicPeerTransport::bind(
            QuicPeerConfig {
                local: PeerId(config.node_id.clone()),
                bind: config.peer_listen,
                certificates: certificates(&relative(path, &config.certificate))?,
                private_key: key,
                roots: roots(&relative(path, &config.ca_certificate))?,
                trusted,
                max_blob_bytes: config.max_blob_bytes,
                transfer_bytes: config.peer_transfer_bytes,
                max_inflight: config.max_inflight,
                deadline,
            },
            local.clone(),
        )?;
        let mut members = config
            .peers
            .iter()
            .map(|p| PeerId(p.id.clone()))
            .collect::<Vec<_>>();
        members.push(PeerId(config.node_id.clone()));
        let mode = match config.discovery {
            DiscoverySelection::Deterministic => DiscoveryMode::Deterministic,
            DiscoverySelection::PeerQuery => DiscoveryMode::PeerQuery,
            DiscoverySelection::Directory => DiscoveryMode::Directory,
        };
        let discovery: Arc<dyn Discovery> = if let Some(redis) = &config.redis {
            let password = if let Some(file) = &redis.password_file {
                use std::io::Read;
                let mut bytes = Vec::new();
                std::fs::File::open(relative(path, file))
                    .map_err(|_| CliError::runtime("cannot read redis password file"))?
                    .take(4097)
                    .read_to_end(&mut bytes)
                    .map_err(|_| CliError::runtime("cannot read redis password file"))?;
                if bytes.len() > 4096 {
                    return Err(CliError::usage("redis password file exceeds limit"));
                }
                Some(
                    String::from_utf8(bytes)
                        .map_err(|_| CliError::usage("invalid redis password file"))?
                        .trim_end_matches(['\r', '\n'])
                        .to_owned(),
                )
            } else {
                None
            };
            let tls = redis
                .tls
                .as_ref()
                .map(|tls| {
                    Ok::<_, CliError>(RedisTlsConfig {
                        server_name: tls.server_name.clone(),
                        roots: roots(&relative(path, &tls.ca_certificate))?,
                    })
                })
                .transpose()?;
            RedisDiscovery::new(
                RedisConfig {
                    address: redis.address.clone(),
                    username: redis.username.clone(),
                    password,
                    namespace: redis.namespace.clone(),
                    ttl: Duration::from_millis(redis.ttl_ms),
                    deadline,
                    max_reply_bytes: 1024 * 1024,
                    tls,
                },
                members,
                config.peer_query_limit,
                mode,
            )?
        } else {
            FixedDiscovery::new_with_mode(members, config.peer_query_limit, mode)?
        };
        let runtime = DistributedRuntime::new(
            discovery,
            peers.clone(),
            PeerId(config.node_id.clone()),
            DistributedConfig {
                max_peer_queries: config.peer_query_limit,
                deadline,
                maintenance_capacity: config.maintenance_capacity,
                placement_concurrency: config.placement_concurrency,
                hedge_delay: Duration::from_millis(config.hedge_delay_ms),
                max_inflight_misses: config.max_inflight,
            },
        )?;
        Ok(Self {
            local,
            runtime,
            peers,
            cluster: config.cluster.clone(),
            metrics: Arc::new(Mutex::new(BTreeMap::new())),
        })
    }
    pub(crate) fn decorator(&self, partition: &str, drive: &str) -> DriveCacheDecorator {
        DriveCacheDecorator {
            cache: self.local.clone(),
            runtime: self.runtime.clone(),
            identity: ScopeIdentity {
                cluster: self.cluster.clone(),
                partition: partition.to_owned(),
                drive: drive.to_owned(),
            },
            metrics: self.metrics.clone(),
        }
    }
    pub(crate) async fn shutdown(&self) {
        self.runtime.shutdown().await;
        self.peers.shutdown().await;
        self.local.shutdown().await;
        if let Ok(metrics) = self.metrics.lock() {
            for ((partition, drive), metrics) in metrics.iter() {
                let stats = metrics.snapshot();
                eprintln!(
                    "blob_cache {}",
                    serde_json::json!({"partition":partition,"drive":drive,"local_hits":stats.local_hits,"peer_hits":stats.peer_hits,"backing_fetches":stats.backing_fetches,"hit_bytes":stats.hit_bytes,"cache_errors":stats.cache_errors,"maintenance_dropped":stats.maintenance_dropped})
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> CacheServiceConfig {
        serde_json::from_value(serde_json::json!({"cluster":"cluster","node_id":"node","disk_path":"cache","ram_bytes":4096,"disk_bytes":4096,"max_entries":10,"peer_listen":"127.0.0.1:4000","ca_certificate":"ca.pem","certificate":"cert.pem","private_key":"key.pem","discovery":"peer-query","peers":[]})).unwrap()
    }
    #[test]
    fn rejects_unbounded_and_duplicate_peer_configuration() {
        let mut c = config();
        c.peer_query_limit = 0;
        assert!(c.validate().is_err());
        let mut c = config();
        c.max_blob_bytes = usize::MAX;
        assert!(c.validate().is_err());
        let mut c = config();
        c.discovery = DiscoverySelection::Directory;
        assert!(c.validate().is_err());
        let mut c = config();
        let p = serde_json::json!({"id":"peer","address":"127.0.0.1:4001","server_name":"peer.local","certificate_sha256":"00".repeat(32),"partitions":["partition"]});
        c.peers = vec![
            serde_json::from_value(p.clone()).unwrap(),
            serde_json::from_value(p).unwrap(),
        ];
        assert!(c.validate().is_err());
    }
    #[test]
    fn remote_directory_requires_tls() {
        let mut c = config();
        c.discovery = DiscoverySelection::Directory;
        c.redis = Some(DirectoryConfig {
            address: "192.0.2.1:6379".into(),
            namespace: "cache".into(),
            username: None,
            password_file: None,
            tls: None,
            ttl_ms: 30_000,
        });
        assert!(c.validate().is_err());
        c.redis.as_mut().unwrap().address = "127.0.0.1:6379".into();
        assert!(c.validate().is_ok());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn sqlite_drive_uses_service_decorator_and_reopens_with_cache_hits() {
        use async_trait::async_trait;
        struct MissingPeer;
        #[async_trait]
        impl mount_rs_blob_cache::PeerTransport for MissingPeer {
            async fn get(
                &self,
                _: &PeerId,
                _: &mount_rs_blob_cache::CacheScope,
                _: &mount_rs_core::storage::BlockId,
            ) -> Result<Option<Vec<u8>>> {
                Ok(None)
            }
            async fn put(
                &self,
                _: &PeerId,
                _: &mount_rs_blob_cache::CacheScope,
                _: &mount_rs_core::storage::BlockId,
                _: &[u8],
            ) -> Result<()> {
                Ok(())
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(LocalCacheConfig {
            directory: dir.path().join("cache"),
            memory_bytes: 8192,
            disk_bytes: 16384,
            max_entries: 100,
            max_blob_bytes: 4096,
        })
        .unwrap();
        let runtime = DistributedRuntime::new(
            FixedDiscovery::new(vec![PeerId("self".into())], 1).unwrap(),
            Arc::new(MissingPeer),
            PeerId("self".into()),
            DistributedConfig::default(),
        )
        .unwrap();
        let metrics = Arc::new(Mutex::new(BTreeMap::new()));
        let decorator = DriveCacheDecorator {
            cache,
            runtime: runtime.clone(),
            identity: ScopeIdentity {
                cluster: "cluster".into(),
                partition: "partition".into(),
                drive: "drive".into(),
            },
            metrics: metrics.clone(),
        };
        let mut options =
            mount_rs_sdk::SplitOptions::memory("first", 4096).with_concurrent_writes(true);
        options.metadata = StoreConfig::Sqlite {
            path: dir.path().join("metadata.sqlite"),
        };
        options.blocks = StoreConfig::Sqlite {
            path: dir.path().join("blocks.sqlite"),
        };
        let first =
            mount_rs_sdk::Filesystem::split_with_block_decorator(options.clone(), &decorator)
                .await
                .unwrap();
        let view = mount_rs_core::Loopback::from_arc(first.driver());
        view.write_file("/data", b"durable cached bytes")
            .await
            .unwrap();
        assert_eq!(
            view.read_file("/data").await.unwrap(),
            b"durable cached bytes"
        );
        first.shutdown().await.unwrap();
        drop(view);
        drop(first);
        options.owner = "second".into();
        let second = mount_rs_sdk::Filesystem::split_with_block_decorator(options, &decorator)
            .await
            .unwrap();
        let view = mount_rs_core::Loopback::from_arc(second.driver());
        assert_eq!(
            view.read_file("/data").await.unwrap(),
            b"durable cached bytes"
        );
        let stats = metrics.lock().unwrap().values().next().unwrap().snapshot();
        assert!(stats.local_hits > 0);
        assert_eq!(stats.backing_fetches, 0);
        second.shutdown().await.unwrap();
        runtime.shutdown().await;
    }
}
