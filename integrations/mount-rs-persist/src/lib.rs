//! Shared persistence adapter used by the SQLite, R2, and PGlite crates.
//!
//! The core crate remains backend-free. Each integration supplies a small
//! `StateStore` implementation, while this crate keeps the filesystem model
//! and all driver operations identical across those stores.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, FsError, MemoryFs, MkdirOptions,
    OpenFlags, Result, Stats, StatsFs,
};

/// A snapshot and the backend version that was observed with it.
///
/// The version is intentionally opaque. SQLite and PGlite use a decimal
/// revision, while object stores use their conditional-write version (usually
/// an ETag). An empty version means that the store does not provide optimistic
/// concurrency control.
#[derive(Debug, Default)]
pub struct LoadedSnapshot {
    pub snapshot: Option<Vec<u8>>,
    pub version: String,
}

/// A durable byte store for one serialized filesystem snapshot.
#[async_trait]
pub trait StateStore: Send + Sync {
    async fn load(&self) -> Result<Option<Vec<u8>>>;

    /// Load the snapshot together with an opaque version token.
    ///
    /// Existing stores that only implement `load` continue to work, but their
    /// version token is empty and concurrent independent filesystem instances
    /// cannot be protected from a stale whole-snapshot write. Stores with a
    /// native revision or conditional-write primitive should override this
    /// method and `save_versioned` together.
    async fn load_versioned(&self) -> Result<LoadedSnapshot> {
        Ok(LoadedSnapshot {
            snapshot: self.load().await?,
            version: String::new(),
        })
    }

    /// Atomically replace the snapshot and do not resolve successfully until
    /// the replacement is durable in the backend. On error, the previously
    /// committed snapshot must remain readable so a later `persist` can retry.
    async fn save(&self, snapshot: Vec<u8>) -> Result<()>;

    /// Save a snapshot only if `expected_version` is still current, returning
    /// the version of the committed snapshot. The default preserves the
    /// original `StateStore` contract for stores without version support.
    async fn save_versioned(&self, snapshot: Vec<u8>, expected_version: &str) -> Result<String> {
        self.save(snapshot).await?;
        Ok(expected_version.to_owned())
    }
}

/// Return the retryable error used when another filesystem instance committed
/// a newer snapshot first. The stale instance must be reopened or explicitly
/// reconciled before it can write again.
pub fn snapshot_conflict(backend: &str) -> FsError {
    FsError::new(ErrorCode::Eagain).with_message(format!(
        "{backend} snapshot changed concurrently; reopen before retrying"
    ))
}

/// A small runtime-neutral async mutex used to serialize snapshot creation and
/// backend writes.
///
/// `PersistedFs` clones share the same `MemoryFs`. If a backend write is
/// delayed, taking a snapshot before the write has completed lets a newer
/// operation put its snapshot first and then lets the older write overwrite
/// it. Holding this gate across both serialization and `StateStore::save`
/// preserves operation order without requiring every integration to depend on
/// a particular async runtime.
#[derive(Clone)]
struct SaveGate {
    state: Arc<Mutex<SaveGateState>>,
}

struct SaveGateState {
    held: bool,
    waiters: Vec<Waker>,
}

struct SaveGateFuture {
    state: Arc<Mutex<SaveGateState>>,
}

struct SaveGateGuard {
    state: Arc<Mutex<SaveGateState>>,
}

impl SaveGate {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SaveGateState {
                held: false,
                waiters: Vec::new(),
            })),
        }
    }

    async fn lock(&self) -> SaveGateGuard {
        SaveGateFuture {
            state: Arc::clone(&self.state),
        }
        .await
    }
}

impl Future for SaveGateFuture {
    type Output = SaveGateGuard;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.held {
            state.held = true;
            return Poll::Ready(SaveGateGuard {
                state: Arc::clone(&self.state),
            });
        }

        // A future can be polled more than once with a new waker. Keep only
        // the current registration so a cancelled waiter cannot accumulate an
        // unbounded list of stale wakers.
        state
            .waiters
            .retain(|waker| !waker.will_wake(context.waker()));
        state.waiters.push(context.waker().clone());
        Poll::Pending
    }
}

impl Drop for SaveGateGuard {
    fn drop(&mut self) {
        let waiter = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.held = false;
            state.waiters.pop()
        };
        if let Some(waker) = waiter {
            waker.wake();
        }
    }
}

