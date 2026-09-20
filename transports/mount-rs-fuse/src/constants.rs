//! Linux FUSE wire constants for protocol 7.41.
//!
//! Values follow the Linux `include/uapi/linux/fuse.h` layout used by the
//! pinned mountx oracle (`85361a8212ff9bff8e69f62fa8993ef2c2ec51e8`).  The
//! constants are deliberately independent of the host libc: FUSE request
//! flags are Linux wire values even when a codec is exercised on macOS.

pub const FUSE_KERNEL_VERSION: u32 = 7;
pub const FUSE_KERNEL_MINOR_VERSION: u32 = 41;
pub const FUSE_ROOT_ID: u64 = 1;
pub const FUSE_MIN_READ_BUFFER: usize = 8192;
pub const FUSE_PAGE_SIZE: usize = 4096;
pub const FUSE_MAX_MAX_PAGES: u16 = 256;
pub const FUSE_DEFAULT_MAX_PAGES_PER_REQ: u16 = 32;

// enum fuse_opcode
pub const FUSE_LOOKUP: u32 = 1;
pub const FUSE_FORGET: u32 = 2;
pub const FUSE_GETATTR: u32 = 3;
pub const FUSE_SETATTR: u32 = 4;
pub const FUSE_READLINK: u32 = 5;
pub const FUSE_SYMLINK: u32 = 6;
pub const FUSE_MKNOD: u32 = 8;
pub const FUSE_MKDIR: u32 = 9;
pub const FUSE_UNLINK: u32 = 10;
pub const FUSE_RMDIR: u32 = 11;
pub const FUSE_RENAME: u32 = 12;
pub const FUSE_LINK: u32 = 13;
pub const FUSE_OPEN: u32 = 14;
pub const FUSE_READ: u32 = 15;
pub const FUSE_WRITE: u32 = 16;
pub const FUSE_STATFS: u32 = 17;
pub const FUSE_RELEASE: u32 = 18;
pub const FUSE_FSYNC: u32 = 20;
pub const FUSE_SETXATTR: u32 = 21;
pub const FUSE_GETXATTR: u32 = 22;
pub const FUSE_LISTXATTR: u32 = 23;
pub const FUSE_REMOVEXATTR: u32 = 24;
pub const FUSE_FLUSH: u32 = 25;
pub const FUSE_INIT: u32 = 26;
pub const FUSE_OPENDIR: u32 = 27;
pub const FUSE_READDIR: u32 = 28;
pub const FUSE_RELEASEDIR: u32 = 29;
pub const FUSE_FSYNCDIR: u32 = 30;
pub const FUSE_GETLK: u32 = 31;
pub const FUSE_SETLK: u32 = 32;
pub const FUSE_SETLKW: u32 = 33;
pub const FUSE_ACCESS: u32 = 34;
pub const FUSE_CREATE: u32 = 35;
pub const FUSE_INTERRUPT: u32 = 36;
pub const FUSE_BMAP: u32 = 37;
pub const FUSE_DESTROY: u32 = 38;
pub const FUSE_IOCTL: u32 = 39;
pub const FUSE_POLL: u32 = 40;
pub const FUSE_NOTIFY_REPLY: u32 = 41;
pub const FUSE_BATCH_FORGET: u32 = 42;
pub const FUSE_FALLOCATE: u32 = 43;
pub const FUSE_READDIRPLUS: u32 = 44;
pub const FUSE_RENAME2: u32 = 45;
pub const FUSE_LSEEK: u32 = 46;
pub const FUSE_COPY_FILE_RANGE: u32 = 47;
pub const FUSE_SETUPMAPPING: u32 = 48;
pub const FUSE_REMOVEMAPPING: u32 = 49;
pub const FUSE_SYNCFS: u32 = 50;
pub const FUSE_TMPFILE: u32 = 51;
pub const FUSE_STATX: u32 = 52;
pub const CUSE_INIT: u32 = 4096;

