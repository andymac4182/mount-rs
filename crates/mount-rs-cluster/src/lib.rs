//! Server-only advisory routing. TiDB, never Redis, grants volume ownership.
//!
//! Adapts the exact-owner compare/renew/release pattern inspected in agentuplink
//! tunnel-catalog. This implementation uses TiDB generations rather than its
//! Redis epoch allocator. No sockets, file bytes or credentials are stored here.
/// Pinned HTTP/3 peer transport. Applications supply authenticated membership
/// policy and volume-owner admission; transport does not authorize ownership.
#[cfg(feature = "http3")]
pub use tunnel_transport as transport;

#[cfg(feature = "http3")]
mod forward_trace;
#[cfg(feature = "http3")]
mod forwarding;
#[cfg(feature = "http3")]
pub use forward_trace::{ForwardRequestTiming, ForwardRequestTrace};
#[cfg(feature = "http3")]
mod request_class;
#[cfg(feature = "http3")]
pub use forwarding::{
    ClusterIngress, ForwardDiagnostics, ForwardError, ForwardOptions, IngressForwarder,
    PeerDirectory, TrustedPeer,
};
#[cfg(feature = "http3")]
mod owner_http;
#[cfg(feature = "http3")]
pub use owner_http::{HOP_HEADER, OwnerHttpHandler, OwnerHttpRouter, ROUTE_HEADER};

use redis::aio::{ConnectionManager, MultiplexedConnection};
use serde::{Deserialize, Serialize};
use std::{net::SocketAddr, time::Duration};

#[cfg(feature = "http3")]
mod session_auth;
#[cfg(feature = "http3")]
pub use session_auth::SessionCredentials;

#[cfg(feature = "http3")]
mod session_http;
#[cfg(feature = "http3")]
pub use session_http::{SESSION_HEADER, SessionHttpHandler};

mod session_supervisor;
pub use session_supervisor::SessionSupervisor;

mod registration_supervisor;
pub use registration_supervisor::RegistrationSupervisor;

mod sessions;
pub use sessions::{SessionBudget, SessionToken, VolumeSessions};

mod coalesce;
mod handles;
mod join_slot;
pub use handles::OwnedHandleSession;

mod owner;
pub use owner::{
    OwnedVolumeRegistration, RegistrationError, RegistrationRecovery, RegistrationStatus,
};

const MAX_RECORD: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VolumeRoute {
    pub volume: String,
    pub node: String,
    pub boot: String,
    pub registration: String,
    /// Generation obtained from the authoritative TiDB writer lease.
    pub fence: u64,
    pub address: SocketAddr,
    /// Must be authenticated against configured membership, not trusted from Redis.
    pub tls_server_name: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistryError {
    InvalidInput,
    InvalidRecord,
    Unavailable,
    /// A timed-out or failed write might have reached Redis. Reconcile first.
    WriteOutcomeUnknown,
}
impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "route registry: {self:?}")
    }
}
impl std::error::Error for RegistryError {}

/// The server supplies an authenticated TLS connection and deployment namespace.
/// Keep maintenance traffic on a separately established physical connection when
/// ordinary lookup traffic could exhaust its latency budget.
#[derive(Clone)]
pub struct RedisRouteRegistry {
    connection: RegistryConnection,
    namespace: String,
    timeout: Duration,
}

