use std::sync::Arc;

use async_trait::async_trait;

use crate::error::{FsError, Result};
use crate::path::normalize_path;
use crate::types::{Capabilities, DirEntry, MkdirOptions, Stats, StatsFs};

/// An open file handle. The buffer passed to `read` is owned by the caller;
/// `position == None` uses and advances the handle cursor, while `Some` is an
/// explicit positional operation.
#[async_trait]
pub trait FileHandle: Send + Sync {
    fn fd(&self) -> Option<u64> {
        None
    }
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize>;
    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize>;
    async fn stat(&self) -> Result<Stats>;
    async fn truncate(&self, length: u64) -> Result<()>;
    async fn sync(&self) -> Result<()> {
        Ok(())
    }
    async fn datasync(&self) -> Result<()> {
        Ok(())
    }
    async fn close(&self) -> Result<()>;
}

/// The async filesystem driver contract shared by every backend.
#[async_trait]
pub trait FsDriver: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    /// Establish a filesystem-wide persistence barrier.
    ///
    /// Volatile drivers have nothing to flush and therefore succeed by
    /// default. A driver that advertises durable writes must override this
    /// method with its backend-specific durability operation; otherwise the
    /// caller receives the same explicit unsupported result used by the
    /// transport contract instead of a false durability acknowledgement.
    async fn syncfs(&self) -> Result<()> {
        if self.capabilities().durable_writes {
            Err(crate::error::FsError::enosys("syncfs"))
        } else {
            Ok(())
        }
    }

    async fn stat(&self, path: &str) -> Result<Stats>;
    async fn lstat(&self, _path: &str) -> Result<Stats> {
        Err(crate::error::FsError::enosys("lstat"))
    }
    async fn statfs(&self, _path: &str) -> Result<StatsFs> {
        Err(crate::error::FsError::enosys("statfs"))
    }
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>>;
    /// Enumerate at most `max_entries` directory entries.
    ///
    /// HTTP and other remotely reachable adapters must use this method before
    /// serializing a directory response. Drivers that cannot enforce the
    /// bound at their enumeration boundary fail closed instead of falling back
    /// to [`FsDriver::readdir`], which may materialize an unbounded listing.
    async fn readdir_bounded(&self, _path: &str, _max_entries: usize) -> Result<Vec<DirEntry>> {
        Err(crate::error::FsError::enotsup("scandir"))
    }
    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>>;

    /// Open using decoded flags. Transports decode their own flag namespace.
    async fn open_flags(
        &self,
        path: &str,
        flags: crate::OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        for name in ["r", "r+", "w", "wx", "w+", "wx+", "a", "ax", "a+", "ax+"] {
            if crate::OpenFlags::parse(name, path)? == flags {
                return self.open(path, name, mode).await;
            }
        }
        Err(crate::FsError::enotsup("open").with_path(path))
    }

    /// Replace a complete file through the driver's most efficient safe
    /// path. Drivers that do not have an atomic whole-file implementation use
    /// the ordinary open/write/close sequence. A specialized implementation
    /// must acknowledge only after immutable bytes are durable and the
    /// namespace publication is fenced and durable.
    async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        let handle = self.open(path, "w", 0o666).await?;
        let operation = async {
            let mut written = 0;
            while written < data.len() {
                let count = handle.write(&data[written..], Some(written as u64)).await?;
                if count == 0 || count > data.len() - written {
                    return Err(crate::error::FsError::new(crate::error::ErrorCode::Eio)
                        .with_syscall("write")
                        .with_path(path));
                }
                written += count;
            }
            Ok(())
        }
        .await;
        handle.close().await?;
        operation
    }

    /// Creates a directory, returning the first created path for recursive
    /// creation, or `None` when a non-recursive call succeeds or a recursive
    /// call creates nothing. Existing non-recursive targets return `EEXIST`.
    async fn mkdir(&self, _path: &str, _options: MkdirOptions) -> Result<Option<String>> {
        Err(crate::error::FsError::enosys("mkdir"))
    }
    async fn rmdir(&self, _path: &str) -> Result<()> {
        Err(crate::error::FsError::enosys("rmdir"))
    }
    async fn unlink(&self, _path: &str) -> Result<()> {
        Err(crate::error::FsError::enosys("unlink"))
    }
    async fn rename(&self, _old_path: &str, _new_path: &str) -> Result<()> {
        Err(crate::error::FsError::enosys("rename"))
    }
    async fn link(&self, _existing_path: &str, _new_path: &str) -> Result<()> {
        Err(crate::error::FsError::enosys("link"))
    }
    async fn symlink(&self, _target: &str, _path: &str) -> Result<()> {
        Err(crate::error::FsError::enosys("symlink"))
    }
    async fn readlink(&self, _path: &str) -> Result<String> {
        Err(crate::error::FsError::enosys("readlink"))
    }
    async fn chmod(&self, _path: &str, _mode: u32) -> Result<()> {
        Err(crate::error::FsError::enosys("chmod"))
    }
    async fn chown(&self, _path: &str, _uid: u32, _gid: u32) -> Result<()> {
        Err(crate::error::FsError::enosys("chown"))
    }
    async fn lchown(&self, _path: &str, _uid: u32, _gid: u32) -> Result<()> {
        Err(crate::error::FsError::enosys("lchown"))
    }
    async fn truncate(&self, _path: &str, _length: u64) -> Result<()> {
        Err(crate::error::FsError::enosys("truncate"))
    }
    /// Whether the optional exact-nanosecond extension is implemented.
    fn has_utimens(&self) -> bool {
        false
    }
    /// Exact timestamps use i128 to cover the full signed 64-bit seconds
    /// carried by filesystem wire protocols. Missing extensions return ENOSYS.
    async fn utimens(
        &self,
        _path: &str,
        _atime_ns: i128,
        _mtime_ns: i128,
        _follow_symlinks: bool,
    ) -> Result<()> {
        Err(crate::error::FsError::enosys("utimens"))
    }
    async fn utimes(&self, _path: &str, _atime_ms: i64, _mtime_ms: i64) -> Result<()> {
        Err(crate::error::FsError::enosys("utime"))
    }
    async fn lutimes(&self, _path: &str, _atime_ms: i64, _mtime_ms: i64) -> Result<()> {
        Err(crate::error::FsError::enosys("lutime"))
    }
    async fn mknod(&self, _path: &str, _mode: u32, _dev: u64) -> Result<()> {
        Err(crate::error::FsError::enosys("mknod"))
    }
}

