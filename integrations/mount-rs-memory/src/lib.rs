//! Volatile, independent metadata and immutable block stores for mount-rs.
//!
//! The two stores deliberately have separate state and handles. Metadata keeps
//! namespace records and block references only; file bytes are owned by
//! [`MemoryBlockStore`]. A metadata publication is guarded by a volume-wide
//! writer lease with a monotonically increasing fencing token. The store is
//! volatile, so `durable()` is always false and `flush()` is only a successful
//! no-op barrier.

use async_trait::async_trait;
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{ErrorCode, FsError, Result};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Provider-owned time source used by the metadata lease implementation.
///
/// Lease callers never supply a wall-clock timestamp. Tests can inject a
/// [`ManualClock`] so expiry and fencing behavior are deterministic.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Wall-clock implementation used by [`MemoryMetadataStore::new`].
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(duration) => u64::try_from(duration.as_millis()).unwrap_or(u64::MAX),
            Err(_) => 0,
        }
    }
}

/// Injectable monotonic test clock.
#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    value_ms: Arc<AtomicU64>,
}

impl ManualClock {
    pub fn new(now_ms: u64) -> Self {
        Self {
            value_ms: Arc::new(AtomicU64::new(now_ms)),
        }
    }

    pub fn set_ms(&self, now_ms: u64) {
        self.value_ms.store(now_ms, Ordering::SeqCst);
    }

    /// Advance the clock, returning false instead of wrapping on overflow.
    pub fn advance_ms(&self, amount_ms: u64) -> bool {
        self.value_ms
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |now| {
                now.checked_add(amount_ms)
            })
            .is_ok()
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.value_ms.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone)]
struct LeaseState {
    owner: String,
    fence: u64,
    expires_at_ms: u64,
}

#[derive(Default)]
struct MetadataState {
    revision: u64,
    namespace: Option<Namespace>,
    last_fence: u64,
    lease: Option<LeaseState>,
}

/// Volatile metadata store with a linearizable in-process lease/revision CAS.
///
/// Cloning this value creates another handle to the same store. The clock is
/// shared by the handles, while block storage remains entirely separate.
#[derive(Clone)]
pub struct MemoryMetadataStore {
    state: Arc<Mutex<MetadataState>>,
    clock: Arc<dyn Clock>,
}

impl fmt::Debug for MemoryMetadataStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryMetadataStore")
            .field("durable", &false)
            .finish_non_exhaustive()
    }
}

impl Default for MemoryMetadataStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryMetadataStore {
    pub fn new() -> Self {
        Self::with_clock(Arc::new(SystemClock))
    }

    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self {
            state: Arc::new(Mutex::new(MetadataState::default())),
            clock,
        }
    }

    fn lock_state(&self) -> Result<MutexGuard<'_, MetadataState>> {
        self.state
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("metadata store lock poisoned"))
    }
}

#[async_trait]
impl MetadataStore for MemoryMetadataStore {
    fn durable(&self) -> bool {
        false
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let state = self.lock_state()?;
        Ok(LoadedMetadata {
            revision: state.revision,
            namespace: state.namespace.clone(),
        })
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        if owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("writer lease owner must not be empty"));
        }
        let ttl_ms = duration_ms(ttl)?;
        let mut state = self.lock_state()?;
        let now_ms = self.clock.now_ms();
        let expires_at_ms = expiry(now_ms, ttl_ms)?;

        if let Some(current) = &state.lease
            && now_ms < current.expires_at_ms
        {
            return Err(
                FsError::new(ErrorCode::Eagain).with_message("a writer lease is already active")
            );
        }

        let fence = state
            .last_fence
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let lease = LeaseState {
            owner: owner.to_owned(),
            fence,
            expires_at_ms,
        };
        state.last_fence = fence;
        state.lease = Some(lease.clone());
        Ok(to_public_lease(lease))
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        let ttl_ms = duration_ms(ttl)?;
        let mut state = self.lock_state()?;
        let now_ms = self.clock.now_ms();
        let expires_at_ms = expiry(now_ms, ttl_ms)?;
        validate_lease(&state, lease, now_ms)?;

        let renewed = LeaseState {
            owner: lease.owner.clone(),
            fence: lease.fence,
            expires_at_ms,
        };
        state.lease = Some(renewed.clone());
        Ok(to_public_lease(renewed))
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        let mut state = self.lock_state()?;
        let now_ms = self.clock.now_ms();
        validate_lease(&state, lease, now_ms)?;
        state.lease = None;
        Ok(())
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        let mut state = self.lock_state()?;
        let now_ms = self.clock.now_ms();
        validate_lease(&state, lease, now_ms)?;
        if state.revision != expected_revision {
            return Err(FsError::new(ErrorCode::Eagain).with_message(format!(
                "metadata revision conflict: expected {expected_revision}, actual {}",
                state.revision
            )));
        }

        let revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        state.namespace = Some(namespace);
        state.revision = revision;
        Ok(revision)
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }
}

