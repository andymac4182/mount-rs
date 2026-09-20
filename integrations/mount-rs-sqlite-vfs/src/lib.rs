//! A mount-free SQLite VFS boundary for storage implementations that can make
//! SQLite's synchronous durability, locking, and (when explicitly enabled)
//! WAL shared-memory contract explicit.
//!
//! This crate deliberately does not adapt [`mount_rs_core::FsDriver`] or any
//! other async filesystem trait.  SQLite invokes a VFS synchronously and its
//! rollback journal protocol requires locks that are visible to other
//! processes.  A backend used here must therefore implement [`Backend`] and
//! [`VfsFile`] directly, including the `sync` and lock methods.
//!
//! The built-in [`HostDirectory`] backend is a small native-directory
//! implementation used for engine and fault tests.  It is also useful as a
//! reference backend, but it is not evidence that an arbitrary remote or
//! async backend is suitable for SQLite hosting.  It supports rollback
//! journaling on Unix and Windows. WAL is opt-in and capability-gated by the
//! backend's shared-memory scope.

#![cfg_attr(
    not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
    allow(dead_code)
)]

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
compile_error!("mount-rs-sqlite-vfs currently supports macOS, Linux, and Windows only");

use std::collections::HashMap;
use std::ffi::{CStr, CString, OsString};
use std::fmt;
use std::fs::{self, File, OpenOptions as StdOpenOptions};
use std::io;
use std::mem::size_of;
use std::ops::Deref;
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Component, Path, PathBuf};
use std::ptr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::ffi::OsStr;
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::FileExt;
#[cfg(windows)]
use std::os::windows::fs::FileExt;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;

use rusqlite::ffi;
use rusqlite::{Connection, OpenFlags};

mod storage;

#[cfg(feature = "tokio-executor")]
pub use storage::TokioExecutor;
pub use storage::{
    BlockingExecutor, InlineExecutor, STORAGE_BRIDGE_VERSION, StorageBackend, StorageOptions,
};

const SQLITE_MAX_PATHNAME: c_int = 4096;

/// Errors returned by a synchronous VFS backend.
#[derive(Debug)]
pub enum VfsError {
    /// An operating-system operation failed.
    Io {
        operation: &'static str,
        source: io::Error,
    },
    /// Another connection or process owns a conflicting SQLite lock.
    Busy,
    /// The requested object does not exist.
    NotFound,
    /// The requested SQLite feature is not implemented by this VFS.
    Unsupported(&'static str),
    /// The caller supplied an invalid path, range, or protocol value.
    InvalidInput(&'static str),
    /// A backend-specific failure with a stable diagnostic.
    Other(String),
}

impl VfsError {
    fn io(operation: &'static str, source: io::Error) -> Self {
        Self::Io { operation, source }
    }
}

impl fmt::Display for VfsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { operation, source } => write!(f, "{operation}: {source}"),
            Self::Busy => f.write_str("SQLite lock is busy"),
            Self::NotFound => f.write_str("SQLite VFS object was not found"),
            Self::Unsupported(feature) => write!(f, "SQLite VFS feature is unsupported: {feature}"),
            Self::InvalidInput(value) => write!(f, "invalid SQLite VFS input: {value}"),
            Self::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for VfsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// The kind of file SQLite is asking the VFS to open.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileKind {
    MainDatabase,
    MainJournal,
    TempDatabase,
    TempJournal,
    SubJournal,
    MasterJournal,
    Other,
}

/// Decoded flags for one `xOpen` call.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OpenOptions {
    pub read_only: bool,
    pub create: bool,
    pub delete_on_close: bool,
    pub kind: FileKind,
    pub raw_flags: c_int,
    pub wal_scope: WalScope,
}

impl OpenOptions {
    fn from_sqlite(flags: c_int) -> Self {
        let kind = if flags & ffi::SQLITE_OPEN_MAIN_DB != 0 {
            FileKind::MainDatabase
        } else if flags & ffi::SQLITE_OPEN_MAIN_JOURNAL != 0 {
            FileKind::MainJournal
        } else if flags & ffi::SQLITE_OPEN_TEMP_DB != 0 {
            FileKind::TempDatabase
        } else if flags & ffi::SQLITE_OPEN_TEMP_JOURNAL != 0 {
            FileKind::TempJournal
        } else if flags & ffi::SQLITE_OPEN_SUBJOURNAL != 0 {
            FileKind::SubJournal
        } else if flags & ffi::SQLITE_OPEN_MASTER_JOURNAL != 0 {
            FileKind::MasterJournal
        } else {
            FileKind::Other
        };

        Self {
            read_only: flags & ffi::SQLITE_OPEN_READWRITE == 0,
            create: flags & ffi::SQLITE_OPEN_CREATE != 0,
            delete_on_close: flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0,
            kind,
            raw_flags: flags,
            wal_scope: WalScope::Disabled,
        }
    }
}

/// The access query requested by SQLite's `xAccess` callback.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessMode {
    Exists,
    ReadWrite,
    Read,
}

/// SQLite's rollback-journal lock levels.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LockLevel {
    None,
    Shared,
    Reserved,
    Pending,
    Exclusive,
}

impl LockLevel {
    fn from_sqlite(value: c_int) -> Option<Self> {
        Some(match value {
            ffi::SQLITE_LOCK_NONE => Self::None,
            ffi::SQLITE_LOCK_SHARED => Self::Shared,
            ffi::SQLITE_LOCK_RESERVED => Self::Reserved,
            ffi::SQLITE_LOCK_PENDING => Self::Pending,
            ffi::SQLITE_LOCK_EXCLUSIVE => Self::Exclusive,
            _ => return None,
        })
    }

    fn as_sqlite(self) -> c_int {
        match self {
            Self::None => ffi::SQLITE_LOCK_NONE,
            Self::Shared => ffi::SQLITE_LOCK_SHARED,
            Self::Reserved => ffi::SQLITE_LOCK_RESERVED,
            Self::Pending => ffi::SQLITE_LOCK_PENDING,
            Self::Exclusive => ffi::SQLITE_LOCK_EXCLUSIVE,
        }
    }
}

/// The strongest shared-memory scope a WAL implementation promises.
///
/// `ProcessLocal` is deliberately weaker than `HostLocal`: it is valid only
/// for connections sharing one backend's process state. It must never be used
/// as evidence that another process or host can safely open the same database.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum WalScope {
    /// WAL is disabled and requests fail explicitly.
    Disabled,
    /// Shared memory and lock state are shared only inside one process.
    ProcessLocal,
    /// File-backed shared memory and OS-visible locks work across processes on
    /// one host and one supported local filesystem.
    HostLocal,
    /// A provider-coordinated distributed shared-memory implementation.
    /// This is reserved for a future provider contract.
    Distributed,
}

impl WalScope {
    fn supports(self, requested: Self) -> bool {
        self >= requested
    }

    fn enabled(self) -> bool {
        self != Self::Disabled
    }
}

/// Backend WAL capability and durability declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WalCapability {
    pub scope: WalScope,
    /// Whether the backend can substantiate durable WAL/database bytes when
    /// the VFS requests full synchronization.
    pub durable: bool,
}

impl WalCapability {
    pub const fn unsupported() -> Self {
        Self {
            scope: WalScope::Disabled,
            durable: false,
        }
    }
}

/// Options that describe the durability envelope advertised by a VFS.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VfsOptions {
    /// Refuse `PRAGMA synchronous=OFF` and `NORMAL`.  This is enabled by
    /// default because the backend contract below promises full sync only.
    pub require_full_sync: bool,
    /// Explicitly request a WAL shared-memory scope. The default keeps the
    /// existing rollback-only behavior.
    pub wal_scope: WalScope,
}

impl Default for VfsOptions {
    fn default() -> Self {
        Self {
            require_full_sync: true,
            wal_scope: WalScope::Disabled,
        }
    }
}

/// Synchronous filesystem boundary required by SQLite.
pub trait Backend: Send + Sync + 'static {
    /// Describe the strongest WAL shared-memory scope this backend can
    /// actually coordinate.  The conservative default keeps existing
    /// rollback-only backends fail-closed.
    fn wal_capability(&self) -> WalCapability {
        WalCapability::unsupported()
    }

    /// Open a file.  `name` is the SQLite filename without its trailing NUL.
    fn open(&self, name: &[u8], options: OpenOptions) -> Result<Box<dyn VfsFile>, VfsError>;

    /// Delete a file and honor the `sync_dir` request where the host VFS
    /// exposes directory-entry durability.  A backend must not claim this
    /// guarantee on a platform where it cannot provide it.
    fn delete(&self, name: &[u8], sync_dir: bool) -> Result<(), VfsError>;

    /// Answer a SQLite access query.
    fn access(&self, name: &[u8], mode: AccessMode) -> Result<bool, VfsError>;

    /// Return the canonical path bytes that fit in SQLite's output buffer.
    fn full_pathname(&self, name: &[u8]) -> Result<Vec<u8>, VfsError>;

    /// Fill a buffer with non-cryptographic randomness suitable for SQLite's
    /// temporary names and salts.
    fn randomness(&self, output: &mut [u8]) -> Result<(), VfsError>;

    /// Generate a temporary filename when SQLite supplies a null `zName`.
    fn temporary_name(&self) -> Result<Vec<u8>, VfsError>;
}

/// A single synchronously operated file returned by [`Backend::open`].
///
/// `sync(true)` must not return success until file data is durable.  `sync`
/// with `data_only == false` must additionally honor the backend's metadata
/// durability guarantee.  `lock` and `check_reserved_lock` must be visible to
/// other processes; an in-process mutex is not a valid implementation.
pub trait VfsFile: Send {
    fn read_at(&mut self, output: &mut [u8], offset: u64) -> Result<usize, VfsError>;
    fn write_at(&mut self, input: &[u8], offset: u64) -> Result<(), VfsError>;
    fn truncate(&mut self, size: u64) -> Result<(), VfsError>;
    fn sync(&mut self, data_only: bool) -> Result<(), VfsError>;
    fn size(&mut self) -> Result<u64, VfsError>;
    fn lock(&mut self, level: LockLevel) -> Result<(), VfsError>;
    fn unlock(&mut self, level: LockLevel) -> Result<(), VfsError>;
    fn check_reserved_lock(&mut self) -> Result<bool, VfsError>;