// enum fuse_notify_code
pub const FUSE_NOTIFY_POLL: i32 = 1;
pub const FUSE_NOTIFY_INVAL_INODE: i32 = 2;
pub const FUSE_NOTIFY_INVAL_ENTRY: i32 = 3;
pub const FUSE_NOTIFY_STORE: i32 = 4;
pub const FUSE_NOTIFY_RETRIEVE: i32 = 5;
pub const FUSE_NOTIFY_DELETE: i32 = 6;
pub const FUSE_NOTIFY_RESEND: i32 = 7;
pub const FUSE_NOTIFY_UNIQUE: u64 = 0;

// fuse_setattr_in.valid
pub const FATTR_MODE: u32 = 1 << 0;
pub const FATTR_UID: u32 = 1 << 1;
pub const FATTR_GID: u32 = 1 << 2;
pub const FATTR_SIZE: u32 = 1 << 3;
pub const FATTR_ATIME: u32 = 1 << 4;
pub const FATTR_MTIME: u32 = 1 << 5;
pub const FATTR_FH: u32 = 1 << 6;
pub const FATTR_ATIME_NOW: u32 = 1 << 7;
pub const FATTR_MTIME_NOW: u32 = 1 << 8;
pub const FATTR_LOCKOWNER: u32 = 1 << 9;
pub const FATTR_CTIME: u32 = 1 << 10;
pub const FATTR_KILL_SUIDGID: u32 = 1 << 11;

// fuse_open_out.open_flags
pub const FOPEN_DIRECT_IO: u32 = 1 << 0;
pub const FOPEN_KEEP_CACHE: u32 = 1 << 1;
pub const FOPEN_NONSEEKABLE: u32 = 1 << 2;
pub const FOPEN_CACHE_DIR: u32 = 1 << 3;
pub const FOPEN_STREAM: u32 = 1 << 4;
pub const FOPEN_NOFLUSH: u32 = 1 << 5;
pub const FOPEN_PARALLEL_DIRECT_WRITES: u32 = 1 << 6;
pub const FOPEN_PASSTHROUGH: u32 = 1 << 7;

// FUSE_INIT flags. These occupy one joined 64-bit space in the public API.
pub const FUSE_ASYNC_READ: u64 = 1 << 0;
pub const FUSE_POSIX_LOCKS: u64 = 1 << 1;
pub const FUSE_FILE_OPS: u64 = 1 << 2;
pub const FUSE_ATOMIC_O_TRUNC: u64 = 1 << 3;
pub const FUSE_EXPORT_SUPPORT: u64 = 1 << 4;
pub const FUSE_BIG_WRITES: u64 = 1 << 5;
pub const FUSE_DONT_MASK: u64 = 1 << 6;
pub const FUSE_SPLICE_WRITE: u64 = 1 << 7;
pub const FUSE_SPLICE_MOVE: u64 = 1 << 8;
pub const FUSE_SPLICE_READ: u64 = 1 << 9;
pub const FUSE_FLOCK_LOCKS: u64 = 1 << 10;
pub const FUSE_HAS_IOCTL_DIR: u64 = 1 << 11;
pub const FUSE_AUTO_INVAL_DATA: u64 = 1 << 12;
pub const FUSE_DO_READDIRPLUS: u64 = 1 << 13;
pub const FUSE_READDIRPLUS_AUTO: u64 = 1 << 14;
pub const FUSE_ASYNC_DIO: u64 = 1 << 15;
pub const FUSE_WRITEBACK_CACHE: u64 = 1 << 16;
pub const FUSE_NO_OPEN_SUPPORT: u64 = 1 << 17;
pub const FUSE_PARALLEL_DIROPS: u64 = 1 << 18;
pub const FUSE_HANDLE_KILLPRIV: u64 = 1 << 19;
pub const FUSE_POSIX_ACL: u64 = 1 << 20;
pub const FUSE_ABORT_ERROR: u64 = 1 << 21;
pub const FUSE_MAX_PAGES: u64 = 1 << 22;
pub const FUSE_CACHE_SYMLINKS: u64 = 1 << 23;
pub const FUSE_NO_OPENDIR_SUPPORT: u64 = 1 << 24;
pub const FUSE_EXPLICIT_INVAL_DATA: u64 = 1 << 25;
pub const FUSE_MAP_ALIGNMENT: u64 = 1 << 26;
pub const FUSE_SUBMOUNTS: u64 = 1 << 27;
pub const FUSE_HANDLE_KILLPRIV_V2: u64 = 1 << 28;
pub const FUSE_SETXATTR_EXT: u64 = 1 << 29;
pub const FUSE_INIT_EXT: u64 = 1 << 30;
pub const FUSE_INIT_RESERVED: u64 = 1 << 31;
pub const FUSE_SECURITY_CTX: u64 = 1 << 32;
pub const FUSE_HAS_INODE_DAX: u64 = 1 << 33;
pub const FUSE_CREATE_SUPP_GROUP: u64 = 1 << 34;
pub const FUSE_HAS_EXPIRE_ONLY: u64 = 1 << 35;
pub const FUSE_DIRECT_IO_ALLOW_MMAP: u64 = 1 << 36;
pub const FUSE_PASSTHROUGH: u64 = 1 << 37;
pub const FUSE_NO_EXPORT_SUPPORT: u64 = 1 << 38;
pub const FUSE_HAS_RESEND: u64 = 1 << 39;
pub const FUSE_ALLOW_IDMAP: u64 = 1 << 40;

