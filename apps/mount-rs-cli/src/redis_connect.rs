//! Opens the cluster route registry's Redis connection.
//!
//! The URL comes from an environment variable and may carry a password, so
//! nothing here formats the URL itself: errors name the [`RedisTarget`], which
//! is the scheme, host and port only, and any text that came from the Redis
//! client is scrubbed of the password before it is reported.
//!
//! With `http.forwarding.redis_sentinel` the registry is the primary of a
//! Redis Sentinel deployment instead. The sentinels and the servers they
//! name are held to the same rules: TLS that cannot be turned off, the same
//! optional CA, and no plaintext to another host without the opt-in.
use crate::{
    config::{ForwardingConfig, RedisSentinelConfig, is_loopback_host, is_plain_host},
    runtime::CliError,
};
use redis::{ConnectionAddr, ConnectionInfo, IntoConnectionInfo};
use std::{fmt, sync::Arc, time::Duration};

/// How long startup waits for the first connection, handshake and login.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// The registry's bound on each Redis command.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// The bound on each step of finding the primary through the sentinels:
/// connecting to a sentinel, its answer, connecting to the server it names
/// and that server's answer.
const SENTINEL_STEP_TIMEOUT: Duration = Duration::from_secs(2);

/// Where a Redis URL points, without its credentials, database or query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RedisTarget {
    tls: bool,
    /// `host:port`, or the path of a Unix socket.
    endpoint: String,
    unix: bool,
}

impl fmt::Display for RedisTarget {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.unix, self.tls) {
            (true, _) => write!(formatter, "unix:{}", self.endpoint),
            (false, true) => write!(formatter, "rediss://{}", self.endpoint),
            (false, false) => write!(formatter, "redis://{}", self.endpoint),
        }
    }
}

/// Why a Redis URL was refused before any connection was attempted. No
/// variant carries the URL or any part of its credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RedisUrlError {
    /// Not a URL the Redis client understands.
    Invalid,
    /// `rediss://…#insecure`, which would turn certificate verification off.
    InsecureTls,
    /// Plaintext to a host other than this one, without the explicit opt-in.
    PlaintextRefused(RedisTarget),
    /// A CA was configured for a connection that does not use TLS.
    CaWithoutTls(RedisTarget),
}

impl fmt::Display for RedisUrlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => formatter.write_str(
                "invalid forwarding Redis URL: expected rediss://[user:password@]host[:port][/db] \
                 or redis://[user:password@]host[:port][/db]",
            ),
            Self::InsecureTls => formatter.write_str(
                "the forwarding Redis URL ends in #insecure, which would disable certificate \
                 verification and is not supported; for a private CA set \
                 http.forwarding.redis_ca instead",
            ),
            Self::PlaintextRefused(target) => write!(
                formatter,
                "refusing an unencrypted Redis connection to {target}: use a rediss:// URL, or \
                 set http.forwarding.allow_plaintext_redis to true if the network between this \
                 server and Redis is trusted"
            ),
            Self::CaWithoutTls(target) => write!(
                formatter,
                "http.forwarding.redis_ca is set but the Redis connection to {target} does not \
                 use TLS; use a rediss:// URL or remove redis_ca"
            ),
        }
    }
}

/// A Redis URL that has passed the connection policy.
pub(crate) struct CheckedRedisUrl {
    info: ConnectionInfo,
    target: RedisTarget,
}

impl CheckedRedisUrl {
    /// Parse `url` and apply the policy: TLS verification cannot be turned
    /// off, plaintext is allowed only to this host unless `allow_plaintext`,
    /// and a CA is only meaningful with TLS.
    pub(crate) fn parse(
        url: &str,
        allow_plaintext: bool,
        has_ca: bool,
    ) -> Result<Self, RedisUrlError> {
        // The client's own error can quote the URL, so it is dropped.
        let info = url
            .into_connection_info()
            .map_err(|_| RedisUrlError::Invalid)?;
        let target = match info.addr() {
            ConnectionAddr::Tcp(host, port) => RedisTarget {
                tls: false,
                endpoint: endpoint(host, *port),
                unix: false,
            },
            ConnectionAddr::TcpTls {
                host,
                port,
                insecure,
                ..
            } => {
                if *insecure {
                    return Err(RedisUrlError::InsecureTls);
                }
                RedisTarget {
                    tls: true,
                    endpoint: endpoint(host, *port),
                    unix: false,
                }
            }
            ConnectionAddr::Unix(path) => RedisTarget {
                tls: false,
                endpoint: path.display().to_string(),
                unix: true,
            },
            // The address type is non-exhaustive; nothing else is supported.
            _ => return Err(RedisUrlError::Invalid),
        };
        if let ConnectionAddr::Tcp(host, _) = info.addr()
            && !allow_plaintext
            && !is_loopback_host(host)
        {
            return Err(RedisUrlError::PlaintextRefused(target));
        }
        if has_ca && !target.tls {
            return Err(RedisUrlError::CaWithoutTls(target));
        }
        Ok(Self { info, target })
    }

