//! Rooted host-filesystem driver.
//!
//! This is the Rust counterpart of mountx's `node:fs` driver. Every virtual
//! path is walked component by component below the configured root. Symlinks
//! are followed only after their target has been rewritten back into that
//! virtual root, so an absolute or relative host symlink cannot escape it.
//!
//! Resolution and the final host syscall intentionally remain separate, just
//! like the upstream implementation: a component can still be renamed between
//! the walk and the syscall. Closing that race portably would require
//! descriptor-relative walking and platform-specific handling (`openat2` is
//! Linux-only). This implementation is not a sandbox against concurrent host
//! namespace changes.

use std::collections::{HashMap, VecDeque};
#[cfg(not(windows))]
use std::fs::Metadata;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mount_rs_core::path::{normalize_path, split_path};
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FileType as CoreFileType, FsDriver, FsError,
    MkdirOptions, OpenFlags, Result, Stats, StatsFs,
};

#[cfg(unix)]
use std::ffi::CString;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd};

const MAX_SYMLINK_DEPTH: usize = 40;

#[cfg(windows)]
mod windows;

/// Windows requires a file/directory bit even for a dangling symbolic link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostSymlinkType {
    File,
    Directory,
}

/// Options for [`HostFs`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostFsOptions {
    /// Report the filesystem as read-only without enforcing it locally.
    pub read_only: bool,
}

/// A host filesystem rooted at one directory.
#[derive(Clone)]
pub struct HostFs {
    root: Arc<PathBuf>,
    options: HostFsOptions,
}

/// Alias useful to callers that name drivers by their role.
pub type HostFsDriver = HostFs;

impl HostFs {
    /// Create a driver rooted at `root` with default options.
    ///
    /// The root is made absolute lexically, matching Node's `resolve()`
    /// behavior. Construction does not require the directory to exist; the
    /// first host operation then returns the corresponding filesystem error.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self::with_options(root, HostFsOptions::default())
    }

    /// Create a driver with explicit reporting options.
    pub fn with_options(root: impl AsRef<Path>, options: HostFsOptions) -> Self {
        let root = lexical_absolute(root.as_ref());
        Self {
            root: Arc::new(root),
            options,
        }
    }

    /// The host directory used as the virtual `/`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The options with which this driver was created.
    pub const fn options(&self) -> HostFsOptions {
        self.options
    }

    /// Create a symlink with an optional Windows type hint. With no hint,
    /// infer the host target's type like Node; a missing target means file.
    /// The portable FsDriver method has no type parameter and uses inference.
    pub async fn symlink_with_type(
        &self,
        target: &str,
        path: &str,
        kind: Option<HostSymlinkType>,
    ) -> Result<()> {
        let real = self.secure(path, false, "symlink").await?;
        let error_path = real.clone();
        let target = target.to_owned();
        let target_error = target.clone();
        run_blocking(move || {
            #[cfg(unix)]
            let result = {
                let _ = kind;
                std::os::unix::fs::symlink(&target, &real)
            };
            #[cfg(windows)]
            let result = {
                let directory = kind
                    .map(|kind| kind == HostSymlinkType::Directory)
                    .unwrap_or_else(|| {
                        real.parent()
                            .unwrap_or(Path::new("/"))
                            .join(&target)
                            .is_dir()
                    });
                if directory {
                    std::os::windows::fs::symlink_dir(&target, &real)
                } else {
                    std::os::windows::fs::symlink_file(&target, &real)
                }
            };
            #[cfg(not(any(unix, windows)))]
            let result = {
                let _ = kind;
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "host symlink unavailable",
                ))
            };
            result.map_err(|error| {
                fs_error_from_io_with_dest(error, "symlink", target_error, path_string(&error_path))
            })
        })
        .await
    }

    async fn secure(&self, path: &str, follow: bool, syscall: &'static str) -> Result<PathBuf> {
        let mut seen = HashMap::new();
        self.secure_with_memo(path, follow, syscall, &mut seen)
            .await
    }

    async fn secure_with_memo(
        &self,
        path: &str,
        follow: bool,
        syscall: &'static str,
        seen: &mut HashMap<PathBuf, ComponentKind>,
    ) -> Result<PathBuf> {
        let virtual_path = normalize_path(path);
        let mut pending = VecDeque::from(split_path(&virtual_path));
        let mut current = self.root.as_ref().clone();
        let mut depth = 0_usize;
        let mut links = 0_usize;

        while let Some(name) = pending.pop_front() {
            if name == "." {
                continue;
            }
            if name == ".." {
                if depth > 0 {
                    let _ = current.pop();
                    depth -= 1;
                }
                continue;
            }

            // A virtual POSIX component must not become a drive, ADS, device
            // name or a backslash-separated host path on Windows.
            #[cfg(windows)]
            if !windows_component_is_safe(&name) {
                return Err(FsError::new(ErrorCode::Einval)
                    .with_syscall(syscall)
                    .with_path(virtual_path));
            }
            let candidate = current.join(&name);
            let last = pending.is_empty();
            if last && !follow {
                return Ok(candidate);
            }

            let component = if let Some(component) = seen.get(&candidate).cloned() {
                component
            } else {
                let component = look_at(candidate.clone())
                    .await
                    .map_err(|error| resolution_error(error, syscall, &virtual_path))?;
                seen.insert(candidate.clone(), component.clone());
                component
            };
            match component {
                ComponentKind::Missing | ComponentKind::Other => {
                    current = candidate;
                    depth += 1;
                }
                ComponentKind::Symlink(target) => {
                    links += 1;
                    if links > MAX_SYMLINK_DEPTH {
                        return Err(FsError::new(ErrorCode::Eloop)
                            .with_syscall(syscall)
                            .with_path(virtual_path));
                    }
                    let (absolute, target_segments) = symlink_segments(&target);
                    if absolute {
                        current = self.root.as_ref().clone();
                        depth = 0;
                    }
                    for segment in target_segments.into_iter().rev() {
                        pending.push_front(segment);
                    }
                }
            }
        }
        Ok(current)
    }

    async fn secure_pair(
        &self,
        first: &str,
        second: &str,
        syscall: &'static str,
    ) -> Result<(PathBuf, PathBuf)> {
        // The upstream driver shares one per-call memo for both walks and
        // reports the first path's failure. Resolving in this order preserves
        // that observable rule while retaining the shared component answers.
        let mut seen = HashMap::new();
        let first = self
            .secure_with_memo(first, false, syscall, &mut seen)
            .await?;
        let second = self
            .secure_with_memo(second, false, syscall, &mut seen)
            .await?;
        Ok((first, second))
    }

    fn virtual_path(&self, path: &Path) -> Option<String> {
        virtual_path_for_root(&self.root, path)
    }
}