// Per-operation flags.
pub const FUSE_RELEASE_FLUSH: u32 = 1 << 0;
pub const FUSE_RELEASE_FLOCK_UNLOCK: u32 = 1 << 1;
pub const FUSE_GETATTR_FH: u32 = 1 << 0;
pub const FUSE_LK_FLOCK: u32 = 1 << 0;
pub const FUSE_WRITE_CACHE: u32 = 1 << 0;
pub const FUSE_WRITE_LOCKOWNER: u32 = 1 << 1;
pub const FUSE_WRITE_KILL_SUIDGID: u32 = 1 << 2;
pub const FUSE_READ_LOCKOWNER: u32 = 1 << 1;
pub const FUSE_POLL_SCHEDULE_NOTIFY: u32 = 1 << 0;
pub const FUSE_FSYNC_FDATASYNC: u32 = 1 << 0;
pub const FUSE_ATTR_SUBMOUNT: u32 = 1 << 0;
pub const FUSE_ATTR_DAX: u32 = 1 << 1;
pub const FUSE_OPEN_KILL_SUIDGID: u32 = 1 << 0;
pub const FUSE_SETXATTR_ACL_KILL_SGID: u32 = 1 << 0;
pub const FUSE_EXPIRE_ONLY: u32 = 1 << 0;
pub const FUSE_UNIQUE_RESEND: u64 = 1 << 63;
pub const FUSE_INVALID_UIDGID: u32 = u32::MAX;

pub const FUSE_MAX_NR_SECCTX: u32 = 31;
pub const FUSE_EXT_GROUPS: u32 = 32;

// POSIX d_type values.
pub const DT_UNKNOWN: u32 = 0;
pub const DT_FIFO: u32 = 1;
pub const DT_CHR: u32 = 2;
pub const DT_DIR: u32 = 4;
pub const DT_BLK: u32 = 6;
pub const DT_REG: u32 = 8;
pub const DT_LNK: u32 = 10;
pub const DT_SOCK: u32 = 12;

// Linux open(2) values as carried by fuse_open_in.flags.
pub const O_ACCMODE: u32 = 0o3;
pub const O_RDONLY: u32 = 0o0;
pub const O_WRONLY: u32 = 0o1;
pub const O_RDWR: u32 = 0o2;
pub const O_CREAT: u32 = 0o100;
pub const O_EXCL: u32 = 0o200;
pub const O_TRUNC: u32 = 0o1000;
pub const O_APPEND: u32 = 0o2000;

