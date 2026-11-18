//! The registry's connection to the primary of a Redis Sentinel deployment.
//!
//! Sentinels are asked which server is the primary of a named service; the
//! named server is connected to and must itself report the `master` role
//! before any registry command is sent to it. Commands only ever go to that
//! connection, so a replica is never read or written.
//!
//! The primary is looked up again when its connection drops, when it
//! answers `READONLY` or `MASTERDOWN` (it was demoted), when three commands
//! in a row go unanswered, and when a periodic check finds that the
//! sentinels name another server or that the connected one no longer
//! reports `master`. A command that fails this way is not replayed: the
//! caller sees the failure and reconciles, as after any Redis outage.
//!
//! Redis replication is asynchronous, so the server a failover promotes can
//! lack the last writes its predecessor acknowledged, and can still hold
//! records its predecessor had already replaced or deleted. See
//! `docs/serve-http-failover.md` for what the registry's callers do then.
use arc_swap::{ArcSwap, ArcSwapOption};
use redis::aio::MultiplexedConnection;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::time::Instant;

/// Builds the client for the primary a sentinel named, from its host and
/// port. The caller decides TLS, credentials and whether the address is one
/// it is willing to connect to. The error text is reported as given.
pub type PrimaryClientFactory =
    Arc<dyn Fn(&str, u16) -> Result<redis::Client, String> + Send + Sync>;

/// Why no primary could be connected to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SentinelError {
    /// No sentinel named a primary for the service: none answered, or none
    /// monitors a service of that name. `detail` is the last failure.
    NoPrimaryNamed { detail: String },
    /// A sentinel named this address, which could not be connected to or
    /// did not answer.
    PrimaryUnreachable { address: String, detail: String },
    /// A sentinel named this address, but the server there does not report
    /// the `master` role (a failover is in progress, or the sentinel is out
    /// of date).
    NotPrimary { address: String },
}
impl std::fmt::Display for SentinelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPrimaryNamed { detail } => {
                write!(formatter, "no sentinel named a primary: {detail}")
            }
            Self::PrimaryUnreachable { address, detail } => write!(
                formatter,
                "the primary {address} named by a sentinel is unreachable: {detail}"
            ),
            Self::NotPrimary { address } => write!(
                formatter,
                "the server {address} named by a sentinel does not report the master role"
            ),
        }
    }
}
impl std::error::Error for SentinelError {}

struct SentinelNode {
    client: redis::Client,
    /// Kept between lookups; dropped when a command on it fails.
    connection: ArcSwapOption<MultiplexedConnection>,
}

struct Source {
    sentinels: Vec<SentinelNode>,
    service: String,
    primary: PrimaryClientFactory,
    connect_timeout: Duration,
    /// The sentinel that answered last; asked first next time.
    preferred: AtomicUsize,
}

/// A Sentinel-monitored service: the sentinels to ask, the service name and
/// how to connect to the server they name.
#[derive(Clone)]
pub struct SentinelPrimary {
    source: Arc<Source>,
}

/// A checked connection to the current primary.
pub struct ResolvedPrimary {
    /// `host:port` as the sentinel reported it.
    pub address: String,
    pub connection: MultiplexedConnection,
}

