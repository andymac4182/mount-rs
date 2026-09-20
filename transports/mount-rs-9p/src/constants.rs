//! 9P2000.L constants.
//!
//! Message numbers and masks are protocol values, not values taken from the
//! host's libc. In particular, the open flags carried by 9P are the Linux
//! `O_*` namespace; the session decodes them into mount-rs `OpenFlags` before
//! invoking a driver.

pub const P9_TLERROR: u8 = 6;
pub const P9_RLERROR: u8 = 7;
pub const P9_TSTATFS: u8 = 8;
pub const P9_RSTATFS: u8 = 9;
pub const P9_TLOPEN: u8 = 12;
pub const P9_RLOPEN: u8 = 13;
pub const P9_TLCREATE: u8 = 14;
pub const P9_RLCREATE: u8 = 15;
pub const P9_TSYMLINK: u8 = 16;
pub const P9_RSYMLINK: u8 = 17;
pub const P9_TMKNOD: u8 = 18;
pub const P9_RMKNOD: u8 = 19;
pub const P9_TRENAME: u8 = 20;
pub const P9_RRENAME: u8 = 21;
pub const P9_TREADLINK: u8 = 22;
pub const P9_RREADLINK: u8 = 23;
pub const P9_TGETATTR: u8 = 24;
pub const P9_RGETATTR: u8 = 25;
pub const P9_TSETATTR: u8 = 26;
pub const P9_RSETATTR: u8 = 27;
pub const P9_TXATTRWALK: u8 = 30;
pub const P9_RXATTRWALK: u8 = 31;
pub const P9_TXATTRCREATE: u8 = 32;
pub const P9_RXATTRCREATE: u8 = 33;
pub const P9_TREADDIR: u8 = 40;
pub const P9_RREADDIR: u8 = 41;
pub const P9_TFSYNC: u8 = 50;
pub const P9_RFSYNC: u8 = 51;
pub const P9_TLOCK: u8 = 52;
pub const P9_RLOCK: u8 = 53;
pub const P9_TGETLOCK: u8 = 54;
pub const P9_RGETLOCK: u8 = 55;
pub const P9_TLINK: u8 = 70;
pub const P9_RLINK: u8 = 71;
pub const P9_TMKDIR: u8 = 72;
pub const P9_RMKDIR: u8 = 73;
pub const P9_TRENAMEAT: u8 = 74;
pub const P9_RRENAMEAT: u8 = 75;
pub const P9_TUNLINKAT: u8 = 76;
pub const P9_RUNLINKAT: u8 = 77;
pub const P9_TVERSION: u8 = 100;
pub const P9_RVERSION: u8 = 101;
pub const P9_TAUTH: u8 = 102;
pub const P9_RAUTH: u8 = 103;
pub const P9_TATTACH: u8 = 104;
pub const P9_RATTACH: u8 = 105;

// Legacy 9P2000 numbers. .L keeps these numbers allocated so a server can
// refuse the four operations it replaced while still answering the common
// Tclunk/Tflush/Twalk/Tread/Twrite/Tremove messages.
pub const P9_TERROR: u8 = 106;
pub const P9_RERROR: u8 = 107;
pub const P9_TFLUSH: u8 = 108;
pub const P9_RFLUSH: u8 = 109;
pub const P9_TWALK: u8 = 110;
pub const P9_RWALK: u8 = 111;
pub const P9_TOPEN: u8 = 112;
pub const P9_ROPEN: u8 = 113;
pub const P9_TCREATE: u8 = 114;
pub const P9_RCREATE: u8 = 115;
pub const P9_TREAD: u8 = 116;
pub const P9_RREAD: u8 = 117;
pub const P9_TWRITE: u8 = 118;
pub const P9_RWRITE: u8 = 119;
pub const P9_TCLUNK: u8 = 120;
pub const P9_RCLUNK: u8 = 121;
pub const P9_TREMOVE: u8 = 122;
pub const P9_RREMOVE: u8 = 123;
pub const P9_TSTAT: u8 = 124;
pub const P9_RSTAT: u8 = 125;
pub const P9_TWSTAT: u8 = 126;
pub const P9_RWSTAT: u8 = 127;

