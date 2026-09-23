use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, FileHandle, FsDriver, GuardedMutation, GuardedMutationResult,
    GuardedRead, GuardedReadResult, MkdirOptions, OpenFlags, Result, Stats,
};
use mount_rs_memfs::MemoryFs;
use mount_rs_nfs::constants::{
    CREATE_UNCHECKED, FILE_SYNC, MNT3ERR_NOTSUPP, MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_MNT,
    NFS_PROGRAM, NFS_V3, NFS3_OK, NFS3ERR_BAD_COOKIE, NFS3ERR_EXIST, NFS3ERR_INVAL,
    NFS3ERR_NOTSUPP, NFS3ERR_STALE, NFSPROC3_CREATE, NFSPROC3_GETATTR, NFSPROC3_LOOKUP,
    NFSPROC3_MKDIR, NFSPROC3_READ, NFSPROC3_READDIR, NFSPROC3_READDIRPLUS, NFSPROC3_READLINK,
    NFSPROC3_REMOVE, NFSPROC3_RENAME, NFSPROC3_SETATTR, NFSPROC3_WRITE,
};
use mount_rs_nfs::handles::{FileHandleTable, FileHandleTableOptions};
use mount_rs_nfs::protocol::{
    Create3args, DirOpArgs, Mkdir3args, Read3args, Readdir3args, Readdir3res, Readdirplus3args,
    Rename3args, Sattr3, Setattr3args, WccData, Write3args, read_create_res, read_getattr_res,
    read_lookup_res, read_mount_res, read_read_res, read_readdir_res, read_readdirplus_res,
    read_readlink_res, read_rename_res, read_wcc_res, read_write_res, write_create_args,
    write_dir_op, write_mkdir_args, write_read_args, write_readdir_args, write_readdirplus_args,
    write_rename_args, write_setattr_args, write_write_args,
};
use mount_rs_nfs::rpc::{
    MSG_ACCEPTED, RPC_SUCCESS, RPC_SYSTEM_ERR, decode_reply, encode_call, frame_record,
};
use mount_rs_nfs::v4::{NFSPROC4_COMPOUND, NFSPROC4_NULL};
use mount_rs_nfs::xdr::encode_xdr;
use mount_rs_nfs::{
    NFS_V4, NFS4_PROGRAM, Nfs3Session, Nfs4Session, NfsRequestContext, NfsServer, NfsServerOptions,
    NfsSessionOptions,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;

async fn exchange(stream: &mut TcpStream, call: Vec<u8>) -> Vec<u8> {
    stream
        .write_all(&frame_record(&call).unwrap())
        .await
        .unwrap();

    let mut marker = [0_u8; 4];
    stream.read_exact(&mut marker).await.unwrap();
    let marker = u32::from_be_bytes(marker);
    assert_ne!(marker & 0x8000_0000, 0, "test server returned fragments");
    let length = (marker & 0x7fff_ffff) as usize;
    let mut record = vec![0_u8; length];
    stream.read_exact(&mut record).await.unwrap();
    record
}

async fn readdir_page(
    stream: &mut TcpStream,
    xid: u32,
    dir: Vec<u8>,
    cookie: u64,
    cookieverf: Vec<u8>,
    count: u32,
) -> Readdir3res {
    let record = exchange(
        stream,
        encode_call(
            xid,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_READDIR,
            None,
            None,
            &encode_xdr(|writer| {
                write_readdir_args(
                    writer,
                    &Readdir3args {
                        dir,
                        cookie,
                        cookieverf,
                        count,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let result = read_readdir_res(&mut body).unwrap();
    body.end("READDIR page response").unwrap();
    result
}

#[derive(Clone)]
struct ReplaceBeforeWriteOpen {
    backing: MemoryFs,
    replace_once: Arc<AtomicBool>,
}

#[async_trait]
impl FsDriver for ReplaceBeforeWriteOpen {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_mutations(&self) -> bool {
        self.backing.supports_guarded_mutations()
    }

    async fn guarded_mutation(&self, request: GuardedMutation) -> Result<GuardedMutationResult> {
        self.backing.guarded_mutation(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    // Deliberately use the trait's ENOSYS lstat default. Shared handle
    // validation must still work for a stat-only driver.

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        if flags.write && !flags.create && self.replace_once.swap(false, Ordering::AcqRel) {
            self.backing.unlink(path).await?;
            self.backing.open(path, "w", 0o644).await?.close().await?;
        }
        self.backing.open_flags(path, flags, mode).await
    }
}

#[derive(Clone)]
struct PauseFirstReadOpen {
    backing: MemoryFs,
    pause_once: Arc<AtomicBool>,
    started: Arc<Notify>,
    release: Arc<Notify>,
}

#[async_trait]
impl FsDriver for PauseFirstReadOpen {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.backing.supports_guarded_reads()
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        self.backing.guarded_read(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.backing.lstat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let handle = self.backing.open_flags(path, flags, mode).await?;
        if path == "/race"
            && flags.read
            && !flags.write
            && self.pause_once.swap(false, Ordering::AcqRel)
        {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(handle)
    }
}

#[derive(Clone)]
struct ReplaceBeforeRetainOpen {
    backing: MemoryFs,
    replace_once: Arc<AtomicBool>,
}

#[async_trait]
impl FsDriver for ReplaceBeforeRetainOpen {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.backing.supports_guarded_reads()
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        self.backing.guarded_read(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.backing.lstat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        if path == "/listed"
            && flags.read
            && !flags.write
            && self.replace_once.swap(false, Ordering::AcqRel)
        {
            self.backing.unlink(path).await?;
            let replacement = self.backing.open(path, "w", 0o644).await?;
            replacement.write(b"replacement bytes", Some(0)).await?;
            replacement.close().await?;
        }
        self.backing.open_flags(path, flags, mode).await
    }
}

struct StatBarrierHandle {
    inner: Arc<dyn FileHandle>,
    stat_started: Arc<Notify>,
    stat_release: Arc<Notify>,
    pause_once: Arc<AtomicBool>,
    close_calls: Arc<AtomicUsize>,
    closed: Arc<Notify>,
    close_barrier: Option<CloseBarrier>,
}

#[derive(Clone)]
struct CloseBarrier {
    started: Arc<Notify>,
    release: Arc<Notify>,
    pause_once: Arc<AtomicBool>,
}

#[async_trait]
impl FileHandle for StatBarrierHandle {
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
        if self.pause_once.swap(false, Ordering::AcqRel) {
            self.stat_started.notify_one();
            self.stat_release.notified().await;
        }
        self.inner.stat().await
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        self.inner.truncate(length).await
    }

    async fn close(&self) -> Result<()> {
        if let Some(barrier) = &self.close_barrier
            && barrier.pause_once.swap(false, Ordering::AcqRel)
        {
            barrier.started.notify_one();
            barrier.release.notified().await;
        }
        let result = self.inner.close().await;
        self.close_calls.fetch_add(1, Ordering::AcqRel);
        self.closed.notify_one();
        result
    }
}

#[derive(Clone)]
struct StatBarrierDriver {
    backing: MemoryFs,
    stat_started: Arc<Notify>,
    stat_release: Arc<Notify>,
    pause_once: Arc<AtomicBool>,
    close_calls: Arc<AtomicUsize>,
    closed: Arc<Notify>,
    close_barrier: Option<CloseBarrier>,
}

#[async_trait]
impl FsDriver for StatBarrierDriver {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.backing.supports_guarded_reads()
    }

    fn stable_inode_ids(&self) -> bool {
        self.backing.stable_inode_ids()
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        self.backing.guarded_read(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.backing.lstat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let inner = self.backing.open_flags(path, flags, mode).await?;
        if path == "/cancel" && flags.read && !flags.write {
            Ok(Arc::new(StatBarrierHandle {
                inner,
                stat_started: self.stat_started.clone(),
                stat_release: self.stat_release.clone(),
                pause_once: self.pause_once.clone(),
                close_calls: self.close_calls.clone(),
                closed: self.closed.clone(),
                close_barrier: self.close_barrier.clone(),
            }))
        } else {
            Ok(inner)
        }
    }
}

struct BlockingCloseHandle {
    inner: Arc<dyn FileHandle>,
    pause_first_close: Arc<AtomicBool>,
    close_started: Arc<Notify>,
    close_release: Arc<Notify>,
    completed_closes: Arc<AtomicUsize>,
}

#[async_trait]
impl FileHandle for BlockingCloseHandle {
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

    async fn close(&self) -> Result<()> {
        if self.pause_first_close.swap(false, Ordering::AcqRel) {
            self.close_started.notify_one();
            self.close_release.notified().await;
        }
        let result = self.inner.close().await;
        self.completed_closes.fetch_add(1, Ordering::AcqRel);
        result
    }
}

#[derive(Clone)]
struct BlockingCloseDriver {
    backing: MemoryFs,
    pause_first_close: Arc<AtomicBool>,
    close_started: Arc<Notify>,
    close_release: Arc<Notify>,
    first_completed: Arc<AtomicUsize>,
    second_completed: Arc<AtomicUsize>,
}

#[async_trait]
impl FsDriver for BlockingCloseDriver {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.backing.supports_guarded_reads()
    }

    fn stable_inode_ids(&self) -> bool {
        self.backing.stable_inode_ids()
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        self.backing.guarded_read(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.backing.lstat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let inner = self.backing.open_flags(path, flags, mode).await?;
        let completed_closes = match path {
            "/first" => Some(self.first_completed.clone()),
            "/second" => Some(self.second_completed.clone()),
            _ => None,
        };
        if flags.read
            && !flags.write
            && let Some(completed_closes) = completed_closes
        {
            return Ok(Arc::new(BlockingCloseHandle {
                inner,
                pause_first_close: self.pause_first_close.clone(),
                close_started: self.close_started.clone(),
                close_release: self.close_release.clone(),
                completed_closes,
            }));
        }
        Ok(inner)
    }
}

struct ErrOnceCloseHandle {
    inner: Arc<dyn FileHandle>,
    fail_once: Arc<AtomicBool>,
    close_attempts: Arc<AtomicUsize>,
    successful_closes: Arc<AtomicUsize>,
}

#[async_trait]
impl FileHandle for ErrOnceCloseHandle {
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

    async fn close(&self) -> Result<()> {
        self.close_attempts.fetch_add(1, Ordering::AcqRel);
        if self.fail_once.swap(false, Ordering::AcqRel) {
            return Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio)
                .with_message("injected first backend close failure"));
        }
        self.inner.close().await?;
        self.successful_closes.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[derive(Clone)]
struct ErrOnceCloseDriver {
    backing: MemoryFs,
    fail_once: Arc<AtomicBool>,
    close_attempts: Arc<AtomicUsize>,
    successful_closes: Arc<AtomicUsize>,
}

#[async_trait]
impl FsDriver for ErrOnceCloseDriver {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.backing.supports_guarded_reads()
    }

    fn stable_inode_ids(&self) -> bool {
        self.backing.stable_inode_ids()
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        self.backing.guarded_read(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.backing.lstat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let inner = self.backing.open_flags(path, flags, mode).await?;
        if path == "/retry" && flags.read && !flags.write {
            Ok(Arc::new(ErrOnceCloseHandle {
                inner,
                fail_once: self.fail_once.clone(),
                close_attempts: self.close_attempts.clone(),
                successful_closes: self.successful_closes.clone(),
            }))
        } else {
            Ok(inner)
        }
    }
}

#[derive(Clone)]
struct SwapAfterGetattrAliasStat {
    backing: MemoryFs,
    swap_once: Arc<AtomicBool>,
}

#[async_trait]
impl FsDriver for SwapAfterGetattrAliasStat {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.backing.supports_guarded_reads()
    }

    fn stable_inode_ids(&self) -> bool {
        self.backing.stable_inode_ids()
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        self.backing.guarded_read(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        let stats = self.backing.lstat(path).await?;
        if path == "/getattr" && self.swap_once.swap(false, Ordering::AcqRel) {
            // Return A's observed alias stat to GETATTR after the peer has
            // moved A and put B at that path. Its guarded stat must fail and
            // the retained A descriptor must supply the response.
            self.backing.rename("/getattr", "/moved").await?;
            let replacement = self.backing.open("/getattr", "w", 0o644).await?;
            replacement.write(b"replacement bytes", Some(0)).await?;
            replacement.close().await?;
        }
        Ok(stats)
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        self.backing.open_flags(path, flags, mode).await
    }
}

#[derive(Clone)]
struct ZeroInodeStat {
    backing: MemoryFs,
}

#[async_trait]
impl FsDriver for ZeroInodeStat {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.backing.supports_guarded_reads()
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        let zero = matches!(
            &request,
            GuardedRead::Lookup { parent, name } if parent.path == "/" && name == "zero"
        );
        let mut result = self.backing.guarded_read(request).await?;
        if zero && let GuardedReadResult::Lookup { child, .. } = &mut result {
            child.ino = 0;
        }
        Ok(result)
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        let mut stats = self.backing.stat(path).await?;
        if path == "/zero" {
            stats.ino = 0;
        }
        Ok(stats)
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        let mut stats = self.backing.lstat(path).await?;
        if path == "/zero" {
            stats.ino = 0;
        }
        Ok(stats)
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadSwapKind {
    Lookup,
    Readdir,
    Readlink,
}

#[derive(Clone)]
struct SwapBeforeRead {
    backing: MemoryFs,
    kind: ReadSwapKind,
    swap_once: Arc<AtomicBool>,
}

impl SwapBeforeRead {
    async fn swap_if(&self, kind: ReadSwapKind) -> Result<()> {
        if self.kind != kind || !self.swap_once.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        if kind == ReadSwapKind::Readlink {
            self.backing.rename("/link", "/original-link").await?;
            self.backing.symlink("/new-target", "/link").await?;
        } else {
            self.backing.rename("/parent", "/original-parent").await?;
            self.backing
                .mkdir(
                    "/parent",
                    MkdirOptions {
                        recursive: false,
                        mode: Some(0o755),
                    },
                )
                .await?;
            self.backing
                .open("/parent/b-child", "w", 0o644)
                .await?
                .close()
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl FsDriver for SwapBeforeRead {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.backing.supports_guarded_reads()
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        match &request {
            GuardedRead::Lookup { parent, name }
                if parent.path == "/parent" && name == "b-child" =>
            {
                self.swap_if(ReadSwapKind::Lookup).await?;
            }
            GuardedRead::Readdir { directory, .. } if directory.path == "/parent" => {
                self.swap_if(ReadSwapKind::Readdir).await?;
            }
            GuardedRead::Readlink { target } if target.path == "/link" => {
                self.swap_if(ReadSwapKind::Readlink).await?;
            }
            _ => {}
        }
        self.backing.guarded_read(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        if path == "/parent/b-child" {
            self.swap_if(ReadSwapKind::Lookup).await?;
        }
        self.backing.lstat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        if path == "/parent" {
            self.swap_if(ReadSwapKind::Readdir).await?;
        }
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }

    async fn readlink(&self, path: &str) -> Result<String> {
        if path == "/link" {
            self.swap_if(ReadSwapKind::Readlink).await?;
        }
        self.backing.readlink(path).await
    }
}

async fn wrong_object_read(kind: ReadSwapKind) -> (u32, bool, bool) {
    let backing = MemoryFs::empty();
    if kind == ReadSwapKind::Readlink {
        backing.symlink("/old-target", "/link").await.unwrap();
    } else {
        backing
            .mkdir(
                "/parent",
                MkdirOptions {
                    recursive: false,
                    mode: Some(0o755),
                },
            )
            .await
            .unwrap();
        backing
            .open("/parent/a-child", "w", 0o644)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }
    let swap_once = Arc::new(AtomicBool::new(true));
    let driver = SwapBeforeRead {
        backing: backing.clone(),
        kind,
        swap_once: swap_once.clone(),
    };
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let name = if kind == ReadSwapKind::Readlink {
        "link"
    } else {
        "parent"
    };
    let handle = lookup_handle(&mut stream, 2, root, name).await;

    let (status, got_replacement) = match kind {
        ReadSwapKind::Lookup => {
            let record = exchange(
                &mut stream,
                encode_call(
                    3,
                    NFS_PROGRAM,
                    NFS_V3,
                    NFSPROC3_LOOKUP,
                    None,
                    None,
                    &encode_xdr(|writer| {
                        write_dir_op(
                            writer,
                            &DirOpArgs {
                                dir: handle,
                                name: "b-child".to_owned(),
                            },
                        )
                    }),
                ),
            )
            .await;
            let (_, mut body) = decode_reply(&record).unwrap();
            let found = read_lookup_res(&mut body).unwrap();
            body.end("LOOKUP after parent replacement").unwrap();
            (found.status, found.object.is_some())
        }
        ReadSwapKind::Readdir => {
            let record = exchange(
                &mut stream,
                encode_call(
                    3,
                    NFS_PROGRAM,
                    NFS_V3,
                    NFSPROC3_READDIR,
                    None,
                    None,
                    &encode_xdr(|writer| {
                        write_readdir_args(
                            writer,
                            &Readdir3args {
                                dir: handle,
                                cookie: 0,
                                cookieverf: vec![0; 8],
                                count: 4096,
                            },
                        )
                    }),
                ),
            )
            .await;
            let (_, mut body) = decode_reply(&record).unwrap();
            let listed = read_readdir_res(&mut body).unwrap();
            body.end("READDIR after parent replacement").unwrap();
            (
                listed.status,
                listed.entries.iter().any(|entry| entry.name == "b-child"),
            )
        }
        ReadSwapKind::Readlink => {
            let record = exchange(
                &mut stream,
                encode_call(
                    3,
                    NFS_PROGRAM,
                    NFS_V3,
                    NFSPROC3_READLINK,
                    None,
                    None,
                    &encode_xdr(|writer| writer.var_opaque(&handle)),
                ),
            )
            .await;
            let (_, mut body) = decode_reply(&record).unwrap();
            let link = read_readlink_res(&mut body).unwrap();
            body.end("READLINK after link replacement").unwrap();
            (link.status, link.target.as_deref() == Some("/new-target"))
        }
    };
    let swapped = !swap_once.load(Ordering::Acquire);
    if swapped {
        if kind == ReadSwapKind::Readlink {
            assert_eq!(
                backing.readlink("/original-link").await.unwrap(),
                "/old-target"
            );
            assert_eq!(backing.readlink("/link").await.unwrap(), "/new-target");
        } else {
            assert!(backing.stat("/original-parent/a-child").await.is_ok());
            assert!(backing.stat("/parent/b-child").await.is_ok());
        }
    }
    server.close().await.unwrap();
    (status, got_replacement, swapped)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_lookup_rejects_parent_replacement_before_child_stat() {
    let (status, got_replacement, swapped) = wrong_object_read(ReadSwapKind::Lookup).await;
    assert!(swapped, "fixture must replace the parent at read boundary");
    assert_eq!(status, NFS3ERR_STALE);
    assert!(!got_replacement, "LOOKUP must not return B's child handle");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_readdir_rejects_parent_replacement_before_listing() {
    let (status, got_replacement, swapped) = wrong_object_read(ReadSwapKind::Readdir).await;
    assert!(swapped, "fixture must replace the parent at read boundary");
    assert_eq!(status, NFS3ERR_STALE);
    assert!(!got_replacement, "READDIR must not return B's entries");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_readlink_rejects_link_replacement_before_read() {
    let (status, got_replacement, swapped) = wrong_object_read(ReadSwapKind::Readlink).await;
    assert!(swapped, "fixture must replace the link at read boundary");
    assert_eq!(status, NFS3ERR_STALE);
    assert!(!got_replacement, "READLINK must not return B's target");
}

#[derive(Clone)]
struct MutationOnlyReadDriver {
    backing: MemoryFs,
    path_lookup_calls: Arc<AtomicUsize>,
    path_readdir_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl FsDriver for MutationOnlyReadDriver {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_mutations(&self) -> bool {
        true
    }

    async fn guarded_mutation(&self, request: GuardedMutation) -> Result<GuardedMutationResult> {
        self.backing.guarded_mutation(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        if path != "/" {
            self.path_lookup_calls.fetch_add(1, Ordering::AcqRel);
        }
        self.backing.lstat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.path_readdir_calls.fetch_add(1, Ordering::AcqRel);
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_reads_fail_closed_for_mutation_only_driver() {
    let backing = MemoryFs::empty();
    backing
        .open("/visible", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let path_lookup_calls = Arc::new(AtomicUsize::new(0));
    let path_readdir_calls = Arc::new(AtomicUsize::new(0));
    let driver = MutationOnlyReadDriver {
        backing,
        path_lookup_calls: path_lookup_calls.clone(),
        path_readdir_calls: path_readdir_calls.clone(),
    };
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;

    let lookup = exchange(
        &mut stream,
        encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_LOOKUP,
            None,
            None,
            &encode_xdr(|writer| {
                write_dir_op(
                    writer,
                    &DirOpArgs {
                        dir: root.clone(),
                        name: "visible".to_owned(),
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&lookup).unwrap();
    let lookup = read_lookup_res(&mut body).unwrap();
    body.end("mutation-only LOOKUP response").unwrap();

    let readdir = exchange(
        &mut stream,
        encode_call(
            3,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_READDIR,
            None,
            None,
            &encode_xdr(|writer| {
                write_readdir_args(
                    writer,
                    &Readdir3args {
                        dir: root,
                        cookie: 0,
                        cookieverf: vec![0; 8],
                        count: 4096,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&readdir).unwrap();
    let readdir = read_readdir_res(&mut body).unwrap();
    body.end("mutation-only READDIR response").unwrap();

    assert_eq!([lookup.status, readdir.status], [NFS3ERR_NOTSUPP; 2]);
    assert!(lookup.object.is_none());
    assert!(readdir.entries.is_empty());
    assert_eq!(path_lookup_calls.load(Ordering::Acquire), 0);
    assert_eq!(path_readdir_calls.load(Ordering::Acquire), 0);
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_readdir_cookie_page_rejects_replaced_directory_with_same_names() {
    let backing = MemoryFs::empty();
    backing
        .mkdir(
            "/page-dir",
            MkdirOptions {
                recursive: false,
                mode: Some(0o755),
            },
        )
        .await
        .unwrap();
    for name in ["same-a", "same-b"] {
        backing
            .open(&format!("/page-dir/{name}"), "w", 0o644)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }
    let old_inode = backing.stat("/page-dir").await.unwrap().ino;
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(backing.clone(), options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let directory = lookup_handle(&mut stream, 2, root, "page-dir").await;
    let first = readdir_page(&mut stream, 3, directory.clone(), 0, vec![0; 8], 160).await;
    assert_eq!(first.status, NFS3_OK);
    assert_eq!(first.entries.len(), 1, "first page must supply a cookie");
    assert!(!first.eof);
    let cookie = first.entries[0].cookie;
    assert_ne!(cookie, 0);
    let verifier = first.cookieverf;

    backing
        .rename("/page-dir", "/original-page-dir")
        .await
        .unwrap();
    backing
        .mkdir(
            "/page-dir",
            MkdirOptions {
                recursive: false,
                mode: Some(0o755),
            },
        )
        .await
        .unwrap();
    for name in ["same-a", "same-b"] {
        backing
            .open(&format!("/page-dir/{name}"), "w", 0o644)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }
    assert_ne!(backing.stat("/page-dir").await.unwrap().ino, old_inode);
    let old_names = backing
        .readdir("/original-page-dir")
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    let new_names = backing
        .readdir("/page-dir")
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(
        old_names, new_names,
        "names alone cannot detect replacement"
    );

    let second = readdir_page(&mut stream, 4, directory, cookie, verifier, 4096).await;
    assert!(
        matches!(second.status, NFS3ERR_STALE | NFS3ERR_BAD_COOKIE),
        "old directory FH must not accept a replacement's next page: {}",
        second.status
    );
    assert!(second.entries.is_empty());
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_readdir_cookie_rejects_replaced_child_with_same_name() {
    let backing = MemoryFs::empty();
    backing
        .mkdir(
            "/page-dir",
            MkdirOptions {
                recursive: false,
                mode: Some(0o755),
            },
        )
        .await
        .unwrap();
    for name in ["same-a", "same-b"] {
        backing
            .open(&format!("/page-dir/{name}"), "w", 0o644)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }
    let parent_inode = backing.stat("/page-dir").await.unwrap().ino;
    let child_inode = backing.stat("/page-dir/same-b").await.unwrap().ino;
    let old_names = backing
        .readdir("/page-dir")
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(backing.clone(), options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let directory = lookup_handle(&mut stream, 2, root, "page-dir").await;
    let first = readdir_page(&mut stream, 3, directory.clone(), 0, vec![0; 8], 160).await;
    assert_eq!(first.status, NFS3_OK);
    assert_eq!(first.entries.len(), 1);
    assert!(!first.eof);
    let cookie = first.entries[0].cookie;
    assert_ne!(cookie, 0);
    let verifier = first.cookieverf;

    backing.unlink("/page-dir/same-b").await.unwrap();
    backing
        .open("/page-dir/same-b", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    assert_eq!(backing.stat("/page-dir").await.unwrap().ino, parent_inode);
    assert_ne!(
        backing.stat("/page-dir/same-b").await.unwrap().ino,
        child_inode
    );
    let new_names = backing
        .readdir("/page-dir")
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(new_names, old_names);

    let second = readdir_page(&mut stream, 4, directory, cookie, verifier, 4096).await;
    assert_eq!(
        second.status, NFS3ERR_BAD_COOKIE,
        "same names with a new child inode must invalidate the old cookie"
    );
    assert!(second.entries.is_empty());
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_readdirplus_old_handle_survives_remote_unlink_and_rebind() {
    let backing = MemoryFs::empty();
    let original = backing.open("/listed", "w", 0o644).await.unwrap();
    original.write(b"original bytes", Some(0)).await.unwrap();
    original.close().await.unwrap();
    let old_inode = backing.stat("/listed").await.unwrap().ino;
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(backing.clone(), options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;

    // READDIRPLUS is the only wire operation to issue the original file FH.
    let record = exchange(
        &mut stream,
        encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_READDIRPLUS,
            None,
            None,
            &encode_xdr(|writer| {
                write_readdirplus_args(
                    writer,
                    &Readdirplus3args {
                        dir: root.clone(),
                        cookie: 0,
                        cookieverf: vec![0; 8],
                        dircount: 4096,
                        maxcount: 8192,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let listing = read_readdirplus_res(&mut body).unwrap();
    body.end("READDIRPLUS original file response").unwrap();
    assert_eq!(listing.status, NFS3_OK);
    let entry = listing
        .entries
        .into_iter()
        .find(|entry| entry.name == "listed")
        .expect("file must appear in READDIRPLUS");
    let old_attrs = entry.attributes.expect("PLUS must describe file");
    assert_eq!(old_attrs.fileid, entry.fileid);
    assert_eq!(old_attrs.size, b"original bytes".len() as u64);
    let old_handle = entry.handle.expect("PLUS must issue file FH");

    // A peer replaces the name, then a new LOOKUP forces the local handle
    // table to rebind that path to the replacement inode.
    backing.unlink("/listed").await.unwrap();
    let replacement = backing.open("/listed", "w", 0o644).await.unwrap();
    replacement
        .write(b"replacement bytes", Some(0))
        .await
        .unwrap();
    replacement.close().await.unwrap();
    assert_ne!(backing.stat("/listed").await.unwrap().ino, old_inode);
    let new_handle = lookup_handle(&mut stream, 3, root, "listed").await;
    assert_ne!(new_handle, old_handle);

    let record = exchange(
        &mut stream,
        encode_call(
            4,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_READ,
            None,
            None,
            &encode_xdr(|writer| {
                write_read_args(
                    writer,
                    &Read3args {
                        file: old_handle.clone(),
                        offset: 0,
                        count: 64,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let old_read = read_read_res(&mut body, 64).unwrap();
    body.end("old PLUS FH READ after replacement").unwrap();
    assert_eq!(old_read.status, NFS3_OK);
    assert_eq!(old_read.data, b"original bytes");
    let read_attrs = old_read.attributes.expect("old FH READ attributes");
    assert_eq!(read_attrs.fileid, old_attrs.fileid);
    assert_eq!(read_attrs.size, old_attrs.size);

    let record = exchange(
        &mut stream,
        encode_call(
            5,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_GETATTR,
            None,
            None,
            &encode_xdr(|writer| writer.var_opaque(&old_handle)),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let getattr = read_getattr_res(&mut body).unwrap();
    body.end("old PLUS FH GETATTR after replacement").unwrap();
    assert_eq!(getattr.status, NFS3_OK);
    let attr = getattr.attributes.expect("old FH GETATTR attributes");
    assert_eq!(attr.fileid, old_attrs.fileid);
    assert_eq!(attr.size, old_attrs.size);
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_readdirplus_snapshot_child_replacement_omits_unsafe_handle() {
    let backing = MemoryFs::empty();
    let original = backing.open("/listed", "w", 0o644).await.unwrap();
    original.write(b"original bytes", Some(0)).await.unwrap();
    original.close().await.unwrap();
    let old_inode = backing.stat("/listed").await.unwrap().ino;
    let replace_once = Arc::new(AtomicBool::new(true));
    let driver = ReplaceBeforeRetainOpen {
        backing: backing.clone(),
        replace_once: replace_once.clone(),
    };
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;

    // The guarded directory page snapshots A. Opening its read-only
    // descriptor swaps the child path to B before handle retention can finish.
    let record = exchange(
        &mut stream,
        encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_READDIRPLUS,
            None,
            None,
            &encode_xdr(|writer| {
                write_readdirplus_args(
                    writer,
                    &Readdirplus3args {
                        dir: root,
                        cookie: 0,
                        cookieverf: vec![0; 8],
                        dircount: 4096,
                        maxcount: 8192,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let listing = read_readdirplus_res(&mut body).unwrap();
    body.end("READDIRPLUS child replaced during descriptor open")
        .unwrap();
    assert_eq!(listing.status, NFS3_OK);
    assert!(!replace_once.load(Ordering::Acquire));
    let replacement_inode = backing.stat("/listed").await.unwrap().ino;
    assert_ne!(replacement_inode, old_inode);
    let entry = listing
        .entries
        .into_iter()
        .find(|entry| entry.name == "listed")
        .expect("snapshot child must remain in READDIRPLUS");
    assert_eq!(entry.fileid, old_inode);
    let attrs = entry.attributes.expect("snapshot attributes must survive");
    assert_eq!(attrs.fileid, entry.fileid);
    assert_eq!(attrs.size, b"original bytes".len() as u64);
    assert!(
        entry.handle.is_none(),
        "an unchecked replacement handle must not be issued for A"
    );
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_lookup_abort_during_open_identity_check_closes_provisional_handle() {
    let backing = MemoryFs::empty();
    backing
        .open("/cancel", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let stat_started = Arc::new(Notify::new());
    let stat_release = Arc::new(Notify::new());
    let pause_once = Arc::new(AtomicBool::new(true));
    let close_calls = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(Notify::new());
    let driver = StatBarrierDriver {
        backing,
        stat_started: stat_started.clone(),
        stat_release: stat_release.clone(),
        pause_once: pause_once.clone(),
        close_calls: close_calls.clone(),
        closed: closed.clone(),
        close_barrier: None,
    };
    let options = NfsSessionOptions {
        shared_concurrent_view: true,
        omit_wcc_attributes: true,
        ..NfsSessionOptions::default()
    };
    let session = Nfs3Session::new(driver, options);
    let root = direct_mounted_root(&session).await;
    let lookup_call = encode_call(
        2,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_LOOKUP,
        None,
        None,
        &encode_xdr(|writer| {
            write_dir_op(
                writer,
                &DirOpArgs {
                    dir: root,
                    name: "cancel".to_owned(),
                },
            )
        }),
    );
    let worker_session = session.clone();
    let worker = tokio::spawn(async move {
        worker_session
            .handle_call(&lookup_call, NfsRequestContext::default())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), stat_started.notified())
        .await
        .expect("LOOKUP must open a provisional descriptor and await identity stat");
    assert!(!pause_once.load(Ordering::Acquire));
    assert_eq!(close_calls.load(Ordering::Acquire), 0);

    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    stat_release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), closed.notified())
        .await
        .expect("cancelling identity validation must close the opened descriptor");
    assert_eq!(close_calls.load(Ordering::Acquire), 1);
    session.destroy().await;
    assert_eq!(close_calls.load(Ordering::Acquire), 1);
    assert_eq!(
        session.handles.size(),
        1,
        "destroy must clear provisional pins"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_destroy_completes_both_retained_file_closes_exactly_once() {
    let backing = MemoryFs::empty();
    for path in ["/first", "/second"] {
        backing
            .open(path, "w", 0o644)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
    }
    let pause_first_close = Arc::new(AtomicBool::new(true));
    let close_started = Arc::new(Notify::new());
    let close_release = Arc::new(Notify::new());
    let first_completed = Arc::new(AtomicUsize::new(0));
    let second_completed = Arc::new(AtomicUsize::new(0));
    let driver = BlockingCloseDriver {
        backing,
        pause_first_close: pause_first_close.clone(),
        close_started: close_started.clone(),
        close_release: close_release.clone(),
        first_completed: first_completed.clone(),
        second_completed: second_completed.clone(),
    };
    let options = NfsSessionOptions {
        shared_concurrent_view: true,
        omit_wcc_attributes: true,
        ..NfsSessionOptions::default()
    };
    let session = Nfs3Session::new(driver, options);
    let root = direct_mounted_root(&session).await;
    let first_fh = direct_lookup_handle(&session, 2, &root, "first").await;
    let second_fh = direct_lookup_handle(&session, 3, &root, "second").await;
    assert_ne!(first_fh, second_fh);
    assert_eq!(first_completed.load(Ordering::Acquire), 0);
    assert_eq!(second_completed.load(Ordering::Acquire), 0);
    assert!(pause_first_close.load(Ordering::Acquire));

    // The first retained close started by destroy waits. Abort the destroy
    // future while it is awaiting this close, then release and retry.
    let worker_session = session.clone();
    let destroy_worker = tokio::spawn(async move { worker_session.destroy().await });
    tokio::time::timeout(std::time::Duration::from_secs(5), close_started.notified())
        .await
        .expect("destroy must start closing a retained descriptor");
    assert!(!pause_first_close.load(Ordering::Acquire));
    destroy_worker.abort();
    assert!(destroy_worker.await.unwrap_err().is_cancelled());
    close_release.notify_one();
    tokio::time::timeout(std::time::Duration::from_secs(5), session.destroy())
        .await
        .expect("repeated destroy must settle all retained closes");
    assert_eq!(first_completed.load(Ordering::Acquire), 1);
    assert_eq!(second_completed.load(Ordering::Acquire), 1);
    assert_eq!(session.handles.size(), 1, "destroy must settle handle pins");
}

#[test]
fn retained_file_closes_survive_destroy_retry_on_a_new_tokio_runtime() {
    let pause_first_close = Arc::new(AtomicBool::new(true));
    let close_started = Arc::new(Notify::new());
    let close_release = Arc::new(Notify::new());
    let first_completed = Arc::new(AtomicUsize::new(0));
    let second_completed = Arc::new(AtomicUsize::new(0));
    let runtime_a = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (session, first_fh, second_fh) = runtime_a.block_on(async {
        let backing = MemoryFs::empty();
        for path in ["/first", "/second"] {
            backing
                .open(path, "w", 0o644)
                .await
                .unwrap()
                .close()
                .await
                .unwrap();
        }
        let driver = BlockingCloseDriver {
            backing,
            pause_first_close: pause_first_close.clone(),
            close_started: close_started.clone(),
            close_release: close_release.clone(),
            first_completed: first_completed.clone(),
            second_completed: second_completed.clone(),
        };
        let options = NfsSessionOptions {
            shared_concurrent_view: true,
            omit_wcc_attributes: true,
            ..NfsSessionOptions::default()
        };
        let session = Nfs3Session::new(driver, options);
        let root = direct_mounted_root(&session).await;
        let first_fh = direct_lookup_handle(&session, 2, &root, "first").await;
        let second_fh = direct_lookup_handle(&session, 3, &root, "second").await;
        (session, first_fh, second_fh)
    });
    assert_ne!(first_fh, second_fh);
    assert_eq!(first_completed.load(Ordering::Acquire), 0);
    assert_eq!(second_completed.load(Ordering::Acquire), 0);

    runtime_a.block_on(async {
        let worker_session = session.clone();
        let destroy_worker = tokio::spawn(async move { worker_session.destroy().await });
        tokio::time::timeout(std::time::Duration::from_secs(5), close_started.notified())
            .await
            .expect("runtime A destroy must start a retained close");
        destroy_worker.abort();
        assert!(destroy_worker.await.unwrap_err().is_cancelled());
    });
    runtime_a.shutdown_timeout(std::time::Duration::from_millis(100));
    close_release.notify_one();

    let runtime_b = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime_b.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), session.destroy())
            .await
            .expect("runtime B must settle closes from destroyed runtime A");
    });
    assert_eq!(first_completed.load(Ordering::Acquire), 1);
    assert_eq!(second_completed.load(Ordering::Acquire), 1);
    assert_eq!(session.handles.size(), 1);
    assert!(session.handles.decode(&first_fh).is_err());
    assert!(session.handles.decode(&second_fh).is_err());
}

#[test]
fn provisional_file_close_survives_destroy_retry_on_a_new_tokio_runtime() {
    let stat_started = Arc::new(Notify::new());
    let stat_release = Arc::new(Notify::new());
    let pause_stat_once = Arc::new(AtomicBool::new(true));
    let close_started = Arc::new(Notify::new());
    let close_release = Arc::new(Notify::new());
    let pause_close_once = Arc::new(AtomicBool::new(true));
    let close_calls = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(Notify::new());
    let runtime_a = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let (session, lookup_call) = runtime_a.block_on(async {
        let backing = MemoryFs::empty();
        backing
            .open("/cancel", "w", 0o644)
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let driver = StatBarrierDriver {
            backing,
            stat_started: stat_started.clone(),
            stat_release: stat_release.clone(),
            pause_once: pause_stat_once.clone(),
            close_calls: close_calls.clone(),
            closed: closed.clone(),
            close_barrier: Some(CloseBarrier {
                started: close_started.clone(),
                release: close_release.clone(),
                pause_once: pause_close_once.clone(),
            }),
        };
        let options = NfsSessionOptions {
            shared_concurrent_view: true,
            omit_wcc_attributes: true,
            ..NfsSessionOptions::default()
        };
        let session = Nfs3Session::new(driver, options);
        let root = direct_mounted_root(&session).await;
        let lookup_call = encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_LOOKUP,
            None,
            None,
            &encode_xdr(|writer| {
                write_dir_op(
                    writer,
                    &DirOpArgs {
                        dir: root,
                        name: "cancel".to_owned(),
                    },
                )
            }),
        );
        (session, lookup_call)
    });
    runtime_a.block_on(async {
        let worker_session = session.clone();
        let lookup_worker = tokio::spawn(async move {
            worker_session
                .handle_call(&lookup_call, NfsRequestContext::default())
                .await
        });
        tokio::time::timeout(std::time::Duration::from_secs(5), stat_started.notified())
            .await
            .expect("LOOKUP must await provisional descriptor identity stat");
        lookup_worker.abort();
        assert!(lookup_worker.await.unwrap_err().is_cancelled());
        stat_release.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(5), close_started.notified())
            .await
            .expect("aborted LOOKUP must schedule provisional descriptor close");
    });
    assert_eq!(close_calls.load(Ordering::Acquire), 0);
    runtime_a.shutdown_timeout(std::time::Duration::from_millis(100));
    close_release.notify_one();

    let runtime_b = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime_b.block_on(async {
        tokio::time::timeout(std::time::Duration::from_secs(5), session.destroy())
            .await
            .expect("runtime B must recover provisional close from destroyed runtime A");
    });
    assert_eq!(close_calls.load(Ordering::Acquire), 1);
    assert_eq!(session.handles.size(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_close_reports_backend_error_and_retries_retained_handle() {
    let backing = MemoryFs::empty();
    let original = backing.open("/retry", "w", 0o644).await.unwrap();
    original.write(b"retained bytes", Some(0)).await.unwrap();
    original.close().await.unwrap();
    let fail_once = Arc::new(AtomicBool::new(true));
    let close_attempts = Arc::new(AtomicUsize::new(0));
    let successful_closes = Arc::new(AtomicUsize::new(0));
    let driver = ErrOnceCloseDriver {
        backing,
        fail_once: fail_once.clone(),
        close_attempts: close_attempts.clone(),
        successful_closes: successful_closes.clone(),
    };
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let file = lookup_handle(&mut stream, 2, root, "retry").await;
    assert!(server.session().handles.decode(&file).is_ok());
    assert_eq!(close_attempts.load(Ordering::Acquire), 0);

    let error = server
        .close()
        .await
        .expect_err("first backend close must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert!(error.to_string().contains("retry server.close()"));
    assert!(!fail_once.load(Ordering::Acquire));
    assert_eq!(close_attempts.load(Ordering::Acquire), 1);
    assert_eq!(successful_closes.load(Ordering::Acquire), 0);
    assert!(
        server.session().handles.decode(&file).is_ok(),
        "failed close must keep the original table entry and backend handle"
    );
    assert!(server.session().handles.size() > 1);

    server
        .close()
        .await
        .expect("second backend close must succeed");
    assert_eq!(close_attempts.load(Ordering::Acquire), 2);
    assert_eq!(successful_closes.load(Ordering::Acquire), 1);
    assert_eq!(server.session().handles.size(), 1);
    assert!(server.session().handles.decode(&file).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destroying_shared_v4_session_preserves_live_v3_file_handle() {
    let backing = MemoryFs::empty();
    let original = backing.open("/owned", "w", 0o644).await.unwrap();
    original.write(b"v3 retained bytes", Some(0)).await.unwrap();
    original.close().await.unwrap();
    let old_inode = backing.stat("/owned").await.unwrap().ino;
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(backing, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let old_handle = lookup_handle(&mut stream, 2, root, "owned").await;
    assert!(server.session().handles.decode(&old_handle).is_ok());

    // V4 and V3 sessions share one table in NfsServer. Destroying V4 alone
    // must not retire a V3 handle while its session and transport are live.
    server.v4_session().destroy().await;
    assert!(server.v4_session().destroyed());
    assert!(!server.session().destroyed());
    assert!(
        server.session().handles.decode(&old_handle).is_ok(),
        "V4 teardown must preserve V3-owned file handle entries"
    );
    let record = exchange(
        &mut stream,
        encode_call(
            3,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_READ,
            None,
            None,
            &encode_xdr(|writer| {
                write_read_args(
                    writer,
                    &Read3args {
                        file: old_handle,
                        offset: 0,
                        count: 64,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let read = read_read_res(&mut body, 64).unwrap();
    body.end("V3 READ after shared V4 destroy").unwrap();
    assert_eq!(read.status, NFS3_OK);
    assert_eq!(read.data, b"v3 retained bytes");
    assert_eq!(read.attributes.unwrap().fileid, old_inode);
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destroying_standalone_v4_session_clears_its_owned_file_handle_table() {
    let backing = MemoryFs::empty();
    backing
        .open("/standalone", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let stats = backing.stat("/standalone").await.unwrap();
    let session = Nfs4Session::new(backing, NfsSessionOptions::default());
    let entry = session.handles.bind("/standalone", &stats);
    let file = session.handles.encode(&entry);
    assert!(session.handles.decode(&file).is_ok());
    assert!(session.handles.size() > 1);

    session.destroy().await;
    assert!(session.destroyed());
    assert_eq!(session.handles.size(), 1);
    assert!(session.handles.decode(&file).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destroyed_standalone_v4_session_rejects_decoded_null_and_compound_calls() {
    let backing = MemoryFs::empty();
    backing
        .open("/a", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let stats = backing.stat("/a").await.unwrap();
    let session = Nfs4Session::new(backing, NfsSessionOptions::default());
    let entry = session.handles.bind("/a", &stats);
    let old_handle = session.handles.encode(&entry);
    assert!(session.handles.decode(&old_handle).is_ok());
    session.destroy().await;
    assert!(session.destroyed());
    assert_eq!(session.handles.size(), 1);
    assert!(session.handles.decode(&old_handle).is_err());

    let empty_compound = encode_xdr(|writer| {
        writer.string("postdestroy");
        writer.u32(1); // NFSv4.1 minor version
        writer.u32(0); // valid empty argarray
    });
    let mut observed = Vec::new();
    for (xid, procedure, args) in [
        (41, NFSPROC4_NULL, Vec::new()),
        (42, NFSPROC4_COMPOUND, empty_compound),
    ] {
        let call = encode_call(xid, NFS4_PROGRAM, NFS_V4, procedure, None, None, &args);
        let reply = session
            .handle_call(&call, NfsRequestContext::default())
            .await
            .expect("decoded XID must receive a post-destroy RPC reply");
        let (header, body) = decode_reply(&reply).unwrap();
        observed.push((
            header.xid,
            header.reply_stat,
            header.accept_stat,
            body.at_end(),
        ));
    }
    assert_eq!(
        observed,
        [
            (41, MSG_ACCEPTED, Some(RPC_SYSTEM_ERR), true),
            (42, MSG_ACCEPTED, Some(RPC_SYSTEM_ERR), true),
        ],
        "V4 must reject NULL and valid COMPOUND after destroy without a result body"
    );
    assert_eq!(session.handles.size(), 1);
    assert!(session.handles.decode(&old_handle).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destroyed_session_refuses_old_directory_handle_even_if_called_again() {
    let backing = MemoryFs::empty();
    let options = NfsSessionOptions {
        shared_concurrent_view: true,
        omit_wcc_attributes: true,
        ..NfsSessionOptions::default()
    };
    let session = Nfs3Session::new(backing.clone(), options);
    let old_root = direct_mounted_root(&session).await;
    session.destroy().await;
    backing
        .open("/b", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let lookup_call = encode_call(
        2,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_LOOKUP,
        None,
        None,
        &encode_xdr(|writer| {
            write_dir_op(
                writer,
                &DirOpArgs {
                    dir: old_root,
                    name: "b".to_owned(),
                },
            )
        }),
    );
    let reply = session
        .handle_call(&lookup_call, NfsRequestContext::default())
        .await
        .expect("decoded calls after destroy must receive RPC failure");
    let (reply, body) = decode_reply(&reply).unwrap();
    assert_eq!(reply.accept_stat, Some(RPC_SYSTEM_ERR));
    body.end("post-destroy RPC failure").unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_getattr_uses_retained_inode_when_peer_replaces_alias_during_stat() {
    let backing = MemoryFs::empty();
    let original = backing.open("/getattr", "w", 0o644).await.unwrap();
    original.write(b"original bytes", Some(0)).await.unwrap();
    original.close().await.unwrap();
    let old_inode = backing.stat("/getattr").await.unwrap().ino;
    let swap_once = Arc::new(AtomicBool::new(false));
    let driver = SwapAfterGetattrAliasStat {
        backing: backing.clone(),
        swap_once: swap_once.clone(),
    };
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let old_handle = lookup_handle(&mut stream, 2, root, "getattr").await;

    swap_once.store(true, Ordering::Release);
    let record = exchange(
        &mut stream,
        encode_call(
            3,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_GETATTR,
            None,
            None,
            &encode_xdr(|writer| writer.var_opaque(&old_handle)),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let getattr = read_getattr_res(&mut body).unwrap();
    body.end("GETATTR after peer alias replacement").unwrap();
    assert!(!swap_once.load(Ordering::Acquire));
    assert_eq!(getattr.status, NFS3_OK);
    let attrs = getattr.attributes.expect("retained A attributes");
    assert_eq!(attrs.fileid, old_inode);
    assert_eq!(attrs.size, b"original bytes".len() as u64);
    assert_eq!(backing.stat("/moved").await.unwrap().ino, old_inode);
    assert_ne!(backing.stat("/getattr").await.unwrap().ino, old_inode);
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_lookup_pins_old_handle_while_backend_open_is_in_flight() {
    let backing = MemoryFs::empty();
    let original = backing.open("/race", "w", 0o644).await.unwrap();
    original.write(b"original bytes", Some(0)).await.unwrap();
    original.close().await.unwrap();
    let old_inode = backing.stat("/race").await.unwrap().ino;
    let pause_once = Arc::new(AtomicBool::new(true));
    let started = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let driver = PauseFirstReadOpen {
        backing: backing.clone(),
        pause_once: pause_once.clone(),
        started: started.clone(),
        release: release.clone(),
    };
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut first_stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut first_stream).await;
    let mut peer_stream = TcpStream::connect(address).await.unwrap();

    // The first LOOKUP binds and pins the old inode, then pauses after the
    // backend has opened its old descriptor and before retention completes.
    let first_root = root.clone();
    let old_lookup = tokio::spawn(async move {
        let handle = lookup_handle(&mut first_stream, 2, first_root, "race").await;
        (first_stream, handle)
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .expect("old LOOKUP must reach backend descriptor open");
    assert!(!pause_once.load(Ordering::Acquire));

    // Peer replacement and a second LOOKUP force same-path handle rebind
    // while the original backend open is still awaiting release.
    backing.unlink("/race").await.unwrap();
    let replacement = backing.open("/race", "w", 0o644).await.unwrap();
    replacement
        .write(b"replacement bytes", Some(0))
        .await
        .unwrap();
    replacement.close().await.unwrap();
    assert_ne!(backing.stat("/race").await.unwrap().ino, old_inode);
    let new_handle = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        lookup_handle(&mut peer_stream, 3, root, "race"),
    )
    .await
    .expect("replacement LOOKUP must finish while old open is paused");
    release.notify_one();
    let (mut first_stream, old_handle) =
        tokio::time::timeout(std::time::Duration::from_secs(5), old_lookup)
            .await
            .expect("old LOOKUP must finish after release")
            .unwrap();
    assert_ne!(old_handle, new_handle);

    let record = exchange(
        &mut first_stream,
        encode_call(
            4,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_READ,
            None,
            None,
            &encode_xdr(|writer| {
                write_read_args(
                    writer,
                    &Read3args {
                        file: old_handle.clone(),
                        offset: 0,
                        count: 64,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let old_read = read_read_res(&mut body, 64).unwrap();
    body.end("old FH READ after in-flight bind race").unwrap();
    assert_eq!(old_read.status, NFS3_OK);
    assert_eq!(old_read.data, b"original bytes");
    assert_eq!(old_read.attributes.unwrap().fileid, old_inode);
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_stable_inode_handle_follows_peer_rename_without_mutating_replacement() {
    let backing = MemoryFs::empty();
    let original = backing.open("/file", "w", 0o644).await.unwrap();
    original.write(b"original bytes", Some(0)).await.unwrap();
    original.close().await.unwrap();
    let old_inode = backing.stat("/file").await.unwrap().ino;
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(backing.clone(), options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let old_handle = lookup_handle(&mut stream, 2, root.clone(), "file").await;

    // A peer moves A and creates B at A's former name. Lookup B first to
    // rebind /file, then discover A at /moved through its stable backend inode.
    backing.rename("/file", "/moved").await.unwrap();
    let replacement = backing.open("/file", "w", 0o644).await.unwrap();
    replacement
        .write(b"replacement bytes", Some(0))
        .await
        .unwrap();
    replacement.close().await.unwrap();
    let replacement_before = backing.stat("/file").await.unwrap();
    assert_ne!(replacement_before.ino, old_inode);
    let replacement_handle = lookup_handle(&mut stream, 3, root.clone(), "file").await;
    let moved_handle = lookup_handle(&mut stream, 4, root, "moved").await;
    assert_ne!(replacement_handle, old_handle);
    assert_eq!(
        moved_handle, old_handle,
        "A's stable inode must keep the original opaque file handle ID"
    );

    let record = exchange(
        &mut stream,
        encode_call(
            5,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_SETATTR,
            None,
            None,
            &encode_xdr(|writer| {
                write_setattr_args(
                    writer,
                    &Setattr3args {
                        object: old_handle,
                        attributes: Sattr3 {
                            mode: Some(0o755),
                            ..Sattr3::default()
                        },
                        guard: None,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let setattr = read_wcc_res(&mut body).unwrap();
    body.end("old stable FH SETATTR after peer rename").unwrap();
    assert_eq!(setattr.status, NFS3_OK);
    let moved_after = backing.stat("/moved").await.unwrap();
    assert_eq!(moved_after.ino, old_inode);
    assert_eq!(moved_after.mode & 0o7777, 0o755);
    let replacement_after = backing.stat("/file").await.unwrap();
    assert_eq!(replacement_after.ino, replacement_before.ino);
    assert_eq!(replacement_after.mode, replacement_before.mode);
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_lookup_rejects_disabled_driver_inode_handles() {
    let backing = MemoryFs::empty();
    backing
        .open("/visible", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let mut options = NfsServerOptions::default();
    options.session.use_driver_ino = false;
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let verifier = [0x5a; 8];
    options.session.verifier = Some(verifier);
    // MOUNT itself fails closed in this configuration. Send NFS a correctly
    // encoded root FH to verify LOOKUP also refuses to issue a child handle.
    let table = FileHandleTable::new(FileHandleTableOptions {
        use_driver_ino: false,
        verifier: Some(verifier),
        max_handles: None,
    });
    let root = table.encode(&table.root());
    let server = NfsServer::new(backing, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let record = exchange(
        &mut stream,
        encode_call(
            1,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_MNT,
            None,
            None,
            &encode_xdr(|writer| writer.string("/")),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let mount = read_mount_res(&mut body).unwrap();
    body.end("shared MOUNT without driver inode binding")
        .unwrap();
    assert_eq!(mount.status, MNT3ERR_NOTSUPP);
    assert!(mount.fh.is_none(), "must not issue an unusable root FH");
    let record = exchange(
        &mut stream,
        encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_LOOKUP,
            None,
            None,
            &encode_xdr(|writer| {
                write_dir_op(
                    writer,
                    &DirOpArgs {
                        dir: root,
                        name: "visible".to_owned(),
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let lookup = read_lookup_res(&mut body).unwrap();
    body.end("shared LOOKUP without driver inode binding")
        .unwrap();
    assert_eq!(lookup.status, NFS3ERR_NOTSUPP);
    assert!(lookup.object.is_none(), "must not issue an unusable FH");
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_lookup_rejects_zero_inode_before_issuing_handle() {
    let backing = MemoryFs::empty();
    backing
        .open("/zero", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let driver = ZeroInodeStat {
        backing: backing.clone(),
    };
    let mut options = NfsServerOptions::default();
    options.session.shared_concurrent_view = true;
    options.session.omit_wcc_attributes = true;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let record = exchange(
        &mut stream,
        encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_LOOKUP,
            None,
            None,
            &encode_xdr(|writer| {
                write_dir_op(
                    writer,
                    &DirOpArgs {
                        dir: root,
                        name: "zero".to_owned(),
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let lookup = read_lookup_res(&mut body).unwrap();
    body.end("zero-inode LOOKUP response").unwrap();
    assert_eq!(lookup.status, NFS3ERR_STALE);
    assert!(
        lookup.object.is_none(),
        "server must not issue an unusable FH"
    );
    assert!(backing.stat("/zero").await.is_ok());
    server.close().await.unwrap();
}

async fn stat_only_shared_write(replace_before_open: bool) -> (u32, u64, u64, Vec<u8>) {
    let backing = MemoryFs::empty();
    let driver = ReplaceBeforeWriteOpen {
        backing: backing.clone(),
        replace_once: Arc::new(AtomicBool::new(replace_before_open)),
    };
    let mut options = NfsServerOptions::default();
    options.session.omit_wcc_attributes = true;
    options.session.shared_concurrent_view = true;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();

    let record = exchange(
        &mut stream,
        encode_call(
            1,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_MNT,
            None,
            None,
            &encode_xdr(|writer| writer.string("/")),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let root = read_mount_res(&mut body).unwrap().fh.unwrap();

    let create = encode_xdr(|writer| {
        write_create_args(
            writer,
            &Create3args {
                where_: DirOpArgs {
                    dir: root,
                    name: "victim".to_owned(),
                },
                mode: CREATE_UNCHECKED,
                attributes: None,
                verf: None,
            },
        )
    });
    let record = exchange(
        &mut stream,
        encode_call(2, NFS_PROGRAM, NFS_V3, NFSPROC3_CREATE, None, None, &create),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let created = read_create_res(&mut body).unwrap();
    assert_eq!(created.status, NFS3_OK);
    let handle = created.obj.unwrap();
    let old_ino = backing.stat("/victim").await.unwrap().ino;

    let write = encode_xdr(|writer| {
        write_write_args(
            writer,
            &Write3args {
                file: handle,
                offset: 0,
                count: 1,
                stable: FILE_SYNC,
                data: b"x".to_vec(),
            },
        )
    });
    let record = exchange(
        &mut stream,
        encode_call(3, NFS_PROGRAM, NFS_V3, NFSPROC3_WRITE, None, None, &write),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let written = read_write_res(&mut body).unwrap();
    body.end("WRITE response").unwrap();
    let new_ino = backing.stat("/victim").await.unwrap().ino;
    let file = backing.open("/victim", "r", 0).await.unwrap();
    let mut bytes = vec![0; 1];
    let read = file.read(&mut bytes, Some(0)).await.unwrap();
    bytes.truncate(read);
    file.close().await.unwrap();
    server.close().await.unwrap();
    (written.status, old_ino, new_ino, bytes)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stat_only_shared_driver_can_write_its_original_inode() {
    let (status, old_ino, new_ino, bytes) = stat_only_shared_write(false).await;
    assert_eq!(status, NFS3_OK);
    assert_eq!(new_ino, old_ino);
    assert_eq!(bytes, b"x");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_rejects_replacement_between_handle_validation_and_open() {
    let (status, old_ino, new_ino, bytes) = stat_only_shared_write(true).await;
    assert_eq!(status, NFS3ERR_STALE);
    assert_ne!(new_ino, old_ino);
    assert!(
        bytes.is_empty(),
        "replacement file must not receive old-FH bytes"
    );
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReplacementMutation {
    Setattr,
    Remove,
    Rename,
}

#[derive(Clone)]
struct SwapAtMutation {
    backing: MemoryFs,
    mutation: ReplacementMutation,
    swap_once: Arc<AtomicBool>,
}

impl SwapAtMutation {
    async fn swap_if(&self, mutation: ReplacementMutation) -> Result<()> {
        if self.mutation != mutation || !self.swap_once.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        if mutation == ReplacementMutation::Setattr {
            self.backing.rename("/victim", "/original-victim").await?;
            self.backing
                .open("/victim", "w", 0o600)
                .await?
                .close()
                .await?;
        } else {
            self.backing.rename("/parent", "/original-parent").await?;
            self.backing
                .mkdir(
                    "/parent",
                    MkdirOptions {
                        recursive: false,
                        mode: Some(0o755),
                    },
                )
                .await?;
            self.backing
                .open("/parent/entry", "w", 0o644)
                .await?
                .close()
                .await?;
        }
        Ok(())
    }
}

#[async_trait]
impl FsDriver for SwapAtMutation {
    fn capabilities(&self) -> Capabilities {
        self.backing.capabilities()
    }

    fn supports_guarded_reads(&self) -> bool {
        self.backing.supports_guarded_reads()
    }

    async fn guarded_read(&self, request: GuardedRead) -> Result<GuardedReadResult> {
        self.backing.guarded_read(request).await
    }

    fn supports_guarded_mutations(&self) -> bool {
        self.backing.supports_guarded_mutations()
    }

    async fn guarded_mutation(&self, request: GuardedMutation) -> Result<GuardedMutationResult> {
        let mutation = match &request {
            GuardedMutation::Setattr { .. } => ReplacementMutation::Setattr,
            GuardedMutation::Unlink { .. } => ReplacementMutation::Remove,
            GuardedMutation::Rename { .. } => ReplacementMutation::Rename,
            _ => return self.backing.guarded_mutation(request).await,
        };
        self.swap_if(mutation).await?;
        self.backing.guarded_mutation(request).await
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        self.backing.stat(path).await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.backing.lstat(path).await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.backing.readdir(path).await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.backing.open(path, flags, mode).await
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        self.swap_if(ReplacementMutation::Setattr).await?;
        self.backing.chmod(path, mode).await
    }

    async fn unlink(&self, path: &str) -> Result<()> {
        self.swap_if(ReplacementMutation::Remove).await?;
        self.backing.unlink(path).await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        self.swap_if(ReplacementMutation::Rename).await?;
        self.backing.rename(old_path, new_path).await
    }
}

async fn mounted_root(stream: &mut TcpStream) -> Vec<u8> {
    let record = exchange(
        stream,
        encode_call(
            1,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_MNT,
            None,
            None,
            &encode_xdr(|writer| writer.string("/")),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let mount = read_mount_res(&mut body).unwrap();
    body.end("MOUNT response").unwrap();
    assert_eq!(mount.status, NFS3_OK);
    mount.fh.unwrap()
}

async fn direct_mounted_root(session: &Nfs3Session) -> Vec<u8> {
    let call = encode_call(
        1,
        MOUNT_PROGRAM,
        MOUNT_V3,
        MOUNTPROC3_MNT,
        None,
        None,
        &encode_xdr(|writer| writer.string("/")),
    );
    let reply = session
        .handle_call(&call, NfsRequestContext::default())
        .await
        .expect("MOUNT must produce an RPC reply");
    let (_, mut body) = decode_reply(&reply).unwrap();
    let mount = read_mount_res(&mut body).unwrap();
    body.end("direct MOUNT reply").unwrap();
    assert_eq!(mount.status, NFS3_OK);
    mount.fh.unwrap()
}

async fn direct_lookup_handle(session: &Nfs3Session, xid: u32, dir: &[u8], name: &str) -> Vec<u8> {
    let call = encode_call(
        xid,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_LOOKUP,
        None,
        None,
        &encode_xdr(|writer| {
            write_dir_op(
                writer,
                &DirOpArgs {
                    dir: dir.to_vec(),
                    name: name.to_owned(),
                },
            )
        }),
    );
    let reply = session
        .handle_call(&call, NfsRequestContext::default())
        .await
        .expect("LOOKUP must produce an RPC reply");
    let (_, mut body) = decode_reply(&reply).unwrap();
    let lookup = read_lookup_res(&mut body).unwrap();
    body.end("direct LOOKUP reply").unwrap();
    assert_eq!(lookup.status, NFS3_OK);
    lookup.object.unwrap()
}

async fn lookup_handle(stream: &mut TcpStream, xid: u32, dir: Vec<u8>, name: &str) -> Vec<u8> {
    let record = exchange(
        stream,
        encode_call(
            xid,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_LOOKUP,
            None,
            None,
            &encode_xdr(|writer| {
                write_dir_op(
                    writer,
                    &DirOpArgs {
                        dir,
                        name: name.to_owned(),
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let lookup = read_lookup_res(&mut body).unwrap();
    body.end("LOOKUP response").unwrap();
    assert_eq!(lookup.status, NFS3_OK);
    lookup.object.unwrap()
}

async fn setattr_with_replacement(shared: bool, replace: bool) -> (u32, MemoryFs, bool) {
    let backing = MemoryFs::empty();
    backing
        .open("/victim", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let swap_once = Arc::new(AtomicBool::new(replace));
    let driver = SwapAtMutation {
        backing: backing.clone(),
        mutation: ReplacementMutation::Setattr,
        swap_once: swap_once.clone(),
    };
    let mut options = NfsServerOptions::default();
    options.session.omit_wcc_attributes = shared;
    options.session.shared_concurrent_view = shared;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let victim = lookup_handle(&mut stream, 2, root, "victim").await;
    let record = exchange(
        &mut stream,
        encode_call(
            3,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_SETATTR,
            None,
            None,
            &encode_xdr(|writer| {
                write_setattr_args(
                    writer,
                    &Setattr3args {
                        object: victim,
                        attributes: Sattr3 {
                            mode: Some(0o755),
                            ..Sattr3::default()
                        },
                        guard: None,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let status = read_wcc_res(&mut body).unwrap().status;
    body.end("SETATTR response").unwrap();
    server.close().await.unwrap();
    (
        status,
        backing,
        replace && !swap_once.load(Ordering::Acquire),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_setattr_rejects_file_replacement_at_mutation_boundary() {
    let (status, backing, swapped) = setattr_with_replacement(true, true).await;
    assert!(
        swapped,
        "the replacement must occur after handle resolution"
    );
    assert_eq!(status, NFS3ERR_STALE);
    assert_eq!(backing.stat("/victim").await.unwrap().mode & 0o7777, 0o600);
    assert_eq!(
        backing.stat("/original-victim").await.unwrap().mode & 0o7777,
        0o644
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_setattr_still_updates_original_file() {
    let (status, backing, swapped) = setattr_with_replacement(false, false).await;
    assert!(!swapped);
    assert_eq!(status, NFS3_OK);
    assert_eq!(backing.stat("/victim").await.unwrap().mode & 0o7777, 0o755);
}

async fn parent_mutation_with_replacement(
    mutation: ReplacementMutation,
    shared: bool,
    replace: bool,
) -> (u32, MemoryFs, bool) {
    let backing = MemoryFs::empty();
    backing
        .mkdir(
            "/parent",
            MkdirOptions {
                recursive: false,
                mode: Some(0o755),
            },
        )
        .await
        .unwrap();
    backing
        .open("/parent/entry", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let swap_once = Arc::new(AtomicBool::new(replace));
    let driver = SwapAtMutation {
        backing: backing.clone(),
        mutation,
        swap_once: swap_once.clone(),
    };
    let mut options = NfsServerOptions::default();
    options.session.omit_wcc_attributes = shared;
    options.session.shared_concurrent_view = shared;
    let server = NfsServer::new(driver, options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let parent = lookup_handle(&mut stream, 2, root, "parent").await;
    let status = match mutation {
        ReplacementMutation::Remove => {
            let record = exchange(
                &mut stream,
                encode_call(
                    3,
                    NFS_PROGRAM,
                    NFS_V3,
                    NFSPROC3_REMOVE,
                    None,
                    None,
                    &encode_xdr(|writer| {
                        write_dir_op(
                            writer,
                            &DirOpArgs {
                                dir: parent,
                                name: "entry".to_owned(),
                            },
                        )
                    }),
                ),
            )
            .await;
            let (_, mut body) = decode_reply(&record).unwrap();
            let status = read_wcc_res(&mut body).unwrap().status;
            body.end("REMOVE response").unwrap();
            status
        }
        ReplacementMutation::Rename => {
            let record = exchange(
                &mut stream,
                encode_call(
                    3,
                    NFS_PROGRAM,
                    NFS_V3,
                    NFSPROC3_RENAME,
                    None,
                    None,
                    &encode_xdr(|writer| {
                        write_rename_args(
                            writer,
                            &Rename3args {
                                from: DirOpArgs {
                                    dir: parent.clone(),
                                    name: "entry".to_owned(),
                                },
                                to: DirOpArgs {
                                    dir: parent,
                                    name: "moved".to_owned(),
                                },
                            },
                        )
                    }),
                ),
            )
            .await;
            let (_, mut body) = decode_reply(&record).unwrap();
            let status = read_rename_res(&mut body).unwrap().status;
            body.end("RENAME response").unwrap();
            status
        }
        ReplacementMutation::Setattr => unreachable!(),
    };
    server.close().await.unwrap();
    (
        status,
        backing,
        replace && !swap_once.load(Ordering::Acquire),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_remove_rejects_replaced_parent_directory() {
    let (status, backing, swapped) =
        parent_mutation_with_replacement(ReplacementMutation::Remove, true, true).await;
    assert!(
        swapped,
        "the parent replacement must reach the mutation boundary"
    );
    assert_eq!(status, NFS3ERR_STALE);
    assert!(backing.stat("/original-parent/entry").await.is_ok());
    assert!(backing.stat("/parent/entry").await.is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_rename_rejects_replaced_parent_directory() {
    let (status, backing, swapped) =
        parent_mutation_with_replacement(ReplacementMutation::Rename, true, true).await;
    assert!(
        swapped,
        "the parent replacement must reach the mutation boundary"
    );
    assert_eq!(status, NFS3ERR_STALE);
    assert!(backing.stat("/original-parent/entry").await.is_ok());
    assert!(backing.stat("/parent/entry").await.is_ok());
    assert!(backing.stat("/parent/moved").await.is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_remove_still_unlinks_named_entry() {
    let (status, backing, swapped) =
        parent_mutation_with_replacement(ReplacementMutation::Remove, false, false).await;
    assert!(!swapped);
    assert_eq!(status, NFS3_OK);
    assert!(backing.stat("/parent/entry").await.is_err());
}

fn assert_wcc_attrs(wcc: &WccData, present: bool) {
    assert_eq!(wcc.before.is_some(), present, "WCC before attribute");
    assert_eq!(wcc.after.is_some(), present, "WCC after attribute");
}

async fn mutation_wcc_replies(omit_wcc_attributes: bool, shared_concurrent_view: bool) {
    let mut options = NfsServerOptions::default();
    options.session.omit_wcc_attributes = omit_wcc_attributes;
    options.session.shared_concurrent_view = shared_concurrent_view;
    let wcc_present = !omit_wcc_attributes && !shared_concurrent_view;
    let server = NfsServer::new(MemoryFs::empty(), options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();

    let record = exchange(
        &mut stream,
        encode_call(
            1,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_MNT,
            None,
            None,
            &encode_xdr(|writer| writer.string("/")),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let root = read_mount_res(&mut body).unwrap().fh.unwrap();
    body.end("MOUNT response").unwrap();

    let create = encode_xdr(|writer| {
        write_create_args(
            writer,
            &Create3args {
                where_: DirOpArgs {
                    dir: root.clone(),
                    name: "wcc-before".to_owned(),
                },
                mode: CREATE_UNCHECKED,
                attributes: Some(Sattr3 {
                    mode: Some(0o644),
                    ..Sattr3::default()
                }),
                verf: None,
            },
        )
    });
    let record = exchange(
        &mut stream,
        encode_call(2, NFS_PROGRAM, NFS_V3, NFSPROC3_CREATE, None, None, &create),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let created = read_create_res(&mut body).unwrap();
    body.end("CREATE response").unwrap();
    assert_eq!(created.status, NFS3_OK);
    assert_wcc_attrs(&created.dir_wcc, wcc_present);
    let file = created.obj.unwrap();

    let write = |offset| {
        encode_xdr(|writer| {
            write_write_args(
                writer,
                &Write3args {
                    file: file.clone(),
                    offset,
                    count: 1,
                    stable: FILE_SYNC,
                    data: b"x".to_vec(),
                },
            )
        })
    };
    let record = exchange(
        &mut stream,
        encode_call(
            3,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_WRITE,
            None,
            None,
            &write(0),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let written = read_write_res(&mut body).unwrap();
    body.end("WRITE response").unwrap();
    assert_eq!(written.status, NFS3_OK);
    assert_wcc_attrs(&written.wcc, wcc_present);

    let record = exchange(
        &mut stream,
        encode_call(
            4,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_WRITE,
            None,
            None,
            &write(u64::MAX),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let failed_write = read_write_res(&mut body).unwrap();
    body.end("failed WRITE response").unwrap();
    assert_ne!(failed_write.status, NFS3_OK);
    assert_eq!(failed_write.wcc.before, None);
    assert_eq!(failed_write.wcc.after.is_some(), wcc_present);

    let rename = |name: &str| {
        encode_xdr(|writer| {
            write_rename_args(
                writer,
                &Rename3args {
                    from: DirOpArgs {
                        dir: root.clone(),
                        name: name.to_owned(),
                    },
                    to: DirOpArgs {
                        dir: root.clone(),
                        name: "wcc-after".to_owned(),
                    },
                },
            )
        })
    };
    let record = exchange(
        &mut stream,
        encode_call(
            5,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_RENAME,
            None,
            None,
            &rename("wcc-before"),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let renamed = read_rename_res(&mut body).unwrap();
    body.end("RENAME response").unwrap();
    assert_eq!(renamed.status, NFS3_OK);
    assert_wcc_attrs(&renamed.from_wcc, wcc_present);
    assert_wcc_attrs(&renamed.to_wcc, wcc_present);

    let record = exchange(
        &mut stream,
        encode_call(
            6,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_RENAME,
            None,
            None,
            &rename("missing"),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let failed_rename = read_rename_res(&mut body).unwrap();
    body.end("failed RENAME response").unwrap();
    assert_ne!(failed_rename.status, NFS3_OK);
    assert_wcc_attrs(&failed_rename.from_wcc, wcc_present);
    assert_wcc_attrs(&failed_rename.to_wcc, wcc_present);

    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_mutation_replies_include_wcc_attributes() {
    mutation_wcc_replies(false, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_mutation_replies_omit_wcc_attributes() {
    mutation_wcc_replies(true, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_view_replies_omit_wcc_even_when_omit_option_is_false() {
    mutation_wcc_replies(false, true).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_rename_keeps_the_new_lookup_handle_live() {
    let backing = MemoryFs::empty();
    let mut options = NfsServerOptions::default();
    options.session.omit_wcc_attributes = true;
    options.session.shared_concurrent_view = true;
    let server = NfsServer::new(backing.clone(), options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();

    let record = exchange(
        &mut stream,
        encode_call(
            1,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_MNT,
            None,
            None,
            &encode_xdr(|writer| writer.string("/")),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let root = read_mount_res(&mut body).unwrap().fh.unwrap();
    body.end("MOUNT response").unwrap();

    let create = encode_xdr(|writer| {
        write_create_args(
            writer,
            &Create3args {
                where_: DirOpArgs {
                    dir: root.clone(),
                    name: "created-by-a".to_owned(),
                },
                mode: CREATE_UNCHECKED,
                attributes: None,
                verf: None,
            },
        )
    });
    let record = exchange(
        &mut stream,
        encode_call(2, NFS_PROGRAM, NFS_V3, NFSPROC3_CREATE, None, None, &create),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let created = read_create_res(&mut body).unwrap();
    assert_eq!(created.status, NFS3_OK);
    let old_handle = created.obj.unwrap();
    body.end("CREATE response").unwrap();

    // Simulate a different server publishing the rename while this server
    // retains its old inode-to-path binding.
    backing
        .rename("/created-by-a", "/renamed-by-a")
        .await
        .unwrap();
    let record = exchange(
        &mut stream,
        encode_call(
            3,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_GETATTR,
            None,
            None,
            &encode_xdr(|writer| writer.var_opaque(&old_handle)),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    assert_eq!(read_getattr_res(&mut body).unwrap().status, NFS3_OK);
    body.end("old handle GETATTR before new LOOKUP").unwrap();

    let lookup = encode_xdr(|writer| {
        write_dir_op(
            writer,
            &DirOpArgs {
                dir: root.clone(),
                name: "renamed-by-a".to_owned(),
            },
        )
    });
    let record = exchange(
        &mut stream,
        encode_call(4, NFS_PROGRAM, NFS_V3, NFSPROC3_LOOKUP, None, None, &lookup),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let found = read_lookup_res(&mut body).unwrap();
    body.end("LOOKUP response").unwrap();
    assert_eq!(found.status, NFS3_OK);
    let handle = found.object.unwrap();
    assert_eq!(
        handle, old_handle,
        "remote rename must preserve handle identity"
    );

    let record = exchange(
        &mut stream,
        encode_call(
            5,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_GETATTR,
            None,
            None,
            &encode_xdr(|writer| writer.var_opaque(&handle)),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let attr = read_getattr_res(&mut body).unwrap();
    body.end("GETATTR response").unwrap();
    assert_eq!(attr.status, NFS3_OK, "new name's handle must stay live");

    let record = exchange(
        &mut stream,
        encode_call(
            6,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_READ,
            None,
            None,
            &encode_xdr(|writer| {
                write_read_args(
                    writer,
                    &Read3args {
                        file: handle.clone(),
                        offset: 0,
                        count: 1,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let read = read_read_res(&mut body, 1).unwrap();
    body.end("READ response").unwrap();
    assert_eq!(read.status, NFS3_OK);
    assert!(
        read.attributes.is_some(),
        "READ must describe the live alias"
    );

    // A hardlink keeps the inode alive after the renamed path is removed and
    // reused by a different inode. The old opaque handle must find that alias.
    backing.link("/renamed-by-a", "/z-hardlink").await.unwrap();
    let linked = encode_xdr(|writer| {
        write_dir_op(
            writer,
            &DirOpArgs {
                dir: root,
                name: "z-hardlink".to_owned(),
            },
        )
    });
    let record = exchange(
        &mut stream,
        encode_call(7, NFS_PROGRAM, NFS_V3, NFSPROC3_LOOKUP, None, None, &linked),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let found_link = read_lookup_res(&mut body).unwrap();
    body.end("hardlink LOOKUP response").unwrap();
    assert_eq!(found_link.object.unwrap(), handle);
    backing.unlink("/renamed-by-a").await.unwrap();
    backing
        .open("/renamed-by-a", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();

    let record = exchange(
        &mut stream,
        encode_call(
            8,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_GETATTR,
            None,
            None,
            &encode_xdr(|writer| writer.var_opaque(&handle)),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let attr = read_getattr_res(&mut body).unwrap();
    body.end("hardlink GETATTR response").unwrap();
    assert_eq!(attr.status, NFS3_OK, "hardlink must remain a live alias");

    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_directory_rename_recovers_old_handle_after_new_lookup() {
    let backing = MemoryFs::empty();
    backing
        .mkdir(
            "/dir-old",
            MkdirOptions {
                recursive: false,
                mode: Some(0o755),
            },
        )
        .await
        .unwrap();
    backing
        .open("/dir-old/child", "w", 0o644)
        .await
        .unwrap()
        .close()
        .await
        .unwrap();
    let mut options = NfsServerOptions::default();
    options.session.omit_wcc_attributes = true;
    options.session.shared_concurrent_view = true;
    let server = NfsServer::new(backing.clone(), options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let old_handle = lookup_handle(&mut stream, 2, root.clone(), "dir-old").await;

    // A second server moves the directory. This server sees the stale old
    // alias through GETATTR before it has looked up the new name.
    backing.rename("/dir-old", "/dir-new").await.unwrap();
    let record = exchange(
        &mut stream,
        encode_call(
            3,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_GETATTR,
            None,
            None,
            &encode_xdr(|writer| writer.var_opaque(&old_handle)),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let stale = read_getattr_res(&mut body).unwrap();
    body.end("old directory GETATTR before new LOOKUP").unwrap();
    assert_eq!(stale.status, NFS3ERR_STALE);

    let recovered = lookup_handle(&mut stream, 4, root, "dir-new").await;
    assert_eq!(
        recovered, old_handle,
        "remote directory rename must retain its opaque handle identity"
    );
    let record = exchange(
        &mut stream,
        encode_call(
            5,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_GETATTR,
            None,
            None,
            &encode_xdr(|writer| writer.var_opaque(&old_handle)),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let live = read_getattr_res(&mut body).unwrap();
    body.end("recovered directory GETATTR").unwrap();
    assert_eq!(live.status, NFS3_OK);
    lookup_handle(&mut stream, 6, old_handle, "child").await;

    server.close().await.unwrap();
}

async fn create_unchecked_on_existing_symlink(shared: bool) -> (u32, u64, u64, u64, u64, Vec<u8>) {
    let backing = MemoryFs::empty();
    let target = backing.open("/target", "w", 0o644).await.unwrap();
    target.write(b"keep this", Some(0)).await.unwrap();
    target.close().await.unwrap();
    backing.symlink("/target", "/link").await.unwrap();
    let target_before = backing.stat("/target").await.unwrap();
    let link_before = backing.lstat("/link").await.unwrap();

    let mut options = NfsServerOptions::default();
    options.session.omit_wcc_attributes = shared;
    options.session.shared_concurrent_view = shared;
    let server = NfsServer::new(backing.clone(), options);
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;
    let record = exchange(
        &mut stream,
        encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_CREATE,
            None,
            None,
            &encode_xdr(|writer| {
                write_create_args(
                    writer,
                    &Create3args {
                        where_: DirOpArgs {
                            dir: root,
                            name: "link".to_owned(),
                        },
                        mode: CREATE_UNCHECKED,
                        attributes: None,
                        verf: None,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&record).unwrap();
    let created = read_create_res(&mut body).unwrap();
    body.end("CREATE_UNCHECKED existing symlink response")
        .unwrap();

    let target_after = backing.stat("/target").await.unwrap();
    let link_after = backing.lstat("/link").await.unwrap();
    let file = backing.open("/target", "r", 0).await.unwrap();
    let mut bytes = vec![0; 16];
    let count = file.read(&mut bytes, Some(0)).await.unwrap();
    bytes.truncate(count);
    file.close().await.unwrap();
    server.close().await.unwrap();
    (
        created.status,
        target_before.ino,
        target_after.ino,
        link_before.ino,
        link_after.ino,
        bytes,
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_create_unchecked_rejects_existing_symlink_without_touching_target() {
    let (status, old_target, new_target, old_link, new_link, bytes) =
        create_unchecked_on_existing_symlink(true).await;
    assert_eq!(status, NFS3ERR_EXIST);
    assert_eq!(new_target, old_target);
    assert_eq!(new_link, old_link);
    assert_eq!(bytes, b"keep this");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_create_unchecked_existing_symlink_keeps_target_content() {
    let (status, old_target, new_target, old_link, new_link, bytes) =
        create_unchecked_on_existing_symlink(false).await;
    assert_eq!(status, NFS3_OK);
    assert_eq!(new_target, old_target);
    assert_eq!(new_link, old_link);
    assert_eq!(bytes, b"keep this");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn embedded_nul_names_are_rejected_before_directory_operations() {
    let backing = MemoryFs::empty();
    let server = NfsServer::new(backing.clone(), NfsServerOptions::default());
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();
    let root = mounted_root(&mut stream).await;

    let mkdir = exchange(
        &mut stream,
        encode_call(
            2,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_MKDIR,
            None,
            None,
            &encode_xdr(|writer| {
                write_mkdir_args(
                    writer,
                    &Mkdir3args {
                        where_: DirOpArgs {
                            dir: root.clone(),
                            name: "bad\0mkdir".to_owned(),
                        },
                        attributes: Sattr3::default(),
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&mkdir).unwrap();
    let mkdir_status = read_create_res(&mut body).unwrap().status;
    body.end("MKDIR embedded NUL response").unwrap();

    let create = exchange(
        &mut stream,
        encode_call(
            3,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_CREATE,
            None,
            None,
            &encode_xdr(|writer| {
                write_create_args(
                    writer,
                    &Create3args {
                        where_: DirOpArgs {
                            dir: root.clone(),
                            name: "bad\0create".to_owned(),
                        },
                        mode: CREATE_UNCHECKED,
                        attributes: None,
                        verf: None,
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&create).unwrap();
    let create_status = read_create_res(&mut body).unwrap().status;
    body.end("CREATE embedded NUL response").unwrap();

    let lookup = exchange(
        &mut stream,
        encode_call(
            4,
            NFS_PROGRAM,
            NFS_V3,
            NFSPROC3_LOOKUP,
            None,
            None,
            &encode_xdr(|writer| {
                write_dir_op(
                    writer,
                    &DirOpArgs {
                        dir: root,
                        name: "bad\0lookup".to_owned(),
                    },
                )
            }),
        ),
    )
    .await;
    let (_, mut body) = decode_reply(&lookup).unwrap();
    let lookup_status = read_lookup_res(&mut body).unwrap().status;
    body.end("LOOKUP embedded NUL response").unwrap();

    assert_eq!(
        [mkdir_status, create_status, lookup_status],
        [NFS3ERR_INVAL; 3],
        "all three RPCs must reject embedded NUL names"
    );
    let entries = backing.readdir("/").await.unwrap();
    assert!(
        entries.iter().all(|entry| !entry.name.starts_with("bad")),
        "invalid names must not create backing entries"
    );
    server.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rootless_tcp_round_trip_uses_real_filesystem_operations() {
    let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
    let address = server.listen().await.unwrap();
    let mut stream = TcpStream::connect(address).await.unwrap();

    let mount_call = encode_call(
        1,
        MOUNT_PROGRAM,
        MOUNT_V3,
        MOUNTPROC3_MNT,
        None,
        None,
        &encode_xdr(|writer| writer.string("/")),
    );
    let record = exchange(&mut stream, mount_call).await;
    let (reply, mut body) = decode_reply(&record).unwrap();
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let mount = read_mount_res(&mut body).unwrap();
    body.end("MOUNT response").unwrap();
    assert_eq!(mount.status, 0);
    let root = mount.fh.unwrap();

    let create_args = encode_xdr(|writer| {
        write_create_args(
            writer,
            &Create3args {
                where_: DirOpArgs {
                    dir: root.clone(),
                    name: "wire.txt".to_owned(),
                },
                mode: CREATE_UNCHECKED,
                attributes: Some(Sattr3 {
                    mode: Some(0o644),
                    ..Sattr3::default()
                }),
                verf: None,
            },
        )
    });
    let create_call = encode_call(
        2,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_CREATE,
        None,
        None,
        &create_args,
    );
    let record = exchange(&mut stream, create_call).await;
    let (reply, mut body) = decode_reply(&record).unwrap();
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let created = read_create_res(&mut body).unwrap();
    body.end("CREATE response").unwrap();
    assert_eq!(created.status, NFS3_OK);
    let file = created.obj.unwrap();

    let write_args = encode_xdr(|writer| {
        write_write_args(
            writer,
            &Write3args {
                file: file.clone(),
                offset: 0,
                count: 11,
                stable: FILE_SYNC,
                data: b"hello wire!".to_vec(),
            },
        )
    });
    let write_call = encode_call(
        3,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_WRITE,
        None,
        None,
        &write_args,
    );
    let record = exchange(&mut stream, write_call).await;
    let (reply, mut body) = decode_reply(&record).unwrap();
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let written = read_write_res(&mut body).unwrap();
    body.end("WRITE response").unwrap();
    assert_eq!(written.status, NFS3_OK);
    assert_eq!(written.count, 11);
    assert_eq!(written.committed, FILE_SYNC);

    let read_args = encode_xdr(|writer| {
        write_read_args(
            writer,
            &Read3args {
                file,
                offset: 0,
                count: 64,
            },
        )
    });
    let read_call = encode_call(
        4,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_READ,
        None,
        None,
        &read_args,
    );
    let record = exchange(&mut stream, read_call).await;
    let (reply, mut body) = decode_reply(&record).unwrap();
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let read = read_read_res(&mut body, 64).unwrap();
    body.end("READ response").unwrap();
    assert_eq!(read.status, NFS3_OK);
    assert_eq!(read.count, 11);
    assert_eq!(read.data, b"hello wire!");

    server.close().await.unwrap();
}