    /// Map one SQLite WAL shared-memory region.  A successful pointer must
    /// remain valid until the corresponding unmap or file close.
    fn shm_map(
        &mut self,
        _region: usize,
        _region_size: usize,
        _extend: bool,
    ) -> Result<*mut c_void, VfsError> {
        Err(VfsError::Unsupported("WAL shared-memory mapping"))
    }

    /// Apply one legal SQLite WAL shared-memory lock operation.
    fn shm_lock(&mut self, _offset: usize, _number: usize, _flags: c_int) -> Result<(), VfsError> {
        Err(VfsError::Unsupported("WAL shared-memory locking"))
    }

    /// Publish memory barriers between WAL-index accesses.
    fn shm_barrier(&mut self) {}

    /// Release this file's shared-memory mapping and locks.
    fn shm_unmap(&mut self, _delete: bool) -> Result<(), VfsError> {
        Err(VfsError::Unsupported("WAL shared-memory unmapping"))
    }

    fn sector_size(&self) -> c_int {
        4096
    }

    /// Return only capabilities that are true for the backend.  The default
    /// is deliberately conservative.
    fn device_characteristics(&self) -> c_int {
        0
    }
}

/// A registered rollback-journal SQLite VFS.
#[derive(Clone)]
pub struct SqliteVfs {
    registration: Arc<RegisteredVfs>,
}

impl fmt::Debug for SqliteVfs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SqliteVfs")
            .field("name", &self.name())
            .field("options", &self.registration.app.options)
            .finish()
    }
}

impl SqliteVfs {
    /// Register a VFS with the default rollback-journal-only policy under `name`.
    ///
    /// SQLite's global registry retains each successful registration until
    /// [`Self::close`] is called. Closing permits the name to be reused, but a
    /// small backend-free callback tombstone remains for the process lifetime;
    /// use one stable name per logical backend rather than generating one per
    /// request.
    pub fn new(name: &str, backend: Arc<dyn Backend>) -> Result<Self, VfsError> {
        Self::with_options(name, backend, VfsOptions::default())
    }

    /// Register a VFS with an explicit durability policy.
    pub fn with_options(
        name: &str,
        backend: Arc<dyn Backend>,
        options: VfsOptions,
    ) -> Result<Self, VfsError> {
        let name =
            CString::new(name).map_err(|_| VfsError::InvalidInput("VFS name contains NUL"))?;
        if name.as_bytes().is_empty() || name.as_bytes().len() >= 64 {
            return Err(VfsError::InvalidInput("VFS name must be 1..63 bytes"));
        }

        let name_string = name.to_str().expect("validated VFS name").to_owned();
        let capability = backend.wal_capability();
        if !capability.scope.supports(options.wal_scope) {
            return Err(VfsError::Unsupported("requested WAL shared-memory scope"));
        }
        if options.wal_scope.enabled() && options.require_full_sync && !capability.durable {
            return Err(VfsError::Unsupported(
                "WAL full synchronization requires a durable backend",
            ));
        }
        let registry = registered_vfs_registry();
        let mut registry = registry
            .lock()
            .map_err(|_| VfsError::Other("SQLite VFS registry lock poisoned".to_owned()))?;
        if registry.active.contains_key(&name_string) {
            return Err(VfsError::Other(
                "SQLite VFS name is already registered".to_owned(),
            ));
        }

        let mut registration = Box::new(RegisteredVfs {
            app: Box::new(VfsApp {
                lifecycle: LifecycleState::new(backend),
                options,
            }),
            name,
            vfs: unsafe { std::mem::zeroed() },
        });
        registration.vfs = make_vfs(&registration.name, &registration.app);

        let registration: Arc<RegisteredVfs> = registration.into();
        let rc = unsafe {
            ffi::sqlite3_vfs_register(
                &registration.vfs as *const ffi::sqlite3_vfs as *mut ffi::sqlite3_vfs,
                0,
            )
        };
        if rc != ffi::SQLITE_OK {
            return Err(VfsError::Other(format!(
                "sqlite3_vfs_register failed: {rc}"
            )));
        }

        registry
            .active
            .insert(name_string, Arc::clone(&registration));

        Ok(Self { registration })
    }

    /// The registered name passed to `sqlite3_open_v2`.
    pub fn name(&self) -> &str {
        self.registration.name.to_str().expect("validated VFS name")
    }

    /// Quiesce and unregister this VFS, releasing its backend and provider
    /// resources while retaining a backend-free callback tombstone.
    ///
    /// The operation is deliberately non-blocking. It returns
    /// [`VfsError::Busy`] while any VFS callback or SQLite file handle is
    /// active; callers should close those connections and retry. A successful
    /// close prevents new opens, unregisters the VFS from SQLite's global
    /// list, and makes a subsequent call idempotently succeed.
    pub fn close(&self) -> Result<(), VfsError> {
        let name = self.name().to_owned();
        let state = Arc::clone(&self.registration.app.lifecycle);
        {
            let mut lifecycle = state.lock()?;
            match lifecycle.phase {
                LifecyclePhase::Closed => return Ok(()),
                LifecyclePhase::Closing => return Err(VfsError::Busy),
                LifecyclePhase::Open => {}
            }
            if lifecycle.active_operations != 0 || lifecycle.active_files != 0 {
                return Err(VfsError::Busy);
            }
            lifecycle.phase = LifecyclePhase::Closing;
        }

        let registry = registered_vfs_registry();
        let mut registry = registry
            .lock()
            .map_err(|_| VfsError::Other("SQLite VFS registry lock poisoned".to_owned()))?;
        let rc = unsafe {
            ffi::sqlite3_vfs_unregister(
                &self.registration.vfs as *const ffi::sqlite3_vfs as *mut ffi::sqlite3_vfs,
            )
        };
        if rc != ffi::SQLITE_OK {
            let mut lifecycle = state.lock()?;
            lifecycle.phase = LifecyclePhase::Open;
            return Err(VfsError::Other(format!(
                "sqlite3_vfs_unregister failed: {rc}"
            )));
        }

        let retired = registry
            .active
            .remove(&name)
            .expect("active VFS registration disappeared before close");
        assert!(Arc::ptr_eq(&retired, &self.registration));
        registry.retired.push(retired);
        drop(registry);

        let backend = {
            let mut lifecycle = state.lock()?;
            debug_assert_eq!(lifecycle.phase, LifecyclePhase::Closing);
            lifecycle.phase = LifecyclePhase::Closed;
            lifecycle.backend.take()
        };
        drop(backend);
        Ok(())
    }

    /// Open a connection that keeps this VFS registered for its whole life.
    pub fn open<P: AsRef<Path>>(
        &self,
        path: P,
        flags: OpenFlags,
    ) -> rusqlite::Result<VfsConnection> {
        // Hold this registration open while SQLite resolves its name. Without
        // this guard a closed wrapper could resolve a replacement registration
        // that happens to reuse the name and silently switch backing stores.
        let _operation = self.registration.app.begin_operation().map_err(|error| {
            rusqlite::Error::SqliteFailure(
                ffi::Error::new(ffi::SQLITE_CANTOPEN),
                Some(error.to_string()),
            )
        })?;
        let connection = Connection::open_with_flags_and_vfs(path, flags, self.name())?;
        Ok(VfsConnection {
            connection,
            _vfs: self.clone(),
        })
    }
}

/// A rusqlite connection whose VFS registration cannot be dropped early.
pub struct VfsConnection {
    connection: Connection,
    _vfs: SqliteVfs,
}

impl VfsConnection {
    pub fn as_connection(&self) -> &Connection {
        &self.connection
    }

    pub fn with_connection_mut<R>(&mut self, function: impl FnOnce(&mut Connection) -> R) -> R {
        function(&mut self.connection)
    }
}

impl Deref for VfsConnection {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.connection
    }
}

struct VfsApp {
    lifecycle: Arc<LifecycleState>,
    options: VfsOptions,
}

impl VfsApp {
    fn begin_operation(&self) -> Result<OperationGuard, VfsError> {
        self.lifecycle.begin_operation()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LifecyclePhase {
    Open,
    Closing,
    Closed,
}

struct LifecycleInner {
    phase: LifecyclePhase,
    active_operations: usize,
    active_files: usize,
    backend: Option<Arc<dyn Backend>>,
}

struct LifecycleState {
    inner: Mutex<LifecycleInner>,
}

impl LifecycleState {
    fn new(backend: Arc<dyn Backend>) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(LifecycleInner {
                phase: LifecyclePhase::Open,
                active_operations: 0,
                active_files: 0,
                backend: Some(backend),
            }),
        })
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, LifecycleInner>, VfsError> {
        self.inner
            .lock()
            .map_err(|_| VfsError::Other("SQLite VFS lifecycle lock poisoned".to_owned()))
    }

    fn begin_operation(self: &Arc<Self>) -> Result<OperationGuard, VfsError> {
        let mut lifecycle = self.lock()?;
        match lifecycle.phase {
            LifecyclePhase::Open => {}
            LifecyclePhase::Closing => return Err(VfsError::Busy),
            LifecyclePhase::Closed => {
                return Err(VfsError::Other("SQLite VFS is closed".to_owned()));
            }
        }
        let backend = lifecycle
            .backend
            .clone()
            .ok_or_else(|| VfsError::Other("SQLite VFS backend is detached".to_owned()))?;
        lifecycle.active_operations += 1;
        Ok(OperationGuard {
            state: Arc::clone(self),
            backend,
        })
    }

    fn acquire_file(self: &Arc<Self>) -> Result<FileLease, VfsError> {
        let mut lifecycle = self.lock()?;
        if lifecycle.phase != LifecyclePhase::Open {
            return Err(match lifecycle.phase {
                LifecyclePhase::Closing => VfsError::Busy,
                LifecyclePhase::Closed => VfsError::Other("SQLite VFS is closed".to_owned()),
                LifecyclePhase::Open => unreachable!(),
            });
        }
        lifecycle.active_files += 1;
        Ok(FileLease {
            state: Arc::clone(self),
        })
    }

