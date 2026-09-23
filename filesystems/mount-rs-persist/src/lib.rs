//! Shared persistence adapter used by the SQLite, R2, and PGlite crates.
//!
//! The core crate remains backend-free. Each integration supplies a small
//! `StateStore` implementation, while this crate keeps the filesystem model
//! and all driver operations identical across those stores.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, FsError, MkdirOptions, OpenFlags,
    Result, Stats, StatsFs,
};
pub use mount_rs_core::{LoadedSnapshot, StateStore, snapshot_conflict};
use mount_rs_memfs::MemoryFs;

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
    waiters: VecDeque<SaveGateWaiter>,
}

struct SaveGateWaiter {
    registration: Arc<()>,
    waker: Waker,
}

struct SaveGateFuture {
    state: Arc<Mutex<SaveGateState>>,
    registration: Option<Arc<()>>,
}

struct SaveGateGuard {
    state: Arc<Mutex<SaveGateState>>,
}

impl SaveGate {
    fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SaveGateState {
                held: false,
                waiters: VecDeque::new(),
            })),
        }
    }

    async fn lock(&self) -> SaveGateGuard {
        SaveGateFuture {
            state: Arc::clone(&self.state),
            registration: None,
        }
        .await
    }
}

impl SaveGateState {
    // Keeping the selected waiter in the queue lets its cancellation hand an
    // unlocked gate to another live waiter.
    fn remove_waiter(&mut self, registration: &Arc<()>) -> bool {
        let Some(index) = self
            .waiters
            .iter()
            .position(|waiter| Arc::ptr_eq(&waiter.registration, registration))
        else {
            return false;
        };
        self.waiters.remove(index);
        true
    }
}

impl Future for SaveGateFuture {
    type Output = SaveGateGuard;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let future = self.get_mut();
        let mut state = future
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !state.held {
            state.held = true;
            if let Some(registration) = future.registration.take() {
                state.remove_waiter(&registration);
            }
            return Poll::Ready(SaveGateGuard {
                state: Arc::clone(&future.state),
            });
        }

        // Re-polling one future updates its registration, including after a
        // wakeup if another future acquired the gate first.
        let registration = future.registration.get_or_insert_with(|| Arc::new(()));
        if let Some(waiter) = state
            .waiters
            .iter_mut()
            .find(|waiter| Arc::ptr_eq(&waiter.registration, registration))
        {
            if !waiter.waker.will_wake(context.waker()) {
                waiter.waker = context.waker().clone();
            }
        } else {
            state.waiters.push_back(SaveGateWaiter {
                registration: Arc::clone(registration),
                waker: context.waker().clone(),
            });
        }
        Poll::Pending
    }
}

impl Drop for SaveGateFuture {
    fn drop(&mut self) {
        let Some(registration) = self.registration.take() else {
            return;
        };
        let next = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.remove_waiter(&registration) && !state.held {
                state.waiters.front().map(|waiter| waiter.waker.clone())
            } else {
                None
            }
        };
        if let Some(waker) = next {
            waker.wake();
        }
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
            state.waiters.pop_front().map(|waiter| {
                let waker = waiter.waker.clone();
                state.waiters.push_back(waiter);
                waker
            })
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
    use mount_rs_core::{FsDriver, OpenFlags};
    use mount_rs_memfs::MemoryFs;
    use std::collections::VecDeque;
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::task::Wake;
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

    struct QueuedWake {
        task: usize,
        runnable: Arc<Mutex<VecDeque<usize>>>,
    }

    impl Wake for QueuedWake {
        fn wake(self: Arc<Self>) {
            self.wake_by_ref();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.runnable.lock().unwrap().push_back(self.task);
        }
    }

    fn acquire_uncontended_gate(
        gate: &SaveGate,
        runnable: &Arc<Mutex<VecDeque<usize>>>,
    ) -> SaveGateGuard {
        let waker = Waker::from(Arc::new(QueuedWake {
            task: usize::MAX,
            runnable: Arc::clone(runnable),
        }));
        let mut future = Box::pin(gate.lock());
        match future.as_mut().poll(&mut Context::from_waker(&waker)) {
            Poll::Ready(guard) => guard,
            Poll::Pending => panic!("an uncontended gate did not resolve on its first poll"),
        }
    }

    #[test]
    fn cancelled_save_gate_waiter_does_not_strand_a_live_waiter() {
        const LIVE: usize = 0;
        const CANCELLED: usize = 1;

        let gate = SaveGate::new();
        let runnable = Arc::new(Mutex::new(VecDeque::new()));
        let holder = acquire_uncontended_gate(&gate, &runnable);
        let live_waker = Waker::from(Arc::new(QueuedWake {
            task: LIVE,
            runnable: Arc::clone(&runnable),
        }));
        let cancelled_waker = Waker::from(Arc::new(QueuedWake {
            task: CANCELLED,
            runnable: Arc::clone(&runnable),
        }));
        let mut live = Box::pin(gate.lock());
        let mut cancelled = Box::pin(gate.lock());

        assert!(matches!(
            live.as_mut().poll(&mut Context::from_waker(&live_waker)),
            Poll::Pending
        ));
        assert!(matches!(
            cancelled
                .as_mut()
                .poll(&mut Context::from_waker(&cancelled_waker)),
            Poll::Pending
        ));
        drop(cancelled);
        drop(holder);

        // Drive only tasks the gate actually schedules. A cancelled task can
        // still have a queued wakeup, but it has no future left to poll.
        let mut live_acquired = false;
        loop {
            let task = { runnable.lock().unwrap().pop_front() };
            let Some(task) = task else { break };
            if task == LIVE {
                live_acquired = matches!(
                    live.as_mut().poll(&mut Context::from_waker(&live_waker)),
                    Poll::Ready(_)
                );
                break;
            }
            assert_eq!(task, CANCELLED);
        }
        assert!(live_acquired, "the live waiter was never scheduled");
    }