#[derive(Debug, Clone)]
enum ComponentKind {
    Missing,
    Symlink(PathBuf),
    Other,
}

fn symlink_segments(target: &Path) -> (bool, Vec<String>) {
    #[cfg(windows)]
    {
        // Windows readlink uses backslashes and may carry a drive/UNC prefix.
        // Strip the prefix and replay components inside the virtual root;
        // never hand a rooted/prefixed segment to PathBuf::join.
        let absolute =
            target.has_root() || matches!(target.components().next(), Some(Component::Prefix(_)));
        let segments = target
            .components()
            .filter_map(|component| match component {
                Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                Component::ParentDir => Some("..".to_owned()),
                Component::CurDir => Some(".".to_owned()),
                _ => None,
            })
            .collect();
        (absolute, segments)
    }
    #[cfg(not(windows))]
    {
        (
            target.is_absolute(),
            target
                .to_string_lossy()
                .split('/')
                .filter(|part| !part.is_empty())
                .map(ToOwned::to_owned)
                .collect(),
        )
    }
}

#[cfg(windows)]
fn windows_component_is_safe(name: &str) -> bool {
    if name.contains(['\\', ':', '\0']) || name.ends_with(['.', ' ']) {
        return false;
    }
    let base = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    !matches!(
        base.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) && !(base.len() == 4
        && (base.starts_with("COM") || base.starts_with("LPT"))
        && matches!(base.as_bytes()[3], b'1'..=b'9'))
}

async fn look_at(candidate: PathBuf) -> io::Result<ComponentKind> {
    run_blocking_io(move || match fs::symlink_metadata(&candidate) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Ok(ComponentKind::Symlink(fs::read_link(candidate)?))
        }
        Ok(_) => Ok(ComponentKind::Other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(ComponentKind::Missing),
        Err(error) => Err(error),
    })
    .await
}

async fn run_blocking<T, F>(operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| FsError::backend(format!("host filesystem worker failed: {error}")))?
}

async fn run_blocking_io<T, F>(operation: F) -> io::Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> io::Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(|error| io::Error::other(format!("host filesystem worker failed: {error}")))?
}

fn lexical_absolute(path: &Path) -> PathBuf {
    let raw = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    };
    let mut result = PathBuf::new();
    for component in raw.components() {
        match component {
            Component::Prefix(prefix) => result.push(prefix.as_os_str()),
            Component::RootDir => result.push(Path::new("/")),
            Component::CurDir => {}
            Component::ParentDir => {
                if result != Path::new("/") {
                    let _ = result.pop();
                }
            }
            Component::Normal(name) => result.push(name),
        }
    }
    if result.as_os_str().is_empty() {
        PathBuf::from("/")
    } else {
        result
    }
}

fn virtual_path_for_root(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    if relative.as_os_str().is_empty() {
        return Some("/".to_owned());
    }
    let mut result = String::new();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return None;
        };
        result.push('/');
        result.push_str(&name.to_string_lossy());
    }
    Some(if result.is_empty() {
        "/".to_owned()
    } else {
        result
    })
}

fn resolution_error(error: io::Error, syscall: &'static str, path: &str) -> FsError {
    fs_error_from_io(error, syscall, path.to_owned())
}

fn fs_error_from_io(error: io::Error, syscall: &'static str, path: String) -> FsError {
    FsError::new(error_code(&error))
        .with_syscall(syscall)
        .with_path(path)
}

fn fs_error_from_io_with_dest(
    error: io::Error,
    syscall: &'static str,
    path: String,
    dest: String,
) -> FsError {
    FsError::new(error_code(&error))
        .with_syscall(syscall)
        .with_path(path)
        .with_dest(dest)
}

