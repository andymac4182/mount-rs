//! Independently selectable SQLite metadata and immutable block providers.
//! Each database contains one namespace. Metadata and blocks may reside in
//! different databases, or either provider may be paired with another backend.

use async_trait::async_trait;
use mount_rs_core::storage::{
    BlockId, BlockStore, CheckoutRequest, ConcurrentBackingId, ConcurrentModeState,
    DelegatedCheckin, DelegatedPublish, DelegatedRecovery, DelegationState, DirectoryGrant,
    LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::versioning::{
    PublicationId, ReadLease, ReadLeaseRequest, VersionHead, VersionId, VersionInfo, VersionKind,
    VersionPublication, VersionedMetadataStore, VolumeId,
};
use mount_rs_core::{ErrorCode, FsError, Result, backend_error};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
#[cfg(unix)]
use std::{os::unix::ffi::OsStrExt, os::unix::fs::MetadataExt, path::PathBuf};

// SQLite supplies the clock inside the same statement as lease validation.
const NOW: &str = "CAST(unixepoch('subsec') * 1000 AS INTEGER)";
const NOW_SELECT: &str = "SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)";
const CONCURRENT_WRITE_MODE: &str = "MRC1";
const BOUND_WRITE_MODE: &str = "MRC2";
const DELEGATED_WRITE_MODE: &str = "MRC3";
const CONCURRENT_FENCE_SENTINEL: i64 = i64::MAX;
const MAX_SQLITE_BUSY_RETRIES: usize = 16;
const SQLITE_BUSY_RETRY_BUDGET: Duration = Duration::from_secs(30);
#[cfg(unix)]
const CONCURRENT_PUBLISH_BUSY_TIMEOUT: Duration = Duration::from_millis(250);

fn sqlite_busy_known_noncommit(
    error: &rusqlite::Error,
    confirmed_noncommit: bool,
    syscall: &'static str,
) -> Option<FsError> {
    // BEGIN IMMEDIATE cannot have written if it returned SQLITE_BUSY. A busy
    // COMMIT leaves the transaction active; rusqlite's default Drop rolls it
    // back. Check autocommit after Drop before allowing the caller to replay.
    (error.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy) && confirmed_noncommit)
        .then(|| {
            FsError::new(ErrorCode::Eagain)
                .with_syscall(syscall)
                .with_message("SQLite writer contention did not commit")
        })
}

async fn sqlite_busy_backoff(attempt: usize) {
    let window = 250_u64 << attempt.min(6);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| u64::from(duration.subsec_nanos()));
    let jitter = (tick ^ (attempt as u64).wrapping_mul(0x9e3779b97f4a7c15)) % window;
    async_io::Timer::after(Duration::from_micros((window + jitter).min(20_000))).await;
}

#[cfg(target_os = "macos")]
fn require_local_concurrent_backing(
    path: &Path,
    kind: &'static str,
    _expected: FileStamp,
) -> Result<()> {
    use std::{ffi::CString, mem::MaybeUninit, os::unix::ffi::OsStrExt};

    let path = path.canonicalize().map_err(|error| {
        FsError::new(ErrorCode::Eio)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message(format!("cannot resolve SQLite {kind} file: {error}"))
    })?;
    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        FsError::new(ErrorCode::Einval)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message(format!("SQLite {kind} path contains a NUL byte"))
    })?;
    let mut filesystem = MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::statfs(path.as_ptr(), filesystem.as_mut_ptr()) } != 0 {
        return Err(FsError::new(ErrorCode::Eio)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message(format!(
                "cannot inspect SQLite {kind} filesystem: {}",
                std::io::Error::last_os_error()
            )));
    }
    let filesystem = unsafe { filesystem.assume_init() };
    if filesystem.f_flags & (libc::MNT_LOCAL as u32) == 0 {
        let verb = if kind == "blocks" {
            "require"
        } else {
            "requires"
        };
        return Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("prepare concurrent SQLite volume")
            .with_message(format!(
                "concurrent SQLite {kind} {verb} a local filesystem; NFS/network-backed databases are unsupported",
            )));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn linux_qualified_local_fs_type(magic: u32) -> bool {
    // Linux UAPI include/uapi/linux/magic.h. TMPFS is local, but its contents
    // do not survive a host reboot; callers must choose durable backing when
    // that recovery guarantee matters.
    const EXT_FAMILY: u32 = 0x0000_ef53;
    const XFS: u32 = 0x5846_5342;
    const BTRFS: u32 = 0x9123_683e;
    const F2FS: u32 = 0xf2f5_2010;
    const TMPFS: u32 = 0x0102_1994;
    [EXT_FAMILY, XFS, BTRFS, F2FS, TMPFS].contains(&magic)
}

#[cfg(any(all(target_os = "linux", target_env = "gnu"), test))]
fn require_linux_mount_attributes(
    attributes: u64,
    supported: u64,
    kind: &'static str,
) -> Result<()> {
    // Linux UAPI include/uapi/linux/stat.h; available since Linux 5.8.
    // A clear unsupported attribute is not evidence that this is an ordinary file.
    // https://man7.org/linux/man-pages/man2/statx.2.html
    const MOUNT_ROOT: u64 = 0x2000;
    if supported & MOUNT_ROOT == 0 {
        return Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message("concurrent SQLite requires Linux 5.8 or newer with supported STATX_ATTR_MOUNT_ROOT inspection"));
    }
    if attributes & MOUNT_ROOT != 0 {
        return Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("prepare concurrent SQLite volume")
            .with_message(format!(
                "concurrent SQLite {kind} require a directory mount; individually mounted database files are unsupported"
            )));
    }
    Ok(())
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn require_linux_file_mount_status(file: &std::fs::File, kind: &'static str) -> Result<()> {
    use std::{mem::MaybeUninit, os::fd::AsRawFd};

    // Use the syscall directly so concurrent inspection does not add the
    // glibc 2.28 statx wrapper as a load-time requirement for legacy users.
    // The supported attribute mask still requires Linux 5.8 or newer.
    let mut status = MaybeUninit::<libc::statx>::uninit();
    if unsafe {
        libc::syscall(
            libc::SYS_statx,
            file.as_raw_fd(),
            c"".as_ptr(),
            libc::AT_EMPTY_PATH,
            libc::STATX_BASIC_STATS,
            status.as_mut_ptr(),
        )
    } != 0
    {
        return Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message(format!(
                "cannot inspect SQLite {kind} selected file mount status: {}",
                std::io::Error::last_os_error()
            )));
    }
    let status = unsafe { status.assume_init() };
    require_linux_mount_attributes(status.stx_attributes, status.stx_attributes_mask, kind)
}

#[cfg(all(target_os = "linux", not(target_env = "gnu")))]
fn require_linux_file_mount_status(_file: &std::fs::File, _kind: &'static str) -> Result<()> {
    // The locked libc exposes the statx UAPI type on GNU targets. Other Linux
    // environments keep legacy/exclusive operation and refuse this opt-in.
    Err(FsError::new(ErrorCode::Enotsup)
        .with_syscall("inspect concurrent SQLite backing")
        .with_message("concurrent SQLite requires a qualified GNU Linux target with supported STATX_ATTR_MOUNT_ROOT inspection"))
}

#[cfg(target_os = "linux")]
fn require_local_concurrent_backing(
    path: &Path,
    kind: &'static str,
    expected: FileStamp,
) -> Result<()> {
    use std::{mem::MaybeUninit, os::fd::AsRawFd, os::unix::fs::OpenOptionsExt};

    // SQLite does not expose its open descriptor. Open the canonical database
    // file ourselves, then require its physical stamp to match the file that
    // this provider selected at open. Never classify only the parent path.
    // Closing a regular inspection descriptor would release this process's
    // SQLite POSIX locks for the inode. O_PATH supports fstat/fstatfs without
    // the regular-descriptor close path's locks_remove_posix side effect.
    // https://man7.org/linux/man-pages/man2/open.2.html
    // https://github.com/torvalds/linux/blob/v6.18/fs/open.c#L1451-L1466
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| {
            FsError::new(ErrorCode::Eio)
                .with_syscall("inspect concurrent SQLite backing")
                .with_message(format!(
                    "cannot open SQLite {kind} file for inspection: {error}"
                ))
        })?;
    let metadata = file.metadata().map_err(backend_error)?;
    let inspected = FileStamp {
        dev: metadata.dev(),
        ino: metadata.ino(),
    };
    if inspected != expected {
        return Err(FsError::new(ErrorCode::Estale)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message(format!(
                "SQLite {kind} inspection selected another physical file"
            )));
    }
    let mut filesystem = MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
        return Err(FsError::new(ErrorCode::Eio)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message(format!(
                "cannot inspect SQLite {kind} filesystem: {}",
                std::io::Error::last_os_error()
            )));
    }
    let magic = unsafe { filesystem.assume_init() }.f_type as u32;
    // An overlay can hide a remote writable upper layer, so it is refused.
    if !linux_qualified_local_fs_type(magic) {
        return Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("prepare concurrent SQLite volume")
            .with_message(format!(
                "concurrent SQLite {kind} require a qualified local filesystem; type 0x{magic:08x} is unsupported"
            )));
    }
    // A file-only bind mount can hide a committed authority in another WAL
    // namespace. Classify the selected file independently of any SQLite row,
    // before a concurrent authority INSERT or namespace publication. Provider
    // schema open is earlier and is not promised to have zero side effects.
    // statx with AT_EMPTY_PATH classifies this descriptor's mount root directly,
    // so no pathname decoding or mountinfo/pathname comparison is required.
    // https://man7.org/linux/man-pages/man2/statx.2.html
    require_linux_file_mount_status(&file, kind)
}

#[derive(Clone)]
struct Database {
    connection: Arc<Mutex<Connection>>,
    durable: bool,
    #[cfg(unix)]
    opened_file: Option<OpenedFile>,
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileStamp {
    dev: u64,
    ino: u64,
}

#[cfg(unix)]
impl FileStamp {
    fn at(path: &Path) -> Result<Self> {
        let metadata = std::fs::metadata(path).map_err(backend_error)?;
        Ok(Self {
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }

    fn from_text(dev: Option<&str>, ino: Option<&str>) -> Result<Option<Self>> {
        match (dev, ino) {
            (None, None) => Ok(None),
            (Some(dev), Some(ino)) => Ok(Some(Self {
                dev: dev
                    .parse()
                    .map_err(|_| incompatible_schema("SQLite metadata file stamp is invalid"))?,
                ino: ino
                    .parse()
                    .map_err(|_| incompatible_schema("SQLite metadata file stamp is invalid"))?,
            })),
            _ => Err(incompatible_schema(
                "SQLite metadata file stamp is incomplete",
            )),
        }
    }
}

#[cfg(unix)]
fn require_matching_metadata_stamp(
    database: &Database,
    stored_dev: Option<&str>,
    stored_ino: Option<&str>,
    stored_path: Option<&str>,
) -> Result<FileStamp> {
    let stored = FileStamp::from_text(stored_dev, stored_ino)?
        .ok_or_else(|| incompatible_schema("SQLite metadata has no trusted file stamp for MRC2"))?;
    let actual = database.require_concurrent_local_file("metadata")?;
    if stored != actual {
        return Err(FsError::new(ErrorCode::Estale)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message("SQLite metadata authority belongs to another physical file"));
    }
    require_matching_auxiliary_path(database, stored_path)?;
    Ok(actual)
}

#[cfg(unix)]
fn require_matching_auxiliary_path(database: &Database, stored: Option<&str>) -> Result<()> {
    let stored = stored
        .filter(|path| {
            !path.is_empty()
                && path.len().is_multiple_of(2)
                && path
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
        .ok_or_else(|| {
            incompatible_schema("SQLite MRC2 authority has no trusted auxiliary path")
        })?;
    let actual = database.current_auxiliary_path()?;
    if stored != actual {
        return Err(FsError::new(ErrorCode::Estale)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message("SQLite auxiliary file authority belongs to another pathname"));
    }
    Ok(())
}

#[cfg(unix)]
fn encode_auxiliary_path(path: &Path) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let bytes = path.as_os_str().as_bytes();
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

#[cfg(unix)]
#[derive(Clone)]
struct OpenedFile {
    // SQLite does not expose its file descriptor through rusqlite. These
    // paths and stamps reject ordinary copy/restore and stable retargeting;
    // they do not prove descriptor identity across an adversarial ABA swap.
    requested: PathBuf,
    canonical: PathBuf,
    stamp: FileStamp,
    created_by_open: bool,
}

#[cfg(unix)]
fn canonical_database_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(backend_error)?.join(path)
    };
    match absolute.canonicalize() {
        Ok(canonical) => Ok(canonical),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = absolute
                .parent()
                .ok_or_else(|| backend_error("SQLite path has no parent"))?;
            let name = absolute
                .file_name()
                .ok_or_else(|| backend_error("SQLite path has no file name"))?;
            Ok(parent.canonicalize().map_err(backend_error)?.join(name))
        }
        Err(error) => Err(backend_error(error)),
    }
}

impl Database {
    fn open(path: Option<&Path>, schema: &str) -> Result<Self> {
        if let Some(path) = path {
            super::ensure_parent_directory(path)?;
        }
        #[cfg(unix)]
        let path_info = path
            .filter(|path| {
                !path.as_os_str().is_empty()
                    && *path != Path::new(":memory:")
                    && !path.to_str().is_some_and(|text| text.starts_with("file:"))
            })
            .map(|path| -> Result<_> {
                let requested = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    std::env::current_dir().map_err(backend_error)?.join(path)
                };
                let before = match std::fs::metadata(&requested) {
                    Ok(metadata) => Some(FileStamp {
                        dev: metadata.dev(),
                        ino: metadata.ino(),
                    }),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(backend_error(error)),
                };
                let canonical = canonical_database_path(&requested)?;
                let (expected, created_by_open) = if let Some(before) = before {
                    (before, false)
                } else {
                    // Claim a truly fresh database path before SQLite opens
                    // it. ENOENT alone would let an intervening copied legacy
                    // file become incorrectly enrolled as our new backing.
                    let file = std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&canonical)
                        .map_err(|error| {
                            if error.kind() == std::io::ErrorKind::AlreadyExists {
                                FsError::new(ErrorCode::Estale)
                                    .with_syscall("open SQLite backing")
                                    .with_message("SQLite file appeared during exclusive creation")
                            } else {
                                backend_error(error)
                            }
                        })?;
                    let metadata = file.metadata().map_err(backend_error)?;
                    (
                        FileStamp {
                            dev: metadata.dev(),
                            ino: metadata.ino(),
                        },
                        true,
                    )
                };
                Ok((requested, canonical, expected, created_by_open))
            })
            .transpose()?;
        let connection = match path {
            #[cfg(unix)]
            Some(_) if path_info.is_some() => {
                Connection::open(&path_info.as_ref().expect("checked").1)
            }
            Some(path) => Connection::open(path),
            None => Connection::open_in_memory(),
        }
        .map_err(backend_error)?;
        #[cfg(unix)]
        let opened_file = if let Some((requested, canonical, expected, created_by_open)) = path_info
        {
            let stamp = FileStamp::at(&canonical)?;
            if FileStamp::at(&requested)? != stamp || expected != stamp {
                return Err(FsError::new(ErrorCode::Estale)
                    .with_syscall("open SQLite backing")
                    .with_message("SQLite file changed while opening"));
            }
            Some(OpenedFile {
                requested,
                canonical,
                stamp,
                created_by_open,
            })
        } else {
            None
        };
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(backend_error)?;
        connection
            .execute_batch("PRAGMA synchronous=FULL;")
            .map_err(backend_error)?;
        connection.execute_batch(schema).map_err(backend_error)?;
        // Empty paths and :memory: are not durable even when passed to open().
        let durable = connection.path().is_some_and(|path| !path.is_empty());
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
            durable,
            #[cfg(unix)]
            opened_file,
        })
    }

    #[cfg(unix)]
    fn current_file_stamp(&self) -> Result<FileStamp> {
        let opened = self.opened_file.as_ref().ok_or_else(|| {
            FsError::new(ErrorCode::Enotsup).with_syscall("inspect concurrent SQLite backing")
        })?;
        let requested = FileStamp::at(&opened.requested).map_err(|_| stale())?;
        let canonical = FileStamp::at(&opened.canonical).map_err(|_| stale())?;
        if requested != opened.stamp || canonical != opened.stamp {
            return Err(FsError::new(ErrorCode::Estale)
                .with_syscall("inspect concurrent SQLite backing")
                .with_message("SQLite file path no longer selects its opened physical file"));
        }
        Ok(opened.stamp)
    }

    #[cfg(unix)]
    fn current_auxiliary_path(&self) -> Result<String> {
        self.current_file_stamp()?;
        let opened = self.opened_file.as_ref().ok_or_else(|| {
            FsError::new(ErrorCode::Enotsup).with_syscall("inspect concurrent SQLite backing")
        })?;
        Ok(encode_auxiliary_path(&opened.canonical))
    }

    #[cfg(unix)]
    fn require_concurrent_local_file(&self, kind: &'static str) -> Result<FileStamp> {
        let stamp = self.current_file_stamp()?;
        let opened = self.opened_file.as_ref().ok_or_else(|| {
            FsError::new(ErrorCode::Enotsup).with_syscall("inspect concurrent SQLite backing")
        })?;
        // SQLite names rollback journals, WAL and shared-memory files from
        // the opened pathname. Two hard links share dev/inode but select
        // different auxiliary files, so neither can hold MRC2 authority.
        let links = std::fs::metadata(&opened.canonical)
            .map_err(|_| stale())?
            .nlink();
        if links != 1 {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("prepare concurrent SQLite volume")
                .with_message(format!(
                    "concurrent SQLite {kind} require a single pathname; hard-linked database files are unsupported"
                )));
        }
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        require_local_concurrent_backing(&opened.canonical, kind, stamp)?;
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        return Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("inspect concurrent SQLite backing")
            .with_message("this Unix platform has no qualified SQLite filesystem guard"));
        self.current_file_stamp()?;
        Ok(stamp)
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.connection
            .lock()
            .map_err(|_| backend_error("SQLite storage lock poisoned"))
    }

    #[cfg(unix)]
    fn with_concurrent_publish_timeout<T>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T>,
    ) -> Result<T> {
        let mut connection = self.lock()?;
        let original_ms: i64 = connection
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .map_err(backend_error)?;
        let original_ms = u64::try_from(original_ms)
            .map_err(|_| backend_error("SQLite busy timeout is negative"))?;
        connection
            .busy_timeout(CONCURRENT_PUBLISH_BUSY_TIMEOUT)
            .map_err(backend_error)?;
        let result = operation(&mut connection);
        // Restore under the same mutex even when the CAS fails. If restoration
        // fails after a commit, fail closed instead of reporting a success
        // while the connection has an unexpected contention policy.
        connection
            .busy_timeout(Duration::from_millis(original_ms))
            .map_err(backend_error)?;
        result
    }

    fn flush(&self) -> Result<()> {
        // Every modifying statement is an autocommit transaction with FULL
        // synchronous durability. There are no deferred writes to flush.
        let connection = self.lock()?;
        if !connection.is_autocommit() {
            return Err(backend_error(
                "SQLite storage has an unfinished transaction",
            ));
        }
        Ok(())
    }
}

const METADATA_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_metadata (
 id INTEGER PRIMARY KEY CHECK(id=1),
 revision INTEGER NOT NULL CHECK(revision>=0), namespace TEXT,
 owner TEXT, fence INTEGER NOT NULL CHECK(fence>=0), expires INTEGER NOT NULL);
 INSERT OR IGNORE INTO mount_rs_metadata
 (id, revision, namespace, owner, fence, expires) VALUES(1,0,NULL,NULL,0,0);";
const BLOCK_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS mount_rs_blocks (
 id TEXT PRIMARY KEY NOT NULL, bytes BLOB NOT NULL);
 CREATE TABLE IF NOT EXISTS mount_rs_block_authority (
 id INTEGER PRIMARY KEY CHECK(id=1),
 backing_id TEXT NOT NULL CHECK(length(backing_id)=32 AND backing_id!='00000000000000000000000000000000'),
 physical_dev TEXT, physical_ino TEXT, physical_path TEXT,
 CHECK((physical_dev IS NULL) = (physical_ino IS NULL)));";
const SCHEMA_VERSION_TABLE: &str = "mount_rs_schema_versions";
const VERSION_SCHEMA_NAME: &str = "mount-rs-versioning";
// Version 2 marks stores that can contain the namespace-publication kind.
// Keep version 1 until the first such record commits so older readers can reopen.
const VERSION_SCHEMA_BASE_VERSION: i64 = 1;
const VERSION_SCHEMA_VERSION: i64 = 2;

type MetadataModeRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
);
#[cfg(unix)]
type MetadataClaimRow = (
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
);
#[cfg(unix)]
type MetadataPublicationRow = (
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
);
#[cfg(unix)]
type MetadataMigrationRow = (
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
);

#[cfg(unix)]
type TrustedMrc1MigrationRow = (
    Option<String>,
    Option<String>,
    i64,
    Option<String>,
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
);

const VERSION_SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS mount_rs_version_state (
 id INTEGER PRIMARY KEY CHECK(id=1),
 volume_id TEXT NOT NULL,
 head_id TEXT,
 next_sequence INTEGER NOT NULL CHECK(next_sequence>0),
 next_read_fence INTEGER NOT NULL CHECK(next_read_fence>=0));
CREATE TABLE IF NOT EXISTS mount_rs_versions (
 id TEXT PRIMARY KEY NOT NULL,
 volume_id TEXT NOT NULL,
 sequence INTEGER NOT NULL CHECK(sequence>0),
 parent_id TEXT,
 restored_from TEXT,
 forked_from TEXT,
 namespace TEXT NOT NULL,
 block_store_id TEXT NOT NULL,
 kind TEXT NOT NULL,
 created_at_ms INTEGER NOT NULL,
 durable INTEGER NOT NULL CHECK(durable IN (0,1)),
 operation_id TEXT NOT NULL UNIQUE);
CREATE TABLE IF NOT EXISTS mount_rs_version_pins (
 view_id TEXT PRIMARY KEY NOT NULL,
 volume_id TEXT NOT NULL,
 version_id TEXT NOT NULL,
 owner TEXT NOT NULL,
 fence INTEGER NOT NULL CHECK(fence>0),
 expires INTEGER NOT NULL CHECK(expires>=0));";

fn expect_authority_constraint(result: rusqlite::Result<usize>) -> Result<()> {
    match result {
        Err(error)
            if error.sqlite_error_code() == Some(rusqlite::ErrorCode::ConstraintViolation) =>
        {
            Ok(())
        }
        Err(error) => Err(backend_error(error)),
        Ok(_) => Err(incompatible_schema(
            "SQLite block authority constraints are missing",
        )),
    }
}

