//! N-API bindings for the NFS XDR and ONC-RPC codec primitives.
//!
//! The transport crate owns all wire-format logic.  This module only gives the
//! JavaScript boundary owned reader/writer handles and translates the small
//! RPC value objects.  The `Nfs` prefix is intentional: the package postlude
//! maps these names into the transport-specific `./nfs` subpath.

use mount_rs_nfs::{rpc, xdr};
use napi::bindgen_prelude::{BigInt, Buffer};
use napi::{Error, Status};
use napi_derive::napi;

const XDR_ERROR_MARKER: &str = "__mount_rs_nfs_xdr_error_v1__";
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

fn hex(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn codec_error(error: xdr::XdrError, base: usize) -> Error {
    Error::new(
        Status::GenericFailure,
        format!(
            "{XDR_ERROR_MARKER}|{}|{}",
            error.offset.saturating_add(base),
            hex(&error.message)
        ),
    )
}

fn invalid_argument(message: impl Into<String>) -> Error {
    Error::new(Status::InvalidArg, message.into())
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

fn u32_value(name: &str, value: Option<f64>, default: u32) -> napi::Result<u32> {
    let value = usize_value(name, value, default as usize)?;
    u32::try_from(value)
        .map_err(|_| invalid_argument(format!("{name} must fit in an unsigned 32-bit integer")))
}

fn u64_from_bigint(value: BigInt, name: &str) -> napi::Result<u64> {
    let (negative, magnitude, _) = value.get_u128();
    let low = magnitude as u64;
    // Match the upstream XdrWriter's BigInt.asUintN(64) conversion.  The
    // lower 64 bits are the wire value, including for negative or wider input.
    let _ = name;
    Ok(if negative {
        0u64.wrapping_sub(low)
    } else {
        low
    })
}

fn i64_from_bigint(value: BigInt, name: &str) -> napi::Result<i64> {
    let (negative, magnitude, _) = value.get_u128();
    let low = magnitude as u64;
    // Match BigInt.asIntN(64) by interpreting the modulo-2^64 word as signed.
    let _ = name;
    Ok((if negative {
        0u64.wrapping_sub(low)
    } else {
        low
    }) as i64)
}

fn reader_error(error: xdr::XdrError, base: usize) -> Error {
    codec_error(error, base)
}

/// An owned XDR reader.  The transport reader remains borrowed internally for
/// each operation, so no N-API object can retain a view into JavaScript memory.
#[napi]
pub struct NfsXdrReader {
    bytes: Vec<u8>,
    offset: usize,
}

impl NfsXdrReader {
    fn read<T>(
        &mut self,
        operation: impl FnOnce(&mut xdr::XdrReader<'_>) -> Result<T, xdr::XdrError>,
    ) -> napi::Result<T> {
        let base = self.offset;
        let (result, consumed) = {
            let mut reader = xdr::XdrReader::new(&self.bytes[base..]);
            let result = operation(&mut reader);
            (result, reader.offset())
        };
        self.offset = base.saturating_add(consumed);
        result.map_err(|error| reader_error(error, base))
    }
}

#[napi]
impl NfsXdrReader {
    #[napi(constructor)]
    pub fn new(
        #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
        offset: Option<f64>,
    ) -> napi::Result<Self> {
        let bytes = bytes.as_ref().to_vec();
        let offset = usize_value("offset", offset, 0)?;
        if offset > bytes.len() {
            return Err(invalid_argument(format!(
                "offset must be between 0 and {}",
                bytes.len()
            )));
        }
        Ok(Self { bytes, offset })
    }

    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(self.bytes.clone())
    }

    #[napi(getter)]
    pub fn offset(&self) -> u32 {
        self.offset as u32
    }

    #[napi(getter)]
    pub fn remaining(&self) -> u32 {
        self.bytes.len().saturating_sub(self.offset) as u32
    }

    #[napi(getter, js_name = "atEnd")]
    pub fn at_end(&self) -> bool {
        self.offset >= self.bytes.len()
    }

    #[napi]
    pub fn u32(&mut self, what: Option<String>) -> napi::Result<u32> {
        let what = what.unwrap_or_else(|| "uint32".to_owned());
        self.read(|reader| reader.u32(&what))
    }

    #[napi]
    pub fn i32(&mut self, what: Option<String>) -> napi::Result<i32> {
        let what = what.unwrap_or_else(|| "int32".to_owned());
        self.read(|reader| reader.i32(&what))
    }

    #[napi]
    pub fn u64(&mut self, what: Option<String>) -> napi::Result<BigInt> {
        let what = what.unwrap_or_else(|| "uint64".to_owned());
        self.read(|reader| reader.u64(&what).map(BigInt::from))
    }

    #[napi]
    pub fn i64(&mut self, what: Option<String>) -> napi::Result<BigInt> {
        let what = what.unwrap_or_else(|| "int64".to_owned());
        self.read(|reader| reader.i64(&what).map(BigInt::from))
    }

    #[napi]
    pub fn bool(&mut self, what: Option<String>) -> napi::Result<bool> {
        let what = what.unwrap_or_else(|| "bool".to_owned());
        self.read(|reader| reader.bool(&what))
    }

    #[napi(js_name = "fixedOpaque")]
    pub fn fixed_opaque(&mut self, length: f64, what: Option<String>) -> napi::Result<Buffer> {
        let length = usize_value("length", Some(length), 0)?;
        let what = what.unwrap_or_else(|| "opaque".to_owned());
        self.read(|reader| reader.fixed_opaque(length, &what).map(Buffer::from))
    }

    #[napi(js_name = "varOpaque")]
    pub fn var_opaque(&mut self, max: Option<f64>, what: Option<String>) -> napi::Result<Buffer> {
        let max = usize_value("max", max, xdr::XDR_MAX_ITEM)?;
        let what = what.unwrap_or_else(|| "opaque<>".to_owned());
        self.read(|reader| reader.var_opaque(max, &what).map(Buffer::from))
    }

    #[napi]
    pub fn string(&mut self, max: Option<f64>, what: Option<String>) -> napi::Result<String> {
        let max = usize_value("max", max, xdr::XDR_MAX_ITEM)?;
        let what = what.unwrap_or_else(|| "string<>".to_owned());
        self.read(|reader| reader.string(max, &what))
    }

    #[napi]
    pub fn rest(&mut self) -> Buffer {
        let result = self.bytes[self.offset..].to_vec();
        self.offset = self.bytes.len();
        Buffer::from(result)
    }

    #[napi]
    pub fn end(&self, what: Option<String>) -> napi::Result<()> {
        let what = what.unwrap_or_else(|| "message".to_owned());
        let reader = xdr::XdrReader::new(&self.bytes[self.offset..]);
        reader
            .end(&what)
            .map_err(|error| reader_error(error, self.offset))
    }
}

/// An owned XDR writer backed by the transport crate's writer.
#[napi]
pub struct NfsXdrWriter {
    inner: xdr::XdrWriter,
}

#[napi]
impl NfsXdrWriter {
    #[napi(constructor)]
    pub fn new(capacity: Option<f64>) -> napi::Result<Self> {
        let inner = match capacity {
            Some(capacity) => {
                xdr::XdrWriter::with_capacity(usize_value("capacity", Some(capacity), 512)?)
            }
            None => xdr::XdrWriter::new(),
        };
        Ok(Self { inner })
    }

    #[napi(getter)]
    pub fn length(&self) -> u32 {
        self.inner.len() as u32
    }

    #[napi]
    pub fn ensure(&mut self, count: f64) -> napi::Result<()> {
        let count = usize_value("count", Some(count), 0)?;
        self.inner.ensure(count);
        Ok(())
    }

    #[napi]
    pub fn truncate(&mut self, length: f64) -> napi::Result<()> {
        let length = usize_value("length", Some(length), 0)?;
        self.inner
            .truncate(length)
            .map_err(|error| codec_error(error, 0))
    }

    #[napi]
    pub fn u32(&mut self, value: u32) {
        self.inner.u32(value);
    }

    #[napi]
    pub fn i32(&mut self, value: i32) {
        self.inner.i32(value);
    }

    #[napi]
    pub fn u64(&mut self, value: BigInt) -> napi::Result<()> {
        self.inner.u64(u64_from_bigint(value, "value")?);
        Ok(())
    }

    #[napi]
    pub fn i64(&mut self, value: BigInt) -> napi::Result<()> {
        self.inner.i64(i64_from_bigint(value, "value")?);
        Ok(())
    }

    #[napi]
    pub fn bool(&mut self, value: bool) {
        self.inner.bool(value);
    }

    #[napi(js_name = "fixedOpaque")]
    pub fn fixed_opaque(
        &mut self,
        #[napi(ts_arg_type = "Uint8Array")] value: Buffer,
        length: Option<f64>,
    ) -> napi::Result<()> {
        let bytes = value.as_ref();
        let length = length
            .map(|length| usize_value("length", Some(length), bytes.len()))
            .transpose()?
            .unwrap_or(bytes.len());
        self.inner.fixed_opaque(bytes, length);
        Ok(())
    }

    #[napi(js_name = "varOpaque")]
    pub fn var_opaque(&mut self, #[napi(ts_arg_type = "Uint8Array")] value: Buffer) {
        self.inner.var_opaque(value.as_ref());
    }

    #[napi]
    pub fn string(&mut self, value: String) {
        self.inner.string(&value);
    }

    #[napi]
    pub fn raw(&mut self, #[napi(ts_arg_type = "Uint8Array")] value: Buffer) {
        self.inner.raw(value.as_ref());
    }

    #[napi]
    pub fn bytes(&self) -> Buffer {
        Buffer::from(self.inner.bytes())
    }

    #[napi]
    pub fn view(&self) -> Buffer {
        // N-API cannot safely expose a borrowed view into a Rust object whose
        // next method may reallocate.  Returning an owned copy preserves the
        // byte contract without retaining invalid memory.
        Buffer::from(self.inner.bytes())
    }

    #[napi(js_name = "nfsWriteAcceptedReplyHeader")]
    pub fn write_accepted_reply_header(&mut self, xid: u32) {
        rpc::write_accepted_reply_header(&mut self.inner, xid);
    }
}

#[napi(js_name = "nfsXdrPad")]
pub fn nfs_xdr_pad(length: f64) -> napi::Result<u32> {
    Ok(xdr::xdr_pad(usize_value("length", Some(length), 0)?) as u32)
}

#[napi(js_name = "nfsXdrAlign")]
pub fn nfs_xdr_align(length: f64) -> napi::Result<u32> {
    let length = usize_value("length", Some(length), 0)?;
    Ok(xdr::xdr_align(length) as u32)
}

#[napi(js_name = "nfsStringByteLength")]
pub fn nfs_string_byte_length(value: String) -> u32 {
    value.len() as u32
}

#[napi(object)]
pub struct NfsRpcConstants {
    #[napi(js_name = "RPC_VERSION")]
    pub rpc_version: u32,
    #[napi(js_name = "RPC_CALL")]
    pub rpc_call: u32,
    #[napi(js_name = "RPC_REPLY")]
    pub rpc_reply: u32,
    #[napi(js_name = "MSG_ACCEPTED")]
    pub msg_accepted: u32,
    #[napi(js_name = "MSG_DENIED")]
    pub msg_denied: u32,
    #[napi(js_name = "RPC_SUCCESS")]
    pub rpc_success: u32,
    #[napi(js_name = "RPC_PROG_UNAVAIL")]
    pub rpc_prog_unavail: u32,
    #[napi(js_name = "RPC_PROG_MISMATCH")]
    pub rpc_prog_mismatch: u32,
    #[napi(js_name = "RPC_PROC_UNAVAIL")]
    pub rpc_proc_unavail: u32,
    #[napi(js_name = "RPC_GARBAGE_ARGS")]
    pub rpc_garbage_args: u32,
    #[napi(js_name = "RPC_SYSTEM_ERR")]
    pub rpc_system_err: u32,
    #[napi(js_name = "RPC_MISMATCH")]
    pub rpc_mismatch: u32,
    #[napi(js_name = "RPC_AUTH_ERROR")]
    pub rpc_auth_error: u32,
    #[napi(js_name = "AUTH_OK")]
    pub auth_ok: u32,
    #[napi(js_name = "AUTH_BADCRED")]
    pub auth_badcred: u32,
    #[napi(js_name = "AUTH_REJECTEDCRED")]
    pub auth_rejectedcred: u32,
    #[napi(js_name = "AUTH_BADVERF")]
    pub auth_badverf: u32,
    #[napi(js_name = "AUTH_REJECTEDVERF")]
    pub auth_rejectedverf: u32,
    #[napi(js_name = "AUTH_TOOWEAK")]
    pub auth_tooweak: u32,
    #[napi(js_name = "AUTH_INVALIDRESP")]
    pub auth_invalidresp: u32,
    #[napi(js_name = "AUTH_FAILED")]
    pub auth_failed: u32,
    #[napi(js_name = "AUTH_NONE")]
    pub auth_none: u32,
    #[napi(js_name = "AUTH_SYS")]
    pub auth_sys: u32,
    #[napi(js_name = "AUTH_SHORT")]
    pub auth_short: u32,
    #[napi(js_name = "RPC_MAX_AUTH_BYTES")]
    pub rpc_max_auth_bytes: u32,
    #[napi(js_name = "RM_LAST_FRAGMENT")]
    pub rm_last_fragment: u32,
    #[napi(js_name = "RM_LENGTH_MASK")]
    pub rm_length_mask: u32,
    #[napi(js_name = "ACCEPTED_REPLY_HEADER_SIZE")]
    pub accepted_reply_header_size: u32,
    #[napi(js_name = "DEFAULT_RECORD_LIMIT")]
    pub default_record_limit: u32,
}

#[napi(js_name = "nfsRpcConstants")]
pub fn nfs_rpc_constants() -> NfsRpcConstants {
    NfsRpcConstants {
        rpc_version: rpc::RPC_VERSION,
        rpc_call: rpc::RPC_CALL,
        rpc_reply: rpc::RPC_REPLY,
        msg_accepted: rpc::MSG_ACCEPTED,
        msg_denied: rpc::MSG_DENIED,
        rpc_success: rpc::RPC_SUCCESS,
        rpc_prog_unavail: rpc::RPC_PROG_UNAVAIL,
        rpc_prog_mismatch: rpc::RPC_PROG_MISMATCH,
        rpc_proc_unavail: rpc::RPC_PROC_UNAVAIL,
        rpc_garbage_args: rpc::RPC_GARBAGE_ARGS,
        rpc_system_err: rpc::RPC_SYSTEM_ERR,
        rpc_mismatch: rpc::RPC_MISMATCH,
        rpc_auth_error: rpc::RPC_AUTH_ERROR,
        auth_ok: rpc::AUTH_OK,
        auth_badcred: rpc::AUTH_BADCRED,
        auth_rejectedcred: rpc::AUTH_REJECTEDCRED,
        auth_badverf: rpc::AUTH_BADVERF,
        auth_rejectedverf: rpc::AUTH_REJECTEDVERF,
        auth_tooweak: rpc::AUTH_TOOWEAK,
        auth_invalidresp: rpc::AUTH_INVALIDRESP,
        auth_failed: rpc::AUTH_FAILED,
        auth_none: rpc::AUTH_NONE,
        auth_sys: rpc::AUTH_SYS,
        auth_short: rpc::AUTH_SHORT,
        rpc_max_auth_bytes: rpc::RPC_MAX_AUTH_BYTES as u32,
        rm_last_fragment: rpc::RM_LAST_FRAGMENT,
        rm_length_mask: rpc::RM_LENGTH_MASK,
        accepted_reply_header_size: rpc::ACCEPTED_REPLY_HEADER_SIZE as u32,
        default_record_limit: rpc::DEFAULT_RECORD_LIMIT as u32,
    }
}

#[napi(object)]
pub struct NfsOpaqueAuth {
    pub flavor: u32,
    #[napi(ts_type = "Uint8Array")]
    pub body: Buffer,
}

fn to_auth(value: &NfsOpaqueAuth) -> rpc::OpaqueAuth {
    rpc::OpaqueAuth {
        flavor: value.flavor,
        body: value.body.as_ref().to_vec(),
    }
}

fn from_auth(value: rpc::OpaqueAuth) -> NfsOpaqueAuth {
    NfsOpaqueAuth {
        flavor: value.flavor,
        body: Buffer::from(value.body),
    }
}

#[napi(js_name = "nfsAuthNull")]
pub fn nfs_auth_null() -> NfsOpaqueAuth {
    from_auth(rpc::auth_null())
}

#[napi(object)]
pub struct NfsAuthSysParams {
    pub stamp: u32,
    #[napi(js_name = "machineName")]
    pub machine_name: String,
    pub uid: u32,
    pub gid: u32,
    pub gids: Vec<u32>,
}

fn to_auth_sys(value: &NfsAuthSysParams) -> rpc::AuthSysParams {
    rpc::AuthSysParams {
        stamp: value.stamp,
        machine_name: value.machine_name.clone(),
        uid: value.uid,
        gid: value.gid,
        gids: value.gids.clone(),
    }
}

fn from_auth_sys(value: rpc::AuthSysParams) -> NfsAuthSysParams {
    NfsAuthSysParams {
        stamp: value.stamp,
        machine_name: value.machine_name,
        uid: value.uid,
        gid: value.gid,
        gids: value.gids,
    }
}

#[napi(js_name = "nfsEncodeAuthSys")]
pub fn nfs_encode_auth_sys(params: NfsAuthSysParams) -> Buffer {
    Buffer::from(rpc::encode_auth_sys(&to_auth_sys(&params)))
}

#[napi(js_name = "nfsDecodeAuthSys")]
pub fn nfs_decode_auth_sys(
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
) -> napi::Result<NfsAuthSysParams> {
    rpc::decode_auth_sys(body.as_ref())
        .map(from_auth_sys)
        .map_err(|error| codec_error(error, 0))
}

#[napi(js_name = "nfsAuthSys")]
pub fn nfs_auth_sys(
    uid: Option<f64>,
    gid: Option<f64>,
    machine_name: Option<String>,
) -> napi::Result<NfsOpaqueAuth> {
    let uid = u32_value("uid", uid, 0)?;
    let gid = u32_value("gid", gid, 0)?;
    Ok(from_auth(rpc::auth_sys(
        uid,
        gid,
        machine_name.unwrap_or_else(|| "mountx".to_owned()),
    )))
}

#[napi(object)]
pub struct NfsRpcCredentials {
    pub flavor: u32,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub gids: Vec<u32>,
}

#[napi(js_name = "nfsCredentialsOf")]
pub fn nfs_credentials_of(credential: NfsOpaqueAuth) -> NfsRpcCredentials {
    let value = rpc::credentials_of(&to_auth(&credential));
    NfsRpcCredentials {
        flavor: value.flavor,
        uid: value.uid,
        gid: value.gid,
        gids: value.gids,
    }
}

#[napi(object)]
pub struct NfsRpcCall {
    pub xid: u32,
    #[napi(js_name = "rpcVersion")]
    pub rpc_version: u32,
    pub program: u32,
    pub version: u32,
    pub procedure: u32,
    pub cred: NfsOpaqueAuth,
    pub verf: NfsOpaqueAuth,
}

pub(crate) fn from_call(value: rpc::RpcCall) -> NfsRpcCall {
    NfsRpcCall {
        xid: value.xid,
        rpc_version: value.rpc_version,
        program: value.program,
        version: value.version,
        procedure: value.procedure,
        cred: from_auth(value.cred),
        verf: from_auth(value.verifier),
    }
}

#[napi(object)]
pub struct NfsRpcCallOptions {
    pub xid: u32,
    pub program: u32,
    pub version: u32,
    pub procedure: u32,
    pub cred: Option<NfsOpaqueAuth>,
    pub verf: Option<NfsOpaqueAuth>,
    #[napi(ts_type = "Uint8Array")]
    pub args: Option<Buffer>,
}

#[napi(object)]
pub struct NfsDecodedCall {
    pub call: NfsRpcCall,
    #[napi(js_name = "argsBytes", ts_type = "Uint8Array")]
    pub args_bytes: Buffer,
    pub args_offset: u32,
}

#[napi(js_name = "nfsDecodeCall")]
pub fn nfs_decode_call(
    #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
) -> napi::Result<NfsDecodedCall> {
    let bytes = bytes.as_ref().to_vec();
    let (call, args_offset) = {
        let (call, reader) = rpc::decode_call(&bytes).map_err(|error| codec_error(error, 0))?;
        (from_call(call), reader.offset())
    };
    Ok(NfsDecodedCall {
        call,
        args_bytes: Buffer::from(bytes),
        args_offset: args_offset as u32,
    })
}

#[napi(js_name = "nfsEncodeCall")]
pub fn nfs_encode_call(options: NfsRpcCallOptions) -> Buffer {
    let credential = options.cred.as_ref().map(to_auth);
    let verifier = options.verf.as_ref().map(to_auth);
    let args = options
        .args
        .as_ref()
        .map_or(&[][..], |value| value.as_ref());
    Buffer::from(rpc::encode_call(
        options.xid,
        options.program,
        options.version,
        options.procedure,
        credential.as_ref(),
        verifier.as_ref(),
        args,
    ))
}

#[napi(object)]
pub struct NfsRpcReply {
    pub xid: u32,
    #[napi(js_name = "replyStat")]
    pub reply_stat: u32,
    #[napi(js_name = "acceptStat")]
    pub accept_stat: Option<u32>,
    #[napi(js_name = "rejectStat")]
    pub reject_stat: Option<u32>,
    #[napi(js_name = "authStat")]
    pub auth_stat: Option<u32>,
    pub low: Option<u32>,
    pub high: Option<u32>,
    pub verf: Option<NfsOpaqueAuth>,
}

fn from_reply(value: rpc::RpcReply) -> NfsRpcReply {
    NfsRpcReply {
        xid: value.xid,
        reply_stat: value.reply_stat,
        accept_stat: value.accept_stat,
        reject_stat: value.reject_stat,
        auth_stat: value.auth_stat,
        low: value.low,
        high: value.high,
        verf: value.verifier.map(from_auth),
    }
}

#[napi(object)]
pub struct NfsDecodedReply {
    pub reply: NfsRpcReply,
    #[napi(js_name = "resultsBytes", ts_type = "Uint8Array")]
    pub results_bytes: Buffer,
    pub results_offset: u32,
}

#[napi(js_name = "nfsDecodeReply")]
pub fn nfs_decode_reply(
    #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
) -> napi::Result<NfsDecodedReply> {
    let bytes = bytes.as_ref().to_vec();
    let (reply, results_offset) = {
        let (reply, reader) = rpc::decode_reply(&bytes).map_err(|error| codec_error(error, 0))?;
        (from_reply(reply), reader.offset())
    };
    Ok(NfsDecodedReply {
        reply,
        results_bytes: Buffer::from(bytes),
        results_offset: results_offset as u32,
    })
}

#[napi(object)]
pub struct NfsRpcMismatch {
    pub low: u32,
    pub high: u32,
}

#[napi(js_name = "nfsEncodeAcceptedReply")]
pub fn nfs_encode_accepted_reply(xid: u32, results: Option<Buffer>) -> Buffer {
    Buffer::from(rpc::encode_accepted_reply(
        xid,
        results.as_ref().map_or(&[][..], |value| value.as_ref()),
    ))
}

#[napi(js_name = "nfsEncodeAcceptError")]
pub fn nfs_encode_accept_error(
    xid: u32,
    accept_stat: u32,
    mismatch: Option<NfsRpcMismatch>,
) -> Buffer {
    Buffer::from(rpc::encode_accept_error(
        xid,
        accept_stat,
        mismatch.map(|value| (value.low, value.high)),
    ))
}

#[napi(js_name = "nfsEncodeAuthError")]
pub fn nfs_encode_auth_error(xid: u32, auth_stat: u32) -> Buffer {
    Buffer::from(rpc::encode_auth_error(xid, auth_stat))
}

#[napi(js_name = "nfsEncodeRpcMismatch")]
pub fn nfs_encode_rpc_mismatch(xid: u32, low: Option<u32>, high: Option<u32>) -> Buffer {
    Buffer::from(rpc::encode_rpc_mismatch(
        xid,
        low.unwrap_or(rpc::RPC_VERSION),
        high.unwrap_or(rpc::RPC_VERSION),
    ))
}

#[napi(js_name = "nfsRecordMark")]
pub fn nfs_record_mark(length: f64) -> napi::Result<Buffer> {
    let length = usize_value("length", Some(length), 0)?;
    rpc::record_mark(length)
        .map(|mark| Buffer::from(mark.to_vec()))
        .map_err(|error| codec_error(error, 0))
}

#[napi(js_name = "nfsFrameRecord")]
pub fn nfs_frame_record(
    #[napi(ts_arg_type = "Uint8Array")] message: Buffer,
) -> napi::Result<Buffer> {
    rpc::frame_record(message.as_ref())
        .map(Buffer::from)
        .map_err(|error| codec_error(error, 0))
}

#[napi(js_name = "nfsFrameFragments")]
pub fn nfs_frame_fragments(
    #[napi(ts_arg_type = "Uint8Array")] message: Buffer,
    size: f64,
) -> napi::Result<Buffer> {
    let size = usize_value("size", Some(size), 0)?;
    rpc::frame_fragments(message.as_ref(), size)
        .map(Buffer::from)
        .map_err(|error| codec_error(error, 0))
}

#[napi(js_name = "nfsCopyBytes")]
pub fn nfs_copy_bytes(
    #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
    start: Option<f64>,
    end: Option<f64>,
) -> napi::Result<Buffer> {
    let bytes = bytes.as_ref();
    let start = usize_value("start", start, 0)?;
    let end = usize_value("end", end, bytes.len())?;
    if start > end || end > bytes.len() {
        return Err(invalid_argument(format!(
            "copy range must satisfy 0 <= start <= end <= {}",
            bytes.len()
        )));
    }
    Ok(Buffer::from(bytes[start..end].to_vec()))
}

#[napi]
pub struct NfsRecordAssembler {
    inner: rpc::RecordAssembler,
}

#[napi]
impl NfsRecordAssembler {
    #[napi(constructor)]
    pub fn new(limit: Option<f64>) -> napi::Result<Self> {
        let limit = usize_value("limit", limit, rpc::DEFAULT_RECORD_LIMIT)?;
        Ok(Self {
            inner: rpc::RecordAssembler::new(limit),
        })
    }

    #[napi(getter)]
    pub fn pending(&self) -> u32 {
        self.inner.pending() as u32
    }

    #[napi]
    pub fn push(
        &mut self,
        #[napi(ts_arg_type = "Uint8Array")] chunk: Buffer,
    ) -> napi::Result<Vec<Buffer>> {
        self.inner
            .push(chunk.as_ref())
            .map(|records| records.into_iter().map(Buffer::from).collect())
            .map_err(|error| codec_error(error, 0))
    }
}
