use crate::owned::OwnedTask;
use crate::*;
use async_trait::async_trait;
use mount_rs_core::diagnostics::{
    profile,
    storage::{Operation, Span},
};
use mount_rs_core::storage::{BlockReconcileReport, BlockStore};
use std::{
    collections::{BTreeMap, BTreeSet},
    future::{Future, poll_fn},
    pin::Pin,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::Poll,
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
    lease: ScopeLease,
    id: BlockId,
    bytes: Arc<[u8]>,
    _reservation: Arc<PendingReservation>,
}
struct Advertisement {
    scope: Arc<CacheScope>,
    lease: ScopeLease,
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
    worker: Arc<OwnedTask>,
    failure: Mutex<Option<mount_rs_core::FsError>>,
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
            let mut failure = None;
            let mut heartbeat = tokio::time::interval(heartbeat_period);
            heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _=stopped.changed()=>{break;}
                    completed=placements.join_next(), if !placements.is_empty()=>{
                        if matches!(completed,Some(Err(_))) {failure.get_or_insert_with(error);}
                    }
                    item=receiver.recv(), if placements.len()<placement_concurrency=>{
                        let Some(item)=item else {break};
                        let d=d.clone(); let t=t.clone(); let p=p.clone();
                        placements.spawn(async move {
                            let _=timeout(deadline,async {
                                if !item.lease.is_current().unwrap_or(false) { return; }
                                let targets=distributed::unique(d.placement(&item.scope,&item.id),&p,2);
                                let put=|target: PeerId| {
                                    let d=d.clone(); let t=t.clone(); let scope=item.scope.clone();
                                    let id=item.id.clone(); let bytes=item.bytes.clone();
                                    let lease=item.lease.clone();
                                    async move {
                                        if !lease.is_current().unwrap_or(false) { return; }
                                        if t.put_shared(&target,&scope,&id,bytes).await.is_ok()
                                            && lease.is_current().unwrap_or(false)
                                        {
                                            let _=d.advertise(&scope,&id,&target).await;
                                        }
                                    }
                                };
                                let mut targets=targets.into_iter();
                                let first_target=targets.next(); let second_target=targets.next();
                                let first=async {if let Some(target)=first_target{put(target).await;}};
                                let second=async {if let Some(target)=second_target{put(target).await;}};
                                let local_advertise=async {
                                    if item.lease.is_current().unwrap_or(false) {
                                        let _=d.advertise(&item.scope,&item.id,&p).await;
                                    }
                                };
                                let _=tokio::join!(first,second,local_advertise);
                            }).await;
                            drop(item);
                        });
                    }
                    hint=hints.recv()=>{let Some(hint)=hint else{break};if hint.lease.is_current().unwrap_or(false) {let _=timeout(deadline,d.advertise(&hint.scope,&hint.id,&p)).await;}}
                    _=heartbeat.tick()=>{let _=timeout(deadline,d.heartbeat(&p)).await;}
                }
            }
            receiver.close();
            hints.close();
            // Every placement has its existing deadline. Stop admitting work,
            // then positively join it rather than dropping/aborting the set.
            while let Some(result) = placements.join_next().await {
                if result.is_err() {
                    failure.get_or_insert_with(error);
                }
            }
            failure.map_or(Ok(()), Err)
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
            worker: OwnedTask::new(worker),
            failure: Mutex::new(None),
        }))
    }
    fn touch(
        &self,
        cache: &LocalCache,
        scope: &Arc<CacheScope>,
        lease: &ScopeLease,
        id: &BlockId,
        key: &CacheKey,
    ) {
        if self.stopping.load(Ordering::Acquire)
            || id.0.len() > 1024
            || !lease.is_current().unwrap_or(false)
        {
            return;
        }
        if let Some(period) = self.advertisement_period
            && cache.should_advertise_key(key, period)
        {
            let _ = self.advertisements.try_send(Advertisement {
                scope: scope.clone(),
                lease: lease.clone(),
                id: id.clone(),
            });
        }
    }
    fn enqueue(
        &self,
        cache: &LocalCache,
        scope: Arc<CacheScope>,
        lease: ScopeLease,
        id: BlockId,
        bytes: Arc<[u8]>,
        metrics: &CacheMetrics,
    ) {
        if self.stopping.load(Ordering::Acquire) || !lease.is_current().unwrap_or(false) {
            return;
        }
        let Some(reservation) = cache.reserve_pending(bytes.len()) else {
            metrics.maintenance_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        };
        self.enqueue_reserved(scope, lease, id, bytes, Arc::new(reservation), metrics);
    }
    fn enqueue_reserved(
        &self,
        scope: Arc<CacheScope>,
        lease: ScopeLease,
        id: BlockId,
        bytes: Arc<[u8]>,
        reservation: Arc<PendingReservation>,
        metrics: &CacheMetrics,
    ) {
        if self.stopping.load(Ordering::Acquire) || !lease.is_current().unwrap_or(false) {
            return;
        }
        if self
            .queue
            .try_send(Maintenance {
                scope,
                lease,
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
    pub async fn shutdown(&self) -> Result<()> {
        self.stopping.store(true, Ordering::Release);
        self.misses.close();
        let _ = self.stop.send(true);
        let joined = timeout(
            self.config.deadline + Duration::from_secs(1),
            self.worker.join(),
        )
        .await
        .unwrap_or_else(|_| {
            Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Ebusy)
                .with_syscall("cache maintenance drain timeout"))
        });
        let mut failure = match self.failure.lock() {
            Ok(failure) => failure,
            Err(poisoned) => {
                let mut failure = poisoned.into_inner();
                failure.get_or_insert_with(error);
                failure
            }
        };
        if let Err(error) = joined {
            failure.get_or_insert(error);
        }
        failure.clone().map_or(Ok(()), Err)
    }
}
impl Drop for DistributedRuntime {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}
struct PendingBlob {
    scope: Arc<CacheScope>,
    lease: ScopeLease,
    id: BlockId,
    bytes: Arc<[u8]>,
    _reservation: Arc<PendingReservation>,
}
pub struct CachedBlockStore {
    backing: Arc<dyn BlockStore>,
    cache: Arc<LocalCache>,
    identity: ScopeIdentity,
    policy: IntegrityPolicy,
    scope: Mutex<Option<(Arc<CacheScope>, CacheKey, ScopeLease)>>,
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
        Self::new_with_metrics(
            backing,
            cache,
            identity,
            policy,
            Arc::new(CacheMetrics::default()),
        )
    }

    /// Reuse a configured Drive's counters across its runtime generations.
    /// The caller owns the metric bank's lifetime independently of this store.
    pub fn new_with_metrics(
        backing: Arc<dyn BlockStore>,
        cache: Arc<LocalCache>,
        identity: ScopeIdentity,
        policy: IntegrityPolicy,
        metrics: Arc<CacheMetrics>,
    ) -> Self {
        Self {
            backing,
            cache,
            identity,
            policy,
            scope: Mutex::new(None),
            distributed: None,
            metrics,
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
        self.current_hashed().map(|(scope, _, _)| scope)
    }
    fn current_hashed(&self) -> Option<(Arc<CacheScope>, CacheKey, ScopeLease)> {
        let mut active = self.scope.lock().ok()?;
        if !active.as_ref()?.2.is_current().unwrap_or(false) {
            active.take();
            return None;
        }
        active.clone()
    }
    fn activate(&self, backing: ConcurrentBackingId, verified_from: IdentityEpoch) -> Result<()> {
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
        // Serialize this store's publication before the shared epoch CAS. No
        // late successful attempt can overwrite a newer store-owned lease.
        let mut active = self.scope.lock().map_err(|_| error())?;
        if let Some((scope, _, lease)) = active.as_ref()
            && scope.backing == backing
            && lease.accepts_verified_epoch(&verified_from)?
        {
            return Ok(());
        }
        let scope = Arc::new(CacheScope {
            identity: self.identity.clone(),
            backing,
        });
        let lease =
            self.cache
                .register_scope_lease((*scope).clone(), self.policy, verified_from)?;
        let fingerprint = LocalCache::scope_hash(&scope);
        *active = Some((scope, fingerprint, lease));
        Ok(())
    }
    fn deactivate(&self) {
        if let Ok(mut active) = self.scope.lock() {
            active.take();
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }
    fn deactivate_if_stale(&self) {
        // A losing old epoch may finish after a newer verifier published a
        // current lease on this same store. Preserve that newer valid owner.
        if let Ok(mut active) = self.scope.lock() {
            if active
                .as_ref()
                .is_some_and(|(_, _, lease)| lease.is_current().unwrap_or(false))
            {
                return;
            }
            active.take();
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
    }
    async fn disk_get(
        &self,
        scope: &Arc<CacheScope>,
        lease: &ScopeLease,
        id: &BlockId,
    ) -> Option<Vec<u8>> {
        let deadline = self
            .distributed
            .as_ref()
            .map(|d| d.config.deadline)
            .unwrap_or(Duration::from_millis(500));
        let mut span = Span::new(Operation::BlobCacheDiskLookup);
        let result = timeout(deadline, async {
            let scope = scope.clone();
            let lease = lease.clone();
            let id = id.clone();
            let policy = self.policy;
            self.cache
                .run_blocking(move |cache| {
                    if !lease.is_current().unwrap_or(false) {
                        return None;
                    }
                    cache.get_disk(&scope, &id, policy)
                })
                .await
                .ok()
                .flatten()
        })
        .await;
        match result {
            Ok(bytes) => {
                let returned = bytes.as_ref().map_or(0, |bytes| bytes.len() as u64);
                span.finish_success(returned);
                if bytes.is_some() {
                    profile::add(profile::Event::BlobCacheDiskHitBytes, returned);
                }
                bytes
            }
            Err(_) => {
                span.finish_error();
                None
            }
        }
    }
    async fn fill(
        &self,
        scope: &Arc<CacheScope>,
        lease: &ScopeLease,
        id: &BlockId,
        bytes: Arc<[u8]>,
        reservation: Option<Arc<PendingReservation>>,
    ) {
        if bytes.len() > self.cache.max_blob_bytes() || !lease.is_current().unwrap_or(false) {
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
        let reservation =
            reservation.or_else(|| self.cache.reserve_pending(bytes.len()).map(Arc::new));
        let Some(reservation) = reservation else {
            self.metrics
                .maintenance_dropped
                .fetch_add(1, Ordering::Relaxed);
            return;
        };
        let scope = scope.clone();
        let lease = lease.clone();
        let id = id.clone();
        let policy = self.policy;
        let metrics = self.metrics.clone();
        if self
            .cache
            .try_spawn_blocking(move |cache| {
                let _reservation = reservation;
                if !lease.is_current().unwrap_or(false) {
                    return;
                }
                if cache.insert_shared(&scope, &id, bytes, policy).is_err() {
                    metrics.cache_errors.fetch_add(1, Ordering::Relaxed);
                }
            })
            .is_err()
        {
            self.metrics
                .maintenance_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    async fn fetch(
        &self,
        scope: &Arc<CacheScope>,
        lease: &ScopeLease,
        id: &BlockId,
    ) -> Result<Vec<u8>> {
        if !lease.is_current().unwrap_or(false) {
            self.metrics.backing_fetches.fetch_add(1, Ordering::Relaxed);
            return self.backing.get(id).await;
        }
        let bytes = {
            let mut span = Span::new(Operation::BlobCacheRamLookup);
            let bytes = self.cache.get_memory(scope, id, self.policy);
            let returned = bytes.as_ref().map_or(0, |bytes| bytes.len() as u64);
            span.finish_success(returned);
            if bytes.is_some() {
                profile::add(profile::Event::BlobCacheRamHitBytes, returned);
            }
            bytes
        };
        if let Some(bytes) = bytes.filter(|_| lease.is_current().unwrap_or(false)) {
            self.metrics.local_hit(bytes.len());
            if let Some(d) = &self.distributed {
                d.touch(
                    &self.cache,
                    scope,
                    lease,
                    id,
                    &LocalCache::key_hashed(&LocalCache::scope_hash(scope), id),
                );
            }
            return Ok(bytes);
        }
        if let Some(bytes) = self
            .disk_get(scope, lease, id)
            .await
            .filter(|_| lease.is_current().unwrap_or(false))
        {
            self.metrics.local_hit(bytes.len());
            if let Some(d) = &self.distributed {
                d.touch(
                    &self.cache,
                    scope,
                    lease,
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
                let peers = {
                    let mut span = Span::new(Operation::BlobCacheDiscoveryLocate);
                    match d.discovery.locate(scope, id).await {
                        Ok(peers) => {
                            span.finish_success(0);
                            peers
                        }
                        Err(error) => {
                            span.finish_error();
                            return Err(error);
                        }
                    }
                };
                if !lease.is_current().unwrap_or(false) { return Ok(None); }
                let mut peers=distributed::unique(peers,&d.local,d.config.max_peer_queries).into_iter();
                type Query<'a> = Pin<Box<dyn Future<Output=Result<Option<bytes::Bytes>>> + Send + 'a>>;
                let mut queries: Vec<Query<'_>>=Vec::with_capacity(2);
                fn start<'a>(queries: &mut Vec<Query<'a>>, d: &'a DistributedRuntime, scope: &'a CacheScope, id: &'a BlockId, peer: PeerId) {
                    queries.push(Box::pin(async move {d.transport.get_shared(&peer,scope,id).await}));
                }
                if let Some(peer)=peers.next(){start(&mut queries,d,scope,id,peer);}
                let hedge=tokio::time::sleep(d.config.hedge_delay.min(d.config.deadline/4));
                tokio::pin!(hedge);
                let mut hedged=false;
                while !queries.is_empty() {
                    if !lease.is_current().unwrap_or(false) { return Ok(None); }
                    tokio::select! {
                        _=&mut hedge, if !hedged=>{
                            hedged=true;
                            if let Some(peer)=peers.next(){start(&mut queries,d,scope,id,peer);}
                        }
                        result=poll_fn(|cx| {
                            for index in 0..queries.len() {
                                // Preserve optional-query panic fallback without spawning
                                // owners that can outlive a winning/canceled read.
                                let polled=std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| queries[index].as_mut().poll(cx)));
                                let result=match polled {
                                    Ok(Poll::Pending)=>continue,
                                    Ok(Poll::Ready(result))=>result,
                                    Err(_)=>Err(error()),
                                };
                                drop(queries.swap_remove(index));
                                return Poll::Ready(result);
                            }
                            Poll::Pending
                        })=>{
                            match result {
                                Ok(Some(bytes)) if bytes.len()<=self.cache.max_blob_bytes() && self.policy.verify(id,&bytes).is_ok()=>return Ok(Some(bytes)),
                                Ok(None)=>{},
                                _=>{self.metrics.cache_errors.fetch_add(1,Ordering::Relaxed);}
                            }
                            if let Some(peer)=peers.next(){start(&mut queries,d,scope,id,peer);}
                        }
                    }
                }
                Ok::<_, mount_rs_core::FsError>(None)
            })
            .await;
            if let Ok(Ok(Some(bytes))) = peer_result
                && lease.is_current().unwrap_or(false)
            {
                self.metrics.peer_hits.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .hit_bytes
                    .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                self.fill(scope, lease, id, Arc::from(bytes.as_ref()), None)
                    .await;
                d.touch(
                    &self.cache,
                    scope,
                    lease,
                    id,
                    &LocalCache::key_hashed(&LocalCache::scope_hash(scope), id),
                );
                return Ok(bytes.to_vec());
            }
        }
        self.metrics.backing_fetches.fetch_add(1, Ordering::Relaxed);
        let bytes = self.backing.get(id).await?;
        self.policy.verify(id, &bytes)?;
        if !self.dirty.load(Ordering::Acquire)
            && bytes.len() <= self.cache.max_blob_bytes()
            && lease.is_current().unwrap_or(false)
        {
            let shared = Arc::from(bytes.as_slice());
            self.fill(scope, lease, id, Arc::clone(&shared), None).await;
            if let Some(d) = &self.distributed {
                d.enqueue(
                    &self.cache,
                    scope.clone(),
                    lease.clone(),
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
        let epoch = self.cache.identity_epoch(&self.identity);
        match self.backing.prepare_concurrent_backing().await {
            Ok(id) => {
                if epoch.and_then(|epoch| self.activate(id, epoch)).is_err() {
                    self.metrics.cache_errors.fetch_add(1, Ordering::Relaxed);
                    self.deactivate_if_stale();
                }
                Ok(id)
            }
            Err(error) => {
                if self
                    .cache
                    .revoke_identity_admission(&self.identity)
                    .is_err()
                {
                    self.metrics.cache_errors.fetch_add(1, Ordering::Relaxed);
                }
                self.deactivate();
                Err(error)
            }
        }
    }
    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        let epoch = self.cache.identity_epoch(&self.identity);
        match self.backing.verify_concurrent_backing(expected).await {
            Ok(()) => {
                if epoch
                    .and_then(|epoch| self.activate(expected, epoch))
                    .is_err()
                {
                    self.metrics.cache_errors.fetch_add(1, Ordering::Relaxed);
                    self.deactivate_if_stale();
                }
                Ok(())
            }
            Err(error) => {
                if self
                    .cache
                    .revoke_identity_admission(&self.identity)
                    .is_err()
                {
                    self.metrics.cache_errors.fetch_add(1, Ordering::Relaxed);
                }
                self.deactivate();
                Err(error)
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
        if let Some((scope, _, lease)) = self.current_hashed()
            && let Some(reservation) = self.cache.reserve_pending(bytes.len())
            && self.policy.verify(&id, bytes).is_ok()
        {
            self.pending.lock().map_err(|_| error())?.push(PendingBlob {
                scope,
                lease,
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
        let Some((scope, fingerprint, lease)) = self.current_hashed() else {
            return self.backing.get(id);
        };
        let key = LocalCache::key_hashed(&fingerprint, id);
        let bytes = {
            let mut span = Span::new(Operation::BlobCacheRamLookup);
            let bytes = self.cache.get_memory_hashed(&key);
            let returned = bytes.as_ref().map_or(0, |bytes| bytes.len() as u64);
            span.finish_success(returned);
            if bytes.is_some() {
                profile::add(profile::Event::BlobCacheRamHitBytes, returned);
            }
            bytes
        };
        if let Some(bytes) = bytes.filter(|_| lease.is_current().unwrap_or(false)) {
            self.metrics.local_hit(bytes.len());
            if let Some(d) = &self.distributed {
                d.touch(&self.cache, &scope, &lease, id, &key);
            }
            // The hit future holds only the ready result, not the cold-path state machine.
            return Box::pin(std::future::ready(Ok(bytes)));
        }
        Box::pin(async move {
            if let Some(d) = &self.distributed {
                let _permit = {
                    let mut span = Span::new(Operation::BlobCacheMissAdmissionWait);
                    match d.misses.clone().acquire_owned().await {
                        Ok(permit) => {
                            span.finish_success(0);
                            permit
                        }
                        Err(_) => {
                            span.finish_error();
                            return Err(error());
                        }
                    }
                };
                let flight = d.flight(digest(&[scope.digest().as_bytes(), id.0.as_bytes()]))?;
                let _guard = {
                    let mut span = Span::new(Operation::BlobCacheMissSingleflightWait);
                    let guard = flight.lock().await;
                    span.finish_success(0);
                    guard
                };
                return self.fetch(&scope, &lease, id).await;
            }
            self.fetch(&scope, &lease, id).await
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
            if blob.lease.is_current().unwrap_or(false) {
                self.fill(
                    &blob.scope,
                    &blob.lease,
                    &blob.id,
                    blob.bytes.clone(),
                    Some(blob._reservation.clone()),
                )
                .await;
                if let Some(d) = &self.distributed {
                    d.enqueue_reserved(
                        blob.scope,
                        blob.lease,
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
            let id = id.clone();
            self.cache
                .run_blocking(move |cache| cache.invalidate(&scope, &id))
                .await?;
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
            self.cache
                .run_blocking(move |cache| cache.invalidate_scope(&scope))
                .await?;
        }
        Ok(report)
    }
}

#[cfg(all(test, unix))]
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
    fn runtime(peer: Arc<dyn PeerTransport>) -> Arc<DistributedRuntime> {
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
        runtime.shutdown().await.unwrap();
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
            runtime.shutdown().await.unwrap();
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
    #[tokio::test]
    async fn shutdown_does_not_acknowledge_a_detached_fill_still_holding_disk_ownership() {
        let backing = Backing::new();
        let (dir, cache, store) = fixture(backing);
        store.prepare_concurrent_backing().await.unwrap();
        let scope = store.current().unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (worker_tx, worker_rx) = tokio::sync::oneshot::channel();
        *cache.test_disk_insert_gate.lock().unwrap() = Some(crate::local::TestDiskInsertGate {
            entered: entered_tx,
            release: release_rx,
        });
        *cache.test_fill_worker.lock().unwrap() = Some(worker_tx);
        let bytes = b"detached fill shutdown ownership";
        let id = store.put(bytes).await.unwrap();
        store.flush().await.unwrap();
        // The handle is the one submitted by the actual optional fill path.
        let worker = worker_rx.await.expect("actual fill worker handle retained");
        let entered = timeout(Duration::from_secs(2), entered_rx).await;
        let entered_disk = matches!(entered, Ok(Ok(())));
        let shutdown_returned = if entered_disk {
            // Exceeds LocalCache's unchanged two-second drain timeout.
            timeout(Duration::from_millis(2500), cache.shutdown())
                .await
                .is_ok_and(|result| result.is_ok())
        } else {
            false
        };
        let worker_finished_before_release = worker.poll_result().is_ready();
        let config = LocalCacheConfig {
            directory: dir.path().join("cache"),
            memory_bytes: 8192,
            disk_bytes: 16384,
            max_entries: 128,
            max_blob_bytes: 4096,
        };
        // Remove the test/runtime cache owners. Only the real paused worker
        // may retain the old LocalCache's directory lock at this point.
        drop(store);
        drop(cache);
        let reuse_while_held = LocalCache::new(config.clone());
        let directory_still_owned = reuse_while_held.is_err();
        drop(reuse_while_held);
        // Release and positively join before any assertion or fixture deletion.
        drop(release_tx);
        let joined = worker.join().await;
        let reopened = LocalCache::new(config);
        let fresh_bytes = reopened.as_ref().ok().and_then(|cache| {
            cache
                .get_disk(&scope, &id, IntegrityPolicy::Sha256Prefixed)
                .ok()
                .flatten()
        });
        drop(reopened);
        assert!(
            entered_disk,
            "actual fill did not enter the disk-locked insert"
        );
        assert!(!worker_finished_before_release, "actual fill was not held");
        assert!(
            directory_still_owned,
            "paused worker did not retain the directory lock"
        );
        assert!(joined.is_ok(), "actual fill worker did not positively join");
        assert_eq!(
            fresh_bytes.as_deref(),
            Some(bytes.as_slice()),
            "fresh cache reuse lost completed disk bytes"
        );
        assert!(
            !shutdown_returned,
            "LocalCache shutdown acknowledged before the detached fill released disk ownership"
        );
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
        reads: AtomicU64,
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
            self.reads.fetch_add(1, Ordering::Relaxed);
            let _active = ActivePlacement(&self.reads);
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
            reads: AtomicU64::new(0),
            active: AtomicU64::new(0),
            peak: AtomicU64::new(0),
            release: tokio::sync::Notify::new(),
        })
    }
    #[tokio::test]
    async fn cancelled_maintenance_close_rejoins_actual_placement_owners() {
        let (_dir, cache, _) = fixture(Backing::new());
        let peer = latency_peer();
        let runtime = DistributedRuntime::new(
            Arc::new(OrderedPeers),
            peer.clone(),
            PeerId("self".into()),
            DistributedConfig {
                deadline: Duration::from_secs(2),
                ..Default::default()
            },
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
        let lease = cache
            .register_scope_lease(
                (*scope).clone(),
                IntegrityPolicy::Opaque,
                cache.identity_epoch(&scope.identity).unwrap(),
            )
            .unwrap();
        runtime.enqueue(
            &cache,
            scope.clone(),
            lease.clone(),
            BlockId("held".into()),
            Arc::from(b"bytes".as_slice()),
            &CacheMetrics::default(),
        );
        timeout(Duration::from_secs(1), async {
            while peer.active.load(Ordering::Relaxed) != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let r = runtime.clone();
        let first = tokio::spawn(async move { r.shutdown().await });
        while !runtime.stopping.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        first.abort();
        assert!(first.await.unwrap_err().is_cancelled());
        let before = peer.peak.load(Ordering::Relaxed);
        runtime.enqueue(
            &cache,
            scope,
            lease,
            BlockId("closed".into()),
            Arc::from(b"bytes".as_slice()),
            &CacheMetrics::default(),
        );
        let r = runtime.clone();
        let mut second = tokio::spawn(async move { r.shutdown().await });
        let premature = timeout(Duration::from_millis(50), &mut second).await;
        peer.release.notify_waiters();
        let joined = second.await.unwrap();
        assert!(
            premature.is_err(),
            "maintenance close acknowledged live placement owners"
        );
        assert!(joined.is_ok());
        assert_eq!(peer.active.load(Ordering::Relaxed), 0);
        assert_eq!(
            peer.peak.load(Ordering::Relaxed),
            before,
            "sealed maintenance admitted a new transfer"
        );
        assert_eq!(
            Arc::strong_count(&peer),
            2,
            "actual placement task transport owners must be released before ACK"
        );
    }
    struct PanicPlacement(AtomicBool);
    #[async_trait]
    impl PeerTransport for PanicPlacement {
        async fn get(&self, _: &PeerId, _: &CacheScope, _: &BlockId) -> Result<Option<Vec<u8>>> {
            Ok(None)
        }
        async fn put(&self, _: &PeerId, _: &CacheScope, _: &BlockId, _: &[u8]) -> Result<()> {
            self.0.store(true, Ordering::Release);
            panic!("actual placement task failure")
        }
    }
    #[tokio::test]
    async fn actual_placement_panic_is_a_sticky_failed_close() {
        let (_dir, cache, _) = fixture(Backing::new());
        let peer = Arc::new(PanicPlacement(AtomicBool::new(false)));
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
        let lease = cache
            .register_scope_lease(
                (*scope).clone(),
                IntegrityPolicy::Opaque,
                cache.identity_epoch(&scope.identity).unwrap(),
            )
            .unwrap();
        runtime.enqueue(
            &cache,
            scope,
            lease,
            BlockId("panic".into()),
            Arc::from(b"bytes".as_slice()),
            &CacheMetrics::default(),
        );
        // Wait for the actual supervisor to consume the accepted item, rather
        // than racing shutdown against a still-queued placement.
        timeout(Duration::from_secs(1), async {
            while !peer.0.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for _ in 0..2 {
            assert!(
                runtime
                    .shutdown()
                    .await
                    .is_err_and(|e| e.code == ErrorCode::Eio)
            );
        }
    }
    struct PanicQuery;
    #[async_trait]
    impl PeerTransport for PanicQuery {
        async fn get(&self, _: &PeerId, _: &CacheScope, _: &BlockId) -> Result<Option<Vec<u8>>> {
            panic!("optional inline peer query failure")
        }
        async fn put(&self, _: &PeerId, _: &CacheScope, _: &BlockId, _: &[u8]) -> Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn inline_peer_query_panic_preserves_exact_backing_fallback() {
        let backing = Backing::new();
        let id = backing.put(b"panic fallback").await.unwrap();
        let (_dir, cache, _) = fixture(backing.clone());
        let runtime = runtime(Arc::new(PanicQuery));
        let store = CachedBlockStore::new(
            backing.clone(),
            cache.clone(),
            ScopeIdentity {
                cluster: "c".into(),
                partition: "p".into(),
                drive: "d".into(),
            },
            IntegrityPolicy::Sha256Prefixed,
        )
        .with_runtime(runtime.clone());
        store.prepare_concurrent_backing().await.unwrap();
        let bytes = store.get(&id).await.unwrap();
        let errors = store.metrics().snapshot().cache_errors;
        runtime.shutdown().await.unwrap();
        cache.shutdown().await.unwrap();
        assert_eq!(bytes, b"panic fallback");
        assert_eq!(backing.gets.load(Ordering::Relaxed), 1);
        assert_eq!(
            errors, 1,
            "optional query panic must retain its cache error accounting"
        );
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
        assert_eq!(
            peer.reads.load(Ordering::Relaxed),
            0,
            "completed hedged read must release every actual query owner"
        );
        runtime.shutdown().await.unwrap();
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
        let lease = cache
            .register_scope_lease(
                (*scope).clone(),
                IntegrityPolicy::Opaque,
                cache.identity_epoch(&scope.identity).unwrap(),
            )
            .unwrap();
        let metrics = CacheMetrics::default();
        for n in 0..16 {
            runtime.enqueue(
                &cache,
                scope.clone(),
                lease.clone(),
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
        runtime.shutdown().await.unwrap();
        assert_eq!(peer.active.load(Ordering::Relaxed), 0);
    }

    // Authority transition regressions retain an independent holder registration.
    // They exercise acknowledged provider outcomes and positively joined held
    // operations rather than inspecting the proposed epoch implementation.
    type HeldAuthorityCall = (
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    );
    struct AuthorityBacking {
        backing: Arc<Backing>,
        next: std::sync::atomic::AtomicU8,
        refuse: AtomicBool,
        held_prepare: Mutex<Option<HeldAuthorityCall>>,
        held_get: Mutex<Option<HeldAuthorityCall>>,
    }
    impl AuthorityBacking {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                backing: Backing::new(),
                next: std::sync::atomic::AtomicU8::new(1),
                refuse: AtomicBool::new(false),
                held_prepare: Mutex::new(None),
                held_get: Mutex::new(None),
            })
        }
    }
    #[async_trait]
    impl BlockStore for AuthorityBacking {
        fn durable(&self) -> bool {
            true
        }
        async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
            let held = self.held_prepare.lock().map_err(|_| error())?.take();
            if let Some((entered, release)) = held {
                let _ = entered.send(());
                release.await.map_err(|_| error())?;
            }
            ConcurrentBackingId::from_bytes([self.next.load(Ordering::Acquire); 16])
        }
        async fn verify_concurrent_backing(&self, _: ConcurrentBackingId) -> Result<()> {
            if self.refuse.load(Ordering::Acquire) {
                Err(error())
            } else {
                Ok(())
            }
        }
        async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
            self.backing.put(bytes).await
        }
        async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
            let held = self.held_get.lock().map_err(|_| error())?.take();
            if let Some((entered, release)) = held {
                let _ = entered.send(());
                release.await.map_err(|_| error())?;
            }
            self.backing.get(id).await
        }
        async fn flush(&self) -> Result<()> {
            self.backing.flush().await
        }
        async fn delete(&self, id: &BlockId) -> Result<()> {
            self.backing.delete(id).await
        }
    }
    fn authority_store(
        backing: Arc<AuthorityBacking>,
        cache: Arc<LocalCache>,
    ) -> Arc<CachedBlockStore> {
        Arc::new(CachedBlockStore::new(
            backing,
            cache,
            ScopeIdentity {
                cluster: "test".into(),
                partition: "p".into(),
                drive: "authority".into(),
            },
            IntegrityPolicy::Sha256Prefixed,
        ))
    }
    #[tokio::test]
    async fn observed_authority_failure_revokes_an_independent_holder_registration() {
        let backing = AuthorityBacking::new();
        let (_dir, cache, _) = fixture(Backing::new());
        let store = authority_store(backing.clone(), cache.clone());
        let scope = CacheScope {
            identity: store.identity.clone(),
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
        };
        let holder = cache
            .register_scope_lease(
                scope.clone(),
                IntegrityPolicy::Sha256Prefixed,
                cache.identity_epoch(&scope.identity).unwrap(),
            )
            .unwrap();
        backing.refuse.store(true, Ordering::Release);
        let refused = store.verify_concurrent_backing(scope.backing).await;
        let holder_policy = cache.scope_policy(&scope);
        drop(holder);
        drop(store);
        cache.shutdown().await.unwrap();
        assert!(
            refused.is_err(),
            "actual provider must refuse the selected authority"
        );
        assert_eq!(
            holder_policy, None,
            "observed provider refusal left independent holder admission live"
        );
    }
    #[tokio::test]
    async fn authority_refusal_before_held_success_does_not_resurrect_empty_admission() {
        let backing = AuthorityBacking::new();
        let (_dir, cache, _) = fixture(Backing::new());
        let old = authority_store(backing.clone(), cache.clone());
        let observer = authority_store(backing.clone(), cache.clone());
        let scope = CacheScope {
            identity: old.identity.clone(),
            backing: ConcurrentBackingId::from_bytes([1; 16]).unwrap(),
        };
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *backing.held_prepare.lock().unwrap() = Some((entered_tx, release_rx));
        let old_task = old.clone();
        let owned = tokio::spawn(async move { old_task.prepare_concurrent_backing().await });
        let entered = timeout(Duration::from_secs(1), entered_rx).await;
        backing.refuse.store(true, Ordering::Release);
        let refusal = observer.verify_concurrent_backing(scope.backing).await;
        let was_empty = cache.scope_policy(&scope).is_none();
        let _ = release_tx.send(());
        let joined = owned.await;
        let after = cache.scope_policy(&scope);
        backing.refuse.store(false, Ordering::Release);
        let fresh = observer.verify_concurrent_backing(scope.backing).await;
        let fresh_policy = cache.scope_policy(&scope);
        drop(old);
        drop(observer);
        cache.shutdown().await.unwrap();
        assert!(
            matches!(entered, Ok(Ok(()))),
            "actual provider prepare was not held"
        );
        assert!(refusal.is_err(), "actual provider refusal not observed");
        assert!(
            was_empty,
            "control needs an empty scope registry at revocation"
        );
        assert!(
            matches!(joined, Ok(Ok(_))),
            "actual held provider success did not join"
        );
        assert!(fresh.is_ok(), "fresh post-revocation provider proof failed");
        assert_eq!(
            fresh_policy,
            Some(IntegrityPolicy::Sha256Prefixed),
            "fresh proof after joined stale work was not admitted"
        );
        assert_eq!(
            after, None,
            "old successful await resurrected an authority already refused locally"
        );
    }
    #[tokio::test]
    async fn observed_different_backing_replaces_prior_holder_admission() {
        let backing = AuthorityBacking::new();
        let (_dir, cache, _) = fixture(Backing::new());
        let old = authority_store(backing.clone(), cache.clone());
        let next = authority_store(backing.clone(), cache.clone());
        let first = old.prepare_concurrent_backing().await.unwrap();
        let before = CacheScope {
            identity: old.identity.clone(),
            backing: first,
        };
        let holder = cache
            .current_scope_lease(&before, IntegrityPolicy::Sha256Prefixed)
            .unwrap()
            .unwrap();
        backing.next.store(2, Ordering::Release);
        let second = next.prepare_concurrent_backing().await.unwrap();
        let after = CacheScope {
            identity: next.identity.clone(),
            backing: second,
        };
        let old_policy = cache.scope_policy(&before);
        let next_policy = cache.scope_policy(&after);
        drop(holder);
        drop(old);
        drop(next);
        cache.shutdown().await.unwrap();
        assert_ne!(
            first, second,
            "actual provider did not acknowledge a backing transition"
        );
        assert_eq!(next_policy, Some(IntegrityPolicy::Sha256Prefixed));
        assert_eq!(
            old_policy, None,
            "old holder admission survived the observed backing transition"
        );
    }

    #[tokio::test]
    async fn held_old_epoch_cannot_deactivate_a_newer_lease_on_the_same_store() {
        let backing = AuthorityBacking::new();
        let (_dir, cache, _) = fixture(Backing::new());
        let store = authority_store(backing.clone(), cache.clone());
        let observer = authority_store(backing.clone(), cache.clone());
        let id = ConcurrentBackingId::from_bytes([1; 16]).unwrap();
        let scope = CacheScope {
            identity: store.identity.clone(),
            backing: id,
        };
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *backing.held_prepare.lock().unwrap() = Some((entered_tx, release_rx));
        let owned_store = store.clone();
        let owned = tokio::spawn(async move { owned_store.prepare_concurrent_backing().await });
        let entered = timeout(Duration::from_secs(1), entered_rx).await;
        backing.refuse.store(true, Ordering::Release);
        let refusal = observer.verify_concurrent_backing(id).await;
        backing.refuse.store(false, Ordering::Release);
        let fresh = store.verify_concurrent_backing(id).await;
        let before = store.current().is_some();
        let _ = release_tx.send(());
        let joined = owned.await;
        let after = store.current().is_some();
        let policy = cache.scope_policy(&scope);
        drop(store);
        drop(observer);
        cache.shutdown().await.unwrap();
        assert!(matches!(entered, Ok(Ok(()))));
        assert!(refusal.is_err());
        assert!(fresh.is_ok());
        assert!(
            matches!(joined, Ok(Ok(_))),
            "underlying successful prepare must remain successful"
        );
        assert!(
            before && after,
            "losing old CAS removed a newer current lease on this store"
        );
        assert_eq!(policy, Some(IntegrityPolicy::Sha256Prefixed));
    }
    #[tokio::test]
    async fn another_store_authority_refusal_bypasses_an_existing_warm_cache() {
        let backing = AuthorityBacking::new();
        let (_dir, cache, _) = fixture(Backing::new());
        let store = authority_store(backing.clone(), cache.clone());
        let observer = authority_store(backing.clone(), cache.clone());
        let scope_id = store.prepare_concurrent_backing().await.unwrap();
        let id = store.put(b"warm authority bytes").await.unwrap();
        store.flush().await.unwrap();
        assert_eq!(store.get(&id).await.unwrap(), b"warm authority bytes");
        assert_eq!(
            backing.backing.gets.load(Ordering::Relaxed),
            0,
            "warm control should hit cache"
        );
        backing.refuse.store(true, Ordering::Release);
        assert!(observer.verify_concurrent_backing(scope_id).await.is_err());
        let bytes = store.get(&id).await.unwrap();
        let reads = backing.backing.gets.load(Ordering::Relaxed);
        drop(store);
        drop(observer);
        cache.shutdown().await.unwrap();
        assert_eq!(bytes, b"warm authority bytes");
        assert_eq!(
            reads, 1,
            "revoked warm store used stale admission rather than backing"
        );
    }

    #[tokio::test]
    async fn held_read_does_not_fill_a_new_same_backing_authority_generation() {
        let backing = AuthorityBacking::new();
        let (_dir, cache, _) = fixture(Backing::new());
        let store = authority_store(backing.clone(), cache.clone());
        let observer = authority_store(backing.clone(), cache.clone());
        let authority = store.prepare_concurrent_backing().await.unwrap();
        let id = backing.put(b"provider owned held read").await.unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        *backing.held_get.lock().unwrap() = Some((entered_tx, release_rx));
        let reader = store.clone();
        let owned = tokio::spawn(async move { reader.get(&id).await });
        let entered = timeout(Duration::from_secs(1), entered_rx).await;
        backing.refuse.store(true, Ordering::Release);
        let refused = observer.verify_concurrent_backing(authority).await;
        backing.refuse.store(false, Ordering::Release);
        let fresh = observer.verify_concurrent_backing(authority).await;
        let fresh_current = observer.current().is_some();
        let _ = release_tx.send(());
        let joined = owned.await;
        let entries = cache.usage().2;
        drop(store);
        drop(observer);
        cache.shutdown().await.unwrap();
        assert!(
            matches!(entered, Ok(Ok(()))),
            "actual backing read was not held"
        );
        assert!(refused.is_err());
        assert!(fresh.is_ok() && fresh_current);
        assert!(matches!(joined, Ok(Ok(ref bytes)) if bytes == b"provider owned held read"));
        assert_eq!(
            entries, 0,
            "old generation read populated a fresh generation after revoke/restore"
        );
    }

    #[tokio::test]
    async fn repeated_same_backing_verification_reuses_the_existing_registration_group() {
        let backing = AuthorityBacking::new();
        let (_dir, cache, _) = fixture(Backing::new());
        let store = authority_store(backing, cache.clone());
        let id = store.prepare_concurrent_backing().await.unwrap();
        let before = store.scope.lock().unwrap().as_ref().unwrap().2.clone();
        store.verify_concurrent_backing(id).await.unwrap();
        let after = store.scope.lock().unwrap().as_ref().unwrap().2.clone();
        // This checks the exact cache-registration allocation, rather than
        // asserting that provider async futures or the entire operation allocate nothing.
        assert!(before.same_group(&after));
        drop(before);
        drop(after);
        drop(store);
        cache.shutdown().await.unwrap();
    }
}
