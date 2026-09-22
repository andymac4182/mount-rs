use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, ErrorCode, FileHandle, FsDriver, FsError, OpenFlags, Result, Stats,
};
use mount_rs_fuse::{RequestHeader, constants::FUSE_SYNCFS, session::FuseSession};
use mount_rs_memfs::MemoryFs;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

fn frame(opcode: u32, nodeid: u64, body: &[u8]) -> Vec<u8> {
    let mut bytes = RequestHeader {
        len: (40 + body.len()) as u32,
        opcode,
        unique: 42,
        nodeid,
        uid: 0,
        gid: 0,
        pid: 0,
        total_extlen: 0,
    }
    .encode()
    .to_vec();
    bytes.extend(body);
    bytes
}

fn number(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

async fn raw_request(session: &mut FuseSession, opcode: u32, nodeid: u64, body: &[u8]) -> Vec<u8> {
    if session.negotiated.is_none() && opcode != 26 {
        let init: Vec<u8> = [7u32, 41, 65536, u32::MAX, u32::MAX]
            .into_iter()
            .flat_map(u32::to_le_bytes)
            .collect();
        let reply = session.handle(&frame(26, 0, &init)).await.unwrap().unwrap();
        assert_eq!(&reply[4..8], &[0; 4]);
    }
    session
        .handle(&frame(opcode, nodeid, body))
        .await
        .unwrap()
        .unwrap()
}

fn success(reply: &[u8]) -> &[u8] {
    assert_eq!(&reply[4..8], &[0; 4]);
    reply.get(16..).unwrap()
}

fn errno(reply: &[u8]) -> i32 {
    i32::from_le_bytes(reply[4..8].try_into().unwrap())
}

struct BarrierHandle {
    inner: Arc<dyn FileHandle>,
    sync_calls: Arc<AtomicUsize>,
    fail_sync: Arc<AtomicBool>,
}

#[async_trait]
impl FileHandle for BarrierHandle {
    fn fd(&self) -> Option<u64> {
        self.inner.fd()
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        self.inner.read(buffer, position).await
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        self.inner.write(buffer, position).await
    }

    async fn stat(&self) -> Result<Stats> {
        self.inner.stat().await
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        self.inner.truncate(length).await
    }

    async fn sync(&self) -> Result<()> {
        self.sync_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_sync.swap(false, Ordering::SeqCst) {
            return Err(FsError::backend("injected handle sync failure"));
        }
        self.inner.sync().await
    }

    async fn datasync(&self) -> Result<()> {
        self.inner.datasync().await
    }

    async fn close(&self) -> Result<()> {
        self.inner.close().await
    }
}

struct BarrierDriver {
    inner: MemoryFs,
    durable: bool,
    syncfs_calls: AtomicUsize,
    fail_syncfs: AtomicBool,
    handle_sync_calls: Arc<AtomicUsize>,
    fail_handle_sync: Arc<AtomicBool>,
}

impl BarrierDriver {
    fn new(durable: bool) -> Self {
        Self {
            inner: MemoryFs::empty(),
            durable,
            syncfs_calls: AtomicUsize::new(0),
            fail_syncfs: AtomicBool::new(false),
            handle_sync_calls: Arc::new(AtomicUsize::new(0)),
            fail_handle_sync: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn with_file(durable: bool) -> Arc<Self> {
        let driver = Arc::new(Self::new(durable));
        let handle = driver.inner.open("/file", "w", 0o644).await.unwrap();
        handle.close().await.unwrap();
        driver
    }

    fn wrapped(&self, handle: Arc<dyn FileHandle>) -> Arc<dyn FileHandle> {
        Arc::new(BarrierHandle {
            inner: handle,
            sync_calls: Arc::clone(&self.handle_sync_calls),
            fail_sync: Arc::clone(&self.fail_handle_sync),
        })
    }
}

#[async_trait]
impl FsDriver for BarrierDriver {
    fn capabilities(&self) -> Capabilities {
        let mut capabilities = self.inner.capabilities();
        capabilities.durable_writes = self.durable;
        capabilities
    }

    async fn syncfs(&self) -> Result<()> {
        self.syncfs_calls.fetch_add(1, Ordering::SeqCst);
        if self.fail_syncfs.swap(false, Ordering::SeqCst) {
            return Err(FsError::backend("injected filesystem sync failure"));
        }
        Ok(())
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.inner.stat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<mount_rs_core::DirEntry>> {
        self.inner.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        Ok(self.wrapped(self.inner.open(path, flags, mode).await?))
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        Ok(self.wrapped(self.inner.open_flags(path, flags, mode).await?))
    }
}

struct DurableDefault;

#[async_trait]
impl FsDriver for DurableDefault {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            durable_writes: true,
            ..Capabilities::default()
        }
    }

    async fn stat(&self, _path: &str) -> Result<Stats> {
        Err(FsError::enosys("stat"))
    }

    async fn readdir(&self, _path: &str) -> Result<Vec<mount_rs_core::DirEntry>> {
        Err(FsError::enosys("readdir"))
    }

    async fn open(&self, _path: &str, _flags: &str, _mode: u32) -> Result<Arc<dyn FileHandle>> {
        Err(FsError::enosys("open"))
    }
}

#[tokio::test]
async fn default_syncfs_rejects_unimplemented_durable_barrier() {
    let error = DurableDefault.syncfs().await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Enosys);
    assert_eq!(error.syscall.as_deref(), Some("syncfs"));

    // MemoryFs is volatile, so the same default is a successful no-op.
    MemoryFs::empty().syncfs().await.unwrap();
}

#[tokio::test]
async fn syncfs_dispatches_driver_barrier_and_propagates_errors() {
    let driver = Arc::new(BarrierDriver::new(false));
    let mut session = FuseSession::new(Arc::clone(&driver) as Arc<dyn FsDriver>);
    let body = [0u8; 8];

    success(&raw_request(&mut session, FUSE_SYNCFS, 1, &body).await);
    assert_eq!(driver.syncfs_calls.load(Ordering::SeqCst), 1);

    driver.fail_syncfs.store(true, Ordering::SeqCst);
    let reply = raw_request(&mut session, FUSE_SYNCFS, 1, &body).await;
    assert_eq!(errno(&reply), -ErrorCode::Eio.errno());
    assert_eq!(driver.syncfs_calls.load(Ordering::SeqCst), 2);

    let malformed = raw_request(&mut session, FUSE_SYNCFS, 1, &[0; 7]).await;
    assert_eq!(errno(&malformed), -ErrorCode::Einval.errno());
    let mut trailing = body.to_vec();
    trailing.push(0);
    let malformed = raw_request(&mut session, FUSE_SYNCFS, 1, &trailing).await;
    assert_eq!(errno(&malformed), -ErrorCode::Einval.errno());
    assert_eq!(driver.syncfs_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn fsyncdir_validates_directory_then_calls_syncfs_and_propagates_errors() {
    let driver = BarrierDriver::with_file(false).await;
    let mut session = FuseSession::new(Arc::clone(&driver) as Arc<dyn FsDriver>);
    let opened_reply = raw_request(&mut session, 27, 1, &[0; 8]).await;
    let opened = success(&opened_reply);
    let directory_handle = number(opened, 0);

    let mut syncdir = vec![0; 16];
    syncdir[..8].copy_from_slice(&directory_handle.to_le_bytes());
    success(&raw_request(&mut session, 30, 1, &syncdir).await);
    assert_eq!(driver.syncfs_calls.load(Ordering::SeqCst), 1);

    driver.fail_syncfs.store(true, Ordering::SeqCst);
    let reply = raw_request(&mut session, 30, 1, &syncdir).await;
    assert_eq!(errno(&reply), -ErrorCode::Eio.errno());
    assert_eq!(driver.syncfs_calls.load(Ordering::SeqCst), 2);

    let mut invalid = vec![0; 16];
    invalid[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    let reply = raw_request(&mut session, 30, 1, &invalid).await;
    assert_eq!(errno(&reply), -ErrorCode::Ebadf.errno());
    assert_eq!(driver.syncfs_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn flush_validates_handle_syncs_durable_files_and_is_volatile_noop() {
    let driver = BarrierDriver::with_file(true).await;
    let mut session = FuseSession::new(Arc::clone(&driver) as Arc<dyn FsDriver>);
    let entry_reply = raw_request(&mut session, 1, 1, b"file\0").await;
    let entry = success(&entry_reply);
    let inode = number(entry, 0);
    let opened_reply = raw_request(&mut session, 14, inode, &[2, 0, 0, 0, 0, 0, 0, 0]).await;
    let opened = success(&opened_reply);
    let file_handle = number(opened, 0);

    let mut flush = vec![0; 24];
    flush[..8].copy_from_slice(&file_handle.to_le_bytes());
    success(&raw_request(&mut session, 25, inode, &flush).await);
    assert_eq!(driver.handle_sync_calls.load(Ordering::SeqCst), 1);

    driver.fail_handle_sync.store(true, Ordering::SeqCst);
    let reply = raw_request(&mut session, 25, inode, &flush).await;
    assert_eq!(errno(&reply), -ErrorCode::Eio.errno());
    assert_eq!(driver.handle_sync_calls.load(Ordering::SeqCst), 2);

    let mut invalid = vec![0; 24];
    invalid[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    let reply = raw_request(&mut session, 25, inode, &invalid).await;
    assert_eq!(errno(&reply), -ErrorCode::Ebadf.errno());
    assert_eq!(driver.handle_sync_calls.load(Ordering::SeqCst), 2);

    // The upstream oracle can deliberately return ENOSYS for durable FLUSH;
    // this Rust contract extension instead makes it a per-handle sync point.
    let volatile = BarrierDriver::with_file(false).await;
    let mut session = FuseSession::new(Arc::clone(&volatile) as Arc<dyn FsDriver>);
    let entry_reply = raw_request(&mut session, 1, 1, b"file\0").await;
    let entry = success(&entry_reply);
    let inode = number(entry, 0);
    let opened_reply = raw_request(&mut session, 14, inode, &[2, 0, 0, 0, 0, 0, 0, 0]).await;
    let opened = success(&opened_reply);
    let file_handle = number(opened, 0);
    flush[..8].copy_from_slice(&file_handle.to_le_bytes());
    success(&raw_request(&mut session, 25, inode, &flush).await);
    assert_eq!(volatile.handle_sync_calls.load(Ordering::SeqCst), 0);
}