    fn release_operation(&self) {
        let Ok(mut lifecycle) = self.inner.lock() else {
            return;
        };
        debug_assert!(lifecycle.active_operations > 0);
        lifecycle.active_operations = lifecycle.active_operations.saturating_sub(1);
    }

    fn release_file(&self) {
        let Ok(mut lifecycle) = self.inner.lock() else {
            return;
        };
        debug_assert!(lifecycle.active_files > 0);
        lifecycle.active_files = lifecycle.active_files.saturating_sub(1);
    }
}

struct OperationGuard {
    state: Arc<LifecycleState>,
    backend: Arc<dyn Backend>,
}

impl OperationGuard {
    fn backend(&self) -> &dyn Backend {
        self.backend.as_ref()
    }

    fn acquire_file(&self) -> Result<FileLease, VfsError> {
        self.state.acquire_file()
    }
}

impl Drop for OperationGuard {
    fn drop(&mut self) {
        self.state.release_operation();
    }
}

struct FileLease {
    state: Arc<LifecycleState>,
}

impl Drop for FileLease {
    fn drop(&mut self) {
        self.state.release_file();
    }
}

struct RegisteredVfs {
    app: Box<VfsApp>,
    name: CString,
    vfs: ffi::sqlite3_vfs,
}

// SQLite stores and invokes the callback table after registration.  The Arc
// allocation is stable, and the callback table only points at the immutable
// VfsApp inside that allocation.  Backend requires Send + Sync.
unsafe impl Send for RegisteredVfs {}
unsafe impl Sync for RegisteredVfs {}

#[derive(Default)]
struct VfsRegistry {
    active: HashMap<String, Arc<RegisteredVfs>>,
    // SQLite may still call a raw callback pointer that it obtained just
    // before unregistering the VFS. Keep the callback allocation alive, but
    // release its backend through SqliteVfs::close.
    retired: Vec<Arc<RegisteredVfs>>,
}

fn registered_vfs_registry() -> &'static Mutex<VfsRegistry> {
    static REGISTRY: OnceLock<Mutex<VfsRegistry>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(VfsRegistry::default()))
}

#[repr(C)]
struct VfsFileHandle {
    base: ffi::sqlite3_file,
    file: Option<Box<dyn VfsFile>>,
    level: LockLevel,
    wal_scope: WalScope,
    require_full_sync: bool,
    file_lease: Option<FileLease>,
}

impl VfsFileHandle {
    fn begin_operation(&self) -> Result<OperationGuard, VfsError> {
        self.file_lease
            .as_ref()
            .ok_or_else(|| VfsError::Other("SQLite VFS file is closed".to_owned()))?
            .state
            .begin_operation()
    }
}

fn make_vfs(name: &CStr, app: &VfsApp) -> ffi::sqlite3_vfs {
    ffi::sqlite3_vfs {
        iVersion: 3,
        szOsFile: size_of::<VfsFileHandle>() as c_int,
        mxPathname: SQLITE_MAX_PATHNAME,
        pNext: ptr::null_mut(),
        zName: name.as_ptr(),
        pAppData: app as *const VfsApp as *mut c_void,
        xOpen: Some(x_open),
        xDelete: Some(x_delete),
        xAccess: Some(x_access),
        xFullPathname: Some(x_full_pathname),
        xDlOpen: None,
        xDlError: None,
        xDlSym: None,
        xDlClose: None,
        xRandomness: Some(x_randomness),
        xSleep: Some(x_sleep),
        xCurrentTime: Some(x_current_time),
        xGetLastError: Some(x_get_last_error),
        xCurrentTimeInt64: Some(x_current_time_int64),
        xSetSystemCall: None,
        xGetSystemCall: None,
        xNextSystemCall: None,
    }
}

fn catch_code<F>(function: F) -> c_int
where
    F: FnOnce() -> c_int,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(function)) {
        Ok(code) => code,
        Err(_) => ffi::SQLITE_INTERNAL,
    }
}

unsafe fn app_from_vfs<'a>(vfs: *mut ffi::sqlite3_vfs) -> Option<&'a VfsApp> {
    if vfs.is_null() || unsafe { (*vfs).pAppData }.is_null() {
        None
    } else {
        let app_data = unsafe { (*vfs).pAppData };
        Some(unsafe { &*(app_data as *const VfsApp) })
    }
}

unsafe fn file_from_base<'a>(file: *mut ffi::sqlite3_file) -> Option<&'a mut VfsFileHandle> {
    if file.is_null() {
        None
    } else {
        Some(unsafe { &mut *(file as *mut VfsFileHandle) })
    }
}

fn map_error(error: VfsError, fallback: c_int) -> c_int {
    match error {
        VfsError::Busy => ffi::SQLITE_BUSY,
        VfsError::NotFound => ffi::SQLITE_NOTFOUND,
        VfsError::Unsupported(_) => ffi::SQLITE_NOTFOUND,
        VfsError::InvalidInput(_) => ffi::SQLITE_MISUSE,
        VfsError::Other(_) => fallback,
        VfsError::Io { source, .. } => {
            if source.kind() == io::ErrorKind::WouldBlock {
                ffi::SQLITE_BUSY
            } else {
                fallback
            }
        }
    }
}

unsafe extern "C" fn x_open(
    vfs: *mut ffi::sqlite3_vfs,
    z_name: *const c_char,
    p_file: *mut ffi::sqlite3_file,
    flags: c_int,
    p_out_flags: *mut c_int,
) -> c_int {
    catch_code(|| unsafe {
        if p_file.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        ptr::write_bytes(p_file.cast::<u8>(), 0, size_of::<VfsFileHandle>());
        let Some(app) = app_from_vfs(vfs) else {
            return ffi::SQLITE_MISUSE;
        };
        let operation = match app.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_CANTOPEN),
        };
        let mut options = OpenOptions::from_sqlite(flags);
        if options.raw_flags & ffi::SQLITE_OPEN_WAL != 0 && !app.options.wal_scope.enabled() {
            return ffi::SQLITE_CANTOPEN;
        }
        options.wal_scope = app.options.wal_scope;

        let temporary_name;
        let name = if z_name.is_null() {
            match operation.backend().temporary_name() {
                Ok(value) => {
                    temporary_name = value;
                    temporary_name.as_slice()
                }
                Err(error) => return map_error(error, ffi::SQLITE_CANTOPEN),
            }
        } else {
            CStr::from_ptr(z_name).to_bytes()
        };

        let file = match operation.backend().open(name, options) {
            Ok(file) => file,
            Err(error) => return map_error(error, ffi::SQLITE_CANTOPEN),
        };
        let file_lease = match operation.acquire_file() {
            Ok(lease) => lease,
            Err(error) => return map_error(error, ffi::SQLITE_CANTOPEN),
        };
        let vfs_file = VfsFileHandle {
            base: ffi::sqlite3_file {
                pMethods: &IO_METHODS,
            },
            file: Some(file),
            level: LockLevel::None,
            wal_scope: app.options.wal_scope,
            require_full_sync: app.options.require_full_sync,
            file_lease: Some(file_lease),
        };
        ptr::write(p_file as *mut VfsFileHandle, vfs_file);
        if !p_out_flags.is_null() {
            *p_out_flags = flags;
        }
        ffi::SQLITE_OK
    })
}

unsafe extern "C" fn x_delete(
    vfs: *mut ffi::sqlite3_vfs,
    z_name: *const c_char,
    sync_dir: c_int,
) -> c_int {
    catch_code(|| unsafe {
        let Some(app) = app_from_vfs(vfs) else {
            return ffi::SQLITE_MISUSE;
        };
        if z_name.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        let operation = match app.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_DELETE),
        };
        match operation
            .backend()
            .delete(CStr::from_ptr(z_name).to_bytes(), sync_dir != 0)
        {
            Ok(()) => ffi::SQLITE_OK,
            Err(VfsError::NotFound) => ffi::SQLITE_OK,
            Err(error) => map_error(error, ffi::SQLITE_IOERR_DELETE),
        }
    })
}

unsafe extern "C" fn x_access(
    vfs: *mut ffi::sqlite3_vfs,
    z_name: *const c_char,
    flags: c_int,
    p_res_out: *mut c_int,
) -> c_int {
    catch_code(|| unsafe {
        let Some(app) = app_from_vfs(vfs) else {
            return ffi::SQLITE_MISUSE;
        };
        if z_name.is_null() || p_res_out.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        let mode = match flags {
            ffi::SQLITE_ACCESS_EXISTS => AccessMode::Exists,
            ffi::SQLITE_ACCESS_READWRITE => AccessMode::ReadWrite,
            ffi::SQLITE_ACCESS_READ => AccessMode::Read,
            _ => return ffi::SQLITE_MISUSE,
        };
        let operation = match app.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_ACCESS),
        };
        match operation
            .backend()
            .access(CStr::from_ptr(z_name).to_bytes(), mode)
        {
            Ok(value) => {
                *p_res_out = i32::from(value);
                ffi::SQLITE_OK
            }
            Err(error) => map_error(error, ffi::SQLITE_IOERR_ACCESS),
        }
    })
}

unsafe extern "C" fn x_full_pathname(
    vfs: *mut ffi::sqlite3_vfs,
    z_name: *const c_char,
    n_out: c_int,
    z_out: *mut c_char,
) -> c_int {
    catch_code(|| unsafe {
        let Some(app) = app_from_vfs(vfs) else {
            return ffi::SQLITE_MISUSE;
        };
        if z_name.is_null() || z_out.is_null() || n_out <= 0 {
            return ffi::SQLITE_MISUSE;
        }
        let operation = match app.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_CANTOPEN_FULLPATH),
        };
        let path = match operation
            .backend()
            .full_pathname(CStr::from_ptr(z_name).to_bytes())
        {
            Ok(path) => path,
            Err(error) => return map_error(error, ffi::SQLITE_CANTOPEN_FULLPATH),
        };
        let capacity = (n_out as usize).saturating_sub(1);
        let length = path.len().min(capacity);
        ptr::copy_nonoverlapping(path.as_ptr().cast::<c_char>(), z_out, length);
        *z_out.add(length) = 0;
        if length < path.len() {
            ffi::SQLITE_CANTOPEN_FULLPATH
        } else {
            ffi::SQLITE_OK
        }
    })
}