pub const P9_GETATTR_MODE: u64 = 0x0000_0001;
pub const P9_GETATTR_NLINK: u64 = 0x0000_0002;
pub const P9_GETATTR_UID: u64 = 0x0000_0004;
pub const P9_GETATTR_GID: u64 = 0x0000_0008;
pub const P9_GETATTR_RDEV: u64 = 0x0000_0010;
pub const P9_GETATTR_ATIME: u64 = 0x0000_0020;
pub const P9_GETATTR_MTIME: u64 = 0x0000_0040;
pub const P9_GETATTR_CTIME: u64 = 0x0000_0080;
pub const P9_GETATTR_INO: u64 = 0x0000_0100;
pub const P9_GETATTR_SIZE: u64 = 0x0000_0200;
pub const P9_GETATTR_BLOCKS: u64 = 0x0000_0400;
pub const P9_GETATTR_BTIME: u64 = 0x0000_0800;
pub const P9_GETATTR_GEN: u64 = 0x0000_1000;
pub const P9_GETATTR_DATA_VERSION: u64 = 0x0000_2000;
pub const P9_GETATTR_BASIC: u64 = 0x0000_07ff;
pub const P9_GETATTR_ALL: u64 = 0x0000_3fff;

pub const P9_SETATTR_MODE: u32 = 1 << 0;
pub const P9_SETATTR_UID: u32 = 1 << 1;
pub const P9_SETATTR_GID: u32 = 1 << 2;
pub const P9_SETATTR_SIZE: u32 = 1 << 3;
pub const P9_SETATTR_ATIME: u32 = 1 << 4;
pub const P9_SETATTR_MTIME: u32 = 1 << 5;
pub const P9_SETATTR_CTIME: u32 = 1 << 6;
pub const P9_SETATTR_ATIME_SET: u32 = 1 << 7;
pub const P9_SETATTR_MTIME_SET: u32 = 1 << 8;

pub const P9_LOCK_TYPE_RDLCK: u8 = 0;
pub const P9_LOCK_TYPE_WRLCK: u8 = 1;
pub const P9_LOCK_TYPE_UNLCK: u8 = 2;
pub const P9_LOCK_SUCCESS: u8 = 0;
pub const P9_LOCK_BLOCKED: u8 = 1;
pub const P9_LOCK_ERROR: u8 = 2;
pub const P9_LOCK_GRACE: u8 = 3;
pub const P9_LOCK_FLAGS_BLOCK: u32 = 1;
pub const P9_LOCK_FLAGS_RECLAIM: u32 = 2;

pub const P9_QTDIR: u8 = 0x80;
pub const P9_QTAPPEND: u8 = 0x40;
pub const P9_QTEXCL: u8 = 0x20;
pub const P9_QTMOUNT: u8 = 0x10;
pub const P9_QTAUTH: u8 = 0x08;
pub const P9_QTTMP: u8 = 0x04;
pub const P9_QTSYMLINK: u8 = 0x02;
pub const P9_QTLINK: u8 = 0x01;
pub const P9_QTFILE: u8 = 0;

pub const P9_NOTAG: u16 = 0xffff;
pub const P9_NOFID: u32 = 0xffff_ffff;
pub const P9_MAXWELEM: usize = 16;
pub const P9_HDRSZ: usize = 7;
pub const P9_QID_SIZE: usize = 13;
pub const P9_IOHDRSZ: u32 = 24;
pub const P9_READDIRHDRSZ: u32 = 24;
pub const P9_DOTL_AT_REMOVEDIR: u32 = 0x200;
pub const P9_DEFAULT_MAX_FRAME: usize = 1024 * 1024;
pub const P9_MAX_STRING: usize = 0xffff;
pub const P9_MAX_ITEM: usize = 16 * 1024 * 1024;
pub const P9_MIN_MSIZE: u32 = 4096;
pub const V9FS_MAGIC: u32 = 0x0102_1997;
pub const P9_VERSION_DOTL: &str = "9P2000.L";
pub const P9_VERSION_UNKNOWN: &str = "unknown";

