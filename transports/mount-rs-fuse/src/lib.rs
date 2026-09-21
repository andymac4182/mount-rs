//! FUSE protocol primitives and the Linux native mount transport.
use mount_rs_core::{ErrorCode, OpenFlags};
pub mod constants;
pub mod device;
pub mod init;
pub mod inodes;
pub mod mount;
pub mod notify;
pub mod protocol;
pub mod record;
pub mod session;

pub use constants::{
    CUSE_INIT, DT_BLK, DT_CHR, DT_DIR, DT_FIFO, DT_LNK, DT_REG, DT_SOCK, DT_UNKNOWN, FATTR_ATIME,
    FATTR_ATIME_NOW, FATTR_CTIME, FATTR_FH, FATTR_GID, FATTR_KILL_SUIDGID, FATTR_LOCKOWNER,
    FATTR_MODE, FATTR_MTIME, FATTR_MTIME_NOW, FATTR_SIZE, FATTR_UID, FOPEN_CACHE_DIR,
    FOPEN_DIRECT_IO, FOPEN_KEEP_CACHE, FOPEN_NOFLUSH, FOPEN_NONSEEKABLE,
    FOPEN_PARALLEL_DIRECT_WRITES, FOPEN_PASSTHROUGH, FOPEN_STREAM, FUSE_ACCESS, FUSE_ASYNC_DIO,
    FUSE_ASYNC_READ, FUSE_ATOMIC_O_TRUNC, FUSE_ATTR_DAX, FUSE_ATTR_SUBMOUNT, FUSE_AUTO_INVAL_DATA,
    FUSE_BATCH_FORGET, FUSE_BIG_WRITES, FUSE_BMAP, FUSE_COMPAT_22_INIT_OUT_SIZE,
    FUSE_COMPAT_ATTR_OUT_SIZE, FUSE_COMPAT_ENTRY_OUT_SIZE, FUSE_COMPAT_INIT_OUT_SIZE,
    FUSE_COMPAT_MKNOD_IN_SIZE, FUSE_COMPAT_SETXATTR_IN_SIZE, FUSE_COMPAT_STATFS_SIZE,
    FUSE_COMPAT_WRITE_IN_SIZE, FUSE_COPY_FILE_RANGE, FUSE_CREATE, FUSE_CREATE_SUPP_GROUP,
    FUSE_DEFAULT_MAX_PAGES_PER_REQ, FUSE_DESTROY, FUSE_DIRECT_IO_ALLOW_MMAP,
    FUSE_DIRENT_HEADER_SIZE, FUSE_DO_READDIRPLUS, FUSE_EXPIRE_ONLY, FUSE_EXPLICIT_INVAL_DATA,
    FUSE_EXPORT_SUPPORT, FUSE_EXT_GROUPS, FUSE_FALLOCATE, FUSE_FILE_OPS, FUSE_FLOCK_LOCKS,
    FUSE_FLUSH, FUSE_FSYNC, FUSE_FSYNC_FDATASYNC, FUSE_FSYNCDIR, FUSE_GETATTR, FUSE_GETATTR_FH,
    FUSE_GETLK, FUSE_GETXATTR, FUSE_HANDLE_KILLPRIV, FUSE_HANDLE_KILLPRIV_V2, FUSE_HAS_EXPIRE_ONLY,
    FUSE_HAS_INODE_DAX, FUSE_HAS_IOCTL_DIR, FUSE_HAS_RESEND, FUSE_IN_HEADER_SIZE, FUSE_INIT,
    FUSE_INIT_EXT, FUSE_INIT_OUT_SIZE, FUSE_INIT_RESERVED, FUSE_INVALID_UIDGID, FUSE_IOCTL,
    FUSE_KERNEL_MINOR_VERSION, FUSE_KERNEL_VERSION, FUSE_LINK, FUSE_LISTXATTR, FUSE_LK_FLOCK,
    FUSE_LOOKUP, FUSE_LSEEK, FUSE_MAP_ALIGNMENT, FUSE_MAX_MAX_PAGES, FUSE_MAX_NR_SECCTX,
    FUSE_MAX_PAGES, FUSE_MIN_READ_BUFFER, FUSE_MKDIR, FUSE_MKNOD, FUSE_NO_EXPORT_SUPPORT,
    FUSE_NO_OPEN_SUPPORT, FUSE_NO_OPENDIR_SUPPORT, FUSE_NOTIFY_DELETE, FUSE_NOTIFY_POLL,
    FUSE_NOTIFY_REPLY, FUSE_NOTIFY_RESEND, FUSE_NOTIFY_RETRIEVE, FUSE_NOTIFY_STORE, FUSE_OPEN,
    FUSE_OPEN_KILL_SUIDGID, FUSE_OPENDIR, FUSE_OUT_HEADER_SIZE, FUSE_PAGE_SIZE,
    FUSE_PARALLEL_DIROPS, FUSE_PASSTHROUGH, FUSE_POLL, FUSE_POLL_SCHEDULE_NOTIFY, FUSE_POSIX_ACL,
    FUSE_POSIX_LOCKS, FUSE_READ, FUSE_READ_LOCKOWNER, FUSE_READDIR, FUSE_READDIRPLUS,
    FUSE_READDIRPLUS_AUTO, FUSE_READLINK, FUSE_RELEASE, FUSE_RELEASE_FLOCK_UNLOCK,
    FUSE_RELEASE_FLUSH, FUSE_RELEASEDIR, FUSE_REMOVEMAPPING, FUSE_REMOVEXATTR, FUSE_RENAME,
    FUSE_RENAME2, FUSE_ROOT_ID, FUSE_SECURITY_CTX, FUSE_SETATTR, FUSE_SETLK, FUSE_SETLKW,
    FUSE_SETUPMAPPING, FUSE_SETXATTR, FUSE_SETXATTR_ACL_KILL_SGID, FUSE_SETXATTR_EXT, FUSE_STATFS,
    FUSE_STATX, FUSE_SUBMOUNTS, FUSE_SYNCFS, FUSE_TMPFILE, FUSE_UNIQUE_RESEND, FUSE_UNLINK,
    FUSE_WRITE, FUSE_WRITE_CACHE, FUSE_WRITE_KILL_SUIDGID, FUSE_WRITE_LOCKOWNER,
    FUSE_WRITEBACK_CACHE, OPCODE_NAMES,
};
pub use notify::{
    FUSE_NAME_MAX, FUSE_NOTIFY_INVAL_ENTRY, FUSE_NOTIFY_INVAL_INODE, FUSE_NOTIFY_UNIQUE,
    FuseNotification, FuseNotifyInvalEntryOut, FuseNotifyInvalInodeOut, NotifyError, decode_notify,
    decode_notify_inval_entry, decode_notify_inval_inode, encode_notify, encode_notify_inval_entry,
    encode_notify_inval_inode,
};
pub use protocol::*;
pub use record::{
    ReplayFailure, ReplayReport, TRANSCRIPT_MAGIC, TRANSCRIPT_VERSION, TranscriptDirection,
    TranscriptError, TranscriptFrame, TranscriptRecorder, decode_transcript, encode_transcript,
    replay_transcript,
};
pub use session::{FuseFlushMechanism, FuseSession, FuseSessionOptions};

