use crate::*;
use arc_swap::ArcSwapOption;
use async_trait::async_trait;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
    time::{Instant, timeout_at},
};
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(pub String);
#[async_trait]
pub trait Discovery: Send + Sync {
    fn advertisement_refresh_interval(&self) -> Option<Duration> {
        None
    }
    fn heartbeat_interval(&self) -> Duration {
        Duration::from_secs(5)
    }
    async fn locate(&self, scope: &CacheScope, id: &BlockId) -> Result<Vec<PeerId>>;
    fn placement(&self, scope: &CacheScope, id: &BlockId) -> Vec<PeerId>;
    async fn advertise(&self, scope: &CacheScope, id: &BlockId, peer: &PeerId) -> Result<()>;
    async fn withdraw(&self, scope: &CacheScope, id: &BlockId, peer: &PeerId) -> Result<()>;
    async fn heartbeat(&self, peer: &PeerId) -> Result<()>;
}
#[async_trait]
pub trait PeerTransport: Send + Sync {
    async fn get(&self, peer: &PeerId, scope: &CacheScope, id: &BlockId)
    -> Result<Option<Vec<u8>>>;
    async fn get_shared(
        &self,
        peer: &PeerId,
        scope: &CacheScope,
        id: &BlockId,
    ) -> Result<Option<bytes::Bytes>> {
        Ok(self.get(peer, scope, id).await?.map(bytes::Bytes::from))
    }
    async fn put_shared(
        &self,
        peer: &PeerId,
        scope: &CacheScope,
        id: &BlockId,
        bytes: Arc<[u8]>,
    ) -> Result<()> {
        self.put(peer, scope, id, &bytes).await
    }
    async fn put(
        &self,
        peer: &PeerId,
        scope: &CacheScope,
        id: &BlockId,
        bytes: &[u8],
    ) -> Result<()>;
}
#[derive(Clone, Debug)]
pub struct DistributedConfig {
    pub max_peer_queries: usize,
    pub deadline: Duration,
    pub maintenance_capacity: usize,
    pub placement_concurrency: usize,
    pub hedge_delay: Duration,
    pub max_inflight_misses: usize,
}
impl Default for DistributedConfig {
    fn default() -> Self {
        Self {
            max_peer_queries: 3,
            deadline: Duration::from_millis(500),
            maintenance_capacity: 64,
            placement_concurrency: 4,
            hedge_delay: Duration::from_millis(25),
            max_inflight_misses: 64,
        }
    }
}
#[derive(Clone, Copy, Debug)]
pub enum DiscoveryMode {
    Directory,
    Deterministic,
    PeerQuery,
}
pub struct FixedDiscovery {
    peers: Vec<PeerId>,
    max_queries: usize,
    mode: DiscoveryMode,
}
impl FixedDiscovery {
    pub fn new(peers: Vec<PeerId>, max_queries: usize) -> Result<Arc<Self>> {
        Self::new_with_mode(peers, max_queries, DiscoveryMode::Deterministic)
    }
    pub fn new_with_mode(
        peers: Vec<PeerId>,
        max_queries: usize,
        mode: DiscoveryMode,
    ) -> Result<Arc<Self>> {
        let mut peers = peers;
        peers.sort();
        peers.dedup();
        if peers.len() > 1024
            || max_queries == 0
            || max_queries > 1024
            || peers.iter().any(|p| p.0.is_empty() || p.0.len() > 1024)
        {
            return Err(error());
        }
        Ok(Arc::new(Self {
            peers,
            max_queries,
            mode,
        }))
    }
}
#[async_trait]
impl Discovery for FixedDiscovery {
    async fn locate(&self, scope: &CacheScope, id: &BlockId) -> Result<Vec<PeerId>> {
        let mut p = self.placement(scope, id);
        if matches!(
            self.mode,
            DiscoveryMode::PeerQuery | DiscoveryMode::Directory
        ) {
            for peer in &self.peers {
                if !p.contains(peer) {
                    p.push(peer.clone());
                }
            }
        }
        p.truncate(self.max_queries);
        Ok(p)
    }
    fn placement(&self, scope: &CacheScope, id: &BlockId) -> Vec<PeerId> {
        let mut p = self.peers.clone();
        let scope_hash = scope.digest();
        p.sort_by_cached_key(|peer| {
            digest(&[scope_hash.as_bytes(), id.0.as_bytes(), peer.0.as_bytes()])
        });
        p.truncate(2);
        p
    }
    async fn advertise(&self, _: &CacheScope, _: &BlockId, peer: &PeerId) -> Result<()> {
        if self.peers.contains(peer) {
            Ok(())
        } else {
            Err(error())
        }
    }
    async fn withdraw(&self, s: &CacheScope, i: &BlockId, p: &PeerId) -> Result<()> {
        self.advertise(s, i, p).await
    }
    async fn heartbeat(&self, p: &PeerId) -> Result<()> {
        if self.peers.contains(p) {
            Ok(())
        } else {
            Err(error())
        }
    }
}
/// Bounded RESP hints over a persistent authenticated connection. Credentials are redacted.
#[derive(Clone, Debug)]
pub struct RedisTlsConfig {
    pub roots: rustls::RootCertStore,
    pub server_name: String,
}
#[derive(Clone)]
pub struct RedisConfig {
    pub address: String,
    pub username: Option<String>,
    pub password: Option<String>,
    pub namespace: String,
    pub ttl: Duration,
    pub deadline: Duration,
    pub max_reply_bytes: usize,
    pub tls: Option<RedisTlsConfig>,
}
impl std::fmt::Debug for RedisConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisConfig")
            .field("address", &self.address)
            .field("namespace", &self.namespace)
            .field("credentials", &"[redacted]")
            .finish()
    }
}
pub struct RedisDiscovery {
    config: Arc<RedisConfig>,
    fallback: Arc<FixedDiscovery>,
    mode: DiscoveryMode,
    /// The way to the task that owns the connection. Empty until the first
    /// command, which starts the task on the runtime it is called from; the
    /// task ends, and closes the connection, when this sender is dropped.
    owner: ArcSwapOption<mpsc::UnboundedSender<Exchange>>,
}
trait RedisStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> RedisStream for T {}
/// One command and where its reply goes.
struct Exchange {
    args: Vec<String>,
    /// When the caller stops waiting, counted from when it asked.
    deadline: Instant,
    reply: oneshot::Sender<Result<Reply>>,
}
/// Owns the connection: one exchange at a time, in the order they were sent,
/// so a reply is only ever read by the command that asked for it. The stream
/// is kept only after a complete exchange. One that fails, runs out of time,
/// or whose caller stops waiting part way is dropped with its stream, and
/// the next connects again.
async fn own_connection(config: Arc<RedisConfig>, mut jobs: mpsc::UnboundedReceiver<Exchange>) {
    let mut kept: Option<Box<dyn RedisStream>> = None;
    while let Some(Exchange {
        args,
        deadline,
        mut reply,
    }) = jobs.recv().await
    {
        // The caller gave up while this waited its turn.
        if reply.is_closed() {
            continue;
        }
        let outcome = tokio::select! {
            biased;
            _ = reply.closed() => None,
            outcome = timeout_at(deadline, exchange(&config, kept.take(), &args)) => {
                Some(outcome.unwrap_or_else(|_| Err(error())))
            }
        };
        let Some(outcome) = outcome else { continue };
        let _ = reply.send(outcome.map(|(response, stream)| {
            kept = Some(stream);
            response
        }));
    }
}
async fn connect(config: &RedisConfig) -> Result<Box<dyn RedisStream>> {
    let tcp = TcpStream::connect(&config.address)
        .await
        .map_err(|_| error())?;
    tcp.set_nodelay(true).map_err(|_| error())?;
    let mut stream: Box<dyn RedisStream> = if let Some(tls) = &config.tls {
        let client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| error())?
        .with_root_certificates(tls.roots.clone())
        .with_no_client_auth();
        let name = rustls::pki_types::ServerName::try_from(tls.server_name.clone())
            .map_err(|_| error())?;
        Box::new(
            tokio_rustls::TlsConnector::from(Arc::new(client))
                .connect(name, tcp)
                .await
                .map_err(|_| error())?,
        )
    } else {
        if !tcp.peer_addr().map_err(|_| error())?.ip().is_loopback() {
            return Err(error());
        }
        Box::new(tcp)
    };
    if let Some(password) = &config.password {
        let mut auth = vec!["AUTH".to_owned()];
        if let Some(user) = &config.username {
            auth.push(user.clone());
        }
        auth.push(password.clone());
        send(&mut *stream, &auth).await?;
        if !matches!(reply(&mut *stream,config.max_reply_bytes).await?,Reply::Text(v) if v=="OK") {
            return Err(error());
        }
    }
    Ok(stream)
}
/// Takes the stream and gives it back only with a complete reply.
async fn exchange(
    config: &RedisConfig,
    stream: Option<Box<dyn RedisStream>>,
    args: &[String],
) -> Result<(Reply, Box<dyn RedisStream>)> {
    let mut stream = match stream {
        Some(stream) => stream,
        None => connect(config).await?,
    };
    send(&mut *stream, args).await?;
    let response = reply(&mut *stream, config.max_reply_bytes).await?;
    Ok((response, stream))
}
impl RedisDiscovery {
    pub fn new(
        config: RedisConfig,
        peers: Vec<PeerId>,
        max_queries: usize,
        mode: DiscoveryMode,
    ) -> Result<Arc<Self>> {
        if config.deadline.is_zero()
            || config.deadline > Duration::from_secs(60)
            || config.ttl > Duration::from_secs(86400)
            || config.namespace.is_empty()
            || config.ttl < Duration::from_millis(30)
            || config.max_reply_bytes == 0
            || config.max_reply_bytes > 16 * 1024 * 1024
            || config.namespace.len() > 1024
            || config.address.len() > 1024
            || config.username.as_ref().is_some_and(|v| v.len() > 1024)
            || config.password.as_ref().is_some_and(|v| v.len() > 16384)
        {
            return Err(error());
        }
        Ok(Arc::new(Self {
            config: Arc::new(config),
            fallback: FixedDiscovery::new_with_mode(peers, max_queries, DiscoveryMode::PeerQuery)?,
            mode,
            owner: ArcSwapOption::empty(),
        }))
    }
    fn key(&self, scope: &CacheScope, id: &BlockId, peer: &PeerId) -> String {
        format!(
            "{}:blob:{}:{}:{}",
            self.config.namespace,
            scope.digest(),
            digest(&[id.0.as_bytes()]),
            digest(&[peer.0.as_bytes()])
        )
    }
    fn lease(&self, p: &PeerId) -> String {
        format!(
            "{}:node:{}",
            self.config.namespace,
            digest(&[p.0.as_bytes()])
        )
    }
    async fn command(&self, args: Vec<String>) -> Result<Reply> {
        let deadline = Instant::now() + self.config.deadline;
        let (reply, answer) = oneshot::channel();
        self.submit(Exchange {
            args,
            deadline,
            reply,
        });
        // Dropping `answer`, here or with this future, is what tells the
        // owner to abandon the exchange and its stream.
        match timeout_at(deadline, answer).await {
            Ok(Ok(outcome)) => outcome,
            _ => Err(error()),
        }
    }
    /// Hands the exchange to the owner task, starting one if there is none
    /// or if the last one went away with the runtime it ran on. An exchange
    /// that cannot be handed over is dropped, which its caller sees as an
    /// error.
    fn submit(&self, mut job: Exchange) {
        for _ in 0..2 {
            let current = self.owner.load();
            if let Some(owner) = &*current {
                match owner.send(job) {
                    Ok(()) => return,
                    Err(refused) => job = refused.0,
                }
            }
            let (sender, jobs) = mpsc::unbounded_channel();
            let previous = self
                .owner
                .compare_and_swap(&*current, Some(Arc::new(sender)));
            // Losing the exchange means another caller installed an owner;
            // `jobs` is dropped and no second task starts.
            if previous.as_ref().map(Arc::as_ptr) == current.as_ref().map(Arc::as_ptr) {
                tokio::spawn(own_connection(self.config.clone(), jobs));
            }
        }
        if let Some(owner) = &*self.owner.load() {
            let _ = owner.send(job);
        }
    }
    fn trusted(&self, p: &PeerId) -> bool {
        self.fallback.peers.contains(p)
    }
}
#[async_trait]
impl Discovery for RedisDiscovery {
    fn advertisement_refresh_interval(&self) -> Option<Duration> {
        Some(self.heartbeat_interval())
    }
    fn heartbeat_interval(&self) -> Duration {
        (self.config.ttl / 3).max(Duration::from_millis(10))
    }
    async fn locate(&self, s: &CacheScope, i: &BlockId) -> Result<Vec<PeerId>> {
        let fallback = self.fallback.locate(s, i).await?;
        if matches!(
            self.mode,
            DiscoveryMode::Deterministic | DiscoveryMode::PeerQuery
        ) {
            return Ok(fallback);
        }
        // One round trip examines all trusted holders and their leases, then bounds queries.
        let mut args = vec!["MGET".to_owned()];
        for p in &self.fallback.peers {
            args.push(self.key(s, i, p));
            args.push(self.lease(p));
        }
        let values = match self.command(args).await {
            Ok(Reply::Array(v)) => v,
            _ => return Ok(fallback),
        };
        if values.len() != self.fallback.peers.len() * 2 {
            return Ok(fallback);
        }
        let mut found = Vec::new();
        for (p, pair) in self.fallback.peers.iter().zip(values.chunks_exact(2)) {
            if pair.iter().all(|v| matches!(v,Reply::Text(h) if h==&p.0)) {
                found.push(p.clone());
            }
        }
        for p in fallback {
            if !found.contains(&p) {
                found.push(p);
            }
        }
        found.truncate(self.fallback.max_queries);
        Ok(found)
    }
    fn placement(&self, s: &CacheScope, i: &BlockId) -> Vec<PeerId> {
        self.fallback.placement(s, i)
    }
    async fn advertise(&self, s: &CacheScope, i: &BlockId, p: &PeerId) -> Result<()> {
        if !self.trusted(p) {
            return Err(error());
        }
        self.command(vec![
            "SET".into(),
            self.key(s, i, p),
            p.0.clone(),
            "PX".into(),
            self.config.ttl.as_millis().to_string(),
        ])
        .await?;
        Ok(())
    }
    async fn withdraw(&self, s: &CacheScope, i: &BlockId, p: &PeerId) -> Result<()> {
        if !self.trusted(p) {
            return Err(error());
        }
        self.command(vec!["DEL".into(), self.key(s, i, p)]).await?;
        Ok(())
    }
    async fn heartbeat(&self, p: &PeerId) -> Result<()> {
        if !self.trusted(p) {
            return Err(error());
        }
        self.command(vec![
            "SET".into(),
            self.lease(p),
            p.0.clone(),
            "PX".into(),
            self.config.ttl.as_millis().to_string(),
        ])
        .await?;
        Ok(())
    }
}
async fn send<S: AsyncWrite + Unpin + ?Sized>(s: &mut S, args: &[String]) -> Result<()> {
    let mut bytes = format!("*{}\r\n", args.len()).into_bytes();
    for a in args {
        bytes.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
        bytes.extend_from_slice(a.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    s.write_all(&bytes).await.map_err(|_| error())
}
#[derive(Debug)]
enum Reply {
    Text(String),
    Nil,
    Array(Vec<Reply>),
}
async fn line<S: AsyncRead + Unpin + ?Sized>(s: &mut S, remaining: &mut usize) -> Result<String> {
    let mut line = Vec::new();
    loop {
        if *remaining == 0 {
            return Err(error());
        }
        *remaining -= 1;
        line.push(s.read_u8().await.map_err(|_| error())?);
        if line.ends_with(b"\r\n") {
            break;
        }
    }
    String::from_utf8(line[..line.len() - 2].to_vec()).map_err(|_| error())
}
async fn scalar<S: AsyncRead + Unpin + ?Sized>(s: &mut S, remaining: &mut usize) -> Result<Reply> {
    let text = line(s, remaining).await?;
    match text.as_bytes().first() {
        Some(b'+') | Some(b':') => Ok(Reply::Text(text[1..].into())),
        Some(b'$') => {
            let n = text[1..].parse::<i64>().map_err(|_| error())?;
            if n == -1 {
                return Ok(Reply::Nil);
            }
            let n = usize::try_from(n).map_err(|_| error())?;
            if n.checked_add(2).is_none_or(|n| n > *remaining) {
                return Err(error());
            }
            *remaining -= n + 2;
            let mut b = vec![0; n + 2];
            s.read_exact(&mut b).await.map_err(|_| error())?;
            if &b[n..] != b"\r\n" {
                return Err(error());
            }
            String::from_utf8(b[..n].to_vec())
                .map(Reply::Text)
                .map_err(|_| error())
        }
        _ => Err(error()),
    }
}
async fn reply<S: AsyncRead + Unpin + ?Sized>(s: &mut S, max: usize) -> Result<Reply> {
    let mut remaining = max;
    let first = s.read_u8().await.map_err(|_| error())?;
    remaining = remaining.checked_sub(1).ok_or_else(error)?;
    if first != b'*' {
        let prefix = std::io::Cursor::new(vec![first]);
        let mut combined = prefix.chain(s);
        return scalar(&mut combined, &mut remaining).await;
    }
    let count = line(s, &mut remaining)
        .await?
        .parse::<usize>()
        .map_err(|_| error())?;
    if count > 2048 || count > remaining {
        return Err(error());
    }
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(scalar(s, &mut remaining).await?);
    }
    Ok(Reply::Array(values))
}
pub(crate) fn unique(peers: Vec<PeerId>, local: &PeerId, max: usize) -> Vec<PeerId> {
    let mut seen = BTreeSet::new();
    peers
        .into_iter()
        .filter(|p| p != local && seen.insert(p.clone()))
        .take(max)
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::timeout;
    fn scope() -> CacheScope {
        CacheScope {
            identity: ScopeIdentity {
                cluster: "cluster".into(),
                partition: "partition".into(),
                drive: "drive".into(),
            },
            backing: ConcurrentBackingId::from_bytes([3; 16]).unwrap(),
        }
    }
    #[tokio::test]
    async fn cancelled_redis_command_reconnects_instead_of_reusing_partial_reply() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (seen, ready) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut first, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 1024];
            assert!(first.read(&mut buffer).await.unwrap() > 0);
            seen.send(()).unwrap();
            match timeout(Duration::from_millis(200), listener.accept()).await {
                Ok(Ok((mut second, _))) => {
                    assert!(second.read(&mut buffer).await.unwrap() > 0);
                    second.write_all(b"+OK\r\n").await.unwrap();
                    2
                }
                _ => {
                    first.write_all(b"+OK\r\n").await.unwrap();
                    1
                }
            }
        });
        let cfg = RedisConfig {
            address,
            username: None,
            password: None,
            namespace: "cancel".into(),
            ttl: Duration::from_secs(1),
            deadline: Duration::from_secs(1),
            max_reply_bytes: 1024,
            tls: None,
        };
        let peer = PeerId("peer".into());
        let d = RedisDiscovery::new(cfg, vec![peer.clone()], 1, DiscoveryMode::Directory).unwrap();
        let first = d.clone();
        let p = peer.clone();
        let command = tokio::spawn(async move { first.heartbeat(&p).await });
        ready.await.unwrap();
        command.abort();
        let _ = command.await;
        d.heartbeat(&peer).await.unwrap();
        assert_eq!(
            server.await.unwrap(),
            2,
            "cancelled stream must be dropped before another command"
        );
    }
    fn loopback(address: String, deadline: Duration) -> RedisConfig {
        RedisConfig {
            address,
            username: None,
            password: None,
            namespace: "owner".into(),
            ttl: Duration::from_secs(1),
            deadline,
            max_reply_bytes: 1024,
            tls: None,
        }
    }
    /// A loopback server that answers each command with its last argument,
    /// after a pause when the argument starts with `slow`, and that hangs up
    /// instead when it starts with `drop`. Counts connections accepted and
    /// connections that ended.
    async fn echo_server() -> (String, Arc<[std::sync::atomic::AtomicUsize; 2]>) {
        use std::sync::atomic::Ordering::SeqCst;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let counts = Arc::new([const { std::sync::atomic::AtomicUsize::new(0) }; 2]);
        let counted = counts.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                counted[0].fetch_add(1, SeqCst);
                let counted = counted.clone();
                tokio::spawn(async move {
                    let mut stream = tokio::io::BufReader::new(stream);
                    let mut budget = usize::MAX;
                    while let Ok(head) = line(&mut stream, &mut budget).await {
                        let count: usize = head[1..].parse().unwrap();
                        let mut last = Vec::new();
                        for _ in 0..count {
                            let size: usize = line(&mut stream, &mut budget).await.unwrap()[1..]
                                .parse()
                                .unwrap();
                            last = vec![0; size + 2];
                            stream.read_exact(&mut last).await.unwrap();
                        }
                        if last.starts_with(b"drop") {
                            break;
                        }
                        if last.starts_with(b"slow") {
                            tokio::time::sleep(Duration::from_millis(2)).await;
                        }
                        let mut answer = format!("${}\r\n", last.len() - 2).into_bytes();
                        answer.extend_from_slice(&last);
                        // In two writes, so a reader can be caught between them.
                        let (first, second) = answer.split_at(answer.len() / 2);
                        if stream.get_mut().write_all(first).await.is_err() {
                            break;
                        }
                        tokio::task::yield_now().await;
                        if stream.get_mut().write_all(second).await.is_err() {
                            break;
                        }
                    }
                    counted[1].fetch_add(1, SeqCst);
                });
            }
        });
        (address, counts)
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_redis_commands_each_read_their_own_reply() {
        let (address, _) = echo_server().await;
        let d = RedisDiscovery::new(
            loopback(address, Duration::from_secs(10)),
            vec![PeerId("peer".into())],
            1,
            DiscoveryMode::Directory,
        )
        .unwrap();
        // Callers that are cancelled part way, that run out of time, and
        // whose connection is closed under them, among callers that must
        // each still get the answer to their own question.
        let mut callers = Vec::new();
        for task in 0..16_usize {
            let d = d.clone();
            callers.push(tokio::spawn(async move {
                let mut answered = 0_usize;
                for n in 0..300_usize {
                    let asked = match (task + n) % 23 {
                        0 => format!("drop-{task}-{n}"),
                        7 | 15 => format!("slow-{task}-{n}"),
                        _ => format!("echo-{task}-{n}"),
                    };
                    let command = d.command(vec!["ECHO".into(), asked.clone()]);
                    let outcome = if (task + n) % 11 == 3 {
                        // Dropped after a moment: often mid-exchange.
                        match timeout(Duration::from_micros(((task * n) % 400) as u64), command)
                            .await
                        {
                            Ok(outcome) => outcome,
                            Err(_) => continue,
                        }
                    } else {
                        command.await
                    };
                    match outcome {
                        Ok(Reply::Text(got)) => {
                            assert_eq!(got, asked, "a command read another command's reply");
                            answered += 1;
                        }
                        Ok(other) => panic!("unexpected reply {other:?}"),
                        // A closed connection fails that command, and may
                        // fail none other: the next one connects again.
                        Err(_) => assert!(asked.starts_with("drop"), "{asked} failed"),
                    }
                }
                answered
            }));
        }
        let mut answered = 0;
        for caller in callers {
            answered += caller.await.unwrap();
        }
        assert!(answered > 16 * 300 * 3 / 4, "only {answered} answered");
    }
    #[tokio::test]
    async fn redis_command_deadline_counts_the_wait_behind_other_commands() {
        let (address, _) = echo_server().await;
        // Each slow command takes 2 ms; 100 of them queue for 200 ms, which
        // the last ones do not have.
        let d = RedisDiscovery::new(
            loopback(address, Duration::from_millis(60)),
            vec![PeerId("peer".into())],
            1,
            DiscoveryMode::Directory,
        )
        .unwrap();
        let started = std::time::Instant::now();
        let commands: Vec<_> = (0..100)
            .map(|n| {
                let d = d.clone();
                tokio::spawn(async move {
                    let asked = format!("slow-{n}");
                    match d.command(vec!["ECHO".into(), asked.clone()]).await {
                        Ok(Reply::Text(got)) => {
                            assert_eq!(got, asked);
                            true
                        }
                        Ok(other) => panic!("unexpected reply {other:?}"),
                        Err(_) => false,
                    }
                })
            })
            .collect();
        let mut answered = 0;
        for command in commands {
            answered += usize::from(command.await.unwrap());
        }
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "every caller is released by its own deadline"
        );
        assert!((1..100).contains(&answered), "{answered} answered");
        // What timed out left nothing behind for the next command to read.
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            matches!(d.command(vec!["ECHO".into(), "after".into()]).await, Ok(Reply::Text(got)) if got == "after")
        );
    }
    #[test]
    fn redis_owner_task_ends_with_the_discovery_and_restarts_on_a_new_runtime() {
        use std::sync::atomic::Ordering::SeqCst;
        // The server outlives the runtimes that the discovery is used from.
        let server = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let (address, counts) = server.block_on(echo_server());
        // Built with no runtime around it.
        let d = RedisDiscovery::new(
            loopback(address, Duration::from_secs(5)),
            vec![PeerId("peer".into())],
            1,
            DiscoveryMode::Directory,
        )
        .unwrap();
        let ask = |d: Arc<RedisDiscovery>, text: &'static str| async move {
            matches!(d.command(vec!["ECHO".into(), text.into()]).await, Ok(Reply::Text(got)) if got == text)
        };
        let closed = |n: usize| {
            let began = std::time::Instant::now();
            while counts[1].load(SeqCst) != n {
                assert!(
                    began.elapsed() < Duration::from_secs(5),
                    "connection {n} stayed open"
                );
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        let first = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(first.block_on(ask(d.clone(), "one")));
        assert!(first.block_on(ask(d.clone(), "two")));
        assert_eq!(counts[0].load(SeqCst), 1, "one connection is reused");
        // The runtime takes the owner task with it; the next command, on
        // another runtime, starts a new one.
        drop(first);
        closed(1);
        let second = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert!(second.block_on(ask(d.clone(), "three")));
        assert!(second.block_on(ask(d.clone(), "four")));
        assert_eq!(counts[0].load(SeqCst), 2);
        // Dropping the discovery ends the task and closes its connection
        // while the runtime it ran on is still alive.
        drop(d);
        second.block_on(async {
            for _ in 0..1000 {
                if counts[1].load(SeqCst) == 2 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        closed(2);
    }
    /// A loopback server that answers every complete RESP command with `+OK`.
    async fn ok_server() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    stream.set_nodelay(true).unwrap();
                    let mut stream = tokio::io::BufReader::new(stream);
                    let mut budget = usize::MAX;
                    loop {
                        let Ok(head) = line(&mut stream, &mut budget).await else {
                            return;
                        };
                        let count: usize = head[1..].parse().unwrap();
                        for _ in 0..count {
                            let size: usize = line(&mut stream, &mut budget).await.unwrap()[1..]
                                .parse()
                                .unwrap();
                            let mut body = vec![0; size + 2];
                            stream.read_exact(&mut body).await.unwrap();
                        }
                        if stream.get_mut().write_all(b"+OK\r\n").await.is_err() {
                            return;
                        }
                    }
                });
            }
        });
        address
    }
    /// Not a test: heartbeats over one loopback connection, from one task
    /// and then from 8.
    /// `cargo test -p mount-rs-blob-cache --release profile_redis_commands -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn profile_redis_commands() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let cfg = RedisConfig {
                address: ok_server().await,
                username: None,
                password: None,
                namespace: "profile".into(),
                ttl: Duration::from_secs(1),
                deadline: Duration::from_secs(5),
                max_reply_bytes: 1024,
                tls: None,
            };
            let peer = PeerId("peer".into());
            let d =
                RedisDiscovery::new(cfg, vec![peer.clone()], 1, DiscoveryMode::Directory).unwrap();
            d.heartbeat(&peer).await.unwrap();
            for tasks in [1_usize, 8] {
                let each = 40_000 / tasks;
                let started = std::time::Instant::now();
                let handles: Vec<_> = (0..tasks)
                    .map(|_| {
                        let (d, peer) = (d.clone(), peer.clone());
                        tokio::spawn(async move {
                            let mut waits = Vec::with_capacity(each);
                            for _ in 0..each {
                                let began = std::time::Instant::now();
                                d.heartbeat(&peer).await.unwrap();
                                waits.push(began.elapsed().as_nanos() as u64);
                            }
                            waits
                        })
                    })
                    .collect();
                let mut waits = Vec::new();
                for handle in handles {
                    waits.extend(handle.await.unwrap());
                }
                let seconds = started.elapsed().as_secs_f64();
                waits.sort_unstable();
                let at = |q: f64| waits[((waits.len() - 1) as f64 * q) as usize] as f64 / 1e6;
                println!(
                    "redis commands x{tasks}: {:.0}/s p50={:.4} p99={:.4} p99.9={:.4} ms",
                    waits.len() as f64 / seconds,
                    at(0.5),
                    at(0.99),
                    at(0.999)
                );
            }
        });
    }
    #[test]
    fn public_redis_config_requires_a_finite_nonzero_deadline() {
        let cfg = RedisConfig {
            address: "127.0.0.1:1".into(),
            username: None,
            password: None,
            namespace: "test".into(),
            ttl: Duration::from_secs(1),
            deadline: Duration::ZERO,
            max_reply_bytes: 1024,
            tls: None,
        };
        assert!(
            RedisDiscovery::new(cfg, vec![PeerId("a".into())], 1, DiscoveryMode::Directory)
                .is_err()
        );
    }
    #[tokio::test]
    async fn modes_bound_queries_and_choose_owner_plus_one_replica() {
        let peers = (0..6).map(|n| PeerId(format!("p{n}"))).collect::<Vec<_>>();
        let deterministic =
            FixedDiscovery::new_with_mode(peers.clone(), 4, DiscoveryMode::Deterministic).unwrap();
        let query = FixedDiscovery::new_with_mode(peers, 4, DiscoveryMode::PeerQuery).unwrap();
        let id = BlockId("block".into());
        let s = scope();
        assert_eq!(deterministic.locate(&s, &id).await.unwrap().len(), 2);
        assert_eq!(query.locate(&s, &id).await.unwrap().len(), 4);
        assert_eq!(query.placement(&s, &id), deterministic.placement(&s, &id));
        assert!(query.heartbeat(&PeerId("injected".into())).await.is_err());
    }
    #[tokio::test]
    async fn redis_outage_is_bounded_trusted_fallback() {
        let peers = vec![PeerId("a".into()), PeerId("b".into())];
        let config = RedisConfig {
            address: "127.0.0.1:1".into(),
            username: None,
            password: None,
            namespace: "outage-test".into(),
            ttl: Duration::from_secs(1),
            deadline: Duration::from_millis(100),
            max_reply_bytes: 8192,
            tls: None,
        };
        let d = RedisDiscovery::new(config, peers.clone(), 2, DiscoveryMode::Directory).unwrap();
        let found = d.locate(&scope(), &BlockId("id".into())).await.unwrap();
        assert!(found.iter().all(|p| peers.contains(p)));
        assert_eq!(found.len(), 2);
    }
    #[tokio::test]
    #[ignore = "requires MOUNT_RS_CACHE_REDIS_ADDRESS pointing to an isolated Redis fixture"]
    async fn real_redis_hints_leases_expire_and_untrusted_ids_never_enter() {
        let address = std::env::var("MOUNT_RS_CACHE_REDIS_ADDRESS").expect("fixture address");
        let config = RedisConfig {
            address,
            username: None,
            password: None,
            namespace: format!("cache-test-{}", std::process::id()),
            ttl: Duration::from_millis(100),
            deadline: Duration::from_secs(1),
            max_reply_bytes: 65536,
            tls: None,
        };
        let peers = (0..5).map(|n| PeerId(format!("n{n}"))).collect::<Vec<_>>();
        let d = RedisDiscovery::new(config, peers.clone(), 2, DiscoveryMode::Directory).unwrap();
        let s = scope();
        let id = BlockId("opaque".into());
        let outside = d.fallback.locate(&s, &id).await.unwrap();
        let holder = peers.iter().find(|p| !outside.contains(p)).unwrap();
        d.heartbeat(holder).await.unwrap();
        d.advertise(&s, &id, holder).await.unwrap();
        assert_eq!(&d.locate(&s, &id).await.unwrap()[0], holder);
        d.command(vec![
            "SET".into(),
            d.key(&s, &id, holder),
            "injected".into(),
        ])
        .await
        .unwrap();
        assert!(
            d.locate(&s, &id)
                .await
                .unwrap()
                .iter()
                .all(|p| peers.contains(p))
        );
        d.advertise(&s, &id, holder).await.unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(d.locate(&s, &id).await.unwrap(), outside);
        d.withdraw(&s, &id, holder).await.unwrap();
    }
    #[tokio::test]
    #[ignore = "requires isolated MOUNT_RS_CACHE_REDIS_ADDRESS fixture"]
    async fn real_redis_tls_auth_and_certificate_validation() {
        let backend = std::env::var("MOUNT_RS_CACHE_REDIS_ADDRESS").expect("fixture address");
        let password = std::env::var("MOUNT_RS_CACHE_REDIS_PASSWORD").ok();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let cert = certificate.cert.der().clone();
        let key =
            rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der())
                .into();
        let server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .unwrap();
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(server));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (stop, mut stopping) = tokio::sync::watch::channel(false);
        let proxy = tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            loop {
                tokio::select! {_=stopping.changed()=>break,accepted=listener.accept()=>{let (tcp,_)=accepted.unwrap();if workers.len()>=4{continue;}let acceptor=acceptor.clone();let backend=backend.clone();workers.spawn(async move{let Ok(Ok(mut tls))=timeout(Duration::from_secs(1),acceptor.accept(tcp)).await else{return;};let mut redis=TcpStream::connect(backend).await.unwrap();let _=tokio::io::copy_bidirectional(&mut tls,&mut redis).await;});},_=workers.join_next(),if !workers.is_empty()=>{}}
            }
        });
        let mut roots = rustls::RootCertStore::empty();
        roots.add(cert).unwrap();
        let cfg = RedisConfig {
            address,
            username: None,
            password,
            namespace: format!("tls-cache-test-{}", std::process::id()),
            ttl: Duration::from_millis(150),
            deadline: Duration::from_secs(1),
            max_reply_bytes: 65536,
            tls: Some(RedisTlsConfig {
                roots,
                server_name: "localhost".into(),
            }),
        };
        let peer = PeerId("trusted".into());
        let d = RedisDiscovery::new(cfg.clone(), vec![peer.clone()], 1, DiscoveryMode::Directory)
            .unwrap();
        d.heartbeat(&peer).await.unwrap();
        d.advertise(&scope(), &BlockId("id".into()), &peer)
            .await
            .unwrap();
        assert_eq!(
            d.locate(&scope(), &BlockId("id".into())).await.unwrap(),
            vec![peer.clone()]
        );
        let mut bad_ca = cfg.clone();
        bad_ca.tls.as_mut().unwrap().roots = rustls::RootCertStore::empty();
        let untrusted =
            RedisDiscovery::new(bad_ca, vec![peer.clone()], 1, DiscoveryMode::Directory).unwrap();
        assert!(untrusted.heartbeat(&peer).await.is_err());
        assert_eq!(
            untrusted
                .locate(&scope(), &BlockId("id".into()))
                .await
                .unwrap(),
            vec![peer.clone()]
        );
        if cfg.password.is_some() {
            let mut bad_auth = cfg;
            bad_auth.password = Some("wrong-fixture-password".into());
            let rejected =
                RedisDiscovery::new(bad_auth, vec![peer.clone()], 1, DiscoveryMode::Directory)
                    .unwrap();
            assert!(rejected.heartbeat(&peer).await.is_err());
        }
        let _ = stop.send(true);
        proxy.await.unwrap();
    }
}