/// Volatile immutable block store. A clone shares the same block map.
#[derive(Debug, Clone, Default)]
pub struct MemoryBlockStore {
    blocks: Arc<Mutex<BTreeMap<BlockId, Vec<u8>>>>,
}

impl MemoryBlockStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_blocks(&self) -> Result<MutexGuard<'_, BTreeMap<BlockId, Vec<u8>>>> {
        self.blocks
            .lock()
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("block store lock poisoned"))
    }
}

#[async_trait]
impl BlockStore for MemoryBlockStore {
    fn durable(&self) -> bool {
        false
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let base_id = block_id(bytes);
        let mut id = base_id.clone();
        let mut collision = 0_u64;
        let mut blocks = self.lock_blocks()?;
        loop {
            match blocks.get(&id) {
                Some(existing) if existing == bytes => return Ok(id),
                Some(_) => {
                    collision = collision
                        .checked_add(1)
                        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                    id = BlockId(format!("{}-{collision}", base_id.0));
                }
                None => {
                    blocks.insert(id.clone(), bytes.to_vec());
                    return Ok(id);
                }
            }
        }
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        let blocks = self.lock_blocks()?;
        blocks
            .get(id)
            .cloned()
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_message("block not found"))
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        let mut blocks = self.lock_blocks()?;
        if blocks.remove(id).is_some() {
            Ok(())
        } else {
            Err(FsError::new(ErrorCode::Enoent).with_message("block not found"))
        }
    }
}

fn duration_ms(ttl: Duration) -> Result<u64> {
    let ttl_ms = u64::try_from(ttl.as_millis())
        .map_err(|_| FsError::new(ErrorCode::Eoverflow).with_message("lease TTL overflows ms"))?;
    if ttl_ms == 0 {
        return Err(FsError::new(ErrorCode::Einval).with_message("lease TTL must be positive"));
    }
    Ok(ttl_ms)
}

fn expiry(now_ms: u64, ttl_ms: u64) -> Result<u64> {
    now_ms
        .checked_add(ttl_ms)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow).with_message("lease expiry overflows ms"))
}

fn validate_lease(state: &MetadataState, lease: &WriterLease, now_ms: u64) -> Result<()> {
    let Some(current) = &state.lease else {
        return Err(stale_lease("no active writer lease"));
    };
    if now_ms >= current.expires_at_ms {
        return Err(stale_lease("writer lease expired"));
    }
    if current.owner != lease.owner
        || current.fence != lease.fence
        || current.expires_at_ms != lease.expires_at_ms
    {
        return Err(stale_lease("writer lease fencing token is stale"));
    }
    Ok(())
}

fn stale_lease(message: &'static str) -> FsError {
    FsError::new(ErrorCode::Estale).with_message(message)
}

fn to_public_lease(lease: LeaseState) -> WriterLease {
    WriterLease {
        owner: lease.owner,
        fence: lease.fence,
        expires_at_ms: lease.expires_at_ms,
    }
}

