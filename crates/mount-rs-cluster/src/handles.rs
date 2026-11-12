//! Owner-local handle lifecycle for a future authenticated remote session.
//!
//! A session must be bound to one authenticated client by the transport. IDs
//! are session-local references, not bearer credentials. This module does not
//! expose HTTP routes or implement reconnect/replay. The server must retain the
//! session until `close` succeeds, including after a client disconnect.
use mount_rs_core::drain_gate::{DrainGate, OwnedDrainExclusive};
use mount_rs_core::{ErrorCode, FileHandle, FsError, OpenFlags, Result, Stats};
use mount_rs_sdk::Filesystem;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

/// One open handle. Calls share `gate`; cursor moves, mutations and close
/// take it exclusively. The gate is an atomic counter for shared entry, so
/// concurrent positional reads on one handle do not meet on a lock.
struct Slot {
    id: u64,
    gate: Arc<DrainGate>,
    /// Set once, when the backend open returns.
    handle: OnceLock<Arc<dyn FileHandle>>,
    /// Written only while holding `gate` exclusively.
    closed: AtomicBool,
}
impl Slot {
    fn handle(&self, syscall: &str) -> Result<&Arc<dyn FileHandle>> {
        if self.closed.load(Ordering::Acquire) {
            return Err(error(ErrorCode::Ebadf, syscall));
        }
        self.handle
            .get()
            .ok_or_else(|| error(ErrorCode::Ebadf, syscall))
    }
}
type Entry = Arc<Slot>;
/// The session's open handles, with no lock on any path. A handle lives in
/// the slot its id selects, so a lookup is one atomic load and an id check.
/// Ids only grow and are never reused; an id whose slot is still held by an
/// older handle is skipped.
struct Table {
    next: AtomicU64,
    len: AtomicUsize,
    slots: Box<[arc_swap::ArcSwapOption<Slot>]>,
}
impl Table {
    fn new(limit: usize) -> Self {
        Self {
            next: AtomicU64::new(1),
            len: AtomicUsize::new(0),
            slots: (0..limit)
                .map(|_| arc_swap::ArcSwapOption::empty())
                .collect(),
        }
    }
    fn slot(&self, id: u64) -> &arc_swap::ArcSwapOption<Slot> {
        &self.slots[(id % self.slots.len() as u64) as usize]
    }
    fn get(&self, id: u64) -> Option<Entry> {
        self.slot(id).load_full().filter(|entry| entry.id == id)
    }
    /// Reserves a slot for a handle that is still opening. The entry's gate
    /// is held exclusively before it is published, so a drain cannot mistake
    /// an admitted, pending open for a closed handle.
    fn reserve(&self) -> Result<(Entry, OwnedDrainExclusive)> {
        if self.len.fetch_add(1, Ordering::AcqRel) >= self.slots.len() {
            self.len.fetch_sub(1, Ordering::AcqRel);
            return Err(error(ErrorCode::Emfile, "open"));
        }
        // Fewer handles than slots are reserved, so a free slot exists.
        loop {
            let id = self.next.fetch_add(1, Ordering::AcqRel);
            if id == u64::MAX {
                self.next.store(u64::MAX, Ordering::Release);
                self.len.fetch_sub(1, Ordering::AcqRel);
                return Err(error(ErrorCode::Eoverflow, "open"));
            }
            let slot = self.slot(id);
            let current = slot.load();
            if current.is_some() {
                continue;
            }
            let entry = Arc::new(Slot {
                id,
                gate: Arc::new(DrainGate::new()),
                handle: OnceLock::new(),
                closed: AtomicBool::new(false),
            });
            let admission = entry.gate.try_write_owned().unwrap();
            if slot
                .compare_and_swap(&current, Some(Arc::clone(&entry)))
                .is_none()
            {
                return Ok((entry, admission));
            }
        }
    }
    fn remove(&self, entry: &Entry) {
        let slot = self.slot(entry.id);
        let current = slot.load();
        if !current
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, entry))
        {
            return;
        }
        let previous = slot.compare_and_swap(&current, None);
        if previous
            .as_ref()
            .is_some_and(|held| Arc::ptr_eq(held, entry))
        {
            self.len.fetch_sub(1, Ordering::AcqRel);
        }
    }
    fn entries(&self) -> Vec<Entry> {
        self.slots
            .iter()
            .filter_map(|slot| slot.load_full())
            .collect()
    }
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }
}