/// Build a directory result from a lazy iterator while enforcing the caller's
/// entry bound before the result can grow beyond it.
pub fn collect_bounded_dir_entries<I>(
    path: &str,
    max_entries: usize,
    entries: I,
) -> Result<Vec<DirEntry>>
where
    I: IntoIterator<Item = DirEntry>,
{
    if max_entries == 0 {
        return Err(FsError::new(crate::error::ErrorCode::Einval)
            .with_syscall("scandir")
            .with_path(path)
            .with_message("directory entry limit must be positive"));
    }
    let mut output = Vec::new();
    for entry in entries {
        if output.len() == max_entries {
            return Err(FsError::new(crate::error::ErrorCode::Eoverflow)
                .with_syscall("scandir")
                .with_path(path)
                .with_message("directory exceeds the configured entry limit"));
        }
        output.push(entry);
    }
    Ok(output)
}

/// The loopback harness: normalize paths, expose resolved capabilities, and
/// provide whole-file helpers without involving any mount transport.
#[derive(Clone)]
pub struct Loopback {
    pub driver: Arc<dyn FsDriver>,
    pub capabilities: Capabilities,
}

impl Loopback {
    pub fn new<D>(driver: D) -> Self
    where
        D: FsDriver + 'static,
    {
        let capabilities = driver.capabilities();
        Self {
            driver: Arc::new(driver),
            capabilities,
        }
    }

