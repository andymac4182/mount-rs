//! Owner-local handle lifecycle for a future authenticated remote session.
//!
//! A session must be bound to one authenticated client by the transport. IDs
//! are session-local references, not bearer credentials. This module does not
//! expose HTTP routes or implement reconnect/replay. The server must retain the
//! session until `close` succeeds, including after a client disconnect.
use mount_rs_core::{ErrorCode, FileHandle, FsError, OpenFlags, Result, Stats};
use mount_rs_sdk::Filesystem;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex as TableMutex,
        atomic::{AtomicBool, Ordering},
    },
};
use tokio::sync::RwLock;

type Entry = Arc<RwLock<Option<Arc<dyn FileHandle>>>>;
struct State {
    next: u64,
    handles: BTreeMap<u64, Entry>,
}

// Own the pending slot before polling the backend. Cancellation removes the
// reservation synchronously; no cleanup task or runtime is needed for an open
// that has not returned a handle.
struct PendingOpen<'a> {
    state: &'a TableMutex<State>,
    id: u64,
    handle: tokio::sync::OwnedRwLockWriteGuard<Option<Arc<dyn FileHandle>>>,
    committed: bool,
}
impl Drop for PendingOpen<'_> {
    fn drop(&mut self) {
        if !self.committed {
            self.state.lock().unwrap().handles.remove(&self.id);
        }
    }
}

/// One volume, one owner generation, and one client session. Call `close` before
/// dropping the session; failed/cancelled cleanup remains retryable. A future
/// transport must own expiry, authentication and bounded session counts.
pub struct OwnedHandleSession {
    filesystem: Arc<Filesystem>,
    fence: u64,
    limit: usize,
    closing: AtomicBool,
    path_operations: tokio::sync::RwLock<()>,
    state: TableMutex<State>,
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
            limit,
            closing: AtomicBool::new(false),
            path_operations: tokio::sync::RwLock::new(()),
            state: TableMutex::new(State {
                next: 1,
                handles: BTreeMap::new(),
            }),
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
        let mut pending = {
            let mut state = self.state.lock().unwrap();
            // Serialize admission with close's table snapshot.
            if self.closing.load(Ordering::Acquire) {
                return Err(error(ErrorCode::Ebadf, "open"));
            }
            if state.handles.len() >= self.limit {
                return Err(error(ErrorCode::Emfile, "open"));
            }
            let next = state
                .next
                .checked_add(1)
                .ok_or_else(|| error(ErrorCode::Eoverflow, "open"))?;
            let id = state.next;
            state.next = next;
            let entry = Arc::new(RwLock::new(None));
            // The fresh entry is locked before publication so drain cannot
            // mistake an admitted, pending open for an already-closed handle.
            let handle = entry.clone().try_write_owned().unwrap();
            state.handles.insert(id, entry);
            PendingOpen {
                state: &self.state,
                id,
                handle,
                committed: false,
            }
        };
        let handle = open.await?;
        // Store the returned handle without another suspension point.
        *pending.handle = Some(handle);
        pending.committed = true;
        if self.closing.load(Ordering::Acquire) {
            return Err(error(ErrorCode::Ebadf, "open"));
        }
        Ok(pending.id)
    }

    async fn entry(&self, id: u64) -> Result<Entry> {
        self.state
            .lock()
            .unwrap()
            .handles
            .get(&id)
            .cloned()
            .ok_or_else(|| error(ErrorCode::Ebadf, "handle"))
    }

    pub async fn read(&self, id: u64, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let entry = self.entry(id).await?;
        // Positional reads do not mutate the cursor. Shared admission still
        // keeps close and mutations excluded until the provider call finishes.
        if position.is_some() {
            let entry = entry.read().await;
            self.check("read").await?;
            return entry
                .as_ref()
                .ok_or_else(|| error(ErrorCode::Ebadf, "read"))?
                .read(buffer, position)
                .await;
        }
        let entry = entry.write().await;
        self.check("read").await?;
        entry
            .as_ref()
            .ok_or_else(|| error(ErrorCode::Ebadf, "read"))?
            .read(buffer, position)
            .await
    }

    pub async fn write(&self, id: u64, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let entry = self.entry(id).await?;
        let entry = entry.write().await;
        self.check("write").await?;
        entry
            .as_ref()
            .ok_or_else(|| error(ErrorCode::Ebadf, "write"))?
            .write(buffer, position)
            .await
    }

    pub async fn stat(&self, id: u64) -> Result<Stats> {
        let entry = self.entry(id).await?;
        let entry = entry.read().await;
        self.check("fstat").await?;
        entry
            .as_ref()
            .ok_or_else(|| error(ErrorCode::Ebadf, "fstat"))?
            .stat()
            .await
    }

    pub async fn truncate(&self, id: u64, length: u64) -> Result<()> {
        let entry = self.entry(id).await?;
        let entry = entry.write().await;
        self.check("ftruncate").await?;
        entry
            .as_ref()
            .ok_or_else(|| error(ErrorCode::Ebadf, "ftruncate"))?
            .truncate(length)
            .await
    }

    pub async fn sync(&self, id: u64, data_only: bool) -> Result<()> {
        let entry = self.entry(id).await?;
        let entry = entry.write().await;
        self.check("fsync").await?;
        let handle = entry
            .as_ref()
            .ok_or_else(|| error(ErrorCode::Ebadf, "fsync"))?;
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
            let mut entry = entry.write().await;
            if let Some(handle) = entry.as_ref() {
                handle.close().await?;
                *entry = None;
            }
        }
        self.state.lock().unwrap().handles.remove(&id);
        Ok(())
    }

    /// Stop admission, wait for admitted operations, and close every handle.
    /// Does not release the volume lease; the server does that after all sessions.
    pub async fn close(&self) -> Result<()> {
        self.closing.store(true, Ordering::Release);
        let _paths_drained = self.path_operations.write().await;
        let entries: Vec<_> = self
            .state
            .lock()
            .unwrap()
            .handles
            .values()
            .cloned()
            .collect();
        let mut failure = None;
        for entry in entries {
            let mut entry = entry.write().await;
            if let Some(handle) = entry.as_ref() {
                match handle.close().await {
                    Ok(()) => *entry = None,
                    Err(error) => {
                        failure.get_or_insert(error);
                    }
                }
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        self.state.lock().unwrap().handles.clear();
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
        let admitted = entry.read().await;
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
        assert!(session.state.lock().unwrap().handles.is_empty());
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
        let in_flight = entry.write().await;
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
        assert!(session.state.lock().unwrap().handles.is_empty());
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