// Own the pending slot before polling the backend. Cancellation removes the
// reservation synchronously; no cleanup task or runtime is needed for an open
// that has not returned a handle.
struct PendingOpen<'a> {
    table: &'a Table,
    slot: Entry,
    _admission: OwnedDrainExclusive,
    committed: bool,
}
impl Drop for PendingOpen<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.table.remove(&self.slot);
        }
    }
}

/// One volume, one owner generation, and one client session. Call `close` before
/// dropping the session; failed/cancelled cleanup remains retryable. A future
/// transport must own expiry, authentication and bounded session counts.
pub struct OwnedHandleSession {
    filesystem: Arc<Filesystem>,
    fence: u64,
    closing: AtomicBool,
    path_operations: DrainGate,
    table: Table,
}

fn error(code: ErrorCode, syscall: &str) -> FsError {
    FsError::new(code).with_syscall(syscall)
}

impl OwnedHandleSession {
    pub async fn new(filesystem: Arc<Filesystem>, limit: usize) -> Result<Self> {
        if !(1..=4096).contains(&limit) {
            return Err(error(ErrorCode::Einval, "session"));
        }
        let fence = filesystem.checked_owner_fence().await?;
        Ok(Self {
            filesystem,
            fence,
            closing: AtomicBool::new(false),
            path_operations: DrainGate::new(),
            table: Table::new(limit),
        })
    }