fn require_authority_primary_key(connection: &Connection) -> Result<()> {
    let mut statement = connection
        .prepare("PRAGMA table_info(\"mount_rs_block_authority\")")
        .map_err(backend_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(backend_error)?;
    let mut valid_id = false;
    for row in rows {
        let (name, kind, ordinal) = row.map_err(backend_error)?;
        if name == "id" {
            valid_id = kind.eq_ignore_ascii_case("INTEGER") && ordinal == 1;
        } else if ordinal != 0 {
            return Err(incompatible_schema(
                "SQLite block authority needs id as sole primary key",
            ));
        }
    }
    if valid_id {
        Ok(())
    } else {
        Err(incompatible_schema(
            "SQLite block authority needs id as sole primary key",
        ))
    }
}

fn initialize_block_schema(database: &Database) -> Result<()> {
    let mut connection = database.lock()?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(backend_error)?;
    let table = "mount_rs_block_authority";
    let columns = table_columns(&tx, table)?
        .ok_or_else(|| incompatible_schema("SQLite block authority table is missing"))?;
    require_columns(table, &columns, &["id", "backing_id"])?;
    require_authority_primary_key(&tx)?;
    if !columns.contains("physical_dev") {
        tx.execute(
            "ALTER TABLE mount_rs_block_authority ADD COLUMN physical_dev TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    if !columns.contains("physical_ino") {
        tx.execute(
            "ALTER TABLE mount_rs_block_authority ADD COLUMN physical_ino TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    if !columns.contains("physical_path") {
        tx.execute(
            "ALTER TABLE mount_rs_block_authority ADD COLUMN physical_path TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    // Probe actual constraints inside a transaction; matching column names
    // alone do not establish a unique authority row.
    let valid = "11111111111111111111111111111111";
    expect_authority_constraint(tx.execute(
        "INSERT INTO mount_rs_block_authority(id,backing_id) VALUES(2,?1)",
        params![valid],
    ))?;
    let existing: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM mount_rs_block_authority WHERE id=1)",
            [],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    for invalid in ["short", "00000000000000000000000000000000"] {
        let result = if existing {
            tx.execute(
                "UPDATE mount_rs_block_authority SET backing_id=?1 WHERE id=1",
                params![invalid],
            )
        } else {
            tx.execute(
                "INSERT INTO mount_rs_block_authority(id,backing_id) VALUES(1,?1)",
                params![invalid],
            )
        };
        expect_authority_constraint(result)?;
    }
    let null_result = if existing {
        tx.execute(
            "UPDATE mount_rs_block_authority SET backing_id=NULL WHERE id=1",
            [],
        )
    } else {
        tx.execute(
            "INSERT INTO mount_rs_block_authority(id,backing_id) VALUES(1,NULL)",
            [],
        )
    };
    expect_authority_constraint(null_result)?;
    let mut statement = tx
        .prepare("SELECT id, backing_id, physical_dev, physical_ino, physical_path FROM mount_rs_block_authority")
        .map_err(backend_error)?;
    let mut rows = statement.query([]).map_err(backend_error)?;
    if let Some(row) = rows.next().map_err(backend_error)? {
        let id: i64 = row.get(0).map_err(backend_error)?;
        let backing: String = row.get(1).map_err(backend_error)?;
        let dev: Option<String> = row.get(2).map_err(backend_error)?;
        let ino: Option<String> = row.get(3).map_err(backend_error)?;
        let path: Option<String> = row.get(4).map_err(backend_error)?;
        if id != 1
            || ConcurrentBackingId::from_hex(&backing).is_err()
            || dev.is_some() != ino.is_some()
            || dev
                .as_deref()
                .is_some_and(|dev| dev.parse::<u64>().is_err())
            || ino
                .as_deref()
                .is_some_and(|ino| ino.parse::<u64>().is_err())
            || rows.next().map_err(backend_error)?.is_some()
        {
            return Err(incompatible_schema("SQLite block authority row is invalid"));
        }
        #[cfg(unix)]
        {
            let stored = FileStamp::from_text(dev.as_deref(), ino.as_deref())?
                .ok_or_else(|| incompatible_schema("SQLite block authority has no file stamp"))?;
            if stored != database.current_file_stamp()? {
                return Err(FsError::new(ErrorCode::Estale)
                    .with_syscall("inspect concurrent SQLite backing")
                    .with_message("SQLite block authority belongs to another physical file"));
            }
            database.require_concurrent_local_file("blocks")?;
            require_matching_auxiliary_path(database, path.as_deref())?;
        }
        #[cfg(not(unix))]
        let _ = path;
    }
    drop(rows);
    drop(statement);
    tx.commit().map_err(backend_error)
}

fn initialize_version_schema(database: &Database) -> Result<()> {
    let mut connection = database.lock()?;
    let tx = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(backend_error)?;

    let schema_columns = table_columns(&tx, SCHEMA_VERSION_TABLE)?;
    if let Some(columns) = schema_columns {
        require_columns(
            SCHEMA_VERSION_TABLE,
            &columns,
            &["schema_name", "schema_version"],
        )?;
    } else {
        tx.execute_batch(
            "CREATE TABLE mount_rs_schema_versions (
                 schema_name TEXT PRIMARY KEY NOT NULL,
                 schema_version INTEGER NOT NULL CHECK(schema_version>=0)
             )",
        )
        .map_err(backend_error)?;
    }
    require_primary_key(&tx, SCHEMA_VERSION_TABLE, "schema_name")?;

    let stored_version: Option<i64> = tx
        .query_row(
            "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=?1",
            params![VERSION_SCHEMA_NAME],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend_error)?;
    if stored_version.is_some_and(|version| !(0..=VERSION_SCHEMA_VERSION).contains(&version)) {
        return Err(incompatible_schema(
            "unsupported mount-rs versioning schema version",
        ));
    }
    let schema_is_current =
        stored_version.is_some_and(|version| version >= VERSION_SCHEMA_BASE_VERSION);

    let metadata_columns = table_columns(&tx, "mount_rs_metadata")?
        .ok_or_else(|| incompatible_schema("mount_rs_metadata table is missing"))?;
    require_columns(
        "mount_rs_metadata",
        &metadata_columns,
        &["id", "revision", "namespace", "owner", "fence", "expires"],
    )?;
    require_primary_key(&tx, "mount_rs_metadata", "id")?;
    if !metadata_columns.contains("delegation_state") {
        tx.execute(
            "ALTER TABLE mount_rs_metadata ADD COLUMN delegation_state TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    let has_volume_id = metadata_columns.contains("volume_id");
    let has_write_mode = metadata_columns.contains("write_mode");
    let has_backing_id = metadata_columns.contains("backing_id");
    let has_physical_dev = metadata_columns.contains("physical_dev");
    let has_physical_ino = metadata_columns.contains("physical_ino");
    let has_physical_path = metadata_columns.contains("physical_path");
    if schema_is_current && !has_volume_id {
        return Err(incompatible_schema(
            "current versioning schema is missing metadata.volume_id",
        ));
    }
    if !has_volume_id {
        tx.execute(
            "ALTER TABLE mount_rs_metadata ADD COLUMN volume_id TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    if !has_write_mode {
        tx.execute(
            "ALTER TABLE mount_rs_metadata ADD COLUMN write_mode TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    if !has_backing_id {
        tx.execute(
            "ALTER TABLE mount_rs_metadata ADD COLUMN backing_id TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    if !has_physical_dev {
        tx.execute(
            "ALTER TABLE mount_rs_metadata ADD COLUMN physical_dev TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    if !has_physical_ino {
        tx.execute(
            "ALTER TABLE mount_rs_metadata ADD COLUMN physical_ino TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    if !has_physical_path {
        tx.execute(
            "ALTER TABLE mount_rs_metadata ADD COLUMN physical_path TEXT",
            [],
        )
        .map_err(backend_error)?;
    }
    let (mode, backing, owner, fence, expires, physical_dev, physical_ino, physical_path): MetadataModeRow = tx
        .query_row(
            "SELECT write_mode, backing_id, owner, fence, expires, physical_dev, physical_ino, physical_path FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?)),
        )
        .map_err(backend_error)?;
    match mode.as_deref() {
        None if backing.is_none() && fence != CONCURRENT_FENCE_SENTINEL => {}
        Some(CONCURRENT_WRITE_MODE)
            if backing.is_none()
                && owner.is_none()
                && fence == CONCURRENT_FENCE_SENTINEL
                && expires == 0 => {}
        Some(BOUND_WRITE_MODE | DELEGATED_WRITE_MODE)
            if backing
                .as_deref()
                .is_some_and(|id| ConcurrentBackingId::from_hex(id).is_ok())
                && owner.is_none()
                && fence == CONCURRENT_FENCE_SENTINEL
                && expires == 0 => {}
        _ => {
            return Err(incompatible_schema(
                "concurrent write mode marker and legacy fence disagree",
            ));
        }
    }
    #[cfg(unix)]
    {
        let stamp = FileStamp::from_text(physical_dev.as_deref(), physical_ino.as_deref())?;
        if matches!(
            mode.as_deref(),
            Some(BOUND_WRITE_MODE | DELEGATED_WRITE_MODE)
        ) {
            require_matching_metadata_stamp(
                database,
                physical_dev.as_deref(),
                physical_ino.as_deref(),
                physical_path.as_deref(),
            )?;
        } else if stamp.is_none()
            && database
                .opened_file
                .as_ref()
                .is_some_and(|file| file.created_by_open)
        {
            // Only a file created by this open can be enrolled automatically.
            // Restamping a historical Legacy/MRC1 file would also enroll any
            // already copied sibling with the same volume ID.
            let actual = database.current_file_stamp()?;
            let path = database.current_auxiliary_path()?;
            tx.execute(
                "UPDATE mount_rs_metadata SET physical_dev=?1, physical_ino=?2, physical_path=?3
                 WHERE id=1 AND physical_dev IS NULL AND physical_ino IS NULL AND physical_path IS NULL",
                params![actual.dev.to_string(), actual.ino.to_string(), path],
            )
            .map_err(backend_error)?;
            database.current_file_stamp()?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (&physical_dev, &physical_ino, &physical_path);
        if matches!(
            mode.as_deref(),
            Some(BOUND_WRITE_MODE | DELEGATED_WRITE_MODE)
        ) {
            return Err(incompatible_schema(
                "this platform cannot bind MRC2 SQLite metadata to a physical file",
            ));
        }
    }
    let (delegation_json, namespace_json): (Option<String>, Option<String>) = tx
        .query_row(
            "SELECT delegation_state, namespace FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(backend_error)?;
    if mode.as_deref() == Some(DELEGATED_WRITE_MODE) {
        let state: DelegationState = serde_json::from_str(
            delegation_json
                .as_deref()
                .ok_or_else(|| incompatible_schema("MRC3 grant state is missing"))?,
        )
        .map_err(backend_error)?;
        let namespace: Namespace = serde_json::from_str(
            namespace_json
                .as_deref()
                .ok_or_else(|| incompatible_schema("MRC3 namespace is missing"))?,
        )
        .map_err(backend_error)?;
        if Some(state.backing.to_hex()).as_deref() != backing.as_deref() {
            return Err(incompatible_schema("MRC3 backing and grants disagree"));
        }
        state.validate(&namespace)?;
    } else if delegation_json.is_some() {
        return Err(incompatible_schema(
            "nondelegated SQLite metadata contains grants",
        ));
    }
    tx.execute(
        "UPDATE mount_rs_metadata
         SET volume_id = 'sqlite-' || lower(hex(randomblob(16)))
         WHERE id=1 AND (volume_id IS NULL OR volume_id='')",
        [],
    )
    .map_err(backend_error)?;
    let volume_text: String = tx
        .query_row(
            "SELECT volume_id FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    let volume = VolumeId::new(volume_text.clone())
        .map_err(|_| incompatible_schema("stored SQLite provider volume_id is invalid"))?;

    let version_tables = [
        (
            "mount_rs_version_state",
            [
                "id",
                "volume_id",
                "head_id",
                "next_sequence",
                "next_read_fence",
            ]
            .as_slice(),
        ),
        (
            "mount_rs_versions",
            [
                "id",
                "volume_id",
                "sequence",
                "parent_id",
                "restored_from",
                "forked_from",
                "namespace",
                "block_store_id",
                "kind",
                "created_at_ms",
                "durable",
                "operation_id",
            ]
            .as_slice(),
        ),
        (
            "mount_rs_version_pins",
            [
                "view_id",
                "volume_id",
                "version_id",
                "owner",
                "fence",
                "expires",
            ]
            .as_slice(),
        ),
    ];
    for (table, columns) in version_tables {
        if let Some(existing) = table_columns(&tx, table)? {
            require_columns(table, &existing, columns)?;
        } else if schema_is_current {
            return Err(incompatible_schema(format!(
                "current versioning schema is missing {table}"
            )));
        }
    }
    if !schema_is_current {
        tx.execute_batch(VERSION_SCHEMA).map_err(backend_error)?;
    }
    for (table, columns) in version_tables {
        let existing = table_columns(&tx, table)?
            .ok_or_else(|| incompatible_schema(format!("{table} table is missing")))?;
        require_columns(table, &existing, columns)?;
    }
    require_primary_key(&tx, "mount_rs_version_state", "id")?;
    require_primary_key(&tx, "mount_rs_versions", "id")?;
    require_unique_column(&tx, "mount_rs_versions", "operation_id")?;
    require_primary_key(&tx, "mount_rs_version_pins", "view_id")?;

    let state_volume: Option<String> = tx
        .query_row(
            "SELECT volume_id FROM mount_rs_version_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(backend_error)?;
    match state_volume {
        Some(state_volume) if state_volume != volume_text => {
            return Err(incompatible_schema(
                "version state belongs to another provider volume",
            ));
        }
        Some(_) => {}
        None => {
            tx.execute(
                "INSERT INTO mount_rs_version_state
                 (id, volume_id, head_id, next_sequence, next_read_fence)
                 VALUES (1, ?1, NULL, 1, 0)",
                params![volume_text],
            )
            .map_err(backend_error)?;
        }
    }
    let invalid_state: bool = tx
        .query_row(
            "SELECT next_sequence<=0 OR next_read_fence<0
             FROM mount_rs_version_state WHERE id=1",
            [],
            |row| row.get::<_, Option<bool>>(0),
        )
        .map_err(backend_error)?
        .unwrap_or(true);
    if invalid_state {
        return Err(incompatible_schema("version state counters are invalid"));
    }
    let invalid_versions: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM mount_rs_versions
             WHERE volume_id IS NULL OR volume_id<>?1)",
            params![volume.0],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    let invalid_pins: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM mount_rs_version_pins
             WHERE volume_id IS NULL OR volume_id<>?1)",
            params![volume.0],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    if invalid_versions || invalid_pins {
        return Err(incompatible_schema(
            "version records or pins cross provider volumes",
        ));
    }
    let persisted_head: Option<String> = tx
        .query_row(
            "SELECT head_id FROM mount_rs_version_state WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    if let Some(head_id) = persisted_head {
        let head = VersionId::decode(&head_id)
            .map_err(|_| incompatible_schema("stored version head is malformed"))?;
        if head.volume != volume {
            return Err(incompatible_schema(
                "stored version head belongs to another provider volume",
            ));
        }
        if !stored_version_is_loadable(&tx, &head)? {
            return Err(incompatible_schema(
                "stored version head references an unreadable version",
            ));
        }
    }

    let schema_version = if stored_version == Some(VERSION_SCHEMA_VERSION) {
        VERSION_SCHEMA_VERSION
    } else {
        VERSION_SCHEMA_BASE_VERSION
    };
    tx.execute(
        "INSERT INTO mount_rs_schema_versions(schema_name, schema_version)
         VALUES (?1, ?2)
         ON CONFLICT(schema_name) DO UPDATE SET schema_version=excluded.schema_version",
        params![VERSION_SCHEMA_NAME, schema_version],
    )
    .map_err(backend_error)?;
    tx.commit().map_err(backend_error)
}

fn table_columns(connection: &Connection, table: &str) -> Result<Option<BTreeSet<String>>> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            params![table],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    if !exists {
        return Ok(None);
    }
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(backend_error)?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(backend_error)?;
    let mut columns = BTreeSet::new();
    for row in rows {
        columns.insert(row.map_err(backend_error)?);
    }
    Ok(Some(columns))
}

fn require_primary_key(connection: &Connection, table: &str, column: &str) -> Result<()> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(backend_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, i64>(5)? > 0))
        })
        .map_err(backend_error)?;
    for row in rows {
        let (name, primary_key) = row.map_err(backend_error)?;
        if name == column && primary_key {
            return Ok(());
        }
    }
    Err(incompatible_schema(format!(
        "{table}.{column} must be a primary key"
    )))
}

fn require_unique_column(connection: &Connection, table: &str, column: &str) -> Result<()> {
    let indexes = {
        let mut statement = connection
            .prepare(&format!("PRAGMA index_list(\"{table}\")"))
            .map_err(backend_error)?;
        let rows = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, i64>(2)? != 0))
            })
            .map_err(backend_error)?;
        let mut indexes = Vec::new();
        for row in rows {
            indexes.push(row.map_err(backend_error)?);
        }
        indexes
    };

    for (index, unique) in indexes {
        if !unique {
            continue;
        }
        let pragma_index = index.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("PRAGMA index_info(\"{pragma_index}\")"))
            .map_err(backend_error)?;
        let rows = statement
            .query_map([], |row| row.get::<_, Option<String>>(2))
            .map_err(backend_error)?;
        let mut columns = Vec::new();
        for row in rows {
            if let Some(name) = row.map_err(backend_error)? {
                columns.push(name);
            }
        }
        if columns.len() == 1 && columns[0] == column {
            return Ok(());
        }
    }

    Err(incompatible_schema(format!(
        "{table}.{column} must have a unique constraint"
    )))
}

fn require_columns(table: &str, actual: &BTreeSet<String>, expected: &[&str]) -> Result<()> {
    let missing = expected
        .iter()
        .filter(|column| !actual.contains(**column))
        .copied()
        .collect::<Vec<_>>();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(incompatible_schema(format!(
            "{table} is missing required columns: {}",
            missing.join(", ")
        )))
    }
}

fn incompatible_schema(message: impl Into<String>) -> FsError {
    FsError::new(ErrorCode::Enotsup)
        .with_syscall("sqlite schema")
        .with_message(message.into())
}

fn load_volume_id(database: &Database) -> Result<VolumeId> {
    let value: String = database
        .lock()?
        .query_row(
            "SELECT volume_id FROM mount_rs_metadata WHERE id=1",
            [],
            |row| row.get(0),
        )
        .map_err(backend_error)?;
    VolumeId::new(value)
}

/// Fenced single-writer metadata transactions; no file bytes are stored here.
#[derive(Clone)]
pub struct SqliteMetadataStore(Database, VolumeId);

impl SqliteMetadataStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let database = Database::open(Some(path.as_ref()), METADATA_SCHEMA)?;
        let store = Self::from_database(database)?;
        Ok(store)
    }
    pub fn in_memory() -> Result<Self> {
        Self::from_database(Database::open(None, METADATA_SCHEMA)?)
    }

    fn from_database(database: Database) -> Result<Self> {
        initialize_version_schema(&database)?;
        let volume_id = load_volume_id(&database)?;
        Ok(Self(database, volume_id))
    }
}

/// Immutable blocks, independently configurable from the metadata database.
#[derive(Clone)]
pub struct SqliteBlockStore(Database);

impl SqliteBlockStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::from_database(Database::open(Some(path.as_ref()), BLOCK_SCHEMA)?)
    }
    pub fn in_memory() -> Result<Self> {
        Self::from_database(Database::open(None, BLOCK_SCHEMA)?)
    }

    fn from_database(database: Database) -> Result<Self> {
        initialize_block_schema(&database)?;
        Ok(Self(database))
    }

    fn put_once(&self, bytes: &[u8]) -> Result<BlockId> {
        let mut connection = self.0.lock()?;
        // Database-generated random identities avoid accidental aliasing when
        // namespaces use distinct block databases. Collisions fail, never overwrite.
        let id: String = connection
            .query_row("SELECT lower(hex(randomblob(32)))", [], |row| row.get(0))
            .map_err(backend_error)?;
        let was_autocommit = connection.is_autocommit();
        let transaction = match connection.transaction_with_behavior(TransactionBehavior::Immediate)
        {
            Ok(transaction) => transaction,
            Err(error) => {
                return Err(
                    sqlite_busy_known_noncommit(&error, was_autocommit, "put block")
                        .unwrap_or_else(|| backend_error(error)),
                );
            }
        };
        let inserted = transaction
            .execute(
                "INSERT INTO mount_rs_blocks(id,bytes) VALUES(?1,?2)",
                params![id, bytes],
            )
            .map_err(backend_error)?;
        if inserted != 1 {
            return Err(FsError::new(ErrorCode::Eio)
                .with_syscall("put block")
                .with_message("SQLite block insert was not applied"));
        }
        if let Err(error) = transaction.commit() {
            return Err(sqlite_busy_known_noncommit(
                &error,
                connection.is_autocommit(),
                "put block",
            )
            .unwrap_or_else(|| backend_error(error)));
        }
        Ok(BlockId(id))
    }
}

fn ttl_ms(ttl: Duration) -> Result<i64> {
    let value = i64::try_from(ttl.as_millis()).map_err(|_| FsError::new(ErrorCode::Einval))?;
    if value == 0 {
        return Err(FsError::new(ErrorCode::Einval));
    }
    Ok(value)
}

fn lease_numbers(lease: &WriterLease) -> Result<(i64, i64)> {
    Ok((
        i64::try_from(lease.fence).map_err(|_| stale())?,
        i64::try_from(lease.expires_at_ms).map_err(|_| stale())?,
    ))
}
fn stale() -> FsError {
    FsError::new(ErrorCode::Estale).with_syscall("metadata lease")
}

struct PinRenewalIdentity {
    volume_matches: bool,
    version_matches: bool,
    request_owner_matches: bool,
    lease_owner_matches: bool,
}

struct PinRenewalToken {
    fence: i64,
    expires_at_ms: i64,
}

fn plan_view_pin_renewal(
    identity: PinRenewalIdentity,
    current: PinRenewalToken,
    lease: PinRenewalToken,
    now_ms: i64,
    ttl_ms: i64,
) -> Result<i64> {
    if !identity.volume_matches
        || !identity.version_matches
        || !identity.request_owner_matches
        || !identity.lease_owner_matches
        || current.fence != lease.fence
        || current.expires_at_ms != lease.expires_at_ms
        || current.expires_at_ms <= now_ms
    {
        return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
    }
    now_ms
        .checked_add(ttl_ms)
        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))
}

#[cfg(unix)]
fn mrc1_migration_required() -> FsError {
    FsError::new(ErrorCode::Ebusy)
        .with_syscall("bound concurrent SQLite metadata")
        .with_message("MRC1 requires migrate-concurrent-backing")
}

fn version_kind_name(kind: &VersionKind) -> &'static str {
    match kind {
        VersionKind::Initial => "initial",
        VersionKind::Snapshot => "snapshot",
        VersionKind::NamespacePublication => "namespace-publication",
        VersionKind::Restore => "restore",
        VersionKind::Fork => "fork",
    }
}

fn parse_version_kind(value: &str) -> Result<VersionKind> {
    match value {
        "initial" => Ok(VersionKind::Initial),
        "snapshot" => Ok(VersionKind::Snapshot),
        "namespace-publication" => Ok(VersionKind::NamespacePublication),
        "restore" => Ok(VersionKind::Restore),
        "fork" => Ok(VersionKind::Fork),
        _ => Err(FsError::new(ErrorCode::Enotsup)
            .with_syscall("load version")
            .with_message(format!("unknown version kind '{value}'"))),
    }
}

#[derive(Debug)]
struct RawVersion {
    id: String,
    parent_id: Option<String>,
    restored_from: Option<String>,
    forked_from: Option<String>,
    namespace: String,
    block_store_id: String,
    kind: String,
    created_at_ms: i64,
    durable: bool,
    volume_id: String,
    sequence: u64,
}

fn raw_version(row: &Row<'_>) -> rusqlite::Result<RawVersion> {
    Ok(RawVersion {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        restored_from: row.get(2)?,
        forked_from: row.get(3)?,
        namespace: row.get(4)?,
        block_store_id: row.get(5)?,
        kind: row.get(6)?,
        created_at_ms: row.get(7)?,
        durable: row.get::<_, i64>(8)? != 0,
        volume_id: row.get(9)?,
        sequence: row.get(10)?,
    })
}

fn decode_raw_version(raw: RawVersion) -> Result<VersionInfo> {
    let volume = VolumeId::new(raw.volume_id)?;
    let id = VersionId::new(volume.clone(), raw.sequence)?;
    if raw.id != id.encode() {
        return Err(FsError::new(ErrorCode::Einval)
            .with_syscall("load version")
            .with_message("stored version id does not match its volume and sequence"));
    }
    let parent = raw
        .parent_id
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let restored_from = raw
        .restored_from
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let forked_from = raw
        .forked_from
        .as_deref()
        .map(VersionId::decode)
        .transpose()?;
    let namespace = serde_json::from_str(&raw.namespace).map_err(backend_error)?;
    let block_store_id = mount_rs_core::versioning::BlockStoreId::new(raw.block_store_id)?;
    let info = VersionInfo {
        id,
        parent,
        restored_from,
        forked_from,
        kind: parse_version_kind(&raw.kind)?,
        namespace,
        block_store_id,
        created_at_ms: raw.created_at_ms,
        durable: raw.durable,
    };
    info.validate()?;
    Ok(info)
}

const VERSION_SELECT: &str = "SELECT id, parent_id, restored_from, forked_from,
 namespace, block_store_id, kind, created_at_ms, durable, volume_id, sequence
 FROM mount_rs_versions";

fn raw_version_from_connection(
    connection: &Connection,
    id: &VersionId,
) -> Result<Option<RawVersion>> {
    connection
        .query_row(
            &format!("{VERSION_SELECT} WHERE id=?1"),
            params![id.encode()],
            raw_version,
        )
        .optional()
        .map_err(backend_error)
}

fn decode_version_for_id(raw: RawVersion, id: &VersionId) -> Result<VersionInfo> {
    let info = decode_raw_version(raw)?;
    info.validate_for_volume(&id.volume)?;
    if info.id != *id {
        return Err(FsError::new(ErrorCode::Enoent).with_syscall("load version"));
    }
    Ok(info)
}

fn load_version_from_connection(connection: &Connection, id: &VersionId) -> Result<VersionInfo> {
    let raw = raw_version_from_connection(connection, id)?
        .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("load version"))?;
    decode_version_for_id(raw, id)
}

fn stored_version_is_loadable(connection: &Connection, id: &VersionId) -> Result<bool> {
    let Some(raw) = raw_version_from_connection(connection, id)? else {
        return Ok(false);
    };
    Ok(decode_version_for_id(raw, id).is_ok())
}

#[cfg(unix)]
struct DelegatedRow {
    revision: u64,
    namespace: Namespace,
    state: DelegationState,
}