#[cfg(unix)]
fn raw_error_code(errno: i32) -> Option<ErrorCode> {
    Some(match errno {
        libc::EPERM => ErrorCode::Eperm,
        libc::ENOENT => ErrorCode::Enoent,
        libc::EINTR => ErrorCode::Eintr,
        libc::EIO => ErrorCode::Eio,
        libc::ENXIO => ErrorCode::Enxio,
        libc::EBADF => ErrorCode::Ebadf,
        libc::EAGAIN => ErrorCode::Eagain,
        libc::ENOMEM => ErrorCode::Enomem,
        libc::EACCES => ErrorCode::Eacces,
        libc::EBUSY => ErrorCode::Ebusy,
        libc::EEXIST => ErrorCode::Eexist,
        libc::EXDEV => ErrorCode::Exdev,
        libc::ENODEV => ErrorCode::Enodev,
        libc::ENOTDIR => ErrorCode::Enotdir,
        libc::EISDIR => ErrorCode::Eisdir,
        libc::EINVAL => ErrorCode::Einval,
        libc::ENFILE => ErrorCode::Enfile,
        libc::EMFILE => ErrorCode::Emfile,
        libc::EFBIG => ErrorCode::Efbig,
        libc::ENOSPC => ErrorCode::Enospc,
        libc::ESPIPE => ErrorCode::Espipe,
        libc::EROFS => ErrorCode::Erofs,
        libc::EMLINK => ErrorCode::Emlink,
        libc::ERANGE => ErrorCode::Erange,
        libc::ENAMETOOLONG => ErrorCode::Enametoolong,
        libc::ENOSYS => ErrorCode::Enosys,
        libc::ENOTEMPTY => ErrorCode::Enotempty,
        libc::ELOOP => ErrorCode::Eloop,
        libc::ENOTSUP => ErrorCode::Enotsup,
        libc::ENODATA => ErrorCode::Enodata,
        libc::EDQUOT => ErrorCode::Edquot,
        libc::EPROTO => ErrorCode::Eproto,
        libc::EOVERFLOW => ErrorCode::Eoverflow,
        libc::ESTALE => ErrorCode::Estale,
        _ => return None,
    })
}

// Win32 errors are not POSIX errno values. Match libuv's win/error.c for
// filesystem errors, falling back to ErrorKind for codes not listed here.
#[cfg(windows)]
fn raw_error_code(code: i32) -> Option<ErrorCode> {
    Some(match code {
        1 => ErrorCode::Eisdir, // ERROR_INVALID_FUNCTION
        2 | 3 | 15 | 123 | 161 | 203 | 267 | 4392 => ErrorCode::Enoent,
        4 => ErrorCode::Emfile,
        5 | 1314 => ErrorCode::Eperm, // ACCESS_DENIED / PRIVILEGE_NOT_HELD
        6 | 1004 => ErrorCode::Ebadf, // INVALID_HANDLE / INVALID_FLAGS
        8 | 14 => ErrorCode::Enomem,
        13 | 87 | 122 => ErrorCode::Einval,
        17 => ErrorCode::Exdev,
        19 => ErrorCode::Erofs,
        32 | 33 | 231 => ErrorCode::Ebusy,
        39 | 82 | 112 => ErrorCode::Enospc,
        50 => ErrorCode::Enotsup,
        80 | 183 => ErrorCode::Eexist,
        111 | 206 => ErrorCode::Enametoolong,
        145 => ErrorCode::Enotempty,
        232 => ErrorCode::Eagain,
        740 | 1920 => ErrorCode::Eacces,
        1921 => ErrorCode::Eloop,
        _ => return None,
    })
}

#[cfg(not(any(unix, windows)))]
fn raw_error_code(_code: i32) -> Option<ErrorCode> {
    None
}

fn error_code(error: &io::Error) -> ErrorCode {
    use io::ErrorKind;

    if let Some(errno) = error.raw_os_error().and_then(raw_error_code) {
        return errno;
    }
    match error.kind() {
        ErrorKind::NotFound => ErrorCode::Enoent,
        ErrorKind::IsADirectory => ErrorCode::Eisdir,
        ErrorKind::NotADirectory => ErrorCode::Enotdir,
        ErrorKind::DirectoryNotEmpty => ErrorCode::Enotempty,
        ErrorKind::PermissionDenied => ErrorCode::Eacces,
        ErrorKind::ConnectionRefused | ErrorKind::ConnectionReset => ErrorCode::Eio,
        ErrorKind::AlreadyExists => ErrorCode::Eexist,
        ErrorKind::WouldBlock => ErrorCode::Eagain,
        ErrorKind::InvalidInput | ErrorKind::InvalidData => ErrorCode::Einval,
        ErrorKind::TimedOut => ErrorCode::Eio,
        ErrorKind::Interrupted => ErrorCode::Eintr,
        ErrorKind::Unsupported => ErrorCode::Enotsup,
        ErrorKind::UnexpectedEof | ErrorKind::WriteZero => ErrorCode::Eio,
        ErrorKind::OutOfMemory => ErrorCode::Enomem,
        ErrorKind::Other => ErrorCode::Eio,
        _ => ErrorCode::Eio,
    }
}

fn file_type(file_type: std::fs::FileType) -> CoreFileType {
    if file_type.is_file() {
        CoreFileType::File
    } else if file_type.is_dir() {
        CoreFileType::Directory
    } else if file_type.is_symlink() {
        CoreFileType::Symlink
    } else {
        #[cfg(unix)]
        {
            if file_type.is_block_device() {
                return CoreFileType::BlockDevice;
            }
            if file_type.is_char_device() {
                return CoreFileType::CharacterDevice;
            }
            if file_type.is_fifo() {
                return CoreFileType::Fifo;
            }
            if file_type.is_socket() {
                return CoreFileType::Socket;
            }
        }
        CoreFileType::File
    }
}

#[cfg(unix)]
fn timestamp_ms(seconds: i64, nanoseconds: i64) -> i64 {
    let milliseconds = i128::from(seconds) * 1_000 + i128::from(nanoseconds) / 1_000_000;
    milliseconds.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

#[cfg(unix)]
fn read_file_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buffer, offset)
}

#[cfg(windows)]
fn read_file_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buffer, offset)
}

