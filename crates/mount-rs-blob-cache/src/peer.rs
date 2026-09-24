use crate::*;
use async_trait::async_trait;
use bytes::Bytes;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use tokio::{
    sync::{Mutex, Semaphore},
    time::timeout,
};
const MAX_HEADER_BYTES: usize = 25 + 4 * 1024;
fn peer_transfer_charge(max: usize) -> Result<u32> {
    max.checked_add(MAX_HEADER_BYTES)
        .and_then(|n| n.checked_mul(2))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or_else(error)
}
struct ChargedPayload {
    bytes: Bytes,
    _charge: Arc<tokio::sync::OwnedSemaphorePermit>,
}
impl AsRef<[u8]> for ChargedPayload {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
enum PeerPayload<'a> {
    Borrowed(&'a [u8]),
    Shared(Bytes),
}
impl PeerPayload<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Borrowed(b) => b.len(),
            Self::Shared(b) => b.len(),
        }
    }
    fn owned(self) -> Bytes {
        match self {
            Self::Borrowed(b) => Bytes::copy_from_slice(b),
            Self::Shared(b) => b,
        }
    }
}
#[derive(Clone, Debug)]
pub struct PeerEndpoint {
    pub address: SocketAddr,
    pub server_name: String,
    pub certificate_sha256: [u8; 32],
    pub partitions: BTreeSet<String>,
}
pub struct QuicPeerConfig {
    pub local: PeerId,
    pub bind: SocketAddr,
    pub certificates: Vec<CertificateDer<'static>>,
    pub private_key: PrivateKeyDer<'static>,
    pub roots: rustls::RootCertStore,
    pub trusted: BTreeMap<PeerId, PeerEndpoint>,
    pub max_blob_bytes: usize,
    pub max_inflight: usize,
    /// Total byte admission shared across peers, split between serving and requesting.
    pub transfer_bytes: usize,
    pub deadline: Duration,
}
pub struct QuicPeerTransport {
    endpoint: quinn::Endpoint,
    config: QuicPeerConfig,
    cache: Arc<LocalCache>,
    connections: BTreeMap<PeerId, Arc<Mutex<Option<quinn::Connection>>>>,
    permits: Arc<Semaphore>,
    inbound: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    receive_bytes: Arc<Semaphore>,
    request_bytes: Arc<Semaphore>,
    transfer_charge: u32,
    stop: tokio::sync::watch::Sender<bool>,
}
impl QuicPeerTransport {
    pub fn bind(config: QuicPeerConfig, cache: Arc<LocalCache>) -> Result<Arc<Self>> {
        if config.deadline.is_zero()
            || config.deadline > Duration::from_secs(60)
            || config.max_blob_bytes > 256 * 1024 * 1024
            || config.max_inflight == 0
            || config.max_inflight > 65535
            || config.max_blob_bytes == 0
            || config.max_blob_bytes > usize::MAX - 8192
            || config.trusted.len() > 1024
        {
            return Err(error());
        }
        let transfer_charge = peer_transfer_charge(config.max_blob_bytes)?;
        if config.transfer_bytes > 1024 * 1024 * 1024
            || config.transfer_bytes / 2 < transfer_charge as usize
        {
            return Err(error());
        }
        let mut pins = BTreeSet::new();
        if config.trusted.iter().any(|(id, p)| {
            id.0.is_empty()
                || id.0.len() > 1024
                || !pins.insert(p.certificate_sha256)
                || p.partitions.is_empty()
        }) {
            return Err(error());
        }
        let verifier = rustls::server::WebPkiClientVerifier::builder_with_provider(
            Arc::new(config.roots.clone()),
            Arc::new(rustls::crypto::ring::default_provider()),
        )
        .build()
        .map_err(|_| error())?;
        let mut server = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| error())?
        .with_client_cert_verifier(verifier)
        .with_single_cert(config.certificates.clone(), config.private_key.clone_key())
        .map_err(|_| error())?;
        server.alpn_protocols = vec![b"mount-rs-blob-cache/1".to_vec()];
        let mut server = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server).map_err(|_| error())?,
        ));
        let mut limits = quinn::TransportConfig::default();
        limits.max_concurrent_bidi_streams((config.max_inflight as u32).into());
        limits.max_concurrent_uni_streams(0u32.into());
        limits.max_idle_timeout(Some(
            Duration::from_secs(30).try_into().map_err(|_| error())?,
        ));
        limits.stream_receive_window(((config.max_blob_bytes + MAX_HEADER_BYTES) as u32).into());
        limits.receive_window(
            (u32::try_from(config.transfer_bytes / 2).map_err(|_| error())?).into(),
        );
        limits.send_window((config.transfer_bytes / 2).min(8 * 1024 * 1024) as u64);
        let limits = Arc::new(limits);
        let outbound_limits = limits.clone();
        server.transport_config(limits);
        let mut endpoint = quinn::Endpoint::server(server, config.bind).map_err(|_| error())?;
        let mut client = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| error())?
        .with_root_certificates(config.roots.clone())
        .with_client_auth_cert(config.certificates.clone(), config.private_key.clone_key())
        .map_err(|_| error())?;
        client.alpn_protocols = vec![b"mount-rs-blob-cache/1".to_vec()];
        let mut client_config = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client).map_err(|_| error())?,
        ));
        client_config.transport_config(outbound_limits);
        endpoint.set_default_client_config(client_config);
        let (stop, _) = tokio::sync::watch::channel(false);
        let connections = config
            .trusted
            .keys()
            .map(|peer| (peer.clone(), Arc::new(Mutex::new(None))))
            .collect();
        let result = Arc::new(Self {
            endpoint,
            permits: Arc::new(Semaphore::new(config.max_inflight)),
            inbound: Arc::new(Semaphore::new(config.max_inflight)),
            streams: Arc::new(Semaphore::new(config.max_inflight)),
            receive_bytes: Arc::new(Semaphore::new(config.transfer_bytes / 2)),
            request_bytes: Arc::new(Semaphore::new(config.transfer_bytes / 2)),
            transfer_charge,
            config,
            cache,
            connections,
            stop,
        });
        let weak = Arc::downgrade(&result);
        let endpoint = result.endpoint.clone();
        let mut stopping = result.stop.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {incoming=endpoint.accept()=>{let Some(incoming)=incoming else{break};let Some(service)=weak.upgrade()else{break};let Ok(permit)=service.inbound.clone().try_acquire_owned()else{incoming.refuse();continue};tokio::spawn(async move{let _permit=permit;let _=service.accept(incoming).await;});},_=stopping.changed()=>break}
            }
        });
        Ok(result)
    }
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint.local_addr().map_err(|_| error())
    }
    pub async fn shutdown(&self) {
        let _ = self.stop.send(true);
        self.endpoint.close(0u32.into(), b"shutdown");
        self.endpoint.wait_idle().await;
    }
    fn authenticated(&self, c: &quinn::Connection) -> Result<PeerId> {
        let certs = c
            .peer_identity()
            .ok_or_else(error)?
            .downcast::<Vec<CertificateDer<'static>>>()
            .map_err(|_| error())?;
        let leaf = certs.first().ok_or_else(error)?;
        let pin: [u8; 32] = Sha256::digest(leaf.as_ref()).into();
        self.config
            .trusted
            .iter()
            .find(|(_, p)| p.certificate_sha256 == pin)
            .map(|(id, _)| id.clone())
            .ok_or_else(error)
    }
    async fn connection(&self, id: &PeerId) -> Result<quinn::Connection> {
        let peer = self.config.trusted.get(id).ok_or_else(error)?;
        let slot = self.connections.get(id).ok_or_else(error)?;
        let mut slot = slot.lock().await;
        if let Some(connection) = slot.as_ref()
            && connection.close_reason().is_none()
        {
            return Ok(connection.clone());
        }
        let connection = self
            .endpoint
            .connect(peer.address, &peer.server_name)
            .map_err(|_| error())?
            .await
            .map_err(|_| error())?;
        if &self.authenticated(&connection)? != id {
            connection.close(1u32.into(), b"untrusted peer");
            return Err(error());
        }
        *slot = Some(connection.clone());
        Ok(connection)
    }
    async fn accept(self: Arc<Self>, incoming: quinn::Incoming) -> Result<()> {
        let connection = timeout(self.config.deadline, incoming)
            .await
            .map_err(|_| error())?
            .map_err(|_| error())?;
        let id = match self.authenticated(&connection) {
            Ok(id) => id,
            Err(error) => {
                connection.close(1u32.into(), b"untrusted peer");
                return Err(error);
            }
        };
        let mut workers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                completed=workers.join_next(),if !workers.is_empty()=>{let _=completed;},
                stream=timeout(Duration::from_secs(30),connection.accept_bi())=>{
                    let (mut send,mut recv)=match stream{Ok(Ok(stream))=>stream,_=>break};
                    let permit=match self.streams.clone().try_acquire_owned(){Ok(permit) if workers.len()<self.config.max_inflight=>permit,_=>{let _=send.reset(1u32.into());let _=recv.stop(1u32.into());continue;}};
                    let service=self.clone();let id=id.clone();workers.spawn(async move{let _stream=permit;let result=timeout(service.config.deadline,service.process_stream(&id,&mut send,&mut recv)).await;if !matches!(result,Ok(Ok(()))){let _=send.reset(1u32.into());let _=recv.stop(1u32.into());}});
                }
            }
        }
        workers.abort_all();
        while workers.join_next().await.is_some() {}
        Ok(())
    }
    async fn process_stream(
        &self,
        id: &PeerId,
        send: &mut quinn::SendStream,
        recv: &mut quinn::RecvStream,
    ) -> Result<()> {
        let charge = Arc::new(
            self.receive_bytes
                .clone()
                .acquire_many_owned(self.transfer_charge)
                .await
                .map_err(|_| error())?,
        );
        let data = recv
            .read_to_end(self.config.max_blob_bytes + MAX_HEADER_BYTES)
            .await
            .map_err(|_| error())?;
        let (op, scope, block, bytes) = decode(&data, self.config.max_blob_bytes)?;
        let peer = self.config.trusted.get(id).ok_or_else(error)?;
        if !peer.partitions.contains(&scope.identity.partition) {
            return Err(error());
        }
        let policy = self.cache.scope_policy(&scope).ok_or_else(error)?;
        let response = if op == 0 {
            let key = LocalCache::key_hashed(&LocalCache::scope_hash(&scope), &block);
            if let Some(bytes) = self.cache.get_memory_shared_hashed(&key) {
                Some(Bytes::from_owner(bytes))
            } else {
                let disk_charge = charge.clone();
                bounded_cache_work(self.cache.clone(), move |cache| {
                    let _charge = disk_charge;
                    cache.get_disk(&scope, &block, policy).map(Bytes::from)
                })
                .await?
            }
        } else {
            let shared: Arc<[u8]> = Arc::from(bytes);
            let disk_charge = charge.clone();
            bounded_cache_work(self.cache.clone(), move |cache| {
                let _charge = disk_charge;
                cache.insert_shared(&scope, &block, shared, policy)
            })
            .await??;
            Some(Bytes::new())
        };
        let status = if response.is_some() {
            Bytes::from_static(&[1])
        } else {
            Bytes::from_static(&[0])
        };
        let status = Bytes::from_owner(ChargedPayload {
            bytes: status,
            _charge: charge.clone(),
        });
        let payload = Bytes::from_owner(ChargedPayload {
            bytes: response.unwrap_or_default(),
            _charge: charge,
        });
        let mut chunks = [status, payload];
        send.write_all_chunks(&mut chunks)
            .await
            .map_err(|_| error())?;
        send.finish().map_err(|_| error())?;
        Ok(())
    }
    async fn request(
        &self,
        peer: &PeerId,
        scope: &CacheScope,
        id: &BlockId,
        bytes: Option<PeerPayload<'_>>,
    ) -> Result<Option<Bytes>> {
        let p = self.config.trusted.get(peer).ok_or_else(error)?;
        if !p.partitions.contains(&scope.identity.partition) {
            return Err(error());
        }
        let _permit = self
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| error())?;
        if bytes
            .as_ref()
            .is_some_and(|b| b.len() > self.config.max_blob_bytes)
        {
            return Err(error());
        }
        timeout(self.config.deadline, async {
            let charge = Arc::new(
                self.request_bytes
                    .clone()
                    .acquire_many_owned(self.transfer_charge)
                    .await
                    .map_err(|_| error())?,
            );
            let connection = self.connection(peer).await?;
            let (mut send, mut recv) = connection.open_bi().await.map_err(|_| error())?;
            let header = encode_header(
                scope,
                id,
                bytes.as_ref().map(PeerPayload::len),
                self.config.max_blob_bytes,
            )?;
            let put = bytes.is_some();
            let header = Bytes::from_owner(ChargedPayload {
                bytes: Bytes::from(header),
                _charge: charge.clone(),
            });
            let body = Bytes::from_owner(ChargedPayload {
                bytes: bytes.map(PeerPayload::owned).unwrap_or_default(),
                _charge: charge.clone(),
            });
            let mut chunks = [header, body];
            send.write_all_chunks(&mut chunks)
                .await
                .map_err(|_| error())?;
            send.finish().map_err(|_| error())?;
            let mut status = [0];
            recv.read_exact(&mut status).await.map_err(|_| error())?;
            let body = recv
                .read_to_end(if put || status[0] == 0 {
                    0
                } else {
                    self.config.max_blob_bytes
                })
                .await
                .map_err(|_| error())?;
            match status[0] {
                0 => Ok(None),
                1 => Ok(Some(Bytes::from_owner(ChargedPayload {
                    bytes: Bytes::from(body),
                    _charge: charge,
                }))),
                _ => Err(error()),
            }
        })
        .await
        .map_err(|_| error())?
    }
}
async fn bounded_cache_work<R: Send + 'static>(
    cache: Arc<LocalCache>,
    work: impl FnOnce(Arc<LocalCache>) -> R + Send + 'static,
) -> Result<R> {
    let io = cache.io_permit().await?;
    tokio::task::spawn_blocking(move || {
        let _io = io;
        work(cache)
    })
    .await
    .map_err(|_| error())
}
impl Drop for QuicPeerTransport {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        self.endpoint.close(0u32.into(), b"shutdown");
    }
}
#[async_trait]
impl PeerTransport for QuicPeerTransport {
    async fn get(&self, p: &PeerId, s: &CacheScope, id: &BlockId) -> Result<Option<Vec<u8>>> {
        Ok(self
            .request(p, s, id, None)
            .await?
            .map(|bytes| bytes.to_vec()))
    }
    async fn get_shared(&self, p: &PeerId, s: &CacheScope, id: &BlockId) -> Result<Option<Bytes>> {
        self.request(p, s, id, None).await
    }
    async fn put_shared(
        &self,
        p: &PeerId,
        s: &CacheScope,
        id: &BlockId,
        b: Arc<[u8]>,
    ) -> Result<()> {
        if self
            .request(p, s, id, Some(PeerPayload::Shared(Bytes::from_owner(b))))
            .await?
            .is_some()
        {
            Ok(())
        } else {
            Err(error())
        }
    }
    async fn put(&self, p: &PeerId, s: &CacheScope, id: &BlockId, b: &[u8]) -> Result<()> {
        if self
            .request(p, s, id, Some(PeerPayload::Borrowed(b)))
            .await?
            .is_some()
        {
            Ok(())
        } else {
            Err(error())
        }
    }
}
fn encode_header(
    s: &CacheScope,
    id: &BlockId,
    payload: Option<usize>,
    max: usize,
) -> Result<Vec<u8>> {
    if payload.is_some_and(|length| length > max) {
        return Err(error());
    }
    let texts = [
        &s.identity.cluster,
        &s.identity.partition,
        &s.identity.drive,
        &id.0,
    ];
    if texts
        .iter()
        .any(|text| text.is_empty() || text.len() > 1024)
    {
        return Err(error());
    }
    let length = 25 + texts.iter().map(|t| t.len()).sum::<usize>();
    let mut out = Vec::with_capacity(length);
    out.push(u8::from(payload.is_some()));
    for text in texts {
        out.extend_from_slice(&(text.len() as u16).to_be_bytes());
        out.extend_from_slice(text.as_bytes());
    }
    out.extend_from_slice(&s.backing.as_bytes());
    Ok(out)
}
#[cfg(test)]
fn encode(s: &CacheScope, id: &BlockId, payload: Option<&[u8]>, max: usize) -> Result<Vec<u8>> {
    let mut out = encode_header(s, id, payload.map(<[u8]>::len), max)?;
    if let Some(bytes) = payload {
        out.extend_from_slice(bytes);
    }
    Ok(out)
}
fn decode(data: &[u8], max: usize) -> Result<(u8, CacheScope, BlockId, &[u8])> {
    let mut cursor = 0;
    fn take<'a>(d: &'a [u8], c: &mut usize, n: usize) -> Result<&'a [u8]> {
        let end = c.checked_add(n).ok_or_else(error)?;
        let v = d.get(*c..end).ok_or_else(error)?;
        *c = end;
        Ok(v)
    }
    let op = take(data, &mut cursor, 1)?[0];
    if op > 1 {
        return Err(error());
    }
    let mut texts = [""; 4];
    for text in &mut texts {
        let n = u16::from_be_bytes(
            take(data, &mut cursor, 2)?
                .try_into()
                .map_err(|_| error())?,
        ) as usize;
        if n == 0 || n > 1024 {
            return Err(error());
        }
        *text = std::str::from_utf8(take(data, &mut cursor, n)?).map_err(|_| error())?;
    }
    let backing = ConcurrentBackingId::from_bytes(
        take(data, &mut cursor, 16)?
            .try_into()
            .map_err(|_| error())?,
    )?;
    let bytes = &data[cursor..];
    if bytes.len() > max || (op == 0 && !bytes.is_empty()) {
        return Err(error());
    }
    let mut t = texts.into_iter().map(str::to_owned);
    Ok((
        op,
        CacheScope {
            identity: ScopeIdentity {
                cluster: t.next().unwrap(),
                partition: t.next().unwrap(),
                drive: t.next().unwrap(),
            },
            backing,
        },
        BlockId(t.next().unwrap()),
        bytes,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_disk_work_holds_global_permit_until_the_worker_exits() {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(LocalCacheConfig {
            directory: dir.path().into(),
            memory_bytes: 16,
            disk_bytes: 64,
            max_entries: 4,
            max_blob_bytes: 16,
        })
        .unwrap();
        let (started, ready) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let c = cache.clone();
        let task = tokio::spawn(bounded_cache_work(c, move |_| {
            started.send(()).unwrap();
            blocked.recv().unwrap();
        }));
        ready.await.unwrap();
        task.abort();
        let _ = task.await;
        let available = cache.io_permits.available_permits();
        release.send(()).unwrap();
        timeout(Duration::from_secs(1), async {
            while cache.io_permits.available_permits() != 8 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            available, 7,
            "noncancellable disk work must retain its global slot after async cancellation"
        );
    }
    #[tokio::test]
    async fn queued_payload_ownership_retains_its_transfer_charge() {
        let budget = Arc::new(Semaphore::new(16));
        let charge = Arc::new(budget.clone().acquire_many_owned(8).await.unwrap());
        let bytes = Bytes::from_owner(ChargedPayload {
            bytes: Bytes::from_static(b"payload"),
            _charge: charge.clone(),
        });
        drop(charge);
        let retained = bytes.clone();
        drop(bytes);
        assert_eq!(
            budget.available_permits(),
            8,
            "queued QUIC ownership must retain byte charge"
        );
        drop(retained);
        assert_eq!(budget.available_permits(), 16);
        assert!(peer_transfer_charge(usize::MAX).is_err());
    }
    #[test]
    fn peer_headers_keep_v1_bytes_and_decode_borrows_payload() {
        let scope = CacheScope {
            identity: ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            backing: ConcurrentBackingId::from_bytes([2; 16]).unwrap(),
        };
        let id = BlockId("opaque".into());
        let frame = encode(&scope, &id, Some(b"payload"), 8).unwrap();
        let header = encode_header(&scope, &id, Some(7), 8).unwrap();
        assert_eq!(header.len(), 25 + 1 + 1 + 1 + 6);
        assert_eq!(header.capacity(), header.len());
        assert_eq!(&frame[..header.len()], header);
        let decoded = decode(&frame, 8).unwrap();
        assert_eq!(decoded.3.as_ptr(), frame[header.len()..].as_ptr());
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(LocalCacheConfig {
            directory: dir.path().into(),
            memory_bytes: 16,
            disk_bytes: 0,
            max_entries: 4,
            max_blob_bytes: 16,
        })
        .unwrap();
        let shared: Arc<[u8]> = Arc::from(b"payload".as_slice());
        cache
            .insert_memory_shared(&scope, &id, shared.clone(), IntegrityPolicy::Opaque)
            .unwrap();
        let first = cache
            .get_memory_shared_hashed(&LocalCache::key_hashed(
                &LocalCache::scope_hash(&scope),
                &id,
            ))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &shared));
    }
    #[test]
    fn binary_frames_reject_oversize_truncation_and_unknown_operation() {
        let s = CacheScope {
            identity: ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            backing: ConcurrentBackingId::from_bytes([2; 16]).unwrap(),
        };
        let id = BlockId("opaque".into());
        let f = encode(&s, &id, Some(b"bytes"), 8).unwrap();
        assert_eq!(decode(&f, 8).unwrap().3, b"bytes");
        assert!(decode(&f, 4).is_err());
        for n in 0..f.len() - 5 {
            assert!(decode(&f[..n], 8).is_err());
        }
        let mut bad = f;
        bad[0] = 2;
        assert!(decode(&bad, 8).is_err());
    }
    fn cert(
        ca: &rcgen::CertifiedIssuer<'static, rcgen::KeyPair>,
    ) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec!["localhost".into()]).unwrap();
        params.extended_key_usages = vec![
            rcgen::ExtendedKeyUsagePurpose::ServerAuth,
            rcgen::ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let cert = params.signed_by(&key, ca).unwrap();
        (
            cert.der().clone(),
            rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()).into(),
        )
    }
    #[tokio::test]
    async fn actual_mtls_quic_cache_only_roundtrip_and_partition_denial() {
        let mut params = rcgen::CertificateParams::new(vec!["cache-ca".into()]).unwrap();
        params.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
        let ca = rcgen::CertifiedIssuer::self_signed(params, rcgen::KeyPair::generate().unwrap())
            .unwrap();
        let (ac, ak) = cert(&ca);
        let (bc, bk) = cert(&ca);
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca.der().clone()).unwrap();
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let cache = |d: &std::path::Path| {
            LocalCache::new(LocalCacheConfig {
                directory: d.to_owned(),
                memory_bytes: 1024,
                disk_bytes: 2048,
                max_entries: 10,
                max_blob_bytes: 1024,
            })
            .unwrap()
        };
        let a_cache = cache(ad.path());
        let b_cache = cache(bd.path());
        let s = CacheScope {
            identity: ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            backing: ConcurrentBackingId::from_bytes([2; 16]).unwrap(),
        };
        a_cache
            .register_scope(s.clone(), IntegrityPolicy::Opaque)
            .unwrap();
        b_cache
            .register_scope(s.clone(), IntegrityPolicy::Opaque)
            .unwrap();
        let addr = || {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
            socket.local_addr().unwrap()
        };
        let aa = addr();
        let ba = addr();
        let ep = |address, cert: &CertificateDer<'static>| PeerEndpoint {
            address,
            server_name: "localhost".into(),
            certificate_sha256: Sha256::digest(cert.as_ref()).into(),
            partitions: BTreeSet::from(["p".into()]),
        };
        let config = |local, bind, cert, key, trusted| QuicPeerConfig {
            local: PeerId(local),
            bind,
            certificates: vec![cert],
            private_key: key,
            roots: roots.clone(),
            trusted,
            max_blob_bytes: 1024,
            max_inflight: 128,
            transfer_bytes: 128 * 1024 * 1024,
            deadline: Duration::from_secs(2),
        };
        let blackhole = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let failed_peer = PeerId("blackhole".into());
        let a = QuicPeerTransport::bind(
            config(
                "a".into(),
                aa,
                ac.clone(),
                ak,
                BTreeMap::from([
                    (PeerId("b".into()), ep(ba, &bc)),
                    (
                        failed_peer.clone(),
                        PeerEndpoint {
                            address: blackhole.local_addr().unwrap(),
                            server_name: "localhost".into(),
                            certificate_sha256: [77; 32],
                            partitions: BTreeSet::from(["p".into()]),
                        },
                    ),
                ]),
            ),
            a_cache,
        )
        .unwrap();
        let b = QuicPeerTransport::bind(
            config(
                "b".into(),
                ba,
                bc,
                bk,
                BTreeMap::from([(PeerId("a".into()), ep(aa, &ac))]),
            ),
            b_cache,
        )
        .unwrap();
        let peer = PeerId("b".into());
        let id = BlockId("opaque".into());
        assert!(a.get(&peer, &s, &id).await.unwrap().is_none());
        a.put(&peer, &s, &id, b"binary\0bytes").await.unwrap();
        assert_eq!(
            a.get(&peer, &s, &id).await.unwrap(),
            Some(b"binary\0bytes".to_vec())
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            a.get(&peer, &s, &id).await.unwrap(),
            Some(b"binary\0bytes".to_vec())
        );
        let mut readers = tokio::task::JoinSet::new();
        for _ in 0..100 {
            let a = a.clone();
            let peer = peer.clone();
            let s = s.clone();
            let id = id.clone();
            readers.spawn(async move { a.get(&peer, &s, &id).await });
        }
        while let Some(result) = readers.join_next().await {
            assert_eq!(result.unwrap().unwrap(), Some(b"binary\0bytes".to_vec()));
        }
        let connection = a.connection(&peer).await.unwrap();
        let (mut stalled_send, mut stalled_recv) = connection.open_bi().await.unwrap();
        stalled_send
            .write_all(&encode(&s, &id, None, 1024).unwrap())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        let start = std::time::Instant::now();
        let concurrent = a.get(&peer, &s, &id).await;
        let latency = start.elapsed();
        let _ = stalled_send.reset(0u32.into());
        let _ = stalled_recv.stop(0u32.into());
        assert_eq!(concurrent.unwrap(), Some(b"binary\0bytes".to_vec()));
        assert!(
            latency < Duration::from_millis(200),
            "one unfinished stream stalled unrelated reads: {latency:?}"
        );
        let transport = a.clone();
        let failed_scope = s.clone();
        let failed_id = id.clone();
        let failed =
            tokio::spawn(
                async move { transport.get(&failed_peer, &failed_scope, &failed_id).await },
            );
        tokio::time::sleep(Duration::from_millis(30)).await;
        let start = std::time::Instant::now();
        let healthy = a.get(&peer, &s, &id).await;
        let latency = start.elapsed();
        failed.abort();
        let _ = failed.await;
        assert_eq!(healthy.unwrap(), Some(b"binary\0bytes".to_vec()));
        assert!(
            latency < Duration::from_millis(200),
            "failed peer handshake blocked a healthy persistent connection: {latency:?}"
        );
        let mut denied = s;
        denied.identity.partition = "other".into();
        assert!(a.get(&peer, &denied, &id).await.is_err());
        a.shutdown().await;
        b.shutdown().await;
    }
    #[tokio::test]
    async fn actual_quic_rejects_mismatched_pin_and_unknown_client_ca() {
        for unknown_ca in [false, true] {
            let make_ca = || {
                let mut p = rcgen::CertificateParams::new(vec!["cache-ca".into()]).unwrap();
                p.is_ca = rcgen::IsCa::Ca(rcgen::BasicConstraints::Unconstrained);
                rcgen::CertifiedIssuer::self_signed(p, rcgen::KeyPair::generate().unwrap()).unwrap()
            };
            let ca = make_ca();
            let foreign = make_ca();
            let (ac, ak) = cert(if unknown_ca { &foreign } else { &ca });
            let (bc, bk) = cert(&ca);
            let mut roots = rustls::RootCertStore::empty();
            roots.add(ca.der().clone()).unwrap();
            let ad = tempfile::tempdir().unwrap();
            let bd = tempfile::tempdir().unwrap();
            let cache = |d: &std::path::Path| {
                LocalCache::new(LocalCacheConfig {
                    directory: d.into(),
                    memory_bytes: 1024,
                    disk_bytes: 2048,
                    max_entries: 10,
                    max_blob_bytes: 1024,
                })
                .unwrap()
            };
            let a_cache = cache(ad.path());
            let b_cache = cache(bd.path());
            let scope = CacheScope {
                identity: ScopeIdentity {
                    cluster: "c".into(),
                    partition: "p".into(),
                    drive: "d".into(),
                },
                backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
            };
            a_cache
                .register_scope(scope.clone(), IntegrityPolicy::Opaque)
                .unwrap();
            b_cache
                .register_scope(scope.clone(), IntegrityPolicy::Opaque)
                .unwrap();
            let addr = || {
                std::net::UdpSocket::bind("127.0.0.1:0")
                    .unwrap()
                    .local_addr()
                    .unwrap()
            };
            let aa = addr();
            let ba = addr();
            let endpoint = |address, cert: &CertificateDer<'static>, wrong| PeerEndpoint {
                address,
                server_name: "localhost".into(),
                certificate_sha256: if wrong {
                    [99; 32]
                } else {
                    Sha256::digest(cert.as_ref()).into()
                },
                partitions: BTreeSet::from(["p".into()]),
            };
            let cfg = |local, bind, cert, key, trusted| QuicPeerConfig {
                local: PeerId(local),
                bind,
                certificates: vec![cert],
                private_key: key,
                roots: roots.clone(),
                trusted,
                max_blob_bytes: 1024,
                max_inflight: 4,
                transfer_bytes: 128 * 1024 * 1024,
                deadline: Duration::from_millis(500),
            };
            let a = QuicPeerTransport::bind(
                cfg(
                    "a".into(),
                    aa,
                    ac.clone(),
                    ak,
                    BTreeMap::from([(PeerId("b".into()), endpoint(ba, &bc, !unknown_ca))]),
                ),
                a_cache,
            )
            .unwrap();
            let b = QuicPeerTransport::bind(
                cfg(
                    "b".into(),
                    ba,
                    bc,
                    bk,
                    BTreeMap::from([(PeerId("a".into()), endpoint(aa, &ac, false))]),
                ),
                b_cache.clone(),
            )
            .unwrap();
            let id = BlockId("opaque".into());
            assert!(
                a.put(&PeerId("b".into()), &scope, &id, b"untrusted")
                    .await
                    .is_err()
            );
            assert!(b_cache.get(&scope, &id, IntegrityPolicy::Opaque).is_none());
            a.shutdown().await;
            b.shutdown().await;
        }
    }
}