#[cfg(unix)]
fn load_delegated(
    database: &Database,
    connection: &Connection,
    backing: Option<ConcurrentBackingId>,
) -> Result<DelegatedRow> {
    let (mode, stored, owner, fence, expires, revision, dev, ino, path): MetadataPublicationRow = connection.query_row(
        "SELECT write_mode, backing_id, owner, fence, expires, revision, physical_dev, physical_ino, physical_path FROM mount_rs_metadata WHERE id=1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
    ).map_err(backend_error)?;
    if mode.as_deref() != Some(DELEGATED_WRITE_MODE)
        || backing.is_some_and(|id| Some(id.to_hex()).as_deref() != stored.as_deref())
    {
        return Err(stale());
    }
    if owner.is_some() || fence != CONCURRENT_FENCE_SENTINEL || expires != 0 {
        return Err(incompatible_schema("MRC3 legacy fence is invalid"));
    }
    require_matching_metadata_stamp(database, dev.as_deref(), ino.as_deref(), path.as_deref())?;
    let (namespace, state): (String, String) = connection
        .query_row(
            "SELECT namespace, delegation_state FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(backend_error)?;
    let namespace: Namespace = serde_json::from_str(&namespace).map_err(backend_error)?;
    let state: DelegationState = serde_json::from_str(&state).map_err(backend_error)?;
    if Some(state.backing.to_hex()).as_deref() != stored.as_deref() {
        return Err(stale());
    }
    state.validate(&namespace)?;
    Ok(DelegatedRow {
        revision: u64::try_from(revision).map_err(backend_error)?,
        namespace,
        state,
    })
}

#[cfg(unix)]
impl SqliteMetadataStore {
    fn mutate_delegation<T>(
        &self,
        backing: ConcurrentBackingId,
        expected: Option<u64>,
        mutation: impl FnOnce(&mut DelegatedRow) -> Result<T>,
    ) -> Result<T> {
        self.0.with_concurrent_publish_timeout(|connection| {
            let was_autocommit = connection.is_autocommit();
            let tx = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
                Ok(tx) => tx,
                Err(error) => return Err(sqlite_busy_known_noncommit(&error, was_autocommit, "mutate delegated SQLite metadata").unwrap_or_else(|| backend_error(error))),
            };
            let mut row = load_delegated(&self.0, &tx, Some(backing))?;
            if expected.is_some_and(|expected| row.revision != expected) { return Err(FsError::new(ErrorCode::Eagain)); }
            let previous = (row.revision, serde_json::to_string(&row.namespace).map_err(backend_error)?, serde_json::to_string(&row.state).map_err(backend_error)?);
            let result = mutation(&mut row)?;
            row.state.validate(&row.namespace)?;
            let revision = i64::try_from(row.revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
            let namespace_json = serde_json::to_string(&row.namespace).map_err(backend_error)?;
            let state_json = serde_json::to_string(&row.state).map_err(backend_error)?;
            if previous != (row.revision, namespace_json.clone(), state_json.clone()) {
                let changed = tx.execute("UPDATE mount_rs_metadata SET revision=?1, namespace=?2, delegation_state=?3 WHERE id=1 AND write_mode='MRC3'", params![revision, namespace_json, state_json]).map_err(backend_error)?;
                if changed != 1 { return Err(backend_error("SQLite delegation update returned zero rows")); }
            }
            self.0.current_file_stamp()?;
            if let Err(error) = tx.commit() {
                return Err(sqlite_busy_known_noncommit(&error, connection.is_autocommit(), "mutate delegated SQLite metadata").unwrap_or_else(|| backend_error(error)));
            }
            self.0.current_file_stamp()?;
            Ok(result)
        })
    }
}

#[async_trait]
impl MetadataStore for SqliteMetadataStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    fn publish_includes_flush_barrier(&self) -> bool {
        // SQLite is opened with synchronous=FULL and publish commits an
        // autocommit transaction before returning.
        true
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        let connection = self.0.lock()?;
        let (revision, namespace): (u64, Option<String>) = connection
            .query_row(
                "SELECT revision, namespace FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(backend_error)?;
        Ok(LoadedMetadata {
            revision,
            namespace: namespace
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(backend_error)?,
        })
    }

    async fn delegation_state(&self) -> Result<Option<DelegationState>> {
        #[cfg(not(unix))]
        {
            Err(FsError::new(ErrorCode::Enotsup))
        }
        #[cfg(unix)]
        {
            if !self.0.durable {
                return Err(FsError::new(ErrorCode::Enotsup));
            }
            let connection = self.0.lock()?;
            let mode: Option<String> = connection
                .query_row(
                    "SELECT write_mode FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| row.get(0),
                )
                .map_err(backend_error)?;
            if mode.as_deref() != Some(DELEGATED_WRITE_MODE) {
                return Ok(None);
            }
            Ok(Some(load_delegated(&self.0, &connection, None)?.state))
        }
    }

    async fn prepare_delegated_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        #[cfg(not(unix))]
        {
            let _ = (backing, expected_revision);
            Err(FsError::new(ErrorCode::Enotsup))
        }
        #[cfg(unix)]
        {
            if !self.0.durable {
                return Err(FsError::new(ErrorCode::Enotsup));
            }
            let mut connection = self.0.lock()?;
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(backend_error)?;
            let (mode, stored, revision, namespace, owner, fence, expires, dev, ino, path): MetadataClaimRow = tx.query_row(
                "SELECT write_mode, backing_id, revision, namespace, owner, fence, expires, physical_dev, physical_ino, physical_path FROM mount_rs_metadata WHERE id=1", [],
                |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?)),
            ).map_err(backend_error)?;
            if mode.as_deref() == Some(DELEGATED_WRITE_MODE) {
                let current = load_delegated(&self.0, &tx, Some(backing))?;
                return if current.revision == expected_revision {
                    Ok(())
                } else {
                    Err(FsError::new(ErrorCode::Eagain))
                };
            }
            if owner.is_some() || expires != 0 {
                return Err(FsError::new(ErrorCode::Ebusy));
            }
            match mode.as_deref() {
                None if stored.is_none() && fence != CONCURRENT_FENCE_SENTINEL => {}
                Some(BOUND_WRITE_MODE)
                    if stored.as_deref() == Some(backing.to_hex().as_str())
                        && fence == CONCURRENT_FENCE_SENTINEL => {}
                _ => return Err(FsError::new(ErrorCode::Ebusy)),
            }
            if u64::try_from(revision).map_err(backend_error)? != expected_revision {
                return Err(FsError::new(ErrorCode::Eagain));
            }
            require_matching_metadata_stamp(
                &self.0,
                dev.as_deref(),
                ino.as_deref(),
                path.as_deref(),
            )?;
            let namespace: Namespace =
                serde_json::from_str(namespace.as_deref().ok_or_else(|| {
                    FsError::new(ErrorCode::Ebusy)
                        .with_message("initialize namespace before offline MRC3 enrollment")
                })?)
                .map_err(backend_error)?;
            namespace.validate()?;
            if namespace.nodes.values().any(|node| node.stats.nlink == 0) {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_message("unowned orphan nodes prevent MRC3 enrollment"));
            }
            let (head, versions, pins): (Option<String>, i64, i64) = tx.query_row("SELECT head_id, (SELECT count(*) FROM mount_rs_versions), (SELECT count(*) FROM mount_rs_version_pins) FROM mount_rs_version_state WHERE id=1", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).map_err(backend_error)?;
            if head.is_some() || versions != 0 || pins != 0 {
                return Err(FsError::new(ErrorCode::Ebusy));
            }
            let state = DelegationState::new(backing);
            state.validate(&namespace)?;
            let changed = tx.execute("UPDATE mount_rs_metadata SET write_mode='MRC3', backing_id=?1, fence=?2, delegation_state=?3 WHERE id=1", params![backing.to_hex(), CONCURRENT_FENCE_SENTINEL, serde_json::to_string(&state).map_err(backend_error)?]).map_err(backend_error)?;
            if changed != 1 {
                return Err(backend_error(
                    "SQLite delegation enrollment returned zero rows",
                ));
            }
            self.0.current_file_stamp()?;
            tx.commit().map_err(backend_error)?;
            self.0.current_file_stamp()?;
            Ok(())
        }
    }

    async fn checkout(&self, request: &CheckoutRequest) -> Result<DirectoryGrant> {
        #[cfg(not(unix))]
        {
            let _ = request;
            Err(FsError::new(ErrorCode::Enotsup))
        }
        #[cfg(unix)]
        {
            self.mutate_delegation(request.backing, None, |row| {
                row.state
                    .checkout(&row.namespace, request.root, &request.owner)
            })
        }
    }

    async fn publish_delegated(
        &self,
        publication: &DelegatedPublish,
        namespace: Namespace,
    ) -> Result<u64> {
        #[cfg(not(unix))]
        {
            let _ = (publication, namespace);
            Err(FsError::new(ErrorCode::Enotsup))
        }
        #[cfg(unix)]
        {
            self.mutate_delegation(
                publication.backing,
                Some(publication.expected_revision),
                |row| {
                    row.state
                        .authorize_publish(&row.namespace, &namespace, &publication.token)?;
                    row.revision = row
                        .revision
                        .checked_add(1)
                        .filter(|revision| *revision <= i64::MAX as u64)
                        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                    row.namespace = namespace;
                    Ok(row.revision)
                },
            )
        }
    }

    async fn checkin(&self, release: &DelegatedCheckin) -> Result<()> {
        #[cfg(not(unix))]
        {
            let _ = release;
            Err(FsError::new(ErrorCode::Enotsup))
        }
        #[cfg(unix)]
        {
            self.mutate_delegation(release.backing, None, |row| {
                if row.state.retired.contains(&release.token) {
                    return Ok(());
                }
                if row.revision != release.expected_revision {
                    return Err(FsError::new(ErrorCode::Eagain));
                }
                row.state.checkin(&release.token, &row.namespace)
            })
        }
    }

    async fn recover(&self, recovery: &DelegatedRecovery) -> Result<()> {
        #[cfg(not(unix))]
        {
            let _ = recovery;
            Err(FsError::new(ErrorCode::Enotsup))
        }
        #[cfg(unix)]
        {
            self.mutate_delegation(recovery.backing, None, |row| {
                let old_namespace = serde_json::to_string(&row.namespace).map_err(backend_error)?;
                row.state
                    .recover(recovery.root, recovery.expected_fence, &mut row.namespace)?;
                if serde_json::to_string(&row.namespace).map_err(backend_error)? != old_namespace {
                    row.revision = row
                        .revision
                        .checked_add(1)
                        .filter(|revision| *revision <= i64::MAX as u64)
                        .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                }
                Ok(())
            })
        }
    }

    async fn concurrent_mode_state(&self) -> Result<ConcurrentModeState> {
        let connection = self.0.lock()?;
        let (mode, backing, physical_dev, physical_ino, physical_path):
            (Option<String>, Option<String>, Option<String>, Option<String>, Option<String>) = connection
            .query_row(
                "SELECT write_mode, backing_id, physical_dev, physical_ino, physical_path FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .map_err(backend_error)?;
        match (mode.as_deref(), backing) {
            (None, None) => Ok(ConcurrentModeState::Legacy),
            (Some(CONCURRENT_WRITE_MODE), None) => Ok(ConcurrentModeState::Mrc1),
            (Some(BOUND_WRITE_MODE), Some(id)) => {
                #[cfg(unix)]
                {
                    require_matching_metadata_stamp(
                        &self.0,
                        physical_dev.as_deref(),
                        physical_ino.as_deref(),
                        physical_path.as_deref(),
                    )?;
                    ConcurrentBackingId::from_hex(&id)
                        .map(ConcurrentModeState::Mrc2)
                        .map_err(|_| incompatible_schema("invalid bound SQLite backing ID"))
                }
                #[cfg(not(unix))]
                {
                    let _ = (id, physical_dev, physical_ino, physical_path);
                    Err(incompatible_schema(
                        "MRC2 SQLite metadata requires a Unix file stamp",
                    ))
                }
            }
            _ => Err(incompatible_schema(
                "invalid SQLite concurrent mode and backing ID",
            )),
        }
    }

    async fn preflight_new_bound_mode(&self) -> Result<()> {
        if !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("preflight new bound SQLite metadata")
                .with_message("concurrent SQLite metadata requires a file-backed database"));
        }
        #[cfg(not(unix))]
        {
            Err(incompatible_schema(
                "MRC2 SQLite metadata requires a Unix file stamp",
            ))
        }
        #[cfg(unix)]
        {
            let connection = self.0.lock()?;
            let (mode, backing, revision, namespace, owner, fence, expires, dev, ino, path):
                MetadataClaimRow = connection
                .query_row(
                    "SELECT write_mode, backing_id, revision, namespace, owner, fence, expires,
                            physical_dev, physical_ino, physical_path FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                        ))
                    },
                )
                .map_err(backend_error)?;
            if mode.is_some() || backing.is_some() {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("preflight new bound SQLite metadata"));
            }
            require_matching_metadata_stamp(
                &self.0,
                dev.as_deref(),
                ino.as_deref(),
                path.as_deref(),
            )?;
            let (head, versions, pins): (Option<String>, i64, i64) = connection
                .query_row(
                    "SELECT head_id, (SELECT count(*) FROM mount_rs_versions),
                        (SELECT count(*) FROM mount_rs_version_pins)
                 FROM mount_rs_version_state WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(backend_error)?;
            if revision != 0
                || namespace.is_some()
                || owner.is_some()
                || fence != 0
                || expires != 0
                || head.is_some()
                || versions != 0
                || pins != 0
            {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("preflight new bound SQLite metadata")
                    .with_message(
                        "a populated or fenced Legacy volume needs an offline migration",
                    ));
            }
            self.0.current_file_stamp()?;
            Ok(())
        }
    }

    async fn prepare_bound_concurrent_mode(&self, backing: ConcurrentBackingId) -> Result<()> {
        if !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("prepare bound concurrent SQLite volume")
                .with_message(
                    "independent concurrent writers require a file-backed SQLite database",
                ));
        }
        #[cfg(not(unix))]
        {
            let _ = backing;
            Err(incompatible_schema(
                "MRC2 SQLite metadata requires a Unix file stamp",
            ))
        }
        #[cfg(unix)]
        {
            let mut connection = self.0.lock()?;
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(backend_error)?;
            let (mode, stored, revision, namespace, owner, fence, expires, physical_dev, physical_ino, physical_path):
            MetadataClaimRow = tx
            .query_row("SELECT write_mode, backing_id, revision, namespace, owner, fence, expires, physical_dev, physical_ino, physical_path FROM mount_rs_metadata WHERE id=1", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?, row.get(9)?))
            }).map_err(backend_error)?;
            if mode.as_deref() == Some(BOUND_WRITE_MODE) {
                if stored.as_deref() != Some(backing.to_hex().as_str()) {
                    return Err(stale());
                }
                require_matching_metadata_stamp(
                    &self.0,
                    physical_dev.as_deref(),
                    physical_ino.as_deref(),
                    physical_path.as_deref(),
                )?;
                if owner.is_none() && fence == CONCURRENT_FENCE_SENTINEL && expires == 0 {
                    return Ok(());
                }
                return Err(incompatible_schema(
                    "bound concurrent marker and fence disagree",
                ));
            }
            if mode.as_deref() == Some(CONCURRENT_WRITE_MODE) {
                return Err(mrc1_migration_required());
            }
            if mode.is_some() || stored.is_some() {
                return Err(incompatible_schema(
                    "unsupported SQLite concurrent write mode",
                ));
            }
            let physical = require_matching_metadata_stamp(
                &self.0,
                physical_dev.as_deref(),
                physical_ino.as_deref(),
                physical_path.as_deref(),
            )?;
            let (head, versions, pins): (Option<String>, i64, i64) = tx.query_row(
            "SELECT head_id, (SELECT count(*) FROM mount_rs_versions), (SELECT count(*) FROM mount_rs_version_pins) FROM mount_rs_version_state WHERE id=1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).map_err(backend_error)?;
            if revision != 0
                || namespace.is_some()
                || owner.is_some()
                || fence != 0
                || expires != 0
                || head.is_some()
                || versions != 0
                || pins != 0
            {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("prepare bound concurrent SQLite volume")
                    .with_message("a fenced legacy volume needs an offline migration"));
            }
            let changed = tx.execute(
            "UPDATE mount_rs_metadata SET write_mode='MRC2', backing_id=?1, fence=?2
             WHERE id=1 AND write_mode IS NULL AND backing_id IS NULL AND revision=0 AND namespace IS NULL
               AND owner IS NULL AND fence=0 AND expires=0 AND physical_dev=?3 AND physical_ino=?4 AND physical_path=?5",
            params![backing.to_hex(), CONCURRENT_FENCE_SENTINEL, physical.dev.to_string(), physical.ino.to_string(), physical_path],
        ).map_err(backend_error)?;
            if changed != 1 {
                return Err(backend_error("SQLite bound mode update returned zero rows"));
            }
            self.0.current_file_stamp()?;
            tx.commit().map_err(backend_error)?;
            self.0.current_file_stamp()?;
            Ok(())
        }
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        if owner.is_empty() {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let ttl = ttl_ms(ttl)?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET owner=?1, fence=fence+1, expires={NOW}+?2
            WHERE id=1 AND write_mode IS NULL AND (owner IS NULL OR expires<={NOW})
              AND fence<9223372036854775807 AND ?2<=9223372036854775807-{NOW}"
        );
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let changed = tx
            .execute(&sql, params![owner, ttl])
            .map_err(backend_error)?;
        if changed > 1 {
            return Err(backend_error(
                "SQLite writer acquisition changed multiple rows",
            ));
        }
        let lease = if changed == 1 {
            let (fence, expires_at_ms): (u64, u64) = tx
                .query_row(
                    "SELECT fence, expires FROM mount_rs_metadata WHERE id=1 AND owner=?1",
                    params![owner],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(backend_error)?;
            Some(WriterLease {
                owner: owner.to_owned(),
                fence,
                expires_at_ms,
            })
        } else {
            None
        };
        let mode: Option<String> = if lease.is_none() {
            tx.query_row(
                "SELECT write_mode FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?
        } else {
            None
        };
        tx.commit().map_err(backend_error)?;
        if let Some(lease) = lease {
            return Ok(lease);
        }
        if mode.is_some() {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("acquire writer")
                .with_message("SQLite volume is in concurrent write mode"));
        }
        Err(FsError::new(ErrorCode::Eagain).with_syscall("acquire writer"))
    }

    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        let ttl = ttl_ms(ttl)?;
        let (fence, expires) = lease_numbers(lease)?;
        let mut connection = self.0.lock()?;
        let sql = format!(
            "UPDATE mount_rs_metadata SET expires={NOW}+?4
            WHERE id=1 AND write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}
              AND ?4<=9223372036854775807-{NOW}"
        );
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let changed = tx
            .execute(&sql, params![lease.owner, fence, expires, ttl])
            .map_err(backend_error)?;
        if changed > 1 {
            return Err(backend_error("SQLite writer renewal changed multiple rows"));
        }
        let expiry: Option<u64> = if changed == 1 {
            Some(
                tx.query_row(
                    "SELECT expires FROM mount_rs_metadata WHERE id=1 AND owner=?1 AND fence=?2",
                    params![lease.owner, fence],
                    |row| row.get(0),
                )
                .map_err(backend_error)?,
            )
        } else {
            None
        };
        tx.commit().map_err(backend_error)?;
        let expiry = expiry.ok_or_else(stale)?;
        Ok(WriterLease {
            expires_at_ms: expiry,
            ..lease.clone()
        })
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        let (fence, expires) = lease_numbers(lease)?;
        let connection = self.0.lock()?;
        let changed = connection
            .execute(
                &format!(
                    "UPDATE mount_rs_metadata SET owner=NULL, expires=0
            WHERE id=1 AND write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}"
                ),
                params![lease.owner, fence, expires],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            return Err(stale());
        }
        Ok(())
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        let expected =
            i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let next = expected
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let (fence, expires) = lease_numbers(lease)?;
        let namespace = serde_json::to_string(&namespace).map_err(backend_error)?;
        let mut connection = self.0.lock()?;
        // The successful publication path is one fenced conditional UPDATE.
        // SQLite makes a single autocommit statement atomic and durable under
        // the configured synchronous policy, so avoid opening and committing
        // a second explicit transaction for the common case. A zero-row
        // result opens the Immediate transaction only for the locked
        // stale/revision classification below.
        let changed = connection
            .execute(
                &format!(
                    "UPDATE mount_rs_metadata SET revision=?1, namespace=?2
                     WHERE id=1 AND write_mode IS NULL AND revision=?3 AND owner=?4 AND fence=?5
                       AND expires=?6 AND expires>{NOW}"
                ),
                params![next, namespace, expected, lease.owner, fence, expires],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            // The conditional update is the successful-path CAS. Only the
            // exceptional path needs a read to preserve the stale-versus-
            // revision-conflict classification of the former lease preflight.
            let tx = connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(backend_error)?;
            let (valid, actual_revision): (bool, i64) = tx
                .query_row(
                    &format!(
                        "SELECT write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}, revision
                         FROM mount_rs_metadata WHERE id=1"
                    ),
                    params![lease.owner, fence, expires],
                    |row| Ok((row.get::<_, Option<bool>>(0)?.unwrap_or(false), row.get(1)?)),
                )
                .map_err(backend_error)?;
            if !valid {
                return Err(stale());
            }
            if actual_revision != expected {
                return Err(FsError::new(ErrorCode::Eagain).with_syscall("publish metadata"));
            }
            // The row was still valid and at the expected revision, so an
            // unexplained zero-row CAS fails closed.
            return Err(stale());
        }
        Ok(next as u64)
    }

    async fn publish_bound_if_revision(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        #[cfg(not(unix))]
        {
            let _ = (backing, expected_revision, namespace);
            Err(incompatible_schema(
                "MRC2 SQLite metadata requires a Unix file stamp",
            ))
        }
        #[cfg(unix)]
        {
            namespace.validate()?;
            let expected =
                i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
            let next = expected
                .checked_add(1)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
            let namespace_json = serde_json::to_string(&namespace).map_err(backend_error)?;
            let backing_text = backing.to_hex();
            self.0.with_concurrent_publish_timeout(|connection| {
            let was_autocommit = connection.is_autocommit();
            let tx = match connection.transaction_with_behavior(TransactionBehavior::Immediate) {
                Ok(tx) => tx,
                Err(error) => return Err(sqlite_busy_known_noncommit(
                    &error, was_autocommit, "publish bound concurrent SQLite metadata"
                ).unwrap_or_else(|| backend_error(error))),
            };
            let (mode, stored, owner, fence, expires, actual_revision, physical_dev, physical_ino, physical_path):
                MetadataPublicationRow = tx.query_row(
                    "SELECT write_mode, backing_id, owner, fence, expires, revision, physical_dev, physical_ino, physical_path FROM mount_rs_metadata WHERE id=1",
                    [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?))
                ).map_err(backend_error)?;
            if mode.as_deref() == Some(CONCURRENT_WRITE_MODE) { return Err(mrc1_migration_required()); }
            if mode.as_deref() != Some(BOUND_WRITE_MODE) || stored.as_deref() != Some(&backing_text) {
                return Err(stale());
            }
            let physical = require_matching_metadata_stamp(
                &self.0,
                physical_dev.as_deref(),
                physical_ino.as_deref(),
                physical_path.as_deref(),
            )?;
            if owner.is_some() || fence != CONCURRENT_FENCE_SENTINEL || expires != 0 {
                return Err(incompatible_schema("bound concurrent mode fence is invalid"));
            }
            if actual_revision != expected {
                return Err(FsError::new(ErrorCode::Eagain).with_syscall("publish bound concurrent SQLite metadata"));
            }
            let changed = tx.execute(
                "UPDATE mount_rs_metadata SET revision=?1, namespace=?2
                 WHERE id=1 AND write_mode='MRC2' AND backing_id=?3 AND owner IS NULL
                   AND fence=?4 AND expires=0 AND revision=?5
                   AND physical_dev=?6 AND physical_ino=?7 AND physical_path=?8",
                params![next, namespace_json, backing_text, CONCURRENT_FENCE_SENTINEL, expected, physical.dev.to_string(), physical.ino.to_string(), physical_path],
            ).map_err(backend_error)?;
            if changed != 1 { return Err(stale()); }
            self.0.current_file_stamp()?;
            if let Err(error) = tx.commit() {
                return Err(sqlite_busy_known_noncommit(
                    &error, connection.is_autocommit(), "publish bound concurrent SQLite metadata"
                ).unwrap_or_else(|| backend_error(error)));
            }
            self.0.current_file_stamp()?;
            Ok(next as u64)
        })
        }
    }

    async fn migrate_mrc1_to_bound_mode(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
    ) -> Result<()> {
        if !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("migrate concurrent SQLite metadata")
                .with_message("concurrent SQLite metadata requires a file-backed database"));
        }
        #[cfg(not(unix))]
        {
            let _ = (backing, expected_revision);
            Err(incompatible_schema(
                "MRC2 SQLite metadata requires a Unix file stamp",
            ))
        }
        #[cfg(unix)]
        {
            let expected =
                i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
            let mut connection = self.0.lock()?;
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(backend_error)?;
            let (mode, stored, revision, owner, fence, expires, physical_dev, physical_ino, physical_path):
            MetadataMigrationRow = tx.query_row(
                "SELECT write_mode, backing_id, revision, owner, fence, expires, physical_dev, physical_ino, physical_path FROM mount_rs_metadata WHERE id=1",
                [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?))
            ).map_err(backend_error)?;
            if mode.as_deref() == Some(BOUND_WRITE_MODE) {
                if stored.as_deref() != Some(backing.to_hex().as_str()) {
                    return Err(stale());
                }
                require_matching_metadata_stamp(
                    &self.0,
                    physical_dev.as_deref(),
                    physical_ino.as_deref(),
                    physical_path.as_deref(),
                )?;
                if owner.is_some() || fence != CONCURRENT_FENCE_SENTINEL || expires != 0 {
                    return Err(incompatible_schema(
                        "bound concurrent marker and fence disagree",
                    ));
                }
                return if revision == expected {
                    Ok(())
                } else {
                    Err(FsError::new(ErrorCode::Eagain)
                        .with_syscall("migrate concurrent SQLite metadata"))
                };
            }
            if mode.as_deref() != Some(CONCURRENT_WRITE_MODE)
                || stored.is_some()
                || owner.is_some()
                || fence != CONCURRENT_FENCE_SENTINEL
                || expires != 0
            {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("migrate concurrent SQLite metadata"));
            }
            if revision != expected {
                return Err(FsError::new(ErrorCode::Eagain)
                    .with_syscall("migrate concurrent SQLite metadata"));
            }
            let physical = require_matching_metadata_stamp(
                &self.0,
                physical_dev.as_deref(),
                physical_ino.as_deref(),
                physical_path.as_deref(),
            )?;
            let (head, versions, pins): (Option<String>, i64, i64) = tx.query_row(
            "SELECT head_id, (SELECT count(*) FROM mount_rs_versions), (SELECT count(*) FROM mount_rs_version_pins) FROM mount_rs_version_state WHERE id=1",
            [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        ).map_err(backend_error)?;
            if head.is_some() || versions != 0 || pins != 0 {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("migrate concurrent SQLite metadata")
                    .with_message("version state prevents concurrent backing migration"));
            }
            let changed = tx
                .execute(
                    "UPDATE mount_rs_metadata SET write_mode='MRC2', backing_id=?1
             WHERE id=1 AND write_mode='MRC1' AND backing_id IS NULL AND revision=?2
               AND owner IS NULL AND fence=?3 AND expires=0
               AND physical_dev=?4 AND physical_ino=?5 AND physical_path=?6",
                    params![
                        backing.to_hex(),
                        expected,
                        CONCURRENT_FENCE_SENTINEL,
                        physical.dev.to_string(),
                        physical.ino.to_string(),
                        physical_path
                    ],
                )
                .map_err(backend_error)?;
            if changed != 1 {
                return Err(stale());
            }
            self.0.current_file_stamp()?;
            tx.commit().map_err(backend_error)?;
            self.0.current_file_stamp()?;
            Ok(())
        }
    }

    async fn preflight_mrc1_to_bound_mode(&self, expected_revision: u64) -> Result<()> {
        if !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("preflight MRC1 SQLite metadata")
                .with_message("concurrent SQLite metadata requires a file-backed database"));
        }
        #[cfg(not(unix))]
        {
            let _ = expected_revision;
            Err(incompatible_schema(
                "MRC2 SQLite metadata requires a Unix file stamp",
            ))
        }
        #[cfg(unix)]
        {
            let expected =
                i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
            let connection = self.0.lock()?;
            let (mode, backing, revision, owner, fence, expires, dev, ino, path):
                MetadataMigrationRow = connection
                .query_row(
                    "SELECT write_mode, backing_id, revision, owner, fence, expires,
                            physical_dev, physical_ino, physical_path FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                        ))
                    },
                )
                .map_err(backend_error)?;
            if mode.as_deref() != Some(CONCURRENT_WRITE_MODE)
                || backing.is_some()
                || owner.is_some()
                || fence != CONCURRENT_FENCE_SENTINEL
                || expires != 0
            {
                return Err(
                    FsError::new(ErrorCode::Ebusy).with_syscall("preflight MRC1 SQLite metadata")
                );
            }
            if revision != expected {
                return Err(
                    FsError::new(ErrorCode::Eagain).with_syscall("preflight MRC1 SQLite metadata")
                );
            }
            require_matching_metadata_stamp(
                &self.0,
                dev.as_deref(),
                ino.as_deref(),
                path.as_deref(),
            )?;
            let (head, versions, pins): (Option<String>, i64, i64) = connection
                .query_row(
                    "SELECT head_id, (SELECT count(*) FROM mount_rs_versions),
                        (SELECT count(*) FROM mount_rs_version_pins)
                 FROM mount_rs_version_state WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(backend_error)?;
            if head.is_some() || versions != 0 || pins != 0 {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("preflight MRC1 SQLite metadata")
                    .with_message("version state prevents concurrent backing migration"));
            }
            self.0.current_file_stamp()?;
            Ok(())
        }
    }

    async fn preflight_trusted_unstamped_mrc1(
        &self,
        expected_revision: u64,
        expected_volume: VolumeId,
    ) -> Result<()> {
        if !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("preflight trusted SQLite MRC1 reenrollment"));
        }
        #[cfg(not(unix))]
        {
            let _ = (expected_revision, expected_volume);
            Err(incompatible_schema(
                "trusted SQLite MRC1 reenrollment requires a Unix file stamp",
            ))
        }
        #[cfg(unix)]
        {
            let expected =
                i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
            let connection = self.0.lock()?;
            let (mode, backing, revision, owner, fence, expires, volume, dev, ino, path):
                TrustedMrc1MigrationRow = connection
                .query_row(
                    "SELECT write_mode, backing_id, revision, owner, fence, expires,
                            volume_id, physical_dev, physical_ino, physical_path
                     FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                        ))
                    },
                )
                .map_err(backend_error)?;
            if mode.as_deref() != Some(CONCURRENT_WRITE_MODE)
                || backing.is_some()
                || owner.is_some()
                || fence != CONCURRENT_FENCE_SENTINEL
                || expires != 0
            {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("preflight trusted SQLite MRC1 reenrollment"));
            }
            if revision != expected {
                return Err(FsError::new(ErrorCode::Eagain)
                    .with_syscall("preflight trusted SQLite MRC1 reenrollment"));
            }
            if volume != expected_volume.as_str() {
                return Err(FsError::new(ErrorCode::Estale)
                    .with_syscall("preflight trusted SQLite MRC1 reenrollment")
                    .with_message("expected SQLite volume ID selects a different timeline"));
            }
            if FileStamp::from_text(dev.as_deref(), ino.as_deref())?.is_some() || path.is_some() {
                return Err(FsError::new(ErrorCode::Enotsup)
                    .with_syscall("preflight trusted SQLite MRC1 reenrollment")
                    .with_message("stamped MRC1 metadata uses ordinary migration"));
            }
            self.0.require_concurrent_local_file("metadata")?;
            self.0.current_auxiliary_path()?;
            let (head, versions, pins): (Option<String>, i64, i64) = connection
                .query_row(
                    "SELECT head_id, (SELECT count(*) FROM mount_rs_versions),
                        (SELECT count(*) FROM mount_rs_version_pins)
                 FROM mount_rs_version_state WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(backend_error)?;
            if head.is_some() || versions != 0 || pins != 0 {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("preflight trusted SQLite MRC1 reenrollment")
                    .with_message("version state prevents trusted reenrollment"));
            }
            self.0.current_file_stamp()?;
            Ok(())
        }
    }

    async fn migrate_trusted_unstamped_mrc1(
        &self,
        backing: ConcurrentBackingId,
        expected_revision: u64,
        expected_volume: VolumeId,
    ) -> Result<()> {
        if !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("migrate trusted SQLite MRC1 metadata"));
        }
        #[cfg(not(unix))]
        {
            let _ = (backing, expected_revision, expected_volume);
            Err(incompatible_schema(
                "trusted SQLite MRC1 reenrollment requires a Unix file stamp",
            ))
        }
        #[cfg(unix)]
        {
            let expected =
                i64::try_from(expected_revision).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
            let mut connection = self.0.lock()?;
            let tx = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(backend_error)?;
            let (mode, stored, revision, owner, fence, expires, volume, dev, ino, path):
                TrustedMrc1MigrationRow = tx
                .query_row(
                    "SELECT write_mode, backing_id, revision, owner, fence, expires,
                            volume_id, physical_dev, physical_ino, physical_path
                     FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                            row.get(8)?,
                            row.get(9)?,
                        ))
                    },
                )
                .map_err(backend_error)?;
            if mode.as_deref() != Some(CONCURRENT_WRITE_MODE)
                || stored.is_some()
                || owner.is_some()
                || fence != CONCURRENT_FENCE_SENTINEL
                || expires != 0
            {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("migrate trusted SQLite MRC1 metadata"));
            }
            if revision != expected {
                return Err(FsError::new(ErrorCode::Eagain)
                    .with_syscall("migrate trusted SQLite MRC1 metadata"));
            }
            if volume != expected_volume.as_str() {
                return Err(FsError::new(ErrorCode::Estale)
                    .with_syscall("migrate trusted SQLite MRC1 metadata")
                    .with_message("expected SQLite volume ID selects a different timeline"));
            }
            if FileStamp::from_text(dev.as_deref(), ino.as_deref())?.is_some() || path.is_some() {
                return Err(FsError::new(ErrorCode::Enotsup)
                    .with_syscall("migrate trusted SQLite MRC1 metadata")
                    .with_message("stamped MRC1 metadata uses ordinary migration"));
            }
            let physical = self.0.require_concurrent_local_file("metadata")?;
            let physical_path = self.0.current_auxiliary_path()?;
            let (head, versions, pins): (Option<String>, i64, i64) = tx
                .query_row(
                    "SELECT head_id, (SELECT count(*) FROM mount_rs_versions),
                        (SELECT count(*) FROM mount_rs_version_pins)
                 FROM mount_rs_version_state WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .map_err(backend_error)?;
            if head.is_some() || versions != 0 || pins != 0 {
                return Err(FsError::new(ErrorCode::Ebusy)
                    .with_syscall("migrate trusted SQLite MRC1 metadata")
                    .with_message("version state prevents trusted reenrollment"));
            }
            let changed = tx
                .execute(
                    "UPDATE mount_rs_metadata SET write_mode='MRC2', backing_id=?1,
                        physical_dev=?2, physical_ino=?3, physical_path=?4
                 WHERE id=1 AND write_mode='MRC1' AND backing_id IS NULL
                   AND revision=?5 AND volume_id=?6 AND owner IS NULL
                   AND fence=?7 AND expires=0
                   AND physical_dev IS NULL AND physical_ino IS NULL AND physical_path IS NULL",
                    params![
                        backing.to_hex(),
                        physical.dev.to_string(),
                        physical.ino.to_string(),
                        physical_path,
                        expected,
                        expected_volume.as_str(),
                        CONCURRENT_FENCE_SENTINEL
                    ],
                )
                .map_err(backend_error)?;
            if changed != 1 {
                return Err(FsError::new(ErrorCode::Eagain)
                    .with_syscall("migrate trusted SQLite MRC1 metadata"));
            }
            self.0.current_file_stamp()?;
            tx.commit().map_err(backend_error)?;
            self.0.current_file_stamp()?;
            Ok(())
        }
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush()
    }
}