unsafe extern "C" fn x_randomness(
    vfs: *mut ffi::sqlite3_vfs,
    n_byte: c_int,
    z_out: *mut c_char,
) -> c_int {
    catch_code(|| unsafe {
        let Some(app) = app_from_vfs(vfs) else {
            return -1;
        };
        if n_byte < 0 || (n_byte > 0 && z_out.is_null()) {
            return -1;
        }
        let output = if n_byte == 0 {
            &mut []
        } else {
            std::slice::from_raw_parts_mut(z_out.cast::<u8>(), n_byte as usize)
        };
        let operation = match app.begin_operation() {
            Ok(operation) => operation,
            Err(_) => return -1,
        };
        match operation.backend().randomness(output) {
            Ok(()) => n_byte,
            Err(_) => -1,
        }
    })
}

unsafe extern "C" fn x_sleep(vfs: *mut ffi::sqlite3_vfs, microseconds: c_int) -> c_int {
    catch_code(|| unsafe {
        let Some(app) = app_from_vfs(vfs) else {
            return 0;
        };
        let Ok(_operation) = app.begin_operation() else {
            return 0;
        };
        if microseconds > 0 {
            std::thread::sleep(Duration::from_micros(microseconds as u64));
        }
        microseconds.max(0)
    })
}

unsafe extern "C" fn x_current_time(vfs: *mut ffi::sqlite3_vfs, output: *mut f64) -> c_int {
    catch_code(|| unsafe {
        let Some(app) = app_from_vfs(vfs) else {
            return ffi::SQLITE_MISUSE;
        };
        let Ok(_operation) = app.begin_operation() else {
            return ffi::SQLITE_MISUSE;
        };
        if output.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        *output = current_julian_day();
        ffi::SQLITE_OK
    })
}

unsafe extern "C" fn x_current_time_int64(
    vfs: *mut ffi::sqlite3_vfs,
    output: *mut ffi::sqlite3_int64,
) -> c_int {
    catch_code(|| unsafe {
        let Some(app) = app_from_vfs(vfs) else {
            return ffi::SQLITE_MISUSE;
        };
        let Ok(_operation) = app.begin_operation() else {
            return ffi::SQLITE_MISUSE;
        };
        if output.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        *output = (current_julian_day() * 86_400_000.0) as ffi::sqlite3_int64;
        ffi::SQLITE_OK
    })
}

unsafe extern "C" fn x_get_last_error(
    vfs: *mut ffi::sqlite3_vfs,
    n_byte: c_int,
    z_err_msg: *mut c_char,
) -> c_int {
    catch_code(|| unsafe {
        let Some(app) = app_from_vfs(vfs) else {
            return -1;
        };
        let Ok(_operation) = app.begin_operation() else {
            return -1;
        };
        if n_byte > 0 && !z_err_msg.is_null() {
            *z_err_msg = 0;
        }
        0
    })
}

#[cfg(test)]
const JULIAN_UNIX_EPOCH_MILLIS: i64 = 210_866_760_000_000;

fn current_julian_day() -> f64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => 2_440_587.5 + duration.as_secs_f64() / 86_400.0,
        Err(_) => 2_440_587.5,
    }
}

unsafe extern "C" fn x_close(file: *mut ffi::sqlite3_file) -> c_int {
    catch_code(|| unsafe {
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let mut unmap_error = None;
        if file.wal_scope.enabled()
            && let Ok(_operation) = file.begin_operation()
            && let Some(file_impl) = file.file.as_mut()
            && let Err(error) = file_impl.shm_unmap(false)
        {
            unmap_error = Some(error);
        }
        let file_impl = file.file.take();
        let file_lease = file.file_lease.take();
        drop(file_impl);
        drop(file_lease);
        file.base.pMethods = ptr::null();
        unmap_error.map_or(ffi::SQLITE_OK, |error| {
            map_error(error, ffi::SQLITE_IOERR_SHMOPEN)
        })
    })
}

unsafe extern "C" fn x_read(
    file: *mut ffi::sqlite3_file,
    output: *mut c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    catch_code(|| unsafe {
        if amount < 0 || offset < 0 || (amount > 0 && output.is_null()) {
            return ffi::SQLITE_MISUSE;
        }
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_READ),
        };
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        let buffer = if amount == 0 {
            &mut []
        } else {
            std::slice::from_raw_parts_mut(output.cast::<u8>(), amount as usize)
        };
        match file_impl.read_at(buffer, offset as u64) {
            Ok(read) if read == buffer.len() => ffi::SQLITE_OK,
            Ok(read) if read < buffer.len() => {
                buffer[read..].fill(0);
                ffi::SQLITE_IOERR_SHORT_READ
            }
            Ok(_) => ffi::SQLITE_IOERR_READ,
            Err(error) => map_error(error, ffi::SQLITE_IOERR_READ),
        }
    })
}

unsafe extern "C" fn x_write(
    file: *mut ffi::sqlite3_file,
    input: *const c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    catch_code(|| unsafe {
        if amount < 0 || offset < 0 || (amount > 0 && input.is_null()) {
            return ffi::SQLITE_MISUSE;
        }
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_WRITE),
        };
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        let buffer = if amount == 0 {
            &[]
        } else {
            std::slice::from_raw_parts(input.cast::<u8>(), amount as usize)
        };
        match file_impl.write_at(buffer, offset as u64) {
            Ok(()) => ffi::SQLITE_OK,
            Err(error) => map_error(error, ffi::SQLITE_IOERR_WRITE),
        }
    })
}

unsafe extern "C" fn x_truncate(file: *mut ffi::sqlite3_file, size: ffi::sqlite3_int64) -> c_int {
    catch_code(|| unsafe {
        if size < 0 {
            return ffi::SQLITE_MISUSE;
        }
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_TRUNCATE),
        };
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        match file_impl.truncate(size as u64) {
            Ok(()) => ffi::SQLITE_OK,
            Err(error) => map_error(error, ffi::SQLITE_IOERR_TRUNCATE),
        }
    })
}

unsafe extern "C" fn x_sync(file: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
    catch_code(|| unsafe {
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_FSYNC),
        };
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        match file_impl.sync(flags & ffi::SQLITE_SYNC_DATAONLY != 0) {
            Ok(()) => ffi::SQLITE_OK,
            Err(error) => map_error(error, ffi::SQLITE_IOERR_FSYNC),
        }
    })
}

unsafe extern "C" fn x_file_size(
    file: *mut ffi::sqlite3_file,
    output: *mut ffi::sqlite3_int64,
) -> c_int {
    catch_code(|| unsafe {
        if output.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_FSTAT),
        };
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        match file_impl.size() {
            Ok(size) if size <= ffi::sqlite3_int64::MAX as u64 => {
                *output = size as ffi::sqlite3_int64;
                ffi::SQLITE_OK
            }
            Ok(_) => ffi::SQLITE_FULL,
            Err(error) => map_error(error, ffi::SQLITE_IOERR_FSTAT),
        }
    })
}

unsafe extern "C" fn x_lock(file: *mut ffi::sqlite3_file, requested: c_int) -> c_int {
    catch_code(|| unsafe {
        let Some(level) = LockLevel::from_sqlite(requested) else {
            return ffi::SQLITE_MISUSE;
        };
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_LOCK),
        };
        if level <= file.level {
            return ffi::SQLITE_OK;
        }
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        match file_impl.lock(level) {
            Ok(()) => {
                file.level = level;
                ffi::SQLITE_OK
            }
            Err(error) => map_error(error, ffi::SQLITE_IOERR_LOCK),
        }
    })
}

unsafe extern "C" fn x_unlock(file: *mut ffi::sqlite3_file, requested: c_int) -> c_int {
    catch_code(|| unsafe {
        let Some(level) = LockLevel::from_sqlite(requested) else {
            return ffi::SQLITE_MISUSE;
        };
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_UNLOCK),
        };
        if level >= file.level {
            return ffi::SQLITE_OK;
        }
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        match file_impl.unlock(level) {
            Ok(()) => {
                file.level = level;
                ffi::SQLITE_OK
            }
            Err(error) => map_error(error, ffi::SQLITE_IOERR_UNLOCK),
        }
    })
}

unsafe extern "C" fn x_check_reserved_lock(
    file: *mut ffi::sqlite3_file,
    output: *mut c_int,
) -> c_int {
    catch_code(|| unsafe {
        if output.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_CHECKRESERVEDLOCK),
        };
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        match file_impl.check_reserved_lock() {
            Ok(value) => {
                *output = i32::from(value);
                ffi::SQLITE_OK
            }
            Err(error) => map_error(error, ffi::SQLITE_IOERR_CHECKRESERVEDLOCK),
        }
    })
}

unsafe extern "C" fn x_file_control(
    file: *mut ffi::sqlite3_file,
    operation: c_int,
    argument: *mut c_void,
) -> c_int {
    catch_code(|| unsafe {
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR),
        };
        match operation {
            ffi::SQLITE_FCNTL_LOCKSTATE if !argument.is_null() => {
                *(argument.cast::<c_int>()) = file.level.as_sqlite();
                ffi::SQLITE_OK
            }
            ffi::SQLITE_FCNTL_PRAGMA if !argument.is_null() => {
                let arguments = argument.cast::<*mut c_char>();
                let pragma_pointer = *arguments.add(1);
                let pragma = if pragma_pointer.is_null() {
                    &[][..]
                } else {
                    CStr::from_ptr(pragma_pointer).to_bytes()
                };
                let value_pointer = *arguments.add(2);
                let value = if value_pointer.is_null() {
                    &[][..]
                } else {
                    CStr::from_ptr(value_pointer).to_bytes()
                };
                if file.wal_scope == WalScope::Disabled
                    && pragma.eq_ignore_ascii_case(b"journal_mode")
                    && value.eq_ignore_ascii_case(b"wal")
                {
                    return ffi::SQLITE_ERROR;
                }
                if pragma.eq_ignore_ascii_case(b"journal_mode")
                    && (value.eq_ignore_ascii_case(b"memory") || value.eq_ignore_ascii_case(b"off"))
                {
                    // MEMORY and OFF remove the rollback-journal durability
                    // boundary.  The VFS only claims rollback modes whose
                    // journal bytes pass through xSync, so do not let SQLite
                    // silently select an unsafe mode for a reliability-
                    // qualified database.
                    return ffi::SQLITE_ERROR;
                }
                if file.require_full_sync
                    && pragma.eq_ignore_ascii_case(b"synchronous")
                    && (value.eq_ignore_ascii_case(b"off")
                        || value.eq_ignore_ascii_case(b"normal")
                        || value == b"0"
                        || value == b"1")
                {
                    return ffi::SQLITE_ERROR;
                }
                ffi::SQLITE_NOTFOUND
            }
            ffi::SQLITE_FCNTL_PRAGMA => ffi::SQLITE_NOTFOUND,
            _ => ffi::SQLITE_NOTFOUND,
        }
    })
}

