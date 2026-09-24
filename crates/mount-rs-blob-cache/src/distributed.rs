use crate::*;
use async_trait::async_trait;
use std::{collections::BTreeSet, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    time::timeout,
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
    pub max_inflight_misses: usize,
}
impl Default for DistributedConfig {
    fn default() -> Self {
        Self {
            max_peer_queries: 3,
            deadline: Duration::from_millis(500),
            maintenance_capacity: 64,
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
        p.sort_by_key(|peer| {
            digest(&[
                scope.digest().as_bytes(),
                id.0.as_bytes(),
                peer.0.as_bytes(),
            ])
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
    config: RedisConfig,
    fallback: Arc<FixedDiscovery>,
    mode: DiscoveryMode,
    connection: tokio::sync::Mutex<Option<Box<dyn RedisStream>>>,
}
trait RedisStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> RedisStream for T {}
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
            config,
            fallback: FixedDiscovery::new_with_mode(peers, max_queries, DiscoveryMode::PeerQuery)?,
            mode,
            connection: tokio::sync::Mutex::new(None),
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
        timeout(self.config.deadline,async {
            let mut slot=self.connection.lock().await;
            // The socket belongs to this command until the complete response is
            // consumed. Dropping the future closes it and leaves the pool empty.
            let mut stream=match slot.take(){Some(stream)=>stream,None=>{
                let tcp=TcpStream::connect(&self.config.address).await.map_err(|_|error())?;tcp.set_nodelay(true).map_err(|_|error())?;
                let mut stream:Box<dyn RedisStream>=if let Some(tls)=&self.config.tls {
                    let config=rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider())).with_safe_default_protocol_versions().map_err(|_|error())?.with_root_certificates(tls.roots.clone()).with_no_client_auth();
                    let name=rustls::pki_types::ServerName::try_from(tls.server_name.clone()).map_err(|_|error())?;
                    Box::new(tokio_rustls::TlsConnector::from(Arc::new(config)).connect(name,tcp).await.map_err(|_|error())?)
                }else{if !tcp.peer_addr().map_err(|_|error())?.ip().is_loopback(){return Err(error());}Box::new(tcp)};
                if let Some(password)=&self.config.password{let mut auth=vec!["AUTH".to_owned()];if let Some(user)=&self.config.username{auth.push(user.clone());}auth.push(password.clone());send(&mut *stream,&auth).await?;if !matches!(reply(&mut *stream,self.config.max_reply_bytes).await?,Reply::Text(v) if v=="OK"){return Err(error());}}
                stream
            }};
            send(&mut *stream,&args).await?;let response=reply(&mut *stream,self.config.max_reply_bytes).await?;
            *slot=Some(stream);Ok(response)
        }).await.map_err(|_|error())?
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