#[cfg(not(any(unix, windows)))]
fn read_file_at(_file: &File, _buffer: &mut [u8], _offset: u64) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "positional host reads are unavailable on this platform",
    ))
}

#[cfg(unix)]
fn write_file_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buffer, offset)
}

#[cfg(windows)]
fn write_file_at(file: &File, buffer: &[u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buffer, offset)
}

#[cfg(not(any(unix, windows)))]
fn write_file_at(_file: &File, _buffer: &[u8], _offset: u64) -> io::Result<usize> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "positional host writes are unavailable on this platform",
    ))
}

#[cfg(unix)]
fn stats_from_metadata(metadata: &Metadata) -> Stats {
    #[cfg(target_os = "macos")]
    use std::os::darwin::fs::MetadataExt as DarwinMetadataExt;

    Stats {
        dev: metadata.dev(),
        ino: metadata.ino(),
        mode: metadata.mode(),
        nlink: metadata.nlink(),
        uid: metadata.uid(),
        gid: metadata.gid(),
        rdev: metadata.rdev(),
        size: metadata.size(),
        blksize: metadata.blksize(),
        blocks: metadata.blocks(),
        atime_ms: timestamp_ms(metadata.atime(), metadata.atime_nsec()),
        mtime_ms: timestamp_ms(metadata.mtime(), metadata.mtime_nsec()),
        ctime_ms: timestamp_ms(metadata.ctime(), metadata.ctime_nsec()),
        #[cfg(target_os = "macos")]
        birthtime_ms: timestamp_ms(metadata.st_birthtime(), metadata.st_birthtime_nsec()),
        #[cfg(not(target_os = "macos"))]
        birthtime_ms: 0,
    }
}

#[cfg(not(any(unix, windows)))]
fn stats_from_metadata(metadata: &Metadata) -> Stats {
    let kind = file_type(metadata.file_type());
    Stats {
        dev: 0,
        ino: 0,
        mode: kind.mode_bits()
            | if kind == CoreFileType::Directory {
                0o755
            } else {
                0o666
            },
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        size: metadata.len(),
        blksize: 4096,
        blocks: metadata.len().div_ceil(512),
        atime_ms: 0,
        mtime_ms: 0,
        ctime_ms: 0,
        birthtime_ms: 0,
    }
}

#[cfg(unix)]
fn path_to_cstring(path: &Path) -> io::Result<CString> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "host filesystem path contains an embedded NUL",
        )
    })
}

#[cfg(unix)]
fn open_host(path: &Path, flags: OpenFlags, mode: u32) -> io::Result<File> {
    let path = path_to_cstring(path)?;
    let access = match (flags.read, flags.write) {
        (true, false) => libc::O_RDONLY,
        (false, true) => libc::O_WRONLY,
        (true, true) => libc::O_RDWR,
        (false, false) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "open requires read or write access",
            ));
        }
    };
    let mut host_flags = access | libc::O_CLOEXEC;
    if flags.create {
        host_flags |= libc::O_CREAT;
    }
    if flags.truncate {
        host_flags |= libc::O_TRUNC;
    }
    if flags.append {
        host_flags |= libc::O_APPEND;
    }
    if flags.exclusive {
        host_flags |= libc::O_EXCL;
    }
    // SAFETY: `path` is a valid NUL-terminated pathname and `mode` is only
    // read when O_CREAT is present by the host syscall.
    let fd = unsafe { libc::open(path.as_ptr(), host_flags, mode as libc::c_uint) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor owned by this `File` after success.
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(windows)]
fn open_host(path: &Path, flags: OpenFlags, mode: u32) -> io::Result<File> {
    windows::open(path, flags, mode)
}

#[cfg(not(any(unix, windows)))]
fn open_host(path: &Path, flags: OpenFlags, mode: u32) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(flags.read)
        .write(flags.write)
        .create(flags.create)
        .truncate(flags.truncate)
        .append(flags.append)
        .create_new(flags.create && flags.exclusive);
    let _ = mode;
    options.open(path)
}

#[cfg(unix)]
fn statfs_host(path: &Path) -> io::Result<StatsFs> {
    let path = path_to_cstring(path)?;
    // SAFETY: `info` is an output buffer and `path` is a valid NUL-terminated
    // pathname owned for the duration of the call.
    let mut info: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(path.as_ptr(), &mut info) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(StatsFs {
        filesystem_type: info.f_type as u64,
        block_size: info.f_bsize as u64,
        blocks: info.f_blocks as u64,
        blocks_free: info.f_bfree as u64,
        blocks_available: info.f_bavail as u64,
        files: info.f_files as u64,
        files_free: info.f_ffree as u64,
    })
}

#[cfg(windows)]
fn statfs_host(path: &Path) -> io::Result<StatsFs> {
    windows::statfs(path)
}

#[cfg(not(any(unix, windows)))]
fn statfs_host(_path: &Path) -> io::Result<StatsFs> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "host statfs is unavailable on this platform",
    ))
}

fn stat_host(path: &Path, follow: bool) -> io::Result<Stats> {
    #[cfg(windows)]
    {
        windows::stat(path, follow)
    }
    #[cfg(not(windows))]
    {
        (if follow {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        })
        .map(|metadata| stats_from_metadata(&metadata))
    }
}

fn fstat_host(file: &File) -> io::Result<Stats> {
    #[cfg(windows)]
    {
        windows::fstat(file)
    }
    #[cfg(not(windows))]
    {
        file.metadata()
            .map(|metadata| stats_from_metadata(&metadata))
    }
}