/// A filesystem whose behavior is supplied by the core memfs and whose state
/// is saved by an integration-specific store after each mutation.
pub struct PersistedFs<S: StateStore> {
    core: MemoryFs,
    store: Arc<S>,
    save_gate: SaveGate,
    snapshot_version: Arc<Mutex<String>>,
}

impl<S: StateStore> Clone for PersistedFs<S> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            store: Arc::clone(&self.store),
            save_gate: self.save_gate.clone(),
            snapshot_version: Arc::clone(&self.snapshot_version),
        }
    }
}

impl<S: StateStore> PersistedFs<S> {
    pub async fn open(store: S) -> Result<Self> {
        let store = Arc::new(store);
        let loaded = store.load_versioned().await?;
        let has_snapshot = loaded.snapshot.is_some();
        let core = match loaded.snapshot.as_deref() {
            Some(snapshot) => MemoryFs::from_snapshot(snapshot)?,
            None => MemoryFs::empty(),
        };
        let filesystem = Self {
            core,
            store,
            save_gate: SaveGate::new(),
            snapshot_version: Arc::new(Mutex::new(loaded.version)),
        };
        if !has_snapshot {
            filesystem.persist().await?;
        }
        Ok(filesystem)
    }

    pub fn core(&self) -> &MemoryFs {
        &self.core
    }

    pub async fn persist(&self) -> Result<()> {
        let _guard = self.save_gate.lock().await;
        let snapshot = self.core.snapshot_bytes()?;
        let expected_version = self
            .snapshot_version
            .lock()
            .map_err(|_| {
                FsError::new(ErrorCode::Eio).with_message("snapshot version lock poisoned")
            })?
            .clone();
        let next_version = self
            .store
            .save_versioned(snapshot, &expected_version)
            .await?;
        *self.snapshot_version.lock().map_err(|_| {
            FsError::new(ErrorCode::Eio).with_message("snapshot version lock poisoned")
        })? = next_version;
        Ok(())
    }

    fn open_needs_persist(flags: OpenFlags) -> bool {
        flags.create || flags.truncate
    }
}

struct PersistedHandle<S: StateStore> {
    inner: Arc<dyn FileHandle>,
    filesystem: PersistedFs<S>,
}

#[async_trait]
impl<S: StateStore + 'static> FileHandle for PersistedHandle<S> {
    fn fd(&self) -> Option<u64> {
        self.inner.fd()
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        self.inner.read(buffer, position).await
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let count = self.inner.write(buffer, position).await?;
        self.filesystem.persist().await?;
        Ok(count)
    }

    async fn stat(&self) -> Result<Stats> {
        self.inner.stat().await
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        self.inner.truncate(length).await?;
        self.filesystem.persist().await
    }

    async fn sync(&self) -> Result<()> {
        self.inner.sync().await?;
        self.filesystem.persist().await
    }

    async fn datasync(&self) -> Result<()> {
        self.inner.datasync().await?;
        self.filesystem.persist().await
    }

    async fn close(&self) -> Result<()> {
        self.inner.close().await?;
        self.filesystem.persist().await
    }
}