    async fn check(&self, syscall: &str) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(error(ErrorCode::Ebadf, syscall));
        }
        if self.filesystem.checked_owner_fence().await.ok() != Some(self.fence) {
            return Err(error(ErrorCode::Estale, syscall));
        }
        Ok(())
    }

    #[cfg(any(feature = "http3", test))]
    pub(crate) async fn with_driver<T, F, Fut>(&self, operation: F) -> Result<T>
    where
        F: FnOnce(Arc<dyn mount_rs_core::FsDriver>) -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let _active = self.path_operations.read().await;
        self.check("session").await?;
        operation(self.filesystem.driver()).await
    }

    /// Reserve capacity before opening, so rejecting a full session cannot
    /// create or truncate a file. IDs are never reused during this session.
    pub async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<u64> {
        self.open_flags(path, OpenFlags::parse(flags, path)?, mode)
            .await
    }

    /// Preserve decoded native flags, including combinations with no string
    /// shorthand. The provider decides whether a combination is supported.
    pub async fn open_flags(&self, path: &str, flags: OpenFlags, mode: u32) -> Result<u64> {
        self.open_with(async { self.filesystem.driver().open_flags(path, flags, mode).await })
            .await
    }

    async fn open_with(
        &self,
        open: impl std::future::Future<Output = Result<Arc<dyn FileHandle>>>,
    ) -> Result<u64> {
        self.check("open").await?;
        let (slot, admission) = self.table.reserve()?;
        let mut pending = PendingOpen {
            table: &self.table,
            slot,
            _admission: admission,
            committed: false,
        };
        // Close raises `closing` before it reads the table and this entry was
        // published before the check below, so one of the two sees the other:
        // either close drains this handle or the open is refused here.
        if self.closing.load(Ordering::SeqCst) {
            return Err(error(ErrorCode::Ebadf, "open"));
        }
        let handle = open.await?;
        // Store the returned handle without another suspension point.
        let _ = pending.slot.handle.set(handle);
        pending.committed = true;
        if self.closing.load(Ordering::Acquire) {
            return Err(error(ErrorCode::Ebadf, "open"));
        }
        Ok(pending.slot.id)
    }

    async fn entry(&self, id: u64) -> Result<Entry> {
        self.table
            .get(id)
            .ok_or_else(|| error(ErrorCode::Ebadf, "handle"))
    }

    pub async fn read(&self, id: u64, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let entry = self.entry(id).await?;
        // Positional reads do not mutate the cursor. Shared admission still
        // keeps close and mutations excluded until the provider call finishes.
        if position.is_some() {
            let _admitted = entry.gate.read().await;
            self.check("read").await?;
            return entry.handle("read")?.read(buffer, position).await;
        }
        let _admitted = entry.gate.write().await;
        self.check("read").await?;
        entry.handle("read")?.read(buffer, position).await
    }

    pub async fn write(&self, id: u64, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let entry = self.entry(id).await?;
        let _admitted = entry.gate.write().await;
        self.check("write").await?;
        entry.handle("write")?.write(buffer, position).await
    }

    pub async fn stat(&self, id: u64) -> Result<Stats> {
        let entry = self.entry(id).await?;
        let _admitted = entry.gate.read().await;
        self.check("fstat").await?;
        entry.handle("fstat")?.stat().await
    }

    pub async fn truncate(&self, id: u64, length: u64) -> Result<()> {
        let entry = self.entry(id).await?;
        let _admitted = entry.gate.write().await;
        self.check("ftruncate").await?;
        entry.handle("ftruncate")?.truncate(length).await
    }

    pub async fn sync(&self, id: u64, data_only: bool) -> Result<()> {
        let entry = self.entry(id).await?;
        let _admitted = entry.gate.write().await;
        self.check("fsync").await?;
        let handle = entry.handle("fsync")?;
        if data_only {
            handle.datasync().await
        } else {
            handle.sync().await
        }
    }

    /// Close serializes with in-flight operations. On cancellation or failure,
    /// the table retains the handle so session cleanup can retry it.
    pub async fn close_handle(&self, id: u64) -> Result<()> {
        let entry = self.entry(id).await?;
        {
            let _exclusive = entry.gate.write().await;
            if let Ok(handle) = entry.handle("close") {
                handle.close().await?;
                entry.closed.store(true, Ordering::Release);
            }
        }
        self.table.remove(&entry);
        Ok(())
    }

    /// Stop admission, wait for admitted operations, and close every handle.
    /// Does not release the volume lease; the server does that after all sessions.
    pub async fn close(&self) -> Result<()> {
        self.closing.store(true, Ordering::SeqCst);
        let _paths_drained = self.path_operations.write().await;
        let entries = self.table.entries();
        let mut failure = None;
        for entry in &entries {
            let _exclusive = entry.gate.write().await;
            if let Ok(handle) = entry.handle("close") {
                match handle.close().await {
                    Ok(()) => entry.closed.store(true, Ordering::Release),
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        for entry in &entries {
            self.table.remove(entry);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn positional_admission_shares_reads_but_excludes_cursor_mutation_and_close() {
        let (fs, session) = fixture(1).await;
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        session.write(id, b"data", Some(0)).await.unwrap();
        let entry = session.entry(id).await.unwrap();
        // Hold the admission a positional provider call owns while in flight.
        let admitted = entry.gate.read().await;
        let mut bytes = [0; 4];
        let count = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            session.read(id, &mut bytes, Some(0)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(count, 4);
        assert_eq!(&bytes, b"data");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), session.stat(id))
                .await
                .unwrap()
                .unwrap()
                .size,
            4
        );
        let blocked = std::time::Duration::from_millis(20);
        assert!(
            tokio::time::timeout(blocked, session.read(id, &mut bytes, None))
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(blocked, session.write(id, b"x", Some(0)))
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(blocked, session.close_handle(id))
                .await
                .is_err()
        );
        drop(admitted);
        // Cancelled admission must neither consume cursor bytes nor close the
        // handle. Close succeeds after shared admission drains.
        assert_eq!(session.read(id, &mut bytes, None).await.unwrap(), 4);
        assert_eq!(&bytes, b"data");
        session.close_handle(id).await.unwrap();
        assert_eq!(session.stat(id).await.unwrap_err().code, ErrorCode::Ebadf);
        session.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    async fn fixture(limit: usize) -> (Arc<Filesystem>, Arc<OwnedHandleSession>) {
        let fs = Arc::new(
            Filesystem::split(mount_rs_sdk::SplitOptions::memory("handles", 4096))
                .await
                .unwrap(),
        );
        let session = Arc::new(OwnedHandleSession::new(fs.clone(), limit).await.unwrap());
        (fs, session)
    }

    #[tokio::test]
    async fn close_waits_for_admitted_path_operation() {
        let (fs, session) = fixture(1).await;
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, gate) = tokio::sync::oneshot::channel();
        let active = session.clone();
        let operation = tokio::spawn(async move {
            active
                .with_driver(|driver| async move {
                    entered.send(()).unwrap();
                    gate.await.unwrap();
                    driver.stat("/").await
                })
                .await
        });
        ready.await.unwrap();
        let closing = session.clone();
        let drain = tokio::spawn(async move { closing.close().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !session.closing.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!drain.is_finished());
        release.send(()).unwrap();
        operation.await.unwrap().unwrap();
        drain.await.unwrap().unwrap();
        assert_eq!(
            session
                .with_driver(|driver| async move { driver.stat("/").await })
                .await
                .unwrap_err()
                .code,
            ErrorCode::Ebadf
        );
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn pending_open_does_not_block_reads_and_cancel_releases_capacity() {
        let (fs, session) = fixture(2).await;
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        session.write(id, b"readable", Some(0)).await.unwrap();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let pending = session.clone();
        let task = tokio::spawn(async move {
            pending
                .open_with(async {
                    entered.send(()).unwrap();
                    std::future::pending().await
                })
                .await
        });
        ready.await.unwrap();
        assert_eq!(
            session.open("/rejected", "w", 0).await.unwrap_err().code,
            ErrorCode::Emfile
        );
        let mut bytes = [0; 8];
        let count = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            session.read(id, &mut bytes, Some(0)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(count, 8);
        assert_eq!(&bytes, b"readable");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let next = session.open("/next", "w", 0o600).await.unwrap();
        assert!(next > id + 1); // cancelled IDs are not reused
        session.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn drain_waits_for_admitted_open_and_closes_its_returned_handle() {
        let (fs, session) = fixture(1).await;
        let (entered, ready) = tokio::sync::oneshot::channel();
        let (release, gate) = tokio::sync::oneshot::channel();
        let opening = session.clone();
        let driver = fs.driver();
        let task = tokio::spawn(async move {
            opening
                .open_with(async {
                    entered.send(()).unwrap();
                    gate.await.unwrap();
                    driver.open("/file", "w+", 0o600).await
                })
                .await
        });
        ready.await.unwrap();
        let closing = session.clone();
        let drain = tokio::spawn(async move { closing.close().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !session.closing.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!drain.is_finished());
        release.send(()).unwrap();
        assert_eq!(task.await.unwrap().unwrap_err().code, ErrorCode::Ebadf);
        drain.await.unwrap().unwrap();
        assert!(session.table.is_empty());
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    #[ignore = "requires isolated scripts/test-tidb.sh composition"]
    async fn tidb_sessions_preserve_handles_and_reopen_persisted_writes() {
        assert!(
            std::path::Path::new(
                &std::env::var("MOUNT_RS_TIDB_RUN_DIR").expect("isolated TiDB harness")
            )
            .is_dir()
        );
        let blocks = tempfile::tempdir().unwrap();
        let mut options = mount_rs_sdk::SplitOptions::memory("session-owner", 4096);
        options.metadata = mount_rs_sdk::StoreConfig::Tidb {
            connection: std::env::var("MOUNT_RS_TIDB_URL").expect("TiDB fixture URL"),
            volume_key: format!(
                "handles-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ),
            durable: true,
        };
        options.blocks = mount_rs_sdk::StoreConfig::Local {
            path: blocks.path().to_owned(),
        };
        let backend =
            std::env::var("MOUNT_RS_SESSION_TEST_BLOCKS").unwrap_or_else(|_| "local".into());
        assert!(matches!(backend.as_str(), "local" | "rustfs"));
        if backend == "rustfs" {
            assert!(
                std::path::Path::new(
                    &std::env::var("RUSTFS_RUN_DIR").expect("isolated RustFS harness")
                )
                .is_dir()
            );
            let config = mount_rs_r2::R2Config::from_env().expect("RustFS fixture config");
            assert!(
                config.endpoint.starts_with("http://127.0.0.1:")
                    || config.endpoint.starts_with("http://localhost:")
            );
            options.blocks = mount_rs_sdk::StoreConfig::R2 {
                endpoint: config.endpoint,
                bucket: config.bucket,
                prefix: format!(
                    "{}/owned-handles",
                    std::env::var("RUSTFS_COMBO_PREFIX").expect("isolated RustFS prefix")
                ),
                access_key_id: config.access_key_id,
                secret_access_key: config.secret_access_key,
                durable: true,
            };
        }
        let runtime = mount_rs_sdk::SplitRuntimeOptions {
            lease_ttl: std::time::Duration::from_secs(240),
            read_lease_cache_window: std::time::Duration::from_secs(60),
            atime_policy: mount_rs_sdk::AtimePolicy::NoAtime,
            ..Default::default()
        };
        let fs = Arc::new(
            Filesystem::split_with_runtime(options.clone(), runtime)
                .await
                .unwrap(),
        );
        let fence = fs.checked_owner_fence().await.unwrap();
        let first = OwnedHandleSession::new(fs.clone(), 4).await.unwrap();
        let second = OwnedHandleSession::new(fs.clone(), 4).await.unwrap();
        let old = first.open("/file", "w+", 0o600).await.unwrap();
        first.write(old, b"old", Some(0)).await.unwrap();
        fs.driver().rename("/file", "/renamed").await.unwrap();
        fs.driver().unlink("/renamed").await.unwrap();
        let replacement = second.open("/file", "wx+", 0o600).await.unwrap();
        second.write(replacement, b"new", Some(0)).await.unwrap();
        first.write(old, b"OLD", Some(0)).await.unwrap();
        let mut bytes = [0; 3];
        assert_eq!(first.read(old, &mut bytes, Some(0)).await.unwrap(), 3);
        assert_eq!(&bytes, b"OLD");
        assert_eq!(
            second.read(replacement, &mut bytes, Some(0)).await.unwrap(),
            3
        );
        assert_eq!(&bytes, b"new");
        let a = first.open("/append", "a+", 0o600).await.unwrap();
        let b = second.open("/append", "a+", 0o600).await.unwrap();
        let (left, right) = tokio::join!(
            first.write(a, b"AAAA", None),
            second.write(b, b"BBBB", None)
        );
        assert_eq!(left.unwrap(), 4);
        assert_eq!(right.unwrap(), 4);
        first.sync(a, false).await.unwrap();
        second.sync(replacement, true).await.unwrap();
        first.close().await.unwrap();
        second.close().await.unwrap();
        fs.shutdown().await.unwrap();
        options.owner = "session-reopened".into();
        let reopened = Arc::new(
            Filesystem::split_with_runtime(options, runtime)
                .await
                .unwrap(),
        );
        assert!(reopened.checked_owner_fence().await.unwrap() > fence);
        let session = OwnedHandleSession::new(reopened.clone(), 2).await.unwrap();
        let file = session.open("/file", "r", 0).await.unwrap();
        assert_eq!(session.read(file, &mut bytes, Some(0)).await.unwrap(), 3);
        assert_eq!(&bytes, b"new");
        let append = session.open("/append", "r", 0).await.unwrap();
        let mut appended = [0; 8];
        assert_eq!(
            session.read(append, &mut appended, Some(0)).await.unwrap(),
            8
        );
        assert!(matches!(&appended, b"AAAABBBB" | b"BBBBAAAA"));
        assert_eq!(
            reopened.driver().stat("/renamed").await.unwrap_err().code,
            ErrorCode::Enoent
        );
        session.close().await.unwrap();
        reopened.shutdown().await.unwrap();
        println!("OWNED_HANDLE_PROVIDER_EVIDENCE metadata=tidb blocks={backend} reopen=pass");
    }

    #[tokio::test]
    async fn decoded_create_without_truncate_preserves_existing_bytes() {
        let (fs, session) = fixture(2).await;
        let initial = session.open("/file", "w+", 0o600).await.unwrap();
        session.write(initial, b"keep", Some(0)).await.unwrap();
        session.close_handle(initial).await.unwrap();
        let flags = OpenFlags {
            read: true,
            write: true,
            create: true,
            truncate: false,
            append: false,
            exclusive: false,
        };
        let id = session.open_flags("/file", flags, 0o600).await.unwrap();
        let mut bytes = [0; 4];
        assert_eq!(session.read(id, &mut bytes, Some(0)).await.unwrap(), 4);
        assert_eq!(&bytes, b"keep");
        session.truncate(id, 2).await.unwrap();
        session.sync(id, true).await.unwrap();
        session.sync(id, false).await.unwrap();
        assert_eq!(session.stat(id).await.unwrap().size, 2);
        session.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn independent_sessions_preserve_exclusive_create_and_atomic_append() {
        let (fs, first) = fixture(2).await;
        let second = Arc::new(OwnedHandleSession::new(fs.clone(), 2).await.unwrap());
        let (a, b) = tokio::join!(
            first.open("/exclusive", "wx", 0o600),
            second.open("/exclusive", "wx", 0o600)
        );
        assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
        assert_eq!(a.err().or_else(|| b.err()).unwrap().code, ErrorCode::Eexist);
        let a = first.open("/append", "a+", 0o600).await.unwrap();
        let b = second.open("/append", "a+", 0o600).await.unwrap();
        let write_a = async {
            for _ in 0..16 {
                assert_eq!(first.write(a, &[b'A'; 16], None).await.unwrap(), 16);
            }
        };
        let write_b = async {
            for _ in 0..16 {
                assert_eq!(second.write(b, &[b'B'; 16], None).await.unwrap(), 16);
            }
        };
        tokio::join!(write_a, write_b);
        let mut bytes = [0; 512];
        assert_eq!(first.read(a, &mut bytes, Some(0)).await.unwrap(), 512);
        assert_eq!(
            bytes.chunks_exact(16).filter(|b| *b == [b'A'; 16]).count(),
            16
        );
        assert_eq!(
            bytes.chunks_exact(16).filter(|b| *b == [b'B'; 16]).count(),
            16
        );
        first.close().await.unwrap();
        second.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn retained_handle_survives_rename_unlink_and_path_replacement() {
        let (fs, session) = fixture(4).await;
        let old = session.open("/original", "w+", 0o600).await.unwrap();
        session.write(old, b"original", Some(0)).await.unwrap();
        fs.driver().rename("/original", "/renamed").await.unwrap();
        fs.driver().unlink("/renamed").await.unwrap();
        let new = session.open("/original", "w+", 0o600).await.unwrap();
        session.write(new, b"new", Some(0)).await.unwrap();
        session.write(old, b"O", Some(0)).await.unwrap();
        let mut bytes = [0; 8];
        assert_eq!(session.read(old, &mut bytes, Some(0)).await.unwrap(), 8);
        assert_eq!(&bytes, b"Original");
        assert_eq!(session.read(new, &mut bytes, Some(0)).await.unwrap(), 3);
        assert_eq!(&bytes[..3], b"new");
        assert_ne!(
            session.stat(old).await.unwrap().ino,
            session.stat(new).await.unwrap().ino
        );
        session.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn capacity_rejection_has_no_create_or_truncate_side_effect() {
        let (fs, session) = fixture(1).await;
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        session.write(id, b"keep", Some(0)).await.unwrap();
        assert_eq!(
            session.open("/file", "w", 0).await.unwrap_err().code,
            ErrorCode::Emfile
        );
        assert_eq!(
            session.open("/absent", "w", 0).await.unwrap_err().code,
            ErrorCode::Emfile
        );
        assert_eq!(fs.driver().stat("/file").await.unwrap().size, 4);
        assert_eq!(
            fs.driver().stat("/absent").await.unwrap_err().code,
            ErrorCode::Enoent
        );
        session.close_handle(id).await.unwrap();
        let replacement = session.open("/file", "r", 0).await.unwrap();
        assert_ne!(id, replacement);
        assert_eq!(session.stat(id).await.unwrap_err().code, ErrorCode::Ebadf);
        session.close().await.unwrap();
        assert_eq!(
            session.open("/file", "r", 0).await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        session.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_session_drain_remains_retryable() {
        let (fs, session) = fixture(1).await;
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        let entry = session.entry(id).await.unwrap();
        let in_flight = entry.gate.write().await;
        let closing = session.clone();
        let task = tokio::spawn(async move { closing.close().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !session.closing.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        drop(in_flight);
        assert_eq!(session.stat(id).await.unwrap_err().code, ErrorCode::Ebadf);
        session.close().await.unwrap();
        assert!(session.table.is_empty());
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn lost_owner_rejects_retained_handle_operations() {
        let (fs, session) = fixture(1).await;
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        fs.shutdown().await.unwrap();
        assert_eq!(session.stat(id).await.unwrap_err().code, ErrorCode::Estale);
        assert_eq!(
            session.write(id, b"x", Some(0)).await.unwrap_err().code,
            ErrorCode::Estale
        );
        session.close().await.unwrap();
    }

    /// A handle whose calls park until the test lets them go, and which
    /// records what the session let into it at once.
    struct Parked {
        entered: tokio::sync::Semaphore,
        release: tokio::sync::Semaphore,
        inside: AtomicUsize,
        most_inside: AtomicUsize,
        closes: AtomicUsize,
        closed_over_a_call: AtomicBool,
    }
    type Call<'a, T> = std::pin::Pin<Box<dyn std::future::Future<Output = Result<T>> + Send + 'a>>;
    impl Parked {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                entered: tokio::sync::Semaphore::new(0),
                release: tokio::sync::Semaphore::new(0),
                inside: AtomicUsize::new(0),
                most_inside: AtomicUsize::new(0),
                closes: AtomicUsize::new(0),
                closed_over_a_call: AtomicBool::new(false),
            })
        }
        async fn park(&self) {
            self.entered.add_permits(1);
            self.release.acquire().await.unwrap().forget();
        }
        async fn call(&self) -> Result<usize> {
            let inside = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
            self.most_inside.fetch_max(inside, Ordering::SeqCst);
            self.park().await;
            self.inside.fetch_sub(1, Ordering::SeqCst);
            Ok(0)
        }
        async fn entered(&self) {
            self.entered.acquire().await.unwrap().forget();
        }
    }
    impl FileHandle for Parked {
        fn read<'a, 'b, 'c>(&'a self, _: &'b mut [u8], _: Option<u64>) -> Call<'c, usize>
        where
            'a: 'c,
            'b: 'c,
            Self: 'c,
        {
            Box::pin(self.call())
        }
        fn write<'a, 'b, 'c>(&'a self, _: &'b [u8], _: Option<u64>) -> Call<'c, usize>
        where
            'a: 'c,
            'b: 'c,
            Self: 'c,
        {
            Box::pin(self.call())
        }
        fn stat<'a, 'c>(&'a self) -> Call<'c, Stats>
        where
            'a: 'c,
            Self: 'c,
        {
            Box::pin(async { Err(error(ErrorCode::Ebadf, "fstat")) })
        }
        fn truncate<'a, 'c>(&'a self, _: u64) -> Call<'c, ()>
        where
            'a: 'c,
            Self: 'c,
        {
            Box::pin(async { self.call().await.map(|_| ()) })
        }
        fn close<'a, 'c>(&'a self) -> Call<'c, ()>
        where
            'a: 'c,
            Self: 'c,
        {
            Box::pin(async {
                self.closes.fetch_add(1, Ordering::SeqCst);
                if self.inside.load(Ordering::SeqCst) != 0 {
                    self.closed_over_a_call.store(true, Ordering::SeqCst);
                }
                self.park().await;
                Ok(())
            })
        }
    }
    async fn parked_fixture() -> (Arc<Filesystem>, Arc<OwnedHandleSession>, Arc<Parked>, u64) {
        let (fs, session) = fixture(1).await;
        let handle = Parked::new();
        let opened: Arc<dyn FileHandle> = handle.clone();
        let id = session.open_with(async { Ok(opened) }).await.unwrap();
        (fs, session, handle, id)
    }
    /// Lets every task that can run without the handle's release run.
    async fn settle() {
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn a_second_close_waits_for_the_first_and_the_handle_is_closed_once() {
        let (fs, session, handle, id) = parked_fixture().await;
        let closer = |session: Arc<OwnedHandleSession>| {
            tokio::spawn(async move { session.close_handle(id).await })
        };
        let first = closer(session.clone());
        handle.entered().await;
        // The first close is inside the handle and has not returned.
        let second = closer(session.clone());
        settle().await;
        handle.release.add_permits(2);
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(
            handle.closes.load(Ordering::SeqCst),
            1,
            "two closes both called the handle's close"
        );
        session.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn close_waits_for_a_read_that_is_inside_the_handle() {
        let (fs, session, handle, id) = parked_fixture().await;
        let reading = session.clone();
        let read = tokio::spawn(async move { reading.read(id, &mut [0; 1], Some(0)).await });
        handle.entered().await;
        let closing = session.clone();
        let close = tokio::spawn(async move { closing.close_handle(id).await });
        settle().await;
        assert_eq!(
            handle.closes.load(Ordering::SeqCst),
            0,
            "close ran while a read was inside the handle"
        );
        handle.release.add_permits(2);
        read.await.unwrap().unwrap();
        close.await.unwrap().unwrap();
        assert!(!handle.closed_over_a_call.load(Ordering::SeqCst));
        assert_eq!(handle.closes.load(Ordering::SeqCst), 1);
        session.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cursor_reads_enter_the_handle_one_at_a_time() {
        let (fs, session, handle, id) = parked_fixture().await;
        let reader = |session: Arc<OwnedHandleSession>| {
            tokio::spawn(async move { session.read(id, &mut [0; 1], None).await })
        };
        let first = reader(session.clone());
        handle.entered().await;
        let second = reader(session.clone());
        settle().await;
        handle.release.add_permits(2);
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(
            handle.most_inside.load(Ordering::SeqCst),
            1,
            "two cursor reads were inside the handle together"
        );
        handle.release.add_permits(1);
        session.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_cursor_reads_consume_each_byte_once() {
        let (fs, session) = fixture(1).await;
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        session.write(id, b"abcdefgh", Some(0)).await.unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let session = session.clone();
            tasks.spawn(async move {
                let mut byte = [0];
                assert_eq!(session.read(id, &mut byte, None).await.unwrap(), 1);
                byte[0]
            });
        }
        let mut bytes = Vec::new();
        while let Some(byte) = tasks.join_next().await {
            bytes.push(byte.unwrap());
        }
        bytes.sort_unstable();
        assert_eq!(&bytes, b"abcdefgh");
        let mut byte = [0];
        assert_eq!(session.read(id, &mut byte, None).await.unwrap(), 0);
        session.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }
}