fn mkdir_host(real: &Path, root: &Path, options: MkdirOptions) -> io::Result<Option<PathBuf>> {
    let mode = options.mode.unwrap_or(0o777);
    if !options.recursive {
        create_dir_host(real, mode)?;
        return Ok(None);
    }
    let mut missing = Vec::new();
    let mut probe = real.to_owned();
    loop {
        match fs::symlink_metadata(&probe) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                missing.push(probe.clone());
                if probe == root || !probe.pop() {
                    break;
                }
            }
            Err(error) => return Err(error),
        }
    }
    if missing.is_empty() {
        match fs::metadata(real) {
            Ok(metadata) if metadata.is_dir() => return Ok(None),
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "path exists and is not a directory",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "path exists and is not a directory",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    for directory in missing.iter().rev() {
        match create_dir_host(directory, mode) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::metadata(directory)?.is_dir() {
                    return Err(error);
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(missing.pop())
}

#[cfg(unix)]
fn create_dir_host(path: &Path, mode: u32) -> io::Result<()> {
    let path = path_to_cstring(path)?;
    // `mkdir(2)` applies the process umask, matching Node's host-filesystem
    // behavior while still honoring a caller-provided mode.
    let result = unsafe { libc::mkdir(path.as_ptr(), mode as libc::mode_t) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(unix))]
fn create_dir_host(path: &Path, _mode: u32) -> io::Result<()> {
    fs::create_dir(path)
}

#[cfg(unix)]
fn chmod_host(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o7777))
}

#[cfg(windows)]
fn chmod_host(path: &Path, mode: u32) -> io::Result<()> {
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_readonly(mode & 0o200 == 0);
    fs::set_permissions(path, permissions)
}

#[cfg(not(any(unix, windows)))]
fn chmod_host(_path: &Path, _mode: u32) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "host chmod is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn chown_host(path: &Path, uid: u32, gid: u32, follow: bool) -> io::Result<()> {
    let path = path_to_cstring(path)?;
    let result = unsafe {
        if follow {
            libc::chown(path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t)
        } else {
            libc::lchown(path.as_ptr(), uid as libc::uid_t, gid as libc::gid_t)
        }
    };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn chown_host(_path: &Path, _uid: u32, _gid: u32, _follow: bool) -> io::Result<()> {
    // libuv fs__chown/fs__lchown deliberately succeed without a syscall on
    // Windows, even for missing paths. This is not emulated POSIX ownership.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn chown_host(_path: &Path, _uid: u32, _gid: u32, _follow: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "host chown is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn timespec_from_millis(milliseconds: i64) -> libc::timespec {
    libc::timespec {
        tv_sec: milliseconds.div_euclid(1_000) as libc::time_t,
        tv_nsec: (milliseconds.rem_euclid(1_000) * 1_000_000) as libc::c_long,
    }
}

#[cfg(unix)]
fn utimes_host(path: &Path, atime_ms: i64, mtime_ms: i64, follow: bool) -> io::Result<()> {
    let path = path_to_cstring(path)?;
    let times = [
        timespec_from_millis(atime_ms),
        timespec_from_millis(mtime_ms),
    ];
    let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
    let result = unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), flags) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn utimes_host(path: &Path, atime_ms: i64, mtime_ms: i64, follow: bool) -> io::Result<()> {
    windows::utimes(path, atime_ms, mtime_ms, follow)
}

#[cfg(not(any(unix, windows)))]
fn utimes_host(_path: &Path, _atime_ms: i64, _mtime_ms: i64, _follow: bool) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "host utimes is unavailable on this platform",
    ))
}

struct HandleState {
    file: Option<File>,
    flags: OpenFlags,
    position: u64,
}

struct HostHandle {
    state: Arc<Mutex<HandleState>>,
    path: String,
    fd: Option<u64>,
}