// Linux's wire namespace. These values are protocol values and deliberately
// do not come from the target operating system's libc.
pub const P9_O_ACCMODE: u32 = 0x3;
pub const P9_O_WRONLY: u32 = 0x1;
pub const P9_O_RDWR: u32 = 0x2;
pub const P9_O_CREAT: u32 = 0o100;
pub const P9_O_EXCL: u32 = 0o200;
pub const P9_O_TRUNC: u32 = 0o1000;
pub const P9_O_APPEND: u32 = 0o2000;

pub fn message_name(message_type: u8) -> String {
    let name = match message_type {
        P9_TLERROR => "Tlerror",
        P9_RLERROR => "Rlerror",
        P9_TSTATFS => "Tstatfs",
        P9_RSTATFS => "Rstatfs",
        P9_TLOPEN => "Tlopen",
        P9_RLOPEN => "Rlopen",
        P9_TLCREATE => "Tlcreate",
        P9_RLCREATE => "Rlcreate",
        P9_TSYMLINK => "Tsymlink",
        P9_RSYMLINK => "Rsymlink",
        P9_TMKNOD => "Tmknod",
        P9_RMKNOD => "Rmknod",
        P9_TRENAME => "Trename",
        P9_RRENAME => "Rrename",
        P9_TREADLINK => "Treadlink",
        P9_RREADLINK => "Rreadlink",
        P9_TGETATTR => "Tgetattr",
        P9_RGETATTR => "Rgetattr",
        P9_TSETATTR => "Tsetattr",
        P9_RSETATTR => "Rsetattr",
        P9_TXATTRWALK => "Txattrwalk",
        P9_RXATTRWALK => "Rxattrwalk",
        P9_TXATTRCREATE => "Txattrcreate",
        P9_RXATTRCREATE => "Rxattrcreate",
        P9_TREADDIR => "Treaddir",
        P9_RREADDIR => "Rreaddir",
        P9_TFSYNC => "Tfsync",
        P9_RFSYNC => "Rfsync",
        P9_TLOCK => "Tlock",
        P9_RLOCK => "Rlock",
        P9_TGETLOCK => "Tgetlock",
        P9_RGETLOCK => "Rgetlock",
        P9_TLINK => "Tlink",
        P9_RLINK => "Rlink",
        P9_TMKDIR => "Tmkdir",
        P9_RMKDIR => "Rmkdir",
        P9_TRENAMEAT => "Trenameat",
        P9_RRENAMEAT => "Rrenameat",
        P9_TUNLINKAT => "Tunlinkat",
        P9_RUNLINKAT => "Runlinkat",
        P9_TVERSION => "Tversion",
        P9_RVERSION => "Rversion",
        P9_TAUTH => "Tauth",
        P9_RAUTH => "Rauth",
        P9_TATTACH => "Tattach",
        P9_RATTACH => "Rattach",
        P9_TERROR => "Terror",
        P9_RERROR => "Rerror",
        P9_TFLUSH => "Tflush",
        P9_RFLUSH => "Rflush",
        P9_TWALK => "Twalk",
        P9_RWALK => "Rwalk",
        P9_TOPEN => "Topen",
        P9_ROPEN => "Ropen",
        P9_TCREATE => "Tcreate",
        P9_RCREATE => "Rcreate",
        P9_TREAD => "Tread",
        P9_RREAD => "Rread",
        P9_TWRITE => "Twrite",
        P9_RWRITE => "Rwrite",
        P9_TCLUNK => "Tclunk",
        P9_RCLUNK => "Rclunk",
        P9_TREMOVE => "Tremove",
        P9_RREMOVE => "Rremove",
        P9_TSTAT => "Tstat",
        P9_RSTAT => "Rstat",
        P9_TWSTAT => "Twstat",
        P9_RWSTAT => "Rwstat",
        _ => return format!("UNKNOWN({message_type})"),
    };
    name.to_owned()
}