unsafe extern "C" fn x_sector_size(file: *mut ffi::sqlite3_file) -> c_int {
    catch_code(|| unsafe {
        let Some(file) = file_from_base(file) else {
            return 4096;
        };
        let Ok(_operation) = file.begin_operation() else {
            return 4096;
        };
        file.file.as_ref().map_or(4096, |file| file.sector_size())
    })
}

unsafe extern "C" fn x_device_characteristics(file: *mut ffi::sqlite3_file) -> c_int {
    catch_code(|| unsafe {
        let Some(file) = file_from_base(file) else {
            return 0;
        };
        let Ok(_operation) = file.begin_operation() else {
            return 0;
        };
        file.file
            .as_ref()
            .map_or(0, |file| file.device_characteristics())
    })
}

unsafe extern "C" fn x_shm_map(
    file: *mut ffi::sqlite3_file,
    page: c_int,
    page_size: c_int,
    extend: c_int,
    output: *mut *mut c_void,
) -> c_int {
    catch_code(|| unsafe {
        if page < 0 || page_size <= 0 || output.is_null() {
            return ffi::SQLITE_MISUSE;
        }
        *output = ptr::null_mut();
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_SHMMAP),
        };
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        match file_impl.shm_map(page as usize, page_size as usize, extend != 0) {
            Ok(mapped) => {
                *output = mapped;
                ffi::SQLITE_OK
            }
            Err(error) => map_error(error, ffi::SQLITE_IOERR_SHMMAP),
        }
    })
}

unsafe extern "C" fn x_shm_lock(
    file: *mut ffi::sqlite3_file,
    offset: c_int,
    number: c_int,
    flags: c_int,
) -> c_int {
    catch_code(|| unsafe {
        let legal = ffi::SQLITE_SHM_LOCK
            | ffi::SQLITE_SHM_UNLOCK
            | ffi::SQLITE_SHM_SHARED
            | ffi::SQLITE_SHM_EXCLUSIVE;
        let operation = flags & (ffi::SQLITE_SHM_LOCK | ffi::SQLITE_SHM_UNLOCK);
        let mode = flags & (ffi::SQLITE_SHM_SHARED | ffi::SQLITE_SHM_EXCLUSIVE);
        if offset < 0
            || number <= 0
            || offset > ffi::SQLITE_SHM_NLOCK - number
            || flags & !legal != 0
            || !matches!(operation, ffi::SQLITE_SHM_LOCK | ffi::SQLITE_SHM_UNLOCK)
            || !matches!(mode, ffi::SQLITE_SHM_SHARED | ffi::SQLITE_SHM_EXCLUSIVE)
        {
            return ffi::SQLITE_MISUSE;
        }
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_SHMLOCK),
        };
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        match file_impl.shm_lock(offset as usize, number as usize, flags) {
            Ok(()) => ffi::SQLITE_OK,
            Err(error) => map_error(error, ffi::SQLITE_IOERR_SHMLOCK),
        }
    })
}

unsafe extern "C" fn x_shm_barrier(file: *mut ffi::sqlite3_file) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        let Some(file) = file_from_base(file) else {
            return;
        };
        let Ok(_operation) = file.begin_operation() else {
            return;
        };
        if let Some(file_impl) = file.file.as_mut() {
            file_impl.shm_barrier();
        }
    }));
}

unsafe extern "C" fn x_shm_unmap(file: *mut ffi::sqlite3_file, delete: c_int) -> c_int {
    catch_code(|| unsafe {
        let Some(file) = file_from_base(file) else {
            return ffi::SQLITE_MISUSE;
        };
        let _operation = match file.begin_operation() {
            Ok(operation) => operation,
            Err(error) => return map_error(error, ffi::SQLITE_IOERR_SHMOPEN),
        };
        let Some(file_impl) = file.file.as_mut() else {
            return ffi::SQLITE_MISUSE;
        };
        match file_impl.shm_unmap(delete != 0) {
            Ok(()) => ffi::SQLITE_OK,
            Err(error) => map_error(error, ffi::SQLITE_IOERR_SHMOPEN),
        }
    })
}

unsafe extern "C" fn x_fetch(
    _file: *mut ffi::sqlite3_file,
    _offset: ffi::sqlite3_int64,
    _amount: c_int,
    output: *mut *mut c_void,
) -> c_int {
    if !output.is_null() {
        unsafe { *output = ptr::null_mut() };
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_unfetch(
    _file: *mut ffi::sqlite3_file,
    _offset: ffi::sqlite3_int64,
    _pointer: *mut c_void,
) -> c_int {
    ffi::SQLITE_OK
}

static IO_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 3,
    xClose: Some(x_close),
    xRead: Some(x_read),
    xWrite: Some(x_write),
    xTruncate: Some(x_truncate),
    xSync: Some(x_sync),
    xFileSize: Some(x_file_size),
    xLock: Some(x_lock),
    xUnlock: Some(x_unlock),
    xCheckReservedLock: Some(x_check_reserved_lock),
    xFileControl: Some(x_file_control),
    xSectorSize: Some(x_sector_size),
    xDeviceCharacteristics: Some(x_device_characteristics),
    xShmMap: Some(x_shm_map),
    xShmLock: Some(x_shm_lock),
    xShmBarrier: Some(x_shm_barrier),
    xShmUnmap: Some(x_shm_unmap),
    xFetch: Some(x_fetch),
    xUnfetch: Some(x_unfetch),
};

#[cfg(unix)]
fn path_from_sqlite(name: &[u8]) -> Result<PathBuf, VfsError> {
    Ok(PathBuf::from(OsStr::from_bytes(name)))
}

#[cfg(windows)]
fn path_from_sqlite(name: &[u8]) -> Result<PathBuf, VfsError> {
    let name = std::str::from_utf8(name)
        .map_err(|_| VfsError::InvalidInput("Windows SQLite paths must be UTF-8"))?;
    Ok(PathBuf::from(name))
}

#[cfg(unix)]
fn path_to_sqlite(path: &Path) -> Result<Vec<u8>, VfsError> {
    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(windows)]
fn path_to_sqlite(path: &Path) -> Result<Vec<u8>, VfsError> {
    path.to_str()
        .map(|path| path.as_bytes().to_vec())
        .ok_or(VfsError::InvalidInput(
            "Windows SQLite paths must be valid Unicode",
        ))
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// A small native-directory backend used by the engine tests and as a
/// synchronous reference implementation.  On Windows it follows SQLite's
/// native Win32 VFS policy: file handles are flushed by `xSync`, while the
/// `xDelete` directory-sync request is ignored because directory-entry
/// durability is not claimed here.
#[derive(Clone, Debug)]
pub struct HostDirectory {
    root: PathBuf,
}

impl HostDirectory {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, VfsError> {
        fs::create_dir_all(root.as_ref())
            .map_err(|error| VfsError::io("create VFS root", error))?;
        let root = fs::canonicalize(root.as_ref())
            .map_err(|error| VfsError::io("canonicalize VFS root", error))?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, name: &[u8]) -> Result<PathBuf, VfsError> {
        let path = path_from_sqlite(name)?;
        #[cfg(windows)]
        if !path.is_absolute()
            && path
                .components()
                .any(|component| matches!(component, Component::Prefix(_)))
        {
            return Err(VfsError::InvalidInput(
                "drive-relative Windows paths are unsupported",
            ));
        }
        let candidate = if path.is_absolute() {
            path
        } else {
            self.root.join(&path)
        };
        let normalized = normalize_path(&candidate);
        if !normalized.starts_with(&self.root) {
            return Err(VfsError::InvalidInput("path escapes VFS root"));
        }
        Ok(normalized)
    }
}

impl Backend for HostDirectory {
    fn wal_capability(&self) -> WalCapability {
        WalCapability {
            scope: WalScope::HostLocal,
            durable: true,
        }
    }

    fn open(&self, name: &[u8], options: OpenOptions) -> Result<Box<dyn VfsFile>, VfsError> {
        let path = self.resolve(name)?;
        if let Some(parent) = path.parent()
            && options.create
        {
            fs::create_dir_all(parent)
                .map_err(|error| VfsError::io("create SQLite parent", error))?;
        }

        let mut open = StdOpenOptions::new();
        open.read(true)
            .write(!options.read_only)
            .create(options.create);
        let file = open.open(&path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                VfsError::NotFound
            } else {
                VfsError::io("open SQLite file", error)
            }
        })?;

        let locks = (options.kind == FileKind::MainDatabase)
            .then(|| LockFiles::open(&path))
            .transpose()?;
        Ok(Box::new(HostFile {
            file,
            locks,
            delete_on_close: options.delete_on_close,
            path,
            writable: !options.read_only,
            wal_scope: options.wal_scope,
            shm: None,
        }))
    }

    fn delete(&self, name: &[u8], sync_dir: bool) -> Result<(), VfsError> {
        let path = self.resolve(name)?;
        #[cfg(not(windows))]
        let remove_result = fs::remove_file(&path);
        #[cfg(windows)]
        let remove_result = remove_file_with_retries(&path);
        match remove_result {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(VfsError::NotFound);
            }
            Err(error) => return Err(VfsError::io("delete SQLite file", error)),
        }
        #[cfg(unix)]
        if sync_dir {
            let parent = path
                .parent()
                .ok_or(VfsError::InvalidInput("file has no parent"))?;
            sync_directory(parent).map_err(|error| VfsError::io("sync SQLite directory", error))?;
        }
        #[cfg(windows)]
        {
            // SQLite's native Win32 VFS treats xDelete's syncDir argument as
            // unused.  Windows file-handle sync remains enforced by xSync,
            // but this backend does not claim directory-entry durability or
            // attempt a directory FlushFileBuffers call that may fail.
            let _ = sync_dir;
        }
        Ok(())
    }

    fn access(&self, name: &[u8], mode: AccessMode) -> Result<bool, VfsError> {
        let path = self.resolve(name)?;
        let metadata = match fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(VfsError::io("stat SQLite file", error)),
        };
        Ok(match mode {
            AccessMode::Exists => true,
            AccessMode::Read => true,
            AccessMode::ReadWrite => !metadata.permissions().readonly(),
        })
    }

    fn full_pathname(&self, name: &[u8]) -> Result<Vec<u8>, VfsError> {
        path_to_sqlite(&self.resolve(name)?)
    }

    fn randomness(&self, output: &mut [u8]) -> Result<(), VfsError> {
        fill_randomness(output)
    }

    fn temporary_name(&self) -> Result<Vec<u8>, VfsError> {
        static NEXT_TEMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let counter = NEXT_TEMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        path_to_sqlite(&self.root.join(format!(
            ".mount-rs-sqlite-tmp-{}-{counter}",
            std::process::id()
        )))
    }
}

