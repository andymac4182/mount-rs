//! FUSE protocol primitives and the Linux native mount transport.
use mount_rs_core::{ErrorCode, OpenFlags};
pub mod device;
pub mod init;
pub mod inodes;
pub mod mount;
pub mod notify;
pub mod record;
pub mod session;

pub use notify::{
    FUSE_NAME_MAX, FUSE_NOTIFY_INVAL_ENTRY, FUSE_NOTIFY_INVAL_INODE, FUSE_NOTIFY_UNIQUE,
    FuseNotification, FuseNotifyInvalEntryOut, FuseNotifyInvalInodeOut, NotifyError, decode_notify,
    decode_notify_inval_entry, decode_notify_inval_inode, encode_notify, encode_notify_inval_entry,
    encode_notify_inval_inode,
};
pub use record::{
    ReplayFailure, ReplayReport, TRANSCRIPT_MAGIC, TRANSCRIPT_VERSION, TranscriptDirection,
    TranscriptError, TranscriptFrame, TranscriptRecorder, decode_transcript, encode_transcript,
    replay_transcript,
};

pub const IN_HEADER_SIZE: usize = 40;
pub const OUT_HEADER_SIZE: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolError(pub &'static str);
impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
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
            return Err(ProtocolError("truncated FUSE header"));
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
            return Err(ProtocolError("invalid FUSE message length"));
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
            return Err(ProtocolError(
                "FUSE message length mismatch or limit exceeded",
            ));
        }
        let end = bytes
            .len()
            .checked_sub(header.total_extlen as usize * 8)
            .filter(|end| *end >= IN_HEADER_SIZE)
            .ok_or(ProtocolError("invalid FUSE extension length"))?;
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
