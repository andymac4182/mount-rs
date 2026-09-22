//! ONC RPC v2 (RFC 5531) and TCP record marking (RFC 5531 §11).

use crate::xdr::{XdrError, XdrReader, XdrWriter};

pub const RPC_VERSION: u32 = 2;
pub const RPC_CALL: u32 = 0;
pub const RPC_REPLY: u32 = 1;
pub const MSG_ACCEPTED: u32 = 0;
pub const MSG_DENIED: u32 = 1;
pub const RPC_SUCCESS: u32 = 0;
pub const RPC_PROG_UNAVAIL: u32 = 1;
pub const RPC_PROG_MISMATCH: u32 = 2;
pub const RPC_PROC_UNAVAIL: u32 = 3;
pub const RPC_GARBAGE_ARGS: u32 = 4;
pub const RPC_SYSTEM_ERR: u32 = 5;
pub const RPC_MISMATCH: u32 = 0;
pub const RPC_AUTH_ERROR: u32 = 1;
pub const AUTH_OK: u32 = 0;
pub const AUTH_BADCRED: u32 = 1;
pub const AUTH_REJECTEDCRED: u32 = 2;
pub const AUTH_BADVERF: u32 = 3;
pub const AUTH_REJECTEDVERF: u32 = 4;
pub const AUTH_TOOWEAK: u32 = 5;
pub const AUTH_INVALIDRESP: u32 = 6;
pub const AUTH_FAILED: u32 = 7;
pub const AUTH_NONE: u32 = 0;
pub const AUTH_SYS: u32 = 1;
pub const AUTH_SHORT: u32 = 2;
pub const RPC_MAX_AUTH_BYTES: usize = 400;
pub const RM_LAST_FRAGMENT: u32 = 0x8000_0000;
pub const RM_LENGTH_MASK: u32 = 0x7fff_ffff;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueAuth {
    pub flavor: u32,
    pub body: Vec<u8>,
}