struct HostFile {
    file: File,
    locks: Option<LockFiles>,
    delete_on_close: bool,
    path: PathBuf,
    writable: bool,
    wal_scope: WalScope,
    shm: Option<HostShm>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShmLockMode {
    None,
    Shared,
    Exclusive,
}

struct HostShm {
    path: PathBuf,
    file: File,
    dms: File,
    dms_held: bool,
    writable: bool,
    region_size: Option<usize>,
    mapped: Vec<Option<HostMappedRegion>>,
    locks: Vec<File>,
    held: [ShmLockMode; ffi::SQLITE_SHM_NLOCK as usize],
}

struct HostMappedRegion {
    output: *mut c_void,
    #[cfg(unix)]
    len: usize,
    #[cfg(windows)]
    view: *mut c_void,
    #[cfg(windows)]
    mapping: *mut c_void,
}

// The mapping is owned by HostShm and is only exposed while that HostFile is
// alive. Every callback that can reach it holds &mut self, and Drop unmaps it
// before the backing file is released.
unsafe impl Send for HostMappedRegion {}

impl HostShm {
    fn open(path: &Path, writable: bool) -> Result<Self, VfsError> {
        let shm_path = append_path_suffix(path, "-shm");
        let mut options = StdOpenOptions::new();
        options.read(true).write(writable).create(writable);
        let file = options.open(&shm_path).map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                VfsError::NotFound
            } else {
                VfsError::io("open SQLite WAL shared memory", error)
            }
        })?;
        let dms_path = append_path_suffix(&shm_path, ".mount-rs-wal-dms-lock");
        let dms = StdOpenOptions::new()
            .read(true)
            .write(writable)
            .create(writable)
            .truncate(false)
            .open(&dms_path)
            .map_err(|error| VfsError::io("open SQLite WAL DMS lock", error))?;
        match lock_file(&dms, true) {
            Ok(()) => {
                if !writable {
                    let _ = unlock_file(&dms);
                    return Err(VfsError::Unsupported(
                        "read-only SQLite WAL shared memory cannot initialize",
                    ));
                }
                if let Err(error) = file.set_len(3) {
                    let _ = unlock_file(&dms);
                    return Err(VfsError::io("initialize SQLite WAL shared memory", error));
                }
                unlock_file(&dms)?;
            }
            Err(VfsError::Busy) => {}
            Err(error) => return Err(error),
        }
        lock_file(&dms, false)?;
        let mut locks = Vec::with_capacity(ffi::SQLITE_SHM_NLOCK as usize);
        for index in 0..ffi::SQLITE_SHM_NLOCK {
            let mut lock_options = StdOpenOptions::new();
            lock_options
                .read(true)
                .write(writable)
                .create(writable)
                .truncate(false);
            locks.push(
                lock_options
                    .open(append_path_suffix(
                        &shm_path,
                        &format!(".mount-rs-wal-lock-{index}"),
                    ))
                    .map_err(|error| VfsError::io("open SQLite WAL lock", error))?,
            );
        }
        Ok(Self {
            path: shm_path,
            file,
            dms,
            dms_held: true,
            writable,
            region_size: None,
            mapped: Vec::new(),
            locks,
            held: [ShmLockMode::None; ffi::SQLITE_SHM_NLOCK as usize],
        })
    }

    fn map(
        &mut self,
        region: usize,
        region_size: usize,
        extend: bool,
    ) -> Result<*mut c_void, VfsError> {
        if region_size == 0 {
            return Err(VfsError::InvalidInput("WAL shared-memory region is empty"));
        }
        if let Some(existing) = self.region_size
            && existing != region_size
        {
            return Err(VfsError::InvalidInput(
                "WAL shared-memory region size changed",
            ));
        }
        self.region_size = Some(region_size);
        if region < self.mapped.len()
            && let Some(mapped) = &self.mapped[region]
        {
            return Ok(mapped.output);
        }
        let offset = region
            .checked_mul(region_size)
            .ok_or(VfsError::InvalidInput("WAL shared-memory offset overflows"))?;
        let end = offset
            .checked_add(region_size)
            .ok_or(VfsError::InvalidInput("WAL shared-memory size overflows"))?;
        let length = self
            .file
            .metadata()
            .map_err(|error| VfsError::io("stat SQLite WAL shared memory", error))?
            .len();
        if length < end as u64 {
            if !extend {
                return Ok(ptr::null_mut());
            }
            if !self.writable {
                return Err(VfsError::Io {
                    operation: "extend read-only SQLite WAL shared memory",
                    source: io::Error::new(io::ErrorKind::PermissionDenied, "read-only"),
                });
            }
            self.file
                .set_len(end as u64)
                .map_err(|error| VfsError::io("extend SQLite WAL shared memory", error))?;
        }
        let mapped = map_host_region(&self.file, offset, region_size, self.writable)?;
        if self.mapped.len() <= region {
            self.mapped.resize_with(region + 1, || None);
        }
        let output = mapped.output;
        self.mapped[region] = Some(mapped);
        Ok(output)
    }

    fn lock(&mut self, offset: usize, number: usize, flags: c_int) -> Result<(), VfsError> {
        let mode = if flags & ffi::SQLITE_SHM_SHARED != 0 {
            ShmLockMode::Shared
        } else {
            ShmLockMode::Exclusive
        };
        let locking = flags & ffi::SQLITE_SHM_LOCK != 0;
        let end = offset
            .checked_add(number)
            .ok_or(VfsError::InvalidInput("WAL lock range overflows"))?;
        if end > self.held.len() {
            return Err(VfsError::InvalidInput("WAL lock range is outside SQLite"));
        }
        if !locking {
            for index in offset..end {
                if self.held[index] == ShmLockMode::None {
                    continue;
                }
                if self.held[index] != mode {
                    return Err(VfsError::InvalidInput(
                        "WAL lock unlock mode does not match the held mode",
                    ));
                }
            }
            for index in offset..end {
                if self.held[index] != ShmLockMode::None {
                    unlock_file(&self.locks[index])?;
                    self.held[index] = ShmLockMode::None;
                }
            }
            return Ok(());
        }

        for index in offset..end {
            if self.held[index] != ShmLockMode::None && self.held[index] != mode {
                return Err(VfsError::Busy);
            }
        }
        let mut acquired = Vec::new();
        for index in offset..end {
            if self.held[index] == mode {
                continue;
            }
            let result = lock_file(&self.locks[index], mode == ShmLockMode::Exclusive);
            if let Err(error) = result {
                for acquired_index in acquired {
                    let _ = unlock_file(&self.locks[acquired_index]);
                }
                return Err(error);
            }
            acquired.push(index);
        }
        for index in acquired {
            self.held[index] = mode;
        }
        Ok(())
    }

    fn barrier(&mut self) {
        std::sync::atomic::fence(std::sync::atomic::Ordering::SeqCst);
    }

    fn unmap(&mut self, delete: bool) -> Result<(), VfsError> {
        let mut first_error = None;
        for mapped in self.mapped.iter_mut().filter_map(Option::take) {
            if let Err(error) = unmap_host_region(mapped)
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        for index in 0..self.held.len() {
            if self.held[index] != ShmLockMode::None {
                if let Err(error) = unlock_file(&self.locks[index])
                    && first_error.is_none()
                {
                    first_error = Some(error);
                }
                self.held[index] = ShmLockMode::None;
            }
        }
        self.region_size = None;
        let dms_released = if self.dms_held {
            self.dms_held = false;
            match unlock_file(&self.dms) {
                Ok(()) => true,
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                    false
                }
            }
        } else {
            false
        };
        if delete && dms_released {
            match lock_file(&self.dms, true) {
                Ok(()) => {
                    if let Err(error) = remove_wal_artifact(&self.path)
                        && first_error.is_none()
                    {
                        first_error = Some(error);
                    }
                    for index in 0..self.locks.len() {
                        if let Err(error) = remove_wal_artifact(&append_path_suffix(
                            &self.path,
                            &format!(".mount-rs-wal-lock-{index}"),
                        )) && first_error.is_none()
                        {
                            first_error = Some(error);
                        }
                    }
                    if let Err(error) = remove_wal_artifact(&append_path_suffix(
                        &self.path,
                        ".mount-rs-wal-dms-lock",
                    )) && first_error.is_none()
                    {
                        first_error = Some(error);
                    }
                    if let Err(error) = unlock_file(&self.dms)
                        && first_error.is_none()
                    {
                        first_error = Some(error);
                    }
                }
                Err(VfsError::Busy) => {}
                Err(error) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

impl Drop for HostShm {
    fn drop(&mut self) {
        let _ = self.unmap(false);
    }
}

fn append_path_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name: OsString = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(unix)]
fn map_host_region(
    file: &File,
    offset: usize,
    length: usize,
    writable: bool,
) -> Result<HostMappedRegion, VfsError> {
    let offset = libc::off_t::try_from(offset)
        .map_err(|_| VfsError::InvalidInput("WAL shared-memory offset exceeds off_t"))?;
    let protection = if writable {
        libc::PROT_READ | libc::PROT_WRITE
    } else {
        libc::PROT_READ
    };
    let mapped = unsafe {
        libc::mmap(
            ptr::null_mut(),
            length,
            protection,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            offset,
        )
    };
    if mapped == libc::MAP_FAILED {
        return Err(VfsError::io(
            "map SQLite WAL shared memory",
            io::Error::last_os_error(),
        ));
    }
    Ok(HostMappedRegion {
        output: mapped,
        len: length,
    })
}

#[cfg(unix)]
fn unmap_host_region(mapped: HostMappedRegion) -> Result<(), VfsError> {
    let result = unsafe { libc::munmap(mapped.output, mapped.len) };
    if result == 0 {
        Ok(())
    } else {
        Err(VfsError::io(
            "unmap SQLite WAL shared memory",
            io::Error::last_os_error(),
        ))
    }
}

#[cfg(windows)]
const WINDOWS_ALLOCATION_GRANULARITY: u64 = 64 * 1024;

#[cfg(windows)]
#[link(name = "kernel32")]
#[allow(non_snake_case)]
unsafe extern "system" {
    fn CreateFileMappingW(
        file: *mut c_void,
        attributes: *mut c_void,
        protect: u32,
        maximum_size_high: u32,
        maximum_size_low: u32,
        name: *const u16,
    ) -> *mut c_void;
    fn MapViewOfFile(
        mapping: *mut c_void,
        access: u32,
        offset_high: u32,
        offset_low: u32,
        bytes: usize,
    ) -> *mut c_void;
    fn UnmapViewOfFile(address: *const c_void) -> i32;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

#[cfg(windows)]
fn map_host_region(
    file: &File,
    offset: usize,
    length: usize,
    writable: bool,
) -> Result<HostMappedRegion, VfsError> {
    const PAGE_READONLY: u32 = 0x02;
    const PAGE_READWRITE: u32 = 0x04;
    const FILE_MAP_READ: u32 = 0x0004;
    const FILE_MAP_ALL_ACCESS: u32 = 0x001f_ffff;

    let offset = offset as u64;
    let base = offset / WINDOWS_ALLOCATION_GRANULARITY * WINDOWS_ALLOCATION_GRANULARITY;
    let delta = usize::try_from(offset - base)
        .map_err(|_| VfsError::InvalidInput("WAL shared-memory offset exceeds usize"))?;
    let view_length = delta
        .checked_add(length)
        .ok_or(VfsError::InvalidInput("WAL shared-memory view overflows"))?;
    let maximum = base
        .checked_add(view_length as u64)
        .ok_or(VfsError::InvalidInput(
            "WAL shared-memory mapping overflows",
        ))?;
    let maximum_low = maximum as u32;
    let maximum_high = (maximum >> 32) as u32;
    let mapping = unsafe {
        CreateFileMappingW(
            file.as_raw_handle(),
            ptr::null_mut(),
            if writable {
                PAGE_READWRITE
            } else {
                PAGE_READONLY
            },
            maximum_high,
            maximum_low,
            ptr::null(),
        )
    };
    if mapping.is_null() {
        return Err(VfsError::io(
            "create SQLite WAL shared-memory mapping",
            io::Error::last_os_error(),
        ));
    }
    let view = unsafe {
        MapViewOfFile(
            mapping,
            if writable {
                FILE_MAP_ALL_ACCESS
            } else {
                FILE_MAP_READ
            },
            (base >> 32) as u32,
            base as u32,
            view_length,
        )
    };
    if view.is_null() {
        unsafe { CloseHandle(mapping) };
        return Err(VfsError::io(
            "map SQLite WAL shared memory",
            io::Error::last_os_error(),
        ));
    }
    let output = unsafe { (view.cast::<u8>()).add(delta).cast::<c_void>() };
    Ok(HostMappedRegion {
        output,
        view,
        mapping,
    })
}

#[cfg(windows)]
fn unmap_host_region(mapped: HostMappedRegion) -> Result<(), VfsError> {
    let unmapped = unsafe { UnmapViewOfFile(mapped.view) } != 0;
    let closed = unsafe { CloseHandle(mapped.mapping) } != 0;
    if unmapped && closed {
        Ok(())
    } else {
        Err(VfsError::io(
            "unmap SQLite WAL shared memory",
            io::Error::last_os_error(),
        ))
    }
}

#[cfg(unix)]
fn remove_wal_artifact(path: &Path) -> Result<(), VfsError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VfsError::io("delete SQLite WAL shared memory", error)),
    }
}

