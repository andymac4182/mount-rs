//! Mount-free FUSE codec bindings.
//!
//! The FUSE transport crate owns the wire layouts.  This module only exposes
//! the already-tested, pure notify/record/protocol helpers to the Node
//! subpath.  It deliberately does not bind the native device, session, or
//! mount lifecycle.

use mount_rs_fuse::{ProtocolError, notify, protocol, record};
use napi::bindgen_prelude::{BigInt, Buffer};
use napi::{Error, Status};
use napi_derive::napi;

const ERROR_MARKER: &str = "__mount_rs_fuse_codec_error_v1__";
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn hex(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn invalid_argument(message: impl Into<String>) -> Error {
    Error::new(Status::InvalidArg, message.into())
}

fn codec_error(kind: &str, message: &str, offset: Option<usize>) -> Error {
    Error::new(
        Status::GenericFailure,
        format!(
            "{ERROR_MARKER}|{kind}|{}|{}",
            offset
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            hex(message)
        ),
    )
}

fn protocol_error(error: ProtocolError) -> Error {
    codec_error("protocol", &error.message, error.offset)
}

fn notify_error(error: notify::NotifyError) -> Error {
    codec_error("protocol", &error.0, None)
}

fn transcript_error(error: record::TranscriptError) -> Error {
    codec_error("transcript", &error.0, None)
}

fn usize_value(name: &str, value: Option<f64>, default: usize) -> napi::Result<usize> {
    let value = value.unwrap_or(default as f64);
    if !value.is_finite()
        || value.fract() != 0.0
        || !(0.0..=MAX_SAFE_INTEGER).contains(&value)
        || value > usize::MAX as f64
    {
        return Err(invalid_argument(format!(
            "{name} must be an integer between 0 and {MAX_SAFE_INTEGER}"
        )));
    }
    Ok(value as usize)
}

fn u64_from_bigint(value: &BigInt) -> u64 {
    let (negative, magnitude, _) = value.get_u128();
    let low = magnitude as u64;
    if negative {
        0u64.wrapping_sub(low)
    } else {
        low
    }
}

fn i64_from_bigint(value: &BigInt) -> i64 {
    u64_from_bigint(value) as i64
}

fn bigint(value: u64) -> BigInt {
    BigInt::from(value)
}

fn signed_bigint(value: i64) -> BigInt {
    BigInt::from(value)
}

#[napi(object)]
pub struct NativeFuseProtocolContext {
    pub minor: u32,
    #[napi(js_name = "setxattrExt")]
    pub setxattr_ext: bool,
}

fn protocol_context(value: Option<NativeFuseProtocolContext>) -> Option<protocol::ProtocolContext> {
    value.map(|value| protocol::ProtocolContext {
        minor: value.minor,
        setxattr_ext: value.setxattr_ext,
    })
}

#[napi(object)]
pub struct NativeFuseInHeader {
    pub len: u32,
    pub opcode: u32,
    pub unique: BigInt,
    pub nodeid: BigInt,
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
    #[napi(js_name = "totalExtlen")]
    pub total_extlen: u16,
}

impl From<mount_rs_fuse::RequestHeader> for NativeFuseInHeader {
    fn from(value: mount_rs_fuse::RequestHeader) -> Self {
        Self {
            len: value.len,
            opcode: value.opcode,
            unique: bigint(value.unique),
            nodeid: bigint(value.nodeid),
            uid: value.uid,
            gid: value.gid,
            pid: value.pid,
            total_extlen: value.total_extlen,
        }
    }
}

fn request_header(value: NativeFuseInHeader) -> mount_rs_fuse::RequestHeader {
    mount_rs_fuse::RequestHeader {
        len: value.len,
        opcode: value.opcode,
        unique: u64_from_bigint(&value.unique),
        nodeid: u64_from_bigint(&value.nodeid),
        uid: value.uid,
        gid: value.gid,
        pid: value.pid,
        total_extlen: value.total_extlen,
    }
}

#[napi(object)]
pub struct NativeFuseOutHeader {
    pub len: u32,
    pub error: i32,
    pub unique: BigInt,
}

impl From<protocol::FuseOutHeader> for NativeFuseOutHeader {
    fn from(value: protocol::FuseOutHeader) -> Self {
        Self {
            len: value.len,
            error: value.error,
            unique: bigint(value.unique),
        }
    }
}

fn out_header(value: NativeFuseOutHeader) -> protocol::FuseOutHeader {
    protocol::FuseOutHeader {
        len: value.len,
        error: value.error,
        unique: u64_from_bigint(&value.unique),
    }
}

#[napi(object)]
pub struct NativeFuseNotifyInvalInodeOut {
    pub ino: BigInt,
    pub off: BigInt,
    pub len: BigInt,
}

impl From<notify::FuseNotifyInvalInodeOut> for NativeFuseNotifyInvalInodeOut {
    fn from(value: notify::FuseNotifyInvalInodeOut) -> Self {
        Self {
            ino: bigint(value.ino),
            off: signed_bigint(value.off),
            len: signed_bigint(value.len),
        }
    }
}

fn notify_inval_inode(value: NativeFuseNotifyInvalInodeOut) -> notify::FuseNotifyInvalInodeOut {
    notify::FuseNotifyInvalInodeOut {
        ino: u64_from_bigint(&value.ino),
        off: i64_from_bigint(&value.off),
        len: i64_from_bigint(&value.len),
    }
}

#[napi(object)]
pub struct NativeFuseNotifyInvalEntryOut {
    pub parent: BigInt,
    pub name: String,
    pub flags: u32,
}

impl From<notify::FuseNotifyInvalEntryOut> for NativeFuseNotifyInvalEntryOut {
    fn from(value: notify::FuseNotifyInvalEntryOut) -> Self {
        Self {
            parent: bigint(value.parent),
            name: value.name,
            flags: value.flags,
        }
    }
}

fn notify_inval_entry(value: NativeFuseNotifyInvalEntryOut) -> notify::FuseNotifyInvalEntryOut {
    notify::FuseNotifyInvalEntryOut {
        parent: u64_from_bigint(&value.parent),
        name: value.name,
        flags: value.flags,
    }
}

#[napi(object)]
pub struct NativeFuseNotification {
    pub code: i32,
    #[napi(ts_type = "Uint8Array")]
    pub body: Buffer,
}

#[napi(js_name = "fuseEncodeNotify")]
pub fn fuse_encode_notify(code: i32, #[napi(ts_arg_type = "Uint8Array")] body: Buffer) -> Buffer {
    Buffer::from(notify::encode_notify(code, body.as_ref()))
}

#[napi(js_name = "fuseDecodeNotify")]
pub fn fuse_decode_notify(
    #[napi(ts_arg_type = "Uint8Array")] message: Buffer,
) -> napi::Result<NativeFuseNotification> {
    notify::decode_notify(message.as_ref())
        .map(|value| NativeFuseNotification {
            code: value.code,
            body: Buffer::from(value.body),
        })
        .map_err(notify_error)
}

#[napi(js_name = "fuseEncodeNotifyInvalInode")]
pub fn fuse_encode_notify_inval_inode(value: NativeFuseNotifyInvalInodeOut) -> Buffer {
    Buffer::from(notify::encode_notify_inval_inode(notify_inval_inode(value)))
}

#[napi(js_name = "fuseDecodeNotifyInvalInode")]
pub fn fuse_decode_notify_inval_inode(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
) -> napi::Result<NativeFuseNotifyInvalInodeOut> {
    notify::decode_notify_inval_inode(body.as_ref())
        .map(Into::into)
        .map_err(notify_error)
}

#[napi(js_name = "fuseEncodeNotifyInvalEntry")]
pub fn fuse_encode_notify_inval_entry(
    value: NativeFuseNotifyInvalEntryOut,
) -> napi::Result<Buffer> {
    notify::encode_notify_inval_entry(notify_inval_entry(value))
        .map(Buffer::from)
        .map_err(notify_error)
}

#[napi(js_name = "fuseDecodeNotifyInvalEntry")]
pub fn fuse_decode_notify_inval_entry(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
) -> napi::Result<NativeFuseNotifyInvalEntryOut> {
    notify::decode_notify_inval_entry(body.as_ref())
        .map(Into::into)
        .map_err(notify_error)
}

#[napi(object)]
pub struct NativeFuseTranscriptFrame {
    pub direction: String,
    pub timestamp: BigInt,
    #[napi(ts_type = "Uint8Array")]
    pub bytes: Buffer,
}

fn transcript_direction(value: &str) -> napi::Result<record::TranscriptDirection> {
    match value {
        "in" => Ok(record::TranscriptDirection::In),
        "out" => Ok(record::TranscriptDirection::Out),
        _ => Err(invalid_argument(format!(
            "transcript direction must be \"in\" or \"out\", got {value:?}"
        ))),
    }
}

fn transcript_frame(value: NativeFuseTranscriptFrame) -> napi::Result<record::TranscriptFrame> {
    Ok(record::TranscriptFrame {
        direction: transcript_direction(&value.direction)?,
        timestamp: u64_from_bigint(&value.timestamp),
        bytes: value.bytes.as_ref().to_vec(),
    })
}

fn native_transcript_frame(value: record::TranscriptFrame) -> NativeFuseTranscriptFrame {
    NativeFuseTranscriptFrame {
        direction: match value.direction {
            record::TranscriptDirection::In => "in".to_owned(),
            record::TranscriptDirection::Out => "out".to_owned(),
        },
        timestamp: bigint(value.timestamp),
        bytes: Buffer::from(value.bytes),
    }
}

#[napi(js_name = "fuseEncodeTranscript")]
pub fn fuse_encode_transcript(frames: Vec<NativeFuseTranscriptFrame>) -> napi::Result<Buffer> {
    let frames = frames
        .into_iter()
        .map(transcript_frame)
        .collect::<napi::Result<Vec<_>>>()?;
    Ok(Buffer::from(record::encode_transcript(&frames)))
}

#[napi(js_name = "fuseDecodeTranscript")]
pub fn fuse_decode_transcript(
    #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
) -> napi::Result<Vec<NativeFuseTranscriptFrame>> {
    record::decode_transcript(bytes.as_ref())
        .map(|frames| frames.into_iter().map(native_transcript_frame).collect())
        .map_err(transcript_error)
}

#[napi]
pub struct NativeFuseTranscriptRecorder {
    inner: record::TranscriptRecorder,
}

#[napi]
impl NativeFuseTranscriptRecorder {
    #[napi(constructor)]
    pub fn new(limit: Option<f64>) -> napi::Result<Self> {
        let limit = limit
            .map(|value| usize_value("limit", Some(value), 0))
            .transpose()?;
        Ok(Self {
            inner: record::TranscriptRecorder::with_limit(limit),
        })
    }

    #[napi(getter)]
    pub fn frames(&self) -> Vec<NativeFuseTranscriptFrame> {
        self.inner
            .frames
            .clone()
            .into_iter()
            .map(native_transcript_frame)
            .collect()
    }

    #[napi(getter)]
    pub fn truncated(&self) -> bool {
        self.inner.truncated
    }

    #[napi(getter)]
    pub fn bytes(&self) -> f64 {
        self.inner.bytes as f64
    }

    #[napi]
    pub fn tap(
        &mut self,
        direction: String,
        #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
    ) -> napi::Result<()> {
        self.inner
            .tap(transcript_direction(&direction)?, bytes.as_ref());
        Ok(())
    }

    #[napi(js_name = "tapAt")]
    pub fn tap_at(
        &mut self,
        direction: String,
        #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
        now: BigInt,
    ) -> napi::Result<()> {
        self.inner.tap_at(
            transcript_direction(&direction)?,
            bytes.as_ref(),
            u64_from_bigint(&now),
        );
        Ok(())
    }

    #[napi]
    pub fn encode(&self) -> Buffer {
        Buffer::from(self.inner.encode())
    }
}

#[napi(js_name = "fuseDecodeInHeader")]
pub fn fuse_decode_in_header(
    #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
) -> napi::Result<NativeFuseInHeader> {
    protocol::decode_in_header(bytes.as_ref())
        .map(Into::into)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeInHeader")]
pub fn fuse_encode_in_header(value: NativeFuseInHeader) -> Buffer {
    Buffer::from(protocol::encode_in_header(&request_header(value)).to_vec())
}

#[napi(js_name = "fuseDecodeOutHeader")]
pub fn fuse_decode_out_header(
    #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
) -> napi::Result<NativeFuseOutHeader> {
    protocol::decode_out_header(bytes.as_ref())
        .map(Into::into)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeOutHeader")]
pub fn fuse_encode_out_header(value: NativeFuseOutHeader) -> Buffer {
    Buffer::from(protocol::encode_out_header(out_header(value)).to_vec())
}

#[napi(js_name = "fuseEncodeReply")]
pub fn fuse_encode_reply(
    unique: BigInt,
    #[napi(ts_arg_type = "Uint8Array")] body: Option<Buffer>,
) -> napi::Result<Buffer> {
    protocol::encode_reply(
        u64_from_bigint(&unique),
        body.as_deref().unwrap_or_default(),
    )
    .map(Buffer::from)
    .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeErrorReply")]
pub fn fuse_encode_error_reply(unique: BigInt, errno: i32) -> Buffer {
    Buffer::from(protocol::encode_error_reply(u64_from_bigint(&unique), errno).to_vec())
}

#[napi(object)]
pub struct NativeFuseAttr {
    pub ino: BigInt,
    pub size: BigInt,
    pub blocks: BigInt,
    pub atime: BigInt,
    pub mtime: BigInt,
    pub ctime: BigInt,
    #[napi(js_name = "atimensec")]
    pub atime_nsec: u32,
    #[napi(js_name = "mtimensec")]
    pub mtime_nsec: u32,
    #[napi(js_name = "ctimensec")]
    pub ctime_nsec: u32,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub blksize: u32,
    pub flags: u32,
}

impl From<protocol::FuseAttr> for NativeFuseAttr {
    fn from(value: protocol::FuseAttr) -> Self {
        Self {
            ino: bigint(value.ino),
            size: bigint(value.size),
            blocks: bigint(value.blocks),
            atime: bigint(value.atime),
            mtime: bigint(value.mtime),
            ctime: bigint(value.ctime),
            atime_nsec: value.atime_nsec,
            mtime_nsec: value.mtime_nsec,
            ctime_nsec: value.ctime_nsec,
            mode: value.mode,
            nlink: value.nlink,
            uid: value.uid,
            gid: value.gid,
            rdev: value.rdev,
            blksize: value.blksize,
            flags: value.flags,
        }
    }
}

fn fuse_attr(value: NativeFuseAttr) -> protocol::FuseAttr {
    protocol::FuseAttr {
        ino: u64_from_bigint(&value.ino),
        size: u64_from_bigint(&value.size),
        blocks: u64_from_bigint(&value.blocks),
        atime: u64_from_bigint(&value.atime),
        mtime: u64_from_bigint(&value.mtime),
        ctime: u64_from_bigint(&value.ctime),
        atime_nsec: value.atime_nsec,
        mtime_nsec: value.mtime_nsec,
        ctime_nsec: value.ctime_nsec,
        mode: value.mode,
        nlink: value.nlink,
        uid: value.uid,
        gid: value.gid,
        rdev: value.rdev,
        blksize: value.blksize,
        flags: value.flags,
    }
}

#[napi(object)]
pub struct NativeFuseEntryOut {
    pub nodeid: BigInt,
    pub generation: BigInt,
    #[napi(js_name = "entryValid")]
    pub entry_valid: BigInt,
    #[napi(js_name = "attrValid")]
    pub attr_valid: BigInt,
    #[napi(js_name = "entryValidNsec")]
    pub entry_valid_nsec: u32,
    #[napi(js_name = "attrValidNsec")]
    pub attr_valid_nsec: u32,
    pub attr: NativeFuseAttr,
}

impl From<protocol::FuseEntryOut> for NativeFuseEntryOut {
    fn from(value: protocol::FuseEntryOut) -> Self {
        Self {
            nodeid: bigint(value.nodeid),
            generation: bigint(value.generation),
            entry_valid: bigint(value.entry_valid),
            attr_valid: bigint(value.attr_valid),
            entry_valid_nsec: value.entry_valid_nsec,
            attr_valid_nsec: value.attr_valid_nsec,
            attr: value.attr.into(),
        }
    }
}

fn entry_out(value: NativeFuseEntryOut) -> protocol::FuseEntryOut {
    protocol::FuseEntryOut {
        nodeid: u64_from_bigint(&value.nodeid),
        generation: u64_from_bigint(&value.generation),
        entry_valid: u64_from_bigint(&value.entry_valid),
        attr_valid: u64_from_bigint(&value.attr_valid),
        entry_valid_nsec: value.entry_valid_nsec,
        attr_valid_nsec: value.attr_valid_nsec,
        attr: fuse_attr(value.attr),
    }
}

#[napi(object)]
pub struct NativeFuseAttrOut {
    #[napi(js_name = "attrValid")]
    pub attr_valid: BigInt,
    #[napi(js_name = "attrValidNsec")]
    pub attr_valid_nsec: u32,
    pub attr: NativeFuseAttr,
}

impl From<protocol::FuseAttrOut> for NativeFuseAttrOut {
    fn from(value: protocol::FuseAttrOut) -> Self {
        Self {
            attr_valid: bigint(value.attr_valid),
            attr_valid_nsec: value.attr_valid_nsec,
            attr: value.attr.into(),
        }
    }
}

fn attr_out(value: NativeFuseAttrOut) -> protocol::FuseAttrOut {
    protocol::FuseAttrOut {
        attr_valid: u64_from_bigint(&value.attr_valid),
        attr_valid_nsec: value.attr_valid_nsec,
        attr: fuse_attr(value.attr),
    }
}

#[napi(object)]
pub struct NativeFuseOpenOut {
    pub fh: BigInt,
    #[napi(js_name = "openFlags")]
    pub open_flags: u32,
    #[napi(js_name = "backingId")]
    pub backing_id: i32,
}

impl From<protocol::FuseOpenOut> for NativeFuseOpenOut {
    fn from(value: protocol::FuseOpenOut) -> Self {
        Self {
            fh: bigint(value.fh),
            open_flags: value.open_flags,
            backing_id: value.backing_id,
        }
    }
}

fn open_out(value: NativeFuseOpenOut) -> protocol::FuseOpenOut {
    protocol::FuseOpenOut {
        fh: u64_from_bigint(&value.fh),
        open_flags: value.open_flags,
        backing_id: value.backing_id,
    }
}

#[napi(object)]
pub struct NativeFuseReadIn {
    pub fh: BigInt,
    pub offset: BigInt,
    pub size: u32,
    #[napi(js_name = "readFlags")]
    pub read_flags: u32,
    #[napi(js_name = "lockOwner")]
    pub lock_owner: BigInt,
    pub flags: u32,
}

impl From<protocol::FuseReadIn> for NativeFuseReadIn {
    fn from(value: protocol::FuseReadIn) -> Self {
        Self {
            fh: bigint(value.fh),
            offset: bigint(value.offset),
            size: value.size,
            read_flags: value.read_flags,
            lock_owner: bigint(value.lock_owner),
            flags: value.flags,
        }
    }
}

fn read_in(value: NativeFuseReadIn) -> protocol::FuseReadIn {
    protocol::FuseReadIn {
        fh: u64_from_bigint(&value.fh),
        offset: u64_from_bigint(&value.offset),
        size: value.size,
        read_flags: value.read_flags,
        lock_owner: u64_from_bigint(&value.lock_owner),
        flags: value.flags,
    }
}

#[napi(object)]
pub struct NativeFuseRawData {
    #[napi(ts_type = "Uint8Array")]
    pub data: Buffer,
}

impl From<Vec<u8>> for NativeFuseRawData {
    fn from(value: Vec<u8>) -> Self {
        Self {
            data: Buffer::from(value),
        }
    }
}

fn raw_data(value: NativeFuseRawData) -> Vec<u8> {
    value.data.as_ref().to_vec()
}

#[napi(object)]
pub struct NativeFuseWriteIn {
    pub fh: BigInt,
    pub offset: BigInt,
    pub size: u32,
    #[napi(js_name = "writeFlags")]
    pub write_flags: u32,
    #[napi(js_name = "lockOwner")]
    pub lock_owner: BigInt,
    pub flags: u32,
    #[napi(ts_type = "Uint8Array")]
    pub data: Buffer,
}

impl From<protocol::FuseWriteIn> for NativeFuseWriteIn {
    fn from(value: protocol::FuseWriteIn) -> Self {
        Self {
            fh: bigint(value.fh),
            offset: bigint(value.offset),
            size: value.size,
            write_flags: value.write_flags,
            lock_owner: bigint(value.lock_owner),
            flags: value.flags,
            data: Buffer::from(value.data),
        }
    }
}

fn write_in(value: NativeFuseWriteIn) -> protocol::FuseWriteIn {
    protocol::FuseWriteIn {
        fh: u64_from_bigint(&value.fh),
        offset: u64_from_bigint(&value.offset),
        size: value.size,
        write_flags: value.write_flags,
        lock_owner: u64_from_bigint(&value.lock_owner),
        flags: value.flags,
        data: value.data.as_ref().to_vec(),
    }
}

#[napi(object)]
pub struct NativeFuseWriteOut {
    pub size: u32,
}

impl From<protocol::FuseWriteOut> for NativeFuseWriteOut {
    fn from(value: protocol::FuseWriteOut) -> Self {
        Self { size: value.size }
    }
}

fn write_out(value: NativeFuseWriteOut) -> protocol::FuseWriteOut {
    protocol::FuseWriteOut { size: value.size }
}

#[napi(object)]
pub struct NativeFuseKstatfs {
    pub blocks: BigInt,
    pub bfree: BigInt,
    pub bavail: BigInt,
    pub files: BigInt,
    pub ffree: BigInt,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
}

impl From<protocol::FuseKstatfs> for NativeFuseKstatfs {
    fn from(value: protocol::FuseKstatfs) -> Self {
        Self {
            blocks: bigint(value.blocks),
            bfree: bigint(value.bfree),
            bavail: bigint(value.bavail),
            files: bigint(value.files),
            ffree: bigint(value.ffree),
            bsize: value.bsize,
            namelen: value.namelen,
            frsize: value.frsize,
        }
    }
}

fn kstatfs(value: NativeFuseKstatfs) -> protocol::FuseKstatfs {
    protocol::FuseKstatfs {
        blocks: u64_from_bigint(&value.blocks),
        bfree: u64_from_bigint(&value.bfree),
        bavail: u64_from_bigint(&value.bavail),
        files: u64_from_bigint(&value.files),
        ffree: u64_from_bigint(&value.ffree),
        bsize: value.bsize,
        namelen: value.namelen,
        frsize: value.frsize,
    }
}

#[napi(object)]
pub struct NativeFuseInitIn {
    pub major: u32,
    pub minor: u32,
    #[napi(js_name = "maxReadahead")]
    pub max_readahead: u32,
    pub flags: u32,
    pub flags2: u32,
}

impl From<protocol::FuseInitIn> for NativeFuseInitIn {
    fn from(value: protocol::FuseInitIn) -> Self {
        Self {
            major: value.major,
            minor: value.minor,
            max_readahead: value.max_readahead,
            flags: value.flags,
            flags2: value.flags2,
        }
    }
}

fn init_in(value: NativeFuseInitIn) -> protocol::FuseInitIn {
    protocol::FuseInitIn {
        major: value.major,
        minor: value.minor,
        max_readahead: value.max_readahead,
        flags: value.flags,
        flags2: value.flags2,
    }
}

#[napi(object)]
pub struct NativeFuseInitOut {
    pub major: u32,
    pub minor: u32,
    #[napi(js_name = "maxReadahead")]
    pub max_readahead: u32,
    pub flags: u32,
    #[napi(js_name = "maxBackground")]
    pub max_background: u16,
    #[napi(js_name = "congestionThreshold")]
    pub congestion_threshold: u16,
    #[napi(js_name = "maxWrite")]
    pub max_write: u32,
    #[napi(js_name = "timeGran")]
    pub time_gran: u32,
    #[napi(js_name = "maxPages")]
    pub max_pages: u16,
    #[napi(js_name = "mapAlignment")]
    pub map_alignment: u16,
    pub flags2: u32,
    #[napi(js_name = "maxStackDepth")]
    pub max_stack_depth: u32,
}

impl From<protocol::FuseInitOut> for NativeFuseInitOut {
    fn from(value: protocol::FuseInitOut) -> Self {
        Self {
            major: value.major,
            minor: value.minor,
            max_readahead: value.max_readahead,
            flags: value.flags,
            max_background: value.max_background,
            congestion_threshold: value.congestion_threshold,
            max_write: value.max_write,
            time_gran: value.time_gran,
            max_pages: value.max_pages,
            map_alignment: value.map_alignment,
            flags2: value.flags2,
            max_stack_depth: value.max_stack_depth,
        }
    }
}

fn init_out(value: NativeFuseInitOut) -> protocol::FuseInitOut {
    protocol::FuseInitOut {
        major: value.major,
        minor: value.minor,
        max_readahead: value.max_readahead,
        flags: value.flags,
        max_background: value.max_background,
        congestion_threshold: value.congestion_threshold,
        max_write: value.max_write,
        time_gran: value.time_gran,
        max_pages: value.max_pages,
        map_alignment: value.map_alignment,
        flags2: value.flags2,
        max_stack_depth: value.max_stack_depth,
    }
}

#[napi(object)]
pub struct NativeFuseGetxattrOut {
    pub size: u32,
}

impl From<protocol::FuseGetxattrOut> for NativeFuseGetxattrOut {
    fn from(value: protocol::FuseGetxattrOut) -> Self {
        Self { size: value.size }
    }
}

#[napi(js_name = "fuseDecodeEntryOut")]
pub fn fuse_decode_entry_out(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<NativeFuseEntryOut> {
    protocol::decode_entry_out(body.as_ref(), protocol_context(context))
        .map(Into::into)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeEntryOut")]
pub fn fuse_encode_entry_out(
    value: NativeFuseEntryOut,
    context: Option<NativeFuseProtocolContext>,
) -> Buffer {
    Buffer::from(protocol::encode_entry_out(
        &entry_out(value),
        protocol_context(context),
    ))
}

#[napi(js_name = "fuseDecodeAttrOut")]
pub fn fuse_decode_attr_out(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<NativeFuseAttrOut> {
    protocol::decode_attr_out(body.as_ref(), protocol_context(context))
        .map(Into::into)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeAttrOut")]
pub fn fuse_encode_attr_out(
    value: NativeFuseAttrOut,
    context: Option<NativeFuseProtocolContext>,
) -> Buffer {
    Buffer::from(protocol::encode_attr_out(
        &attr_out(value),
        protocol_context(context),
    ))
}

#[napi(js_name = "fuseDecodeOpenOut")]
pub fn fuse_decode_open_out(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<NativeFuseOpenOut> {
    protocol::decode_open_out(body.as_ref(), protocol_context(context))
        .map(Into::into)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeOpenOut")]
pub fn fuse_encode_open_out(
    value: NativeFuseOpenOut,
    context: Option<NativeFuseProtocolContext>,
) -> Buffer {
    Buffer::from(protocol::encode_open_out(
        &open_out(value),
        protocol_context(context),
    ))
}

#[napi(js_name = "fuseDecodeReadIn")]
pub fn fuse_decode_read_in(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<NativeFuseReadIn> {
    match protocol::decode_request_body(
        mount_rs_fuse::FUSE_READ,
        body.as_ref(),
        protocol_context(context),
    )
    .map_err(protocol_error)?
    {
        protocol::FuseRequestBody::Read(value) => Ok(value.into()),
        _ => Err(protocol_error(ProtocolError::new(
            "FUSE_READ did not decode as a read request",
        ))),
    }
}

#[napi(js_name = "fuseEncodeReadIn")]
pub fn fuse_encode_read_in(
    value: NativeFuseReadIn,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<Buffer> {
    protocol::encode_request_body(
        mount_rs_fuse::FUSE_READ,
        &protocol::FuseRequestBody::Read(read_in(value)),
        protocol_context(context),
    )
    .map(Buffer::from)
    .map_err(protocol_error)
}

#[napi(js_name = "fuseDecodeReadOut")]
pub fn fuse_decode_read_out(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
) -> napi::Result<NativeFuseRawData> {
    match protocol::decode_reply_body(mount_rs_fuse::FUSE_READ, body.as_ref(), None)
        .map_err(protocol_error)?
    {
        protocol::FuseReplyBody::Raw(value) => Ok(value.into()),
        _ => Err(protocol_error(ProtocolError::new(
            "FUSE_READ did not decode as a raw reply",
        ))),
    }
}

#[napi(js_name = "fuseEncodeReadOut")]
pub fn fuse_encode_read_out(value: NativeFuseRawData) -> napi::Result<Buffer> {
    protocol::encode_reply_body(
        mount_rs_fuse::FUSE_READ,
        &protocol::FuseReplyBody::Raw(raw_data(value)),
        None,
    )
    .map(Buffer::from)
    .map_err(protocol_error)
}

#[napi(js_name = "fuseDecodeWriteIn")]
pub fn fuse_decode_write_in(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<NativeFuseWriteIn> {
    match protocol::decode_request_body(
        mount_rs_fuse::FUSE_WRITE,
        body.as_ref(),
        protocol_context(context),
    )
    .map_err(protocol_error)?
    {
        protocol::FuseRequestBody::Write(value) => Ok(value.into()),
        _ => Err(protocol_error(ProtocolError::new(
            "FUSE_WRITE did not decode as a write request",
        ))),
    }
}

#[napi(js_name = "fuseEncodeWriteIn")]
pub fn fuse_encode_write_in(
    value: NativeFuseWriteIn,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<Buffer> {
    protocol::encode_request_body(
        mount_rs_fuse::FUSE_WRITE,
        &protocol::FuseRequestBody::Write(write_in(value)),
        protocol_context(context),
    )
    .map(Buffer::from)
    .map_err(protocol_error)
}

#[napi(js_name = "fuseDecodeWriteOut")]
pub fn fuse_decode_write_out(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
) -> napi::Result<NativeFuseWriteOut> {
    match protocol::decode_reply_body(mount_rs_fuse::FUSE_WRITE, body.as_ref(), None)
        .map_err(protocol_error)?
    {
        protocol::FuseReplyBody::Write(value) => Ok(value.into()),
        _ => Err(protocol_error(ProtocolError::new(
            "FUSE_WRITE did not decode as a write reply",
        ))),
    }
}

#[napi(js_name = "fuseEncodeWriteOut")]
pub fn fuse_encode_write_out(value: NativeFuseWriteOut) -> napi::Result<Buffer> {
    protocol::encode_reply_body(
        mount_rs_fuse::FUSE_WRITE,
        &protocol::FuseReplyBody::Write(write_out(value)),
        None,
    )
    .map(Buffer::from)
    .map_err(protocol_error)
}

#[napi(js_name = "fuseDecodeInitIn")]
pub fn fuse_decode_init_in(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
) -> napi::Result<NativeFuseInitIn> {
    protocol::decode_init_in(body.as_ref())
        .map(Into::into)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeInitIn")]
pub fn fuse_encode_init_in(value: NativeFuseInitIn) -> Buffer {
    Buffer::from(protocol::encode_init_in(&init_in(value)))
}

#[napi(js_name = "fuseDecodeInitOut")]
pub fn fuse_decode_init_out(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
) -> napi::Result<NativeFuseInitOut> {
    protocol::decode_init_out(body.as_ref())
        .map(Into::into)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeInitOut")]
pub fn fuse_encode_init_out(
    value: NativeFuseInitOut,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<Buffer> {
    protocol::encode_init_out(&init_out(value), protocol_context(context))
        .map(Buffer::from)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseDecodeStatfsOut")]
pub fn fuse_decode_statfs_out(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<NativeFuseKstatfs> {
    protocol::decode_statfs_out(body.as_ref(), protocol_context(context))
        .map(Into::into)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeStatfsOut")]
pub fn fuse_encode_statfs_out(
    value: NativeFuseKstatfs,
    context: Option<NativeFuseProtocolContext>,
) -> Buffer {
    Buffer::from(protocol::encode_statfs_out(
        &kstatfs(value),
        protocol_context(context),
    ))
}

#[napi(js_name = "fuseDecodeGetxattrOut")]
pub fn fuse_decode_getxattr_out(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
) -> napi::Result<NativeFuseGetxattrOut> {
    protocol::decode_getxattr_out(body.as_ref())
        .map(Into::into)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseEncodeGetxattrOut")]
pub fn fuse_encode_getxattr_out(value: NativeFuseGetxattrOut) -> Buffer {
    Buffer::from(protocol::encode_getxattr_out(&protocol::FuseGetxattrOut {
        size: value.size,
    }))
}

#[napi(js_name = "fuseEncodeXattrNames")]
pub fn fuse_encode_xattr_names(names: Vec<String>) -> napi::Result<Buffer> {
    protocol::encode_xattr_names(&names)
        .map(Buffer::from)
        .map_err(protocol_error)
}

#[napi(js_name = "fuseDecodeXattrNames")]
pub fn fuse_decode_xattr_names(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
) -> napi::Result<Vec<String>> {
    protocol::decode_xattr_names(body.as_ref()).map_err(protocol_error)
}

#[napi(js_name = "fuseFuseErrno")]
pub fn fuse_fuse_errno(errno: i32) -> i32 {
    protocol::fuse_errno(errno)
}

#[napi(js_name = "fuseAttrSize")]
pub fn fuse_attr_size(minor: u32) -> u32 {
    protocol::attr_size(minor) as u32
}

#[napi(js_name = "fuseEntryOutSize")]
pub fn fuse_entry_out_size(minor: u32) -> u32 {
    protocol::entry_out_size(minor) as u32
}

#[napi(js_name = "fuseAttrOutSize")]
pub fn fuse_attr_out_size(minor: u32) -> u32 {
    protocol::attr_out_size(minor) as u32
}

#[napi(js_name = "fuseKstatfsSize")]
pub fn fuse_kstatfs_size(minor: u32) -> u32 {
    protocol::kstatfs_size(minor) as u32
}

#[napi(js_name = "fuseInitOutSize")]
pub fn fuse_init_out_size(minor: u32) -> u32 {
    protocol::init_out_size(minor) as u32
}

#[napi(js_name = "fuseReadWriteInSize")]
pub fn fuse_read_write_in_size(minor: u32) -> u32 {
    protocol::read_write_in_size(minor) as u32
}

#[napi(js_name = "fuseJoinInitFlags")]
pub fn fuse_join_init_flags(flags: u32, flags2: u32) -> BigInt {
    BigInt::from(mount_rs_fuse::init::join_init_flags(flags, flags2))
}

#[napi(object)]
pub struct NativeFuseSplitInitFlags {
    pub flags: u32,
    pub flags2: u32,
}

#[napi(js_name = "fuseSplitInitFlags")]
pub fn fuse_split_init_flags(flags: BigInt) -> NativeFuseSplitInitFlags {
    let (flags, flags2) = mount_rs_fuse::init::split_init_flags(u64_from_bigint(&flags));
    NativeFuseSplitInitFlags { flags, flags2 }
}

#[napi(js_name = "fuseOpcodeName")]
pub fn fuse_opcode_name(opcode: u32) -> String {
    mount_rs_fuse::constants::opcode_name(opcode)
}

#[napi(js_name = "fuseSupportedOpcodes")]
pub fn fuse_supported_opcodes() -> Vec<u32> {
    mount_rs_fuse::constants::SUPPORTED_OPCODES.to_vec()
}

#[napi(js_name = "fuseUnimplementedOpcodes")]
pub fn fuse_unimplemented_opcodes() -> Vec<u32> {
    mount_rs_fuse::constants::UNIMPLEMENTED_OPCODES.to_vec()
}

#[napi(js_name = "fuseDirentAlign")]
pub fn fuse_dirent_align(size: f64) -> napi::Result<u32> {
    Ok(protocol::dirent_align(usize_value("size", Some(size), 0)?) as u32)
}

#[napi(js_name = "fuseDirentSize")]
pub fn fuse_dirent_size(name_byte_length: f64) -> napi::Result<u32> {
    Ok(protocol::dirent_size(usize_value("nameByteLength", Some(name_byte_length), 0)?) as u32)
}

#[napi(js_name = "fuseDirentPlusSize")]
pub fn fuse_dirent_plus_size(
    name_byte_length: f64,
    context: Option<NativeFuseProtocolContext>,
) -> napi::Result<u32> {
    Ok(protocol::dirent_plus_size(
        usize_value("nameByteLength", Some(name_byte_length), 0)?,
        protocol_context(context),
    ) as u32)
}

#[napi(js_name = "fuseDirentType")]
pub fn fuse_dirent_type(mode: u32) -> u32 {
    protocol::dirent_type(mode)
}