pub fn auth_null() -> OpaqueAuth {
    OpaqueAuth {
        flavor: AUTH_NONE,
        body: Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthSysParams {
    pub stamp: u32,
    pub machine_name: String,
    pub uid: u32,
    pub gid: u32,
    pub gids: Vec<u32>,
}

pub fn encode_auth_sys(params: &AuthSysParams) -> Vec<u8> {
    crate::xdr::encode_xdr(|writer| {
        writer.u32(params.stamp);
        writer.string(&params.machine_name);
        writer.u32(params.uid);
        writer.u32(params.gid);
        let gids = &params.gids[..params.gids.len().min(16)];
        writer.array(gids, |writer, gid| writer.u32(*gid));
    })
}

pub fn decode_auth_sys(body: &[u8]) -> Result<AuthSysParams, XdrError> {
    let mut reader = XdrReader::new(body);
    let result = AuthSysParams {
        stamp: reader.u32("authsys stamp")?,
        machine_name: reader.string(255, "authsys machinename")?,
        uid: reader.u32("authsys uid")?,
        gid: reader.u32("authsys gid")?,
        gids: reader.array(16, "authsys gids", |reader| reader.u32("gid"))?,
    };
    reader.end("authsys body")?;
    Ok(result)
}

pub fn auth_sys(uid: u32, gid: u32, machine_name: impl Into<String>) -> OpaqueAuth {
    OpaqueAuth {
        flavor: AUTH_SYS,
        body: encode_auth_sys(&AuthSysParams {
            stamp: 0,
            machine_name: machine_name.into(),
            uid,
            gid,
            gids: Vec::new(),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcCredentials {
    pub flavor: u32,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub gids: Vec<u32>,
}

pub fn credentials_of(credential: &OpaqueAuth) -> RpcCredentials {
    if credential.flavor != AUTH_SYS {
        return RpcCredentials {
            flavor: credential.flavor,
            uid: None,
            gid: None,
            gids: Vec::new(),
        };
    }
    match decode_auth_sys(&credential.body) {
        Ok(auth) => RpcCredentials {
            flavor: credential.flavor,
            uid: Some(auth.uid),
            gid: Some(auth.gid),
            gids: auth.gids,
        },
        Err(_) => RpcCredentials {
            flavor: credential.flavor,
            uid: None,
            gid: None,
            gids: Vec::new(),
        },
    }
}

/// Validate a call credential before any protocol dispatch. The public
/// `credentials_of` helper is intentionally lossy for codec inspection; a
/// server must not turn a malformed AUTH_SYS body into an anonymous or root
/// identity by using that helper alone.
pub(crate) fn checked_credentials_of(credential: &OpaqueAuth) -> Result<RpcCredentials, u32> {
    match credential.flavor {
        AUTH_NONE if credential.body.is_empty() => Ok(credentials_of(credential)),
        AUTH_NONE => Err(AUTH_BADCRED),
        AUTH_SYS => {
            let auth = decode_auth_sys(&credential.body).map_err(|_| AUTH_BADCRED)?;
            Ok(RpcCredentials {
                flavor: AUTH_SYS,
                uid: Some(auth.uid),
                gid: Some(auth.gid),
                gids: auth.gids,
            })
        }
        _ => Err(AUTH_TOOWEAK),
    }
}

fn read_auth(reader: &mut XdrReader<'_>, what: &str) -> Result<OpaqueAuth, XdrError> {
    Ok(OpaqueAuth {
        flavor: reader.u32(&format!("{what} flavor"))?,
        body: reader.var_opaque(RPC_MAX_AUTH_BYTES, &format!("{what} body"))?,
    })
}

fn write_auth(writer: &mut XdrWriter, auth: &OpaqueAuth) {
    writer.u32(auth.flavor);
    writer.var_opaque(&auth.body);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcCall {
    pub xid: u32,
    pub rpc_version: u32,
    pub program: u32,
    pub version: u32,
    pub procedure: u32,
    pub cred: OpaqueAuth,
    pub verifier: OpaqueAuth,
}

pub fn decode_call(bytes: &[u8]) -> Result<(RpcCall, XdrReader<'_>), XdrError> {
    let mut reader = XdrReader::new(bytes);
    let xid = reader.u32("xid")?;
    let message_type = reader.u32("mtype")?;
    if message_type != RPC_CALL {
        return Err(XdrError::new(
            format!("expected an RPC call, got mtype {message_type}"),
            4,
        ));
    }
    let call = RpcCall {
        xid,
        rpc_version: reader.u32("rpcvers")?,
        program: reader.u32("prog")?,
        version: reader.u32("vers")?,
        procedure: reader.u32("proc")?,
        cred: read_auth(&mut reader, "cred")?,
        verifier: read_auth(&mut reader, "verf")?,
    };
    Ok((call, reader))
}

pub fn encode_call(
    xid: u32,
    program: u32,
    version: u32,
    procedure: u32,
    cred: Option<&OpaqueAuth>,
    verifier: Option<&OpaqueAuth>,
    args: &[u8],
) -> Vec<u8> {
    let null = auth_null();
    let mut writer = XdrWriter::with_capacity(128 + args.len());
    writer.u32(xid);
    writer.u32(RPC_CALL);
    writer.u32(RPC_VERSION);
    writer.u32(program);
    writer.u32(version);
    writer.u32(procedure);
    write_auth(&mut writer, cred.unwrap_or(&null));
    write_auth(&mut writer, verifier.unwrap_or(&null));
    writer.raw(args);
    writer.into_bytes()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcReply {
    pub xid: u32,
    pub reply_stat: u32,
    pub accept_stat: Option<u32>,
    pub reject_stat: Option<u32>,
    pub auth_stat: Option<u32>,
    pub low: Option<u32>,
    pub high: Option<u32>,
    pub verifier: Option<OpaqueAuth>,
}

pub fn decode_reply(bytes: &[u8]) -> Result<(RpcReply, XdrReader<'_>), XdrError> {
    let mut reader = XdrReader::new(bytes);
    let xid = reader.u32("xid")?;
    let message_type = reader.u32("mtype")?;
    if message_type != RPC_REPLY {
        return Err(XdrError::new(
            format!("expected an RPC reply, got mtype {message_type}"),
            4,
        ));
    }
    let reply_stat = reader.u32("reply_stat")?;
    let mut reply = RpcReply {
        xid,
        reply_stat,
        accept_stat: None,
        reject_stat: None,
        auth_stat: None,
        low: None,
        high: None,
        verifier: None,
    };
    match reply_stat {
        MSG_ACCEPTED => {
            reply.verifier = Some(read_auth(&mut reader, "reply verf")?);
            let accept_stat = reader.u32("accept_stat")?;
            reply.accept_stat = Some(accept_stat);
            if accept_stat == RPC_PROG_MISMATCH {
                reply.low = Some(reader.u32("mismatch low")?);
                reply.high = Some(reader.u32("mismatch high")?);
            }
        }
        MSG_DENIED => {
            let reject_stat = reader.u32("reject_stat")?;
            reply.reject_stat = Some(reject_stat);
            match reject_stat {
                RPC_MISMATCH => {
                    reply.low = Some(reader.u32("mismatch low")?);
                    reply.high = Some(reader.u32("mismatch high")?);
                }
                RPC_AUTH_ERROR => reply.auth_stat = Some(reader.u32("auth_stat")?),
                _ => return Err(XdrError::new("unknown reject_stat", reader.offset())),
            }
        }
        _ => return Err(XdrError::new("unknown reply_stat", reader.offset())),
    }
    Ok((reply, reader))
}

pub const ACCEPTED_REPLY_HEADER_SIZE: usize = 24;

pub fn write_accepted_reply_header(writer: &mut XdrWriter, xid: u32) {
    let null = auth_null();
    writer.u32(xid);
    writer.u32(RPC_REPLY);
    writer.u32(MSG_ACCEPTED);
    write_auth(writer, &null);
    writer.u32(RPC_SUCCESS);
}

pub fn encode_accepted_reply(xid: u32, results: &[u8]) -> Vec<u8> {
    let mut writer = XdrWriter::with_capacity(ACCEPTED_REPLY_HEADER_SIZE + results.len());
    write_accepted_reply_header(&mut writer, xid);
    writer.raw(results);
    writer.into_bytes()
}

pub fn encode_accept_error(xid: u32, status: u32, mismatch: Option<(u32, u32)>) -> Vec<u8> {
    let mut writer = XdrWriter::with_capacity(48);
    let null = auth_null();
    writer.u32(xid);
    writer.u32(RPC_REPLY);
    writer.u32(MSG_ACCEPTED);
    write_auth(&mut writer, &null);
    writer.u32(status);
    if let Some((low, high)) = mismatch {
        writer.u32(low);
        writer.u32(high);
    }
    writer.into_bytes()
}

pub fn encode_auth_error(xid: u32, status: u32) -> Vec<u8> {
    let mut writer = XdrWriter::with_capacity(32);
    writer.u32(xid);
    writer.u32(RPC_REPLY);
    writer.u32(MSG_DENIED);
    writer.u32(RPC_AUTH_ERROR);
    writer.u32(status);
    writer.into_bytes()
}

pub fn encode_rpc_mismatch(xid: u32, low: u32, high: u32) -> Vec<u8> {
    let mut writer = XdrWriter::with_capacity(32);
    writer.u32(xid);
    writer.u32(RPC_REPLY);
    writer.u32(MSG_DENIED);
    writer.u32(RPC_MISMATCH);
    writer.u32(low);
    writer.u32(high);
    writer.into_bytes()
}

pub fn record_mark(length: usize) -> Result<[u8; 4], XdrError> {
    if length > RM_LENGTH_MASK as usize {
        return Err(XdrError::new("RPC fragment is too large", 0));
    }
    Ok((RM_LAST_FRAGMENT | length as u32).to_be_bytes())
}

pub fn frame_record(message: &[u8]) -> Result<Vec<u8>, XdrError> {
    let mark = record_mark(message.len())?;
    let mut framed = Vec::with_capacity(4 + message.len());
    framed.extend_from_slice(&mark);
    framed.extend_from_slice(message);
    Ok(framed)
}

pub fn frame_fragments(message: &[u8], size: usize) -> Result<Vec<u8>, XdrError> {
    if size == 0 {
        return Err(XdrError::new("fragment size must be positive", 0));
    }
    let mut result = Vec::new();
    if message.is_empty() {
        result.extend_from_slice(&record_mark(0)?);
        return Ok(result);
    }
    let mut at = 0;
    while at < message.len() {
        let end = (at + size).min(message.len());
        let last = end == message.len();
        let mark = (if last { RM_LAST_FRAGMENT } else { 0 } | (end - at) as u32).to_be_bytes();
        result.extend_from_slice(&mark);
        result.extend_from_slice(&message[at..end]);
        at = end;
    }
    Ok(result)
}

pub const DEFAULT_RECORD_LIMIT: usize = 8 * 1024 * 1024;

/// Reassembles TCP record-marking fragments. Returned records own their bytes.
#[derive(Debug, Clone)]
pub struct RecordAssembler {
    buffer: Vec<u8>,
    fragments: Vec<Vec<u8>>,
    assembled: usize,
    fragment_count: usize,
    limit: usize,
    max_fragments: usize,
}

impl Default for RecordAssembler {
    fn default() -> Self {
        Self::new(DEFAULT_RECORD_LIMIT)
    }
}

impl RecordAssembler {
    pub fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            fragments: Vec::new(),
            assembled: 0,
            fragment_count: 0,
            limit,
            max_fragments: (limit / 1024).max(64),
        }
    }

    pub fn pending(&self) -> usize {
        self.buffer.len() + self.assembled + self.fragment_count * 4
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, XdrError> {
        self.buffer.extend_from_slice(chunk);
        let mut records = Vec::new();
        let mut consumed = 0;
        loop {
            if self.buffer.len().saturating_sub(consumed) < 4 {
                break;
            }
            let mark = u32::from_be_bytes(
                self.buffer[consumed..consumed + 4]
                    .try_into()
                    .expect("record mark is four bytes"),
            );
            let length = (mark & RM_LENGTH_MASK) as usize;
            if self.assembled.saturating_add(length) > self.limit {
                return Err(XdrError::new(
                    format!("RPC record exceeds {} bytes", self.limit),
                    consumed,
                ));
            }
            if self.fragment_count >= self.max_fragments {
                return Err(XdrError::new(
                    format!("RPC record exceeds {} fragments", self.max_fragments),
                    consumed,
                ));
            }
            if self.buffer.len().saturating_sub(consumed) < 4 + length {
                break;
            }
            consumed += 4;
            if length != 0 {
                self.fragments
                    .push(self.buffer[consumed..consumed + length].to_vec());
                consumed += length;
            }
            self.assembled += length;
            self.fragment_count += 1;
            if mark & RM_LAST_FRAGMENT != 0 {
                let mut record = Vec::with_capacity(self.assembled);
                for fragment in self.fragments.drain(..) {
                    record.extend_from_slice(&fragment);
                }
                records.push(record);
                self.assembled = 0;
                self.fragment_count = 0;
            }
        }
        if consumed > 0 {
            self.buffer.drain(..consumed);
        }
        if self.buffer.len() + self.assembled + self.fragment_count * 4 > self.limit + 4 {
            return Err(XdrError::new("RPC record buffer exceeds limit", 0));
        }
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_and_fragmented_record_round_trip() {
        let credential = auth_sys(1000, 1000, "test");
        let call = encode_call(7, 100_003, 3, 6, Some(&credential), None, &[1, 2, 3]);
        let framed = frame_fragments(&call, 5).unwrap();
        let mut assembler = RecordAssembler::new(1024);
        let mut records = Vec::new();
        for chunk in framed.chunks(3) {
            records.extend(assembler.push(chunk).unwrap());
        }
        assert_eq!(records, vec![call]);
        let (decoded, mut args) = decode_call(&records[0]).unwrap();
        assert_eq!(decoded.xid, 7);
        assert_eq!(decoded.program, 100_003);
        assert_eq!(credentials_of(&decoded.cred).uid, Some(1000));
        assert_eq!(args.rest(), vec![1, 2, 3]);
    }

    #[test]
    fn accepted_reply_has_rfc_header() {
        let reply = encode_accepted_reply(9, &[0xaa]);
        assert_eq!(&reply[..8], &[0, 0, 0, 9, 0, 0, 0, 1]);
        assert_eq!(reply.len(), ACCEPTED_REPLY_HEADER_SIZE + 1);
    }
}