// A dependency-free deterministic content identity. This is not advertised as
// a cryptographic digest; the collision branch in put preserves immutable
// identity even if a future implementation changes this function.
fn block_id(bytes: &[u8]) -> BlockId {
    const OFFSETS: [u64; 4] = [
        0xcbf29ce484222325,
        0x84222325cbf29ce4,
        0x9e3779b185ebca87,
        0xd6e8feb86659fd93,
    ];
    const PRIMES: [u64; 4] = [0x100000001b3, 0x100000001b3, 0x100000001b3, 0x100000001b3];
    let mut lanes = OFFSETS;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let index = u64::try_from(index).unwrap_or(u64::MAX);
        for (lane, value) in lanes.iter_mut().enumerate() {
            let lane_value = u64::try_from(lane).unwrap_or(u64::MAX);
            *value ^= u64::from(byte)
                .wrapping_add(index.wrapping_mul(0x9e37_79b9_7f4a_7c15 ^ lane_value));
            *value = value
                .wrapping_mul(PRIMES[lane])
                .rotate_left((lane as u32).wrapping_mul(11).wrapping_add(5));
        }
    }
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    for (lane, value) in lanes.iter_mut().enumerate() {
        let lane_value = u64::try_from(lane).unwrap_or(u64::MAX);
        *value ^= length.wrapping_add(lane_value.wrapping_mul(0x517c_c1b7_2722_0a95));
        *value = value
            .wrapping_mul(PRIMES[lane])
            .rotate_left((lane as u32).wrapping_mul(13).wrapping_add(7));
    }
    BlockId(format!(
        "mem-{length:016x}-{:016x}{:016x}{:016x}{:016x}",
        lanes[0], lanes[1], lanes[2], lanes[3]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
    use mount_rs_core::storage::{BlockExtent, FileLayout, NodeData, NodeMetadata};
    use mount_rs_core::types::{S_IFDIR, S_IFREG, Stats};
    use std::future::Future;
    use std::task::{Context, Poll, Wake, Waker};

    struct NoopWaker;

    impl Wake for NoopWaker {
        fn wake(self: Arc<Self>) {}

        fn wake_by_ref(self: &Arc<Self>) {}
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(NoopWaker));
        let mut context = Context::from_waker(&waker);
        let mut future = Box::pin(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => std::thread::yield_now(),
            }
        }
    }

    fn sample_namespace(marker: u64) -> Namespace {
        let mut nodes = BTreeMap::new();
        nodes.insert(
            1,
            NodeMetadata {
                stats: Stats {
                    dev: 1,
                    ino: 1,
                    mode: S_IFDIR | 0o755,
                    nlink: 2,
                    uid: 1000,
                    gid: 1000,
                    rdev: 0,
                    size: marker,
                    blksize: 4096,
                    blocks: 1,
                    atime_ms: 1,
                    mtime_ms: 1,
                    ctime_ms: 1,
                    birthtime_ms: 1,
                },
                data: NodeData::Directory {
                    entries: Vec::new(),
                },
            },
        );
        Namespace {
            format_version: 1,
            root: 1,
            next_inode: 2,
            default_uid: 1000,
            default_gid: 1000,
            umask: 0o022,
            default_chunker: FixedSizeChunker::new(4096).unwrap().config(),
            nodes,
        }
    }

    fn assert_error<T>(result: Result<T>, code: ErrorCode) {
        match result {
            Ok(_) => panic!("expected {code:?}"),
            Err(error) => assert_eq!(error.code, code),
        }
    }

    #[test]
    fn lease_fence_changes_after_expiry_and_rejects_stale_owner() {
        let clock = Arc::new(ManualClock::new(1_000));
        let store = MemoryMetadataStore::with_clock(clock.clone());
        let alice = block_on(store.acquire_writer("alice", Duration::from_secs(1))).unwrap();
        assert_eq!(alice.fence, 1);
        assert_error(
            block_on(store.acquire_writer("bob", Duration::from_millis(999))),
            ErrorCode::Eagain,
        );

        assert!(clock.advance_ms(1_000));
        let bob = block_on(store.acquire_writer("bob", Duration::from_secs(1))).unwrap();
        assert_eq!(bob.fence, 2);
        assert_error(
            block_on(store.renew_writer(&alice, Duration::from_secs(1))),
            ErrorCode::Estale,
        );
        assert_error(block_on(store.release_writer(&alice)), ErrorCode::Estale);
        assert_error(
            block_on(store.publish(0, &alice, sample_namespace(1))),
            ErrorCode::Estale,
        );
        assert!(!store.durable());
        assert!(!MemoryBlockStore::new().durable());
    }

    #[test]
    fn renewal_is_provider_timed_and_expiry_is_part_of_the_token() {
        let clock = Arc::new(ManualClock::new(50));
        let store = MemoryMetadataStore::with_clock(clock.clone());
        let lease = block_on(store.acquire_writer("owner", Duration::from_millis(100))).unwrap();
        assert_eq!(lease.expires_at_ms, 150);
        assert!(clock.advance_ms(25));
        let renewed = block_on(store.renew_writer(&lease, Duration::from_millis(100))).unwrap();
        assert_eq!(renewed.fence, lease.fence);
        assert_eq!(renewed.expires_at_ms, 175);
        assert_error(
            block_on(store.publish(0, &lease, sample_namespace(1))),
            ErrorCode::Estale,
        );
        assert!(clock.advance_ms(100));
        assert_error(
            block_on(store.renew_writer(&renewed, Duration::from_millis(1))),
            ErrorCode::Estale,
        );
    }

    #[test]
    fn publication_is_revision_cas_and_load_returns_isolated_metadata() {
        let clock = Arc::new(ManualClock::new(0));
        let store = MemoryMetadataStore::with_clock(clock);
        let lease = block_on(store.acquire_writer("owner", Duration::from_secs(10))).unwrap();
        assert_eq!(
            block_on(store.publish(0, &lease, sample_namespace(7))).unwrap(),
            1
        );
        assert_error(
            block_on(store.publish(0, &lease, sample_namespace(8))),
            ErrorCode::Eagain,
        );
        let mut loaded = block_on(store.load()).unwrap();
        let namespace = loaded.namespace.as_mut().unwrap();
        namespace.nodes.get_mut(&1).unwrap().stats.size = 99;
        let loaded_again = block_on(store.load()).unwrap();
        assert_eq!(loaded_again.revision, 1);
        assert_eq!(loaded_again.namespace.unwrap().nodes[&1].stats.size, 7);
    }

    #[test]
    fn metadata_contains_extents_not_file_bytes_and_blocks_are_immutable() {
        let blocks = MemoryBlockStore::new();
        let id = block_on(blocks.put(b"hello")).unwrap();
        assert_eq!(block_on(blocks.put(b"hello")).unwrap(), id);
        let other = block_on(blocks.put(b"world")).unwrap();
        assert_ne!(id, other);
        let mut read = block_on(blocks.get(&id)).unwrap();
        read[0] = b'X';
        assert_eq!(block_on(blocks.get(&id)).unwrap(), b"hello");

        let mut namespace = sample_namespace(0);
        namespace.nodes.insert(
            2,
            NodeMetadata {
                stats: Stats {
                    dev: 1,
                    ino: 2,
                    mode: S_IFREG | 0o644,
                    nlink: 1,
                    uid: 1000,
                    gid: 1000,
                    rdev: 0,
                    size: 5,
                    blksize: 4096,
                    blocks: 1,
                    atime_ms: 1,
                    mtime_ms: 1,
                    ctime_ms: 1,
                    birthtime_ms: 1,
                },
                data: NodeData::File(FileLayout {
                    chunker: FixedSizeChunker::new(4096).unwrap().config(),
                    extents: vec![BlockExtent {
                        file_offset: 0,
                        block: id.clone(),
                        block_offset: 0,
                        length: 5,
                    }],
                }),
            },
        );
        let metadata = MemoryMetadataStore::with_clock(Arc::new(ManualClock::new(0)));
        let lease = block_on(metadata.acquire_writer("owner", Duration::from_secs(1))).unwrap();
        block_on(metadata.publish(0, &lease, namespace)).unwrap();
        let loaded = block_on(metadata.load()).unwrap().namespace.unwrap();
        match &loaded.nodes[&2].data {
            NodeData::File(layout) => {
                assert_eq!(layout.extents[0].block, id);
                assert_eq!(layout.extents[0].length, 5);
            }
            _ => panic!("expected file layout"),
        }

        block_on(blocks.delete(&other)).unwrap();
        assert_error(block_on(blocks.get(&other)), ErrorCode::Enoent);
        assert_error(block_on(blocks.delete(&other)), ErrorCode::Enoent);
    }

    #[test]
    fn rejects_zero_ttl_and_expiry_overflow_without_mutating_lease_state() {
        let clock = Arc::new(ManualClock::new(u64::MAX - 1));
        let store = MemoryMetadataStore::with_clock(clock);
        assert_error(
            block_on(store.acquire_writer("owner", Duration::from_nanos(1))),
            ErrorCode::Einval,
        );
        assert_error(
            block_on(store.acquire_writer("owner", Duration::from_millis(2))),
            ErrorCode::Eoverflow,
        );
        let lease = block_on(store.acquire_writer("owner", Duration::from_millis(1))).unwrap();
        assert_eq!(lease.fence, 1);
        assert_eq!(lease.expires_at_ms, u64::MAX);
    }
}
