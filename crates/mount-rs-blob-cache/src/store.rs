use crate::*;
use async_trait::async_trait;
use mount_rs_core::storage::{BlockReconcileReport, BlockStore};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Semaphore, mpsc},
    time::timeout,
};

#[derive(Default)]
pub struct CacheMetrics {
    pub local_hits: AtomicU64,
    pub peer_hits: AtomicU64,
    pub backing_fetches: AtomicU64,
    pub hit_bytes: AtomicU64,
    pub cache_errors: AtomicU64,
    pub maintenance_dropped: AtomicU64,
}
#[derive(Clone, Debug, Default)]
pub struct CacheMetricsSnapshot {
    pub local_hits: u64,
    pub peer_hits: u64,
    pub backing_fetches: u64,
    pub hit_bytes: u64,
    pub cache_errors: u64,
    pub maintenance_dropped: u64,
}
impl CacheMetrics {
    pub fn snapshot(&self) -> CacheMetricsSnapshot {
        CacheMetricsSnapshot {
            local_hits: self.local_hits.load(Ordering::Relaxed),
            peer_hits: self.peer_hits.load(Ordering::Relaxed),
            backing_fetches: self.backing_fetches.load(Ordering::Relaxed),
            hit_bytes: self.hit_bytes.load(Ordering::Relaxed),
            cache_errors: self.cache_errors.load(Ordering::Relaxed),
            maintenance_dropped: self.maintenance_dropped.load(Ordering::Relaxed),
        }
    }
    fn local_hit(&self, len: usize) {
        self.local_hits.fetch_add(1, Ordering::Relaxed);
        self.hit_bytes.fetch_add(len as u64, Ordering::Relaxed);
    }
}
struct Maintenance {
    scope: Arc<CacheScope>,
    id: BlockId,
    bytes: Arc<[u8]>,
    _reservation: Arc<PendingReservation>,
}
struct Advertisement {
    scope: Arc<CacheScope>,
    id: BlockId,
}
/// Bounded placement workers and miss budget shared by all server drives.
pub struct DistributedRuntime {
    discovery: Arc<dyn Discovery>,
    transport: Arc<dyn PeerTransport>,
    local: PeerId,
    config: DistributedConfig,
    queue: mpsc::Sender<Maintenance>,
    advertisements: mpsc::Sender<Advertisement>,
    advertisement_period: Option<Duration>,
    misses: Arc<Semaphore>,
    flights: Mutex<BTreeMap<String, Weak<tokio::sync::Mutex<()>>>>,
    stopping: AtomicBool,
    stop: tokio::sync::watch::Sender<bool>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl DistributedRuntime {
    pub fn new(
        discovery: Arc<dyn Discovery>,
        transport: Arc<dyn PeerTransport>,
        local: PeerId,
        config: DistributedConfig,
    ) -> Result<Arc<Self>> {
        if config.maintenance_capacity == 0
            || config.maintenance_capacity > 4096
            || config.placement_concurrency == 0
            || config.placement_concurrency > 32
            || config.hedge_delay > Duration::from_secs(30)
            || config.max_inflight_misses == 0
            || config.max_inflight_misses > 1024
            || config.max_peer_queries == 0
            || config.max_peer_queries > 256
            || config.deadline.is_zero()
        {
            return Err(error());
        }
        let (queue, mut receiver) = mpsc::channel::<Maintenance>(config.maintenance_capacity);
        let (advertisements, mut hints) =
            mpsc::channel::<Advertisement>(config.maintenance_capacity);
        let advertisement_period = discovery.advertisement_refresh_interval();
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let d = discovery.clone();
        let t = transport.clone();
        let p = local.clone();
        let deadline = config.deadline;
        let placement_concurrency = config.placement_concurrency;
        let heartbeat_period = discovery
            .heartbeat_interval()
            .max(Duration::from_millis(100));
        let worker = tokio::spawn(async move {
            let mut placements = tokio::task::JoinSet::new();
            let mut heartbeat = tokio::time::interval(heartbeat_period);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _=stopped.changed()=>{break;}
                    _=placements.join_next(), if !placements.is_empty()=>{}
                    item=receiver.recv(), if placements.len()<placement_concurrency=>{
                        let Some(item)=item else {break};
                        let d=d.clone(); let t=t.clone(); let p=p.clone();
                        placements.spawn(async move {
                            let _=timeout(deadline,async {
                                let targets=distributed::unique(d.placement(&item.scope,&item.id),&p,2);
                                let put=|target: PeerId| {
                                    let d=d.clone(); let t=t.clone(); let scope=item.scope.clone();
                                    let id=item.id.clone(); let bytes=item.bytes.clone();
                                    async move {
                                        if t.put_shared(&target,&scope,&id,bytes).await.is_ok() {
                                            let _=d.advertise(&scope,&id,&target).await;
                                        }
                                    }
                                };
                                let mut targets=targets.into_iter();
                                let first_target=targets.next(); let second_target=targets.next();
                                let first=async {if let Some(target)=first_target{put(target).await;}};
                                let second=async {if let Some(target)=second_target{put(target).await;}};
                                let _=tokio::join!(first,second,d.advertise(&item.scope,&item.id,&p));
                            }).await;
                            drop(item);
                        });
                    }
                    hint=hints.recv()=>{let Some(hint)=hint else{break};let _=timeout(deadline,d.advertise(&hint.scope,&hint.id,&p)).await;}
                    _=heartbeat.tick()=>{let _=timeout(deadline,d.heartbeat(&p)).await;}
                }
            }
        });
        Ok(Arc::new(Self {
            discovery,
            transport,
            local,
            misses: Arc::new(Semaphore::new(config.max_inflight_misses)),
            flights: Mutex::new(BTreeMap::new()),
            config,
            queue,
            advertisements,
            advertisement_period,
            stopping: AtomicBool::new(false),
            stop,
            worker: Mutex::new(Some(worker)),
        }))
    }
    fn touch(&self, cache: &LocalCache, scope: &Arc<CacheScope>, id: &BlockId, key: &CacheKey) {
        if self.stopping.load(Ordering::Acquire) || id.0.len() > 1024 {
            return;
        }
        if let Some(period) = self.advertisement_period
            && cache.should_advertise_key(key, period)
        {
            let _ = self.advertisements.try_send(Advertisement {
                scope: scope.clone(),
                id: id.clone(),
            });
        }
    }
    fn enqueue(
        &self,
        cache: &LocalCache,
        scope: Arc<CacheScope>,
        id: BlockId,
        bytes: Arc<[u8]>,
        metrics: &CacheMetrics,
    ) {
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
        let Some(reservation) = cache.reserve_pending(bytes.len()) else {
            metrics.maintenance_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.enqueue_reserved(scope, id, bytes, Arc::new(reservation), metrics);
    }
    fn enqueue_reserved(
        &self,
        scope: Arc<CacheScope>,
        id: BlockId,
        bytes: Arc<[u8]>,
        reservation: Arc<PendingReservation>,
        metrics: &CacheMetrics,
    ) {
        if self.stopping.load(Ordering::Acquire) {
            return;
        }
        if self
            .queue
            .try_send(Maintenance {
                scope,
                id,
                bytes,
                _reservation: reservation,
            })
            .is_err()
        {
            metrics.maintenance_dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
    fn flight(&self, key: String) -> Result<Arc<tokio::sync::Mutex<()>>> {
        let mut flights = self.flights.lock().map_err(|_| error())?;
        flights.retain(|_, f| f.strong_count() > 0);
        if let Some(f) = flights.get(&key).and_then(Weak::upgrade) {
            return Ok(f);
        }
        let f = Arc::new(tokio::sync::Mutex::new(()));
        flights.insert(key, Arc::downgrade(&f));
        Ok(f)
    }
    pub async fn shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
        let _ = self.stop.send(true);
        let worker = self.worker.lock().ok().and_then(|mut w| w.take());
        if let Some(mut worker) = worker
            && timeout(self.config.deadline + Duration::from_secs(1), &mut worker)
                .await
                .is_err()
        {
            worker.abort();
            let _ = worker.await;
        }
    }
}
impl Drop for DistributedRuntime {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Ok(mut worker) = self.worker.lock()
            && let Some(worker) = worker.take()
        {
            worker.abort();
        }
    }
}
struct PendingBlob {
    scope: Arc<CacheScope>,
    id: BlockId,
    bytes: Arc<[u8]>,
    _reservation: Arc<PendingReservation>,
}
pub struct CachedBlockStore {
    backing: Arc<dyn BlockStore>,
    cache: Arc<LocalCache>,
    identity: ScopeIdentity,
    policy: IntegrityPolicy,
    scope: Mutex<Option<(Arc<CacheScope>, CacheKey)>>,
    distributed: Option<Arc<DistributedRuntime>>,
    metrics: Arc<CacheMetrics>,
    pending: Mutex<Vec<PendingBlob>>,
    flush_gate: tokio::sync::Mutex<()>,
    write_gate: tokio::sync::RwLock<()>,
    dirty: AtomicBool,
}
impl CachedBlockStore {
    pub fn new(
        backing: Arc<dyn BlockStore>,
        cache: Arc<LocalCache>,
        identity: ScopeIdentity,
        policy: IntegrityPolicy,
    ) -> Self {
        Self {
            backing,
            cache,
            identity,
            policy,
            scope: Mutex::new(None),
            distributed: None,
            metrics: Arc::new(CacheMetrics::default()),
            pending: Mutex::new(Vec::new()),
            flush_gate: tokio::sync::Mutex::new(()),
            write_gate: tokio::sync::RwLock::new(()),
            dirty: AtomicBool::new(false),
        }
    }
    pub fn with_runtime(mut self, runtime: Arc<DistributedRuntime>) -> Self {
        self.distributed = Some(runtime);
        self
    }
    pub fn with_distributed(
        self,
        discovery: Arc<dyn Discovery>,
        transport: Arc<dyn PeerTransport>,
        local: PeerId,
        config: DistributedConfig,
    ) -> Result<Self> {
        Ok(self.with_runtime(DistributedRuntime::new(
            discovery, transport, local, config,
        )?))
    }
    pub fn metrics(&self) -> Arc<CacheMetrics> {
        self.metrics.clone()
    }
    fn current(&self) -> Option<Arc<CacheScope>> {
        self.current_hashed().map(|(scope, _)| scope)
    }
    fn current_hashed(&self) -> Option<(Arc<CacheScope>, CacheKey)> {
        self.scope.lock().ok()?.clone()
    }
    fn activate(&self, backing: ConcurrentBackingId) -> Result<()> {
        if [
            &self.identity.cluster,
            &self.identity.partition,
            &self.identity.drive,
        ]
        .iter()
        .any(|v| v.is_empty() || v.len() > 256)
        {
            return Err(error());
        }
        let mut active = self.scope.lock().map_err(|_| error())?;
        if active.as_ref().is_some_and(|(s, _)| s.backing == backing) {
            return Ok(());
        }
        let scope = Arc::new(CacheScope {
            identity: self.identity.clone(),
            backing,
        });
        self.cache.register_scope((*scope).clone(), self.policy)?;
        if let Some((previous, _)) = active.take() {
            self.cache.unregister_scope(&previous);
        }
        let fingerprint = LocalCache::scope_hash(&scope);
        *active = Some((scope, fingerprint));
        Ok(())
    }
    fn deactivate(&self) {
        if let Ok(mut s) = self.scope.lock()
            && let Some((scope, _)) = s.take()
        {
            self.cache.unregister_scope(&scope);
        }
        if let Ok(mut p) = self.pending.lock() {
            p.clear();
        }
    }
    async fn disk_get(&self, scope: &Arc<CacheScope>, id: &BlockId) -> Option<Vec<u8>> {
        let deadline = self
            .distributed
            .as_ref()
            .map(|d| d.config.deadline)
            .unwrap_or(Duration::from_millis(500));
        timeout(deadline, async {
            let permit = self.cache.io_permit().await.ok()?;
            let cache = self.cache.clone();
            let scope = scope.clone();
            let id = id.clone();
            let policy = self.policy;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                cache.get_disk(&scope, &id, policy)
            })
            .await
            .ok()
            .flatten()
        })
        .await
        .ok()
        .flatten()
    }
    async fn fill(
        &self,
        scope: &Arc<CacheScope>,
        id: &BlockId,
        bytes: Arc<[u8]>,
        reservation: Option<Arc<PendingReservation>>,
    ) {
        if bytes.len() > self.cache.max_blob_bytes() {
            return;
        }
        if self
            .cache
            .insert_memory_shared(scope, id, bytes.clone(), self.policy)
            .is_err()
        {
            self.metrics.cache_errors.fetch_add(1, Ordering::Relaxed);
            return;
        }
        // Optional disk copies never delay backing acknowledgement. Both payload and
        // non-cancellable worker remain charged until the blocking closure finishes.
        let Ok(permit) = self.cache.io_permits.clone().try_acquire_owned() else {
            self.metrics
                .maintenance_dropped
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        let reservation =
            reservation.or_else(|| self.cache.reserve_pending(bytes.len()).map(Arc::new));
        let Some(reservation) = reservation else {
            self.metrics
                .maintenance_dropped
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        let cache = self.cache.clone();
        let scope = scope.clone();
        let id = id.clone();
        let policy = self.policy;
        let metrics = self.metrics.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _reservation = reservation;
            if cache.insert_shared(&scope, &id, bytes, policy).is_err() {
                metrics.cache_errors.fetch_add(1, Ordering::Relaxed);
            }
        });
    }
    async fn fetch(&self, scope: &Arc<CacheScope>, id: &BlockId) -> Result<Vec<u8>> {
        if let Some(bytes) = self.cache.get_memory(scope, id, self.policy) {
            self.metrics.local_hit(bytes.len());
            if let Some(d) = &self.distributed {
                d.touch(
                    &self.cache,
                    scope,
                    id,
                    &LocalCache::key_hashed(&LocalCache::scope_hash(scope), id),
                );
            }
            return Ok(bytes);
        }
        if let Some(bytes) = self.disk_get(scope, id).await {
            self.metrics.local_hit(bytes.len());
            if let Some(d) = &self.distributed {
                d.touch(
                    &self.cache,
                    scope,
                    id,
                    &LocalCache::key_hashed(&LocalCache::scope_hash(scope), id),
                );
            }
            return Ok(bytes);
        }
        if self.dirty.load(Ordering::Acquire) {
            self.metrics.backing_fetches.fetch_add(1, Ordering::Relaxed);
            return self.backing.get(id).await;
        }
        if let Some(d) = &self.distributed {
            let peer_result = timeout(d.config.deadline, async {
                let peers = d.discovery.locate(scope, id).await?;
                let mut peers=distributed::unique(peers,&d.local,d.config.max_peer_queries).into_iter();
                let mut queries=tokio::task::JoinSet::new();
                let start=|queries: &mut tokio::task::JoinSet<_>, peer: PeerId| {
                    let transport=d.transport.clone(); let scope=scope.clone(); let id=id.clone();
                    queries.spawn(async move {transport.get_shared(&peer,&scope,&id).await});
                };
                if let Some(peer)=peers.next(){start(&mut queries,peer);}
                let hedge=tokio::time::sleep(d.config.hedge_delay.min(d.config.deadline/4));
                tokio::pin!(hedge);
                let mut hedged=false;
                while !queries.is_empty() {
                    tokio::select! {
                        _=&mut hedge, if !hedged=>{
                            hedged=true;
                            if let Some(peer)=peers.next(){start(&mut queries,peer);}
                        }
                        result=queries.join_next()=>{
                            match result {
                                Some(Ok(Ok(Some(bytes)))) if bytes.len()<=self.cache.max_blob_bytes() && self.policy.verify(id,&bytes).is_ok()=>return Ok(Some(bytes)),
                                Some(Ok(Ok(None)))=>{},
                                _=>{self.metrics.cache_errors.fetch_add(1,Ordering::Relaxed);}
                            }
                            if let Some(peer)=peers.next(){start(&mut queries,peer);}
                        }
                    }
                }
                Ok::<_, mount_rs_core::FsError>(None)
            })
            .await;
            if let Ok(Ok(Some(bytes))) = peer_result {
                self.metrics.peer_hits.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .hit_bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                self.fill(scope, id, Arc::from(bytes.as_ref()), None).await;
                d.touch(
                    &self.cache,
                    scope,
                    id,
                    &LocalCache::key_hashed(&LocalCache::scope_hash(scope), id),
                );
                return Ok(bytes.to_vec());
            }
        }
        self.metrics.backing_fetches.fetch_add(1, Ordering::Relaxed);
        let bytes = self.backing.get(id).await?;
        self.policy.verify(id, &bytes)?;
        if !self.dirty.load(Ordering::Acquire) && bytes.len() <= self.cache.max_blob_bytes() {
            let shared = Arc::from(bytes.as_slice());
            self.fill(scope, id, Arc::clone(&shared), None).await;
            if let Some(d) = &self.distributed {
                d.enqueue(
                    &self.cache,
                    scope.clone(),
                    id.clone(),
                    shared,
                    &self.metrics,
                );
            }
        }
        Ok(bytes)
    }
}
impl Drop for CachedBlockStore {
    fn drop(&mut self) {
        self.deactivate();
    }
}
#[async_trait]
impl BlockStore for CachedBlockStore {
    fn durable(&self) -> bool {
        self.backing.durable()
    }
    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        match self.backing.prepare_concurrent_backing().await {
            Ok(id) => {
                if self.activate(id).is_err() {
                    self.metrics.cache_errors.fetch_add(1, Ordering::Relaxed);
                    self.deactivate();
                }
                Ok(id)
            }
            Err(e) => {
                self.deactivate();
                Err(e)
            }
        }
    }
    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        match self.backing.verify_concurrent_backing(expected).await {
            Ok(()) => {
                if self.activate(expected).is_err() {
                    self.metrics.cache_errors.fetch_add(1, Ordering::Relaxed);
                    self.deactivate();
                }
                Ok(())
            }
            Err(e) => {
                self.deactivate();
                Err(e)
            }
        }
    }
    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.backing.get_for_migration(id).await
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let _gate = self.write_gate.read().await;
        self.dirty.store(true, Ordering::Release);
        let id = self.backing.put(bytes).await?;
        if let Some(scope) = self.current()
            && let Some(reservation) = self.cache.reserve_pending(bytes.len())
            && self.policy.verify(&id, bytes).is_ok()
        {
            self.pending.lock().map_err(|_| error())?.push(PendingBlob {
                scope,
                id: id.clone(),
                bytes: Arc::from(bytes),
                _reservation: Arc::new(reservation),
            });
        }
        Ok(id)
    }
    fn get<'life0, 'life1, 'async_trait>(
        &'life0 self,
        id: &'life1 BlockId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<u8>>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        let Some((scope, fingerprint)) = self.current_hashed() else {
            return self.backing.get(id);
        };
        let key = LocalCache::key_hashed(&fingerprint, id);
        if let Some(bytes) = self.cache.get_memory_hashed(&key) {
            self.metrics.local_hit(bytes.len());
            if let Some(d) = &self.distributed {
                d.touch(&self.cache, &scope, id, &key);
            }
            // The hit future holds only the ready result, not the cold-path state machine.
            return Box::pin(std::future::ready(Ok(bytes)));
        }
        Box::pin(async move {
            if let Some(d) = &self.distributed {
                let _permit = d
                    .misses
                    .clone()
                    .acquire_owned()
                    .await
                    .map_err(|_| error())?;
                let flight = d.flight(digest(&[scope.digest().as_bytes(), id.0.as_bytes()]))?;
                let _guard = flight.lock().await;
                return self.fetch(&scope, id).await;
            }
            self.fetch(&scope, id).await
        })
    }
    async fn flush(&self) -> Result<()> {
        let _gate = self.flush_gate.lock().await;
        let writes = self.write_gate.write().await;
        // Take the batch before the barrier: puts completed afterwards wait for a later flush.
        let pending = std::mem::take(&mut *self.pending.lock().map_err(|_| error())?);
        self.backing.flush().await?;
        self.dirty.store(false, Ordering::Release);
        drop(writes);
        for blob in pending {
            if self
                .current()
                .is_some_and(|s| s.backing == blob.scope.backing)
            {
                self.fill(
                    &blob.scope,
                    &blob.id,
                    blob.bytes.clone(),
                    Some(blob._reservation.clone()),
                )
                .await;
                if let Some(d) = &self.distributed {
                    d.enqueue_reserved(
                        blob.scope,
                        blob.id,
                        blob.bytes,
                        blob._reservation,
                        &self.metrics,
                    );
                }
            }
        }
        Ok(())
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.backing.delete(id).await?;
        self.pending
            .lock()
            .map_err(|_| error())?
            .retain(|b| &b.id != id);
        if let Some(scope) = self.current() {
            let permit = self.cache.io_permit().await?;
            let cache = self.cache.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                cache.invalidate(&scope, &id)
            })
            .await
            .map_err(|_| error())?;
        }
        Ok(())
    }
    async fn reconcile(
        &self,
        live: &BTreeSet<BlockId>,
        grace: Duration,
    ) -> Result<BlockReconcileReport> {
        let report = self.backing.reconcile(live, grace).await?;
        self.pending
            .lock()
            .map_err(|_| error())?
            .retain(|b| live.contains(&b.id));
        if let Some(scope) = self.current() {
            let permit = self.cache.io_permit().await?;
            let cache = self.cache.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                cache.invalidate_scope(&scope)
            })
            .await
            .map_err(|_| error())?;
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    struct Backing {
        bytes: Mutex<BTreeMap<BlockId, Vec<u8>>>,
        gets: AtomicU64,
        flushes: AtomicU64,
        fail_flush: AtomicBool,
    }
    impl Backing {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                bytes: Mutex::new(BTreeMap::new()),
                gets: AtomicU64::new(0),
                flushes: AtomicU64::new(0),
                fail_flush: AtomicBool::new(false),
            })
        }
    }
    #[async_trait]
    impl BlockStore for Backing {
        fn durable(&self) -> bool {
            true
        }
        async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
            ConcurrentBackingId::from_bytes([1; 16])
        }
        async fn verify_concurrent_backing(&self, id: ConcurrentBackingId) -> Result<()> {
            if id.as_bytes() == [1; 16] {
                Ok(())
            } else {
                Err(error())
            }
        }
        async fn put(&self, b: &[u8]) -> Result<BlockId> {
            let id = BlockId(format!("b{:x}", Sha256::digest(b)));
            self.bytes.lock().unwrap().insert(id.clone(), b.to_vec());
            Ok(id)
        }
        async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
            self.gets.fetch_add(1, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(10)).await;
            self.bytes
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .ok_or_else(error)
        }
        async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
            self.get(id).await
        }
        async fn flush(&self) -> Result<()> {
            self.flushes.fetch_add(1, Ordering::Relaxed);
            if self.fail_flush.load(Ordering::Relaxed) {
                Err(error())
            } else {
                Ok(())
            }
        }
        async fn delete(&self, id: &BlockId) -> Result<()> {
            self.bytes.lock().unwrap().remove(id);
            Ok(())
        }
    }
    fn fixture(
        backing: Arc<Backing>,
    ) -> (tempfile::TempDir, Arc<LocalCache>, Arc<CachedBlockStore>) {
        let dir = tempfile::tempdir().unwrap();
        let cache = LocalCache::new(LocalCacheConfig {
            directory: dir.path().join("cache"),
            memory_bytes: 8192,
            disk_bytes: 16384,
            max_entries: 128,
            max_blob_bytes: 4096,
        })
        .unwrap();
        let store = Arc::new(CachedBlockStore::new(
            backing,
            cache.clone(),
            ScopeIdentity {
                cluster: "test".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            IntegrityPolicy::Sha256Prefixed,
        ));
        (dir, cache, store)
    }
    #[tokio::test]
    async fn write_admission_waits_for_flush_without_forcing_extra_barriers() {
        let backing = Backing::new();
        let (_dir, cache, store) = fixture(backing.clone());
        store.prepare_concurrent_backing().await.unwrap();
        let id = store.put(b"bytes").await.unwrap();
        assert_eq!(
            backing.flushes.load(Ordering::Relaxed),
            0,
            "put must not add barriers"
        );
        assert_eq!(
            cache.usage().2,
            0,
            "unflushed bytes must not be served to peers"
        );
        store.flush().await.unwrap();
        assert_eq!(store.get(&id).await.unwrap(), b"bytes");
        assert_eq!(backing.gets.load(Ordering::Relaxed), 0);
    }
    #[tokio::test]
    async fn failed_flush_never_admits_pending_blob() {
        let backing = Backing::new();
        let (_dir, cache, store) = fixture(backing.clone());
        store.prepare_concurrent_backing().await.unwrap();
        store.put(b"bytes").await.unwrap();
        backing.fail_flush.store(true, Ordering::Relaxed);
        assert!(store.flush().await.is_err());
        assert_eq!(cache.usage().2, 0);
    }
    #[tokio::test]
    async fn read_of_pending_write_cannot_populate_peer_cache() {
        let backing = Backing::new();
        let (_dir, cache, store) = fixture(backing);
        store.prepare_concurrent_backing().await.unwrap();
        let id = store.put(b"unflushed").await.unwrap();
        assert_eq!(store.get(&id).await.unwrap(), b"unflushed");
        assert_eq!(cache.usage().2, 0);
        store.flush().await.unwrap();
        assert_eq!(cache.usage().2, 1);
    }
    #[tokio::test]
    async fn migration_bypasses_a_warm_cache_and_authority_failure_deactivates() {
        let backing = Backing::new();
        let (_dir, _cache, store) = fixture(backing.clone());
        store.prepare_concurrent_backing().await.unwrap();
        let id = store.put(b"bytes").await.unwrap();
        store.flush().await.unwrap();
        store.get(&id).await.unwrap();
        store.get_for_migration(&id).await.unwrap();
        assert_eq!(backing.gets.load(Ordering::Relaxed), 1);
        assert!(
            store
                .verify_concurrent_backing(ConcurrentBackingId::from_bytes([2; 16]).unwrap())
                .await
                .is_err()
        );
        store.get(&id).await.unwrap();
        assert_eq!(backing.gets.load(Ordering::Relaxed), 2);
    }
    struct TestPeer {
        bytes: Option<Vec<u8>>,
        calls: AtomicU64,
    }
    #[async_trait]
    impl PeerTransport for TestPeer {
        async fn get(&self, _: &PeerId, _: &CacheScope, _: &BlockId) -> Result<Option<Vec<u8>>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.bytes.clone())
        }
        async fn put(&self, _: &PeerId, _: &CacheScope, _: &BlockId, _: &[u8]) -> Result<()> {
            Ok(())
        }
    }
    fn runtime(peer: Arc<TestPeer>) -> Arc<DistributedRuntime> {
        DistributedRuntime::new(
            FixedDiscovery::new(vec![PeerId("self".into()), PeerId("peer".into())], 2).unwrap(),
            peer,
            PeerId("self".into()),
            DistributedConfig::default(),
        )
        .unwrap()
    }
    #[tokio::test]
    async fn hundred_simultaneous_cold_reads_coalesce_to_one_backing_get() {
        let backing = Backing::new();
        let id = backing.put(b"committed").await.unwrap();
        let (_dir, cache, _unused) = fixture(backing.clone());
        let peer = Arc::new(TestPeer {
            bytes: None,
            calls: AtomicU64::new(0),
        });
        let runtime = runtime(peer);
        let store = Arc::new(
            CachedBlockStore::new(
                backing.clone(),
                cache,
                ScopeIdentity {
                    cluster: "test".into(),
                    partition: "p".into(),
                    drive: "fanout".into(),
                },
                IntegrityPolicy::Sha256Prefixed,
            )
            .with_runtime(runtime.clone()),
        );
        store.prepare_concurrent_backing().await.unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(100));
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..100 {
            let store = store.clone();
            let id = id.clone();
            let barrier = barrier.clone();
            tasks.spawn(async move {
                barrier.wait().await;
                assert_eq!(store.get(&id).await.unwrap(), b"committed");
            });
        }
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
        assert_eq!(backing.gets.load(Ordering::Relaxed), 1);
        assert_eq!(store.metrics().snapshot().backing_fetches, 1);
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn corrupt_peer_content_falls_back_and_opaque_peer_content_is_usable() {
        for policy in [IntegrityPolicy::Sha256Prefixed, IntegrityPolicy::Opaque] {
            let backing = Backing::new();
            let id = backing.put(b"original").await.unwrap();
            let (_dir, cache, _unused) = fixture(backing.clone());
            let peer = Arc::new(TestPeer {
                bytes: Some(if policy == IntegrityPolicy::Opaque {
                    b"original".to_vec()
                } else {
                    b"corrupt".to_vec()
                }),
                calls: AtomicU64::new(0),
            });
            let runtime = runtime(peer.clone());
            let store = CachedBlockStore::new(
                backing.clone(),
                cache,
                ScopeIdentity {
                    cluster: "test".into(),
                    partition: "p".into(),
                    drive: "peer".into(),
                },
                policy,
            )
            .with_runtime(runtime.clone());
            store.prepare_concurrent_backing().await.unwrap();
            assert_eq!(store.get(&id).await.unwrap(), b"original");
            assert_eq!(peer.calls.load(Ordering::Relaxed), 1);
            assert_eq!(
                backing.gets.load(Ordering::Relaxed),
                u64::from(policy != IntegrityPolicy::Opaque)
            );
            runtime.shutdown().await;
        }
    }
    #[tokio::test]
    async fn durable_flush_does_not_wait_for_saturated_cache_disk_workers() {
        let backing = Backing::new();
        let (_dir, cache, store) = fixture(backing);
        store.prepare_concurrent_backing().await.unwrap();
        store.put(b"bytes").await.unwrap();
        let mut held = Vec::new();
        for _ in 0..8 {
            held.push(cache.io_permit().await.unwrap());
        }
        let result = timeout(Duration::from_millis(100), store.flush()).await;
        assert!(
            matches!(result, Ok(Ok(()))),
            "durable acknowledgement must not wait for optional cache IO"
        );
        assert_eq!(
            store
                .get(&BlockId(format!("b{:x}", Sha256::digest(b"bytes"))))
                .await
                .unwrap(),
            b"bytes"
        );
        drop(held);
    }
    struct OrderedPeers;
    #[async_trait]
    impl Discovery for OrderedPeers {
        async fn locate(&self, _: &CacheScope, _: &BlockId) -> Result<Vec<PeerId>> {
            Ok(vec![PeerId("slow".into()), PeerId("fast".into())])
        }
        fn placement(&self, _: &CacheScope, _: &BlockId) -> Vec<PeerId> {
            vec![PeerId("slow".into()), PeerId("fast".into())]
        }
        async fn advertise(&self, _: &CacheScope, _: &BlockId, _: &PeerId) -> Result<()> {
            Ok(())
        }
        async fn withdraw(&self, _: &CacheScope, _: &BlockId, _: &PeerId) -> Result<()> {
            Ok(())
        }
        async fn heartbeat(&self, _: &PeerId) -> Result<()> {
            Ok(())
        }
    }
    struct LatencyPeer {
        bytes: Vec<u8>,
        calls: AtomicU64,
        active: AtomicU64,
        peak: AtomicU64,
        release: tokio::sync::Notify,
    }
    struct ActivePlacement<'a>(&'a AtomicU64);
    impl Drop for ActivePlacement<'_> {
        fn drop(&mut self) {
            self.0.fetch_sub(1, Ordering::Relaxed);
        }
    }
    #[async_trait]
    impl PeerTransport for LatencyPeer {
        async fn get(&self, peer: &PeerId, _: &CacheScope, _: &BlockId) -> Result<Option<Vec<u8>>> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            if peer.0 == "slow" {
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Ok(Some(self.bytes.clone()))
        }
        async fn put(&self, _: &PeerId, _: &CacheScope, _: &BlockId, _: &[u8]) -> Result<()> {
            let active = self.active.fetch_add(1, Ordering::Relaxed) + 1;
            self.peak.fetch_max(active, Ordering::Relaxed);
            let _active = ActivePlacement(&self.active);
            self.release.notified().await;
            Ok(())
        }
    }
    fn latency_peer() -> Arc<LatencyPeer> {
        Arc::new(LatencyPeer {
            bytes: b"original".to_vec(),
            calls: AtomicU64::new(0),
            active: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            release: tokio::sync::Notify::new(),
        })
    }
    #[tokio::test]
    async fn slow_owner_does_not_hide_a_healthy_replica() {
        let backing = Backing::new();
        let id = backing.put(b"original").await.unwrap();
        let (_dir, cache, _) = fixture(backing.clone());
        let peer = latency_peer();
        let runtime = DistributedRuntime::new(
            Arc::new(OrderedPeers),
            peer.clone(),
            PeerId("self".into()),
            DistributedConfig {
                deadline: Duration::from_millis(200),
                ..DistributedConfig::default()
            },
        )
        .unwrap();
        let store = CachedBlockStore::new(
            backing.clone(),
            cache,
            ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            IntegrityPolicy::Sha256Prefixed,
        )
        .with_runtime(runtime.clone());
        store.prepare_concurrent_backing().await.unwrap();
        let started = std::time::Instant::now();
        assert_eq!(store.get(&id).await.unwrap(), b"original");
        assert_eq!(
            backing.gets.load(Ordering::Relaxed),
            0,
            "healthy replica must beat origin fallback"
        );
        assert!(started.elapsed() < Duration::from_millis(150));
        assert_eq!(peer.calls.load(Ordering::Relaxed), 2);
        runtime.shutdown().await;
    }
    #[tokio::test]
    async fn placement_tasks_overlap_with_a_fixed_upper_bound() {
        let backing = Backing::new();
        let (_dir, cache, _) = fixture(backing);
        let peer = latency_peer();
        let runtime = DistributedRuntime::new(
            Arc::new(OrderedPeers),
            peer.clone(),
            PeerId("self".into()),
            DistributedConfig::default(),
        )
        .unwrap();
        let scope = Arc::new(CacheScope {
            identity: ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
        });
        let metrics = CacheMetrics::default();
        for n in 0..16 {
            runtime.enqueue(
                &cache,
                scope.clone(),
                BlockId(n.to_string()),
                Arc::from(b"bytes".as_slice()),
                &metrics,
            );
        }
        let overlap = timeout(Duration::from_millis(100), async {
            while peer.active.load(Ordering::Relaxed) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        assert!(overlap.is_ok(), "placement must overlap blocked transfers");
        assert!(
            peer.peak.load(Ordering::Relaxed) <= 8,
            "four tasks, at most two replicas each"
        );
        peer.release.notify_waiters();
        runtime.shutdown().await;
        assert_eq!(peer.active.load(Ordering::Relaxed), 0);
    }
}