#[async_trait]
impl VersionedMetadataStore for SqliteMetadataStore {
    fn volume_id(&self) -> VolumeId {
        self.1.clone()
    }

    async fn version_head(&self) -> Result<Option<VersionHead>> {
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Deferred)
            .map_err(backend_error)?;
        let revision: i64 = tx
            .query_row(
                "SELECT revision FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        let head_id: Option<String> = tx
            .query_row(
                "SELECT head_id FROM mount_rs_version_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        let head = if let Some(id) = head_id {
            let version = VersionId::decode(&id).map_err(|_| {
                FsError::new(ErrorCode::Eio)
                    .with_syscall("version head")
                    .with_message("stored version head is malformed")
            })?;
            if version.volume != self.1 {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("version head")
                    .with_message("stored version head belongs to another volume"));
            }
            if !stored_version_is_loadable(&tx, &version)? {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("version head")
                    .with_message("stored version head references an unreadable version"));
            }
            Some(VersionHead {
                version,
                revision: u64::try_from(revision).map_err(|_| FsError::new(ErrorCode::Eio))?,
            })
        } else {
            None
        };
        tx.commit().map_err(backend_error)?;
        Ok(head)
    }

    async fn load_version(&self, id: &VersionId) -> Result<VersionInfo> {
        if id.volume != self.1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("load version"));
        }
        let connection = self.0.lock()?;
        load_version_from_connection(&connection, id)
    }

    async fn list_versions(&self) -> Result<Vec<VersionInfo>> {
        let connection = self.0.lock()?;
        let mut statement = connection
            .prepare(&format!("{VERSION_SELECT} ORDER BY sequence"))
            .map_err(backend_error)?;
        let rows = statement
            .query_map([], raw_version)
            .map_err(backend_error)?;
        let mut versions = Vec::new();
        for row in rows {
            let version = decode_raw_version(row.map_err(backend_error)?)?;
            version.validate_for_volume(&self.1)?;
            versions.push(version);
        }
        Ok(versions)
    }

    async fn find_publication(&self, operation_id: &PublicationId) -> Result<Option<VersionInfo>> {
        let connection = self.0.lock()?;
        let id: Option<String> = connection
            .query_row(
                "SELECT id FROM mount_rs_versions WHERE operation_id=?1",
                params![operation_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend_error)?;
        id.map(|id| {
            let version = VersionId::decode(&id)?;
            load_version_from_connection(&connection, &version)
        })
        .transpose()
    }

    async fn publish_version(
        &self,
        lease: &WriterLease,
        publication: VersionPublication,
    ) -> Result<VersionInfo> {
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;

        let existing_id: Option<String> = tx
            .query_row(
                "SELECT id FROM mount_rs_versions WHERE operation_id=?1",
                params![publication.operation_id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend_error)?;
        if let Some(existing_id) = existing_id {
            let raw = tx
                .query_row(
                    &format!("{VERSION_SELECT} WHERE id=?1"),
                    params![existing_id],
                    raw_version,
                )
                .map_err(backend_error)?;
            let existing = decode_raw_version(raw)?;
            existing.validate_for_volume(&self.1)?;
            if !publication.matches_committed(&existing)? {
                return Err(FsError::new(ErrorCode::Eexist)
                    .with_syscall("publish version")
                    .with_message("publication id was reused with a different payload"));
            }
            tx.commit().map_err(backend_error)?;
            return Ok(existing);
        }

        // Only a new publication reaches the writer-lease and CAS checks.
        // A committed operation is immutable and may be reconciled after the
        // original writer lease has expired.
        publication.validate(&self.1)?;
        if publication.durable && !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("publish version")
                .with_message("in-memory SQLite cannot publish a durable version"));
        }
        let expected_revision = i64::try_from(publication.expected_revision)
            .map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let (fence, expires) = lease_numbers(lease)?;
        let valid: bool = tx
            .query_row(
                &format!(
                    "SELECT write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}
                     FROM mount_rs_metadata WHERE id=1"
                ),
                params![lease.owner, fence, expires],
                |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
            )
            .map_err(backend_error)?;
        if !valid {
            return Err(stale());
        }
        let (actual_revision, head_id): (i64, Option<String>) = tx
            .query_row(
                "SELECT revision, (SELECT head_id FROM mount_rs_version_state WHERE id=1)
                 FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(backend_error)?;
        if actual_revision != expected_revision {
            return Err(FsError::new(ErrorCode::Eagain)
                .with_syscall("publish version")
                .with_message(format!(
                    "metadata revision conflict: expected {}, actual {actual_revision}",
                    publication.expected_revision
                )));
        }
        if let Some(head_id) = head_id.as_deref() {
            let head = VersionId::decode(head_id).map_err(|_| {
                FsError::new(ErrorCode::Eio)
                    .with_syscall("publish version")
                    .with_message("stored version head is malformed")
            })?;
            if head.volume != self.1 || !stored_version_is_loadable(&tx, &head)? {
                return Err(FsError::new(ErrorCode::Eio)
                    .with_syscall("publish version")
                    .with_message("stored version head references an unreadable version"));
            }
        }
        let expected_parent = publication.expected_parent.as_ref().map(VersionId::encode);
        if head_id != expected_parent {
            return Err(FsError::new(ErrorCode::Eagain)
                .with_syscall("publish version")
                .with_message("version head changed concurrently"));
        }

        let next_sequence: i64 = tx
            .query_row(
                "SELECT next_sequence FROM mount_rs_version_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        let sequence = u64::try_from(next_sequence)
            .map_err(|_| FsError::new(ErrorCode::Eio).with_message("invalid version sequence"))?;
        let id = VersionId::new(self.1.clone(), sequence)?;
        let created_at_ms: i64 = tx
            .query_row(NOW_SELECT, [], |row| row.get(0))
            .map_err(backend_error)?;
        let namespace = serde_json::to_string(&publication.namespace).map_err(backend_error)?;
        let parent_id = publication.expected_parent.as_ref().map(VersionId::encode);
        let restored_from = publication.restored_from.as_ref().map(VersionId::encode);
        let forked_from = publication.forked_from.as_ref().map(VersionId::encode);
        tx.execute(
            "INSERT INTO mount_rs_versions
             (id, volume_id, sequence, parent_id, restored_from, forked_from,
              namespace, block_store_id, kind, created_at_ms, durable, operation_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                id.encode(),
                self.1.0,
                i64::try_from(sequence).map_err(|_| FsError::new(ErrorCode::Eoverflow))?,
                parent_id,
                restored_from,
                forked_from,
                namespace,
                publication.block_store_id.0,
                version_kind_name(&publication.kind),
                created_at_ms,
                if publication.durable { 1_i64 } else { 0_i64 },
                publication.operation_id.0,
            ],
        )
        .map_err(backend_error)?;
        if publication.kind == VersionKind::NamespacePublication {
            let changed = tx
                .execute(
                    "UPDATE mount_rs_schema_versions SET schema_version=?1
                     WHERE schema_name=?2 AND schema_version BETWEEN ?3 AND ?1",
                    params![
                        VERSION_SCHEMA_VERSION,
                        VERSION_SCHEMA_NAME,
                        VERSION_SCHEMA_BASE_VERSION
                    ],
                )
                .map_err(backend_error)?;
            if changed != 1 {
                return Err(incompatible_schema("version kind schema gate is missing"));
            }
        }
        let revision = expected_revision
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let namespace_json =
            serde_json::to_string(&publication.namespace).map_err(backend_error)?;
        let changed = tx
            .execute(
                "UPDATE mount_rs_metadata SET revision=?1, namespace=?2
                 WHERE id=1 AND write_mode IS NULL AND revision=?3",
                params![revision, namespace_json, expected_revision],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            return Err(FsError::new(ErrorCode::Eagain).with_syscall("publish version"));
        }
        tx.execute(
            "UPDATE mount_rs_version_state SET head_id=?1, next_sequence=?2 WHERE id=1",
            params![
                id.encode(),
                next_sequence
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?
            ],
        )
        .map_err(backend_error)?;
        tx.commit().map_err(backend_error)?;

        Ok(VersionInfo {
            id,
            parent: publication.expected_parent,
            restored_from: publication.restored_from,
            forked_from: publication.forked_from,
            kind: publication.kind,
            namespace: publication.namespace,
            block_store_id: publication.block_store_id,
            created_at_ms,
            durable: publication.durable,
        })
    }

    async fn open_view_pin(&self, id: &VersionId, request: ReadLeaseRequest) -> Result<ReadLease> {
        let ttl_ms = request.validate()?;
        let ttl_ms = i64::try_from(ttl_ms).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        if id.volume != self.1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("open version view"));
        }
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let raw = raw_version_from_connection(&tx, id)?;
        let Some(raw) = raw else {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("open version view"));
        };
        decode_version_for_id(raw, id).map_err(|_| {
            FsError::new(ErrorCode::Eio)
                .with_syscall("open version view")
                .with_message("stored version is unreadable")
        })?;
        let now_ms: i64 = tx
            .query_row(NOW_SELECT, [], |row| row.get(0))
            .map_err(backend_error)?;
        let next_fence: i64 = tx
            .query_row(
                "SELECT next_read_fence FROM mount_rs_version_state WHERE id=1",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(backend_error)?
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let expires = now_ms
            .checked_add(ttl_ms)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let view_id = format!("{}-view-{next_fence}", self.1);
        tx.execute(
            "UPDATE mount_rs_version_state SET next_read_fence=?1 WHERE id=1",
            params![next_fence],
        )
        .map_err(backend_error)?;
        tx.execute(
            "INSERT INTO mount_rs_version_pins
             (view_id, volume_id, version_id, owner, fence, expires)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                view_id,
                self.1.0,
                id.encode(),
                request.owner,
                next_fence,
                expires
            ],
        )
        .map_err(backend_error)?;
        tx.commit().map_err(backend_error)?;
        Ok(ReadLease {
            volume: self.1.clone(),
            version: id.clone(),
            view_id,
            owner: request.owner,
            fence: u64::try_from(next_fence).map_err(|_| FsError::new(ErrorCode::Eio))?,
            expires_at_ms: u64::try_from(expires).map_err(|_| FsError::new(ErrorCode::Eio))?,
        })
    }

    async fn renew_view_pin(
        &self,
        lease: &ReadLease,
        request: ReadLeaseRequest,
    ) -> Result<ReadLease> {
        if lease.volume != self.1 || lease.version.volume != self.1 {
            return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
        }
        let ttl_ms = request.validate()?;
        let ttl_ms = i64::try_from(ttl_ms).map_err(|_| FsError::new(ErrorCode::Eoverflow))?;
        let fence = i64::try_from(lease.fence).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let expires =
            i64::try_from(lease.expires_at_ms).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let current: Option<(String, String, String, i64, i64)> = tx
            .query_row(
                "SELECT volume_id, version_id, owner, fence, expires
                 FROM mount_rs_version_pins WHERE view_id=?1",
                params![lease.view_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .optional()
            .map_err(backend_error)?;
        let Some((volume, version, owner, current_fence, current_expires)) = current else {
            return Err(FsError::new(ErrorCode::Estale).with_syscall("renew view"));
        };
        let now_ms: i64 = tx
            .query_row(NOW_SELECT, [], |row| row.get(0))
            .map_err(backend_error)?;
        let next_expires = plan_view_pin_renewal(
            PinRenewalIdentity {
                volume_matches: volume == self.1.0,
                version_matches: version == lease.version.encode(),
                request_owner_matches: owner == request.owner,
                lease_owner_matches: owner == lease.owner,
            },
            PinRenewalToken {
                fence: current_fence,
                expires_at_ms: current_expires,
            },
            PinRenewalToken {
                fence,
                expires_at_ms: expires,
            },
            now_ms,
            ttl_ms,
        )?;
        tx.execute(
            "UPDATE mount_rs_version_pins SET expires=?1 WHERE view_id=?2
             AND fence=?3 AND expires=?4",
            params![next_expires, lease.view_id, fence, expires],
        )
        .map_err(backend_error)?;
        tx.commit().map_err(backend_error)?;
        Ok(ReadLease {
            expires_at_ms: u64::try_from(next_expires).map_err(|_| FsError::new(ErrorCode::Eio))?,
            ..lease.clone()
        })
    }

    async fn close_view_pin(&self, lease: &ReadLease) -> Result<()> {
        let fence = i64::try_from(lease.fence).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let expires =
            i64::try_from(lease.expires_at_ms).map_err(|_| FsError::new(ErrorCode::Estale))?;
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let changed = tx
            .execute(
                "DELETE FROM mount_rs_version_pins
                 WHERE view_id=?1 AND volume_id=?2 AND version_id=?3 AND owner=?4
                   AND fence=?5 AND expires=?6 AND ?7=?2 AND ?8=?2",
                params![
                    lease.view_id,
                    self.1.0,
                    lease.version.encode(),
                    lease.owner,
                    fence,
                    expires,
                    lease.volume.0,
                    lease.version.volume.0,
                ],
            )
            .map_err(backend_error)?;
        if changed == 1 {
            tx.commit().map_err(backend_error)?;
            return Ok(());
        }
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM mount_rs_version_pins WHERE view_id=?1)",
                params![lease.view_id],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        tx.commit().map_err(backend_error)?;
        if exists {
            Err(FsError::new(ErrorCode::Estale).with_syscall("close view"))
        } else {
            // Missing pins are intentionally idempotent so cleanup after a
            // provider expiry or an earlier close does not fail the caller.
            Ok(())
        }
    }

    async fn delete_version(&self, lease: &WriterLease, id: &VersionId) -> Result<()> {
        if id.volume != self.1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("delete version"));
        }
        let (fence, expires) = lease_numbers(lease)?;
        let mut connection = self.0.lock()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        let valid: bool = tx
            .query_row(
                &format!(
                    "SELECT write_mode IS NULL AND owner=?1 AND fence=?2 AND expires=?3 AND expires>{NOW}
                     FROM mount_rs_metadata WHERE id=1"
                ),
                params![lease.owner, fence, expires],
                |row| Ok(row.get::<_, Option<bool>>(0)?.unwrap_or(false)),
            )
            .map_err(backend_error)?;
        if !valid {
            return Err(stale());
        }
        let head_id: Option<String> = tx
            .query_row(
                "SELECT head_id FROM mount_rs_version_state WHERE id=1",
                [],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        if head_id.as_deref() == Some(&id.encode()) {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("delete version")
                .with_message("current version cannot be deleted"));
        }
        let protected: bool = tx
            .query_row(
                &format!(
                    "SELECT EXISTS(SELECT 1 FROM mount_rs_version_pins
                     WHERE version_id=?1 AND expires>{NOW})"
                ),
                params![id.encode()],
                |row| row.get(0),
            )
            .map_err(backend_error)?;
        if protected {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_syscall("delete version")
                .with_message("version has an active read lease"));
        }
        let changed = tx
            .execute(
                "DELETE FROM mount_rs_versions WHERE id=?1",
                params![id.encode()],
            )
            .map_err(backend_error)?;
        if changed != 1 {
            return Err(FsError::new(ErrorCode::Enoent).with_syscall("delete version"));
        }
        tx.commit().map_err(backend_error)?;
        Ok(())
    }
}

impl SqliteBlockStore {
    fn require_concurrent_local_backing(&self) -> Result<()> {
        if !self.0.durable {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("prepare concurrent SQLite blocks")
                .with_message(
                    "independent concurrent writers require a file-backed SQLite block database",
                ));
        }
        #[cfg(not(unix))]
        {
            Err(FsError::new(ErrorCode::Enotsup)
                .with_syscall("prepare concurrent SQLite blocks")
                .with_message("this platform cannot bind a SQLite block ID to a physical file"))
        }
        #[cfg(unix)]
        {
            self.0.require_concurrent_local_file("blocks")?;
            Ok(())
        }
    }
}

#[async_trait]
impl BlockStore for SqliteBlockStore {
    fn durable(&self) -> bool {
        self.0.durable
    }

    async fn prepare_concurrent_backing(&self) -> Result<ConcurrentBackingId> {
        self.require_concurrent_local_backing()?;
        #[cfg(unix)]
        let physical = self.0.current_file_stamp()?;
        #[cfg(unix)]
        let physical_path = self.0.current_auxiliary_path()?;
        let mut connection = self.0.lock()?;
        #[cfg(unix)]
        self.0.current_file_stamp()?;
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend_error)?;
        #[cfg(unix)]
        let (dev, ino) = (physical.dev.to_string(), physical.ino.to_string());
        #[cfg(unix)]
        tx.execute(
            "INSERT OR IGNORE INTO mount_rs_block_authority(id,backing_id,physical_dev,physical_ino,physical_path)
             VALUES(1,lower(hex(randomblob(16))),?1,?2,?3)",
            params![dev, ino, physical_path]
        ).map_err(backend_error)?;
        #[cfg(not(unix))]
        tx.execute(
            "INSERT OR IGNORE INTO mount_rs_block_authority(id,backing_id) VALUES(1,lower(hex(randomblob(16))))",
            []
        ).map_err(backend_error)?;
        let (text, stored_dev, stored_ino, stored_path): (String, Option<String>, Option<String>, Option<String>) = tx
            .query_row(
                "SELECT backing_id, physical_dev, physical_ino, physical_path FROM mount_rs_block_authority WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(backend_error)?;
        let id = ConcurrentBackingId::from_hex(&text)
            .map_err(|_| incompatible_schema("stored SQLite block authority ID is invalid"))?;
        #[cfg(unix)]
        if stored_dev.as_deref() != Some(dev.as_str())
            || stored_ino.as_deref() != Some(ino.as_str())
        {
            return Err(FsError::new(ErrorCode::Estale)
                .with_syscall("prepare concurrent SQLite backing")
                .with_message("SQLite block authority belongs to another physical file"));
        }
        #[cfg(unix)]
        require_matching_auxiliary_path(&self.0, stored_path.as_deref())?;
        #[cfg(not(unix))]
        let _ = (stored_dev, stored_ino, stored_path);
        #[cfg(unix)]
        self.0.current_file_stamp()?;
        tx.commit().map_err(backend_error)?;
        Ok(id)
    }