    pub(crate) fn target(&self) -> &RedisTarget {
        &self.target
    }

    /// Replace every occurrence of this URL's password in `text`.
    fn scrub(&self, text: &str) -> String {
        match self.info.redis_settings().password() {
            Some(password) if !password.is_empty() => text.replace(password, "[redacted]"),
            _ => text.to_owned(),
        }
    }
}

fn endpoint(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// Connect to the registry named by `config`, enforcing the URL policy.
pub(crate) async fn connect_registry(
    config: &ForwardingConfig,
) -> Result<mount_rs_cluster::RedisRouteRegistry, CliError> {
    let ca = match &config.redis_ca {
        None => None,
        Some(path) => Some(read_ca(path)?),
    };
    if let Some(sentinel) = &config.redis_sentinel {
        return connect_sentinel(config, sentinel, ca).await;
    }
    let Some(redis_url) = &config.redis_url else {
        // Configuration parsing requires one of the two.
        return Err(CliError::usage(
            "http.forwarding requires redis_url or redis_sentinel",
        ));
    };
    let url = env_value(&redis_url.name)?;
    let checked = CheckedRedisUrl::parse(&url, config.allow_plaintext_redis, ca.is_some())
        .map_err(|error| CliError::usage(error.to_string()))?;
    let client = connect(&checked, ca).await.map_err(CliError::runtime)?;
    // The registry reconnects by itself from here on, and replaces a
    // connection that stops answering.
    mount_rs_cluster::RedisRouteRegistry::with_client(
        client,
        redis::aio::ConnectionManagerConfig::new(),
        config.namespace.clone(),
        REQUEST_TIMEOUT,
    )
    .map_err(|e| CliError::runtime(e.to_string()))
}

fn env_value(name: &str) -> Result<String, CliError> {
    std::env::var(name)
        .map_err(|_| CliError::runtime(format!("missing environment variable {name}")))
}

/// The TLS setting, credentials and address rules shared by every
/// connection of a Sentinel deployment. `Debug` is not derived: this holds
/// passwords.
struct SentinelPolicy {
    tls: bool,
    allow_plaintext: bool,
    ca: Option<Vec<u8>>,
    /// For the Redis servers.
    data: redis::RedisConnectionInfo,
    /// For the sentinels.
    sentinel: redis::RedisConnectionInfo,
    /// Every password in use, for scrubbing error text.
    secrets: Vec<String>,
}

impl SentinelPolicy {
    fn new(
        config: &RedisSentinelConfig,
        allow_plaintext: bool,
        ca: Option<Vec<u8>>,
        password: Option<String>,
        sentinel_password: Option<String>,
    ) -> Self {
        let settings = |username: &Option<String>, password: &Option<String>| {
            let mut settings = redis::RedisConnectionInfo::default();
            if let Some(username) = username {
                settings = settings.set_username(username);
            }
            if let Some(password) = password {
                settings = settings.set_password(password);
            }
            settings
        };
        Self {
            tls: config.tls,
            allow_plaintext,
            data: settings(&config.username, &password).set_db(i64::from(config.database)),
            // A sentinel has no databases to select.
            sentinel: settings(&config.sentinel_username, &sentinel_password),
            secrets: [password, sentinel_password]
                .into_iter()
                .flatten()
                .filter(|secret| !secret.is_empty())
                .collect(),
            ca,
        }
    }

    fn target(&self, host: &str, port: u16) -> RedisTarget {
        RedisTarget {
            tls: self.tls,
            endpoint: endpoint(host, port),
            unix: false,
        }
    }

    fn scrub(&self, text: &str) -> String {
        self.secrets.iter().fold(text.to_owned(), |text, secret| {
            text.replace(secret, "[redacted]")
        })
    }

    /// A client for `host:port`, which is a sentinel or a server one named.
    /// The address is refused if it is not a plain host name or IP address,
    /// or if reaching it would be plaintext to another host without the
    /// opt-in. The error text is safe to print.
    fn client(
        &self,
        host: &str,
        port: u16,
        settings: &redis::RedisConnectionInfo,
    ) -> Result<redis::Client, String> {
        let target = self.target(host, port);
        let literal = host.parse::<std::net::Ipv6Addr>().is_ok();
        if port == 0 || !(literal || is_plain_host(host)) {
            return Err("a sentinel named an address that is not a host and port".to_owned());
        }
        if !self.tls && !self.allow_plaintext && !is_loopback_host(host) {
            return Err(format!(
                "refusing an unencrypted Redis connection to {target}: leave \
                 http.forwarding.redis_sentinel.tls on, or set \
                 http.forwarding.allow_plaintext_redis to true if the network between this \
                 server, the sentinels and the Redis servers is trusted"
            ));
        }
        let configure = |error: redis::RedisError| {
            format!(
                "forwarding Redis connection to {target} cannot be configured: {}",
                self.scrub(&error.to_string())
            )
        };
        // The scheme decides TLS; the address was checked above and holds
        // no credentials.
        let info = target
            .to_string()
            .into_connection_info()
            .map_err(configure)?
            .set_redis_settings(settings.clone());
        build_client(info, self.ca.clone()).map_err(configure)
    }
}

/// Connect to the primary of the Sentinel deployment named by `sentinel`.
async fn connect_sentinel(
    config: &ForwardingConfig,
    sentinel: &RedisSentinelConfig,
    ca: Option<Vec<u8>>,
) -> Result<mount_rs_cluster::RedisRouteRegistry, CliError> {
    let secret = |reference: &Option<crate::config::EnvReference>| {
        reference
            .as_ref()
            .map(|reference| env_value(&reference.name))
            .transpose()
    };
    let policy = Arc::new(SentinelPolicy::new(
        sentinel,
        config.allow_plaintext_redis,
        ca,
        secret(&sentinel.password)?,
        secret(&sentinel.sentinel_password)?,
    ));
    if policy.tls {
        // See `connect`: one TLS provider has to be chosen for the client.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let sentinels = sentinel
        .sentinels
        .iter()
        .map(|address| policy.client(&address.host, address.port, &policy.sentinel))
        .collect::<Result<Vec<_>, _>>()
        .map_err(CliError::usage)?;
    let count = sentinels.len();
    let factory = policy.clone();
    let primary = mount_rs_cluster::SentinelPrimary::new(
        sentinels,
        sentinel.service.clone(),
        Arc::new(move |host, port| factory.client(host, port, &factory.data)),
        SENTINEL_STEP_TIMEOUT,
    )
    .ok_or_else(|| CliError::usage("invalid http.forwarding.redis_sentinel"))?;
    // Find the primary once before reporting ready, so that a wrong
    // address, service name, certificate or password is reported here.
    let failed = |detail: String| {
        CliError::runtime(format!(
            "forwarding Redis Sentinel: no primary for service {:?} through {count} \
             sentinel(s): {}",
            sentinel.service,
            policy.scrub(&detail)
        ))
    };
    let resolved = tokio::time::timeout(CONNECT_TIMEOUT * 3, primary.resolve())
        .await
        .map_err(|_| failed("timed out".to_owned()))?
        .map_err(|error| failed(error.to_string()))?;
    mount_rs_cluster::RedisRouteRegistry::with_sentinel(
        primary,
        Some(resolved),
        config.namespace.clone(),
        REQUEST_TIMEOUT,
    )
    .map_err(|e| CliError::runtime(e.to_string()))
}

/// A client that trusts only `ca` when one is given, and the system roots
/// otherwise.
fn build_client(info: ConnectionInfo, ca: Option<Vec<u8>>) -> redis::RedisResult<redis::Client> {
    match ca {
        // With a CA, only that bundle is trusted for this connection.
        Some(root_cert) => redis::Client::build_with_tls(
            info,
            redis::TlsCertificates {
                client_tls: None,
                root_cert: Some(root_cert),
            },
        ),
        // Without one, a TLS server is verified against the system roots.
        None => redis::Client::open(info),
    }
}

/// Read a PEM bundle and require at least one certificate in it, so that a
/// wrong file is reported as such rather than as a failed handshake.
fn read_ca(path: &std::path::Path) -> Result<Vec<u8>, CliError> {
    let pem = std::fs::read(path).map_err(|error| {
        CliError::runtime(format!(
            "cannot read http.forwarding.redis_ca {}: {error}",
            path.display()
        ))
    })?;
    let certificates = rustls_pemfile::certs(&mut pem.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_default();
    if certificates.is_empty() {
        return Err(CliError::usage(format!(
            "http.forwarding.redis_ca {} contains no PEM certificate",
            path.display()
        )));
    }
    Ok(pem)
}

/// Build the client and prove it once: connect, complete the handshake and
/// the login, and run a command. The returned error text is safe to print.
async fn connect(checked: &CheckedRedisUrl, ca: Option<Vec<u8>>) -> Result<redis::Client, String> {
    let target = checked.target();
    if target.tls {
        // The Redis client builds its TLS configuration from the process
        // default provider, and this binary links more than one, so one has
        // to be chosen. An error only means one is already installed.
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let client = build_client(checked.info.clone(), ca).map_err(|error| {
        format!(
            "forwarding Redis connection to {target} cannot be configured: {}",
            checked.scrub(&error.to_string())
        )
    })?;
    // One plain attempt, not the reconnecting client: that one retries its
    // first connection with backoff for longer than the startup timeout,
    // which would report a refused certificate or password only as a timeout.
    let config = redis::AsyncConnectionConfig::new().set_connection_timeout(Some(CONNECT_TIMEOUT));
    let timed_out = || format!("forwarding Redis connection to {target} timed out");
    let failed = |error: redis::RedisError| {
        format!(
            "forwarding Redis connection to {target} failed: {}",
            checked.scrub(&error.to_string())
        )
    };
    let mut probe = tokio::time::timeout(
        CONNECT_TIMEOUT,
        client.get_multiplexed_async_connection_with_config(&config),
    )
    .await
    .map_err(|_| timed_out())?
    .map_err(failed)?;
    // A URL without credentials connects to a server that requires them and
    // only fails at the first command, so send one before reporting ready.
    tokio::time::timeout(
        CONNECT_TIMEOUT,
        redis::cmd("PING").query_async::<String>(&mut probe),
    )
    .await
    .map_err(|_| timed_out())?
    .map_err(failed)?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &str = "s3cr3t-registry-pass";

    fn parse(url: &str, allow_plaintext: bool, has_ca: bool) -> Result<String, RedisUrlError> {
        CheckedRedisUrl::parse(url, allow_plaintext, has_ca)
            .map(|checked| checked.target().to_string())
    }

    #[test]
    fn tls_urls_are_accepted_for_any_host_with_and_without_credentials() {
        assert_eq!(
            parse("rediss://redis.internal/", false, false).unwrap(),
            "rediss://redis.internal:6379"
        );
        assert_eq!(
            parse(
                &format!("rediss://svc:{PASSWORD}@redis.internal:6380/2"),
                false,
                true
            )
            .unwrap(),
            "rediss://redis.internal:6380"
        );
        assert_eq!(
            parse(
                &format!("rediss://:{PASSWORD}@[2001:db8::1]:6380"),
                false,
                false
            )
            .unwrap(),
            "rediss://[2001:db8::1]:6380"
        );
        let checked =
            CheckedRedisUrl::parse("rediss://svc:p%40ss%2Fword@10.0.0.5:6380/", false, false)
                .unwrap();
        let settings = checked.info.redis_settings();
        assert_eq!(settings.username(), Some("svc"));
        // Reserved characters in a password are percent-encoded in the URL.
        assert_eq!(settings.password(), Some("p@ss/word"));
    }

    #[test]
    fn plaintext_is_allowed_only_to_this_host_unless_explicitly_enabled() {
        for local in [
            "redis://127.0.0.1:6379/",
            "redis://localhost/",
            "redis://LOCALHOST:7000/",
            "redis://[::1]:6379/",
            "redis://127.8.9.10/",
        ] {
            assert!(parse(local, false, false).is_ok(), "{local}");
        }
        for remote in [
            "redis://redis.internal:6379/",
            "redis://10.0.0.5/",
            "redis://[2001:db8::1]:6379/",
            // A name is not assumed to be this host, whatever it resolves to.
            "redis://localhost.example.com/",
            "redis://redis/",
        ] {
            assert!(
                matches!(
                    parse(remote, false, false),
                    Err(RedisUrlError::PlaintextRefused(_))
                ),
                "{remote}"
            );
            assert!(parse(remote, true, false).is_ok(), "{remote}");
        }
        let refused = parse("redis://10.0.0.5:6380/", false, false).unwrap_err();
        let message = refused.to_string();
        assert!(message.contains("redis://10.0.0.5:6380"), "{message}");
        assert!(message.contains("allow_plaintext_redis"), "{message}");
        assert!(message.contains("rediss://"), "{message}");
    }

    #[test]
    fn certificate_verification_cannot_be_disabled_from_the_url() {
        assert_eq!(
            parse("rediss://redis.internal/#insecure", true, false),
            Err(RedisUrlError::InsecureTls)
        );
        assert_eq!(
            parse("rediss://redis.internal/#other", true, false),
            Err(RedisUrlError::Invalid)
        );
    }

    #[test]
    fn a_ca_requires_a_tls_url() {
        assert!(matches!(
            parse("redis://127.0.0.1/", false, true),
            Err(RedisUrlError::CaWithoutTls(_))
        ));
        assert!(matches!(
            parse("redis://10.0.0.5/", true, true),
            Err(RedisUrlError::CaWithoutTls(_))
        ));
        assert!(parse("rediss://127.0.0.1/", false, true).is_ok());
    }

    #[test]
    fn unsupported_and_malformed_urls_are_refused() {
        for invalid in [
            "",
            "redis.internal:6379",
            "http://redis.internal/",
            "rediss://",
            "redis://0.0.0.0/",
            "rediss://redis.internal/not-a-database",
        ] {
            assert_eq!(
                parse(invalid, true, false),
                Err(RedisUrlError::Invalid),
                "{invalid}"
            );
        }
    }

    // The Redis client only understands `unix://` addresses on Unix.
    #[cfg(unix)]
    #[test]
    fn a_unix_socket_is_local_and_never_tls() {
        assert_eq!(
            parse("unix:///run/redis.sock", false, false).unwrap(),
            "unix:/run/redis.sock"
        );
        assert!(matches!(
            parse("redis+unix:///run/redis.sock", false, true),
            Err(RedisUrlError::CaWithoutTls(_))
        ));
    }

    /// Every message this module can build from a URL, for a URL that has a
    /// username and a password: none may contain either.
    #[test]
    fn no_error_or_target_text_contains_the_credentials() {
        let user = "registry-user";
        let urls = [
            format!("redis://{user}:{PASSWORD}@10.0.0.5:6379/"),
            format!("redis://{user}:{PASSWORD}@127.0.0.1:6379/"),
            format!("rediss://{user}:{PASSWORD}@redis.internal:6380/3"),
            format!("rediss://{user}:{PASSWORD}@redis.internal/#insecure"),
            format!("rediss://{user}:{PASSWORD}@redis.internal/#other"),
            format!("rediss://{user}:{PASSWORD}@redis.internal/not-a-database"),
            format!("rediss://{user}:{PASSWORD}@redis.internal:notaport/"),
            format!("http://{user}:{PASSWORD}@redis.internal/"),
            format!("unix://{user}:{PASSWORD}@/run/redis.sock"),
            format!("{user}:{PASSWORD}@redis.internal"),
        ];
        let mut checked_some_success = false;
        let mut checked_some_failure = false;
        for url in &urls {
            for (allow_plaintext, has_ca) in
                [(false, false), (true, false), (false, true), (true, true)]
            {
                let text = match CheckedRedisUrl::parse(url, allow_plaintext, has_ca) {
                    Ok(checked) => {
                        checked_some_success = true;
                        format!("{} {:?}", checked.target(), checked.target())
                    }
                    Err(error) => {
                        checked_some_failure = true;
                        format!("{error} {error:?} {}", CliError::usage(error.to_string()))
                    }
                };
                assert!(!text.contains(PASSWORD), "{text}");
                assert!(!text.contains(user), "{text}");
            }
        }
        assert!(checked_some_success && checked_some_failure);
    }

    #[test]
    fn client_error_text_is_scrubbed_of_the_password() {
        let checked = CheckedRedisUrl::parse(
            &format!("rediss://svc:{PASSWORD}@redis.internal/"),
            false,
            false,
        )
        .unwrap();
        assert_eq!(
            checked.scrub(&format!("AUTH svc {PASSWORD} failed: {PASSWORD}")),
            "AUTH svc [redacted] failed: [redacted]"
        );
        let open = CheckedRedisUrl::parse("rediss://redis.internal/", false, false).unwrap();
        assert_eq!(open.scrub("connection refused"), "connection refused");
    }

    #[tokio::test]
    async fn a_failed_connection_reports_the_target_without_the_password() {
        // A loopback port with nothing listening on it.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        for scheme in ["redis", "rediss"] {
            let checked = CheckedRedisUrl::parse(
                &format!("{scheme}://svc:{PASSWORD}@127.0.0.1:{port}/"),
                false,
                false,
            )
            .unwrap();
            let error = connect(&checked, None).await.err().unwrap();
            assert!(
                error.contains(&format!("{scheme}://127.0.0.1:{port}")),
                "{error}"
            );
            assert!(!error.contains(PASSWORD), "{error}");
            assert!(!error.contains("svc"), "{error}");
        }
    }

    fn policy(tls: bool, allow_plaintext: bool) -> SentinelPolicy {
        let config = RedisSentinelConfig {
            sentinels: Vec::new(),
            service: "mount-rs".into(),
            tls,
            username: Some("registry-user".into()),
            password: None,
            sentinel_username: None,
            sentinel_password: None,
            database: 2,
        };
        SentinelPolicy::new(
            &config,
            allow_plaintext,
            None,
            Some(PASSWORD.into()),
            Some("s3ntinel-pass".into()),
        )
    }

    #[test]
    fn a_server_named_by_a_sentinel_is_held_to_the_plaintext_rule() {
        let strict = policy(false, false);
        for local in ["127.0.0.1", "localhost", "::1"] {
            assert!(strict.client(local, 6379, &strict.data).is_ok(), "{local}");
        }
        for remote in ["10.0.0.5", "redis-1.internal", "2001:db8::1"] {
            let error = strict.client(remote, 6379, &strict.data).unwrap_err();
            assert!(error.contains("refusing an unencrypted"), "{error}");
            assert!(error.contains("allow_plaintext_redis"), "{error}");
            assert!(!error.contains(PASSWORD), "{error}");
            assert!(
                policy(false, true)
                    .client(remote, 6379, &strict.data)
                    .is_ok()
            );
            assert!(
                policy(true, false)
                    .client(remote, 6379, &strict.data)
                    .is_ok()
            );
        }
        assert!(
            strict
                .client("10.0.0.5", 6379, &strict.data)
                .unwrap_err()
                .contains("redis://10.0.0.5:6379")
        );
    }

    #[test]
    fn a_sentinel_cannot_name_anything_but_a_host_and_port() {
        let open = policy(true, true);
        for host in [
            "",
            "user:secret@evil.example",
            "evil.example/0",
            "evil.example#insecure",
            "evil.example?x=1",
            "a b",
            "[::1]",
        ] {
            assert!(open.client(host, 6379, &open.data).is_err(), "{host:?}");
        }
        assert!(open.client("redis-1.internal", 0, &open.data).is_err());
    }

    #[test]
    fn sentinel_clients_carry_the_right_credentials_and_tls() {
        let tls = policy(true, false);
        let data = tls.client("10.0.0.5", 6380, &tls.data).unwrap();
        let info = data.get_connection_info();
        assert!(matches!(
            info.addr(),
            ConnectionAddr::TcpTls { host, port: 6380, insecure: false, .. } if host == "10.0.0.5"
        ));
        assert_eq!(info.redis_settings().username(), Some("registry-user"));
        assert_eq!(info.redis_settings().password(), Some(PASSWORD));
        assert_eq!(info.redis_settings().db(), 2);
        // The sentinels get their own password and no database.
        let sentinel = tls.client("2001:db8::7", 26379, &tls.sentinel).unwrap();
        let info = sentinel.get_connection_info();
        assert!(matches!(
            info.addr(),
            ConnectionAddr::TcpTls { host, port: 26379, .. } if host == "2001:db8::7"
        ));
        assert_eq!(info.redis_settings().username(), None);
        assert_eq!(info.redis_settings().password(), Some("s3ntinel-pass"));
        assert_eq!(info.redis_settings().db(), 0);
        let plain = policy(false, false);
        let local = plain.client("127.0.0.1", 6379, &plain.data).unwrap();
        assert!(matches!(
            local.get_connection_info().addr(),
            ConnectionAddr::Tcp(host, 6379) if host == "127.0.0.1"
        ));
        assert_eq!(
            plain.scrub(&format!("AUTH {PASSWORD} then s3ntinel-pass")),
            "AUTH [redacted] then [redacted]"
        );
    }

    #[test]
    fn a_ca_file_must_exist_and_hold_a_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let missing = read_ca(&dir.path().join("absent.pem")).unwrap_err();
        assert!(missing.to_string().contains("cannot read"), "{missing}");
        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "not a certificate\n").unwrap();
        let invalid = read_ca(&empty).unwrap_err();
        assert!(
            invalid.to_string().contains("contains no PEM certificate"),
            "{invalid}"
        );
    }
}