pub const IN_HEADER_SIZE: usize = 40;
pub const OUT_HEADER_SIZE: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError {
    pub message: String,
    pub offset: Option<usize>,
}

impl ProtocolError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            offset: None,
        }
    }

    pub fn at(message: impl Into<String>, offset: usize) -> Self {
        Self {
            message: message.into(),
            offset: Some(offset),
        }
    }
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(offset) = self.offset {
            write!(f, "{} at byte {offset}", self.message)
        } else {
            f.write_str(&self.message)
        }
    }
}
impl std::error::Error for ProtocolError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestHeader {
    pub len: u32,
    pub opcode: u32,
    pub unique: u64,
    pub nodeid: u64,
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
    pub total_extlen: u16,
}

impl RequestHeader {
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < IN_HEADER_SIZE {
            return Err(ProtocolError::at("truncated FUSE header", bytes.len()));
        }
        let u32_at = |i| u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap());
        let u64_at = |i| u64::from_le_bytes(bytes[i..i + 8].try_into().unwrap());
        let header = Self {
            len: u32_at(0),
            opcode: u32_at(4),
            unique: u64_at(8),
            nodeid: u64_at(16),
            uid: u32_at(24),
            gid: u32_at(28),
            pid: u32_at(32),
            total_extlen: u16::from_le_bytes(bytes[36..38].try_into().unwrap()),
        };
        if header.len < IN_HEADER_SIZE as u32 {
            return Err(ProtocolError::new("invalid FUSE message length"));
        }
        Ok(header)
    }
    pub fn encode(&self) -> [u8; IN_HEADER_SIZE] {
        let mut out = [0; IN_HEADER_SIZE];
        out[0..4].copy_from_slice(&self.len.to_le_bytes());
        out[4..8].copy_from_slice(&self.opcode.to_le_bytes());
        out[8..16].copy_from_slice(&self.unique.to_le_bytes());
        out[16..24].copy_from_slice(&self.nodeid.to_le_bytes());
        out[24..28].copy_from_slice(&self.uid.to_le_bytes());
        out[28..32].copy_from_slice(&self.gid.to_le_bytes());
        out[32..36].copy_from_slice(&self.pid.to_le_bytes());
        out[36..38].copy_from_slice(&self.total_extlen.to_le_bytes());
        out
    }
}