    async fn verify_concurrent_backing(&self, expected: ConcurrentBackingId) -> Result<()> {
        self.require_concurrent_local_backing()?;
        let connection = self.0.lock()?;
        #[cfg(unix)]
        let physical = self.0.current_file_stamp()?;
        let row: Option<(String, Option<String>, Option<String>, Option<String>)> = connection
            .query_row(
                "SELECT backing_id, physical_dev, physical_ino, physical_path FROM mount_rs_block_authority WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(backend_error)?;
        #[cfg(unix)]
        self.0.current_file_stamp()?;
        #[cfg(unix)]
        let matches = row.as_ref().is_some_and(|(id, dev, ino, _)| {
            id == &expected.to_hex()
                && dev.as_deref() == Some(physical.dev.to_string().as_str())
                && ino.as_deref() == Some(physical.ino.to_string().as_str())
        });
        #[cfg(unix)]
        if let Some((_, _, _, stored_path)) = row.as_ref() {
            require_matching_auxiliary_path(&self.0, stored_path.as_deref())?;
        }
        #[cfg(not(unix))]
        let matches = row
            .as_ref()
            .is_some_and(|(id, _, _, _)| id == &expected.to_hex());
        if matches { Ok(()) } else { Err(stale()) }
    }

    async fn get_for_migration(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.get(id).await
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let started = Instant::now();
        for attempt in 0..MAX_SQLITE_BUSY_RETRIES {
            match self.put_once(bytes) {
                Ok(id) => return Ok(id),
                Err(error) if error.code == ErrorCode::Eagain => {
                    if attempt + 1 == MAX_SQLITE_BUSY_RETRIES
                        || started.elapsed() >= SQLITE_BUSY_RETRY_BUDGET
                    {
                        return Err(error.with_message(
                            "SQLite block writer remained busy after bounded retries",
                        ));
                    }
                    // Each put_once releases the SQLite connection lock before
                    // yielding, so another writer can finish its transaction.
                    sqlite_busy_backoff(attempt).await;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!("bounded SQLite block retry loop always returns")
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.0
            .lock()?
            .query_row(
                "SELECT bytes FROM mount_rs_blocks WHERE id=?1",
                params![id.0],
                |row| row.get(0),
            )
            .optional()
            .map_err(backend_error)?
            .ok_or_else(|| FsError::new(ErrorCode::Enoent).with_syscall("get block"))
    }

    async fn flush(&self) -> Result<()> {
        self.0.flush()
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.0
            .lock()?
            .execute("DELETE FROM mount_rs_blocks WHERE id=?1", params![id.0])
            .map_err(backend_error)?;
        Ok(())
    }
}

#[cfg(kani)]
mod verification {
    use super::*;

    #[kani::proof]
    #[kani::unwind(8)]
    fn view_pin_renewal_requires_exact_live_token_and_checked_expiry() {
        let volume_matches: bool = kani::any();
        let version_matches: bool = kani::any();
        let request_owner_matches: bool = kani::any();
        let lease_owner_matches: bool = kani::any();
        let current_fence: i64 = kani::any();
        let lease_fence: i64 = kani::any();
        let current_expires: i64 = kani::any();
        let lease_expires: i64 = kani::any();
        let now_ms: i64 = kani::any();
        let ttl_ms: i64 = kani::any();
        kani::assume(current_fence > 0);
        kani::assume(lease_fence >= 0);
        kani::assume(current_expires >= 0);
        kani::assume(lease_expires >= 0);
        kani::assume(now_ms >= 0);
        kani::assume(ttl_ms > 0);

        let observed = plan_view_pin_renewal(
            PinRenewalIdentity {
                volume_matches,
                version_matches,
                request_owner_matches,
                lease_owner_matches,
            },
            PinRenewalToken {
                fence: current_fence,
                expires_at_ms: current_expires,
            },
            PinRenewalToken {
                fence: lease_fence,
                expires_at_ms: lease_expires,
            },
            now_ms,
            ttl_ms,
        )
        .map_err(|error| error.code);
        let exact_identity = volume_matches
            && version_matches
            && request_owner_matches
            && lease_owner_matches
            && current_fence == lease_fence
            && current_expires == lease_expires;
        let wide_expiry = i128::from(now_ms) + i128::from(ttl_ms);
        let expected = if !exact_identity || current_expires <= now_ms {
            Err(ErrorCode::Estale)
        } else if wide_expiry > i128::from(i64::MAX) {
            Err(ErrorCode::Eoverflow)
        } else {
            Ok(wide_expiry as i64)
        };

        kani::cover!(exact_identity && current_expires > now_ms && observed.is_ok());
        kani::cover!(exact_identity && current_expires == now_ms && observed.is_err());
        kani::cover!(
            exact_identity && current_expires < now_ms && observed == Err(ErrorCode::Estale)
        );
        kani::cover!(
            !volume_matches
                && version_matches
                && request_owner_matches
                && lease_owner_matches
                && current_fence == lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && !version_matches
                && request_owner_matches
                && lease_owner_matches
                && current_fence == lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && version_matches
                && !request_owner_matches
                && lease_owner_matches
                && current_fence == lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && version_matches
                && request_owner_matches
                && lease_owner_matches
                && current_fence != lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && version_matches
                && request_owner_matches
                && !lease_owner_matches
                && current_fence == lease_fence
                && current_expires == lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            volume_matches
                && version_matches
                && request_owner_matches
                && lease_owner_matches
                && current_fence == lease_fence
                && current_expires != lease_expires
                && current_expires > now_ms
        );
        kani::cover!(
            exact_identity
                && current_expires > now_ms
                && wide_expiry > i128::from(i64::MAX)
                && observed == Err(ErrorCode::Eoverflow)
        );
        kani::cover!(
            exact_identity && current_expires > now_ms && wide_expiry == i128::from(i64::MAX)
        );
        assert_eq!(observed, expected);
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn signed_writer_lease_boundaries_are_checked() {
        let seconds: u64 = kani::any();
        let nanos: u32 = kani::any();
        let fence: u64 = kani::any();
        let expiry: u64 = kani::any();
        kani::assume(nanos < 1_000_000_000);

        let exact_ms = u128::from(seconds) * 1_000 + u128::from(nanos / 1_000_000);
        let observed_ttl = ttl_ms(Duration::new(seconds, nanos)).map_err(|error| error.code);
        let expected_ttl = if exact_ms == 0 || exact_ms > i64::MAX as u128 {
            Err(ErrorCode::Einval)
        } else {
            Ok(exact_ms as i64)
        };
        assert_eq!(observed_ttl, expected_ttl);

        let lease = WriterLease {
            owner: "writer".to_owned(),
            fence,
            expires_at_ms: expiry,
        };
        let observed_token = lease_numbers(&lease).map_err(|error| error.code);
        let expected_token = if fence > i64::MAX as u64 || expiry > i64::MAX as u64 {
            Err(ErrorCode::Estale)
        } else {
            Ok((fence as i64, expiry as i64))
        };
        assert_eq!(observed_token, expected_token);

        kani::cover!(exact_ms == 0 && observed_ttl == Err(ErrorCode::Einval));
        kani::cover!(exact_ms == 1 && observed_ttl == Ok(1));
        kani::cover!(exact_ms == i64::MAX as u128 && observed_ttl == Ok(i64::MAX));
        kani::cover!(exact_ms > i64::MAX as u128 && observed_ttl == Err(ErrorCode::Einval));
        kani::cover!(
            fence <= i64::MAX as u64 && expiry <= i64::MAX as u64 && observed_token.is_ok()
        );
        kani::cover!(
            fence > i64::MAX as u64 && expiry <= i64::MAX as u64 && observed_token.is_err()
        );
        kani::cover!(
            fence <= i64::MAX as u64 && expiry > i64::MAX as u64 && observed_token.is_err()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_sqlite_mrc2_filesystem_type_guard_fails_closed() {
        for magic in [
            0x0000_ef53,
            0x5846_5342,
            0x9123_683e,
            0xf2f5_2010,
            0x0102_1994,
        ] {
            assert!(
                linux_qualified_local_fs_type(magic),
                "local type {magic:#x}"
            );
        }
        for magic in [
            0x0000_6969, // NFS
            0xff53_4d42, // CIFS
            0x6573_5546, // FUSE
            0x0102_1997, // 9P
            0x794c_7630, // overlayfs
            0,           // unknown
        ] {
            assert!(
                !linux_qualified_local_fs_type(magic),
                "remote/unknown type {magic:#x}"
            );
        }
    }
    use mount_rs_core::{
        FsDriver,
        chunking::{Chunker, FixedSizeChunker},
        storage::{ConcurrentBackingId, NodeData, NodeMetadata},
    };
    use mount_rs_memfs::MemoryFs;
    use std::{
        collections::BTreeMap,
        future::Future,
        sync::Barrier,
        task::{Context, Poll, Waker},
        thread,
        time::Instant,
    };

    fn run<T>(future: impl Future<Output = T>) -> T {
        let mut future = Box::pin(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("SQLite operations must complete synchronously"),
        }
    }

    #[test]
    fn bundled_sqlite_has_wal_reset_fix() {
        // The WAL-reset race affects concurrent writers/checkpointers through
        // SQLite 3.51.2. Our bundled line must include the upstream fix:
        // https://sqlite.org/wal.html#the_wal_reset_bug
        assert!(
            rusqlite::version_number() >= 3_051_003,
            "bundled SQLite {} lacks the WAL-reset fix",
            rusqlite::version(),
        );
        assert_eq!(
            rusqlite::version_number(),
            rusqlite::ffi::SQLITE_VERSION_NUMBER,
            "runtime SQLite must match the bundled headers",
        );
    }

    #[test]
    fn wal_checkpoint_contention_preserves_acknowledged_blocks() {
        struct OwnedDirectory(std::path::PathBuf);
        impl Drop for OwnedDirectory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }

        let owned = OwnedDirectory(super::super::tests::unique_database_path());
        std::fs::create_dir(&owned.0).unwrap();
        let path = owned.0.join("blocks.sqlite");
        let first = SqliteBlockStore::open(&path).unwrap();
        {
            let connection = first.0.lock().unwrap();
            let journal: String = connection
                .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "wal");
            connection
                .execute_batch("PRAGMA wal_autocheckpoint=0")
                .unwrap();
        }
        let second = SqliteBlockStore::open(&path).unwrap();
        second
            .0
            .lock()
            .unwrap()
            .execute_batch("PRAGMA wal_autocheckpoint=0")
            .unwrap();
        let initial = b"before the pinned reader".to_vec();
        let initial_id = futures_lite::future::block_on(first.put(&initial)).unwrap();
        let mut acknowledged = vec![(initial_id, initial)];

        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let count: i64 = reader
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        let after_reader = b"after the pinned reader".to_vec();
        let id = futures_lite::future::block_on(second.put(&after_reader)).unwrap();
        acknowledged.push((id, after_reader));

        let checkpointer = Connection::open(&path).unwrap();
        checkpointer
            .busy_timeout(Duration::from_millis(10))
            .unwrap();
        let blocked: (i64, i64, i64) = checkpointer
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(blocked.0, 1, "the pinned reader prevents WAL reset");
        assert!(
            blocked.1 > blocked.2,
            "new frames remain after the reader snapshot"
        );

        // Interleave real provider commits with each checkpoint mode. Every
        // checkpoint runs while a provider connection holds a reserved writer
        // transaction, and the next round waits for the preceding commit ACK.
        // This bounded edge test is not a deterministic reproducer for SQLite's
        // rare upstream race; the version gate above ensures its fix is present.
        thread::scope(|scope| {
            let (round_tx, round_rx) = std::sync::mpsc::channel();
            let (completed_tx, completed_rx) = std::sync::mpsc::channel();
            let worker_path = &path;
            let worker = scope.spawn(move || {
                let connection = Connection::open(worker_path).unwrap();
                connection.busy_timeout(Duration::from_millis(10)).unwrap();
                let mut attempted = 0;
                let mut saw_frames = false;
                for attempt in 0..64 {
                    assert_eq!(
                        round_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
                        attempt,
                    );
                    let mode = ["PASSIVE", "FULL", "RESTART", "TRUNCATE"][attempt % 4];
                    let result = connection.query_row(
                        &format!("PRAGMA wal_checkpoint({mode})"),
                        [],
                        |row| {
                            Ok((
                                row.get::<_, i64>(0)?,
                                row.get::<_, i64>(1)?,
                                row.get::<_, i64>(2)?,
                            ))
                        },
                    );
                    match result {
                        Ok((busy, frames, copied)) => {
                            assert!([0, 1].contains(&busy));
                            if mode != "PASSIVE" {
                                assert_eq!(busy, 1, "a reserved writer prevents {mode}");
                            }
                            assert!(frames >= 0 && copied >= 0 && copied <= frames);
                            saw_frames |= frames > 0;
                        }
                        Err(error) => assert_eq!(
                            error.sqlite_error_code(),
                            Some(rusqlite::ErrorCode::DatabaseBusy),
                            "only checkpoint contention is retryable: {error}",
                        ),
                    }
                    attempted += 1;
                    completed_tx.send(attempt).unwrap();
                }
                (attempted, saw_frames)
            });
            for index in 0..64_u8 {
                let store = if index % 2 == 0 { &first } else { &second };
                {
                    let connection = store.0.lock().unwrap();
                    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
                    round_tx.send(usize::from(index)).unwrap();
                    assert_eq!(
                        completed_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
                        usize::from(index),
                    );
                    connection.execute_batch("ROLLBACK").unwrap();
                }
                let bytes = vec![index; 4096];
                let id = futures_lite::future::block_on(store.put(&bytes)).unwrap();
                assert_eq!(
                    futures_lite::future::block_on(second.get(&id)).unwrap(),
                    bytes,
                    "acknowledged blocks are immediately visible",
                );
                acknowledged.push((id, bytes));
            }
            let (attempted, saw_frames) = worker.join().unwrap();
            assert_eq!(attempted, 64);
            assert!(saw_frames, "checkpoints must inspect a nonempty WAL");
        });
        let pinned_count: i64 = reader
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(pinned_count, 1, "the reader retains its original snapshot");
        reader.execute_batch("ROLLBACK").unwrap();
        let reset: (i64, i64, i64) = checkpointer
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(
            reset,
            (0, 0, 0),
            "WAL resets after the pinned reader closes"
        );
        drop(checkpointer);
        drop(reader);
        drop(second);
        drop(first);

        let reopened = SqliteBlockStore::open(&path).unwrap();
        for (id, bytes) in &acknowledged {
            assert_eq!(
                futures_lite::future::block_on(reopened.get(id)).unwrap(),
                *bytes
            );
        }
        drop(reopened);
        let fresh = Connection::open(&path).unwrap();
        let count: i64 = fresh
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, acknowledged.len() as i64);
        let integrity: String = fresh
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))
            .unwrap();
        assert_eq!(integrity, "ok");
    }

    fn namespace() -> Namespace {
        let stats = run(MemoryFs::empty().stat("/")).unwrap();
        let root = stats.ino;
        Namespace {
            format_version: 1,
            root,
            next_inode: root + 1,
            default_uid: 0,
            default_gid: 0,
            umask: 0o022,
            default_chunker: FixedSizeChunker::new(4096).unwrap().config(),
            nodes: BTreeMap::from([(
                root,
                NodeMetadata {
                    stats,
                    data: NodeData::Directory { entries: vec![] },
                },
            )]),
        }
    }

    #[cfg(unix)]
    #[test]
    fn delegated_authority_is_durable_and_fences_old_writers() {
        use mount_rs_core::storage::{CheckoutRequest, DelegatedCheckin, DelegatedPublish};
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);
        let ns = namespace();
        run(store.publish_bound_if_revision(backing, 0, ns.clone())).unwrap();
        run(store.prepare_delegated_mode(backing, 1)).unwrap();
        assert_eq!(
            run(store.acquire_writer("old", Duration::from_secs(30)))
                .unwrap_err()
                .code,
            ErrorCode::Ebusy
        );
        assert_eq!(
            run(store.publish_bound_if_revision(backing, 1, ns.clone()))
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        let request = CheckoutRequest {
            backing,
            root: ns.clone().root,
            owner: "session-a".into(),
        };
        let grant = run(store.checkout(&request)).unwrap();
        assert_eq!(run(store.checkout(&request)).unwrap().token, grant.token);
        assert_eq!(
            run(store.checkout(&CheckoutRequest {
                owner: "session-b".into(),
                ..request
            }))
            .unwrap_err()
            .code,
            ErrorCode::Estale
        );
        drop(store);
        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(
            run(reopened.delegation_state())
                .unwrap()
                .unwrap()
                .grants
                .get(&grant.token.root)
                .unwrap()
                .token,
            grant.token
        );
        let publication = DelegatedPublish {
            backing,
            token: grant.token.clone(),
            expected_revision: 1,
        };
        assert_eq!(
            run(reopened.publish_delegated(&publication, ns.clone())).unwrap(),
            2
        );
        let release = DelegatedCheckin {
            backing,
            token: grant.token.clone(),
            expected_revision: 2,
        };
        run(reopened.checkin(&release)).unwrap();
        run(reopened.checkin(&release)).unwrap();
        let next = run(reopened.checkout(&CheckoutRequest {
            backing,
            root: ns.clone().root,
            owner: "session-b".into(),
        }))
        .unwrap();
        assert!(next.token.fence > grant.token.fence);
        let current = run(reopened.load()).unwrap().namespace.unwrap();
        assert_eq!(
            run(reopened.publish_delegated(
                &DelegatedPublish {
                    backing,
                    token: next.token,
                    expected_revision: 2
                },
                current
            ))
            .unwrap(),
            3
        );
        run(reopened.checkin(&release)).unwrap();

        assert_eq!(
            run(reopened.publish_delegated(
                &DelegatedPublish {
                    expected_revision: 3,
                    ..publication
                },
                ns.clone()
            ))
            .unwrap_err()
            .code,
            ErrorCode::Estale
        );
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn delegated_publication_checks_header_backing_and_revision_inside_transaction() {
        use mount_rs_core::storage::{CheckoutRequest, DelegatedPublish, DelegatedRecovery};
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);
        run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap();
        run(store.prepare_delegated_mode(backing, 1)).unwrap();
        let grant = run(store.checkout(&CheckoutRequest {
            backing,
            root: namespace().root,
            owner: "session".into(),
        }))
        .unwrap();
        let publication = DelegatedPublish {
            backing,
            token: grant.token.clone(),
            expected_revision: 1,
        };
        let mut hostile = namespace();
        hostile.umask = 0o077;
        assert!(run(store.publish_delegated(&publication, hostile)).is_err());
        assert_eq!(
            run(store.publish_delegated(
                &DelegatedPublish {
                    expected_revision: 0,
                    ..publication.clone()
                },
                namespace()
            ))
            .unwrap_err()
            .code,
            ErrorCode::Eagain
        );
        assert_eq!(
            run(store.publish_delegated(
                &DelegatedPublish {
                    backing: ConcurrentBackingId::from_bytes([0x43; 16]).unwrap(),
                    ..publication.clone()
                },
                namespace()
            ))
            .unwrap_err()
            .code,
            ErrorCode::Estale
        );
        assert_eq!(
            run(store.recover(&DelegatedRecovery {
                backing,
                root: grant.token.root,
                expected_fence: grant.token.fence + 1
            }))
            .unwrap_err()
            .code,
            ErrorCode::Estale
        );
        run(store.recover(&DelegatedRecovery {
            backing,
            root: grant.token.root,
            expected_fence: grant.token.fence,
        }))
        .unwrap();
        assert_eq!(
            run(store.publish_delegated(&publication, namespace()))
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    fn delegated_two_directories() -> Namespace {
        use mount_rs_core::storage::DirectoryEntry;
        let mut ns = namespace();
        let root = ns.root;
        for inode in [root + 1, root + 2] {
            let mut node = ns.nodes[&root].clone();
            node.stats.ino = inode;
            node.stats.nlink = 2;
            ns.nodes.insert(inode, node);
        }
        ns.nodes.get_mut(&root).unwrap().stats.nlink = 4;
        ns.nodes.get_mut(&root).unwrap().data = NodeData::Directory {
            entries: vec![
                DirectoryEntry {
                    name: "a".into(),
                    inode: root + 1,
                },
                DirectoryEntry {
                    name: "b".into(),
                    inode: root + 2,
                },
            ],
        };
        ns.next_inode = root + 3;
        ns.validate().unwrap();
        ns
    }

    #[cfg(unix)]
    #[test]
    fn delegated_subtree_blocks_hostile_outside_edits_and_allocation_reuse() {
        use mount_rs_core::storage::{CheckoutRequest, DelegatedPublish};
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);
        let ns = delegated_two_directories();
        run(store.publish_bound_if_revision(backing, 0, ns.clone())).unwrap();
        run(store.prepare_delegated_mode(backing, 1)).unwrap();
        let a = run(store.checkout(&CheckoutRequest {
            backing,
            root: ns.root + 1,
            owner: "a".into(),
        }))
        .unwrap();
        let _b = run(store.checkout(&CheckoutRequest {
            backing,
            root: ns.root + 2,
            owner: "b".into(),
        }))
        .unwrap();
        assert!(
            run(store.checkout(&CheckoutRequest {
                backing,
                root: ns.root,
                owner: "parent".into()
            }))
            .is_err()
        );
        let publish = DelegatedPublish {
            backing,
            token: a.token,
            expected_revision: 1,
        };
        let mut hostile = ns.clone();
        hostile.nodes.get_mut(&(ns.root + 2)).unwrap().stats.uid = 123;
        assert!(run(store.publish_delegated(&publish, hostile)).is_err());
        let mut hostile = ns.clone();
        hostile.next_inode -= 1;
        assert!(run(store.publish_delegated(&publish, hostile)).is_err());
        let mut allowed = ns;
        allowed
            .nodes
            .get_mut(&(allowed.root + 1))
            .unwrap()
            .stats
            .uid = 123;
        assert_eq!(run(store.publish_delegated(&publish, allowed)).unwrap(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    #[test]
    fn delegated_failed_claim_commit_retains_no_authority_and_copy_is_rejected() {
        use mount_rs_core::storage::CheckoutRequest;
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);
        run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap();
        run(store.prepare_delegated_mode(backing, 1)).unwrap();
        // A rollback-journal reader permits BEGIN IMMEDIATE but prevents COMMIT.
        store
            .0
            .lock()
            .unwrap()
            .execute_batch("PRAGMA journal_mode=DELETE")
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        reader
            .execute_batch("BEGIN; SELECT namespace FROM mount_rs_metadata")
            .unwrap();
        let request = CheckoutRequest {
            backing,
            root: namespace().root,
            owner: "retry".into(),
        };
        assert_eq!(
            run(store.checkout(&request)).unwrap_err().code,
            ErrorCode::Eagain
        );
        reader.execute_batch("ROLLBACK").unwrap();
        assert!(
            run(store.delegation_state())
                .unwrap()
                .unwrap()
                .grants
                .is_empty()
        );
        let granted = run(store.checkout(&request)).unwrap();
        assert_eq!(granted.token.fence, 1);
        reader
            .execute_batch("BEGIN; SELECT namespace FROM mount_rs_metadata")
            .unwrap();
        let release = mount_rs_core::storage::DelegatedCheckin {
            backing,
            token: granted.token.clone(),
            expected_revision: 1,
        };
        assert_eq!(
            run(store.checkin(&release)).unwrap_err().code,
            ErrorCode::Eagain
        );
        reader.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            run(store.delegation_state())
                .unwrap()
                .unwrap()
                .grants
                .get(&granted.token.root)
                .unwrap()
                .token,
            granted.token
        );
        run(store.checkin(&release)).unwrap();
        drop(store);
        let copy = super::super::tests::unique_database_path();
        std::fs::copy(&path, &copy).unwrap();
        assert!(SqliteMetadataStore::open(&copy).is_err());
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(copy);
    }

    #[cfg(unix)]
    #[test]
    fn delegated_ignored_updates_cannot_acknowledge_authority() {
        use mount_rs_core::storage::CheckoutRequest;
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);
        run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap();
        store.0.lock().unwrap().execute_batch("CREATE TRIGGER ignore_delegation BEFORE UPDATE ON mount_rs_metadata BEGIN SELECT RAISE(IGNORE); END").unwrap();
        assert!(run(store.prepare_delegated_mode(backing, 1)).is_err());
        store
            .0
            .lock()
            .unwrap()
            .execute_batch("DROP TRIGGER ignore_delegation")
            .unwrap();
        run(store.prepare_delegated_mode(backing, 1)).unwrap();
        store.0.lock().unwrap().execute_batch("CREATE TRIGGER ignore_delegation BEFORE UPDATE ON mount_rs_metadata BEGIN SELECT RAISE(IGNORE); END").unwrap();
        assert!(
            run(store.checkout(&CheckoutRequest {
                backing,
                root: namespace().root,
                owner: "ignored".into()
            }))
            .is_err()
        );
        assert!(
            run(store.delegation_state())
                .unwrap()
                .unwrap()
                .grants
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    fn test_backing_id() -> ConcurrentBackingId {
        ConcurrentBackingId::from_bytes([0x42; 16]).unwrap()
    }

    #[cfg(unix)]
    fn prepare_bound_metadata(store: &SqliteMetadataStore) -> ConcurrentBackingId {
        let backing = test_backing_id();
        run(store.prepare_bound_concurrent_mode(backing)).unwrap();
        backing
    }

    #[cfg(unix)]
    #[test]
    fn trusted_mrc1_claim_rechecks_absent_path_and_preserves_timeline() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let volume: String = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT volume_id FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let volume = VolumeId::new(volume).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET write_mode='MRC1', fence=9223372036854775807,
                physical_dev=NULL, physical_ino=NULL, physical_path=NULL WHERE id=1",
                [],
            )
            .unwrap();
        run(store.preflight_trusted_unstamped_mrc1(0, volume.clone())).unwrap();
        // A stamp appearing after advisory preflight must never be overwritten.
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET physical_path='2f6f6c642d70617468' WHERE id=1",
                [],
            )
            .unwrap();
        assert_eq!(
            run(store.preflight_trusted_unstamped_mrc1(0, volume.clone()))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            run(store.migrate_trusted_unstamped_mrc1(test_backing_id(), 0, volume.clone()))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        let row: (String, Option<String>, String) = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT write_mode, backing_id, physical_path FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(row, ("MRC1".into(), None, "2f6f6c642d70617468".into()));
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET physical_path=NULL, revision=1 WHERE id=1",
                [],
            )
            .unwrap();
        assert_eq!(
            run(store.migrate_trusted_unstamped_mrc1(test_backing_id(), 0, volume.clone()))
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        run(store.migrate_trusted_unstamped_mrc1(test_backing_id(), 1, volume.clone())).unwrap();
        let row: (String, i64, String) = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT volume_id, revision, physical_path FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                volume.as_str().into(),
                1,
                store.0.current_auxiliary_path().unwrap()
            )
        );
        drop(store);
        assert!(SqliteMetadataStore::open(&path).is_ok());
        std::fs::remove_file(path).unwrap();
    }

    // The retired MRC1 writer API is deliberately unavailable. Seed only an
    // owned test volume to exercise the offline migration and old-client fence.
    #[cfg(unix)]
    fn seed_mrc1_metadata(
        store: &SqliteMetadataStore,
        revision: u64,
        namespace: Option<Namespace>,
    ) {
        let revision = i64::try_from(revision).unwrap();
        let namespace = namespace.map(|value| serde_json::to_string(&value).unwrap());
        assert_eq!(
            store
                .0
                .lock()
                .unwrap()
                .execute(
                    "UPDATE mount_rs_metadata
                     SET write_mode='MRC1', fence=?1, revision=?2, namespace=?3
                     WHERE id=1 AND write_mode IS NULL AND backing_id IS NULL
                       AND revision=0 AND namespace IS NULL AND owner IS NULL
                       AND fence=0 AND expires=0",
                    params![CONCURRENT_FENCE_SENTINEL, revision, namespace],
                )
                .unwrap(),
            1
        );
        assert_eq!(
            run(store.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Mrc1
        );
    }

    fn snapshot_publication(
        expected_revision: u64,
        expected_parent: Option<VersionId>,
        operation_id: &str,
        durable: bool,
    ) -> VersionPublication {
        VersionPublication {
            expected_revision,
            expected_parent,
            operation_id: PublicationId::new(operation_id).unwrap(),
            namespace: namespace(),
            block_store_id: mount_rs_core::versioning::BlockStoreId::new("blocks").unwrap(),
            kind: VersionKind::Snapshot,
            restored_from: None,
            forked_from: None,
            durable,
        }
    }

    fn reader_request() -> ReadLeaseRequest {
        ReadLeaseRequest {
            owner: "reader".to_owned(),
            ttl: Duration::from_secs(60),
        }
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_backing_identity_is_persisted_in_block_database() {
        let first_path = super::super::tests::unique_database_path();
        let second_path = super::super::tests::unique_database_path();
        let first = SqliteBlockStore::open(&first_path).unwrap();
        let reopened = SqliteBlockStore::open(&first_path).unwrap();
        let second = SqliteBlockStore::open(&second_path).unwrap();
        let id = run(first.prepare_concurrent_backing()).unwrap();
        assert_eq!(
            run(second.verify_concurrent_backing(id)).unwrap_err().code,
            ErrorCode::Estale
        );
        assert_eq!(run(reopened.prepare_concurrent_backing()).unwrap(), id);
        assert_ne!(run(second.prepare_concurrent_backing()).unwrap(), id);
        run(reopened.verify_concurrent_backing(id)).unwrap();
        assert_eq!(
            run(second.verify_concurrent_backing(id)).unwrap_err().code,
            ErrorCode::Estale
        );
        let rows: i64 = first
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(rows, 1);
        drop((first, reopened, second));
        let fresh = SqliteBlockStore::open(&first_path).unwrap();
        assert_eq!(run(fresh.prepare_concurrent_backing()).unwrap(), id);
        drop(fresh);
        std::fs::remove_file(first_path).unwrap();
        std::fs::remove_file(second_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bound_metadata_rejects_wrong_authority_before_revision_conflict() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let a = ConcurrentBackingId::from_bytes([1; 16]).unwrap();
        let b = ConcurrentBackingId::from_bytes([2; 16]).unwrap();
        run(store.prepare_bound_concurrent_mode(a)).unwrap();
        assert_eq!(
            run(store.concurrent_mode_state()).unwrap(),
            mount_rs_core::storage::ConcurrentModeState::Mrc2(a)
        );
        assert_eq!(
            run(store.prepare_bound_concurrent_mode(b))
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(
            run(store.publish_bound_if_revision(b, 99, namespace()))
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(run(store.load()).unwrap().revision, 0);
        assert_eq!(
            run(store.publish_bound_if_revision(a, 0, namespace())).unwrap(),
            1
        );
        drop(store);
        let store = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(
            run(store.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Mrc2(a)
        );
        assert_eq!(
            run(store.publish_bound_if_revision(a, 0, namespace()))
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn malformed_mrc2_marker_fails_schema_validation() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        store.0.lock().unwrap().execute(
            "UPDATE mount_rs_metadata SET write_mode='MRC2', backing_id=NULL, fence=?1 WHERE id=1",
            params![CONCURRENT_FENCE_SENTINEL]
        ).unwrap();
        drop(store);
        assert!(SqliteMetadataStore::open(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn mrc1_requires_explicit_bound_migration() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let id = ConcurrentBackingId::from_bytes([3; 16]).unwrap();
        seed_mrc1_metadata(&store, 0, None);
        let error = run(store.prepare_bound_concurrent_mode(id)).unwrap_err();
        assert_eq!(error.code, ErrorCode::Ebusy);
        assert!(error.to_string().contains("migrate-concurrent-backing"));
        assert_eq!(
            run(store.publish_bound_if_revision(id, 0, namespace()))
                .unwrap_err()
                .code,
            ErrorCode::Ebusy
        );
        run(store.migrate_mrc1_to_bound_mode(id, 0)).unwrap();
        assert_eq!(
            run(store.concurrent_mode_state()).unwrap(),
            mount_rs_core::storage::ConcurrentModeState::Mrc2(id)
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn migration_rejects_version_records_and_revision_conflict() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let id = ConcurrentBackingId::from_bytes([4; 16]).unwrap();
        seed_mrc1_metadata(&store, 1, Some(namespace()));
        assert_eq!(
            run(store.migrate_mrc1_to_bound_mode(id, 0))
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_version_state SET head_id='occupied' WHERE id=1",
                [],
            )
            .unwrap();
        assert_eq!(
            run(store.migrate_mrc1_to_bound_mode(id, 1))
                .unwrap_err()
                .code,
            ErrorCode::Ebusy
        );
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_version_state SET head_id=NULL WHERE id=1",
                [],
            )
            .unwrap();
        run(store.migrate_mrc1_to_bound_mode(id, 1)).unwrap();
        assert_eq!(run(store.load()).unwrap().revision, 1);
        assert!(run(store.load()).unwrap().namespace.is_some());
        assert_eq!(
            run(store.migrate_mrc1_to_bound_mode(id, 0))
                .unwrap_err()
                .code,
            ErrorCode::Eagain
        );
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn migration_rejects_volatile_mrc1_metadata_before_mode_change() {
        let store = SqliteMetadataStore::in_memory().unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET write_mode='MRC1', fence=?1 WHERE id=1",
                params![CONCURRENT_FENCE_SENTINEL],
            )
            .unwrap();
        let id = ConcurrentBackingId::from_bytes([5; 16]).unwrap();
        assert_eq!(
            run(store.migrate_mrc1_to_bound_mode(id, 0))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            run(store.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Mrc1
        );
    }

    #[test]
    fn malformed_authority_table_with_duplicate_id_is_rejected() {
        let path = super::super::tests::unique_database_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mount_rs_block_authority(id INTEGER, backing_id TEXT);
             INSERT INTO mount_rs_block_authority VALUES(1,'11111111111111111111111111111111');
             INSERT INTO mount_rs_block_authority VALUES(1,'22222222222222222222222222222222');",
            )
            .unwrap();
        drop(connection);
        assert!(SqliteBlockStore::open(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn authority_table_requires_id_as_sole_primary_key() {
        let path = super::super::tests::unique_database_path();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(
            "CREATE TABLE mount_rs_block_authority(
                id INTEGER CHECK(id=1),
                backing_id TEXT NOT NULL CHECK(length(backing_id)=32 AND backing_id!='00000000000000000000000000000000'),
                PRIMARY KEY(id, backing_id));"
        ).unwrap();
        drop(connection);
        assert!(SqliteBlockStore::open(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unclaimed_old_block_database_receives_physical_stamp_columns() {
        let path = super::super::tests::unique_database_path();
        let connection = Connection::open(&path).unwrap();
        connection.execute_batch(
            "CREATE TABLE mount_rs_block_authority(
                id INTEGER PRIMARY KEY CHECK(id=1),
                backing_id TEXT NOT NULL CHECK(length(backing_id)=32 AND backing_id!='00000000000000000000000000000000'));"
        ).unwrap();
        drop(connection);
        let store = SqliteBlockStore::open(&path).unwrap();
        let id = run(store.prepare_concurrent_backing()).unwrap();
        run(store.verify_concurrent_backing(id)).unwrap();
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_retarget_after_open_cannot_reuse_authority() {
        use std::os::unix::fs::symlink;
        let original_path = super::super::tests::unique_database_path();
        let copy_path = super::super::tests::unique_database_path();
        let link_path = super::super::tests::unique_database_path();
        let original = SqliteBlockStore::open(&original_path).unwrap();
        let id = run(original.prepare_concurrent_backing()).unwrap();
        drop(original);
        std::fs::copy(&original_path, &copy_path).unwrap();
        symlink(&original_path, &link_path).unwrap();
        let linked = SqliteBlockStore::open(&link_path).unwrap();
        std::fs::remove_file(&link_path).unwrap();
        symlink(&copy_path, &link_path).unwrap();
        assert!(run(linked.verify_concurrent_backing(id)).is_err());
        assert!(run(linked.prepare_concurrent_backing()).is_err());
        drop(linked);
        std::fs::remove_file(original_path).unwrap();
        std::fs::remove_file(copy_path).unwrap();
        std::fs::remove_file(link_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn idempotent_migration_rejects_corrupted_mrc2_fence() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let id = ConcurrentBackingId::from_bytes([6; 16]).unwrap();
        run(store.prepare_bound_concurrent_mode(id)).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET owner='stale', fence=0, expires=123 WHERE id=1",
                [],
            )
            .unwrap();
        assert!(run(store.migrate_mrc1_to_bound_mode(id, 0)).is_err());
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copied_block_database_cannot_reuse_authority() {
        let original_path = super::super::tests::unique_database_path();
        let copy_path = super::super::tests::unique_database_path();
        let original = SqliteBlockStore::open(&original_path).unwrap();
        run(original.prepare_concurrent_backing()).unwrap();
        drop(original);
        std::fs::copy(&original_path, &copy_path).unwrap();
        match SqliteBlockStore::open(&copy_path) {
            Err(error) => assert_eq!(error.code, ErrorCode::Estale),
            Ok(_) => panic!("copied block authority reopened"),
        }
        std::fs::remove_file(original_path).unwrap();
        std::fs::remove_file(copy_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_block_database_cannot_claim_or_verify_authority() {
        let original_path = super::super::tests::unique_database_path();
        let alias_path = super::super::tests::unique_database_path();
        let store = SqliteBlockStore::open(&original_path).unwrap();
        std::fs::hard_link(&original_path, &alias_path).unwrap();

        assert_eq!(
            run(store.prepare_concurrent_backing()).unwrap_err().code,
            ErrorCode::Enotsup
        );
        let markers: i64 = store
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(markers, 0, "rejected alias must not claim authority");

        std::fs::remove_file(&alias_path).unwrap();
        let backing = run(store.prepare_concurrent_backing()).unwrap();
        std::fs::hard_link(&original_path, &alias_path).unwrap();
        assert_eq!(
            run(store.verify_concurrent_backing(backing))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        match SqliteBlockStore::open(&alias_path) {
            Err(error) => assert_eq!(error.code, ErrorCode::Enotsup),
            Ok(_) => panic!("hard-linked block authority reopened through another pathname"),
        }

        std::fs::remove_file(&alias_path).unwrap();
        run(store.verify_concurrent_backing(backing)).unwrap();
        drop(store);
        std::fs::remove_file(original_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn hard_linked_wal_metadata_cannot_claim_or_publish_authority() {
        let original_path = super::super::tests::unique_database_path();
        let alias_path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&original_path).unwrap();
        let journal: String = store
            .0
            .lock()
            .unwrap()
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal.to_ascii_lowercase(), "wal");
        let backing = test_backing_id();
        std::fs::hard_link(&original_path, &alias_path).unwrap();

        assert_eq!(
            run(store.prepare_bound_concurrent_mode(backing))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            run(store.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Legacy
        );
        assert_eq!(run(store.load()).unwrap().revision, 0);

        std::fs::remove_file(&alias_path).unwrap();
        run(store.prepare_bound_concurrent_mode(backing)).unwrap();
        std::fs::hard_link(&original_path, &alias_path).unwrap();
        assert_eq!(
            run(store.publish_bound_if_revision(backing, 0, namespace()))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(run(store.load()).unwrap().revision, 0);
        drop(store);
        match SqliteMetadataStore::open(&original_path) {
            Err(error) => assert_eq!(error.code, ErrorCode::Enotsup),
            Ok(_) => panic!("hard-linked bound WAL database reopened"),
        }

        std::fs::remove_file(&alias_path).unwrap();
        let reopened = SqliteMetadataStore::open(&original_path).unwrap();
        assert_eq!(
            run(reopened.publish_bound_if_revision(backing, 0, namespace())).unwrap(),
            1
        );
        drop(reopened);
        std::fs::remove_file(original_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bound_metadata_rejects_another_auxiliary_path_without_mutation() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = test_backing_id();
        run(store.prepare_bound_concurrent_mode(backing)).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET physical_path=lower(hex(CAST(?1 AS BLOB))) WHERE id=1",
                params!["/private/tmp/mount-rs-bind-alias.db"],
            )
            .unwrap();

        assert_eq!(
            run(store.concurrent_mode_state()).unwrap_err().code,
            ErrorCode::Estale
        );
        assert_eq!(
            run(store.prepare_bound_concurrent_mode(backing))
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(
            run(store.publish_bound_if_revision(backing, 0, namespace()))
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        let row: (String, i64) = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT physical_path, revision FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.1, 0);
        drop(store);
        match SqliteMetadataStore::open(&path) {
            Err(error) => assert_eq!(error.code, ErrorCode::Estale),
            Ok(_) => panic!("bound metadata opened through a different auxiliary path"),
        }
        assert_eq!(
            Connection::open(&path)
                .unwrap()
                .query_row(
                    "SELECT physical_path, revision FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .unwrap(),
            row
        );
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn bound_block_authority_rejects_another_auxiliary_path_without_mutation() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteBlockStore::open(&path).unwrap();
        let backing = run(store.prepare_concurrent_backing()).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_block_authority SET physical_path=lower(hex(CAST(?1 AS BLOB))) WHERE id=1",
                params!["/private/tmp/mount-rs-bind-alias.db"],
            )
            .unwrap();

        assert_eq!(
            run(store.verify_concurrent_backing(backing))
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(
            run(store.prepare_concurrent_backing()).unwrap_err().code,
            ErrorCode::Estale
        );
        let row: (String, String) = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT backing_id, physical_path FROM mount_rs_block_authority WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        drop(store);
        match SqliteBlockStore::open(&path) {
            Err(error) => assert_eq!(error.code, ErrorCode::Estale),
            Ok(_) => panic!("block authority opened through a different auxiliary path"),
        }
        assert_eq!(
            Connection::open(&path)
                .unwrap()
                .query_row(
                    "SELECT backing_id, physical_path FROM mount_rs_block_authority WHERE id=1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .unwrap(),
            row
        );
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn unstamped_auxiliary_paths_cannot_reopen_old_bound_markers() {
        let metadata_path = super::super::tests::unique_database_path();
        let blocks_path = super::super::tests::unique_database_path();
        let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
        let blocks = SqliteBlockStore::open(&blocks_path).unwrap();
        let backing = run(blocks.prepare_concurrent_backing()).unwrap();
        run(metadata.prepare_bound_concurrent_mode(backing)).unwrap();
        metadata
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET physical_path=NULL WHERE id=1",
                [],
            )
            .unwrap();
        blocks
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_block_authority SET physical_path=NULL WHERE id=1",
                [],
            )
            .unwrap();

        assert_eq!(
            run(metadata.publish_bound_if_revision(backing, 0, namespace()))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            run(blocks.verify_concurrent_backing(backing))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        drop((metadata, blocks));
        match SqliteMetadataStore::open(&metadata_path) {
            Err(error) => assert_eq!(error.code, ErrorCode::Enotsup),
            Ok(_) => panic!("old bound metadata marker reopened without an auxiliary path"),
        }
        match SqliteBlockStore::open(&blocks_path) {
            Err(error) => assert_eq!(error.code, ErrorCode::Enotsup),
            Ok(_) => panic!("old bound block marker reopened without an auxiliary path"),
        }
        let revision: i64 = Connection::open(&metadata_path)
            .unwrap()
            .query_row(
                "SELECT revision FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 0);
        std::fs::remove_file(metadata_path).unwrap();
        std::fs::remove_file(blocks_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_resolve_to_the_same_auxiliary_path() {
        use std::os::unix::fs::symlink;

        let metadata_path = super::super::tests::unique_database_path();
        let blocks_path = super::super::tests::unique_database_path();
        let metadata_alias = super::super::tests::unique_database_path();
        let blocks_alias = super::super::tests::unique_database_path();
        let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
        let blocks = SqliteBlockStore::open(&blocks_path).unwrap();
        let backing = run(blocks.prepare_concurrent_backing()).unwrap();
        run(metadata.prepare_bound_concurrent_mode(backing)).unwrap();
        symlink(&metadata_path, &metadata_alias).unwrap();
        symlink(&blocks_path, &blocks_alias).unwrap();

        let aliased_metadata = SqliteMetadataStore::open(&metadata_alias).unwrap();
        let aliased_blocks = SqliteBlockStore::open(&blocks_alias).unwrap();
        assert_eq!(
            run(aliased_metadata.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Mrc2(backing)
        );
        run(aliased_blocks.verify_concurrent_backing(backing)).unwrap();
        run(aliased_metadata.publish_bound_if_revision(backing, 0, namespace())).unwrap();
        assert_eq!(run(metadata.load()).unwrap().revision, 1);

        drop((metadata, blocks, aliased_metadata, aliased_blocks));
        std::fs::remove_file(metadata_alias).unwrap();
        std::fs::remove_file(blocks_alias).unwrap();
        std::fs::remove_file(metadata_path).unwrap();
        std::fs::remove_file(blocks_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn auxiliary_path_identity_preserves_non_utf8_unix_bytes() {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        let mut raw = b"/private/tmp/mount-rs-non-utf8-".to_vec();
        raw.push(0xff);
        raw.extend_from_slice(b".db");
        let path = PathBuf::from(OsString::from_vec(raw.clone()));
        let encoded = encode_auxiliary_path(&path);
        assert_eq!(encoded.len(), raw.len() * 2);
        assert!(encoded.ends_with("ff2e6462"));
        assert!(!encoded.contains("efbfbd"), "non-UTF8 byte was replaced");
    }

    #[cfg(target_os = "linux")]
    fn run_linux_sqlite_worker(command: &mut std::process::Command) -> std::process::Output {
        use std::process::Stdio;

        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if child.try_wait().unwrap().is_some() {
                return child.wait_with_output().unwrap();
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let output = child.wait_with_output().unwrap();
                panic!(
                    "SQLite worker timed out: {} {}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_inspection_preserves_reserved_sqlite_locks() {
        struct OwnedFile(PathBuf);
        impl Drop for OwnedFile {
            fn drop(&mut self) {
                for suffix in ["-journal", "-wal", "-shm"] {
                    let mut sidecar = self.0.as_os_str().to_os_string();
                    sidecar.push(suffix);
                    let _ = std::fs::remove_file(PathBuf::from(sidecar));
                }
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let path = super::super::tests::unique_database_path();
        let _cleanup = OwnedFile(path.clone());
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let mut connection = metadata.0.lock().unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=DELETE; PRAGMA locking_mode=NORMAL;")
            .unwrap();
        connection.busy_timeout(Duration::ZERO).unwrap();
        let tx = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let probe = |expect_busy: bool| {
            let output = run_linux_sqlite_worker(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--ignored",
                        "--exact",
                        "storage::tests::linux_reserved_lock_probe_worker",
                        "--nocapture",
                    ])
                    .env("MOUNT_RS_SQLITE_RESERVED_LOCK_PROBE", &path)
                    .env(
                        "MOUNT_RS_SQLITE_LOCK_EXPECT_BUSY",
                        if expect_busy { "1" } else { "0" },
                    ),
            );
            assert!(
                output.status.success(),
                "reserved lock probe failed: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        probe(true);
        metadata
            .0
            .require_concurrent_local_file("metadata")
            .unwrap();
        assert!(
            !tx.is_autocommit(),
            "inspection must preserve the SQLite transaction"
        );
        probe(true);
        tx.rollback().unwrap();
        probe(false);
        drop(connection);
        drop(metadata);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "worker invoked by the owned distinct-process SQLite lock fixture"]
    fn linux_reserved_lock_probe_worker() {
        let path = std::env::var_os("MOUNT_RS_SQLITE_RESERVED_LOCK_PROBE")
            .expect("worker requires its owned fixture path");
        let connection = Connection::open(Path::new(&path)).unwrap();
        connection.busy_timeout(Duration::ZERO).unwrap();
        let result = connection.execute(
            "UPDATE mount_rs_metadata SET revision=revision WHERE id=1",
            [],
        );
        if std::env::var("MOUNT_RS_SQLITE_LOCK_EXPECT_BUSY").unwrap() == "1" {
            let error =
                result.expect_err("another process must retain its BEGIN IMMEDIATE reserved lock");
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
        } else {
            assert_eq!(
                result.expect("write must succeed after the owner rolls back"),
                1
            );
        }
    }

    #[test]
    fn linux_mount_root_attributes_fail_closed_without_rejecting_ordinary_files() {
        assert!(require_linux_mount_attributes(0, 0x2000, "blocks").is_ok());
        assert!(require_linux_mount_attributes(0x1000, 0x3000, "metadata").is_ok());
        let mounted = require_linux_mount_attributes(0x2000, 0x2000, "blocks").unwrap_err();
        assert_eq!(mounted.code, ErrorCode::Enotsup);
        assert!(mounted.to_string().contains("individually mounted"));
        for (attributes, supported) in [(0, 0), (0x2000, 0), (0, 0x1000)] {
            let unsupported =
                require_linux_mount_attributes(attributes, supported, "blocks").unwrap_err();
            assert_eq!(unsupported.code, ErrorCode::Enotsup);
            assert!(unsupported.to_string().contains("Linux 5.8"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "worker invoked by the owned active-WAL Linux file bind fixture"]
    fn linux_active_wal_bind_alias_worker() {
        let block_alias = std::env::var_os("MOUNT_RS_SQLITE_ACTIVE_WAL_BLOCK_ALIAS")
            .expect("worker requires its owned block alias");
        let metadata_alias = std::env::var_os("MOUNT_RS_SQLITE_ACTIVE_WAL_METADATA_ALIAS")
            .expect("worker requires its owned metadata alias");
        let backing = ConcurrentBackingId::from_hex(
            &std::env::var("MOUNT_RS_SQLITE_ACTIVE_WAL_BACKING").unwrap(),
        )
        .unwrap();
        let snapshot = std::env::var_os("MOUNT_RS_SQLITE_ACTIVE_WAL_SNAPSHOT")
            .expect("worker requires its owned snapshot path");
        // Main-file inspection belongs to this distinct process. Opening and
        // closing its source must not release the canonical owner's POSIX locks.
        std::fs::copy(Path::new(&block_alias), Path::new(&snapshot)).unwrap();
        let checkpointed = Connection::open_with_flags(
            Path::new(&snapshot),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let checkpointed_count: i64 = checkpointed
            .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            checkpointed_count, 0,
            "canonical authority must exist only in its active WAL"
        );
        drop(checkpointed);
        let blocks = SqliteBlockStore::open(Path::new(&block_alias)).unwrap();
        let count = || {
            blocks
                .0
                .lock()
                .unwrap()
                .query_row("SELECT count(*) FROM mount_rs_block_authority", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
        };
        assert_eq!(
            count(),
            0,
            "alias must not see the uncheckpointed canonical authority"
        );
        assert_eq!(
            run(blocks.prepare_concurrent_backing()).unwrap_err().code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            run(blocks.verify_concurrent_backing(backing))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        assert_eq!(
            count(),
            0,
            "rejected alias must not install a second authority"
        );
        let metadata = SqliteMetadataStore::open(Path::new(&metadata_alias)).unwrap();
        assert_eq!(
            run(metadata.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Legacy
        );
        assert_eq!(
            run(metadata.prepare_bound_concurrent_mode(backing))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        let row: (Option<String>, Option<String>, i64, Option<String>) = metadata
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT write_mode,backing_id,revision,namespace FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (None, None, 0, None),
            "rejected metadata alias changed protocol or namespace"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires a Linux mount namespace with CAP_SYS_ADMIN and qualified local /tmp backing"]
    fn linux_file_bind_alias_with_one_link_cannot_reuse_bound_authority() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};

        struct OwnedFile(PathBuf);
        impl Drop for OwnedFile {
            fn drop(&mut self) {
                for suffix in ["-journal", "-wal", "-shm"] {
                    let mut sidecar = self.0.as_os_str().to_os_string();
                    sidecar.push(suffix);
                    let _ = std::fs::remove_file(PathBuf::from(sidecar));
                }
                let _ = std::fs::remove_file(&self.0);
            }
        }
        struct BindMount {
            target: CString,
            mounted: bool,
        }
        impl BindMount {
            fn new(source: &Path, target: &Path) -> Self {
                let source = CString::new(source.as_os_str().as_bytes()).unwrap();
                let target = CString::new(target.as_os_str().as_bytes()).unwrap();
                assert_eq!(
                    unsafe {
                        libc::mount(
                            source.as_ptr(),
                            target.as_ptr(),
                            std::ptr::null(),
                            libc::MS_BIND as _,
                            std::ptr::null(),
                        )
                    },
                    0,
                    "bind mount failed: {}",
                    std::io::Error::last_os_error()
                );
                Self {
                    target,
                    mounted: true,
                }
            }
            fn unmount(&mut self) {
                assert_eq!(
                    unsafe { libc::umount2(self.target.as_ptr(), 0) },
                    0,
                    "bind unmount failed: {}",
                    std::io::Error::last_os_error()
                );
                self.mounted = false;
            }
        }
        impl Drop for BindMount {
            fn drop(&mut self) {
                if self.mounted {
                    let _ = unsafe { libc::umount2(self.target.as_ptr(), libc::MNT_DETACH) };
                }
            }
        }

        let metadata_path = super::super::tests::unique_database_path();
        let blocks_path = super::super::tests::unique_database_path();
        let metadata_alias = super::super::tests::unique_database_path();
        let blocks_alias = super::super::tests::unique_database_path();
        let _metadata_cleanup = OwnedFile(metadata_path.clone());
        let _blocks_cleanup = OwnedFile(blocks_path.clone());
        let _metadata_alias_cleanup = OwnedFile(metadata_alias.clone());
        let _blocks_alias_cleanup = OwnedFile(blocks_alias.clone());

        let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
        let blocks = SqliteBlockStore::open(&blocks_path).unwrap();
        let backing = run(blocks.prepare_concurrent_backing()).unwrap();
        run(metadata.prepare_bound_concurrent_mode(backing)).unwrap();
        drop((metadata, blocks));
        std::fs::File::create(&metadata_alias).unwrap();
        std::fs::File::create(&blocks_alias).unwrap();
        let mut metadata_mount = BindMount::new(&metadata_path, &metadata_alias);
        let mut blocks_mount = BindMount::new(&blocks_path, &blocks_alias);
        assert_eq!(
            FileStamp::at(&metadata_path).unwrap(),
            FileStamp::at(&metadata_alias).unwrap()
        );
        assert_eq!(
            FileStamp::at(&blocks_path).unwrap(),
            FileStamp::at(&blocks_alias).unwrap()
        );
        assert_eq!(std::fs::metadata(&metadata_alias).unwrap().nlink(), 1);
        assert_eq!(std::fs::metadata(&blocks_alias).unwrap().nlink(), 1);

        match SqliteMetadataStore::open(&metadata_alias) {
            Err(error) => assert_eq!(error.code, ErrorCode::Enotsup),
            Ok(_) => panic!("bind alias reopened bound metadata authority"),
        }
        match SqliteBlockStore::open(&blocks_alias) {
            Err(error) => assert_eq!(error.code, ErrorCode::Enotsup),
            Ok(_) => panic!("bind alias reopened bound block authority"),
        }
        blocks_mount.unmount();
        metadata_mount.unmount();

        // Keep both canonical providers open with their authority only in WAL.
        // A distinct process is required: SQLite reuses inode bookkeeping and
        // shared-memory state for connections within a single process.
        // The first case has checkpointed authority. Use new owned files for
        // the active-WAL case so the checkpointed block table starts empty.
        std::fs::remove_file(&metadata_path).unwrap();
        std::fs::remove_file(&blocks_path).unwrap();
        let metadata = SqliteMetadataStore::open(&metadata_path).unwrap();
        let blocks = SqliteBlockStore::open(&blocks_path).unwrap();
        for database in [&metadata.0, &blocks.0] {
            let connection = database.lock().unwrap();
            connection
                .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
                .unwrap();
            let checkpoint: (i64, i64, i64) = connection
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                })
                .unwrap();
            assert_eq!(checkpoint, (0, 0, 0));
        }
        let backing = run(blocks.prepare_concurrent_backing()).unwrap();
        run(metadata.prepare_bound_concurrent_mode(backing)).unwrap();
        let snapshot = super::super::tests::unique_database_path();
        let _snapshot_cleanup = OwnedFile(snapshot.clone());
        let mut metadata_mount = BindMount::new(&metadata_path, &metadata_alias);
        let mut blocks_mount = BindMount::new(&blocks_path, &blocks_alias);
        assert_eq!(std::fs::metadata(&blocks_alias).unwrap().nlink(), 1);
        let output = run_linux_sqlite_worker(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "storage::tests::linux_active_wal_bind_alias_worker",
                    "--nocapture",
                ])
                .env("MOUNT_RS_SQLITE_ACTIVE_WAL_BLOCK_ALIAS", &blocks_alias)
                .env("MOUNT_RS_SQLITE_ACTIVE_WAL_METADATA_ALIAS", &metadata_alias)
                .env("MOUNT_RS_SQLITE_ACTIVE_WAL_SNAPSHOT", &snapshot)
                .env("MOUNT_RS_SQLITE_ACTIVE_WAL_BACKING", backing.to_hex()),
        );
        assert!(
            output.status.success(),
            "active WAL alias worker failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        run(blocks.verify_concurrent_backing(backing)).unwrap();
        assert_eq!(
            run(metadata.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Mrc2(backing)
        );
        blocks_mount.unmount();
        metadata_mount.unmount();
        drop((metadata, blocks));
    }

    #[cfg(unix)]
    #[test]
    fn copied_bound_metadata_database_cannot_reuse_claim() {
        let original_path = super::super::tests::unique_database_path();
        let copy_path = super::super::tests::unique_database_path();
        let original = SqliteMetadataStore::open(&original_path).unwrap();
        let backing = test_backing_id();
        run(original.prepare_bound_concurrent_mode(backing)).unwrap();
        run(original.publish_bound_if_revision(backing, 0, namespace())).unwrap();
        drop(original);

        std::fs::copy(&original_path, &copy_path).unwrap();
        let before: (String, String, i64) = Connection::open(&copy_path)
            .unwrap()
            .query_row(
                "SELECT write_mode, backing_id, revision FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(before, (BOUND_WRITE_MODE.to_owned(), backing.to_hex(), 1));
        match SqliteMetadataStore::open(&copy_path) {
            Err(error) => assert_eq!(error.code, ErrorCode::Estale),
            Ok(_) => panic!("copied claimed metadata database opened"),
        }
        let after: (String, String, i64) = Connection::open(&copy_path)
            .unwrap()
            .query_row(
                "SELECT write_mode, backing_id, revision FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(after, before, "rejected copy changed the claimed database");
        assert!(SqliteMetadataStore::open(&original_path).is_ok());
        std::fs::remove_file(original_path).unwrap();
        std::fs::remove_file(copy_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copied_legacy_metadata_cannot_claim_the_same_block_authority() {
        let original_path = super::super::tests::unique_database_path();
        let copy_path = super::super::tests::unique_database_path();
        let original = SqliteMetadataStore::open(&original_path).unwrap();
        let volume = original.volume_id();
        drop(original);
        std::fs::copy(&original_path, &copy_path).unwrap();

        let original = SqliteMetadataStore::open(&original_path).unwrap();
        let copied = SqliteMetadataStore::open(&copy_path).unwrap();
        assert_eq!(copied.volume_id(), volume);
        let backing = test_backing_id();
        run(original.prepare_bound_concurrent_mode(backing)).unwrap();
        assert_eq!(
            run(copied.prepare_bound_concurrent_mode(backing))
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        assert_eq!(
            run(copied.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Legacy
        );
        assert_eq!(run(copied.load()).unwrap().revision, 0);
        drop((original, copied));
        std::fs::remove_file(original_path).unwrap();
        std::fs::remove_file(copy_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn historical_unstamped_legacy_metadata_stays_readable_without_concurrent_enrollment() {
        let path = super::super::tests::unique_database_path();
        // An already existing database file has no trusted creation record.
        drop(Connection::open(&path).unwrap());
        let store = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(
            run(store.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Legacy
        );
        assert_eq!(run(store.load()).unwrap().revision, 0);
        assert_eq!(
            run(store.prepare_bound_concurrent_mode(test_backing_id()))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        let row: (Option<String>, Option<String>, Option<String>, Option<String>, i64, i64) =
            store
                .0
                .lock()
                .unwrap()
                .query_row(
                    "SELECT write_mode, backing_id, physical_dev, physical_ino, revision, fence FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
                )
                .unwrap();
        assert_eq!(row, (None, None, None, None, 0, 0));
        let lease =
            run(store.acquire_writer("legacy-still-usable", Duration::from_secs(30))).unwrap();
        assert_eq!(lease.owner, "legacy-still-usable");
        drop(store);
        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(run(reopened.load()).unwrap().revision, 0);
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn historical_unstamped_mrc1_metadata_cannot_migrate_without_claiming() {
        let path = super::super::tests::unique_database_path();
        drop(Connection::open(&path).unwrap());
        let store = SqliteMetadataStore::open(&path).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET write_mode='MRC1', fence=?1 WHERE id=1",
                params![CONCURRENT_FENCE_SENTINEL],
            )
            .unwrap();
        assert_eq!(
            run(store.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Mrc1
        );
        assert_eq!(
            run(store.migrate_mrc1_to_bound_mode(test_backing_id(), 0))
                .unwrap_err()
                .code,
            ErrorCode::Enotsup
        );
        let row: (String, Option<String>, Option<String>, Option<String>, i64, i64) = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT write_mode, backing_id, physical_dev, physical_ino, revision, fence FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                CONCURRENT_WRITE_MODE.to_owned(),
                None,
                None,
                None,
                0,
                CONCURRENT_FENCE_SENTINEL
            )
        );
        drop(store);
        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(
            run(reopened.concurrent_mode_state()).unwrap(),
            ConcurrentModeState::Mrc1
        );
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn retargeted_bound_metadata_handle_cannot_publish() {
        use std::os::unix::fs::symlink;

        let original_path = super::super::tests::unique_database_path();
        let copy_path = super::super::tests::unique_database_path();
        let link_path = super::super::tests::unique_database_path();
        let original = SqliteMetadataStore::open(&original_path).unwrap();
        let backing = test_backing_id();
        run(original.prepare_bound_concurrent_mode(backing)).unwrap();
        drop(original);
        std::fs::copy(&original_path, &copy_path).unwrap();
        symlink(&original_path, &link_path).unwrap();
        let linked = SqliteMetadataStore::open(&link_path).unwrap();
        std::fs::remove_file(&link_path).unwrap();
        symlink(&copy_path, &link_path).unwrap();

        assert_eq!(
            run(linked.publish_bound_if_revision(backing, 0, namespace()))
                .unwrap_err()
                .code,
            ErrorCode::Estale
        );
        let original = SqliteMetadataStore::open(&original_path).unwrap();
        assert_eq!(run(original.load()).unwrap().revision, 0);
        drop((linked, original));
        std::fs::remove_file(original_path).unwrap();
        std::fs::remove_file(copy_path).unwrap();
        std::fs::remove_file(link_path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_publish_begin_busy_is_a_known_noncommit() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);
        store
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();

        let error = run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        assert!(store.0.lock().unwrap().is_autocommit());
        holder.execute_batch("ROLLBACK").unwrap();
        let unloaded = run(store.load()).unwrap();
        assert_eq!(unloaded.revision, 0);
        assert!(unloaded.namespace.is_none());
        assert_eq!(
            run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap(),
            1
        );

        drop(holder);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_publish_commit_busy_rolls_back_before_retry() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);
        store
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let revision: i64 = reader
            .query_row(
                "SELECT revision FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(revision, 0);

        let error = run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        assert!(store.0.lock().unwrap().is_autocommit());
        reader.execute_batch("ROLLBACK").unwrap();
        let unloaded = run(store.load()).unwrap();
        assert_eq!(unloaded.revision, 0);
        assert!(unloaded.namespace.is_none());
        assert_eq!(
            run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap(),
            1
        );

        drop(reader);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_publish_begin_busy_uses_scoped_short_timeout() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();

        let started = Instant::now();
        let error = run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "one CAS contention attempt should be shorter than the default 5s"
        );
        let timeout: i64 = store
            .0
            .lock()
            .unwrap()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000, "scoped CAS timeout must be restored");
        holder.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap(),
            1
        );

        drop(holder);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_publish_commit_busy_uses_scoped_short_timeout() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        assert_eq!(
            reader
                .query_row(
                    "SELECT revision FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| { row.get::<_, i64>(0) }
                )
                .unwrap(),
            0
        );

        let started = Instant::now();
        let error = run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "one CAS contention attempt should be shorter than the default 5s"
        );
        assert!(store.0.lock().unwrap().is_autocommit());
        let timeout: i64 = store
            .0
            .lock()
            .unwrap()
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5000, "scoped CAS timeout must be restored");
        reader.execute_batch("ROLLBACK").unwrap();
        let unloaded = run(store.load()).unwrap();
        assert_eq!(unloaded.revision, 0);
        assert!(unloaded.namespace.is_none());
        assert_eq!(
            run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap(),
            1
        );

        drop(reader);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn lease_fencing_revision_conflicts_and_expiry_fail_closed() {
        let store = SqliteMetadataStore::in_memory().unwrap();
        assert!(!store.durable());
        assert!(run(store.load()).unwrap().namespace.is_none());
        assert!(
            run(store.acquire_writer("", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Einval)
        );
        assert!(
            run(store.acquire_writer("a", Duration::ZERO))
                .unwrap_err()
                .is(ErrorCode::Einval)
        );
        let first = run(store.acquire_writer("a", Duration::from_secs(60))).unwrap();
        assert!(
            run(store.acquire_writer("a", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        assert_eq!(run(store.publish(0, &first, namespace())).unwrap(), 1);
        assert!(
            run(store.publish(0, &first, namespace()))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        let renewed = run(store.renew_writer(&first, Duration::from_secs(120))).unwrap();
        assert!(
            run(store.publish(1, &first, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert_eq!(run(store.publish(1, &renewed, namespace())).unwrap(), 2);
        // Deterministic provider-clock expiration: no timing-sensitive sleeps.
        store
            .0
            .lock()
            .unwrap()
            .execute("UPDATE mount_rs_metadata SET expires=0", [])
            .unwrap();
        assert!(
            run(store.renew_writer(&renewed, Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert!(
            run(store.publish(2, &renewed, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        let second = run(store.acquire_writer("b", Duration::from_secs(60))).unwrap();
        assert!(second.fence > renewed.fence);
        assert!(
            run(store.release_writer(&renewed))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert_eq!(run(store.publish(2, &second, namespace())).unwrap(), 3);
        run(store.release_writer(&second)).unwrap();
        assert!(
            run(store.publish(3, &second, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        let third = run(store.acquire_writer("a", Duration::from_secs(60))).unwrap();
        assert!(third.fence > second.fence);
        run(store.flush()).unwrap();
        assert_eq!(run(store.load()).unwrap().revision, 3);
    }

    #[test]
    fn acquire_writer_does_not_acknowledge_a_lease_before_rollback_journal_commit() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        {
            let connection = store.0.lock().unwrap();
            let journal: String = connection
                .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "delete");
            connection.busy_timeout(Duration::from_millis(40)).unwrap();
        }
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let initial: (Option<String>, i64, i64) = reader
            .query_row(
                "SELECT owner, fence, expires FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(initial, (None, 0, 0));

        let result = run(store.acquire_writer("ghost-owner", Duration::from_secs(30)));
        reader.execute_batch("ROLLBACK").unwrap();
        drop(reader);
        drop(store);
        let reopened = Connection::open(&path).unwrap();
        let durable: (Option<String>, i64, i64) = reopened
            .query_row(
                "SELECT owner, fence, expires FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        drop(reopened);
        std::fs::remove_file(path).unwrap();
        assert_eq!(durable, initial, "failed commit must leave lease unchanged");
        assert!(
            result.is_err(),
            "acquire acknowledged an uncommitted lease: {result:?}"
        );
    }

    #[test]
    fn renew_writer_does_not_acknowledge_an_expiry_before_rollback_journal_commit() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let lease = run(store.acquire_writer("original-owner", Duration::from_secs(30))).unwrap();
        {
            let connection = store.0.lock().unwrap();
            let journal: String = connection
                .query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "delete");
            connection.busy_timeout(Duration::from_millis(40)).unwrap();
        }
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        let initial_expiry: i64 = reader
            .query_row(
                "SELECT expires FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(initial_expiry as u64, lease.expires_at_ms);

        let result = run(store.renew_writer(&lease, Duration::from_secs(120)));
        reader.execute_batch("ROLLBACK").unwrap();
        drop(reader);
        drop(store);
        let reopened = Connection::open(&path).unwrap();
        let durable_expiry: i64 = reopened
            .query_row(
                "SELECT expires FROM mount_rs_metadata WHERE id=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        drop(reopened);
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            durable_expiry, initial_expiry,
            "failed commit must leave expiry unchanged"
        );
        assert!(
            result.is_err(),
            "renew acknowledged an uncommitted expiry: {result:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn separate_connections_share_persisted_concurrent_mode_and_cas() {
        let path = super::super::tests::unique_database_path();
        let first = SqliteMetadataStore::open(&path).unwrap();
        let second = SqliteMetadataStore::open(&path).unwrap();
        let backing = test_backing_id();

        run(first.prepare_bound_concurrent_mode(backing)).unwrap();
        run(second.prepare_bound_concurrent_mode(backing)).unwrap();
        assert!(
            run(second.acquire_writer("legacy", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Ebusy)
        );

        assert_eq!(
            run(first.publish_bound_if_revision(backing, 0, namespace())).unwrap(),
            1
        );
        assert!(
            run(second.publish_bound_if_revision(backing, 0, namespace()))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        assert_eq!(run(second.load()).unwrap().revision, 1);
        assert_eq!(
            run(second.publish_bound_if_revision(backing, 1, namespace())).unwrap(),
            2
        );
        drop(first);
        drop(second);

        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(run(reopened.load()).unwrap().revision, 2);
        run(reopened.prepare_bound_concurrent_mode(backing)).unwrap();
        let connection = reopened.0.lock().unwrap();
        let (mode, fence): (Option<String>, i64) = connection
            .query_row(
                "SELECT write_mode, fence FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(mode.as_deref(), Some("MRC2"));
        assert_eq!(fence, i64::MAX);
        assert_eq!(
            connection
                .execute(
                    "UPDATE mount_rs_metadata SET owner='old', fence=fence+1
                     WHERE id=1 AND owner IS NULL AND fence<9223372036854775807",
                    [],
                )
                .unwrap(),
            0,
            "the old acquisition query must stay fenced"
        );
        drop(connection);
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_mode_rejects_non_durable_and_historically_fenced_volumes() {
        let memory = SqliteMetadataStore::in_memory().unwrap();
        assert!(
            run(memory.prepare_bound_concurrent_mode(test_backing_id()))
                .unwrap_err()
                .is(ErrorCode::Enotsup)
        );

        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let lease = run(store.acquire_writer("old", Duration::from_secs(60))).unwrap();
        run(store.release_writer(&lease)).unwrap();
        assert!(
            run(store.prepare_bound_concurrent_mode(test_backing_id()))
                .unwrap_err()
                .is(ErrorCode::Ebusy)
        );
        let (mode, fence): (Option<String>, i64) = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT write_mode, fence FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(mode.is_none());
        assert_eq!(fence, lease.fence as i64);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_publication_fails_closed_on_corrupt_fence_and_sqlite_io_error() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let backing = prepare_bound_metadata(&store);

        store
            .0
            .lock()
            .unwrap()
            .execute("UPDATE mount_rs_metadata SET fence=0 WHERE id=1", [])
            .unwrap();
        assert!(
            run(store.publish_bound_if_revision(backing, 0, namespace()))
                .unwrap_err()
                .is(ErrorCode::Enotsup)
        );
        assert_eq!(run(store.load()).unwrap().revision, 0);
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET fence=9223372036854775807 WHERE id=1",
                [],
            )
            .unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute_batch("PRAGMA query_only=ON")
            .unwrap();
        let error = run(store.publish_bound_if_revision(backing, 0, namespace())).unwrap_err();
        assert!(!error.is(ErrorCode::Eagain));
        assert_eq!(run(store.load()).unwrap().revision, 0);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn legacy_version_mutations_reject_a_concurrent_mode_marker() {
        let store = SqliteMetadataStore::in_memory().unwrap();
        let lease = run(store.acquire_writer("old", Duration::from_secs(60))).unwrap();
        let first = run(store.publish_version(
            &lease,
            VersionPublication {
                expected_revision: 0,
                expected_parent: None,
                operation_id: PublicationId::new("first-old-version").unwrap(),
                namespace: namespace(),
                block_store_id: mount_rs_core::versioning::BlockStoreId::new("blocks").unwrap(),
                kind: VersionKind::Snapshot,
                restored_from: None,
                forked_from: None,
                durable: false,
            },
        ))
        .unwrap();
        let second = run(store.publish_version(
            &lease,
            VersionPublication {
                expected_revision: 1,
                expected_parent: Some(first.id.clone()),
                operation_id: PublicationId::new("second-old-version").unwrap(),
                namespace: namespace(),
                block_store_id: mount_rs_core::versioning::BlockStoreId::new("blocks").unwrap(),
                kind: VersionKind::Snapshot,
                restored_from: None,
                forked_from: None,
                durable: false,
            },
        ))
        .unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_metadata SET write_mode='MRC1' WHERE id=1",
                [],
            )
            .unwrap();

        assert!(
            run(store.delete_version(&lease, &first.id))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert!(
            run(store.publish_version(
                &lease,
                VersionPublication {
                    expected_revision: 2,
                    expected_parent: Some(second.id),
                    operation_id: PublicationId::new("third-old-version").unwrap(),
                    namespace: namespace(),
                    block_store_id: mount_rs_core::versioning::BlockStoreId::new("blocks").unwrap(),
                    kind: VersionKind::Snapshot,
                    restored_from: None,
                    forked_from: None,
                    durable: false,
                },
            ))
            .unwrap_err()
            .is(ErrorCode::Estale)
        );
        assert_eq!(run(store.load()).unwrap().revision, 2);
    }

    #[cfg(unix)]
    #[test]
    fn legacy_acquire_and_mode_conversion_race_without_dual_authority() {
        use std::sync::{Arc, Barrier};

        for journal in ["DELETE", "WAL"] {
            for _ in 0..8 {
                let path = super::super::tests::unique_database_path();
                let old = SqliteMetadataStore::open(&path).unwrap();
                old.0
                    .lock()
                    .unwrap()
                    .execute_batch(&format!("PRAGMA journal_mode={journal};"))
                    .unwrap();
                let concurrent = SqliteMetadataStore::open(&path).unwrap();
                let backing = test_backing_id();
                let start = Arc::new(Barrier::new(3));
                let old_start = Arc::clone(&start);
                let concurrent_start = Arc::clone(&start);
                let old_task = std::thread::spawn(move || {
                    old_start.wait();
                    run(old.acquire_writer("old", Duration::from_secs(60)))
                });
                let concurrent_task = std::thread::spawn(move || {
                    concurrent_start.wait();
                    run(concurrent.prepare_bound_concurrent_mode(backing))
                });
                start.wait();
                let old_result = old_task.join().unwrap();
                let concurrent_result = concurrent_task.join().unwrap();
                let reopened = SqliteMetadataStore::open(&path).unwrap();
                match (old_result, concurrent_result) {
                    (Ok(lease), Err(error)) if error.is(ErrorCode::Ebusy) => {
                        assert_eq!(
                            run(reopened.acquire_writer("new", Duration::from_secs(60)))
                                .unwrap_err()
                                .code,
                            ErrorCode::Eagain
                        );
                        run(reopened.release_writer(&lease)).unwrap();
                        assert!(
                            run(reopened.prepare_bound_concurrent_mode(backing))
                                .unwrap_err()
                                .is(ErrorCode::Ebusy)
                        );
                    }
                    (Err(error), Ok(())) if error.is(ErrorCode::Ebusy) => {
                        assert_eq!(
                            run(reopened.publish_bound_if_revision(backing, 0, namespace()))
                                .unwrap(),
                            1
                        );
                    }
                    (old_result, concurrent_result) => panic!(
                        "journal {journal}: both modes must not win or both fail: {old_result:?}, {concurrent_result:?}"
                    ),
                }
                drop(reopened);
                std::fs::remove_file(&path).unwrap();
                for sidecar in ["-wal", "-shm"] {
                    let _ = std::fs::remove_file(format!("{}{sidecar}", path.display()));
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn ignored_mode_update_must_not_report_concurrent_authority() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER suppress_concurrent_mode
                 BEFORE UPDATE OF write_mode ON mount_rs_metadata
                 BEGIN SELECT RAISE(IGNORE); END;",
            )
            .unwrap();
        let error = run(store.prepare_bound_concurrent_mode(test_backing_id())).unwrap_err();
        assert!(!error.is(ErrorCode::Eagain));
        let (mode, fence): (Option<String>, i64) = store
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT write_mode, fence FROM mount_rs_metadata WHERE id=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert!(mode.is_none());
        assert_eq!(fence, 0);
        drop(store);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_sqlite_blocks_require_shared_file_backing() {
        let memory = SqliteBlockStore::in_memory().unwrap();
        let error = run(memory.prepare_concurrent_backing()).unwrap_err();
        assert!(error.is(ErrorCode::Enotsup));
        assert!(
            error
                .to_string()
                .contains("file-backed SQLite block database")
        );

        let path = super::super::tests::unique_database_path();
        let file = SqliteBlockStore::open(&path).unwrap();
        #[cfg(unix)]
        {
            let id = run(file.prepare_concurrent_backing()).unwrap();
            run(file.verify_concurrent_backing(id)).unwrap();
        }
        #[cfg(not(unix))]
        assert!(
            run(file.prepare_concurrent_backing())
                .unwrap_err()
                .is(ErrorCode::Enotsup)
        );
        drop(file);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn block_put_does_not_acknowledge_a_returning_row_before_rollback_journal_commit() {
        let path = super::super::tests::unique_database_path();
        let blocks = SqliteBlockStore::open(&path).unwrap();
        {
            let connection = blocks.0.lock().unwrap();
            connection
                .execute_batch("PRAGMA journal_mode=DELETE;")
                .unwrap();
            connection.busy_timeout(Duration::from_millis(40)).unwrap();
            let journal: String = connection
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            assert_eq!(journal, "delete");
        }

        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN;").unwrap();
        let count: i64 = reader
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0, "reader must hold a SHARED snapshot before put");

        let result = futures_lite::future::block_on(
            blocks.put(b"commit must precede block acknowledgement"),
        );
        let reader_count: i64 = reader
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        reader.execute_batch("ROLLBACK;").unwrap();
        drop(reader);
        drop(blocks);

        let reopened = SqliteBlockStore::open(&path).unwrap();
        let committed_count: i64 = reopened
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        drop(reopened);
        std::fs::remove_file(&path).unwrap();
        assert_eq!(reader_count, 0, "locked reader cannot see the insert");
        assert_eq!(
            committed_count, 0,
            "failed commit must not publish the insert"
        );
        assert!(
            result.is_err(),
            "put acknowledged an uncommitted RETURNING row: {result:?}"
        );
    }

    #[test]
    fn block_put_retries_busy_begin_after_writer_releases() {
        let path = super::super::tests::unique_database_path();
        let blocks = SqliteBlockStore::open(&path).unwrap();
        blocks
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            holder.execute_batch("ROLLBACK").unwrap();
        });

        let id = futures_lite::future::block_on(blocks.put(b"begin-busy-retry"))
            .expect("transient writer lock must permit one acknowledged block");
        release.join().unwrap();
        let reopened = SqliteBlockStore::open(&path).unwrap();
        assert_eq!(run(reopened.get(&id)).unwrap(), b"begin-busy-retry");
        drop(reopened);
        drop(blocks);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn block_put_retries_busy_commit_after_reader_releases() {
        let path = super::super::tests::unique_database_path();
        let blocks = SqliteBlockStore::open(&path).unwrap();
        blocks
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN").unwrap();
        assert_eq!(
            reader
                .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            reader.execute_batch("ROLLBACK").unwrap();
        });

        let id = futures_lite::future::block_on(blocks.put(b"commit-busy-retry"))
            .expect("transient reader lock must permit one acknowledged block");
        release.join().unwrap();
        let reopened = SqliteBlockStore::open(&path).unwrap();
        assert_eq!(run(reopened.get(&id)).unwrap(), b"commit-busy-retry");
        let durable_count: i64 = reopened
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            durable_count, 1,
            "a rolled-back attempt must not leave a duplicate block"
        );
        drop(reopened);
        drop(blocks);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn block_put_bounds_persistent_writer_lock_without_acknowledgement() {
        let path = super::super::tests::unique_database_path();
        let blocks = SqliteBlockStore::open(&path).unwrap();
        blocks
            .0
            .lock()
            .unwrap()
            .busy_timeout(Duration::from_millis(40))
            .unwrap();
        let holder = Connection::open(&path).unwrap();
        holder.execute_batch("BEGIN IMMEDIATE").unwrap();

        let started = Instant::now();
        let error = futures_lite::future::block_on(blocks.put(b"never-acknowledged")).unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain, "{error:?}");
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_secs(15),
            "short SQLite busy timeout must bound retries; elapsed_ms={}",
            elapsed.as_millis()
        );
        assert!(blocks.0.lock().unwrap().is_autocommit());
        holder.execute_batch("ROLLBACK").unwrap();
        let reopened = SqliteBlockStore::open(&path).unwrap();
        let count: i64 = reopened
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM mount_rs_blocks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);
        drop(reopened);
        drop(holder);
        drop(blocks);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn block_ids_are_immutable_and_separate_from_metadata() {
        let blocks = SqliteBlockStore::in_memory().unwrap();
        assert!(!blocks.durable());
        let a = run(blocks.put(&[0, 255, 1, 2])).unwrap();
        let b = run(blocks.put(&[9])).unwrap();
        assert_ne!(a, b);
        assert_eq!(run(blocks.get(&a)).unwrap(), [0, 255, 1, 2]);
        assert_eq!(run(blocks.get(&b)).unwrap(), [9]);
        run(blocks.flush()).unwrap();
        run(blocks.delete(&a)).unwrap();
        assert!(run(blocks.get(&a)).unwrap_err().is(ErrorCode::Enoent));
        assert_eq!(run(blocks.get(&b)).unwrap(), [9]);
        let other = SqliteBlockStore::in_memory().unwrap();
        assert!(run(other.get(&b)).unwrap_err().is(ErrorCode::Enoent));
        let tables: u64 = blocks
            .0
            .lock()
            .unwrap()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name='mount_rs_metadata'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
    }

    #[test]
    fn version_schema_migrates_legacy_six_column_metadata_and_reopens_idempotently() {
        let path = super::super::tests::unique_database_path();
        let legacy_namespace = serde_json::to_string(&namespace()).unwrap();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mount_rs_metadata (
                     id INTEGER PRIMARY KEY CHECK(id=1),
                     revision INTEGER NOT NULL CHECK(revision>=0),
                     namespace TEXT,
                     owner TEXT,
                     fence INTEGER NOT NULL CHECK(fence>=0),
                     expires INTEGER NOT NULL
                 );
                 INSERT INTO mount_rs_metadata
                 (id, revision, namespace, owner, fence, expires)
                 VALUES (1, 7, NULL, NULL, 0, 0);",
            )
            .unwrap();
        connection
            .execute(
                "UPDATE mount_rs_metadata SET namespace=?1 WHERE id=1",
                params![legacy_namespace],
            )
            .unwrap();
        drop(connection);

        let store = SqliteMetadataStore::open(&path).unwrap();
        let volume = store.volume_id();
        let loaded = run(store.load()).unwrap();
        assert_eq!(loaded.revision, 7);
        assert!(loaded.namespace.is_some());
        drop(store);

        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(reopened.volume_id(), volume);
        let connection = reopened.0.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT schema_version FROM mount_rs_schema_versions
                     WHERE schema_name=?1",
                    params![VERSION_SCHEMA_NAME],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            VERSION_SCHEMA_BASE_VERSION
        );
        assert!(
            connection
                .query_row(
                    "SELECT write_mode FROM mount_rs_metadata WHERE id=1",
                    [],
                    |row| { row.get::<_, Option<String>>(0) }
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM mount_rs_version_state WHERE id=1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        drop(connection);
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn version_one_schema_stays_readable_until_namespace_publication() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        let volume = store.volume_id();
        let lease = run(store.acquire_writer("schema-upgrade", Duration::from_secs(60))).unwrap();
        let first = run(store.publish_version(
            &lease,
            snapshot_publication(0, None, "before-upgrade", false),
        ))
        .unwrap();
        run(store.release_writer(&lease)).unwrap();
        drop(store);

        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=?1",
                    params![VERSION_SCHEMA_NAME],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            VERSION_SCHEMA_BASE_VERSION
        );
        drop(connection);

        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(reopened.volume_id(), volume);
        assert_eq!(
            run(reopened.version_head()).unwrap().unwrap().version,
            first.id
        );
        assert_eq!(run(reopened.list_versions()).unwrap().len(), 1);
        assert_eq!(
            reopened
                .0
                .lock()
                .unwrap()
                .query_row(
                    "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=?1",
                    params![VERSION_SCHEMA_NAME],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            VERSION_SCHEMA_BASE_VERSION
        );
        let lease =
            run(reopened.acquire_writer("schema-upgrade", Duration::from_secs(60))).unwrap();
        let mut next = snapshot_publication(1, Some(first.id), "after-upgrade", false);
        next.kind = VersionKind::NamespacePublication;
        reopened
            .0
            .lock()
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_namespace_activation
                 BEFORE UPDATE OF revision ON mount_rs_metadata
                 BEGIN SELECT RAISE(ABORT, 'injected activation failure'); END;",
            )
            .unwrap();
        assert!(
            run(reopened.publish_version(&lease, next.clone()))
                .unwrap_err()
                .is(ErrorCode::Eio)
        );
        {
            let connection = reopened.0.lock().unwrap();
            assert_eq!(
                connection
                    .query_row(
                        "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=?1",
                        params![VERSION_SCHEMA_NAME],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                VERSION_SCHEMA_BASE_VERSION
            );
            assert_eq!(
                connection
                    .query_row(
                        "SELECT count(*) FROM mount_rs_versions WHERE operation_id='after-upgrade'",
                        [],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap(),
                0
            );
            connection
                .execute_batch("DROP TRIGGER fail_namespace_activation")
                .unwrap();
        }
        let second = run(reopened.publish_version(&lease, next)).unwrap();
        run(reopened.release_writer(&lease)).unwrap();
        let connection = reopened.0.lock().unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT schema_version FROM mount_rs_schema_versions WHERE schema_name=?1",
                    params![VERSION_SCHEMA_NAME],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            VERSION_SCHEMA_VERSION
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT kind FROM mount_rs_versions WHERE id=?1",
                    params![second.id.encode()],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "namespace-publication"
        );
        drop(connection);
        drop(reopened);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn incompatible_version_schema_is_rejected_without_partial_migration() {
        let path = super::super::tests::unique_database_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mount_rs_metadata (
                     id INTEGER PRIMARY KEY CHECK(id=1),
                     revision INTEGER NOT NULL CHECK(revision>=0),
                     namespace TEXT,
                     owner TEXT,
                     fence INTEGER NOT NULL CHECK(fence>=0),
                     expires INTEGER NOT NULL
                 );
                 INSERT INTO mount_rs_metadata
                 (id, revision, namespace, owner, fence, expires)
                 VALUES (1, 0, NULL, NULL, 0, 0);
                 CREATE TABLE mount_rs_versions (id TEXT PRIMARY KEY NOT NULL);",
            )
            .unwrap();
        drop(connection);

        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("incompatible version schema must be rejected"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));

        let connection = Connection::open(&path).unwrap();
        let metadata_has_volume: bool = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM pragma_table_info('mount_rs_metadata')
                     WHERE name='volume_id'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!metadata_has_volume);
        let version_table_columns: i64 = connection
            .query_row(
                "SELECT count(*) FROM pragma_table_info('mount_rs_versions')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version_table_columns, 1);
        let schema_version_table_exists: bool = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM sqlite_master
                     WHERE type='table' AND name='mount_rs_schema_versions'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!schema_version_table_exists);
        drop(connection);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn version_schema_rejects_missing_operation_id_uniqueness_without_partial_migration() {
        let path = super::super::tests::unique_database_path();
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE mount_rs_metadata (
                     id INTEGER PRIMARY KEY CHECK(id=1),
                     revision INTEGER NOT NULL CHECK(revision>=0),
                     namespace TEXT,
                     owner TEXT,
                     fence INTEGER NOT NULL CHECK(fence>=0),
                     expires INTEGER NOT NULL
                 );
                 INSERT INTO mount_rs_metadata
                 (id, revision, namespace, owner, fence, expires)
                 VALUES (1, 0, NULL, NULL, 0, 0);
                 CREATE TABLE mount_rs_versions (
                     id TEXT PRIMARY KEY NOT NULL,
                     volume_id TEXT NOT NULL,
                     sequence INTEGER NOT NULL,
                     parent_id TEXT,
                     restored_from TEXT,
                     forked_from TEXT,
                     namespace TEXT NOT NULL,
                     block_store_id TEXT NOT NULL,
                     kind TEXT NOT NULL,
                     created_at_ms INTEGER NOT NULL,
                     durable INTEGER NOT NULL,
                     operation_id TEXT NOT NULL
                 );",
            )
            .unwrap();
        drop(connection);

        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("missing operation uniqueness must be rejected"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));

        let connection = Connection::open(&path).unwrap();
        let metadata_has_volume: bool = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM pragma_table_info('mount_rs_metadata')
                     WHERE name='volume_id'
                 )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!metadata_has_volume);
        for table in [
            SCHEMA_VERSION_TABLE,
            "mount_rs_version_state",
            "mount_rs_version_pins",
        ] {
            let exists: bool = connection
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1
                     )",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert!(!exists, "migration left table {table}");
        }
        drop(connection);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn future_version_schema_is_rejected_and_preserved() {
        let path = super::super::tests::unique_database_path();
        let store = SqliteMetadataStore::open(&path).unwrap();
        store
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_schema_versions SET schema_version=99
                 WHERE schema_name=?1",
                params![VERSION_SCHEMA_NAME],
            )
            .unwrap();
        drop(store);

        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("future version schema must be rejected"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));
        let connection = Connection::open(&path).unwrap();
        assert_eq!(
            connection
                .query_row(
                    "SELECT schema_version FROM mount_rs_schema_versions
                     WHERE schema_name=?1",
                    params![VERSION_SCHEMA_NAME],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            99
        );
        drop(connection);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn version_head_and_read_lease_validate_persisted_identities() {
        let metadata = SqliteMetadataStore::in_memory().unwrap();
        let writer =
            run(metadata.acquire_writer("version-writer", Duration::from_secs(60))).unwrap();
        let publication = VersionPublication {
            expected_revision: 0,
            expected_parent: None,
            operation_id: PublicationId::new("sqlite-integrity").unwrap(),
            namespace: namespace(),
            block_store_id: mount_rs_core::versioning::BlockStoreId::new("sqlite-blocks").unwrap(),
            kind: VersionKind::Snapshot,
            restored_from: None,
            forked_from: None,
            durable: false,
        };
        let version = run(metadata.publish_version(&writer, publication)).unwrap();
        let pin = run(metadata.open_view_pin(
            &version.id,
            ReadLeaseRequest {
                owner: "reader".to_owned(),
                ttl: Duration::from_secs(60),
            },
        ))
        .unwrap();
        let forged = ReadLease {
            owner: "forged".to_owned(),
            ..pin.clone()
        };
        assert!(
            run(metadata.renew_view_pin(
                &forged,
                ReadLeaseRequest {
                    owner: pin.owner.clone(),
                    ttl: Duration::from_secs(60),
                },
            ))
            .unwrap_err()
            .is(ErrorCode::Estale)
        );

        metadata
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_version_state SET head_id=?1 WHERE id=1",
                params![format!("{}:999", metadata.volume_id())],
            )
            .unwrap();
        assert!(run(metadata.version_head()).unwrap_err().is(ErrorCode::Eio));
    }

    #[test]
    fn version_head_rejects_a_record_whose_sequence_disagrees_with_its_id() {
        let path = super::super::tests::unique_database_path();
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let writer = run(metadata.acquire_writer("head-writer", Duration::from_secs(60))).unwrap();
        let version = run(metadata.publish_version(
            &writer,
            snapshot_publication(0, None, "head-identity", true),
        ))
        .unwrap();
        assert_eq!(
            run(metadata.version_head()).unwrap().unwrap().version,
            version.id
        );
        metadata
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_versions SET sequence=sequence+1 WHERE id=?1",
                params![version.id.encode()],
            )
            .unwrap();

        assert!(run(metadata.version_head()).unwrap_err().is(ErrorCode::Eio));
        drop(metadata);
        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("a head with mismatched row identity must be rejected during open"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn publication_rejects_a_head_with_mismatched_row_identity() {
        let metadata = SqliteMetadataStore::in_memory().unwrap();
        let writer = run(metadata.acquire_writer("head-writer", Duration::from_secs(60))).unwrap();
        let version =
            run(metadata
                .publish_version(&writer, snapshot_publication(0, None, "first-head", false)))
            .unwrap();
        metadata
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_versions SET sequence=sequence+1 WHERE id=?1",
                params![version.id.encode()],
            )
            .unwrap();
        let result = run(metadata.publish_version(
            &writer,
            snapshot_publication(1, Some(version.id.clone()), "second-head", false),
        ));
        assert!(result.unwrap_err().is(ErrorCode::Eio));
        assert_eq!(run(metadata.load()).unwrap().revision, 1);
    }

    #[test]
    fn head_lookup_and_publication_reject_an_unloadable_head_record() {
        let path = super::super::tests::unique_database_path();
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let writer = run(metadata.acquire_writer("writer", Duration::from_secs(60))).unwrap();
        let first =
            run(metadata
                .publish_version(&writer, snapshot_publication(0, None, "valid-head", true)))
            .unwrap();
        metadata
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_versions SET namespace='not-json' WHERE id=?1",
                params![first.id.encode()],
            )
            .unwrap();
        assert!(run(metadata.version_head()).unwrap_err().is(ErrorCode::Eio));
        assert!(
            run(metadata.publish_version(
                &writer,
                snapshot_publication(1, Some(first.id), "child-of-corrupt-head", true),
            ))
            .unwrap_err()
            .is(ErrorCode::Eio)
        );
        assert_eq!(run(metadata.load()).unwrap().revision, 1);
        drop(metadata);
        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("an unloadable version head must be rejected during open"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn open_pin_rejects_an_unloadable_historical_record_without_a_pin() {
        let metadata = SqliteMetadataStore::in_memory().unwrap();
        let writer = run(metadata.acquire_writer("writer", Duration::from_secs(60))).unwrap();
        let first = run(metadata.publish_version(
            &writer,
            snapshot_publication(0, None, "historical-first", false),
        ))
        .unwrap();
        run(metadata.publish_version(
            &writer,
            snapshot_publication(1, Some(first.id.clone()), "historical-second", false),
        ))
        .unwrap();
        metadata
            .0
            .lock()
            .unwrap()
            .execute(
                "UPDATE mount_rs_versions SET namespace='not-json' WHERE id=?1",
                params![first.id.encode()],
            )
            .unwrap();
        assert!(
            run(metadata.open_view_pin(&first.id, reader_request()))
                .unwrap_err()
                .is(ErrorCode::Eio)
        );
        let pins: i64 = metadata
            .0
            .lock()
            .unwrap()
            .query_row("SELECT count(*) FROM mount_rs_version_pins", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(pins, 0);
        run(metadata.delete_version(&writer, &first.id)).unwrap();
    }

    #[test]
    fn read_lease_rejects_a_forged_volume_on_renew_and_close() {
        let metadata = SqliteMetadataStore::in_memory().unwrap();
        let writer = run(metadata.acquire_writer("writer", Duration::from_secs(60))).unwrap();
        let version =
            run(metadata
                .publish_version(&writer, snapshot_publication(0, None, "pin-volume", false)))
            .unwrap();
        let pin = run(metadata.open_view_pin(&version.id, reader_request())).unwrap();
        let forged = ReadLease {
            volume: VolumeId::new("other-volume").unwrap(),
            ..pin.clone()
        };
        assert!(
            run(metadata.renew_view_pin(&forged, reader_request()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert!(
            run(metadata.close_view_pin(&forged))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        run(metadata.close_view_pin(&pin)).unwrap();
        run(metadata.close_view_pin(&forged)).unwrap();
    }

    #[test]
    fn concurrent_pin_and_delete_have_only_serial_outcomes_across_connections() {
        let path = super::super::tests::unique_database_path();
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let writer = run(metadata.acquire_writer("writer", Duration::from_secs(60))).unwrap();
        let first = run(metadata.publish_version(
            &writer,
            snapshot_publication(0, None, "first-version", true),
        ))
        .unwrap();
        let second = run(metadata.publish_version(
            &writer,
            snapshot_publication(1, Some(first.id.clone()), "second-version", true),
        ))
        .unwrap();
        let third = run(metadata.publish_version(
            &writer,
            snapshot_publication(2, Some(second.id.clone()), "third-version", true),
        ))
        .unwrap();
        assert_eq!(
            run(metadata.version_head()).unwrap(),
            Some(VersionHead {
                version: third.id.clone(),
                revision: 3,
            })
        );

        let pin_store = SqliteMetadataStore::open(&path).unwrap();
        let delete_store = SqliteMetadataStore::open(&path).unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let pin_barrier = Arc::clone(&barrier);
        let pin_id = first.id.clone();
        let pin_thread = thread::spawn(move || {
            pin_barrier.wait();
            run(pin_store.open_view_pin(&pin_id, reader_request()))
        });
        let delete_barrier = Arc::clone(&barrier);
        let delete_id = first.id.clone();
        let delete_writer = writer.clone();
        let delete_thread = thread::spawn(move || {
            delete_barrier.wait();
            run(delete_store.delete_version(&delete_writer, &delete_id))
        });
        barrier.wait();
        let pin_result = pin_thread.join().unwrap();
        let delete_result = delete_thread.join().unwrap();
        match (pin_result, delete_result) {
            (Ok(pin), Err(error)) if error.is(ErrorCode::Ebusy) => {
                assert_eq!(run(metadata.load_version(&first.id)).unwrap().id, first.id);
                run(metadata.close_view_pin(&pin)).unwrap();
                run(metadata.delete_version(&writer, &first.id)).unwrap();
                assert!(
                    run(metadata.open_view_pin(&first.id, reader_request()))
                        .unwrap_err()
                        .is(ErrorCode::Enoent)
                );
            }
            (Err(error), Ok(())) if error.is(ErrorCode::Enoent) => {
                assert!(
                    run(metadata.load_version(&first.id))
                        .unwrap_err()
                        .is(ErrorCode::Enoent)
                );
            }
            (pin, delete) => panic!("non-serial pin/delete result: pin={pin:?}, delete={delete:?}"),
        }
        let pin = run(metadata.open_view_pin(&second.id, reader_request())).unwrap();
        assert!(
            run(metadata.delete_version(&writer, &second.id))
                .unwrap_err()
                .is(ErrorCode::Ebusy)
        );
        run(metadata.close_view_pin(&pin)).unwrap();
        run(metadata.delete_version(&writer, &second.id)).unwrap();
        assert!(
            run(metadata.open_view_pin(&second.id, reader_request()))
                .unwrap_err()
                .is(ErrorCode::Enoent)
        );
        assert_eq!(
            run(metadata.version_head()).unwrap(),
            Some(VersionHead {
                version: third.id,
                revision: 3,
            })
        );
        drop(metadata);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn concurrent_head_read_observes_a_committed_version_revision_pair() {
        let path = super::super::tests::unique_database_path();
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let writer = run(metadata.acquire_writer("writer", Duration::from_secs(60))).unwrap();
        run(metadata.publish_version(&writer, snapshot_publication(0, None, "initial-head", true)))
            .unwrap();

        for iteration in 0..8 {
            let before = run(metadata.version_head()).unwrap().unwrap();
            let reader = SqliteMetadataStore::open(&path).unwrap();
            let barrier = Arc::new(Barrier::new(2));
            let read_barrier = Arc::clone(&barrier);
            let read_thread = thread::spawn(move || {
                read_barrier.wait();
                run(reader.version_head())
            });
            barrier.wait();
            let committed = run(metadata.publish_version(
                &writer,
                snapshot_publication(
                    before.revision,
                    Some(before.version.clone()),
                    &format!("head-race-{iteration}"),
                    true,
                ),
            ))
            .unwrap();
            let after = VersionHead {
                version: committed.id,
                revision: before.revision + 1,
            };
            let observed = read_thread.join().unwrap().unwrap();
            assert!(
                observed == Some(before) || observed == Some(after.clone()),
                "version_head returned a mixed revision/version pair: {observed:?}"
            );
            assert_eq!(run(metadata.version_head()).unwrap(), Some(after));
        }
        drop(metadata);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn opening_version_schema_rejects_a_dangling_head_atomically() {
        let path = super::super::tests::unique_database_path();
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let lease = run(metadata.acquire_writer("head-writer", Duration::from_secs(60))).unwrap();
        let publication = VersionPublication {
            expected_revision: 0,
            expected_parent: None,
            operation_id: PublicationId::new("dangling-head").unwrap(),
            namespace: namespace(),
            block_store_id: mount_rs_core::versioning::BlockStoreId::new("head-blocks").unwrap(),
            kind: VersionKind::Snapshot,
            restored_from: None,
            forked_from: None,
            durable: true,
        };
        run(metadata.publish_version(&lease, publication)).unwrap();
        drop(metadata);

        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE mount_rs_version_state SET head_id=?1 WHERE id=1",
                params!["missing-volume:999"],
            )
            .unwrap();
        drop(connection);

        let error = match SqliteMetadataStore::open(&path) {
            Ok(_) => panic!("dangling version head must be rejected during open"),
            Err(error) => error,
        };
        assert!(error.is(ErrorCode::Enotsup));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn separate_files_reopen_and_independent_connections_enforce_one_writer() {
        let path = super::super::tests::unique_database_path();
        let block_path = path.with_extension("blocks.db");
        let metadata = SqliteMetadataStore::open(&path).unwrap();
        let competitor = SqliteMetadataStore::open(&path).unwrap();
        let blocks = SqliteBlockStore::open(&block_path).unwrap();
        assert!(metadata.durable());
        assert!(blocks.durable());
        let lease = run(metadata.acquire_writer("first", Duration::from_secs(60))).unwrap();
        assert!(
            run(competitor.acquire_writer("second", Duration::from_secs(60)))
                .unwrap_err()
                .is(ErrorCode::Eagain)
        );
        let id = run(blocks.put(&[1, 2, 3, 0, 255])).unwrap();
        run(blocks.flush()).unwrap();
        assert_eq!(run(metadata.publish(0, &lease, namespace())).unwrap(), 1);
        run(metadata.flush()).unwrap();
        run(metadata.release_writer(&lease)).unwrap();
        let new_lease = run(competitor.acquire_writer("second", Duration::from_secs(60))).unwrap();
        assert!(new_lease.fence > lease.fence);
        assert!(
            run(metadata.publish(1, &lease, namespace()))
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        drop(metadata);
        drop(competitor);
        drop(blocks);
        let reopened = SqliteMetadataStore::open(&path).unwrap();
        assert_eq!(run(reopened.load()).unwrap().revision, 1);
        let reopened_blocks = SqliteBlockStore::open(&block_path).unwrap();
        assert_eq!(run(reopened_blocks.get(&id)).unwrap(), [1, 2, 3, 0, 255]);
        assert_eq!(
            reopened
                .0
                .lock()
                .unwrap()
                .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        drop(reopened);
        drop(reopened_blocks);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(block_path).unwrap();
    }
}
