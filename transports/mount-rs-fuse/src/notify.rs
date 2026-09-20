//! FUSE server-to-kernel notification frames.
//!
//! Notifications use the ordinary `fuse_out_header` framing, but set
//! `unique` to zero and put the positive `FUSE_NOTIFY_*` code in `error`.
//! These functions only encode and decode bytes; [`crate::device::FuseDevice`]
//! decides when a frame is written.

use std::fmt;

/// `FUSE_NAME_MAX` — the kernel rejects a longer notification name.
pub const FUSE_NAME_MAX: usize = 1024;

/// `FUSE_NOTIFY_INVAL_INODE`.
pub const FUSE_NOTIFY_INVAL_INODE: i32 = 2;
/// `FUSE_NOTIFY_INVAL_ENTRY`.
pub const FUSE_NOTIFY_INVAL_ENTRY: i32 = 3;
/// `fuse_out_header.unique` for a notification.
pub const FUSE_NOTIFY_UNIQUE: u64 = 0;

/// An invalid notification frame or body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotifyError(pub String);

impl fmt::Display for NotifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for NotifyError {}

/// `fuse_notify_inval_inode_out`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseNotifyInvalInodeOut {
    pub ino: u64,
    /// First byte to drop, or a negative value for the whole mapping.
    pub off: i64,
    /// Number of bytes to drop from `off`.
    pub len: i64,
}

/// `fuse_notify_inval_entry_out` followed by a NUL-terminated name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseNotifyInvalEntryOut {
    pub parent: u64,
    pub name: String,
    /// `FUSE_EXPIRE_ONLY` on kernels that support it.
    pub flags: u32,
}

/// A decoded notification header and body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseNotification {
    pub code: i32,
    pub body: Vec<u8>,
}

/// Encode a notification header and arbitrary body.
pub fn encode_notify(code: i32, body: &[u8]) -> Vec<u8> {
    let len = crate::OUT_HEADER_SIZE
        .checked_add(body.len())
        .expect("FUSE notification length overflow");
    let len_u32 = u32::try_from(len).expect("FUSE notification exceeds u32 length");
    let mut message = vec![0; len];
    message[..4].copy_from_slice(&len_u32.to_le_bytes());
    message[4..8].copy_from_slice(&code.to_le_bytes());
    message[8..16].copy_from_slice(&FUSE_NOTIFY_UNIQUE.to_le_bytes());
    message[crate::OUT_HEADER_SIZE..].copy_from_slice(body);
    message
}

/// Encode `FUSE_NOTIFY_INVAL_INODE`.
pub fn encode_notify_inval_inode(value: FuseNotifyInvalInodeOut) -> Vec<u8> {
    let mut body = [0_u8; 24];
    body[..8].copy_from_slice(&value.ino.to_le_bytes());
    body[8..16].copy_from_slice(&value.off.to_le_bytes());
    body[16..24].copy_from_slice(&value.len.to_le_bytes());
    encode_notify(FUSE_NOTIFY_INVAL_INODE, &body)
}

/// Encode `FUSE_NOTIFY_INVAL_ENTRY`.
pub fn encode_notify_inval_entry(value: FuseNotifyInvalEntryOut) -> Result<Vec<u8>, NotifyError> {
    if value.name.contains('\0') {
        return Err(NotifyError(
            "notify entry name contains a NUL byte".to_owned(),
        ));
    }
    let name = value.name.as_bytes();
    if name.len() > FUSE_NAME_MAX {
        return Err(NotifyError(format!(
            "notify entry name is {} bytes, over FUSE_NAME_MAX ({FUSE_NAME_MAX})",
            name.len()
        )));
    }
    let mut body = vec![0; 16 + name.len() + 1];
    body[..8].copy_from_slice(&value.parent.to_le_bytes());
    body[8..12].copy_from_slice(&(name.len() as u32).to_le_bytes());
    body[12..16].copy_from_slice(&value.flags.to_le_bytes());
    body[16..16 + name.len()].copy_from_slice(name);
    Ok(encode_notify(FUSE_NOTIFY_INVAL_ENTRY, &body))
}

/// Decode a notification frame.
pub fn decode_notify(message: &[u8]) -> Result<FuseNotification, NotifyError> {
    if message.len() < crate::OUT_HEADER_SIZE {
        return Err(NotifyError(format!(
            "truncated notification: {} byte(s)",
            message.len()
        )));
    }
    let len = u32::from_le_bytes(message[..4].try_into().unwrap()) as usize;
    let code = i32::from_le_bytes(message[4..8].try_into().unwrap());
    let unique = u64::from_le_bytes(message[8..16].try_into().unwrap());
    if unique != FUSE_NOTIFY_UNIQUE {
        return Err(NotifyError(format!(
            "fuse_out_header.unique is {unique}, not a notification"
        )));
    }
    if len > message.len() {
        return Err(NotifyError(format!(
            "fuse_out_header.len is {len} but only {} byte(s) were read",
            message.len()
        )));
    }
    let body = if len < crate::OUT_HEADER_SIZE {
        Vec::new()
    } else {
        message[crate::OUT_HEADER_SIZE..len].to_vec()
    };
    Ok(FuseNotification { code, body })
}

/// Decode a `FUSE_NOTIFY_INVAL_INODE` body.
pub fn decode_notify_inval_inode(body: &[u8]) -> Result<FuseNotifyInvalInodeOut, NotifyError> {
    if body.len() != 24 {
        return Err(NotifyError(format!(
            "fuse_notify_inval_inode_out is 24 bytes, got {}",
            body.len()
        )));
    }
    Ok(FuseNotifyInvalInodeOut {
        ino: u64::from_le_bytes(body[..8].try_into().unwrap()),
        off: i64::from_le_bytes(body[8..16].try_into().unwrap()),
        len: i64::from_le_bytes(body[16..24].try_into().unwrap()),
    })
}

/// Decode a `FUSE_NOTIFY_INVAL_ENTRY` body.
pub fn decode_notify_inval_entry(body: &[u8]) -> Result<FuseNotifyInvalEntryOut, NotifyError> {
    if body.len() < 17 {
        return Err(NotifyError(format!(
            "truncated fuse_notify_inval_entry_out: {} byte(s)",
            body.len()
        )));
    }
    let parent = u64::from_le_bytes(body[..8].try_into().unwrap());
    let namelen = u32::from_le_bytes(body[8..12].try_into().unwrap()) as usize;
    let flags = u32::from_le_bytes(body[12..16].try_into().unwrap());
    let expected = 16usize
        .checked_add(namelen)
        .and_then(|length| length.checked_add(1))
        .ok_or_else(|| NotifyError("notification name length overflows".to_owned()))?;
    if body.len() != expected {
        return Err(NotifyError(format!(
            "fuse_notify_inval_entry_out.namelen is {namelen} but the body is {} byte(s)",
            body.len()
        )));
    }
    Ok(FuseNotifyInvalEntryOut {
        parent,
        name: String::from_utf8_lossy(&body[16..16 + namelen]).into_owned(),
        flags,
    })
}