pub struct Request<'a> {
    pub header: RequestHeader,
    pub body: &'a [u8],
    pub extensions: &'a [u8],
}
impl<'a> Request<'a> {
    /// Validate one complete message before splitting off trailing extensions.
    pub fn decode(bytes: &'a [u8], max_bytes: usize) -> Result<Self, ProtocolError> {
        let header = RequestHeader::decode(bytes)?;
        if bytes.len() > max_bytes || header.len as usize != bytes.len() {
            return Err(ProtocolError::new(
                "FUSE message length mismatch or limit exceeded",
            ));
        }
        let end = bytes
            .len()
            .checked_sub(header.total_extlen as usize * 8)
            .filter(|end| *end >= IN_HEADER_SIZE)
            .ok_or_else(|| ProtocolError::new("invalid FUSE extension length"))?;
        Ok(Self {
            header,
            body: &bytes[IN_HEADER_SIZE..end],
            extensions: &bytes[end..],
        })
    }
}

pub fn error_reply(unique: u64, code: ErrorCode) -> [u8; OUT_HEADER_SIZE] {
    let mut out = [0; OUT_HEADER_SIZE];
    out[..4].copy_from_slice(&(OUT_HEADER_SIZE as u32).to_le_bytes());
    out[4..8].copy_from_slice(&(-code.errno()).to_le_bytes());
    out[8..].copy_from_slice(&unique.to_le_bytes());
    out
}

/// Decode Linux wire bits independently of the host OS.
pub fn open_flags(wire: u32) -> OpenFlags {
    OpenFlags::from_bits(wire as u64)
}
pub fn reopen_flags(flags: OpenFlags) -> OpenFlags {
    OpenFlags {
        create: false,
        exclusive: false,
        truncate: false,
        ..flags
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn framing_bounds_and_extensions() {
        let header = RequestHeader {
            len: 52,
            opcode: 15,
            unique: u64::MAX,
            nodeid: 2,
            uid: 501,
            gid: 20,
            pid: 42,
            total_extlen: 1,
        };
        let mut bytes = header.encode().to_vec();
        bytes.extend_from_slice(&[1, 2, 3, 4]);
        bytes.extend_from_slice(&[0; 8]);
        let request = Request::decode(&bytes, 4096).unwrap();
        assert_eq!(request.header, header);
        assert_eq!(request.body, [1, 2, 3, 4]);
        assert_eq!(request.extensions.len(), 8);
        for length in 0..bytes.len() {
            assert!(Request::decode(&bytes[..length], 4096).is_err());
        }
        assert!(Request::decode(&bytes, 51).is_err());
        bytes[36..38].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(Request::decode(&bytes, 4096).is_err());
    }
    #[test]
    fn wire_flags_and_errno_do_not_depend_on_host() {
        let flags = open_flags(2 | 0o100 | 0o200 | 0o1000 | 0o2000);
        assert!(
            flags.read
                && flags.write
                && flags.create
                && flags.exclusive
                && flags.truncate
                && flags.append
        );
        let reopened = reopen_flags(flags);
        assert!(reopened.append && reopened.write && reopened.read);
        assert!(!reopened.create && !reopened.exclusive && !reopened.truncate);
        assert_eq!(
            &error_reply(9, ErrorCode::Enoent)[4..8],
            &(-2i32).to_le_bytes()
        );
    }
}
