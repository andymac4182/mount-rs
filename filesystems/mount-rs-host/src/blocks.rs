//! Immutable, fsync-backed local files. The configured directory must already
//! exist on a durable filesystem and be owned exclusively by the application.
//! This is not a sandbox against a hostile process renaming parent directories.
use async_trait::async_trait;
use mount_rs_core::storage::{BlockId, BlockStore};
use mount_rs_core::{ErrorCode, FsError, Result};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub const LOCAL_MAX_BLOCK_BYTES: usize = 16 * 1024 * 1024;

/// Shared bound on active local block mutations across stores and volumes.
/// Reads bypass this budget. Clone one budget into every store on a server.
/// This limits active blocking work, not the number of waiting requests.
#[derive(Clone, Debug)]
pub struct LocalWriteBudget(Arc<Semaphore>);

impl PartialEq for LocalWriteBudget {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for LocalWriteBudget {}

impl LocalWriteBudget {
    pub fn new(max_active: usize) -> Result<Self> {
        if !(1..=1024).contains(&max_active) {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("local write concurrency must be 1..1024"));
        }
        Ok(Self(Arc::new(Semaphore::new(max_active))))
    }

    async fn acquire(&self) -> Result<OwnedSemaphorePermit> {
        self.0
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("local write budget closed"))
    }
}

/// A volume-scoped directory of immutable SHA-256-addressed files.
/// Successful puts sync the file and directory before returning. Gets verify
/// identity, so corrupted bytes fail instead of entering a read-through cache.
/// Deletes sync the directory and are never implicit on close. Crash-created
/// temporary/orphan files are retained for an explicit reachability-aware GC.
/// Local storage is host-affine unless the same directory is shared safely.
#[derive(Clone, Debug)]
pub struct LocalBlockStore {
    root: Arc<PathBuf>,
    write_budget: Option<LocalWriteBudget>,
}