#[derive(Clone)]
enum RegistryConnection {
    Fixed(MultiplexedConnection),
    Managed(ConnectionManager),
}
impl RegistryConnection {
    async fn query<T: redis::FromRedisValue>(&self, command: &redis::Cmd) -> redis::RedisResult<T> {
        match self {
            Self::Fixed(connection) => command.query_async(&mut connection.clone()).await,
            Self::Managed(connection) => command.query_async(&mut connection.clone()).await,
        }
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
}
impl VolumeRoute {
    fn encode(&self) -> Result<String, RegistryError> {
        if ![
            &self.volume,
            &self.node,
            &self.boot,
            &self.registration,
            &self.tls_server_name,
        ]
        .into_iter()
        .all(|s| identifier(s))
            || self.fence == 0
            || self.address.port() == 0
        {
            return Err(RegistryError::InvalidInput);
        }
        let json = serde_json::to_string(self).map_err(|_| RegistryError::InvalidInput)?;
        let record = format!("{:020}|{json}", self.fence);
        if record.len() > MAX_RECORD {
            return Err(RegistryError::InvalidInput);
        }
        Ok(record)
    }
}

// Fixed-width decimal comparison preserves the full u64 fence without Lua's
// floating point conversion. A live successor can never be overwritten by an
// older registration. Redis expiry is only a routing expiry, not lease authority.
const ADVERTISE: &str = r#"
if redis.call('STRLEN', KEYS[1]) > 4096 then return -1 end
local old = redis.call('GET', KEYS[1])
if old and old ~= ARGV[1] then
  if string.len(old) < 22 or string.sub(old,21,21) ~= '|' then return -1 end
  if string.sub(old,1,20) >= string.sub(ARGV[1],1,20) then return 0 end
end
redis.call('SET', KEYS[1], ARGV[1], 'PX', ARGV[2])
return 1
"#;
const REFRESH: &str = r#"
if redis.call('STRLEN', KEYS[1]) > 4096 then return -1 end
if redis.call('GET', KEYS[1]) ~= ARGV[1] then return 0 end
redis.call('PEXPIRE', KEYS[1], ARGV[2])
return 1
"#;
const REMOVE: &str = r#"
if redis.call('STRLEN', KEYS[1]) > 4096 then return -1 end
if redis.call('GET', KEYS[1]) ~= ARGV[1] then return 0 end
redis.call('DEL', KEYS[1])
return 1
"#;

impl RedisRouteRegistry {
    /// Set an outer deadline for registry commands. The supplied Redis
    /// connection retains its own response timeout; a shorter client timeout
    /// can fail a command first. This does not change connection configuration.
    pub fn new(
        connection: MultiplexedConnection,
        namespace: String,
        timeout: Duration,
    ) -> Result<Self, RegistryError> {
        Self::with_connection(RegistryConnection::Fixed(connection), namespace, timeout)
    }

    /// Use a caller-configured reconnecting client. Connection failures still
    /// surface to callers; uncertain writes require explicit reconciliation.
    /// TLS, credentials, connection timeouts and reconnect policy belong to
    /// the supplied manager, separate from this registry's command deadline.
    pub fn with_manager(
        connection: ConnectionManager,
        namespace: String,
        timeout: Duration,
    ) -> Result<Self, RegistryError> {
        Self::with_connection(RegistryConnection::Managed(connection), namespace, timeout)
    }

    fn with_connection(
        connection: RegistryConnection,
        namespace: String,
        timeout: Duration,
    ) -> Result<Self, RegistryError> {
        if !identifier(&namespace) || timeout.is_zero() || timeout > Duration::from_secs(30) {
            return Err(RegistryError::InvalidInput);
        }
        Ok(Self {
            connection,
            namespace,
            timeout,
        })
    }
    fn key(&self, volume: &str) -> Result<String, RegistryError> {
        if !identifier(volume) {
            return Err(RegistryError::InvalidInput);
        }
        Ok(format!("mount-rs:{}:route:{volume}", self.namespace))
    }
    async fn change(
        &self,
        script: &str,
        route: &VolumeRoute,
        ttl: Duration,
    ) -> Result<bool, RegistryError> {
        if ttl < Duration::from_millis(20) || ttl > Duration::from_secs(30) {
            return Err(RegistryError::InvalidInput);
        }
        let record = route.encode()?;
        let mut cmd = redis::cmd("EVAL");
        cmd.arg(script)
            .arg(1)
            .arg(self.key(&route.volume)?)
            .arg(record)
            .arg(ttl.as_millis() as u64);
        let result: i64 = tokio::time::timeout(self.timeout, self.connection.query(&cmd))
            .await
            .map_err(|_| RegistryError::WriteOutcomeUnknown)?
            .map_err(|_| RegistryError::WriteOutcomeUnknown)?;
        match result {
            1 => Ok(true),
            0 => Ok(false),
            _ => Err(RegistryError::InvalidRecord),
        }
    }
    /// Call only after confirming current TiDB ownership and loading the volume.
    /// Repeat advertisement after Redis loss only with freshly validated ownership.
    pub async fn advertise(
        &self,
        route: &VolumeRoute,
        ttl: Duration,
    ) -> Result<bool, RegistryError> {
        self.change(ADVERTISE, route, ttl).await
    }
    /// Does not recreate an expired registration or touch a successor.
    pub async fn refresh(&self, route: &VolumeRoute, ttl: Duration) -> Result<bool, RegistryError> {
        self.change(REFRESH, route, ttl).await
    }
    pub async fn remove(&self, route: &VolumeRoute) -> Result<bool, RegistryError> {
        self.change(REMOVE, route, Duration::from_secs(1)).await
    }
    // GETRANGE includes its end offset. Read one extra byte so a record with
    // the expected prefix and any trailing bytes can never confirm ownership.
    #[cfg(feature = "http3")]
    async fn matches_record(&self, key: &str, expected: &[u8]) -> Result<bool, RegistryError> {
        let mut command = redis::cmd("GETRANGE");
        command.arg(key).arg(0).arg(expected.len());
        let bytes: Vec<u8> = tokio::time::timeout(self.timeout, self.connection.query(&command))
            .await
            .map_err(|_| RegistryError::Unavailable)?
            .map_err(|_| RegistryError::Unavailable)?;
        Ok(bytes == expected)
    }

