//! Symmetric, bounded FUSE request and reply codecs.
//!
//! This is the primary public wire-protocol slice from the pinned mountx
//! source.  It covers the fixed layouts through protocol 7.41, including the
//! version-dependent compatibility forms, whole-message framing, and
//! `READDIR`/`READDIRPLUS` packing.  Decoders copy retained byte fields and
//! return [`ProtocolError`] for truncation, malformed counts, unterminated
//! names, and trailing bytes; they do not expose indexing panics to callers.
//!
//! The codec table is intentionally separate from [`crate::session`].  A
//! codec being present means a message can be represented and tested; it does
//! not claim that the native session dispatches that operation.  The exact
//! unimplemented wire operations are listed in [`crate::constants::UNIMPLEMENTED_OPCODES`]
//! and the crate README records the remaining session/native gaps.

use crate::constants::*;
use crate::{ProtocolError, RequestHeader};

/// The negotiated minor version and the optional 7.33 `SETXATTR` extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolContext {
    pub minor: u32,
    pub setxattr_ext: bool,
}

/// Latest layouts, with the extended `SETXATTR` form disabled until negotiated.
pub const DEFAULT_PROTOCOL: ProtocolContext = ProtocolContext {
    minor: FUSE_KERNEL_MINOR_VERSION,
    setxattr_ext: false,
};

fn context(ctx: Option<ProtocolContext>) -> ProtocolContext {
    ctx.unwrap_or(DEFAULT_PROTOCOL)
}

pub const fn attr_size(minor: u32) -> usize {
    if minor >= 9 { 88 } else { 80 }
}

pub const fn entry_out_size(minor: u32) -> usize {
    40 + attr_size(minor)
}

pub const fn attr_out_size(minor: u32) -> usize {
    16 + attr_size(minor)
}

pub const fn kstatfs_size(minor: u32) -> usize {
    if minor >= 4 {
        80
    } else {
        FUSE_COMPAT_STATFS_SIZE
    }
}

pub const fn init_out_size(minor: u32) -> usize {
    if minor < 5 {
        FUSE_COMPAT_INIT_OUT_SIZE
    } else if minor < 23 {
        FUSE_COMPAT_22_INIT_OUT_SIZE
    } else {
        FUSE_INIT_OUT_SIZE
    }
}