impl LocalBlockStore {
    /// Use a pre-existing, application-owned directory. Provision its durable
    /// parents separately. Unix directory fsync is required; other platforms
    /// currently return ENOTSUP instead of claiming unverified durability.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        if !cfg!(unix) {
            return Err(FsError::enotsup("local block directory durability"));
        }
        let root = fs::canonicalize(root.as_ref()).map_err(|e| io_error(e, "open local blocks"))?;
        if !root.is_dir() {
            return Err(FsError::new(ErrorCode::Enotdir));
        }
        sync_directory(&root)?;
        Ok(Self {
            root: Arc::new(root),
            write_budget: None,
        })
    }
    /// Open with a shared mutation budget; existing `open` behavior is unchanged.
    pub fn open_with_write_budget(
        root: impl AsRef<Path>,
        budget: LocalWriteBudget,
    ) -> Result<Self> {
        let mut store = Self::open(root)?;
        store.write_budget = Some(budget);
        Ok(store)
    }

    async fn write_permit(&self) -> Result<Option<OwnedSemaphorePermit>> {
        match &self.write_budget {
            Some(budget) => budget.acquire().await.map(Some),
            None => Ok(None),
        }
    }
}
fn io_error(error: std::io::Error, syscall: &'static str) -> FsError {
    FsError::new(super::error_code(&error)).with_syscall(syscall)
}
fn identity(bytes: &[u8]) -> BlockId {
    BlockId(format!("b{:x}", Sha256::digest(bytes)))
}
fn validate(id: &BlockId) -> Result<()> {
    if id.0.len() != 65
        || !id.0.starts_with('b')
        || !id.0.as_bytes()[1..]
            .iter()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
    {
        return Err(FsError::new(ErrorCode::Einval).with_message("invalid local block ID"));
    }
    Ok(())
}
fn sync_directory(root: &Path) -> Result<()> {
    File::open(root)
        .and_then(|f| f.sync_all())
        .map_err(|e| io_error(e, "sync local block directory"))
}
fn read_verified(root: &Path, id: &BlockId) -> Result<(File, Vec<u8>)> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options
        .open(root.join(&id.0))
        .map_err(|e| io_error(e, "read local block"))?;
    let metadata = file
        .metadata()
        .map_err(|e| io_error(e, "stat local block"))?;
    if !metadata.is_file() {
        return Err(FsError::new(ErrorCode::Eio).with_message("local block is not a regular file"));
    }
    if metadata.len() > LOCAL_MAX_BLOCK_BYTES as u64 {
        return Err(FsError::new(ErrorCode::Efbig));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(LOCAL_MAX_BLOCK_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(e, "read local block"))?;
    if bytes.len() > LOCAL_MAX_BLOCK_BYTES {
        return Err(FsError::new(ErrorCode::Efbig));
    }
    if identity(&bytes) != *id {
        return Err(FsError::new(ErrorCode::Eio).with_message("local block content hash mismatch"));
    }
    Ok((file, bytes))
}
async fn blocking<T: Send + 'static>(
    operation: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|_| FsError::new(ErrorCode::Eio).with_message("local block worker failed"))?
}
#[async_trait]
impl BlockStore for LocalBlockStore {
    fn durable(&self) -> bool {
        cfg!(unix)
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        if bytes.len() > LOCAL_MAX_BLOCK_BYTES {
            return Err(FsError::new(ErrorCode::Efbig));
        }
        let root = self.root.clone();
        let permit = self.write_permit().await?;
        let bytes = bytes.to_vec();
        blocking(move || {
            let _permit = permit;
            let id = identity(&bytes);
            // Content-addressed dedup: an existing verified block needs the
            // same durability barrier (file and directory sync) but no new
            // temporary file, write or extra fsync. A corrupt existing block
            // still fails with EIO and is never replaced here.
            match read_verified(&root, &id) {
                Ok((file, existing)) => {
                    if existing != bytes {
                        return Err(FsError::new(ErrorCode::Eio)
                            .with_message("local block identity collision"));
                    }
                    file.sync_all()
                        .map_err(|e| io_error(e, "sync existing local block"))?;
                    sync_directory(&root)?;
                    return Ok(id);
                }
                Err(error) if error.code == ErrorCode::Enoent => {}
                Err(error) => return Err(error),
            }
            let mut temp = tempfile::NamedTempFile::new_in(root.as_ref())
                .map_err(|e| io_error(e, "create local block"))?;
            temp.write_all(&bytes)
                .map_err(|e| io_error(e, "write local block"))?;
            temp.as_file()
                .sync_all()
                .map_err(|e| io_error(e, "sync local block"))?;
            match temp.persist_noclobber(root.join(&id.0)) {
                Ok(_) => {}
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let (file, existing) = read_verified(&root, &id)?;
                    if existing != bytes {
                        return Err(FsError::new(ErrorCode::Eio)
                            .with_message("local block identity collision"));
                    }
                    file.sync_all()
                        .map_err(|e| io_error(e, "sync existing local block"))?;
                    // The owned temporary file is removed when error drops.
                    drop(error);
                }
                Err(error) => return Err(io_error(error.error, "publish local block")),
            }
            sync_directory(&root)?;
            Ok(id)
        })
        .await
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        validate(id)?;
        let root = self.root.clone();
        let id = id.clone();
        blocking(move || read_verified(&root, &id).map(|(_, bytes)| bytes)).await
    }
    async fn flush(&self) -> Result<()> {
        // The barrier covers prior successful puts, each of which already
        // synced both its file and directory before acknowledging success.
        // No pending buffered data exists here. In-flight puts must still be
        // awaited by their caller; repeating directory fsync would not make
        // an unfinished put safe to publish.
        Ok(())
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        validate(id)?;
        let root = self.root.clone();
        let id = id.clone();
        let permit = self.write_permit().await?;
        blocking(move || {
            let _permit = permit;
            fs::remove_file(root.join(id.0)).map_err(|e| io_error(e, "delete local block"))?;
            sync_directory(&root)
        })
        .await
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn write_budget_is_shared_and_reads_bypass_it() {
        assert!(LocalWriteBudget::new(0).is_err());
        assert!(LocalWriteBudget::new(1025).is_err());
        let budget = LocalWriteBudget::new(1).unwrap();
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let a = LocalBlockStore::open_with_write_budget(first.path(), budget.clone()).unwrap();
        let b = LocalBlockStore::open_with_write_budget(second.path(), budget.clone()).unwrap();
        let id = a.put(b"readable").await.unwrap();
        let held = b.write_permit().await.unwrap();
        assert_eq!(budget.0.available_permits(), 0);
        assert_eq!(a.get(&id).await.unwrap(), b"readable");
        let mut put = Box::pin(a.put(b"queued"));
        assert!(
            std::future::poll_fn(|cx| {
                std::task::Poll::Ready(put.as_mut().poll(cx).is_pending())
            })
            .await
        );
        drop(put);
        drop(held);
        assert_eq!(budget.0.available_permits(), 1);
        b.put(b"next").await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_blocking_work_retains_write_permit_until_completion() {
        let budget = LocalWriteBudget::new(1).unwrap();
        let permit = budget.acquire().await.unwrap();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(blocking(move || {
            let held = permit;
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            drop(held);
            done_tx.send(()).unwrap();
            Ok(())
        }));
        entered_rx.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(budget.0.available_permits(), 0);
        release_tx.send(()).unwrap();
        done_rx.await.unwrap();
        assert_eq!(budget.0.available_permits(), 1);
    }

    #[tokio::test]
    async fn immutable_files_reopen_verify_and_delete() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalBlockStore::open(dir.path()).unwrap();
        let id = store.put(b"persistent block").await.unwrap();
        assert!(store.durable());
        // A successful put is immediately reopenable, without a later flush.
        let reopened = LocalBlockStore::open(dir.path()).unwrap();
        assert_eq!(reopened.get(&id).await.unwrap(), b"persistent block");
        drop(reopened);
        assert_eq!(store.put(b"persistent block").await.unwrap(), id);
        store.flush().await.unwrap();
        drop(store);
        let store = LocalBlockStore::open(dir.path()).unwrap();
        assert_eq!(store.get(&id).await.unwrap(), b"persistent block");
        fs::write(dir.path().join(&id.0), b"corrupt").unwrap();
        assert_eq!(store.get(&id).await.unwrap_err().code, ErrorCode::Eio);
        assert_eq!(
            store.put(b"persistent block").await.unwrap_err().code,
            ErrorCode::Eio
        );
        assert_eq!(fs::read(dir.path().join(&id.0)).unwrap(), b"corrupt");
        store.delete(&id).await.unwrap();
        assert_eq!(store.get(&id).await.unwrap_err().code, ErrorCode::Enoent);
    }
    #[tokio::test]
    async fn invalid_ids_symlinks_and_oversize_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalBlockStore::open(dir.path()).unwrap();
        assert_eq!(
            store
                .get(&BlockId("../escape".into()))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Einval
        );
        assert_eq!(
            store
                .delete(&BlockId("../escape".into()))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Einval
        );
        let id = identity(b"external");
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), b"external").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join(&id.0)).unwrap();
        assert!(store.get(&id).await.is_err());
        assert!(store.put(b"external").await.is_err());
        assert_eq!(fs::read(outside.path()).unwrap(), b"external");
        assert_eq!(
            store
                .put(&vec![0; LOCAL_MAX_BLOCK_BYTES + 1])
                .await
                .unwrap_err()
                .code,
            ErrorCode::Efbig
        );
    }
    #[tokio::test]
    async fn concurrent_duplicate_puts_publish_one_valid_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalBlockStore::open(dir.path()).unwrap();
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let store = store.clone();
            tasks.spawn(async move { store.put(b"same block").await.unwrap() });
        }
        let expected = identity(b"same block");
        while let Some(result) = tasks.join_next().await {
            assert_eq!(result.unwrap(), expected);
        }
        assert_eq!(store.get(&expected).await.unwrap(), b"same block");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