fn display_address(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

impl SentinelPrimary {
    /// `sentinels` are clients for the sentinel processes themselves, with
    /// whatever TLS and credentials those need. `connect_timeout` bounds
    /// each connection attempt and each lookup command separately. Nothing
    /// is connected here.
    ///
    /// Returns `None` for an empty sentinel list, an empty or oversized
    /// service name, or a timeout outside 10 ms to 30 s.
    pub fn new(
        sentinels: Vec<redis::Client>,
        service: String,
        primary: PrimaryClientFactory,
        connect_timeout: Duration,
    ) -> Option<Self> {
        if sentinels.is_empty()
            || sentinels.len() > 64
            || service.is_empty()
            || service.len() > 256
            || connect_timeout < Duration::from_millis(10)
            || connect_timeout > Duration::from_secs(30)
        {
            return None;
        }
        Some(Self {
            source: Arc::new(Source {
                sentinels: sentinels
                    .into_iter()
                    .map(|client| SentinelNode {
                        client,
                        connection: ArcSwapOption::empty(),
                    })
                    .collect(),
                service,
                primary,
                connect_timeout,
                preferred: AtomicUsize::new(0),
            }),
        })
    }

    async fn bounded<T>(
        &self,
        future: impl Future<Output = redis::RedisResult<T>>,
    ) -> Result<T, String> {
        match tokio::time::timeout(self.source.connect_timeout, future).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(error)) => Err(error.to_string()),
            Err(_) => Err("timed out".to_owned()),
        }
    }

    fn connection_config(&self) -> redis::AsyncConnectionConfig {
        redis::AsyncConnectionConfig::new()
            .set_connection_timeout(Some(self.source.connect_timeout))
    }

    /// Ask one sentinel for the primary's address. `Ok(None)` means it
    /// answered and does not know the service.
    async fn ask(&self, index: usize) -> Result<Option<(String, u16)>, String> {
        let node = &self.source.sentinels[index];
        let mut connection = match node.connection.load_full() {
            Some(connection) => (*connection).clone(),
            None => {
                let config = self.connection_config();
                let connection = self
                    .bounded(
                        node.client
                            .get_multiplexed_async_connection_with_config(&config),
                    )
                    .await?;
                node.connection.store(Some(Arc::new(connection.clone())));
                connection
            }
        };
        let mut command = redis::cmd("SENTINEL");
        command
            .arg("GET-MASTER-ADDR-BY-NAME")
            .arg(&self.source.service);
        let reply = self
            .bounded(command.query_async::<Option<(String, String)>>(&mut connection))
            .await;
        match reply {
            Ok(None) => Ok(None),
            Ok(Some((host, port))) => match port.parse::<u16>() {
                Ok(port) if port != 0 && !host.is_empty() => Ok(Some((host, port))),
                _ => Err("the sentinel returned an invalid address".to_owned()),
            },
            Err(error) => {
                node.connection.store(None);
                Err(error)
            }
        }
    }

    /// The address the sentinels currently name, from the first one that
    /// answers, starting with the one that answered last.
    pub async fn primary_address(&self) -> Result<(String, u16), SentinelError> {
        self.named().await.map(|(_, address)| address)
    }

    async fn named(&self) -> Result<(usize, (String, u16)), SentinelError> {
        self.named_from(self.source.preferred.load(Ordering::Relaxed), 0)
            .await
    }

    /// Ask the sentinels in order from `start`, skipping the first `skip`.
    async fn named_from(
        &self,
        start: usize,
        skip: usize,
    ) -> Result<(usize, (String, u16)), SentinelError> {
        let count = self.source.sentinels.len();
        let mut detail = String::from("every sentinel has been tried");
        for offset in skip..count {
            let index = (start + offset) % count;
            match self.ask(index).await {
                Ok(Some(address)) => return Ok((offset, address)),
                Ok(None) => {
                    detail = format!(
                        "sentinel {} does not monitor a service named {:?}",
                        index + 1,
                        self.source.service
                    );
                }
                Err(error) => detail = format!("sentinel {}: {error}", index + 1),
            }
        }
        Err(SentinelError::NoPrimaryNamed { detail })
    }

    /// Connect to `host:port` and require it to report the `master` role.
    async fn connect_primary(
        &self,
        host: &str,
        port: u16,
    ) -> Result<MultiplexedConnection, SentinelError> {
        let address = display_address(host, port);
        let unreachable = |detail: String| SentinelError::PrimaryUnreachable {
            address: address.clone(),
            detail,
        };
        let client = (self.source.primary)(host, port).map_err(unreachable)?;
        let config = self.connection_config();
        let mut connection = self
            .bounded(client.get_multiplexed_async_connection_with_config(&config))
            .await
            .map_err(unreachable)?;
        if self
            .bounded(is_primary(&mut connection))
            .await
            .map_err(unreachable)?
        {
            Ok(connection)
        } else {
            Err(SentinelError::NotPrimary { address })
        }
    }

    /// Find the primary and connect to it. Each sentinel is asked in turn
    /// until one names a server that is reachable and reports `master`.
    pub async fn resolve(&self) -> Result<ResolvedPrimary, SentinelError> {
        let count = self.source.sentinels.len();
        let start = self.source.preferred.load(Ordering::Relaxed);
        let mut skip = 0;
        let mut primary_error = None;
        loop {
            let (offset, (host, port)) = match self.named_from(start, skip).await {
                Ok(named) => named,
                // A named server that failed says more than the sentinels
                // that named nothing after it.
                Err(error) => return Err(primary_error.unwrap_or(error)),
            };
            match self.connect_primary(&host, port).await {
                Ok(connection) => {
                    self.source
                        .preferred
                        .store((start + offset) % count, Ordering::Relaxed);
                    return Ok(ResolvedPrimary {
                        address: display_address(&host, port),
                        connection,
                    });
                }
                Err(error) => primary_error = Some(error),
            }
            skip = offset + 1;
        }
    }
}