    #[test]
    fn repeated_save_gate_unlocks_wake_distinct_waiters() {
        const FIRST: usize = 0;
        const SECOND: usize = 1;

        let gate = SaveGate::new();
        let runnable = Arc::new(Mutex::new(VecDeque::new()));
        let holder = acquire_uncontended_gate(&gate, &runnable);
        let first_waker = Waker::from(Arc::new(QueuedWake {
            task: FIRST,
            runnable: Arc::clone(&runnable),
        }));
        let second_waker = Waker::from(Arc::new(QueuedWake {
            task: SECOND,
            runnable: Arc::clone(&runnable),
        }));
        let mut first = Box::pin(gate.lock());
        let mut second = Box::pin(gate.lock());
        assert!(matches!(
            first.as_mut().poll(&mut Context::from_waker(&first_waker)),
            Poll::Pending
        ));
        assert!(matches!(
            second
                .as_mut()
                .poll(&mut Context::from_waker(&second_waker)),
            Poll::Pending
        ));

        drop(holder);
        // A new caller can acquire the free gate before the first scheduled
        // waiter is polled. Its release must notify the other waiter too.
        let newcomer = acquire_uncontended_gate(&gate, &runnable);
        drop(newcomer);

        let scheduled = runnable.lock().unwrap().clone();
        assert!(scheduled.contains(&SECOND));
        assert!(scheduled.contains(&FIRST), "the first waiter stayed asleep");
    }

    #[test]
    fn bounded_save_gate_cancellation_schedules_wake_both_live_waiters() {
        const ORDERS: [[usize; 3]; 6] = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];

        // Explore all three-waiter registration orders and every cancellation
        // position, before or after the holder unlocks. Re-poll the first
        // waiter with a replacement waker in each schedule.
        for order in ORDERS {
            for cancelled in 0..3 {
                for cancel_after_unlock in [false, true] {
                    let gate = SaveGate::new();
                    let runnable = Arc::new(Mutex::new(VecDeque::new()));
                    let holder = acquire_uncontended_gate(&gate, &runnable);
                    let mut wakers: [Waker; 3] = std::array::from_fn(|task| {
                        Waker::from(Arc::new(QueuedWake {
                            task,
                            runnable: Arc::clone(&runnable),
                        }))
                    });
                    let mut waiters: [_; 3] = std::array::from_fn(|_| Some(Box::pin(gate.lock())));

                    for task in order {
                        assert!(matches!(
                            waiters[task]
                                .as_mut()
                                .unwrap()
                                .as_mut()
                                .poll(&mut Context::from_waker(&wakers[task])),
                            Poll::Pending
                        ));
                    }
                    let repolled = order[0];
                    wakers[repolled] = Waker::from(Arc::new(QueuedWake {
                        task: repolled,
                        runnable: Arc::clone(&runnable),
                    }));
                    assert!(matches!(
                        waiters[repolled]
                            .as_mut()
                            .unwrap()
                            .as_mut()
                            .poll(&mut Context::from_waker(&wakers[repolled])),
                        Poll::Pending
                    ));
                    assert_eq!(gate.state.lock().unwrap().waiters.len(), 3);

                    if !cancel_after_unlock {
                        waiters[cancelled].take();
                    }
                    drop(holder);
                    if cancel_after_unlock {
                        waiters[cancelled].take();
                    }

                    let mut completed = [false; 3];
                    // The queue is driven only by real gate wakeups. This
                    // budget exceeds the two live acquisitions plus any
                    // stale wakeup queued before a cancellation.
                    for _ in 0..8 {
                        let task = { runnable.lock().unwrap().pop_front() };
                        let Some(task) = task else { break };
                        if task == cancelled || completed[task] {
                            continue;
                        }
                        if let Poll::Ready(guard) = waiters[task]
                            .as_mut()
                            .unwrap()
                            .as_mut()
                            .poll(&mut Context::from_waker(&wakers[task]))
                        {
                            completed[task] = true;
                            waiters[task].take();
                            drop(guard);
                        }
                    }
                    for (task, done) in completed.iter().enumerate() {
                        if task != cancelled {
                            assert!(
                                *done,
                                "schedule {order:?}, cancel {cancelled}, after unlock {cancel_after_unlock} stranded waiter {task}"
                            );
                        }
                    }
                }
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