pub const SEEK_SET: u32 = 0;
pub const SEEK_CUR: u32 = 1;
pub const SEEK_END: u32 = 2;
pub const SEEK_DATA: u32 = 3;
pub const SEEK_HOLE: u32 = 4;

pub const F_RDLCK: u32 = 0;
pub const F_WRLCK: u32 = 1;
pub const F_UNLCK: u32 = 2;
pub const XATTR_CREATE: u32 = 1;
pub const XATTR_REPLACE: u32 = 2;

pub const FUSE_IN_HEADER_SIZE: usize = 40;
pub const FUSE_OUT_HEADER_SIZE: usize = 16;
pub const FUSE_DIRENT_HEADER_SIZE: usize = 24;
pub const FUSE_INIT_OUT_SIZE: usize = 64;
pub const FUSE_COMPAT_INIT_OUT_SIZE: usize = 8;
pub const FUSE_COMPAT_22_INIT_OUT_SIZE: usize = 24;
pub const FUSE_COMPAT_ENTRY_OUT_SIZE: usize = 120;
pub const FUSE_COMPAT_ATTR_OUT_SIZE: usize = 96;
pub const FUSE_COMPAT_STATFS_SIZE: usize = 48;
pub const FUSE_COMPAT_WRITE_IN_SIZE: usize = 24;
pub const FUSE_COMPAT_MKNOD_IN_SIZE: usize = 8;
pub const FUSE_COMPAT_SETXATTR_IN_SIZE: usize = 8;

/// Every opcode with a known request/reply wire layout in this slice.
pub const SUPPORTED_OPCODES: &[u32] = &[
    FUSE_LOOKUP,
    FUSE_FORGET,
    FUSE_GETATTR,
    FUSE_SETATTR,
    FUSE_READLINK,
    FUSE_SYMLINK,
    FUSE_MKNOD,
    FUSE_MKDIR,
    FUSE_UNLINK,
    FUSE_RMDIR,
    FUSE_RENAME,
    FUSE_LINK,
    FUSE_OPEN,
    FUSE_READ,
    FUSE_WRITE,
    FUSE_STATFS,
    FUSE_RELEASE,
    FUSE_FSYNC,
    FUSE_SETXATTR,
    FUSE_GETXATTR,
    FUSE_LISTXATTR,
    FUSE_REMOVEXATTR,
    FUSE_FLUSH,
    FUSE_INIT,
    FUSE_OPENDIR,
    FUSE_READDIR,
    FUSE_RELEASEDIR,
    FUSE_FSYNCDIR,
    FUSE_GETLK,
    FUSE_SETLK,
    FUSE_SETLKW,
    FUSE_ACCESS,
    FUSE_CREATE,
    FUSE_INTERRUPT,
    FUSE_BMAP,
    FUSE_DESTROY,
    FUSE_POLL,
    FUSE_BATCH_FORGET,
    FUSE_FALLOCATE,
    FUSE_READDIRPLUS,
    FUSE_RENAME2,
    FUSE_LSEEK,
];

/// Known wire opcodes intentionally left without a body codec or session
/// implementation. They are still safe to frame and answer with `ENOSYS`.
pub const UNIMPLEMENTED_OPCODES: &[u32] = &[
    FUSE_IOCTL,
    FUSE_NOTIFY_REPLY,
    FUSE_COPY_FILE_RANGE,
    FUSE_SETUPMAPPING,
    FUSE_REMOVEMAPPING,
    FUSE_SYNCFS,
    FUSE_TMPFILE,
    FUSE_STATX,
    CUSE_INIT,
];