/// Whether the connected server reports the `master` role.
async fn is_primary(connection: &mut MultiplexedConnection) -> redis::RedisResult<bool> {
    let role: redis::Value = redis::cmd("ROLE").query_async(connection).await?;
    let name = match &role {
        redis::Value::Array(items) => match items.first() {
            Some(redis::Value::BulkString(name)) => name.as_slice(),
            Some(redis::Value::SimpleString(name)) => name.as_bytes(),
            _ => &[],
        },
        _ => &[],
    };
    Ok(name == b"master")
}

/// Consecutive unanswered commands after which the primary is looked up again.
const UNANSWERED_BEFORE_RESOLVE: u32 = 3;
/// How often a connection in use is checked against the sentinels.
const VERIFY_INTERVAL: Duration = Duration::from_secs(1);
/// The pause after a lookup that found no primary, before the next may start.
const FAILED_RESOLVE_PAUSE: Duration = Duration::from_millis(200);

struct Generation {
    address: String,
    connection: MultiplexedConnection,
    unanswered: AtomicU32,
}

struct Shared {
    primary: SentinelPrimary,
    current: ArcSwapOption<Generation>,
    /// One lookup at a time, in its own task: a caller's deadline must not
    /// cancel a lookup that the next caller would only have to start again.
    resolving: AtomicBool,
    /// One periodic check at a time.
    verifying: AtomicBool,
    /// Milliseconds after `started` when the last periodic check began.
    verified_at: AtomicU64,
    started: Instant,
    /// Signalled when a lookup ends, whatever it found.
    resolved: tokio::sync::Notify,
    /// The last lookup failure, for the caller that waited on it.
    last_error: ArcSwap<String>,
    /// Lookups that ended with a new connection, for observation.
    connections: AtomicU64,
}

/// Sends registry commands to the current primary and finds the new one
/// after a failover.
#[derive(Clone)]
pub(crate) struct SentinelManager {
    shared: Arc<Shared>,
}

fn no_primary(detail: &str) -> redis::RedisError {
    redis::RedisError::from(std::io::Error::new(
        std::io::ErrorKind::NotConnected,
        format!("no Redis primary is connected: {detail}"),
    ))
}

/// Whether a failed command shows that the connection is no longer one to
/// the primary: it is gone, or the server refuses writes because it is (or
/// is becoming) a replica.
fn lost_primary(error: &redis::RedisError) -> bool {
    use redis::{ErrorKind, ServerErrorKind};
    if error.is_connection_dropped() || error.is_io_error() || error.is_connection_refusal() {
        return true;
    }
    if matches!(
        error.kind(),
        ErrorKind::Server(ServerErrorKind::ReadOnly | ServerErrorKind::MasterDown)
    ) {
        return true;
    }
    // A script that fails on a write reports the refusal inside its own
    // error text on some server versions.
    matches!(error.code(), Some("READONLY" | "MASTERDOWN"))
        || error
            .detail()
            .is_some_and(|detail| detail.contains("READONLY") || detail.contains("MASTERDOWN"))
}

impl SentinelManager {
    pub(crate) fn new(primary: SentinelPrimary) -> Self {
        Self {
            shared: Arc::new(Shared {
                primary,
                current: ArcSwapOption::empty(),
                resolving: AtomicBool::new(false),
                verifying: AtomicBool::new(false),
                verified_at: AtomicU64::new(0),
                started: Instant::now(),
                resolved: tokio::sync::Notify::new(),
                last_error: ArcSwap::from_pointee(String::from("not yet looked up")),
                connections: AtomicU64::new(0),
            }),
        }
    }

    /// Start with a connection the caller already resolved and checked.
    pub(crate) fn with_resolved(primary: SentinelPrimary, resolved: ResolvedPrimary) -> Self {
        let manager = Self::new(primary);
        manager.shared.install(resolved);
        manager
    }