#[async_trait]
impl<S: StateStore + 'static> FsDriver for PersistedFs<S> {
    fn capabilities(&self) -> Capabilities {
        let mut capabilities = self.core.capabilities();
        // Every mutating handle operation awaits the corresponding snapshot
        // save before resolving. The persistence adapter therefore satisfies
        // the upstream durable-write claim even if the in-memory core keeps
        // that claim conservative for standalone use.
        capabilities.durable_writes = true;
        capabilities
    }

    async fn syncfs(&self) -> Result<()> {
        self.persist().await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.core.stat(path).await
    }
    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.core.lstat(path).await
    }
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.core.readdir(path).await
    }
    async fn readdir_bounded(&self, path: &str, max_entries: usize) -> Result<Vec<DirEntry>> {
        self.core.readdir_bounded(path, max_entries).await
    }
    async fn statfs(&self, path: &str) -> Result<StatsFs> {
        self.core.statfs(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        let parsed = OpenFlags::parse(flags, path)?;
        let handle = self.core.open(path, flags, mode).await?;
        if Self::open_needs_persist(parsed) {
            self.persist().await?;
        }
        Ok(Arc::new(PersistedHandle {
            inner: handle,
            filesystem: self.clone(),
        }))
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: mount_rs_core::OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let handle = self.core.open_flags(path, flags, mode).await?;
        if Self::open_needs_persist(flags) {
            self.persist().await?;
        }
        Ok(Arc::new(PersistedHandle {
            inner: handle,
            filesystem: self.clone(),
        }))
    }

    async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        self.core.write_file(path, data).await?;
        self.persist().await
    }

    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        let first_created = self.core.mkdir(path, options).await?;
        self.persist().await?;
        Ok(first_created)
    }
    async fn rmdir(&self, path: &str) -> Result<()> {
        self.core.rmdir(path).await?;
        self.persist().await
    }
    async fn unlink(&self, path: &str) -> Result<()> {
        self.core.unlink(path).await?;
        self.persist().await
    }
    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        self.core.rename(old_path, new_path).await?;
        self.persist().await
    }
    async fn link(&self, old_path: &str, new_path: &str) -> Result<()> {
        self.core.link(old_path, new_path).await?;
        self.persist().await
    }
    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        self.core.symlink(target, path).await?;
        self.persist().await
    }
    async fn readlink(&self, path: &str) -> Result<String> {
        self.core.readlink(path).await
    }
    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        self.core.chmod(path, mode).await?;
        self.persist().await
    }
    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.core.chown(path, uid, gid).await?;
        self.persist().await
    }
    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.core.lchown(path, uid, gid).await?;
        self.persist().await
    }
    async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        self.core.truncate(path, length).await?;
        self.persist().await
    }
    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.core.utimes(path, atime_ms, mtime_ms).await?;
        self.persist().await
    }
    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.core.lutimes(path, atime_ms, mtime_ms).await?;
        self.persist().await
    }
    async fn mknod(&self, path: &str, mode: u32, dev: u64) -> Result<()> {
        self.core.mknod(path, mode, dev).await?;
        self.persist().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::{FsDriver, MemoryFs, OpenFlags};
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut context = Context::from_waker(waker);
        let mut future = Box::pin(future);
        loop {
            match Future::poll(future.as_mut(), &mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => thread::yield_now(),
            }
        }
    }

    #[derive(Clone)]
    struct TestStore {
        snapshot: Arc<Mutex<Option<Vec<u8>>>>,
        fail_next: Arc<AtomicBool>,
        save_calls: Arc<AtomicUsize>,
        delay: Arc<(Mutex<DelayState>, std::sync::Condvar)>,
    }

    struct DelayState {
        next: bool,
        started: bool,
        released: bool,
    }

    impl TestStore {
        fn new() -> Self {
            Self {
                snapshot: Arc::new(Mutex::new(None)),
                fail_next: Arc::new(AtomicBool::new(false)),
                save_calls: Arc::new(AtomicUsize::new(0)),
                delay: Arc::new((
                    Mutex::new(DelayState {
                        next: false,
                        started: false,
                        released: false,
                    }),
                    std::sync::Condvar::new(),
                )),
            }
        }

        fn fail_next_save(&self) {
            self.fail_next.store(true, Ordering::SeqCst);
        }

        fn save_calls(&self) -> usize {
            self.save_calls.load(Ordering::SeqCst)
        }

        fn delay_next_save(&self) {
            let (state, _) = &*self.delay;
            let mut state = state.lock().unwrap();
            state.next = true;
            state.started = false;
            state.released = false;
        }

        fn wait_for_delayed_save(&self) {
            let (state, condition) = &*self.delay;
            let mut state = state.lock().unwrap();
            while !state.started {
                state = condition.wait(state).unwrap();
            }
        }

        fn release_delayed_save(&self) {
            let (state, condition) = &*self.delay;
            let mut state = state.lock().unwrap();
            state.released = true;
            condition.notify_all();
        }

        fn snapshot(&self) -> Vec<u8> {
            self.snapshot.lock().unwrap().clone().unwrap()
        }
    }

    #[async_trait]
    impl StateStore for TestStore {
        async fn load(&self) -> Result<Option<Vec<u8>>> {
            Ok(self.snapshot.lock().unwrap().clone())
        }

        async fn save(&self, snapshot: Vec<u8>) -> Result<()> {
            self.save_calls.fetch_add(1, Ordering::SeqCst);
            let should_delay = {
                let (state, condition) = &*self.delay;
                let mut state = state.lock().unwrap();
                if !state.next {
                    false
                } else {
                    state.next = false;
                    state.started = true;
                    condition.notify_all();
                    while !state.released {
                        state = condition.wait(state).unwrap();
                    }
                    true
                }
            };
            let _ = should_delay;
            if self.fail_next.swap(false, Ordering::SeqCst) {
                return Err(mount_rs_core::FsError::backend("injected save failure"));
            }
            *self.snapshot.lock().unwrap() = Some(snapshot);
            Ok(())
        }
    }

    fn read_file(fs: &MemoryFs, path: &str) -> Vec<u8> {
        let handle = block_on(fs.open(path, "r", 0)).unwrap();
        let mut bytes = Vec::new();
        let mut position = 0;
        loop {
            let mut buffer = [0_u8; 64];
            let count = block_on(handle.read(&mut buffer, Some(position))).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            position += count as u64;
        }
        bytes
    }

    #[test]
    fn failed_save_is_reported_and_can_be_retried() {
        let store = TestStore::new();
        let fs = block_on(PersistedFs::open(store.clone())).unwrap();
        let handle = block_on(fs.open("/file", "w", 0o666)).unwrap();
        block_on(handle.write(b"old", Some(0))).unwrap();

        store.fail_next_save();
        assert!(block_on(handle.write(b"new", Some(0))).is_err());
        let before_retry = MemoryFs::from_snapshot(&store.snapshot()).unwrap();
        assert_eq!(read_file(&before_retry, "/file"), b"old");

        block_on(handle.sync()).unwrap();
        let after_retry = MemoryFs::from_snapshot(&store.snapshot()).unwrap();
        assert_eq!(read_file(&after_retry, "/file"), b"new");
    }

    #[test]
    fn syncfs_is_a_durable_barrier_and_propagates_store_failure() {
        let store = TestStore::new();
        let fs = block_on(PersistedFs::open(store.clone())).unwrap();
        let before = store.save_calls();

        store.fail_next_save();
        let error = block_on(fs.syncfs()).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        assert_eq!(store.save_calls(), before + 1);

        block_on(fs.syncfs()).unwrap();
        assert_eq!(store.save_calls(), before + 2);
    }

    #[test]
    fn serialized_saves_cannot_restore_an_older_snapshot() {
        let store = TestStore::new();
        let fs = Arc::new(block_on(PersistedFs::open(store.clone())).unwrap());
        let handle = block_on(fs.open("/file", "w", 0o666)).unwrap();
        block_on(handle.write(b"base", Some(0))).unwrap();
        let handle = block_on(fs.open("/file", "r+", 0)).unwrap();

        store.delay_next_save();
        let first = thread::spawn({
            let handle = Arc::clone(&handle);
            move || block_on(handle.write(b"old!", Some(0)))
        });
        store.wait_for_delayed_save();

        // Mutate the shared core while the first snapshot is still in flight,
        // then queue a newer save behind the gate.
        let direct = block_on(fs.core().open("/file", "r+", 0)).unwrap();
        block_on(direct.write(b"new!", Some(0))).unwrap();
        let second = thread::spawn({
            let fs = Arc::clone(&fs);
            move || block_on(fs.persist())
        });
        thread::sleep(Duration::from_millis(1));
        store.release_delayed_save();

        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();

        let restored = MemoryFs::from_snapshot(&store.snapshot()).unwrap();
        assert_eq!(read_file(&restored, "/file"), b"new!");
    }

    #[test]
    fn open_flags_forwards_decoded_flags_and_persists_creation() {
        let store = TestStore::new();
        let fs = block_on(PersistedFs::open(store.clone())).unwrap();
        assert!(fs.capabilities().durable_writes);
        let flags = OpenFlags {
            read: true,
            write: true,
            create: true,
            truncate: false,
            append: false,
            exclusive: false,
        };
        let handle = block_on(fs.open_flags("/decoded", flags, 0o640)).unwrap();
        block_on(handle.write(b"ok", Some(0))).unwrap();
        let restored = MemoryFs::from_snapshot(&store.snapshot()).unwrap();
        assert_eq!(read_file(&restored, "/decoded"), b"ok");
        assert_eq!(block_on(handle.stat()).unwrap().mode & 0o777, 0o640);
    }
}