/// Human-readable names for every opcode named by the pinned upstream
/// constants surface, including operations intentionally outside this slice.
/// The numeric ordering mirrors `enum fuse_opcode` and makes table-driven
/// parity checks deterministic without requiring a map dependency.
pub const OPCODE_NAMES: &[(u32, &str)] = &[
    (FUSE_LOOKUP, "LOOKUP"),
    (FUSE_FORGET, "FORGET"),
    (FUSE_GETATTR, "GETATTR"),
    (FUSE_SETATTR, "SETATTR"),
    (FUSE_READLINK, "READLINK"),
    (FUSE_SYMLINK, "SYMLINK"),
    (FUSE_MKNOD, "MKNOD"),
    (FUSE_MKDIR, "MKDIR"),
    (FUSE_UNLINK, "UNLINK"),
    (FUSE_RMDIR, "RMDIR"),
    (FUSE_RENAME, "RENAME"),
    (FUSE_LINK, "LINK"),
    (FUSE_OPEN, "OPEN"),
    (FUSE_READ, "READ"),
    (FUSE_WRITE, "WRITE"),
    (FUSE_STATFS, "STATFS"),
    (FUSE_RELEASE, "RELEASE"),
    (FUSE_FSYNC, "FSYNC"),
    (FUSE_SETXATTR, "SETXATTR"),
    (FUSE_GETXATTR, "GETXATTR"),
    (FUSE_LISTXATTR, "LISTXATTR"),
    (FUSE_REMOVEXATTR, "REMOVEXATTR"),
    (FUSE_FLUSH, "FLUSH"),
    (FUSE_INIT, "INIT"),
    (FUSE_OPENDIR, "OPENDIR"),
    (FUSE_READDIR, "READDIR"),
    (FUSE_RELEASEDIR, "RELEASEDIR"),
    (FUSE_FSYNCDIR, "FSYNCDIR"),
    (FUSE_GETLK, "GETLK"),
    (FUSE_SETLK, "SETLK"),
    (FUSE_SETLKW, "SETLKW"),
    (FUSE_ACCESS, "ACCESS"),
    (FUSE_CREATE, "CREATE"),
    (FUSE_INTERRUPT, "INTERRUPT"),
    (FUSE_BMAP, "BMAP"),
    (FUSE_DESTROY, "DESTROY"),
    (FUSE_IOCTL, "IOCTL"),
    (FUSE_POLL, "POLL"),
    (FUSE_NOTIFY_REPLY, "NOTIFY_REPLY"),
    (FUSE_BATCH_FORGET, "BATCH_FORGET"),
    (FUSE_FALLOCATE, "FALLOCATE"),
    (FUSE_READDIRPLUS, "READDIRPLUS"),
    (FUSE_RENAME2, "RENAME2"),
    (FUSE_LSEEK, "LSEEK"),
    (FUSE_COPY_FILE_RANGE, "COPY_FILE_RANGE"),
    (FUSE_SETUPMAPPING, "SETUPMAPPING"),
    (FUSE_REMOVEMAPPING, "REMOVEMAPPING"),
    (FUSE_SYNCFS, "SYNCFS"),
    (FUSE_TMPFILE, "TMPFILE"),
    (FUSE_STATX, "STATX"),
    (CUSE_INIT, "CUSE_INIT"),
];

/// A compact public opcode description, avoiding a map allocation for callers
/// that only need dispatch metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpcodeSpec {
    pub opcode: u32,
    pub name: String,
    pub has_reply: bool,
}

pub fn opcode_name(opcode: u32) -> String {
    OPCODE_NAMES
        .iter()
        .find(|(value, _)| *value == opcode)
        .map_or_else(
            || format!("UNKNOWN({opcode})"),
            |(_, name)| (*name).to_owned(),
        )
}

pub fn opcode_spec(opcode: u32) -> Option<OpcodeSpec> {
    if SUPPORTED_OPCODES.contains(&opcode) {
        Some(OpcodeSpec {
            opcode,
            name: opcode_name(opcode),
            has_reply: !matches!(opcode, FUSE_FORGET | FUSE_BATCH_FORGET),
        })
    } else {
        None
    }
}