#[cfg(windows)]
fn remove_wal_artifact(path: &Path) -> Result<(), VfsError> {
    match remove_file_with_retries(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(VfsError::io("delete SQLite WAL shared memory", error)),
    }
}

impl Drop for HostFile {
    fn drop(&mut self) {
        self.shm.take();
        if let Some(locks) = self.locks.as_mut() {
            let _ = locks.unlock_all();
        }
        if self.delete_on_close {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl VfsFile for HostFile {
    fn shm_map(
        &mut self,
        region: usize,
        region_size: usize,
        extend: bool,
    ) -> Result<*mut c_void, VfsError> {
        if self.wal_scope != WalScope::HostLocal {
            return Err(VfsError::Unsupported("HostLocal WAL shared memory"));
        }
        if self.shm.is_none() {
            self.shm = Some(HostShm::open(&self.path, self.writable)?);
        }
        self.shm
            .as_mut()
            .expect("WAL shared memory initialized")
            .map(region, region_size, extend)
    }

    fn shm_lock(&mut self, offset: usize, number: usize, flags: c_int) -> Result<(), VfsError> {
        if self.wal_scope != WalScope::HostLocal {
            return Err(VfsError::Unsupported("HostLocal WAL locks"));
        }
        if self.shm.is_none() {
            self.shm = Some(HostShm::open(&self.path, self.writable)?);
        }
        self.shm
            .as_mut()
            .expect("WAL shared memory initialized")
            .lock(offset, number, flags)
    }

    fn shm_barrier(&mut self) {
        if let Some(shm) = self.shm.as_mut() {
            shm.barrier();
        }
    }

    fn shm_unmap(&mut self, delete: bool) -> Result<(), VfsError> {
        let Some(mut shm) = self.shm.take() else {
            return Ok(());
        };
        shm.unmap(delete)
    }

    fn read_at(&mut self, output: &mut [u8], offset: u64) -> Result<usize, VfsError> {
        #[cfg(unix)]
        let result = self.file.read_at(output, offset);
        #[cfg(windows)]
        let result = self.file.seek_read(output, offset);
        result.map_err(|error| VfsError::io("read SQLite file", error))
    }

    fn write_at(&mut self, input: &[u8], offset: u64) -> Result<(), VfsError> {
        let mut written = 0;
        while written < input.len() {
            #[cfg(unix)]
            let result = self
                .file
                .write_at(&input[written..], offset + written as u64);
            #[cfg(windows)]
            let result = self
                .file
                .seek_write(&input[written..], offset + written as u64);
            let count = result.map_err(|error| VfsError::io("write SQLite file", error))?;
            if count == 0 {
                return Err(VfsError::io(
                    "write SQLite file",
                    io::Error::new(io::ErrorKind::WriteZero, "zero-length write"),
                ));
            }
            written += count;
        }
        Ok(())
    }

    fn truncate(&mut self, size: u64) -> Result<(), VfsError> {
        self.file
            .set_len(size)
            .map_err(|error| VfsError::io("truncate SQLite file", error))
    }

    fn sync(&mut self, data_only: bool) -> Result<(), VfsError> {
        #[cfg(windows)]
        {
            // SQLite's native win32 VFS uses FlushFileBuffers for both
            // SQLITE_SYNC_NORMAL and SQLITE_SYNC_FULL, including the
            // DATAONLY variant.  File::sync_all is the corresponding Rust
            // operation; Windows has no separate fdatasync contract here.
            let _ = data_only;
            self.file
                .sync_all()
                .map_err(|error| VfsError::io("sync SQLite file", error))
        }

        #[cfg(not(windows))]
        {
            if data_only {
                self.file
                    .sync_data()
                    .map_err(|error| VfsError::io("sync SQLite data", error))
            } else {
                self.file
                    .sync_all()
                    .map_err(|error| VfsError::io("sync SQLite file", error))
            }
        }
    }

    fn size(&mut self) -> Result<u64, VfsError> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|error| VfsError::io("stat SQLite file", error))
    }

    fn lock(&mut self, level: LockLevel) -> Result<(), VfsError> {
        let Some(locks) = self.locks.as_mut() else {
            return Ok(());
        };
        locks.lock(level)
    }

    fn unlock(&mut self, level: LockLevel) -> Result<(), VfsError> {
        let Some(locks) = self.locks.as_mut() else {
            return Ok(());
        };
        locks.unlock(level)
    }

    fn check_reserved_lock(&mut self) -> Result<bool, VfsError> {
        let Some(locks) = self.locks.as_mut() else {
            return Ok(false);
        };
        locks.check_reserved()
    }
}

#[cfg(windows)]
fn remove_file_with_retries(path: &Path) -> io::Result<()> {
    // Match SQLite's winRetryIoerr policy for transient antivirus/indexer
    // sharing conflicts without retrying permanent path or access errors.
    const MAX_RETRIES: usize = 10;
    const RETRY_DELAY_MS: u64 = 25;

    let mut retries = 0;
    loop {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error)
                if retries < MAX_RETRIES
                    && matches!(
                        error.raw_os_error(),
                        Some(5)    // ERROR_ACCESS_DENIED
                            | Some(32) // ERROR_SHARING_VIOLATION
                            | Some(33) // ERROR_LOCK_VIOLATION
                            | Some(55) // ERROR_DEV_NOT_EXIST
                            | Some(64) // ERROR_NETNAME_DELETED
                            | Some(121) // ERROR_SEM_TIMEOUT
                            | Some(1231) // ERROR_NETWORK_UNREACHABLE
                    ) =>
            {
                retries += 1;
                std::thread::sleep(Duration::from_millis(RETRY_DELAY_MS * retries as u64));
            }
            Err(error) => return Err(error),
        }
    }
}