    #[cfg(test)]
    pub(crate) fn connections(&self) -> u64 {
        self.shared.connections.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    pub(crate) fn address(&self) -> Option<String> {
        self.shared
            .current
            .load()
            .as_ref()
            .map(|generation| generation.address.clone())
    }

    pub(crate) async fn query<T: redis::FromRedisValue>(
        &self,
        command: &redis::Cmd,
    ) -> redis::RedisResult<T> {
        struct Watch<'a> {
            shared: &'a Arc<Shared>,
            generation: Arc<Generation>,
            unanswered: bool,
        }
        impl Drop for Watch<'_> {
            fn drop(&mut self) {
                if !self.unanswered {
                    self.generation.unanswered.store(0, Ordering::Release);
                } else if self.generation.unanswered.fetch_add(1, Ordering::AcqRel) + 1
                    >= UNANSWERED_BEFORE_RESOLVE
                {
                    Shared::discard(self.shared, &self.generation);
                }
            }
        }
        let generation = self.current().await?;
        Shared::verify_if_due(&self.shared, &generation);
        // Armed across the await: a caller's deadline drops this future,
        // and that is an unanswered command too.
        let mut watch = Watch {
            shared: &self.shared,
            generation,
            unanswered: true,
        };
        let result = command
            .query_async(&mut watch.generation.connection.clone())
            .await;
        watch.unanswered = matches!(&result, Err(error) if error.is_timeout());
        if let Err(error) = &result
            && lost_primary(error)
        {
            Shared::discard(&self.shared, &watch.generation);
        }
        result
    }

    /// The connection to use, waiting for at most one lookup when there is
    /// none. The caller's own deadline bounds the wait.
    async fn current(&self) -> redis::RedisResult<Arc<Generation>> {
        let resolved = self.shared.resolved.notified();
        tokio::pin!(resolved);
        // Registered before the check, so a lookup that ends in between
        // is not missed.
        resolved.as_mut().enable();
        if let Some(generation) = self.shared.current.load_full() {
            return Ok(generation);
        }
        Shared::start_resolve(&self.shared);
        resolved.await;
        self.shared
            .current
            .load_full()
            .ok_or_else(|| no_primary(&self.shared.last_error.load()))
    }
}

impl Shared {
    fn install(&self, resolved: ResolvedPrimary) {
        self.connections.fetch_add(1, Ordering::Relaxed);
        self.current.store(Some(Arc::new(Generation {
            address: resolved.address,
            connection: resolved.connection,
            unanswered: AtomicU32::new(0),
        })));
    }