pub const fn read_write_in_size(minor: u32) -> usize {
    if minor >= 9 {
        40
    } else {
        FUSE_COMPAT_WRITE_IN_SIZE
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }

    fn need(&mut self, size: usize, what: &str) -> Result<usize, ProtocolError> {
        let end = self
            .offset
            .checked_add(size)
            .ok_or_else(|| ProtocolError::at(format!("{what} length overflows"), self.offset))?;
        if end > self.bytes.len() {
            return Err(ProtocolError::at(
                format!(
                    "truncated {what}: need {size} byte(s), {} remain",
                    self.remaining()
                ),
                self.offset,
            ));
        }
        let start = self.offset;
        self.offset = end;
        Ok(start)
    }

    fn u16(&mut self, what: &str) -> Result<u16, ProtocolError> {
        let at = self.need(2, what)?;
        Ok(u16::from_le_bytes(
            self.bytes[at..at + 2].try_into().unwrap(),
        ))
    }

    fn u32(&mut self, what: &str) -> Result<u32, ProtocolError> {
        let at = self.need(4, what)?;
        Ok(u32::from_le_bytes(
            self.bytes[at..at + 4].try_into().unwrap(),
        ))
    }

    fn i32(&mut self, what: &str) -> Result<i32, ProtocolError> {
        Ok(self.u32(what)? as i32)
    }

    fn u64(&mut self, what: &str) -> Result<u64, ProtocolError> {
        let at = self.need(8, what)?;
        Ok(u64::from_le_bytes(
            self.bytes[at..at + 8].try_into().unwrap(),
        ))
    }

    fn skip(&mut self, size: usize, what: &str) -> Result<(), ProtocolError> {
        self.need(size, what).map(|_| ())
    }

    fn raw(&mut self, size: usize, what: &str) -> Result<Vec<u8>, ProtocolError> {
        let at = self.need(size, what)?;
        Ok(self.bytes[at..at + size].to_vec())
    }

    fn name(&mut self, what: &str) -> Result<String, ProtocolError> {
        let start = self.offset;
        let Some(relative) = self.bytes[start..].iter().position(|byte| *byte == 0) else {
            return Err(ProtocolError::at(format!("unterminated {what}"), start));
        };
        let end = start + relative;
        self.offset = end + 1;
        Ok(String::from_utf8_lossy(&self.bytes[start..end]).into_owned())
    }

    fn rest_string(&mut self) -> String {
        let bytes = &self.bytes[self.offset..];
        self.offset = self.bytes.len();
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn end(&self, what: &str) -> Result<(), ProtocolError> {
        if self.remaining() != 0 {
            Err(ProtocolError::at(
                format!("{what} has {} trailing byte(s)", self.remaining()),
                self.offset,
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Default)]
struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.u32(value as u32);
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    fn skip(&mut self, size: usize) {
        self.bytes.resize(self.bytes.len() + size, 0);
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

fn name_bytes<'a>(name: &'a str, what: &str) -> Result<&'a [u8], ProtocolError> {
    if name.as_bytes().contains(&0) {
        return Err(ProtocolError::new(format!("{what} contains a NUL byte")));
    }
    Ok(name.as_bytes())
}

fn write_name(writer: &mut Writer, name: &str, what: &str) -> Result<(), ProtocolError> {
    let bytes = name_bytes(name, what)?;
    writer.raw(bytes);
    writer.skip(1);
    Ok(())
}

/// `struct fuse_attr`, with fields absent on old protocol minors normalized to
/// zero after decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseAttr {
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub atime_nsec: u32,
    pub mtime_nsec: u32,
    pub ctime_nsec: u32,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub blksize: u32,
    pub flags: u32,
}

fn read_attr(reader: &mut Reader<'_>, minor: u32) -> Result<FuseAttr, ProtocolError> {
    let attr = FuseAttr {
        ino: reader.u64("fuse_attr.ino")?,
        size: reader.u64("fuse_attr.size")?,
        blocks: reader.u64("fuse_attr.blocks")?,
        atime: reader.u64("fuse_attr.atime")?,
        mtime: reader.u64("fuse_attr.mtime")?,
        ctime: reader.u64("fuse_attr.ctime")?,
        atime_nsec: reader.u32("fuse_attr.atimensec")?,
        mtime_nsec: reader.u32("fuse_attr.mtimensec")?,
        ctime_nsec: reader.u32("fuse_attr.ctimensec")?,
        mode: reader.u32("fuse_attr.mode")?,
        nlink: reader.u32("fuse_attr.nlink")?,
        uid: reader.u32("fuse_attr.uid")?,
        gid: reader.u32("fuse_attr.gid")?,
        rdev: reader.u32("fuse_attr.rdev")?,
        blksize: if minor >= 9 {
            reader.u32("fuse_attr.blksize")?
        } else {
            0
        },
        flags: if minor >= 9 {
            let value = reader.u32("fuse_attr.flags")?;
            if minor >= 32 { value } else { 0 }
        } else {
            0
        },
    };
    Ok(attr)
}

fn write_attr(writer: &mut Writer, attr: &FuseAttr, minor: u32) {
    writer.u64(attr.ino);
    writer.u64(attr.size);
    writer.u64(attr.blocks);
    writer.u64(attr.atime);
    writer.u64(attr.mtime);
    writer.u64(attr.ctime);
    writer.u32(attr.atime_nsec);
    writer.u32(attr.mtime_nsec);
    writer.u32(attr.ctime_nsec);
    writer.u32(attr.mode);
    writer.u32(attr.nlink);
    writer.u32(attr.uid);
    writer.u32(attr.gid);
    writer.u32(attr.rdev);
    if minor >= 9 {
        writer.u32(attr.blksize);
        writer.u32(if minor >= 32 { attr.flags } else { 0 });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseKstatfs {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseFileLock {
    pub start: u64,
    pub end: u64,
    pub type_: u32,
    pub pid: u32,
}

fn read_file_lock(reader: &mut Reader<'_>) -> Result<FuseFileLock, ProtocolError> {
    Ok(FuseFileLock {
        start: reader.u64("fuse_file_lock.start")?,
        end: reader.u64("fuse_file_lock.end")?,
        type_: reader.u32("fuse_file_lock.type")?,
        pid: reader.u32("fuse_file_lock.pid")?,
    })
}

fn write_file_lock(writer: &mut Writer, lock: &FuseFileLock) {
    writer.u64(lock.start);
    writer.u64(lock.end);
    writer.u32(lock.type_);
    writer.u32(lock.pid);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseEntryOut {
    pub nodeid: u64,
    pub generation: u64,
    pub entry_valid: u64,
    pub attr_valid: u64,
    pub entry_valid_nsec: u32,
    pub attr_valid_nsec: u32,
    pub attr: FuseAttr,
}

fn read_entry_out(reader: &mut Reader<'_>, minor: u32) -> Result<FuseEntryOut, ProtocolError> {
    Ok(FuseEntryOut {
        nodeid: reader.u64("fuse_entry_out.nodeid")?,
        generation: reader.u64("fuse_entry_out.generation")?,
        entry_valid: reader.u64("fuse_entry_out.entry_valid")?,
        attr_valid: reader.u64("fuse_entry_out.attr_valid")?,
        entry_valid_nsec: reader.u32("fuse_entry_out.entry_valid_nsec")?,
        attr_valid_nsec: reader.u32("fuse_entry_out.attr_valid_nsec")?,
        attr: read_attr(reader, minor)?,
    })
}

fn write_entry_out(writer: &mut Writer, entry: &FuseEntryOut, minor: u32) {
    writer.u64(entry.nodeid);
    writer.u64(entry.generation);
    writer.u64(entry.entry_valid);
    writer.u64(entry.attr_valid);
    writer.u32(entry.entry_valid_nsec);
    writer.u32(entry.attr_valid_nsec);
    write_attr(writer, &entry.attr, minor);
}

/// `fuse_open_out`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseOpenOut {
    pub fh: u64,
    pub open_flags: u32,
    pub backing_id: i32,
}

fn read_open_out(reader: &mut Reader<'_>, minor: u32) -> Result<FuseOpenOut, ProtocolError> {
    let fh = reader.u64("fuse_open_out.fh")?;
    let open_flags = reader.u32("fuse_open_out.open_flags")?;
    let backing_id = reader.i32("fuse_open_out.backing_id")?;
    Ok(FuseOpenOut {
        fh,
        open_flags,
        backing_id: if minor >= 40 { backing_id } else { 0 },
    })
}

fn write_open_out(writer: &mut Writer, value: &FuseOpenOut, minor: u32) {
    writer.u64(value.fh);
    writer.u32(value.open_flags);
    writer.i32(if minor >= 40 { value.backing_id } else { 0 });
}

/// Decode the public `fuse_entry_out` body.
pub fn decode_entry_out(
    body: &[u8],
    ctx: Option<ProtocolContext>,
) -> Result<FuseEntryOut, ProtocolError> {
    let minor = context(ctx).minor;
    let mut reader = Reader::new(body);
    let value = read_entry_out(&mut reader, minor)?;
    reader.end("fuse_entry_out")?;
    Ok(value)
}

pub fn encode_entry_out(value: &FuseEntryOut, ctx: Option<ProtocolContext>) -> Vec<u8> {
    let minor = context(ctx).minor;
    let mut writer = Writer::with_capacity(entry_out_size(minor));
    write_entry_out(&mut writer, value, minor);
    writer.finish()
}

/// Decode the public `fuse_attr_out` body.
pub fn decode_attr_out(
    body: &[u8],
    ctx: Option<ProtocolContext>,
) -> Result<FuseAttrOut, ProtocolError> {
    let minor = context(ctx).minor;
    let mut reader = Reader::new(body);
    let value = FuseAttrOut {
        attr_valid: reader.u64("fuse_attr_out.attr_valid")?,
        attr_valid_nsec: reader.u32("fuse_attr_out.attr_valid_nsec")?,
        attr: {
            reader.skip(4, "fuse_attr_out.dummy")?;
            read_attr(&mut reader, minor)?
        },
    };
    reader.end("fuse_attr_out")?;
    Ok(value)
}

pub fn encode_attr_out(value: &FuseAttrOut, ctx: Option<ProtocolContext>) -> Vec<u8> {
    let minor = context(ctx).minor;
    let mut writer = Writer::with_capacity(attr_out_size(minor));
    writer.u64(value.attr_valid);
    writer.u32(value.attr_valid_nsec);
    writer.skip(4);
    write_attr(&mut writer, &value.attr, minor);
    writer.finish()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseAttrOut {
    pub attr_valid: u64,
    pub attr_valid_nsec: u32,
    pub attr: FuseAttr,
}

pub fn decode_open_out(
    body: &[u8],
    ctx: Option<ProtocolContext>,
) -> Result<FuseOpenOut, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = read_open_out(&mut reader, context(ctx).minor)?;
    reader.end("fuse_open_out")?;
    Ok(value)
}

pub fn encode_open_out(value: &FuseOpenOut, ctx: Option<ProtocolContext>) -> Vec<u8> {
    let mut writer = Writer::with_capacity(16);
    write_open_out(&mut writer, value, context(ctx).minor);
    writer.finish()
}

/// `fuse_in_header` is the existing native header type, exposed with the
/// upstream-shaped codec names as well.
pub type FuseInHeader = RequestHeader;

pub fn decode_in_header(bytes: &[u8]) -> Result<FuseInHeader, ProtocolError> {
    if bytes.len() < FUSE_IN_HEADER_SIZE {
        return Err(ProtocolError::at("truncated fuse_in_header", bytes.len()));
    }
    let header = RequestHeader::decode(bytes)?;
    if header.len < FUSE_IN_HEADER_SIZE as u32 {
        return Err(ProtocolError::new(format!(
            "fuse_in_header.len is {}, below the {FUSE_IN_HEADER_SIZE}-byte header",
            header.len
        )));
    }
    Ok(header)
}

pub fn encode_in_header(header: &FuseInHeader) -> [u8; FUSE_IN_HEADER_SIZE] {
    header.encode()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseOutHeader {
    pub len: u32,
    pub error: i32,
    pub unique: u64,
}

pub fn decode_out_header(bytes: &[u8]) -> Result<FuseOutHeader, ProtocolError> {
    if bytes.len() < FUSE_OUT_HEADER_SIZE {
        return Err(ProtocolError::at("truncated fuse_out_header", bytes.len()));
    }
    let header = FuseOutHeader {
        len: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
        error: i32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        unique: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
    };
    if header.len < FUSE_OUT_HEADER_SIZE as u32 {
        return Err(ProtocolError::new(format!(
            "fuse_out_header.len is {}, below the {FUSE_OUT_HEADER_SIZE}-byte header",
            header.len
        )));
    }
    Ok(header)
}

pub fn write_out_header_into(
    target: &mut [u8],
    header: FuseOutHeader,
) -> Result<(), ProtocolError> {
    if target.len() < FUSE_OUT_HEADER_SIZE {
        return Err(ProtocolError::at(
            "reply target is shorter than fuse_out_header",
            target.len(),
        ));
    }
    target[..4].copy_from_slice(&header.len.to_le_bytes());
    target[4..8].copy_from_slice(&header.error.to_le_bytes());
    target[8..16].copy_from_slice(&header.unique.to_le_bytes());
    Ok(())
}

pub fn encode_out_header(header: FuseOutHeader) -> [u8; FUSE_OUT_HEADER_SIZE] {
    let mut bytes = [0; FUSE_OUT_HEADER_SIZE];
    // The fixed-size array is guaranteed to satisfy this helper's precondition.
    write_out_header_into(&mut bytes, header).expect("fixed FUSE output header");
    bytes
}

pub fn fuse_errno(errno: i32) -> i32 {
    if errno > 0 { -errno } else { errno }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseReplyBuffer {
    pub message: Vec<u8>,
    pub body_offset: usize,
}

impl FuseReplyBuffer {
    pub fn body(&self) -> &[u8] {
        &self.message[self.body_offset..]
    }

    pub fn body_mut(&mut self) -> &mut [u8] {
        &mut self.message[self.body_offset..]
    }
}

pub fn alloc_reply(size: usize) -> Result<FuseReplyBuffer, ProtocolError> {
    let length = FUSE_OUT_HEADER_SIZE
        .checked_add(size)
        .ok_or_else(|| ProtocolError::new("reply body size overflows FUSE output length"))?;
    Ok(FuseReplyBuffer {
        message: vec![0; length],
        body_offset: FUSE_OUT_HEADER_SIZE,
    })
}

pub fn finish_reply(
    reply: &mut FuseReplyBuffer,
    unique: u64,
    bytes_used: Option<usize>,
) -> Result<Vec<u8>, ProtocolError> {
    let used = bytes_used.unwrap_or_else(|| reply.message.len() - reply.body_offset);
    let capacity = reply.message.len() - reply.body_offset;
    if used > capacity {
        return Err(ProtocolError::new(format!(
            "reply used {used} of a {capacity}-byte body"
        )));
    }
    let len = FUSE_OUT_HEADER_SIZE + used;
    write_out_header_into(
        &mut reply.message,
        FuseOutHeader {
            len: u32::try_from(len).map_err(|_| ProtocolError::new("reply length exceeds u32"))?,
            error: 0,
            unique,
        },
    )?;
    Ok(reply.message[..len].to_vec())
}

pub fn encode_reply(unique: u64, body: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let mut reply = alloc_reply(body.len())?;
    reply.body_mut().copy_from_slice(body);
    finish_reply(&mut reply, unique, None)
}

pub fn encode_error_reply(unique: u64, errno: i32) -> [u8; FUSE_OUT_HEADER_SIZE] {
    encode_out_header(FuseOutHeader {
        len: FUSE_OUT_HEADER_SIZE as u32,
        error: fuse_errno(errno),
        unique,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseRawData {
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseNameIn {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseForgetIn {
    pub nlookup: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseForgetOne {
    pub nodeid: u64,
    pub nlookup: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseBatchForgetIn {
    pub forgets: Vec<FuseForgetOne>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseGetattrIn {
    pub getattr_flags: u32,
    pub fh: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseSetattrIn {
    pub valid: u32,
    pub fh: u64,
    pub size: u64,
    pub lock_owner: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub atime_nsec: u32,
    pub mtime_nsec: u32,
    pub ctime_nsec: u32,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseSymlinkIn {
    pub name: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseMknodIn {
    pub mode: u32,
    pub rdev: u32,
    pub umask: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseMkdirIn {
    pub mode: u32,
    pub umask: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseRenameIn {
    pub newdir: u64,
    pub old_name: String,
    pub new_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseRename2In {
    pub newdir: u64,
    pub flags: u32,
    pub old_name: String,
    pub new_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseLinkIn {
    pub oldnodeid: u64,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseOpenIn {
    pub flags: u32,
    pub open_flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseCreateIn {
    pub flags: u32,
    pub mode: u32,
    pub umask: u32,
    pub open_flags: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseCreateOut {
    pub entry: FuseEntryOut,
    pub open: FuseOpenOut,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseReleaseIn {
    pub fh: u64,
    pub flags: u32,
    pub release_flags: u32,
    pub lock_owner: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseFlushIn {
    pub fh: u64,
    pub lock_owner: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseReadIn {
    pub fh: u64,
    pub offset: u64,
    pub size: u32,
    pub read_flags: u32,
    pub lock_owner: u64,
    pub flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseWriteIn {
    pub fh: u64,
    pub offset: u64,
    pub size: u32,
    pub write_flags: u32,
    pub lock_owner: u64,
    pub flags: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseWriteOut {
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseFsyncIn {
    pub fh: u64,
    pub fsync_flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseSyncfsIn {
    pub padding: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseSetxattrIn {
    pub flags: u32,
    pub setxattr_flags: u32,
    pub name: String,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseGetxattrIn {
    pub size: u32,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseListxattrIn {
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseGetxattrOut {
    pub size: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseLkIn {
    pub fh: u64,
    pub owner: u64,
    pub lk: FuseFileLock,
    pub lk_flags: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseLkOut {
    pub lk: FuseFileLock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseAccessIn {
    pub mask: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseInitIn {
    pub major: u32,
    pub minor: u32,
    pub max_readahead: u32,
    pub flags: u32,
    pub flags2: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseInitOut {
    pub major: u32,
    pub minor: u32,
    pub max_readahead: u32,
    pub flags: u32,
    pub max_background: u16,
    pub congestion_threshold: u16,
    pub max_write: u32,
    pub time_gran: u32,
    pub max_pages: u16,
    pub map_alignment: u16,
    pub flags2: u32,
    pub max_stack_depth: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseInterruptIn {
    pub unique: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FusePollIn {
    pub fh: u64,
    pub kh: u64,
    pub flags: u32,
    pub events: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FusePollOut {
    pub revents: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseFallocateIn {
    pub fh: u64,
    pub offset: u64,
    pub length: u64,
    pub mode: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseIoctlIn {
    pub fh: u64,
    pub flags: u32,
    pub cmd: u32,
    pub arg: u64,
    pub in_size: u32,
    pub out_size: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseIoctlOut {
    pub result: i32,
    pub flags: u32,
    pub in_iovs: u32,
    pub out_iovs: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseLseekIn {
    pub fh: u64,
    pub offset: u64,
    pub whence: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseLseekOut {
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseReadlinkOut {
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseBmapIn {
    pub block: u64,
    pub blocksize: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FuseBmapOut {
    pub block: u64,
}

fn decode_empty(body: &[u8], what: &str) -> Result<(), ProtocolError> {
    if body.is_empty() {
        Ok(())
    } else {
        Err(ProtocolError::new(format!(
            "expected an empty {what} body, got {} byte(s)",
            body.len()
        )))
    }
}

fn decode_name_in(body: &[u8], what: &str) -> Result<FuseNameIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseNameIn {
        name: reader.name(what)?,
    };
    reader.end(what)?;
    Ok(value)
}

fn encode_name_in(value: &FuseNameIn, what: &str) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::with_capacity(value.name.len() + 1);
    write_name(&mut writer, &value.name, what)?;
    Ok(writer.finish())
}

fn decode_forget_in(body: &[u8]) -> Result<FuseForgetIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseForgetIn {
        nlookup: reader.u64("fuse_forget_in.nlookup")?,
    };
    reader.end("fuse_forget_in")?;
    Ok(value)
}

fn encode_forget_in(value: &FuseForgetIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u64(value.nlookup);
    writer.finish()
}

fn decode_batch_forget_in(body: &[u8]) -> Result<FuseBatchForgetIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let count = usize::try_from(reader.u32("fuse_batch_forget_in.count")?)
        .map_err(|_| ProtocolError::new("batch forget count does not fit usize"))?;
    reader.skip(4, "fuse_batch_forget_in.dummy")?;
    if count > reader.remaining() / 16 {
        return Err(ProtocolError::new(format!(
            "fuse_batch_forget_in.count is {count} but only {} byte(s) follow",
            reader.remaining()
        )));
    }
    let mut forgets = Vec::with_capacity(count);
    for _ in 0..count {
        forgets.push(FuseForgetOne {
            nodeid: reader.u64("fuse_forget_one.nodeid")?,
            nlookup: reader.u64("fuse_forget_one.nlookup")?,
        });
    }
    reader.end("fuse_batch_forget_in")?;
    Ok(FuseBatchForgetIn { forgets })
}

fn encode_batch_forget_in(value: &FuseBatchForgetIn) -> Result<Vec<u8>, ProtocolError> {
    let capacity = 8usize
        .checked_add(value.forgets.len().checked_mul(16).ok_or_else(|| {
            ProtocolError::new("batch forget count overflows FUSE message length")
        })?)
        .ok_or_else(|| ProtocolError::new("batch forget length overflows FUSE message length"))?;
    let count = u32::try_from(value.forgets.len())
        .map_err(|_| ProtocolError::new("batch forget count exceeds u32"))?;
    let mut writer = Writer::with_capacity(capacity);
    writer.u32(count);
    writer.skip(4);
    for forget in &value.forgets {
        writer.u64(forget.nodeid);
        writer.u64(forget.nlookup);
    }
    Ok(writer.finish())
}

fn decode_getattr_in(body: &[u8]) -> Result<FuseGetattrIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let getattr_flags = reader.u32("fuse_getattr_in.getattr_flags")?;
    reader.skip(4, "fuse_getattr_in.dummy")?;
    let fh = reader.u64("fuse_getattr_in.fh")?;
    reader.end("fuse_getattr_in")?;
    Ok(FuseGetattrIn { getattr_flags, fh })
}

fn encode_getattr_in(value: &FuseGetattrIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(16);
    writer.u32(value.getattr_flags);
    writer.skip(4);
    writer.u64(value.fh);
    writer.finish()
}

fn decode_setattr_in(body: &[u8]) -> Result<FuseSetattrIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let valid = reader.u32("fuse_setattr_in.valid")?;
    reader.skip(4, "fuse_setattr_in.padding")?;
    let value = FuseSetattrIn {
        valid,
        fh: reader.u64("fuse_setattr_in.fh")?,
        size: reader.u64("fuse_setattr_in.size")?,
        lock_owner: reader.u64("fuse_setattr_in.lock_owner")?,
        atime: reader.u64("fuse_setattr_in.atime")?,
        mtime: reader.u64("fuse_setattr_in.mtime")?,
        ctime: reader.u64("fuse_setattr_in.ctime")?,
        atime_nsec: reader.u32("fuse_setattr_in.atimensec")?,
        mtime_nsec: reader.u32("fuse_setattr_in.mtimensec")?,
        ctime_nsec: reader.u32("fuse_setattr_in.ctimensec")?,
        mode: reader.u32("fuse_setattr_in.mode")?,
        uid: {
            reader.skip(4, "fuse_setattr_in.unused4")?;
            reader.u32("fuse_setattr_in.uid")?
        },
        gid: reader.u32("fuse_setattr_in.gid")?,
    };
    reader.skip(4, "fuse_setattr_in.unused5")?;
    reader.end("fuse_setattr_in")?;
    Ok(value)
}

fn encode_setattr_in(value: &FuseSetattrIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(88);
    writer.u32(value.valid);
    writer.skip(4);
    writer.u64(value.fh);
    writer.u64(value.size);
    writer.u64(value.lock_owner);
    writer.u64(value.atime);
    writer.u64(value.mtime);
    writer.u64(value.ctime);
    writer.u32(value.atime_nsec);
    writer.u32(value.mtime_nsec);
    writer.u32(value.ctime_nsec);
    writer.u32(value.mode);
    writer.skip(4);
    writer.u32(value.uid);
    writer.u32(value.gid);
    writer.skip(4);
    writer.finish()
}

fn decode_symlink_in(body: &[u8]) -> Result<FuseSymlinkIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseSymlinkIn {
        name: reader.name("symlink name")?,
        target: reader.name("symlink target")?,
    };
    reader.end("symlink")?;
    Ok(value)
}

fn encode_symlink_in(value: &FuseSymlinkIn) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::with_capacity(value.name.len() + value.target.len() + 2);
    write_name(&mut writer, &value.name, "symlink name")?;
    write_name(&mut writer, &value.target, "symlink target")?;
    Ok(writer.finish())
}

fn decode_mknod_in(body: &[u8], ctx: ProtocolContext) -> Result<FuseMknodIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let mode = reader.u32("fuse_mknod_in.mode")?;
    let rdev = reader.u32("fuse_mknod_in.rdev")?;
    let (umask, name) = if ctx.minor >= 12 {
        let umask = reader.u32("fuse_mknod_in.umask")?;
        reader.skip(4, "fuse_mknod_in.padding")?;
        (umask, reader.name("mknod name")?)
    } else {
        (0, reader.name("mknod name")?)
    };
    reader.end("fuse_mknod_in")?;
    Ok(FuseMknodIn {
        mode,
        rdev,
        umask,
        name,
    })
}

fn encode_mknod_in(value: &FuseMknodIn, ctx: ProtocolContext) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::with_capacity(
        if ctx.minor >= 12 {
            16
        } else {
            FUSE_COMPAT_MKNOD_IN_SIZE
        } + value.name.len()
            + 1,
    );
    writer.u32(value.mode);
    writer.u32(value.rdev);
    if ctx.minor >= 12 {
        writer.u32(value.umask);
        writer.skip(4);
    }
    write_name(&mut writer, &value.name, "mknod name")?;
    Ok(writer.finish())
}

fn decode_mkdir_in(body: &[u8]) -> Result<FuseMkdirIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseMkdirIn {
        mode: reader.u32("fuse_mkdir_in.mode")?,
        umask: reader.u32("fuse_mkdir_in.umask")?,
        name: reader.name("mkdir name")?,
    };
    reader.end("fuse_mkdir_in")?;
    Ok(value)
}

fn encode_mkdir_in(value: &FuseMkdirIn) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::with_capacity(8 + value.name.len() + 1);
    writer.u32(value.mode);
    writer.u32(value.umask);
    write_name(&mut writer, &value.name, "mkdir name")?;
    Ok(writer.finish())
}

fn decode_rename_in(body: &[u8]) -> Result<FuseRenameIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseRenameIn {
        newdir: reader.u64("fuse_rename_in.newdir")?,
        old_name: reader.name("rename oldname")?,
        new_name: reader.name("rename newname")?,
    };
    reader.end("fuse_rename_in")?;
    Ok(value)
}

fn encode_rename_in(value: &FuseRenameIn) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::with_capacity(8 + value.old_name.len() + value.new_name.len() + 2);
    writer.u64(value.newdir);
    write_name(&mut writer, &value.old_name, "rename oldname")?;
    write_name(&mut writer, &value.new_name, "rename newname")?;
    Ok(writer.finish())
}

fn decode_rename2_in(body: &[u8]) -> Result<FuseRename2In, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseRename2In {
        newdir: reader.u64("fuse_rename2_in.newdir")?,
        flags: reader.u32("fuse_rename2_in.flags")?,
        old_name: {
            reader.skip(4, "fuse_rename2_in.padding")?;
            reader.name("rename2 oldname")?
        },
        new_name: reader.name("rename2 newname")?,
    };
    reader.end("fuse_rename2_in")?;
    Ok(value)
}

fn encode_rename2_in(value: &FuseRename2In) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::with_capacity(16 + value.old_name.len() + value.new_name.len() + 2);
    writer.u64(value.newdir);
    writer.u32(value.flags);
    writer.skip(4);
    write_name(&mut writer, &value.old_name, "rename2 oldname")?;
    write_name(&mut writer, &value.new_name, "rename2 newname")?;
    Ok(writer.finish())
}

fn decode_link_in(body: &[u8]) -> Result<FuseLinkIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseLinkIn {
        oldnodeid: reader.u64("fuse_link_in.oldnodeid")?,
        name: reader.name("link name")?,
    };
    reader.end("fuse_link_in")?;
    Ok(value)
}

fn encode_link_in(value: &FuseLinkIn) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::with_capacity(8 + value.name.len() + 1);
    writer.u64(value.oldnodeid);
    write_name(&mut writer, &value.name, "link name")?;
    Ok(writer.finish())
}

fn decode_open_in(body: &[u8]) -> Result<FuseOpenIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseOpenIn {
        flags: reader.u32("fuse_open_in.flags")?,
        open_flags: reader.u32("fuse_open_in.open_flags")?,
    };
    reader.end("fuse_open_in")?;
    Ok(value)
}

fn encode_open_in(value: &FuseOpenIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u32(value.flags);
    writer.u32(value.open_flags);
    writer.finish()
}

fn decode_create_in(body: &[u8], ctx: ProtocolContext) -> Result<FuseCreateIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let flags = reader.u32("fuse_create_in.flags")?;
    let mode = reader.u32("fuse_create_in.mode")?;
    let (umask, open_flags) = if ctx.minor >= 12 {
        (
            reader.u32("fuse_create_in.umask")?,
            reader.u32("fuse_create_in.open_flags")?,
        )
    } else {
        (0, 0)
    };
    let name = reader.name("create name")?;
    reader.end("fuse_create_in")?;
    Ok(FuseCreateIn {
        flags,
        mode,
        umask,
        open_flags,
        name,
    })
}

fn encode_create_in(value: &FuseCreateIn, ctx: ProtocolContext) -> Result<Vec<u8>, ProtocolError> {
    let head = if ctx.minor >= 12 {
        16
    } else {
        FUSE_COMPAT_MKNOD_IN_SIZE
    };
    let mut writer = Writer::with_capacity(head + value.name.len() + 1);
    writer.u32(value.flags);
    writer.u32(value.mode);
    if ctx.minor >= 12 {
        writer.u32(value.umask);
        writer.u32(value.open_flags);
    }
    write_name(&mut writer, &value.name, "create name")?;
    Ok(writer.finish())
}

fn decode_release_in(body: &[u8]) -> Result<FuseReleaseIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseReleaseIn {
        fh: reader.u64("fuse_release_in.fh")?,
        flags: reader.u32("fuse_release_in.flags")?,
        release_flags: reader.u32("fuse_release_in.release_flags")?,
        lock_owner: reader.u64("fuse_release_in.lock_owner")?,
    };
    reader.end("fuse_release_in")?;
    Ok(value)
}

fn encode_release_in(value: &FuseReleaseIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(24);
    writer.u64(value.fh);
    writer.u32(value.flags);
    writer.u32(value.release_flags);
    writer.u64(value.lock_owner);
    writer.finish()
}

fn decode_flush_in(body: &[u8]) -> Result<FuseFlushIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let fh = reader.u64("fuse_flush_in.fh")?;
    reader.skip(8, "fuse_flush_in.unused")?;
    let lock_owner = reader.u64("fuse_flush_in.lock_owner")?;
    reader.end("fuse_flush_in")?;
    Ok(FuseFlushIn { fh, lock_owner })
}

fn encode_flush_in(value: &FuseFlushIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(24);
    writer.u64(value.fh);
    writer.skip(8);
    writer.u64(value.lock_owner);
    writer.finish()
}

fn decode_read_in(body: &[u8], ctx: ProtocolContext) -> Result<FuseReadIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let fh = reader.u64("fuse_read_in.fh")?;
    let offset = reader.u64("fuse_read_in.offset")?;
    let size = reader.u32("fuse_read_in.size")?;
    let read_flags = reader.u32("fuse_read_in.read_flags")?;
    let (lock_owner, flags) = if ctx.minor >= 9 {
        let lock_owner = reader.u64("fuse_read_in.lock_owner")?;
        let flags = reader.u32("fuse_read_in.flags")?;
        reader.skip(4, "fuse_read_in.padding")?;
        (lock_owner, flags)
    } else {
        (0, 0)
    };
    reader.end("fuse_read_in")?;
    Ok(FuseReadIn {
        fh,
        offset,
        size,
        read_flags,
        lock_owner,
        flags,
    })
}

fn encode_read_in(value: &FuseReadIn, ctx: ProtocolContext) -> Vec<u8> {
    let mut writer = Writer::with_capacity(read_write_in_size(ctx.minor));
    writer.u64(value.fh);
    writer.u64(value.offset);
    writer.u32(value.size);
    writer.u32(value.read_flags);
    if ctx.minor >= 9 {
        writer.u64(value.lock_owner);
        writer.u32(value.flags);
        writer.skip(4);
    }
    writer.finish()
}

fn decode_write_in(body: &[u8], ctx: ProtocolContext) -> Result<FuseWriteIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let fh = reader.u64("fuse_write_in.fh")?;
    let offset = reader.u64("fuse_write_in.offset")?;
    let size = reader.u32("fuse_write_in.size")?;
    let write_flags = reader.u32("fuse_write_in.write_flags")?;
    let (lock_owner, flags) = if ctx.minor >= 9 {
        let lock_owner = reader.u64("fuse_write_in.lock_owner")?;
        let flags = reader.u32("fuse_write_in.flags")?;
        reader.skip(4, "fuse_write_in.padding")?;
        (lock_owner, flags)
    } else {
        (0, 0)
    };
    let size_usize = usize::try_from(size)
        .map_err(|_| ProtocolError::new("fuse_write_in.size does not fit usize"))?;
    if size_usize > reader.remaining() {
        return Err(ProtocolError::new(format!(
            "fuse_write_in.size is {size} but only {} byte(s) of payload follow",
            reader.remaining()
        )));
    }
    let data = reader.raw(size_usize, "fuse_write_in.data")?;
    reader.end("fuse_write_in")?;
    Ok(FuseWriteIn {
        fh,
        offset,
        size,
        write_flags,
        lock_owner,
        flags,
        data,
    })
}

fn encode_write_in(value: &FuseWriteIn, ctx: ProtocolContext) -> Result<Vec<u8>, ProtocolError> {
    let head = read_write_in_size(ctx.minor);
    let mut writer = Writer::with_capacity(
        head.checked_add(value.data.len())
            .ok_or_else(|| ProtocolError::new("write request length overflows"))?,
    );
    let size = u32::try_from(value.data.len())
        .map_err(|_| ProtocolError::new("write payload exceeds u32"))?;
    writer.u64(value.fh);
    writer.u64(value.offset);
    writer.u32(size);
    writer.u32(value.write_flags);
    if ctx.minor >= 9 {
        writer.u64(value.lock_owner);
        writer.u32(value.flags);
        writer.skip(4);
    }
    writer.raw(&value.data);
    Ok(writer.finish())
}

fn decode_fsync_in(body: &[u8]) -> Result<FuseFsyncIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let fh = reader.u64("fuse_fsync_in.fh")?;
    let fsync_flags = reader.u32("fuse_fsync_in.fsync_flags")?;
    reader.skip(4, "fuse_fsync_in.padding")?;
    reader.end("fuse_fsync_in")?;
    Ok(FuseFsyncIn { fh, fsync_flags })
}

fn encode_fsync_in(value: &FuseFsyncIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(16);
    writer.u64(value.fh);
    writer.u32(value.fsync_flags);
    writer.skip(4);
    writer.finish()
}

fn decode_syncfs_in(body: &[u8]) -> Result<FuseSyncfsIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let padding = reader.u64("fuse_syncfs_in.padding")?;
    reader.end("fuse_syncfs_in")?;
    Ok(FuseSyncfsIn { padding })
}

fn encode_syncfs_in(value: &FuseSyncfsIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u64(value.padding);
    writer.finish()
}

fn decode_setxattr_in(body: &[u8], ctx: ProtocolContext) -> Result<FuseSetxattrIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let size = reader.u32("fuse_setxattr_in.size")?;
    let flags = reader.u32("fuse_setxattr_in.flags")?;
    let setxattr_flags = if ctx.setxattr_ext {
        let value = reader.u32("fuse_setxattr_in.setxattr_flags")?;
        reader.skip(4, "fuse_setxattr_in.padding")?;
        value
    } else {
        0
    };
    let name = reader.name("setxattr name")?;
    let size_usize = usize::try_from(size)
        .map_err(|_| ProtocolError::new("fuse_setxattr_in.size does not fit usize"))?;
    if size_usize > reader.remaining() {
        return Err(ProtocolError::new(format!(
            "fuse_setxattr_in.size is {size} but only {} byte(s) of value follow",
            reader.remaining()
        )));
    }
    let value = reader.raw(size_usize, "fuse_setxattr_in.value")?;
    reader.end("fuse_setxattr_in")?;
    Ok(FuseSetxattrIn {
        flags,
        setxattr_flags,
        name,
        value,
    })
}

fn encode_setxattr_in(
    value: &FuseSetxattrIn,
    ctx: ProtocolContext,
) -> Result<Vec<u8>, ProtocolError> {
    let head = if ctx.setxattr_ext {
        16
    } else {
        FUSE_COMPAT_SETXATTR_IN_SIZE
    };
    let capacity = head
        .checked_add(value.name.len() + 1)
        .and_then(|length| length.checked_add(value.value.len()))
        .ok_or_else(|| ProtocolError::new("setxattr request length overflows"))?;
    let size = u32::try_from(value.value.len())
        .map_err(|_| ProtocolError::new("setxattr value exceeds u32"))?;
    let mut writer = Writer::with_capacity(capacity);
    writer.u32(size);
    writer.u32(value.flags);
    if ctx.setxattr_ext {
        writer.u32(value.setxattr_flags);
        writer.skip(4);
    }
    write_name(&mut writer, &value.name, "setxattr name")?;
    writer.raw(&value.value);
    Ok(writer.finish())
}

fn decode_getxattr_in(body: &[u8]) -> Result<FuseGetxattrIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let size = reader.u32("fuse_getxattr_in.size")?;
    reader.skip(4, "fuse_getxattr_in.padding")?;
    let name = reader.name("getxattr name")?;
    reader.end("fuse_getxattr_in")?;
    Ok(FuseGetxattrIn { size, name })
}

fn encode_getxattr_in(value: &FuseGetxattrIn) -> Result<Vec<u8>, ProtocolError> {
    let mut writer = Writer::with_capacity(8 + value.name.len() + 1);
    writer.u32(value.size);
    writer.skip(4);
    write_name(&mut writer, &value.name, "getxattr name")?;
    Ok(writer.finish())
}

fn decode_listxattr_in(body: &[u8]) -> Result<FuseListxattrIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let size = reader.u32("fuse_listxattr_in.size")?;
    reader.skip(4, "fuse_listxattr_in.padding")?;
    reader.end("fuse_listxattr_in")?;
    Ok(FuseListxattrIn { size })
}

fn encode_listxattr_in(value: &FuseListxattrIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u32(value.size);
    writer.skip(4);
    writer.finish()
}

fn decode_lk_in(body: &[u8]) -> Result<FuseLkIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let fh = reader.u64("fuse_lk_in.fh")?;
    let owner = reader.u64("fuse_lk_in.owner")?;
    let lk = read_file_lock(&mut reader)?;
    let lk_flags = reader.u32("fuse_lk_in.lk_flags")?;
    reader.skip(4, "fuse_lk_in.padding")?;
    reader.end("fuse_lk_in")?;
    Ok(FuseLkIn {
        fh,
        owner,
        lk,
        lk_flags,
    })
}

fn encode_lk_in(value: &FuseLkIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(48);
    writer.u64(value.fh);
    writer.u64(value.owner);
    write_file_lock(&mut writer, &value.lk);
    writer.u32(value.lk_flags);
    writer.skip(4);
    writer.finish()
}

fn decode_access_in(body: &[u8]) -> Result<FuseAccessIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let mask = reader.u32("fuse_access_in.mask")?;
    reader.skip(4, "fuse_access_in.padding")?;
    reader.end("fuse_access_in")?;
    Ok(FuseAccessIn { mask })
}

fn encode_access_in(value: &FuseAccessIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u32(value.mask);
    writer.skip(4);
    writer.finish()
}

pub fn decode_init_in(body: &[u8]) -> Result<FuseInitIn, ProtocolError> {
    if body.len() < 8 {
        return Err(ProtocolError::at("truncated fuse_init_in", body.len()));
    }
    let mut reader = Reader::new(body);
    let major = reader.u32("fuse_init_in.major")?;
    let minor = reader.u32("fuse_init_in.minor")?;
    let (max_readahead, flags) = if reader.remaining() >= 8 {
        (
            reader.u32("fuse_init_in.max_readahead")?,
            reader.u32("fuse_init_in.flags")?,
        )
    } else {
        (0, 0)
    };
    let flags2 = if reader.remaining() >= 4 {
        reader.u32("fuse_init_in.flags2")?
    } else {
        0
    };
    Ok(FuseInitIn {
        major,
        minor,
        max_readahead,
        flags,
        flags2,
    })
}

pub fn encode_init_in(value: &FuseInitIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(64);
    writer.u32(value.major);
    writer.u32(value.minor);
    writer.u32(value.max_readahead);
    writer.u32(value.flags);
    writer.u32(value.flags2);
    writer.skip(44);
    writer.finish()
}

fn decode_interrupt_in(body: &[u8]) -> Result<FuseInterruptIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseInterruptIn {
        unique: reader.u64("fuse_interrupt_in.unique")?,
    };
    reader.end("fuse_interrupt_in")?;
    Ok(value)
}

fn encode_interrupt_in(value: &FuseInterruptIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u64(value.unique);
    writer.finish()
}

fn decode_poll_in(body: &[u8]) -> Result<FusePollIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FusePollIn {
        fh: reader.u64("fuse_poll_in.fh")?,
        kh: reader.u64("fuse_poll_in.kh")?,
        flags: reader.u32("fuse_poll_in.flags")?,
        events: reader.u32("fuse_poll_in.events")?,
    };
    reader.end("fuse_poll_in")?;
    Ok(value)
}

fn encode_poll_in(value: &FusePollIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(24);
    writer.u64(value.fh);
    writer.u64(value.kh);
    writer.u32(value.flags);
    writer.u32(value.events);
    writer.finish()
}

fn decode_fallocate_in(body: &[u8]) -> Result<FuseFallocateIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseFallocateIn {
        fh: reader.u64("fuse_fallocate_in.fh")?,
        offset: reader.u64("fuse_fallocate_in.offset")?,
        length: reader.u64("fuse_fallocate_in.length")?,
        mode: {
            let value = reader.u32("fuse_fallocate_in.mode")?;
            reader.skip(4, "fuse_fallocate_in.padding")?;
            value
        },
    };
    reader.end("fuse_fallocate_in")?;
    Ok(value)
}

fn encode_fallocate_in(value: &FuseFallocateIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(32);
    writer.u64(value.fh);
    writer.u64(value.offset);
    writer.u64(value.length);
    writer.u32(value.mode);
    writer.skip(4);
    writer.finish()
}

fn decode_ioctl_in(body: &[u8]) -> Result<FuseIoctlIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let mut value = FuseIoctlIn {
        fh: reader.u64("fuse_ioctl_in.fh")?,
        flags: reader.u32("fuse_ioctl_in.flags")?,
        cmd: reader.u32("fuse_ioctl_in.cmd")?,
        arg: reader.u64("fuse_ioctl_in.arg")?,
        in_size: reader.u32("fuse_ioctl_in.in_size")?,
        out_size: reader.u32("fuse_ioctl_in.out_size")?,
        data: Vec::new(),
    };
    value.data = reader.raw(
        usize::try_from(value.in_size)
            .map_err(|_| ProtocolError::new("fuse_ioctl_in.in_size does not fit usize"))?,
        "fuse_ioctl_in.data",
    )?;
    reader.end("fuse_ioctl_in")?;
    Ok(value)
}

fn encode_ioctl_in(value: &FuseIoctlIn) -> Result<Vec<u8>, ProtocolError> {
    let declared = usize::try_from(value.in_size)
        .map_err(|_| ProtocolError::new("fuse_ioctl_in.in_size does not fit usize"))?;
    if declared != value.data.len() {
        return Err(ProtocolError::new(format!(
            "fuse_ioctl_in.in_size is {} but data has {} byte(s)",
            value.in_size,
            value.data.len()
        )));
    }
    let mut writer = Writer::with_capacity(32 + value.data.len());
    writer.u64(value.fh);
    writer.u32(value.flags);
    writer.u32(value.cmd);
    writer.u64(value.arg);
    writer.u32(value.in_size);
    writer.u32(value.out_size);
    writer.raw(&value.data);
    Ok(writer.finish())
}

fn decode_lseek_in(body: &[u8]) -> Result<FuseLseekIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseLseekIn {
        fh: reader.u64("fuse_lseek_in.fh")?,
        offset: reader.u64("fuse_lseek_in.offset")?,
        whence: {
            let value = reader.u32("fuse_lseek_in.whence")?;
            reader.skip(4, "fuse_lseek_in.padding")?;
            value
        },
    };
    reader.end("fuse_lseek_in")?;
    Ok(value)
}

fn encode_lseek_in(value: &FuseLseekIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(24);
    writer.u64(value.fh);
    writer.u64(value.offset);
    writer.u32(value.whence);
    writer.skip(4);
    writer.finish()
}

fn decode_bmap_in(body: &[u8]) -> Result<FuseBmapIn, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseBmapIn {
        block: reader.u64("fuse_bmap_in.block")?,
        blocksize: {
            let value = reader.u32("fuse_bmap_in.blocksize")?;
            reader.skip(4, "fuse_bmap_in.padding")?;
            value
        },
    };
    reader.end("fuse_bmap_in")?;
    Ok(value)
}

fn encode_bmap_in(value: &FuseBmapIn) -> Vec<u8> {
    let mut writer = Writer::with_capacity(16);
    writer.u64(value.block);
    writer.u32(value.blocksize);
    writer.skip(4);
    writer.finish()
}

fn decode_create_out(body: &[u8], ctx: ProtocolContext) -> Result<FuseCreateOut, ProtocolError> {
    let mut reader = Reader::new(body);
    let entry = read_entry_out(&mut reader, ctx.minor)?;
    let open = read_open_out(&mut reader, ctx.minor)?;
    reader.end("fuse_create_out")?;
    Ok(FuseCreateOut { entry, open })
}

fn encode_create_out(value: &FuseCreateOut, ctx: ProtocolContext) -> Vec<u8> {
    let mut writer = Writer::with_capacity(entry_out_size(ctx.minor) + 16);
    write_entry_out(&mut writer, &value.entry, ctx.minor);
    write_open_out(&mut writer, &value.open, ctx.minor);
    writer.finish()
}

fn decode_write_out(body: &[u8]) -> Result<FuseWriteOut, ProtocolError> {
    let mut reader = Reader::new(body);
    let size = reader.u32("fuse_write_out.size")?;
    reader.skip(4, "fuse_write_out.padding")?;
    reader.end("fuse_write_out")?;
    Ok(FuseWriteOut { size })
}

fn encode_write_out(value: &FuseWriteOut) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u32(value.size);
    writer.skip(4);
    writer.finish()
}

pub fn decode_statfs_out(
    body: &[u8],
    ctx: Option<ProtocolContext>,
) -> Result<FuseKstatfs, ProtocolError> {
    let minor = context(ctx).minor;
    let mut reader = Reader::new(body);
    let value = FuseKstatfs {
        blocks: reader.u64("fuse_kstatfs.blocks")?,
        bfree: reader.u64("fuse_kstatfs.bfree")?,
        bavail: reader.u64("fuse_kstatfs.bavail")?,
        files: reader.u64("fuse_kstatfs.files")?,
        ffree: reader.u64("fuse_kstatfs.ffree")?,
        bsize: reader.u32("fuse_kstatfs.bsize")?,
        namelen: reader.u32("fuse_kstatfs.namelen")?,
        frsize: if minor >= 4 {
            let value = reader.u32("fuse_kstatfs.frsize")?;
            reader.skip(4 + 24, "fuse_kstatfs.spare")?;
            value
        } else {
            0
        },
    };
    reader.end("fuse_statfs_out")?;
    Ok(value)
}

pub fn encode_statfs_out(value: &FuseKstatfs, ctx: Option<ProtocolContext>) -> Vec<u8> {
    let minor = context(ctx).minor;
    let mut writer = Writer::with_capacity(kstatfs_size(minor));
    writer.u64(value.blocks);
    writer.u64(value.bfree);
    writer.u64(value.bavail);
    writer.u64(value.files);
    writer.u64(value.ffree);
    writer.u32(value.bsize);
    writer.u32(value.namelen);
    if minor >= 4 {
        writer.u32(value.frsize);
        writer.skip(4 + 24);
    }
    writer.finish()
}

pub fn decode_getxattr_out(body: &[u8]) -> Result<FuseGetxattrOut, ProtocolError> {
    let mut reader = Reader::new(body);
    let size = reader.u32("fuse_getxattr_out.size")?;
    reader.skip(4, "fuse_getxattr_out.padding")?;
    reader.end("fuse_getxattr_out")?;
    Ok(FuseGetxattrOut { size })
}

pub fn encode_getxattr_out(value: &FuseGetxattrOut) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u32(value.size);
    writer.skip(4);
    writer.finish()
}

pub fn encode_xattr_names(names: &[String]) -> Result<Vec<u8>, ProtocolError> {
    let mut capacity = 0usize;
    for name in names {
        name_bytes(name, "xattr name")?;
        capacity = capacity
            .checked_add(name.len() + 1)
            .ok_or_else(|| ProtocolError::new("xattr name list length overflows"))?;
    }
    let mut writer = Writer::with_capacity(capacity);
    for name in names {
        write_name(&mut writer, name, "xattr name")?;
    }
    Ok(writer.finish())
}

pub fn decode_xattr_names(body: &[u8]) -> Result<Vec<String>, ProtocolError> {
    let mut reader = Reader::new(body);
    let mut names = Vec::new();
    while reader.remaining() > 0 {
        names.push(reader.name("xattr name")?);
    }
    Ok(names)
}

fn decode_lk_out(body: &[u8]) -> Result<FuseLkOut, ProtocolError> {
    let mut reader = Reader::new(body);
    let lk = read_file_lock(&mut reader)?;
    reader.end("fuse_lk_out")?;
    Ok(FuseLkOut { lk })
}

fn encode_lk_out(value: &FuseLkOut) -> Vec<u8> {
    let mut writer = Writer::with_capacity(24);
    write_file_lock(&mut writer, &value.lk);
    writer.finish()
}

pub fn decode_init_out(body: &[u8]) -> Result<FuseInitOut, ProtocolError> {
    if body.len() < 8 {
        return Err(ProtocolError::at("truncated fuse_init_out", body.len()));
    }
    let mut reader = Reader::new(body);
    let major = reader.u32("fuse_init_out.major")?;
    let minor = reader.u32("fuse_init_out.minor")?;
    let mut value = FuseInitOut {
        major,
        minor,
        max_readahead: 0,
        flags: 0,
        max_background: 0,
        congestion_threshold: 0,
        max_write: 0,
        time_gran: 0,
        max_pages: 0,
        map_alignment: 0,
        flags2: 0,
        max_stack_depth: 0,
    };
    if reader.remaining() >= 8 {
        value.max_readahead = reader.u32("fuse_init_out.max_readahead")?;
        value.flags = reader.u32("fuse_init_out.flags")?;
    }
    if reader.remaining() >= 8 {
        value.max_background = reader.u16("fuse_init_out.max_background")?;
        value.congestion_threshold = reader.u16("fuse_init_out.congestion_threshold")?;
        value.max_write = reader.u32("fuse_init_out.max_write")?;
    }
    if reader.remaining() >= 4 {
        value.time_gran = reader.u32("fuse_init_out.time_gran")?;
    }
    if reader.remaining() >= 4 {
        value.max_pages = reader.u16("fuse_init_out.max_pages")?;
        value.map_alignment = reader.u16("fuse_init_out.map_alignment")?;
    }
    if reader.remaining() >= 4 {
        value.flags2 = reader.u32("fuse_init_out.flags2")?;
    }
    if reader.remaining() >= 4 {
        value.max_stack_depth = reader.u32("fuse_init_out.max_stack_depth")?;
    }
    Ok(value)
}

pub fn encode_init_out(
    value: &FuseInitOut,
    ctx: Option<ProtocolContext>,
) -> Result<Vec<u8>, ProtocolError> {
    if let Some(ctx) = ctx
        && ctx.minor != value.minor
    {
        return Err(ProtocolError::new(format!(
            "fuse_init_out.minor is {} but the session negotiated 7.{}",
            value.minor, ctx.minor
        )));
    }
    let mut writer = Writer::with_capacity(init_out_size(value.minor));
    writer.u32(value.major);
    writer.u32(value.minor);
    if value.minor < 5 {
        return Ok(writer.finish());
    }
    writer.u32(value.max_readahead);
    writer.u32(value.flags);
    writer.u16(value.max_background);
    writer.u16(value.congestion_threshold);
    writer.u32(value.max_write);
    if value.minor < 23 {
        return Ok(writer.finish());
    }
    writer.u32(value.time_gran);
    writer.u16(value.max_pages);
    writer.u16(value.map_alignment);
    writer.u32(value.flags2);
    writer.u32(value.max_stack_depth);
    writer.skip(24);
    Ok(writer.finish())
}

fn decode_poll_out(body: &[u8]) -> Result<FusePollOut, ProtocolError> {
    let mut reader = Reader::new(body);
    let revents = reader.u32("fuse_poll_out.revents")?;
    reader.skip(4, "fuse_poll_out.padding")?;
    reader.end("fuse_poll_out")?;
    Ok(FusePollOut { revents })
}

fn encode_poll_out(value: &FusePollOut) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u32(value.revents);
    writer.skip(4);
    writer.finish()
}

fn decode_lseek_out(body: &[u8]) -> Result<FuseLseekOut, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseLseekOut {
        offset: reader.u64("fuse_lseek_out.offset")?,
    };
    reader.end("fuse_lseek_out")?;
    Ok(value)
}

fn encode_lseek_out(value: &FuseLseekOut) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u64(value.offset);
    writer.finish()
}

fn decode_bmap_out(body: &[u8]) -> Result<FuseBmapOut, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseBmapOut {
        block: reader.u64("fuse_bmap_out.block")?,
    };
    reader.end("fuse_bmap_out")?;
    Ok(value)
}

fn encode_bmap_out(value: &FuseBmapOut) -> Vec<u8> {
    let mut writer = Writer::with_capacity(8);
    writer.u64(value.block);
    writer.finish()
}

fn decode_ioctl_out(body: &[u8]) -> Result<FuseIoctlOut, ProtocolError> {
    let mut reader = Reader::new(body);
    let value = FuseIoctlOut {
        result: reader.i32("fuse_ioctl_out.result")?,
        flags: reader.u32("fuse_ioctl_out.flags")?,
        in_iovs: reader.u32("fuse_ioctl_out.in_iovs")?,
        out_iovs: reader.u32("fuse_ioctl_out.out_iovs")?,
    };
    reader.end("fuse_ioctl_out")?;
    Ok(value)
}

fn encode_ioctl_out(value: &FuseIoctlOut) -> Vec<u8> {
    let mut writer = Writer::with_capacity(16);
    writer.i32(value.result);
    writer.u32(value.flags);
    writer.u32(value.in_iovs);
    writer.u32(value.out_iovs);
    writer.finish()
}

fn decode_readlink_out(body: &[u8]) -> FuseReadlinkOut {
    let mut reader = Reader::new(body);
    FuseReadlinkOut {
        target: reader.rest_string(),
    }
}

fn encode_readlink_out(value: &FuseReadlinkOut) -> Result<Vec<u8>, ProtocolError> {
    Ok(name_bytes(&value.target, "readlink target")?.to_vec())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseDirent {
    pub ino: u64,
    pub off: u64,
    pub type_: u32,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseDirentPlus {
    pub entry: FuseEntryOut,
    pub dirent: FuseDirent,
}

pub fn dirent_align(size: usize) -> usize {
    size.saturating_add(7) & !7
}

pub fn dirent_size(name_byte_length: usize) -> usize {
    dirent_align(FUSE_DIRENT_HEADER_SIZE.saturating_add(name_byte_length))
}

pub fn dirent_plus_size(name_byte_length: usize, ctx: Option<ProtocolContext>) -> usize {
    dirent_align(
        entry_out_size(context(ctx).minor)
            .saturating_add(FUSE_DIRENT_HEADER_SIZE)
            .saturating_add(name_byte_length),
    )
}

pub fn dirent_type(mode: u32) -> u32 {
    (mode & 0o170000) >> 12
}

/// A size-bounded `READDIR`/`READDIRPLUS` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackedDirents {
    pub buffer: Vec<u8>,
    pub packed: usize,
}

/// Incremental size-bounded dirent writer. `plus` selects the
/// `fuse_entry_out` prefix used by `READDIRPLUS`.
#[derive(Debug, Clone)]
pub struct DirentPacker {
    max_size: usize,
    plus: bool,
    ctx: ProtocolContext,
    chunks: Vec<Vec<u8>>,
    used: usize,
}

impl DirentPacker {
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            plus: false,
            ctx: DEFAULT_PROTOCOL,
            chunks: Vec::new(),
            used: 0,
        }
    }

    pub fn new_plus(max_size: usize, ctx: Option<ProtocolContext>) -> Self {
        Self {
            max_size,
            plus: true,
            ctx: context(ctx),
            chunks: Vec::new(),
            used: 0,
        }
    }

    pub fn max_size(&self) -> usize {
        self.max_size
    }

    pub fn size(&self) -> usize {
        self.used
    }

    pub fn count(&self) -> usize {
        self.chunks.len()
    }

    pub fn remaining(&self) -> usize {
        self.max_size.saturating_sub(self.used)
    }

    /// Add one complete record. A record that does not fit is not partially
    /// written, so the caller can retry it with a larger page.
    pub fn add(
        &mut self,
        dirent: &FuseDirent,
        entry: Option<&FuseEntryOut>,
    ) -> Result<bool, ProtocolError> {
        if self.plus && entry.is_none() {
            return Err(ProtocolError::new("fuse_direntplus needs a fuse_entry_out"));
        }
        let name = name_bytes(&dirent.name, "dirent name")?;
        let prefix = if self.plus {
            entry_out_size(self.ctx.minor)
        } else {
            0
        };
        let header_size = prefix + FUSE_DIRENT_HEADER_SIZE;
        let total = dirent_align(header_size.saturating_add(name.len()));
        if total > self.max_size.saturating_sub(self.used) {
            return Ok(false);
        }
        let mut writer = Writer::with_capacity(total);
        if let Some(entry) = entry
            && self.plus
        {
            write_entry_out(&mut writer, entry, self.ctx.minor);
        }
        writer.u64(dirent.ino);
        writer.u64(dirent.off);
        writer.u32(
            u32::try_from(name.len())
                .map_err(|_| ProtocolError::new("dirent name exceeds u32 length"))?,
        );
        writer.u32(dirent.type_);
        writer.raw(name);
        writer.skip(total - header_size - name.len());
        self.chunks.push(writer.finish());
        self.used += total;
        Ok(true)
    }

    pub fn build(&self) -> Vec<u8> {
        let mut buffer = Vec::with_capacity(self.used);
        for chunk in &self.chunks {
            buffer.extend_from_slice(chunk);
        }
        buffer
    }
}

pub fn pack_dirents(
    entries: &[FuseDirent],
    max_size: usize,
) -> Result<PackedDirents, ProtocolError> {
    let mut packer = DirentPacker::new(max_size);
    for dirent in entries {
        if !packer.add(dirent, None)? {
            break;
        }
    }
    Ok(PackedDirents {
        buffer: packer.build(),
        packed: packer.count(),
    })
}

pub fn pack_dirents_plus(
    entries: &[FuseDirentPlus],
    max_size: usize,
    ctx: Option<ProtocolContext>,
) -> Result<PackedDirents, ProtocolError> {
    let mut packer = DirentPacker::new_plus(max_size, ctx);
    for value in entries {
        if !packer.add(&value.dirent, Some(&value.entry))? {
            break;
        }
    }
    Ok(PackedDirents {
        buffer: packer.build(),
        packed: packer.count(),
    })
}

fn read_dirent(reader: &mut Reader<'_>) -> Result<FuseDirent, ProtocolError> {
    let ino = reader.u64("fuse_dirent.ino")?;
    let off = reader.u64("fuse_dirent.off")?;
    let namelen = usize::try_from(reader.u32("fuse_dirent.namelen")?)
        .map_err(|_| ProtocolError::new("fuse_dirent.namelen does not fit usize"))?;
    let type_ = reader.u32("fuse_dirent.type")?;
    if namelen > reader.remaining() {
        return Err(ProtocolError::at(
            format!(
                "fuse_dirent.namelen is {namelen} but only {} byte(s) remain",
                reader.remaining()
            ),
            reader.offset,
        ));
    }
    let name = String::from_utf8_lossy(&reader.raw(namelen, "fuse_dirent.name")?).into_owned();
    Ok(FuseDirent {
        ino,
        off,
        type_,
        name,
    })
}

pub fn unpack_dirents(body: &[u8]) -> Result<Vec<FuseDirent>, ProtocolError> {
    let mut reader = Reader::new(body);
    let mut entries = Vec::new();
    while reader.remaining() > 0 {
        let start = reader.offset;
        let dirent = read_dirent(&mut reader)?;
        let record_size = reader.offset - start;
        let padded = start
            .checked_add(dirent_align(record_size))
            .ok_or_else(|| ProtocolError::new("fuse_dirent alignment overflows"))?;
        if padded > body.len() {
            return Err(ProtocolError::at(
                "fuse_dirent padding runs past the end of the buffer",
                reader.offset,
            ));
        }
        reader.skip(padded - reader.offset, "fuse_dirent padding")?;
        entries.push(dirent);
    }
    Ok(entries)
}

pub fn unpack_dirents_plus(
    body: &[u8],
    ctx: Option<ProtocolContext>,
) -> Result<Vec<FuseDirentPlus>, ProtocolError> {
    let minor = context(ctx).minor;
    let mut reader = Reader::new(body);
    let mut entries = Vec::new();
    while reader.remaining() > 0 {
        let start = reader.offset;
        let entry = read_entry_out(&mut reader, minor)?;
        let dirent = read_dirent(&mut reader)?;
        let record_size = reader.offset - start;
        let padded = start
            .checked_add(dirent_align(record_size))
            .ok_or_else(|| ProtocolError::new("fuse_direntplus alignment overflows"))?;
        if padded > body.len() {
            return Err(ProtocolError::at(
                "fuse_direntplus padding runs past the end of the buffer",
                reader.offset,
            ));
        }
        reader.skip(padded - reader.offset, "fuse_direntplus padding")?;
        entries.push(FuseDirentPlus { entry, dirent });
    }
    Ok(entries)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FuseRequestBody {
    Empty,
    Raw(Vec<u8>),
    Name(FuseNameIn),
    Forget(FuseForgetIn),
    BatchForget(FuseBatchForgetIn),
    Getattr(FuseGetattrIn),
    Setattr(FuseSetattrIn),
    Symlink(FuseSymlinkIn),
    Mknod(FuseMknodIn),
    Mkdir(FuseMkdirIn),
    Rename(FuseRenameIn),
    Rename2(FuseRename2In),
    Link(FuseLinkIn),
    Open(FuseOpenIn),
    Create(FuseCreateIn),
    Read(FuseReadIn),
    Write(FuseWriteIn),
    Release(FuseReleaseIn),
    Flush(FuseFlushIn),
    Fsync(FuseFsyncIn),
    Syncfs(FuseSyncfsIn),
    Setxattr(FuseSetxattrIn),
    Getxattr(FuseGetxattrIn),
    Listxattr(FuseListxattrIn),
    Lk(FuseLkIn),
    Access(FuseAccessIn),
    Init(FuseInitIn),
    Interrupt(FuseInterruptIn),
    Poll(FusePollIn),
    Fallocate(FuseFallocateIn),
    Ioctl(FuseIoctlIn),
    Lseek(FuseLseekIn),
    Bmap(FuseBmapIn),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FuseReplyBody {
    Empty,
    Raw(Vec<u8>),
    Readlink(FuseReadlinkOut),
    Entry(FuseEntryOut),
    Attr(FuseAttrOut),
    Open(FuseOpenOut),
    Create(FuseCreateOut),
    Write(FuseWriteOut),
    Statfs(FuseKstatfs),
    Lk(FuseLkOut),
    Init(FuseInitOut),
    Poll(FusePollOut),
    Lseek(FuseLseekOut),
    Bmap(FuseBmapOut),
    Ioctl(FuseIoctlOut),
    Dirents(Vec<FuseDirent>),
    DirentsPlus(Vec<FuseDirentPlus>),
}

fn unsupported_opcode(opcode: u32) -> ProtocolError {
    ProtocolError::new(format!(
        "no codec for opcode {} ({})",
        opcode,
        if opcode_name(opcode) == "UNKNOWN" {
            format!("UNKNOWN({opcode})")
        } else {
            opcode_name(opcode)
        }
    ))
}

fn wrong_request_body(opcode: u32, expected: &str) -> ProtocolError {
    ProtocolError::new(format!(
        "{} expects {expected} request body",
        opcode_name(opcode)
    ))
}

fn wrong_reply_body(opcode: u32, expected: &str) -> ProtocolError {
    ProtocolError::new(format!(
        "{} expects {expected} reply body",
        opcode_name(opcode)
    ))
}

pub fn decode_request_body(
    opcode: u32,
    body: &[u8],
    ctx: Option<ProtocolContext>,
) -> Result<FuseRequestBody, ProtocolError> {
    let ctx = context(ctx);
    let decoded = match opcode {
        FUSE_LOOKUP | FUSE_UNLINK | FUSE_RMDIR | FUSE_REMOVEXATTR => {
            FuseRequestBody::Name(decode_name_in(body, &opcode_name(opcode))?)
        }
        FUSE_FORGET => FuseRequestBody::Forget(decode_forget_in(body)?),
        FUSE_BATCH_FORGET => FuseRequestBody::BatchForget(decode_batch_forget_in(body)?),
        FUSE_GETATTR => FuseRequestBody::Getattr(decode_getattr_in(body)?),
        FUSE_SETATTR => FuseRequestBody::Setattr(decode_setattr_in(body)?),
        FUSE_READLINK | FUSE_STATFS | FUSE_DESTROY => {
            decode_empty(body, &opcode_name(opcode))?;
            FuseRequestBody::Empty
        }
        FUSE_SYMLINK => FuseRequestBody::Symlink(decode_symlink_in(body)?),
        FUSE_MKNOD => FuseRequestBody::Mknod(decode_mknod_in(body, ctx)?),
        FUSE_MKDIR => FuseRequestBody::Mkdir(decode_mkdir_in(body)?),
        FUSE_RENAME => FuseRequestBody::Rename(decode_rename_in(body)?),
        FUSE_RENAME2 => FuseRequestBody::Rename2(decode_rename2_in(body)?),
        FUSE_LINK => FuseRequestBody::Link(decode_link_in(body)?),
        FUSE_OPEN | FUSE_OPENDIR => FuseRequestBody::Open(decode_open_in(body)?),
        FUSE_CREATE => FuseRequestBody::Create(decode_create_in(body, ctx)?),
        FUSE_READ | FUSE_READDIR | FUSE_READDIRPLUS => {
            FuseRequestBody::Read(decode_read_in(body, ctx)?)
        }
        FUSE_WRITE => FuseRequestBody::Write(decode_write_in(body, ctx)?),
        FUSE_RELEASE | FUSE_RELEASEDIR => FuseRequestBody::Release(decode_release_in(body)?),
        FUSE_FLUSH => FuseRequestBody::Flush(decode_flush_in(body)?),
        FUSE_FSYNC | FUSE_FSYNCDIR => FuseRequestBody::Fsync(decode_fsync_in(body)?),
        FUSE_SYNCFS => FuseRequestBody::Syncfs(decode_syncfs_in(body)?),
        FUSE_SETXATTR => FuseRequestBody::Setxattr(decode_setxattr_in(body, ctx)?),
        FUSE_GETXATTR => FuseRequestBody::Getxattr(decode_getxattr_in(body)?),
        FUSE_LISTXATTR => FuseRequestBody::Listxattr(decode_listxattr_in(body)?),
        FUSE_GETLK | FUSE_SETLK | FUSE_SETLKW => FuseRequestBody::Lk(decode_lk_in(body)?),
        FUSE_ACCESS => FuseRequestBody::Access(decode_access_in(body)?),
        FUSE_INIT => FuseRequestBody::Init(decode_init_in(body)?),
        FUSE_INTERRUPT => FuseRequestBody::Interrupt(decode_interrupt_in(body)?),
        FUSE_POLL => FuseRequestBody::Poll(decode_poll_in(body)?),
        FUSE_FALLOCATE => FuseRequestBody::Fallocate(decode_fallocate_in(body)?),
        FUSE_IOCTL => FuseRequestBody::Ioctl(decode_ioctl_in(body)?),
        FUSE_LSEEK => FuseRequestBody::Lseek(decode_lseek_in(body)?),
        FUSE_BMAP => FuseRequestBody::Bmap(decode_bmap_in(body)?),
        _ => return Err(unsupported_opcode(opcode)),
    };
    Ok(decoded)
}

pub fn encode_request_body(
    opcode: u32,
    body: &FuseRequestBody,
    ctx: Option<ProtocolContext>,
) -> Result<Vec<u8>, ProtocolError> {
    let ctx = context(ctx);
    match opcode {
        FUSE_LOOKUP | FUSE_UNLINK | FUSE_RMDIR | FUSE_REMOVEXATTR => match body {
            FuseRequestBody::Name(value) => encode_name_in(value, &opcode_name(opcode)),
            _ => Err(wrong_request_body(opcode, "a name")),
        },
        FUSE_FORGET => match body {
            FuseRequestBody::Forget(value) => Ok(encode_forget_in(value)),
            _ => Err(wrong_request_body(opcode, "forget")),
        },
        FUSE_BATCH_FORGET => match body {
            FuseRequestBody::BatchForget(value) => encode_batch_forget_in(value),
            _ => Err(wrong_request_body(opcode, "batch forget")),
        },
        FUSE_GETATTR => match body {
            FuseRequestBody::Getattr(value) => Ok(encode_getattr_in(value)),
            _ => Err(wrong_request_body(opcode, "getattr")),
        },
        FUSE_SETATTR => match body {
            FuseRequestBody::Setattr(value) => Ok(encode_setattr_in(value)),
            _ => Err(wrong_request_body(opcode, "setattr")),
        },
        FUSE_READLINK | FUSE_STATFS | FUSE_DESTROY => match body {
            FuseRequestBody::Empty => Ok(Vec::new()),
            _ => Err(wrong_request_body(opcode, "empty")),
        },
        FUSE_SYMLINK => match body {
            FuseRequestBody::Symlink(value) => encode_symlink_in(value),
            _ => Err(wrong_request_body(opcode, "symlink")),
        },
        FUSE_MKNOD => match body {
            FuseRequestBody::Mknod(value) => encode_mknod_in(value, ctx),
            _ => Err(wrong_request_body(opcode, "mknod")),
        },
        FUSE_MKDIR => match body {
            FuseRequestBody::Mkdir(value) => encode_mkdir_in(value),
            _ => Err(wrong_request_body(opcode, "mkdir")),
        },
        FUSE_RENAME => match body {
            FuseRequestBody::Rename(value) => encode_rename_in(value),
            _ => Err(wrong_request_body(opcode, "rename")),
        },
        FUSE_RENAME2 => match body {
            FuseRequestBody::Rename2(value) => encode_rename2_in(value),
            _ => Err(wrong_request_body(opcode, "rename2")),
        },
        FUSE_LINK => match body {
            FuseRequestBody::Link(value) => encode_link_in(value),
            _ => Err(wrong_request_body(opcode, "link")),
        },
        FUSE_OPEN | FUSE_OPENDIR => match body {
            FuseRequestBody::Open(value) => Ok(encode_open_in(value)),
            _ => Err(wrong_request_body(opcode, "open")),
        },
        FUSE_CREATE => match body {
            FuseRequestBody::Create(value) => encode_create_in(value, ctx),
            _ => Err(wrong_request_body(opcode, "create")),
        },
        FUSE_READ | FUSE_READDIR | FUSE_READDIRPLUS => match body {
            FuseRequestBody::Read(value) => Ok(encode_read_in(value, ctx)),
            _ => Err(wrong_request_body(opcode, "read")),
        },
        FUSE_WRITE => match body {
            FuseRequestBody::Write(value) => encode_write_in(value, ctx),
            _ => Err(wrong_request_body(opcode, "write")),
        },
        FUSE_RELEASE | FUSE_RELEASEDIR => match body {
            FuseRequestBody::Release(value) => Ok(encode_release_in(value)),
            _ => Err(wrong_request_body(opcode, "release")),
        },
        FUSE_FLUSH => match body {
            FuseRequestBody::Flush(value) => Ok(encode_flush_in(value)),
            _ => Err(wrong_request_body(opcode, "flush")),
        },
        FUSE_FSYNC | FUSE_FSYNCDIR => match body {
            FuseRequestBody::Fsync(value) => Ok(encode_fsync_in(value)),
            _ => Err(wrong_request_body(opcode, "fsync")),
        },
        FUSE_SYNCFS => match body {
            FuseRequestBody::Syncfs(value) => Ok(encode_syncfs_in(value)),
            _ => Err(wrong_request_body(opcode, "syncfs")),
        },
        FUSE_SETXATTR => match body {
            FuseRequestBody::Setxattr(value) => encode_setxattr_in(value, ctx),
            _ => Err(wrong_request_body(opcode, "setxattr")),
        },
        FUSE_GETXATTR => match body {
            FuseRequestBody::Getxattr(value) => encode_getxattr_in(value),
            _ => Err(wrong_request_body(opcode, "getxattr")),
        },
        FUSE_LISTXATTR => match body {
            FuseRequestBody::Listxattr(value) => Ok(encode_listxattr_in(value)),
            _ => Err(wrong_request_body(opcode, "listxattr")),
        },
        FUSE_GETLK | FUSE_SETLK | FUSE_SETLKW => match body {
            FuseRequestBody::Lk(value) => Ok(encode_lk_in(value)),
            _ => Err(wrong_request_body(opcode, "lock")),
        },
        FUSE_ACCESS => match body {
            FuseRequestBody::Access(value) => Ok(encode_access_in(value)),
            _ => Err(wrong_request_body(opcode, "access")),
        },
        FUSE_INIT => match body {
            FuseRequestBody::Init(value) => Ok(encode_init_in(value)),
            _ => Err(wrong_request_body(opcode, "init")),
        },
        FUSE_INTERRUPT => match body {
            FuseRequestBody::Interrupt(value) => Ok(encode_interrupt_in(value)),
            _ => Err(wrong_request_body(opcode, "interrupt")),
        },
        FUSE_POLL => match body {
            FuseRequestBody::Poll(value) => Ok(encode_poll_in(value)),
            _ => Err(wrong_request_body(opcode, "poll")),
        },
        FUSE_FALLOCATE => match body {
            FuseRequestBody::Fallocate(value) => Ok(encode_fallocate_in(value)),
            _ => Err(wrong_request_body(opcode, "fallocate")),
        },
        FUSE_IOCTL => match body {
            FuseRequestBody::Ioctl(value) => encode_ioctl_in(value),
            _ => Err(wrong_request_body(opcode, "ioctl")),
        },
        FUSE_LSEEK => match body {
            FuseRequestBody::Lseek(value) => Ok(encode_lseek_in(value)),
            _ => Err(wrong_request_body(opcode, "lseek")),
        },
        FUSE_BMAP => match body {
            FuseRequestBody::Bmap(value) => Ok(encode_bmap_in(value)),
            _ => Err(wrong_request_body(opcode, "bmap")),
        },
        _ => Err(unsupported_opcode(opcode)),
    }
}

pub fn decode_reply_body(
    opcode: u32,
    body: &[u8],
    ctx: Option<ProtocolContext>,
) -> Result<FuseReplyBody, ProtocolError> {
    let ctx = context(ctx);
    let decoded = match opcode {
        FUSE_READLINK => FuseReplyBody::Readlink(decode_readlink_out(body)),
        FUSE_LOOKUP | FUSE_SYMLINK | FUSE_MKNOD | FUSE_MKDIR | FUSE_LINK => {
            FuseReplyBody::Entry(decode_entry_out(body, Some(ctx))?)
        }
        FUSE_GETATTR | FUSE_SETATTR => FuseReplyBody::Attr(decode_attr_out(body, Some(ctx))?),
        FUSE_OPEN | FUSE_OPENDIR => FuseReplyBody::Open(decode_open_out(body, Some(ctx))?),
        FUSE_CREATE => FuseReplyBody::Create(decode_create_out(body, ctx)?),
        FUSE_READ | FUSE_GETXATTR | FUSE_LISTXATTR => FuseReplyBody::Raw(body.to_vec()),
        FUSE_WRITE => FuseReplyBody::Write(decode_write_out(body)?),
        FUSE_STATFS => FuseReplyBody::Statfs(decode_statfs_out(body, Some(ctx))?),
        FUSE_READDIR => FuseReplyBody::Dirents(unpack_dirents(body)?),
        FUSE_READDIRPLUS => FuseReplyBody::DirentsPlus(unpack_dirents_plus(body, Some(ctx))?),
        FUSE_INIT => FuseReplyBody::Init(decode_init_out(body)?),
        FUSE_LSEEK => FuseReplyBody::Lseek(decode_lseek_out(body)?),
        FUSE_GETLK => FuseReplyBody::Lk(decode_lk_out(body)?),
        FUSE_POLL => FuseReplyBody::Poll(decode_poll_out(body)?),
        FUSE_BMAP => FuseReplyBody::Bmap(decode_bmap_out(body)?),
        FUSE_IOCTL => FuseReplyBody::Ioctl(decode_ioctl_out(body)?),
        FUSE_FORGET | FUSE_BATCH_FORGET | FUSE_UNLINK | FUSE_RMDIR | FUSE_RENAME | FUSE_RENAME2
        | FUSE_RELEASE | FUSE_RELEASEDIR | FUSE_FSYNC | FUSE_FSYNCDIR | FUSE_FLUSH
        | FUSE_ACCESS | FUSE_DESTROY | FUSE_INTERRUPT | FUSE_SETXATTR | FUSE_REMOVEXATTR
        | FUSE_FALLOCATE | FUSE_SETLK | FUSE_SETLKW | FUSE_SYNCFS => {
            decode_empty(body, &opcode_name(opcode))?;
            FuseReplyBody::Empty
        }
        _ => return Err(unsupported_opcode(opcode)),
    };
    Ok(decoded)
}

pub fn encode_reply_body(
    opcode: u32,
    body: &FuseReplyBody,
    ctx: Option<ProtocolContext>,
) -> Result<Vec<u8>, ProtocolError> {
    let ctx = context(ctx);
    match opcode {
        FUSE_READLINK => match body {
            FuseReplyBody::Readlink(value) => encode_readlink_out(value),
            _ => Err(wrong_reply_body(opcode, "readlink")),
        },
        FUSE_LOOKUP | FUSE_SYMLINK | FUSE_MKNOD | FUSE_MKDIR | FUSE_LINK => match body {
            FuseReplyBody::Entry(value) => Ok(encode_entry_out(value, Some(ctx))),
            _ => Err(wrong_reply_body(opcode, "entry")),
        },
        FUSE_GETATTR | FUSE_SETATTR => match body {
            FuseReplyBody::Attr(value) => Ok(encode_attr_out(value, Some(ctx))),
            _ => Err(wrong_reply_body(opcode, "attributes")),
        },
        FUSE_OPEN | FUSE_OPENDIR => match body {
            FuseReplyBody::Open(value) => Ok(encode_open_out(value, Some(ctx))),
            _ => Err(wrong_reply_body(opcode, "open")),
        },
        FUSE_CREATE => match body {
            FuseReplyBody::Create(value) => Ok(encode_create_out(value, ctx)),
            _ => Err(wrong_reply_body(opcode, "create")),
        },
        FUSE_READ | FUSE_GETXATTR | FUSE_LISTXATTR => match body {
            FuseReplyBody::Raw(value) => Ok(value.clone()),
            _ => Err(wrong_reply_body(opcode, "raw data")),
        },
        FUSE_WRITE => match body {
            FuseReplyBody::Write(value) => Ok(encode_write_out(value)),
            _ => Err(wrong_reply_body(opcode, "write")),
        },
        FUSE_STATFS => match body {
            FuseReplyBody::Statfs(value) => Ok(encode_statfs_out(value, Some(ctx))),
            _ => Err(wrong_reply_body(opcode, "statfs")),
        },
        FUSE_READDIR => match body {
            FuseReplyBody::Dirents(values) => Ok(pack_dirents(values, usize::MAX)?.buffer),
            _ => Err(wrong_reply_body(opcode, "dirents")),
        },
        FUSE_READDIRPLUS => match body {
            FuseReplyBody::DirentsPlus(values) => {
                Ok(pack_dirents_plus(values, usize::MAX, Some(ctx))?.buffer)
            }
            _ => Err(wrong_reply_body(opcode, "direntsplus")),
        },
        FUSE_INIT => match body {
            FuseReplyBody::Init(value) => encode_init_out(value, Some(ctx)),
            _ => Err(wrong_reply_body(opcode, "init")),
        },
        FUSE_LSEEK => match body {
            FuseReplyBody::Lseek(value) => Ok(encode_lseek_out(value)),
            _ => Err(wrong_reply_body(opcode, "lseek")),
        },
        FUSE_GETLK => match body {
            FuseReplyBody::Lk(value) => Ok(encode_lk_out(value)),
            _ => Err(wrong_reply_body(opcode, "lock")),
        },
        FUSE_POLL => match body {
            FuseReplyBody::Poll(value) => Ok(encode_poll_out(value)),
            _ => Err(wrong_reply_body(opcode, "poll")),
        },
        FUSE_BMAP => match body {
            FuseReplyBody::Bmap(value) => Ok(encode_bmap_out(value)),
            _ => Err(wrong_reply_body(opcode, "bmap")),
        },
        FUSE_IOCTL => match body {
            FuseReplyBody::Ioctl(value) => Ok(encode_ioctl_out(value)),
            _ => Err(wrong_reply_body(opcode, "ioctl")),
        },
        FUSE_FORGET | FUSE_BATCH_FORGET | FUSE_UNLINK | FUSE_RMDIR | FUSE_RENAME | FUSE_RENAME2
        | FUSE_RELEASE | FUSE_RELEASEDIR | FUSE_FSYNC | FUSE_FSYNCDIR | FUSE_FLUSH
        | FUSE_ACCESS | FUSE_DESTROY | FUSE_INTERRUPT | FUSE_SETXATTR | FUSE_REMOVEXATTR
        | FUSE_FALLOCATE | FUSE_SETLK | FUSE_SETLKW | FUSE_SYNCFS => match body {
            FuseReplyBody::Empty => Ok(Vec::new()),
            _ => Err(wrong_reply_body(opcode, "empty")),
        },
        _ => Err(unsupported_opcode(opcode)),
    }
}

/// A fully decoded request. `payload` is always an owned copy, including for
/// an opcode without a typed body codec.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseRequest {
    pub header: FuseInHeader,
    pub name: String,
    pub payload: Vec<u8>,
    pub extensions: Vec<u8>,
    pub body: Option<FuseRequestBody>,
}

pub fn decode_request(
    buffer: &[u8],
    ctx: Option<ProtocolContext>,
) -> Result<FuseRequest, ProtocolError> {
    let header = decode_in_header(buffer)?;
    let length = usize::try_from(header.len)
        .map_err(|_| ProtocolError::new("FUSE request length does not fit usize"))?;
    if length > buffer.len() {
        return Err(ProtocolError::new(format!(
            "fuse_in_header.len is {} but only {} byte(s) were read",
            header.len,
            buffer.len()
        )));
    }
    let extension_bytes = usize::from(header.total_extlen)
        .checked_mul(8)
        .ok_or_else(|| ProtocolError::new("FUSE extension length overflows"))?;
    if extension_bytes > length - FUSE_IN_HEADER_SIZE {
        return Err(ProtocolError::new(format!(
            "fuse_in_header.total_extlen is {} ({} bytes), more than the {}-byte body",
            header.total_extlen,
            extension_bytes,
            length - FUSE_IN_HEADER_SIZE
        )));
    }
    let body_end = length - extension_bytes;
    let payload = buffer[FUSE_IN_HEADER_SIZE..body_end].to_vec();
    let extensions = buffer[body_end..length].to_vec();
    let body = if opcode_spec(header.opcode).is_some() {
        Some(decode_request_body(header.opcode, &payload, ctx)?)
    } else {
        None
    };
    let name = {
        let value = opcode_name(header.opcode);
        if value == "UNKNOWN" {
            format!("UNKNOWN({})", header.opcode)
        } else {
            value
        }
    };
    Ok(FuseRequest {
        header,
        name,
        payload,
        extensions,
        body,
    })
}

/// Input to [`encode_request`]. Use `payload` for an opcode intentionally
/// outside the typed table; typed bodies take precedence when supplied.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EncodeRequest {
    pub opcode: u32,
    pub unique: u64,
    pub nodeid: u64,
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
    pub body: Option<FuseRequestBody>,
    pub payload: Vec<u8>,
    pub extensions: Vec<u8>,
}

pub fn encode_request(
    init: &EncodeRequest,
    ctx: Option<ProtocolContext>,
) -> Result<Vec<u8>, ProtocolError> {
    let payload = match &init.body {
        Some(body) => encode_request_body(init.opcode, body, ctx)?,
        None => init.payload.clone(),
    };
    if !init.extensions.len().is_multiple_of(8) {
        return Err(ProtocolError::new(format!(
            "request extensions must be a multiple of 8 bytes, got {}",
            init.extensions.len()
        )));
    }
    let length = FUSE_IN_HEADER_SIZE
        .checked_add(payload.len())
        .and_then(|value| value.checked_add(init.extensions.len()))
        .ok_or_else(|| ProtocolError::new("FUSE request length overflows"))?;
    let total_extlen = u16::try_from(init.extensions.len() / 8)
        .map_err(|_| ProtocolError::new("FUSE extension count exceeds u16"))?;
    let len =
        u32::try_from(length).map_err(|_| ProtocolError::new("FUSE request length exceeds u32"))?;
    let header = RequestHeader {
        len,
        opcode: init.opcode,
        unique: init.unique,
        nodeid: init.nodeid,
        uid: init.uid,
        gid: init.gid,
        pid: init.pid,
        total_extlen,
    };
    let mut message = Vec::with_capacity(length);
    message.extend_from_slice(&encode_in_header(&header));
    message.extend_from_slice(&payload);
    message.extend_from_slice(&init.extensions);
    Ok(message)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FuseReply {
    pub header: FuseOutHeader,
    pub payload: Vec<u8>,
    pub body: Option<FuseReplyBody>,
}

pub fn decode_reply(
    buffer: &[u8],
    opcode: u32,
    ctx: Option<ProtocolContext>,
) -> Result<FuseReply, ProtocolError> {
    let header = decode_out_header(buffer)?;
    let length = usize::try_from(header.len)
        .map_err(|_| ProtocolError::new("FUSE reply length does not fit usize"))?;
    if length > buffer.len() {
        return Err(ProtocolError::new(format!(
            "fuse_out_header.len is {} but only {} byte(s) were read",
            header.len,
            buffer.len()
        )));
    }
    let payload = buffer[FUSE_OUT_HEADER_SIZE..length].to_vec();
    let body = if header.error == 0 {
        if opcode_spec(opcode).is_some() {
            Some(decode_reply_body(opcode, &payload, ctx)?)
        } else {
            None
        }
    } else {
        None
    };
    Ok(FuseReply {
        header,
        payload,
        body,
    })
}

pub fn encode_reply_for(
    unique: u64,
    opcode: u32,
    body: &FuseReplyBody,
    ctx: Option<ProtocolContext>,
) -> Result<Vec<u8>, ProtocolError> {
    let payload = encode_reply_body(opcode, body, ctx)?;
    encode_reply(unique, &payload)
}

pub fn name_byte_length(name: &str) -> usize {
    name.len()
}