struct LockFiles {
    shared: File,
    reserved: File,
    pending: File,
    level: LockLevel,
}

impl LockFiles {
    fn open(path: &Path) -> Result<Self, VfsError> {
        let base = path_to_sqlite(path)?;
        let open_lock = |suffix: &[u8]| -> Result<File, VfsError> {
            let mut name = base.clone();
            name.extend_from_slice(suffix);
            StdOpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path_from_sqlite(&name)?)
                .map_err(|error| VfsError::io("open SQLite lock sidecar", error))
        };
        Ok(Self {
            shared: open_lock(b".mount-rs-shared-lock")?,
            reserved: open_lock(b".mount-rs-reserved-lock")?,
            pending: open_lock(b".mount-rs-pending-lock")?,
            level: LockLevel::None,
        })
    }

    fn lock(&mut self, requested: LockLevel) -> Result<(), VfsError> {
        if requested <= self.level {
            return Ok(());
        }
        if self.level < LockLevel::Shared {
            // The pending lock is a reader gate, not a persistent reader lock.
            // A reader takes it while acquiring the shared lock, then releases
            // it.  This lets a writer acquire the pending gate while existing
            // readers remain visible on `shared`, which is the SQLite
            // PENDING -> EXCLUSIVE transition.
            lock_file(&self.pending, false)?;
            if let Err(error) = lock_file(&self.shared, false) {
                let _ = unlock_file(&self.pending);
                return Err(error);
            }
            if let Err(error) = unlock_file(&self.pending) {
                let _ = unlock_file(&self.shared);
                return Err(error);
            }
            self.level = LockLevel::Shared;
        }
        if requested >= LockLevel::Reserved && self.level < LockLevel::Reserved {
            lock_file(&self.reserved, true)?;
            self.level = LockLevel::Reserved;
        }
        if requested >= LockLevel::Pending && self.level < LockLevel::Pending {
            lock_file(&self.pending, true)?;
            self.level = LockLevel::Pending;
        }
        if requested >= LockLevel::Exclusive && self.level < LockLevel::Exclusive {
            // Do not rely on same-handle shared/exclusive lock overlap.  The
            // pending gate blocks new readers while this handle briefly
            // releases and reacquires its shared byte range.
            unlock_file(&self.shared)?;
            if let Err(error) = lock_file(&self.shared, true) {
                lock_file(&self.shared, false)?;
                return Err(error);
            }
            self.level = LockLevel::Exclusive;
        }
        Ok(())
    }

    fn unlock(&mut self, requested: LockLevel) -> Result<(), VfsError> {
        if requested >= self.level {
            return Ok(());
        }
        if requested < LockLevel::Exclusive && self.level == LockLevel::Exclusive {
            unlock_file(&self.shared)?;
            lock_file(&self.shared, false)?;
            self.level = LockLevel::Pending;
        }
        if requested < LockLevel::Pending && self.level >= LockLevel::Pending {
            unlock_file(&self.pending)?;
            self.level = LockLevel::Reserved;
        }
        if requested < LockLevel::Reserved && self.level >= LockLevel::Reserved {
            unlock_file(&self.reserved)?;
            self.level = LockLevel::Shared;
        }
        if requested < LockLevel::Shared && self.level >= LockLevel::Shared {
            unlock_file(&self.shared)?;
            self.level = LockLevel::None;
        }
        Ok(())
    }

    fn unlock_all(&mut self) -> Result<(), VfsError> {
        self.unlock(LockLevel::None)
    }

    fn check_reserved(&mut self) -> Result<bool, VfsError> {
        if self.level >= LockLevel::Reserved {
            return Ok(true);
        }
        match lock_file(&self.reserved, true) {
            Ok(()) => {
                unlock_file(&self.reserved)?;
                Ok(false)
            }
            Err(VfsError::Busy) => Ok(true),
            Err(error) => Err(error),
        }
    }
}

#[cfg(unix)]
fn lock_file(file: &File, exclusive: bool) -> Result<(), VfsError> {
    let operation = if exclusive {
        libc::LOCK_EX | libc::LOCK_NB
    } else {
        libc::LOCK_SH | libc::LOCK_NB
    };
    flock(file, operation)
}

#[cfg(unix)]
fn unlock_file(file: &File) -> Result<(), VfsError> {
    flock(file, libc::LOCK_UN)
}

#[cfg(unix)]
fn flock(file: &File, operation: c_int) -> Result<(), VfsError> {
    loop {
        let result = unsafe { libc::flock(file.as_raw_fd(), operation) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.raw_os_error() == Some(libc::EWOULDBLOCK)
            || error.raw_os_error() == Some(libc::EAGAIN)
        {
            return Err(VfsError::Busy);
        }
        return Err(VfsError::io("SQLite file lock", error));
    }
}

#[cfg(windows)]
#[repr(C)]
struct WindowsOverlapped {
    internal: usize,
    internal_high: usize,
    offset: u32,
    offset_high: u32,
    event: *mut c_void,
}

#[cfg(windows)]
#[link(name = "kernel32")]
#[allow(non_snake_case)]
unsafe extern "system" {
    fn LockFileEx(
        file: *mut c_void,
        flags: u32,
        reserved: u32,
        bytes_to_lock_low: u32,
        bytes_to_lock_high: u32,
        overlapped: *mut WindowsOverlapped,
    ) -> i32;
    fn UnlockFileEx(
        file: *mut c_void,
        reserved: u32,
        bytes_to_unlock_low: u32,
        bytes_to_unlock_high: u32,
        overlapped: *mut WindowsOverlapped,
    ) -> i32;
}

#[cfg(windows)]
fn lock_file(file: &File, exclusive: bool) -> Result<(), VfsError> {
    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
    const ERROR_LOCK_VIOLATION: i32 = 33;
    const ERROR_SHARING_VIOLATION: i32 = 32;
    const ERROR_IO_PENDING: i32 = 997;

    let mut overlapped = WindowsOverlapped {
        internal: 0,
        internal_high: 0,
        offset: 0,
        offset_high: 0,
        event: ptr::null_mut(),
    };
    let flags = LOCKFILE_FAIL_IMMEDIATELY
        | if exclusive {
            LOCKFILE_EXCLUSIVE_LOCK
        } else {
            0
        };
    let result = unsafe { LockFileEx(file.as_raw_handle(), flags, 0, 1, 0, &mut overlapped) };
    if result != 0 {
        return Ok(());
    }

    let error = io::Error::last_os_error();
    match error.raw_os_error() {
        Some(code)
            if code == ERROR_LOCK_VIOLATION
                || code == ERROR_SHARING_VIOLATION
                || code == ERROR_IO_PENDING =>
        {
            Err(VfsError::Busy)
        }
        _ => Err(VfsError::io("SQLite file lock", error)),
    }
}

#[cfg(windows)]
fn unlock_file(file: &File) -> Result<(), VfsError> {
    let mut overlapped = WindowsOverlapped {
        internal: 0,
        internal_high: 0,
        offset: 0,
        offset_high: 0,
        event: ptr::null_mut(),
    };
    let result = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
    if result != 0 {
        Ok(())
    } else {
        Err(VfsError::io(
            "SQLite file unlock",
            io::Error::last_os_error(),
        ))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            other => output.push(other.as_os_str()),
        }
    }
    output
}

#[cfg(windows)]
#[link(name = "bcrypt")]
#[allow(non_snake_case)]
unsafe extern "system" {
    fn BCryptGenRandom(algorithm: *mut c_void, buffer: *mut u8, length: u32, flags: u32) -> i32;
}

#[cfg(windows)]
fn fill_randomness(output: &mut [u8]) -> Result<(), VfsError> {
    if output.is_empty() {
        return Ok(());
    }

    const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 0x0000_0002;
    let length = u32::try_from(output.len())
        .map_err(|_| VfsError::InvalidInput("randomness request is too large"))?;
    let status = unsafe {
        BCryptGenRandom(
            ptr::null_mut(),
            output.as_mut_ptr(),
            length,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(VfsError::Other(format!(
            "Windows system randomness failed with NTSTATUS 0x{status:08x}"
        )))
    }
}

#[cfg(not(windows))]
fn fill_randomness(output: &mut [u8]) -> Result<(), VfsError> {
    if output.is_empty() {
        return Ok(());
    }
    if let Ok(mut random) = File::open("/dev/urandom") {
        use std::io::Read;
        if random.read_exact(output).is_ok() {
            return Ok(());
        }
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| VfsError::Other("system clock is before Unix epoch".to_owned()))?;
    let mut state = now.as_nanos() as u64 ^ (std::process::id() as u64);
    for byte in output {
        state ^= state << 7;
        state ^= state >> 9;
        state ^= state << 8;
        *byte = state as u8;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn julian_epoch_is_exact() {
        let value = JULIAN_UNIX_EPOCH_MILLIS;
        assert_eq!(value, 210_866_760_000_000);
    }

    #[test]
    fn lock_levels_order_as_sqlite_requires() {
        assert!(LockLevel::Exclusive > LockLevel::Pending);
        assert!(LockLevel::Pending > LockLevel::Reserved);
        assert!(LockLevel::Reserved > LockLevel::Shared);
    }

    #[test]
    fn rollback_vfs_rejects_invalid_shared_memory_callbacks() {
        let mut mapped = ptr::null_mut();
        assert_eq!(
            unsafe { x_shm_map(ptr::null_mut(), 0, 1, 0, &mut mapped) },
            ffi::SQLITE_MISUSE
        );
        assert_eq!(
            unsafe { x_shm_lock(ptr::null_mut(), 0, 1, 0) },
            ffi::SQLITE_MISUSE
        );
        unsafe { x_shm_barrier(ptr::null_mut()) };
        assert_eq!(
            unsafe { x_shm_unmap(ptr::null_mut(), 0) },
            ffi::SQLITE_MISUSE
        );
        assert!(mapped.is_null());
    }
}