    pub fn from_arc(driver: Arc<dyn FsDriver>) -> Self {
        let capabilities = driver.capabilities();
        Self {
            driver,
            capabilities,
        }
    }

    pub async fn stat(&self, path: &str) -> Result<Stats> {
        self.driver.stat(&normalize_path(path)).await
    }

    pub async fn lstat(&self, path: &str) -> Result<Stats> {
        self.driver.lstat(&normalize_path(path)).await
    }

    pub async fn statfs(&self, path: &str) -> Result<StatsFs> {
        self.driver.statfs(&normalize_path(path)).await
    }

    pub async fn syncfs(&self) -> Result<()> {
        self.driver.syncfs().await
    }

    pub async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.driver.readdir(&normalize_path(path)).await
    }

    pub async fn readdir_bounded(&self, path: &str, max_entries: usize) -> Result<Vec<DirEntry>> {
        self.driver
            .readdir_bounded(&normalize_path(path), max_entries)
            .await
    }

    pub async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.driver.open(&normalize_path(path), flags, mode).await
    }

    pub async fn open_flags(
        &self,
        path: &str,
        flags: crate::OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        self.driver
            .open_flags(&normalize_path(path), flags, mode)
            .await
    }

    pub async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        self.driver.mkdir(&normalize_path(path), options).await
    }

    pub async fn rmdir(&self, path: &str) -> Result<()> {
        self.driver.rmdir(&normalize_path(path)).await
    }
    pub async fn unlink(&self, path: &str) -> Result<()> {
        self.driver.unlink(&normalize_path(path)).await
    }

    pub async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        self.driver
            .rename(&normalize_path(old_path), &normalize_path(new_path))
            .await
    }

    pub async fn link(&self, old_path: &str, new_path: &str) -> Result<()> {
        self.driver
            .link(&normalize_path(old_path), &normalize_path(new_path))
            .await
    }

    pub async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        self.driver.symlink(target, &normalize_path(path)).await
    }

    pub async fn readlink(&self, path: &str) -> Result<String> {
        self.driver.readlink(&normalize_path(path)).await
    }

    pub async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        self.driver.chmod(&normalize_path(path), mode).await
    }

    pub async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.driver.chown(&normalize_path(path), uid, gid).await
    }

    pub async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.driver.lchown(&normalize_path(path), uid, gid).await
    }

    pub async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        self.driver.truncate(&normalize_path(path), length).await
    }

    pub async fn utimens(
        &self,
        path: &str,
        atime_ns: i128,
        mtime_ns: i128,
        follow_symlinks: bool,
    ) -> Result<()> {
        self.driver
            .utimens(&normalize_path(path), atime_ns, mtime_ns, follow_symlinks)
            .await
    }

    pub async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.driver
            .utimes(&normalize_path(path), atime_ms, mtime_ms)
            .await
    }

    pub async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.driver
            .lutimes(&normalize_path(path), atime_ms, mtime_ms)
            .await
    }

    pub async fn mknod(&self, path: &str, mode: u32, dev: u64) -> Result<()> {
        self.driver.mknod(&normalize_path(path), mode, dev).await
    }

    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        let handle = self.open(path, "r", 0).await?;
        let operation = async {
            let mut result = Vec::new();
            let mut position = 0_u64;
            loop {
                let mut buffer = vec![0_u8; 64 * 1024];
                let count = handle.read(&mut buffer, Some(position)).await?;
                if count > buffer.len() {
                    return Err(crate::error::FsError::new(crate::error::ErrorCode::Eio)
                        .with_syscall("read")
                        .with_path(path));
                }
                if count == 0 {
                    break;
                }
                result.extend_from_slice(&buffer[..count]);
                position += count as u64;
            }
            Ok(result)
        }
        .await;
        handle.close().await?;
        operation
    }

    pub async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        self.driver.write_file(&normalize_path(path), data).await
    }
}