    /// Stop using `generation` and look the primary up again. Commands in
    /// flight on an older connection cannot discard a newer one.
    fn discard(shared: &Arc<Self>, generation: &Arc<Generation>) {
        let current = shared.current.load();
        if current
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, generation))
        {
            let previous = shared
                .current
                .compare_and_swap(&current, None::<Arc<Generation>>);
            drop(previous);
        }
        Self::start_resolve(shared);
    }

    fn start_resolve(shared: &Arc<Self>) {
        // A deadline can also fire while the runtime is shutting down.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if shared.resolving.swap(true, Ordering::AcqRel) {
            return;
        }
        let shared = shared.clone();
        runtime.spawn(async move {
            // Cleared on every exit, a panic or an aborted runtime included.
            struct Clear<'a>(&'a Shared);
            impl Drop for Clear<'_> {
                fn drop(&mut self) {
                    self.0.resolving.store(false, Ordering::Release);
                    self.0.resolved.notify_waiters();
                }
            }
            let _clear = Clear(&shared);
            if shared.current.load().is_some() {
                return;
            }
            match shared.primary.resolve().await {
                Ok(resolved) => shared.install(resolved),
                Err(error) => {
                    shared.last_error.store(Arc::new(error.to_string()));
                    // Tell the waiting callers now; hold the next lookup
                    // back so that a refused connection is not retried in
                    // a tight loop.
                    shared.resolved.notify_waiters();
                    tokio::time::sleep(FAILED_RESOLVE_PAUSE).await;
                }
            }
        });
    }

    /// Once per interval, while commands are being sent, check that the
    /// sentinels still name the connected server and that it still reports
    /// `master`. A demoted primary keeps answering reads, so nothing else
    /// would notice it on a connection that only reads. A check that cannot
    /// reach a sentinel or the server changes nothing: it is not evidence.
    fn verify_if_due(shared: &Arc<Self>, generation: &Arc<Generation>) {
        let now = u64::try_from(shared.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let last = shared.verified_at.load(Ordering::Relaxed);
        if now.saturating_sub(last) < VERIFY_INTERVAL.as_millis() as u64 {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if shared.verifying.swap(true, Ordering::AcqRel) {
            return;
        }
        shared.verified_at.store(now, Ordering::Relaxed);
        let shared = shared.clone();
        let generation = generation.clone();
        runtime.spawn(async move {
            struct Clear<'a>(&'a Shared);
            impl Drop for Clear<'_> {
                fn drop(&mut self) {
                    self.0.verifying.store(false, Ordering::Release);
                }
            }
            let _clear = Clear(&shared);
            let moved = match shared.primary.primary_address().await {
                Ok((host, port)) => display_address(&host, port) != generation.address,
                Err(_) => false,
            };
            let demoted = !moved
                && matches!(
                    shared
                        .primary
                        .bounded(is_primary(&mut generation.connection.clone()))
                        .await,
                    Ok(false)
                );
            if moved || demoted {
                Self::discard(&shared, &generation);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::SocketAddr,
        sync::atomic::{AtomicBool, AtomicU16},
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    /// What a fake server says. Everything not listed is answered `+OK`.
    #[derive(Default)]
    struct Behaviour {
        /// `SENTINEL GET-MASTER-ADDR-BY-NAME`: the port to name, 0 for nil.
        names: AtomicU16,
        /// `ROLE` answers `slave`, and `EVAL` answers `READONLY`.
        replica: AtomicBool,
        /// Value returned by `GET`.
        value: &'static str,
        /// Accepted connections are closed at once.
        closed: AtomicBool,
    }

    /// Read one command: an array of bulk strings.
    async fn read_command(
        stream: &mut tokio::net::TcpStream,
        buffer: &mut Vec<u8>,
    ) -> Option<Vec<String>> {
        loop {
            if let Some((command, used)) = parse(buffer) {
                buffer.drain(..used);
                return Some(command);
            }
            let mut chunk = [0u8; 1024];
            let read = stream.read(&mut chunk).await.ok()?;
            if read == 0 {
                return None;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
    }

    fn line(buffer: &[u8], from: usize) -> Option<(&str, usize)> {
        let end = buffer[from..].windows(2).position(|pair| pair == b"\r\n")? + from;
        Some((std::str::from_utf8(&buffer[from..end]).ok()?, end + 2))
    }

    fn parse(buffer: &[u8]) -> Option<(Vec<String>, usize)> {
        let (header, mut at) = line(buffer, 0)?;
        let count: usize = header.strip_prefix('*')?.parse().ok()?;
        let mut parts = Vec::new();
        for _ in 0..count {
            let (length, next) = line(buffer, at)?;
            let length: usize = length.strip_prefix('$')?.parse().ok()?;
            let end = next + length;
            if buffer.len() < end + 2 {
                return None;
            }
            parts.push(String::from_utf8_lossy(&buffer[next..end]).into_owned());
            at = end + 2;
        }
        Some((parts, at))
    }

    async fn fake(behaviour: Arc<Behaviour>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                if behaviour.closed.load(Ordering::Relaxed) {
                    continue;
                }
                let behaviour = behaviour.clone();
                tokio::spawn(async move {
                    let mut buffer = Vec::new();
                    while let Some(command) = read_command(&mut stream, &mut buffer).await {
                        let replica = behaviour.replica.load(Ordering::Relaxed);
                        let reply = match command[0].to_ascii_uppercase().as_str() {
                            "SENTINEL" => match behaviour.names.load(Ordering::Relaxed) {
                                0 => "*-1\r\n".to_owned(),
                                port => {
                                    let port = port.to_string();
                                    format!(
                                        "*2\r\n$9\r\n127.0.0.1\r\n${}\r\n{port}\r\n",
                                        port.len()
                                    )
                                }
                            },
                            "ROLE" if replica => "*1\r\n$5\r\nslave\r\n".to_owned(),
                            "ROLE" => "*1\r\n$6\r\nmaster\r\n".to_owned(),
                            "EVAL" if replica => {
                                "-READONLY You can't write against a read only replica.\r\n"
                                    .to_owned()
                            }
                            "GET" => {
                                format!("${}\r\n{}\r\n", behaviour.value.len(), behaviour.value)
                            }
                            _ => "+OK\r\n".to_owned(),
                        };
                        if stream.write_all(reply.as_bytes()).await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        address
    }

    fn client(address: SocketAddr) -> redis::Client {
        redis::Client::open(format!("redis://{address}/")).unwrap()
    }

    fn primary_for(sentinels: &[SocketAddr]) -> SentinelPrimary {
        SentinelPrimary::new(
            sentinels.iter().copied().map(client).collect(),
            "service".to_owned(),
            Arc::new(|host, port| {
                redis::Client::open(format!("redis://{host}:{port}/"))
                    .map_err(|error| error.to_string())
            }),
            Duration::from_millis(500),
        )
        .unwrap()
    }

    fn server(value: &'static str) -> Arc<Behaviour> {
        Arc::new(Behaviour {
            value,
            ..Behaviour::default()
        })
    }

    async fn get(manager: &SentinelManager) -> redis::RedisResult<String> {
        let mut command = redis::cmd("GET");
        command.arg("key");
        tokio::time::timeout(Duration::from_secs(5), manager.query(&command))
            .await
            .expect("the command or the lookup did not finish")
    }

    /// Retry until the manager answers from the server holding `value`.
    async fn wait_for(manager: &SentinelManager, value: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let got = get(manager).await;
            if got.as_deref() == Ok(value) {
                return;
            }
            assert!(Instant::now() < deadline, "still {got:?}, wanted {value}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[test]
    fn construction_is_bounded() {
        let factory: PrimaryClientFactory = Arc::new(|_, _| Err("unused".to_owned()));
        let one = || vec![redis::Client::open("redis://127.0.0.1:1/").unwrap()];
        let build = |sentinels, service: &str, timeout| {
            SentinelPrimary::new(sentinels, service.to_owned(), factory.clone(), timeout).is_some()
        };
        assert!(build(one(), "service", Duration::from_secs(1)));
        assert!(!build(Vec::new(), "service", Duration::from_secs(1)));
        assert!(!build(one(), "", Duration::from_secs(1)));
        assert!(!build(one(), "service", Duration::from_millis(9)));
        assert!(!build(one(), "service", Duration::from_secs(31)));
    }

    #[tokio::test]
    async fn the_primary_is_found_through_the_first_sentinel_that_answers() {
        let data = server("from-primary");
        let primary = fake(data.clone()).await;
        let sentinel = server("");
        sentinel.names.store(primary.port(), Ordering::Relaxed);
        // The first configured sentinel is down, the second does not know
        // the service, the third names the primary.
        let down = server("");
        down.closed.store(true, Ordering::Relaxed);
        let source = primary_for(&[
            fake(down).await,
            fake(server("")).await,
            fake(sentinel).await,
        ]);
        let resolved = source.resolve().await.unwrap();
        assert_eq!(resolved.address, format!("127.0.0.1:{}", primary.port()));
        // The sentinel that answered is asked first from now on.
        assert_eq!(source.source.preferred.load(Ordering::Relaxed), 2);
        let manager = SentinelManager::new(source);
        assert_eq!(get(&manager).await.unwrap(), "from-primary");
        assert_eq!(manager.connections(), 1);
    }

    #[tokio::test]
    async fn a_named_server_that_is_not_a_primary_is_never_used() {
        let replica = server("from-replica");
        replica.replica.store(true, Ordering::Relaxed);
        let replica_address = fake(replica.clone()).await;
        let sentinel = server("");
        sentinel
            .names
            .store(replica_address.port(), Ordering::Relaxed);
        let source = primary_for(&[fake(sentinel.clone()).await]);
        assert_eq!(
            source.resolve().await.err(),
            Some(SentinelError::NotPrimary {
                address: format!("127.0.0.1:{}", replica_address.port())
            })
        );
        let manager = SentinelManager::new(source.clone());
        let error = get(&manager).await.unwrap_err();
        assert!(error.to_string().contains("master role"), "{error}");
        assert_eq!(manager.connections(), 0);
        // A sentinel that names nothing, and one that is down.
        sentinel.names.store(0, Ordering::Relaxed);
        assert!(matches!(
            source.resolve().await,
            Err(SentinelError::NoPrimaryNamed { .. })
        ));
        // Once the server is promoted, the same manager uses it.
        sentinel
            .names
            .store(replica_address.port(), Ordering::Relaxed);
        replica.replica.store(false, Ordering::Relaxed);
        wait_for(&manager, "from-replica").await;
    }

    #[tokio::test]
    async fn a_refused_write_makes_the_next_command_go_to_the_new_primary() {
        let old = server("old");
        let old_address = fake(old.clone()).await;
        let new_address = fake(server("new")).await;
        let sentinel = server("");
        sentinel.names.store(old_address.port(), Ordering::Relaxed);
        let manager = SentinelManager::new(primary_for(&[fake(sentinel.clone()).await]));
        assert_eq!(get(&manager).await.unwrap(), "old");
        // The old primary is demoted; the sentinel names its successor.
        // Verification is not due yet, so the write is what notices.
        old.replica.store(true, Ordering::Relaxed);
        sentinel.names.store(new_address.port(), Ordering::Relaxed);
        let mut write = redis::cmd("EVAL");
        write.arg("return 1").arg(0);
        let refused = manager.query::<i64>(&write).await.unwrap_err();
        assert!(lost_primary(&refused), "{refused}");
        // The refused write is reported, not replayed; what follows goes
        // to the new primary.
        wait_for(&manager, "new").await;
        assert_eq!(
            manager.address(),
            Some(format!("127.0.0.1:{}", new_address.port()))
        );
        assert_eq!(manager.connections(), 2);
    }

    #[tokio::test]
    async fn reads_leave_a_demoted_primary_within_the_check_interval() {
        let old = server("old");
        let old_address = fake(old.clone()).await;
        let new_address = fake(server("new")).await;
        let sentinel = server("");
        sentinel.names.store(old_address.port(), Ordering::Relaxed);
        let manager = SentinelManager::new(primary_for(&[fake(sentinel.clone()).await]));
        assert_eq!(get(&manager).await.unwrap(), "old");
        // The old server keeps answering reads, as a demoted primary does.
        sentinel.names.store(new_address.port(), Ordering::Relaxed);
        let moved = Instant::now();
        wait_for(&manager, "new").await;
        assert!(
            moved.elapsed() < VERIFY_INTERVAL + Duration::from_secs(2),
            "{:?}",
            moved.elapsed()
        );
        // Sentinels that still name the old server, which says it is a
        // replica: it is left too, and nothing else is used meanwhile.
        let stale = server("");
        let demoted = server("demoted");
        let demoted_address = fake(demoted.clone()).await;
        stale.names.store(demoted_address.port(), Ordering::Relaxed);
        let manager = SentinelManager::new(primary_for(&[fake(stale.clone()).await]));
        assert_eq!(get(&manager).await.unwrap(), "demoted");
        demoted.replica.store(true, Ordering::Relaxed);
        let deadline = Instant::now() + Duration::from_secs(10);
        while get(&manager).await.is_ok() {
            assert!(Instant::now() < deadline, "the replica is still read");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(manager.address(), None);
    }

    #[tokio::test]
    async fn a_lookup_outlives_the_deadline_of_the_command_that_started_it() {
        // A sentinel that accepts and never answers: the lookup takes its
        // whole connect timeout, longer than the caller waits.
        let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let silent_address = silent.local_addr().unwrap();
        let held = Arc::new(tokio::sync::Mutex::new(Vec::new()));
        let keep = held.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = silent.accept().await {
                keep.lock().await.push(stream);
            }
        });
        let data = fake(server("value")).await;
        let sentinel = server("");
        sentinel.names.store(data.port(), Ordering::Relaxed);
        let manager = SentinelManager::new(primary_for(&[silent_address, fake(sentinel).await]));
        let mut command = redis::cmd("GET");
        command.arg("key");
        let first =
            tokio::time::timeout(Duration::from_millis(50), manager.query::<String>(&command))
                .await;
        assert!(first.is_err(), "the lookup cannot have finished yet");
        // Nobody is waiting, and the lookup still completes.
        let deadline = Instant::now() + Duration::from_secs(10);
        while manager.address().is_none() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(get(&manager).await.unwrap(), "value");
        assert_eq!(manager.connections(), 1);
    }
}