    /// A returned route is an untrusted discovery hint. The destination must
    /// authenticate and validate its live TiDB ownership before dispatch.
    pub async fn lookup(&self, volume: &str) -> Result<Option<VolumeRoute>, RegistryError> {
        // GETRANGE is inclusive, so this reads at most MAX_RECORD + 1 bytes:
        // the bound of the former STRLEN-guarded Lua script without its
        // per-call scripting cost. A missing key reads as empty, and an empty
        // value is never a valid record.
        let mut cmd = redis::cmd("GETRANGE");
        cmd.arg(self.key(volume)?).arg(0).arg(MAX_RECORD);
        let bytes: Vec<u8> = tokio::time::timeout(self.timeout, self.connection.query(&cmd))
            .await
            .map_err(|_| RegistryError::Unavailable)?
            .map_err(|_| RegistryError::Unavailable)?;
        if bytes.len() > MAX_RECORD {
            return Err(RegistryError::InvalidRecord);
        }
        let value = (!bytes.is_empty())
            .then(|| String::from_utf8(bytes).map_err(|_| RegistryError::InvalidRecord))
            .transpose()?;
        value
            .map(|encoded| {
                let json = encoded.get(21..).ok_or(RegistryError::InvalidRecord)?;
                let route: VolumeRoute =
                    serde_json::from_str(json).map_err(|_| RegistryError::InvalidRecord)?;
                if route.volume != volume
                    || route.encode().map_err(|_| RegistryError::InvalidRecord)? != encoded
                {
                    return Err(RegistryError::InvalidRecord);
                }
                Ok(route)
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn route(fence: u64) -> VolumeRoute {
        VolumeRoute {
            volume: "volume".into(),
            node: "node".into(),
            boot: format!("boot-{fence}"),
            registration: format!("registration-{fence}"),
            fence,
            address: "127.0.0.1:4443".parse().unwrap(),
            tls_server_name: "node.example".into(),
        }
    }
    #[test]
    fn records_validate_and_preserve_large_fence_order() {
        assert!(route(u64::MAX).encode().unwrap() > route(u64::MAX - 1).encode().unwrap());
        assert!(route(0).encode().is_err());
        let mut invalid = route(1);
        invalid.volume = "bad:key".into();
        assert!(invalid.encode().is_err());
    }
    #[cfg(feature = "http3")]
    #[tokio::test]
    #[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
    async fn redis_refresh_checks_the_entire_expected_record() {
        let client =
            redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
        let mut redis = client.get_multiplexed_async_connection().await.unwrap();
        let registry = RedisRouteRegistry::new(
            redis.clone(),
            format!("compare-{}", std::process::id()),
            Duration::from_secs(2),
        )
        .unwrap();
        let expected = route(1).encode().unwrap();
        let key = registry.key("volume").unwrap();
        for stored in [
            expected.clone(),
            format!("{expected}x"),
            expected[..expected.len() - 1].into(),
            route(2).encode().unwrap(),
            String::new(),
        ] {
            redis::cmd("SET")
                .arg(&key)
                .arg(&stored)
                .arg("EX")
                .arg(30)
                .query_async::<()>(&mut redis)
                .await
                .unwrap();
            assert_eq!(
                registry
                    .matches_record(&key, expected.as_bytes())
                    .await
                    .unwrap(),
                stored == expected
            );
        }
        redis::cmd("DEL")
            .arg(&key)
            .query_async::<i64>(&mut redis)
            .await
            .unwrap();
        assert!(
            !registry
                .matches_record(&key, expected.as_bytes())
                .await
                .unwrap()
        );
    }
    #[tokio::test]
    #[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
    async fn redis_lookup_is_bounded_and_exact() {
        let client =
            redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
        let mut redis = client.get_multiplexed_async_connection().await.unwrap();
        let registry = RedisRouteRegistry::new(
            redis.clone(),
            format!("lookup-{}", std::process::id()),
            Duration::from_secs(2),
        )
        .unwrap();
        let key = registry.key("volume").unwrap();
        assert_eq!(registry.lookup("volume").await.unwrap(), None);
        let expected = route(1);
        let record = expected.encode().unwrap();
        let padded = format!("{record}{}", " ".repeat(MAX_RECORD + 1 - record.len()));
        for (stored, valid) in [
            (record.clone(), true),
            (format!("{record}x"), false),
            (String::new(), false),
            (padded, false),
            ("x".repeat(64 * 1024), false),
        ] {
            redis::cmd("SET")
                .arg(&key)
                .arg(&stored)
                .arg("EX")
                .arg(30)
                .query_async::<()>(&mut redis)
                .await
                .unwrap();
            let found = registry.lookup("volume").await;
            if valid {
                assert_eq!(found.unwrap(), Some(expected.clone()));
            } else if stored.is_empty() {
                assert_eq!(found.unwrap(), None);
            } else {
                assert!(
                    matches!(found, Err(RegistryError::InvalidRecord)),
                    "{} bytes",
                    stored.len()
                );
            }
        }
        redis::cmd("DEL")
            .arg(&key)
            .query_async::<i64>(&mut redis)
            .await
            .unwrap();
    }
    #[tokio::test]
    #[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
    async fn redis_managed_registry_recovers_a_dropped_connection() {
        let url = std::env::var("MOUNT_RS_REDIS_TEST_URL").expect("isolated Redis URL");
        let client = redis::Client::open(url).unwrap();
        let mut manager = ConnectionManager::new(client.clone()).await.unwrap();
        let mut admin = client.get_multiplexed_async_connection().await.unwrap();
        let old_id: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut manager)
            .await
            .unwrap();
        let namespace = format!("reconnect-{}-{old_id}", std::process::id());
        let registry =
            RedisRouteRegistry::with_manager(manager.clone(), namespace, Duration::from_secs(1))
                .unwrap();
        let expected = route(1);
        assert!(
            registry
                .advertise(&expected, Duration::from_secs(10))
                .await
                .unwrap()
        );
        let killed: i64 = redis::cmd("CLIENT")
            .arg("KILL")
            .arg("ID")
            .arg(old_id)
            .query_async(&mut admin)
            .await
            .unwrap();
        assert_eq!(killed, 1);
        // Retry only the advisory lookup. A write error would need read-back
        // reconciliation, not blind replay of a filesystem mutation.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match registry.lookup("volume").await {
                    Ok(Some(found)) => {
                        assert_eq!(found, expected);
                        break;
                    }
                    Err(RegistryError::Unavailable) => {
                        tokio::time::sleep(Duration::from_millis(20)).await
                    }
                    other => panic!("unexpected reconnect result: {other:?}"),
                }
            }
        })
        .await
        .unwrap();
        let new_id: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut manager)
            .await
            .unwrap();
        assert_ne!(new_id, old_id);
        assert!(registry.remove(&expected).await.unwrap());
    }

    #[tokio::test]
    #[ignore = "requires scripts/test-redis-restart.sh and isolated restart markers"]
    async fn registration_recovers_after_server_restart() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        let markers = std::path::PathBuf::from(
            std::env::var("MOUNT_RS_REDIS_RESTART_MARKERS").expect("restart harness markers"),
        );
        let client =
            redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
        let manager = ConnectionManager::new_with_config(
            client,
            redis::aio::ConnectionManagerConfig::new()
                .set_min_delay(Duration::from_millis(50))
                .set_max_delay(Duration::from_millis(200))
                .set_number_of_retries(3)
                .set_connection_timeout(Some(Duration::from_secs(1)))
                .set_response_timeout(Some(Duration::from_millis(500))),
        )
        .await
        .unwrap();
        let registry = RedisRouteRegistry::with_manager(
            manager,
            format!("restart-{}", std::process::id()),
            Duration::from_millis(500),
        )
        .unwrap();
        let metadata =
            std::env::var("MOUNT_RS_REDIS_RESTART_METADATA").unwrap_or_else(|_| "memory".into());
        assert!(matches!(metadata.as_str(), "memory" | "tidb"));
        let mut options = mount_rs_sdk::SplitOptions::memory("restart-owner", 4096);
        if metadata == "tidb" {
            assert!(
                std::path::Path::new(
                    &std::env::var("MOUNT_RS_TIDB_RUN_DIR")
                        .expect("isolated TiDB harness required")
                )
                .is_dir()
            );
            let key = format!(
                "redis-restart-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            options.metadata = mount_rs_sdk::StoreConfig::Tidb {
                connection: std::env::var("MOUNT_RS_TIDB_URL").expect("TiDB harness URL"),
                volume_key: key,
                durable: true,
            };
            let path = markers.join("blocks");
            std::fs::create_dir(&path).unwrap();
            options.blocks = mount_rs_sdk::StoreConfig::Local { path };
        }
        let runtime = mount_rs_sdk::SplitRuntimeOptions {
            lease_ttl: Duration::from_secs(240),
            read_lease_cache_window: Duration::from_secs(60),
            atime_policy: mount_rs_sdk::AtimePolicy::NoAtime,
            ..Default::default()
        };
        let fs = Arc::new(
            mount_rs_sdk::Filesystem::split_with_runtime(options.clone(), runtime)
                .await
                .unwrap(),
        );
        let file = fs.driver().open("/retained", "w+", 0o600).await.unwrap();
        file.write(b"retained", Some(0)).await.unwrap();
        let expected = route(fs.checked_owner_fence().await.unwrap());
        let registration = Arc::new(
            OwnedVolumeRegistration::new(
                fs.clone(),
                registry.clone(),
                expected.clone(),
                Duration::from_secs(3),
            )
            .await
            .unwrap(),
        );
        let stop = tokio_util::sync::CancellationToken::new();
        let worker_stop = stop.clone();
        let (tx, mut rx) = tokio::sync::watch::channel(RegistrationStatus::Starting);
        let mut observe = rx.clone();
        let recoveries = Arc::new(AtomicUsize::new(0));
        let observed = recoveries.clone();
        let observer = tokio::spawn(async move {
            while observe.changed().await.is_ok() {
                if matches!(
                    *observe.borrow_and_update(),
                    RegistrationStatus::Recovering(_)
                ) {
                    observed.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
        let worker = tokio::spawn(async move {
            registration
                .run_with_recovery(
                    worker_stop,
                    tx,
                    RegistrationRecovery {
                        initial_delay: Duration::from_millis(100),
                        max_delay: Duration::from_millis(500),
                        max_attempts: 20,
                    },
                )
                .await
        });
        wait_registration(&mut rx, RegistrationStatus::Ready).await;
        std::fs::write(markers.join("ready"), b"ready").unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            while !markers.join("restarted").exists() {
                assert!(
                    !worker.is_finished(),
                    "registration stopped before Redis restart completed"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            loop {
                assert!(
                    !worker.is_finished(),
                    "registration stopped during recovery"
                );
                let ready = *rx.borrow() == RegistrationStatus::Ready;
                if recoveries.load(Ordering::SeqCst) > 0
                    && ready
                    && registry.lookup("volume").await.ok().flatten() == Some(expected.clone())
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(fs.checked_owner_fence().await.unwrap(), expected.fence);
        let mut bytes = [0u8; 8];
        assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), 8);
        assert_eq!(&bytes, b"retained");
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(3), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(*rx.borrow(), RegistrationStatus::Stopped);
        assert_eq!(registry.lookup("volume").await.unwrap(), None);
        file.close().await.unwrap();
        if metadata == "tidb" {
            let mut contender = options.clone();
            contender.owner = "redis-restart-contender".into();
            let error = mount_rs_sdk::Filesystem::split_with_runtime(contender, runtime)
                .await
                .err()
                .expect("active owner must exclude contender");
            assert_eq!(error.code, mount_rs_sdk::ErrorCode::Eagain);
        }
        fs.shutdown().await.unwrap();
        if metadata == "tidb" {
            options.owner = "redis-restart-reopened".into();
            let reopened = mount_rs_sdk::Filesystem::split_with_runtime(options, runtime)
                .await
                .unwrap();
            assert!(reopened.checked_owner_fence().await.unwrap() > expected.fence);
            let reopened_file = reopened.driver().open("/retained", "r", 0).await.unwrap();
            assert_eq!(reopened_file.read(&mut bytes, Some(0)).await.unwrap(), 8);
            assert_eq!(&bytes, b"retained");
            reopened_file.close().await.unwrap();
            reopened.shutdown().await.unwrap();
        }
        observer.await.unwrap();
        println!(
            "RESTART_METADATA_EVIDENCE={}",
            serde_json::json!({"metadata":metadata,"noatime":true,"lease_ttl_seconds":240,"serving_window_seconds":60,"contender_rejection_and_reopen_verified":metadata=="tidb"})
        );
        println!(
            "REDIS_RESTART_EVIDENCE={{\"restored_route\":true,\"owner_fence_unchanged\":true,\"open_handle_bytes_preserved\":true,\"recovering_notifications\":{}}}",
            recoveries.load(Ordering::SeqCst)
        );
    }

    async fn wait_registration(
        receiver: &mut tokio::sync::watch::Receiver<RegistrationStatus>,
        expected: RegistrationStatus,
    ) {
        tokio::time::timeout(Duration::from_secs(3), async {
            while *receiver.borrow_and_update() != expected {
                receiver.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
    async fn redis_recovers_one_hundred_independent_volume_routes() {
        let client = redis::Client::open(
            std::env::var("MOUNT_RS_REDIS_TEST_URL").expect("isolated Redis URL"),
        )
        .unwrap();
        let registry = RedisRouteRegistry::with_manager(
            ConnectionManager::new(client).await.unwrap(),
            format!("recovery-100-{}", std::process::id()),
            Duration::from_secs(1),
        )
        .unwrap();
        let stop = tokio_util::sync::CancellationToken::new();
        let mut workers = tokio::task::JoinSet::new();
        let mut volumes = Vec::new();
        for index in 0..100 {
            let fs = std::sync::Arc::new(
                mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory(
                    format!("owner-{index}"),
                    4096,
                ))
                .await
                .unwrap(),
            );
            let mut expected = route(fs.checked_owner_fence().await.unwrap());
            expected.volume = format!("volume-{index}");
            let registration = OwnedVolumeRegistration::new(
                fs.clone(),
                registry.clone(),
                expected.clone(),
                Duration::from_secs(3),
            )
            .await
            .unwrap();
            let (tx, rx) = tokio::sync::watch::channel(RegistrationStatus::Starting);
            let cancel = stop.clone();
            workers.spawn(async move {
                registration
                    .run_with_recovery(
                        cancel,
                        tx,
                        RegistrationRecovery {
                            initial_delay: Duration::from_millis(100),
                            max_delay: Duration::from_millis(500),
                            max_attempts: 10,
                        },
                    )
                    .await
            });
            volumes.push((fs, expected, rx));
        }
        for (_, _, rx) in &mut volumes {
            wait_registration(rx, RegistrationStatus::Ready).await;
        }
        // Remove every exact hint concurrently so all owners see the same
        // control-plane outage. No filesystem ownership is changed.
        let mut removals = tokio::task::JoinSet::new();
        for (_, expected, _) in &volumes {
            let registry = registry.clone();
            let expected = expected.clone();
            removals.spawn(async move { registry.remove(&expected).await.unwrap() });
        }
        while let Some(removed) = removals.join_next().await {
            assert!(removed.unwrap());
        }
        tokio::time::timeout(Duration::from_secs(10), async {
            for (fs, expected, rx) in &mut volumes {
                loop {
                    if registry.lookup(&expected.volume).await.unwrap().as_ref() == Some(expected)
                        && *rx.borrow() == RegistrationStatus::Ready
                    {
                        break;
                    }
                    assert!(!matches!(
                        *rx.borrow(),
                        RegistrationStatus::Failed(_) | RegistrationStatus::Stopped
                    ));
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                assert_eq!(fs.checked_owner_fence().await.unwrap(), expected.fence);
            }
        })
        .await
        .unwrap();
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(result) = workers.join_next().await {
                result.unwrap().unwrap();
            }
        })
        .await
        .unwrap();
        for (fs, expected, rx) in volumes {
            assert_eq!(*rx.borrow(), RegistrationStatus::Stopped);
            assert_eq!(registry.lookup(&expected.volume).await.unwrap(), None);
            fs.shutdown().await.unwrap();
        }
        println!(
            "REDIS_RECOVERY_SCALE_EVIDENCE volumes=100 restored=100 fences_preserved=100 withdrawn=100"
        );
    }

    #[tokio::test]
    #[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
    async fn redis_route_takeover_expiry_and_stale_cleanup() {
        let url = std::env::var("MOUNT_RS_REDIS_TEST_URL").expect("isolated Redis URL");
        let client = redis::Client::open(url).unwrap();
        let connection = client.get_multiplexed_async_connection().await.unwrap();
        let namespace = format!(
            "test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let registry =
            RedisRouteRegistry::new(connection, namespace, Duration::from_secs(2)).unwrap();
        let first = route(u64::MAX - 1);
        let second = route(u64::MAX);
        let ttl = Duration::from_secs(5);
        assert!(registry.advertise(&first, ttl).await.unwrap());
        assert_eq!(
            registry.lookup("volume").await.unwrap(),
            Some(first.clone())
        );
        assert!(registry.advertise(&second, ttl).await.unwrap());
        assert!(!registry.advertise(&first, ttl).await.unwrap());
        assert!(!registry.refresh(&first, ttl).await.unwrap());
        assert!(!registry.remove(&first).await.unwrap());
        assert_eq!(
            registry.lookup("volume").await.unwrap(),
            Some(second.clone())
        );
        assert!(
            registry
                .refresh(&second, Duration::from_millis(20))
                .await
                .unwrap()
        );
        tokio::time::sleep(Duration::from_millis(60)).await;
        assert_eq!(registry.lookup("volume").await.unwrap(), None);
        assert!(!registry.refresh(&second, ttl).await.unwrap());
        assert!(registry.advertise(&second, ttl).await.unwrap());
        assert!(registry.remove(&second).await.unwrap());
        assert_eq!(registry.lookup("volume").await.unwrap(), None);

        // SDK lifecycle, not a caller-supplied boolean, gates registration.
        let fs = std::sync::Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory(
                "registered-owner",
                4096,
            ))
            .await
            .unwrap(),
        );
        let current = route(fs.checked_owner_fence().await.unwrap());
        let registration =
            OwnedVolumeRegistration::new(fs.clone(), registry.clone(), current.clone(), ttl)
                .await
                .unwrap();
        registration.advertise().await.unwrap();
        registration.refresh().await.unwrap();
        registration.check_destination(&current).await.unwrap();
        let supervised = std::sync::Arc::new(
            OwnedVolumeRegistration::new(
                fs.clone(),
                registry.clone(),
                current.clone(),
                Duration::from_millis(600),
            )
            .await
            .unwrap(),
        );
        let stop = tokio_util::sync::CancellationToken::new();
        let (status_tx, mut status_rx) = tokio::sync::watch::channel(RegistrationStatus::Starting);
        let worker = supervised.clone();
        let worker_stop = stop.clone();
        let task = tokio::spawn(async move { worker.run(worker_stop, status_tx).await });
        tokio::time::timeout(Duration::from_secs(3), async {
            while *status_rx.borrow_and_update() != RegistrationStatus::Ready {
                status_rx.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(1300)).await;
        assert_eq!(
            registry.lookup("volume").await.unwrap(),
            Some(current.clone())
        );
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(*status_rx.borrow(), RegistrationStatus::Stopped);
        assert_eq!(registry.lookup("volume").await.unwrap(), None);

        // Redis route loss is an availability event, not an ownership grant
        // or revocation. The registration worker must leave Ready and require
        // explicit supervisor reconciliation rather than silently advertising.
        let (loss_tx, loss_rx) = tokio::sync::watch::channel(RegistrationStatus::Starting);
        let mut ready_rx = loss_rx.clone();
        let worker = supervised.clone();
        let lost_route = tokio::spawn(async move {
            worker
                .run(tokio_util::sync::CancellationToken::new(), loss_tx)
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while *ready_rx.borrow_and_update() != RegistrationStatus::Ready {
                ready_rx.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(registry.remove(&current).await.unwrap());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), lost_route)
                .await
                .unwrap()
                .unwrap(),
            Err(RegistrationError::Conflict)
        );
        assert_eq!(
            *loss_rx.borrow(),
            RegistrationStatus::Failed(RegistrationError::Conflict)
        );
        assert_eq!(registry.lookup("volume").await.unwrap(), None);
        assert_eq!(fs.checked_owner_fence().await.unwrap(), current.fence);
        // A deliberate restart revalidates the active owner before publishing
        // a routing hint. It does not acquire a new filesystem generation.
        registration.reconcile().await.unwrap();
        assert_eq!(
            registry.lookup("volume").await.unwrap(),
            Some(current.clone())
        );
        registration.reconcile().await.unwrap();
        assert_eq!(fs.checked_owner_fence().await.unwrap(), current.fence);
        assert!(registry.remove(&current).await.unwrap());
        let conflicting = route(current.fence + 1);
        assert!(registry.advertise(&conflicting, ttl).await.unwrap());
        assert_eq!(
            registration.reconcile().await,
            Err(RegistrationError::Conflict)
        );
        assert_eq!(
            registry.lookup("volume").await.unwrap(),
            Some(conflicting.clone())
        );
        assert!(registry.remove(&conflicting).await.unwrap());
        registration.reconcile().await.unwrap();
        let recovery = RegistrationRecovery {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(200),
            max_attempts: 3,
        };
        let recovery_stop = tokio_util::sync::CancellationToken::new();
        let worker_stop = recovery_stop.clone();
        let worker = supervised.clone();
        let (recovery_tx, mut recovery_rx) =
            tokio::sync::watch::channel(RegistrationStatus::Starting);
        let recovering = tokio::spawn(async move {
            worker
                .run_with_recovery(worker_stop, recovery_tx, recovery)
                .await
        });
        wait_registration(&mut recovery_rx, RegistrationStatus::Ready).await;
        assert!(registry.remove(&current).await.unwrap());
        wait_registration(
            &mut recovery_rx,
            RegistrationStatus::Recovering(RegistrationError::Conflict),
        )
        .await;
        wait_registration(&mut recovery_rx, RegistrationStatus::Ready).await;
        assert_eq!(
            registry.lookup("volume").await.unwrap(),
            Some(current.clone())
        );
        assert_eq!(fs.checked_owner_fence().await.unwrap(), current.fence);
        // Cancel a second outage during backoff rather than healthy refresh.
        assert!(registry.remove(&current).await.unwrap());
        wait_registration(
            &mut recovery_rx,
            RegistrationStatus::Recovering(RegistrationError::Conflict),
        )
        .await;
        recovery_stop.cancel();
        tokio::time::timeout(Duration::from_secs(3), recovering)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(*recovery_rx.borrow(), RegistrationStatus::Stopped);
        assert_eq!(registry.lookup("volume").await.unwrap(), None);
        registration.reconcile().await.unwrap();
        let worker = supervised.clone();
        let (conflict_tx, mut conflict_rx) =
            tokio::sync::watch::channel(RegistrationStatus::Starting);
        let conflicting_worker = tokio::spawn(async move {
            worker
                .run_with_recovery(
                    tokio_util::sync::CancellationToken::new(),
                    conflict_tx,
                    recovery,
                )
                .await
        });
        wait_registration(&mut conflict_rx, RegistrationStatus::Ready).await;
        assert!(registry.remove(&current).await.unwrap());
        assert!(registry.advertise(&conflicting, ttl).await.unwrap());
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), conflicting_worker)
                .await
                .unwrap()
                .unwrap(),
            Err(RegistrationError::Conflict)
        );
        assert_eq!(
            *conflict_rx.borrow(),
            RegistrationStatus::Failed(RegistrationError::Conflict)
        );
        assert_eq!(
            registry.lookup("volume").await.unwrap(),
            Some(conflicting.clone())
        );
        assert_eq!(fs.checked_owner_fence().await.unwrap(), current.fence);
        assert!(registry.remove(&conflicting).await.unwrap());
        registration.reconcile().await.unwrap();

        let mut wrong_boot = current.clone();
        wrong_boot.boot = "other-process".into();
        assert_eq!(
            registration.check_destination(&wrong_boot).await,
            Err(RegistrationError::Conflict)
        );
        let (status_tx, mut status_rx) = tokio::sync::watch::channel(RegistrationStatus::Starting);
        let worker = supervised.clone();
        let task = tokio::spawn(async move {
            worker
                .run(tokio_util::sync::CancellationToken::new(), status_tx)
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), async {
            while *status_rx.borrow_and_update() != RegistrationStatus::Ready {
                status_rx.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        fs.shutdown().await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(3), task)
                .await
                .unwrap()
                .unwrap(),
            Err(RegistrationError::OwnershipLost)
        );
        assert_eq!(
            *status_rx.borrow(),
            RegistrationStatus::Failed(RegistrationError::OwnershipLost)
        );
        assert_eq!(
            registration.refresh().await,
            Err(RegistrationError::OwnershipLost)
        );
        assert_eq!(
            registration.reconcile().await,
            Err(RegistrationError::OwnershipLost)
        );
        assert_eq!(registry.lookup("volume").await.unwrap(), None);
        let successor = route(current.fence + 1);
        assert!(registry.advertise(&successor, ttl).await.unwrap());
        assert_eq!(
            registration.advertise().await,
            Err(RegistrationError::OwnershipLost)
        );
        assert!(!registration.withdraw().await.unwrap());
        assert_eq!(
            registry.lookup("volume").await.unwrap(),
            Some(successor.clone())
        );
        registry.remove(&successor).await.unwrap();
    }
}

#[cfg(feature = "http3")]
mod route_cache;