#[async_trait]
impl FileHandle for HostHandle {
    fn fd(&self) -> Option<u64> {
        self.fd
    }

    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let state = Arc::clone(&self.state);
        let path = self.path.clone();
        let length = buffer.len();
        let (bytes, count) = run_blocking(move || {
            let mut state = state
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("read"))?;
            let file = state.file.as_ref().ok_or_else(|| {
                FsError::new(ErrorCode::Ebadf)
                    .with_syscall("read")
                    .with_path(path.clone())
            })?;
            if !state.flags.read {
                return Err(FsError::new(ErrorCode::Ebadf)
                    .with_syscall("read")
                    .with_path(path.clone()));
            }
            if file
                .metadata()
                .map_err(|error| fs_error_from_io(error, "read", path.clone()))?
                .is_dir()
            {
                // Node's FileHandle.read() reports EISDIR with its syscall,
                // but does not attach the handle's path. Keep path context
                // for closed handles and backend failures below.
                return Err(FsError::new(ErrorCode::Eisdir).with_syscall("read"));
            }
            let start = position.unwrap_or(state.position);
            let mut bytes = vec![0_u8; length];
            let count = read_file_at(file, &mut bytes, start)
                .map_err(|error| fs_error_from_io(error, "read", path.clone()))?;
            if position.is_none() {
                state.position = start.saturating_add(count as u64);
            }
            Ok((bytes, count))
        })
        .await?;
        buffer[..count].copy_from_slice(&bytes[..count]);
        Ok(count)
    }

    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let state = Arc::clone(&self.state);
        let path = self.path.clone();
        let bytes = buffer.to_vec();
        run_blocking(move || {
            let mut state = state
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("write"))?;
            let flags = state.flags;
            if state.file.is_none() {
                return Err(FsError::new(ErrorCode::Ebadf)
                    .with_syscall("write")
                    .with_path(path.clone()));
            }
            if !flags.write {
                // Node's FileHandle.write() reports EBADF with its syscall,
                // but does not attach the handle's path. Keep path context
                // for closed handles and backend failures below.
                return Err(FsError::new(ErrorCode::Ebadf).with_syscall("write"));
            }
            let start = position.unwrap_or(state.position);
            let (count, appended_position) = {
                let file = state.file.as_mut().ok_or_else(|| {
                    FsError::new(ErrorCode::Ebadf)
                        .with_syscall("write")
                        .with_path(path.clone())
                })?;
                if file
                    .metadata()
                    .map_err(|error| fs_error_from_io(error, "write", path.clone()))?
                    .is_dir()
                {
                    return Err(FsError::new(ErrorCode::Eisdir)
                        .with_syscall("write")
                        .with_path(path.clone()));
                }
                let count = if flags.append && position.is_none() {
                    // `pwrite(2)`/`FileExt::write_at` does not consistently honor
                    // O_APPEND across Unix platforms: on macOS it writes at the
                    // supplied offset. Use the descriptor's ordinary write path
                    // for the implicit-position form so the kernel performs the
                    // append atomically on both macOS and Linux. An explicit
                    // position remains positional, matching node:fs.
                    use std::io::Write as _;
                    file.write(&bytes)
                        .map_err(|error| fs_error_from_io(error, "write", path.clone()))?
                } else {
                    write_file_at(file, &bytes, start)
                        .map_err(|error| fs_error_from_io(error, "write", path.clone()))?
                };
                let appended_position = if flags.append && position.is_none() {
                    Some(
                        file.metadata()
                            .map_err(|error| fs_error_from_io(error, "write", path.clone()))?
                            .len(),
                    )
                } else {
                    None
                };
                (count, appended_position)
            };
            if let Some(position) = appended_position {
                state.position = position;
            } else if position.is_none() {
                state.position = start.saturating_add(count as u64);
            }
            Ok(count)
        })
        .await
    }

    async fn stat(&self) -> Result<Stats> {
        let state = Arc::clone(&self.state);
        let path = self.path.clone();
        run_blocking(move || {
            let state = state
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("fstat"))?;
            let file = state.file.as_ref().ok_or_else(|| {
                FsError::new(ErrorCode::Ebadf)
                    .with_syscall("fstat")
                    .with_path(path.clone())
            })?;
            fstat_host(file).map_err(|error| fs_error_from_io(error, "fstat", path))
        })
        .await
    }

    async fn truncate(&self, length: u64) -> Result<()> {
        let state = Arc::clone(&self.state);
        let path = self.path.clone();
        run_blocking(move || {
            let state = state
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("ftruncate"))?;
            let file = state.file.as_ref().ok_or_else(|| {
                FsError::new(ErrorCode::Ebadf)
                    .with_syscall("ftruncate")
                    .with_path(path.clone())
            })?;
            if !state.flags.write {
                return Err(FsError::new(ErrorCode::Ebadf)
                    .with_syscall("ftruncate")
                    .with_path(path.clone()));
            }
            file.set_len(length)
                .map_err(|error| fs_error_from_io(error, "ftruncate", path))
        })
        .await
    }

    async fn sync(&self) -> Result<()> {
        let state = Arc::clone(&self.state);
        let path = self.path.clone();
        run_blocking(move || {
            let state = state
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("fsync"))?;
            let file = state.file.as_ref().ok_or_else(|| {
                FsError::new(ErrorCode::Ebadf)
                    .with_syscall("fsync")
                    .with_path(path.clone())
            })?;
            file.sync_all()
                .map_err(|error| fs_error_from_io(error, "fsync", path))
        })
        .await
    }

    async fn datasync(&self) -> Result<()> {
        let state = Arc::clone(&self.state);
        let path = self.path.clone();
        run_blocking(move || {
            let state = state
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("fdatasync"))?;
            let file = state.file.as_ref().ok_or_else(|| {
                FsError::new(ErrorCode::Ebadf)
                    .with_syscall("fdatasync")
                    .with_path(path.clone())
            })?;
            file.sync_data()
                .map_err(|error| fs_error_from_io(error, "fdatasync", path))
        })
        .await
    }

    async fn close(&self) -> Result<()> {
        let state = Arc::clone(&self.state);
        run_blocking(move || {
            let mut state = state
                .lock()
                .map_err(|_| FsError::new(ErrorCode::Eio).with_syscall("close"))?;
            let _ = state.file.take();
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl FsDriver for HostFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            handles: true,
            hardlinks: true,
            symlinks: true,
            permissions: true,
            times: true,
            truncate: true,
            atomic_rename: true,
            case_sensitive: cfg!(target_os = "linux"),
            statfs: true,
            read_only: self.options.read_only,
            durable_writes: false,
            mknod: false,
        }
    }

    async fn stat(&self, path: &str) -> Result<Stats> {
        let real = self.secure(path, true, "stat").await?;
        let error_path = real.clone();
        run_blocking(move || {
            stat_host(&real, true)
                .map_err(|error| fs_error_from_io(error, "stat", path_string(&error_path)))
        })
        .await
    }

    async fn lstat(&self, path: &str) -> Result<Stats> {
        let real = self.secure(path, false, "lstat").await?;
        let error_path = real.clone();
        run_blocking(move || {
            stat_host(&real, false)
                .map_err(|error| fs_error_from_io(error, "lstat", path_string(&error_path)))
        })
        .await
    }

    async fn statfs(&self, path: &str) -> Result<StatsFs> {
        let real = self.secure(path, true, "statfs").await?;
        let error_path = real.clone();
        run_blocking(move || {
            statfs_host(&real)
                .map_err(|error| fs_error_from_io(error, "statfs", path_string(&error_path)))
        })
        .await
    }

    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        let parent_path = normalize_path(path);
        let real = self.secure(path, true, "scandir").await?;
        let error_path = real.clone();
        run_blocking(move || {
            let entries = fs::read_dir(&real)
                .map_err(|error| fs_error_from_io(error, "scandir", path_string(&error_path)))?;
            let mut result = Vec::new();
            for entry in entries {
                let entry = entry.map_err(|error| {
                    fs_error_from_io(error, "scandir", path_string(&error_path))
                })?;
                let entry_type = entry.file_type().map_err(|error| {
                    fs_error_from_io(error, "scandir", path_string(&error_path))
                })?;
                result.push(DirEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    parent_path: parent_path.clone(),
                    file_type: file_type(entry_type),
                });
            }
            Ok(result)
        })
        .await
    }

    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        let normalized = normalize_path(path);
        let flags = OpenFlags::parse(flags, &normalized)?;
        self.open_flags(&normalized, flags, mode).await
    }

    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let normalized = normalize_path(path);
        if !flags.read && !flags.write {
            return Err(FsError::new(ErrorCode::Einval)
                .with_syscall("open")
                .with_path(normalized));
        }
        let real = self.secure(&normalized, !flags.exclusive, "open").await?;
        let error_path = real.clone();
        run_blocking(move || {
            let file = open_host(&real, flags, mode)
                .map_err(|error| fs_error_from_io(error, "open", path_string(&error_path)))?;
            #[cfg(unix)]
            let fd = u64::try_from(file.as_raw_fd()).ok();
            #[cfg(not(unix))]
            let fd = None;
            Ok(Arc::new(HostHandle {
                state: Arc::new(Mutex::new(HandleState {
                    file: Some(file),
                    flags,
                    position: 0,
                })),
                path: normalized,
                fd,
            }) as Arc<dyn FileHandle>)
        })
        .await
    }

    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        let normalized = normalize_path(path);
        let real = self.secure(&normalized, false, "mkdir").await?;
        let error_path = real.clone();
        let root = self.root.as_ref().clone();
        let created = run_blocking(move || {
            mkdir_host(&real, &root, options)
                .map_err(|error| fs_error_from_io(error, "mkdir", path_string(&error_path)))
        })
        .await?;
        Ok(created.and_then(|path| self.virtual_path(&path)))
    }

    async fn rmdir(&self, path: &str) -> Result<()> {
        let normalized = normalize_path(path);
        let real = self.secure(&normalized, false, "rmdir").await?;
        if real == *self.root {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("rmdir")
                .with_path(normalized));
        }
        let error_path = real.clone();
        run_blocking(move || {
            fs::remove_dir(&real)
                .map_err(|error| fs_error_from_io(error, "rmdir", path_string(&error_path)))
        })
        .await
    }

    async fn unlink(&self, path: &str) -> Result<()> {
        let real = self.secure(path, false, "unlink").await?;
        let error_path = real.clone();
        run_blocking(move || {
            fs::remove_file(&real)
                .map_err(|error| fs_error_from_io(error, "unlink", path_string(&error_path)))
        })
        .await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> Result<()> {
        let (from, to) = self.secure_pair(old_path, new_path, "rename").await?;
        let from_error = from.clone();
        let to_error = to.clone();
        run_blocking(move || {
            fs::rename(&from, &to).map_err(|error| {
                fs_error_from_io_with_dest(
                    error,
                    "rename",
                    path_string(&from_error),
                    path_string(&to_error),
                )
            })
        })
        .await
    }

    async fn link(&self, existing_path: &str, new_path: &str) -> Result<()> {
        let (from, to) = self.secure_pair(existing_path, new_path, "link").await?;
        let from_error = from.clone();
        let to_error = to.clone();
        run_blocking(move || {
            fs::hard_link(&from, &to).map_err(|error| {
                fs_error_from_io_with_dest(
                    error,
                    "link",
                    path_string(&from_error),
                    path_string(&to_error),
                )
            })
        })
        .await
    }

    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        self.symlink_with_type(target, path, None).await
    }

    async fn readlink(&self, path: &str) -> Result<String> {
        let real = self.secure(path, false, "readlink").await?;
        let error_path = real.clone();
        run_blocking(move || {
            fs::read_link(&real)
                .map(|target| target.to_string_lossy().into_owned())
                .map_err(|error| fs_error_from_io(error, "readlink", path_string(&error_path)))
        })
        .await
    }

    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        let real = self.secure(path, true, "chmod").await?;
        let error_path = real.clone();
        run_blocking(move || {
            chmod_host(&real, mode)
                .map_err(|error| fs_error_from_io(error, "chmod", path_string(&error_path)))
        })
        .await
    }

    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        let real = self.secure(path, true, "chown").await?;
        let error_path = real.clone();
        run_blocking(move || {
            chown_host(&real, uid, gid, true)
                .map_err(|error| fs_error_from_io(error, "chown", path_string(&error_path)))
        })
        .await
    }

    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        let real = self.secure(path, false, "lchown").await?;
        let error_path = real.clone();
        run_blocking(move || {
            chown_host(&real, uid, gid, false)
                .map_err(|error| fs_error_from_io(error, "lchown", path_string(&error_path)))
        })
        .await
    }

    async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        let real = self.secure(path, true, "truncate").await?;
        let error_path = real.clone();
        run_blocking(move || {
            let file = OpenOptions::new()
                .write(true)
                .open(&real)
                .map_err(|error| fs_error_from_io(error, "truncate", path_string(&error_path)))?;
            file.set_len(length)
                .map_err(|error| fs_error_from_io(error, "truncate", path_string(&error_path)))
        })
        .await
    }

    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        let real = self.secure(path, true, "utime").await?;
        let error_path = real.clone();
        run_blocking(move || {
            utimes_host(&real, atime_ms, mtime_ms, true)
                .map_err(|error| fs_error_from_io(error, "utime", path_string(&error_path)))
        })
        .await
    }

    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        let real = self.secure(path, false, "lutime").await?;
        let error_path = real.clone();
        run_blocking(move || {
            utimes_host(&real, atime_ms, mtime_ms, false)
                .map_err(|error| fs_error_from_io(error, "lutime", path_string(&error_path)))
        })
        .await
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::Loopback;

    #[test]
    fn lexical_root_clamps_parent_components() {
        let cwd = std::env::current_dir().expect("current directory");
        let root = cwd.ancestors().last().expect("host root");
        assert_eq!(lexical_absolute(Path::new("/tmp/../var")), root.join("var"));
        assert_eq!(
            lexical_absolute(&root.join("tmp/../../../var")),
            root.join("var")
        );
    }

    #[test]
    #[cfg(windows)]
    fn lexical_root_preserves_drive_and_unc_prefixes() {
        for root in [Path::new(r"D:\"), Path::new(r"\\server\share\")] {
            assert_eq!(
                lexical_absolute(&root.join("tmp/../../../var")),
                root.join("var")
            );
        }
    }

    #[test]
    fn portable_directory_error_kinds_keep_their_codes() {
        for (kind, expected) in [
            (io::ErrorKind::IsADirectory, ErrorCode::Eisdir),
            (io::ErrorKind::NotADirectory, ErrorCode::Enotdir),
            (io::ErrorKind::DirectoryNotEmpty, ErrorCode::Enotempty),
        ] {
            assert_eq!(error_code(&io::Error::from(kind)), expected);
        }
    }

    #[test]
    fn virtual_mapping_requires_a_path_boundary() {
        let root = Path::new("/tmp/root");
        assert_eq!(
            virtual_path_for_root(root, Path::new("/tmp/root/a")),
            Some("/a".to_owned())
        );
        assert_eq!(
            virtual_path_for_root(root, Path::new("/tmp/rooted/a")),
            None
        );
    }

    #[test]
    #[cfg(unix)]
    fn error_mapping_covers_common_host_codes() {
        assert_eq!(
            error_code(&io::Error::from_raw_os_error(libc::ENOENT)),
            ErrorCode::Enoent
        );
        assert_eq!(
            error_code(&io::Error::from_raw_os_error(libc::ELOOP)),
            ErrorCode::Eloop
        );
        assert_eq!(
            error_code(&io::Error::from_raw_os_error(libc::ENOTSUP)),
            ErrorCode::Enotsup
        );
    }

    #[test]
    #[cfg(windows)]
    fn error_mapping_covers_common_host_codes() {
        for (raw, expected) in [
            (1, ErrorCode::Eisdir),
            (2, ErrorCode::Enoent),
            (5, ErrorCode::Eperm),
            (6, ErrorCode::Ebadf),
            (32, ErrorCode::Ebusy),
            (50, ErrorCode::Enotsup),
            (80, ErrorCode::Eexist),
            (145, ErrorCode::Enotempty),
            (1921, ErrorCode::Eloop),
        ] {
            assert_eq!(
                error_code(&io::Error::from_raw_os_error(raw)),
                expected,
                "Win32 {raw}"
            );
        }
    }

    #[tokio::test]
    async fn handle_access_errors_match_node_filehandle_shape() {
        let root = std::env::temp_dir().join(format!(
            "mount-rs-host-error-shape-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("system clock is before the Unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir(&root).expect("create host error-shape fixture");

        let result: Result<()> = async {
            let fs = Loopback::new(HostFs::new(&root));
            let creator = fs.open("/file", "w", 0o666).await?;
            creator.write(b"x", Some(0)).await?;
            creator.close().await?;

            let read_only = fs.open("/file", "r", 0).await?;
            let write_error = read_only
                .write(b"y", Some(0))
                .await
                .expect_err("read-only handle write must fail");
            assert_eq!(write_error.code, ErrorCode::Ebadf);
            assert_eq!(write_error.syscall.as_deref(), Some("write"));
            assert_eq!(write_error.path, None);
            read_only.close().await?;

            fs.mkdir(
                "/dir",
                MkdirOptions {
                    recursive: false,
                    mode: Some(0o755),
                },
            )
            .await?;
            let directory = fs.open("/dir", "r", 0).await?;
            let mut buffer = [0_u8; 1];
            let read_error = directory
                .read(&mut buffer, Some(0))
                .await
                .expect_err("directory handle read must fail");
            assert_eq!(read_error.code, ErrorCode::Eisdir);
            assert_eq!(read_error.syscall.as_deref(), Some("read"));
            assert_eq!(read_error.path, None);
            let read_error = directory
                .read(&mut buffer, None)
                .await
                .expect_err("directory cursor read");
            assert_eq!(read_error.code, ErrorCode::Eisdir);
            assert_eq!(read_error.syscall.as_deref(), Some("read"));
            assert_eq!(read_error.path, None);
            directory.close().await?;
            Ok(())
        }
        .await;

        std::fs::remove_dir_all(&root).expect("remove host error-shape fixture");
        result.expect("host error-shape regression");
    }
}
