//! Native 9P2000.L codec bindings.
//!
//! The transport crate owns the wire format.  This module intentionally only
//! translates N-API values to and from the existing Rust reader, writer,
//! protocol codecs, and stream assembler.  The public names are prefixed so
//! the main N-API module can safely expose this surface under its 9P subpath
//! without colliding with the server bindings.

use mount_rs_9p::{
    FidRequest, P9_MAX_ITEM, P9_MAX_STRING, P9Dirent, P9DirentPacker, P9Error, P9FrameAssembler,
    P9Header, P9Qid, P9Reader, P9Time, P9Writer, QidReply, Rattach, Rauth, Rgetattr, Rlerror,
    Rlock, Rlopen, Rread, Rreaddir, Rreadlink, Rstatfs, Rwalk, Rwrite, Rxattrwalk, Tattach, Tauth,
    Tflush, Tfsync, Tgetattr, Tlcreate, Tlink, Tlopen, Tmkdir, Tmknod, Tread, Treaddir, Trename,
    Trenameat, Tsetattr, Tsymlink, Tunlinkat, Tversion, Twalk, Twrite, Txattrcreate, Txattrwalk,
    decode_message, dirent_size, encode_message, read_dirent, read_dirents, read_fid_request,
    read_header, read_qid_reply, read_rattach, read_rauth, read_rgetattr, read_rgetlock,
    read_rlerror, read_rlock, read_rlopen, read_rreadlink, read_rstatfs, read_rversion, read_rwalk,
    read_rwrite, read_rxattrwalk, read_tattach, read_tauth, read_tflush, read_tfsync,
    read_tgetattr, read_tgetlock, read_tlcreate, read_tlink, read_tlock, read_tlopen, read_tmkdir,
    read_tmknod, read_tread, read_treaddir, read_trename, read_trenameat, read_tsetattr,
    read_tsymlink, read_tunlinkat, read_tversion, read_twalk, read_txattrcreate, read_txattrwalk,
    write_dirent, write_fid_request, write_header, write_qid_reply, write_rattach, write_rauth,
    write_rgetattr, write_rgetlock, write_rlerror, write_rlock, write_rlopen, write_rread,
    write_rreaddir, write_rreadlink, write_rstatfs, write_rversion, write_rwalk, write_rwrite,
    write_rxattrwalk, write_tattach, write_tauth, write_tflush, write_tfsync, write_tgetattr,
    write_tgetlock, write_tlcreate, write_tlink, write_tlock, write_tlopen, write_tmkdir,
    write_tmknod, write_tread, write_treaddir, write_trename, write_trenameat, write_tsetattr,
    write_tsymlink, write_tunlinkat, write_tversion, write_twalk, write_twrite, write_txattrcreate,
    write_txattrwalk,
};
use napi::bindgen_prelude::{BigInt, Buffer};
use napi::{Error, Status};
use napi_derive::napi;

const P9_ERROR_MARKER: &str = "__mount_rs_p9_error_v1__";

fn hex(value: &str) -> String {
    let mut output = String::with_capacity(value.len() * 2);
    for byte in value.as_bytes() {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn wire_error(error: P9Error) -> Error {
    Error::new(
        Status::GenericFailure,
        format!(
            "{P9_ERROR_MARKER}|{}|{}",
            error
                .offset
                .map(|offset| offset.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            hex(&error.message),
        ),
    )
}

fn wire_result<T>(result: Result<T, P9Error>) -> napi::Result<T> {
    result.map_err(wire_error)
}

fn bigint_to_u64(value: &BigInt, name: &str) -> napi::Result<u64> {
    let (negative, magnitude, _) = value.get_u128();
    let low = magnitude as u64;
    // The upstream writer applies BigInt.asUintN(64), including to negative
    // and wider values.  Keep that exact modulo-2^64 behavior at the native
    // boundary rather than rejecting values that the JS codec accepts.
    let value = if negative {
        0u64.wrapping_sub(low)
    } else {
        low
    };
    let _ = name;
    Ok(value)
}

fn bigint(value: u64) -> BigInt {
    BigInt::from(value)
}

fn buffer(value: Vec<u8>) -> Buffer {
    Buffer::from(value)
}

fn u32_len(value: usize, what: &str) -> napi::Result<u32> {
    u32::try_from(value).map_err(|_| Error::new(Status::InvalidArg, format!("{what} is too large")))
}

#[napi(object)]
#[derive(Clone)]
pub struct NativeP9Qid {
    #[napi(js_name = "type")]
    pub type_: u8,
    pub version: u32,
    pub path: BigInt,
}

#[napi(object)]
pub struct NativeP9Time {
    pub sec: BigInt,
    pub nsec: BigInt,
}

#[napi(object)]
#[derive(Clone)]
pub struct NativeP9Header {
    pub size: u32,
    #[napi(js_name = "type")]
    pub type_: u8,
    pub tag: u16,
}

#[napi(object)]
pub struct NativeP9Dirent {
    pub qid: NativeP9Qid,
    pub offset: BigInt,
    #[napi(js_name = "type")]
    pub type_: u8,
    pub name: String,
}

#[napi(object)]
pub struct NativeP9Message {
    pub header: NativeP9Header,
    #[napi(ts_type = "Uint8Array")]
    pub body: Buffer,
}

#[napi(object)]
pub struct NativeP9Tversion {
    pub msize: u32,
    pub version: String,
}

#[napi(object)]
pub struct NativeP9Tauth {
    pub afid: u32,
    pub uname: String,
    pub aname: String,
    pub n_uname: u32,
}

#[napi(object)]
pub struct NativeP9Rauth {
    pub aqid: NativeP9Qid,
}

#[napi(object)]
pub struct NativeP9Tattach {
    pub fid: u32,
    pub afid: u32,
    pub uname: String,
    pub aname: String,
    pub n_uname: u32,
}

#[napi(object)]
pub struct NativeP9Rattach {
    pub qid: NativeP9Qid,
}

#[napi(object)]
pub struct NativeP9Rlerror {
    pub ecode: u32,
}

#[napi(object)]
pub struct NativeP9Tflush {
    pub oldtag: u16,
}

#[napi(object)]
pub struct NativeP9Twalk {
    pub fid: u32,
    pub newfid: u32,
    pub wnames: Vec<String>,
}

#[napi(object)]
pub struct NativeP9Rwalk {
    pub wqids: Vec<NativeP9Qid>,
}

#[napi(object)]
pub struct NativeP9Tread {
    pub fid: u32,
    pub offset: BigInt,
    pub count: u32,
}

#[napi(object)]
pub struct NativeP9Rread {
    #[napi(ts_type = "Uint8Array")]
    pub data: Buffer,
}

#[napi(object)]
pub struct NativeP9Twrite {
    pub fid: u32,
    pub offset: BigInt,
    #[napi(ts_type = "Uint8Array")]
    pub data: Buffer,
}

#[napi(object)]
pub struct NativeP9Rwrite {
    pub count: u32,
}

#[napi(object)]
pub struct NativeP9FidRequest {
    pub fid: u32,
}

#[napi(object)]
pub struct NativeP9Rstatfs {
    #[napi(js_name = "type")]
    pub type_: u32,
    pub bsize: u32,
    pub blocks: BigInt,
    pub bfree: BigInt,
    pub bavail: BigInt,
    pub files: BigInt,
    pub ffree: BigInt,
    pub fsid: BigInt,
    pub namelen: u32,
}

#[napi(object)]
pub struct NativeP9Tlopen {
    pub fid: u32,
    pub flags: u32,
}

#[napi(object)]
pub struct NativeP9Rlopen {
    pub qid: NativeP9Qid,
    pub iounit: u32,
}

#[napi(object)]
pub struct NativeP9Tlcreate {
    pub fid: u32,
    pub name: String,
    pub flags: u32,
    pub mode: u32,
    pub gid: u32,
}

#[napi(object)]
pub struct NativeP9Tsymlink {
    pub dfid: u32,
    pub name: String,
    pub symtgt: String,
    pub gid: u32,
}

#[napi(object)]
pub struct NativeP9QidReply {
    pub qid: NativeP9Qid,
}

#[napi(object)]
pub struct NativeP9Tmknod {
    pub dfid: u32,
    pub name: String,
    pub mode: u32,
    pub major: u32,
    pub minor: u32,
    pub gid: u32,
}

#[napi(object)]
pub struct NativeP9Tmkdir {
    pub dfid: u32,
    pub name: String,
    pub mode: u32,
    pub gid: u32,
}

#[napi(object)]
pub struct NativeP9Trename {
    pub fid: u32,
    pub dfid: u32,
    pub name: String,
}

#[napi(object)]
pub struct NativeP9Trenameat {
    pub olddirfid: u32,
    pub oldname: String,
    pub newdirfid: u32,
    pub newname: String,
}

#[napi(object)]
pub struct NativeP9Tunlinkat {
    pub dirfid: u32,
    pub name: String,
    pub flags: u32,
}

#[napi(object)]
pub struct NativeP9Tlink {
    pub dfid: u32,
    pub fid: u32,
    pub name: String,
}

#[napi(object)]
pub struct NativeP9Rreadlink {
    pub target: String,
}

#[napi(object)]
pub struct NativeP9Tgetattr {
    pub fid: u32,
    pub request_mask: BigInt,
}

#[napi(object)]
pub struct NativeP9Rgetattr {
    pub valid: BigInt,
    pub qid: NativeP9Qid,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: BigInt,
    pub rdev: BigInt,
    pub size: BigInt,
    pub blksize: BigInt,
    pub blocks: BigInt,
    pub atime: NativeP9Time,
    pub mtime: NativeP9Time,
    pub ctime: NativeP9Time,
    pub btime: NativeP9Time,
    #[napi(js_name = "gen")]
    pub r#gen: BigInt,
    pub data_version: BigInt,
}

#[napi(object)]
pub struct NativeP9Tsetattr {
    pub fid: u32,
    pub valid: u32,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: BigInt,
    pub atime: NativeP9Time,
    pub mtime: NativeP9Time,
}

#[napi(object)]
pub struct NativeP9Txattrwalk {
    pub fid: u32,
    pub newfid: u32,
    pub name: String,
}

#[napi(object)]
pub struct NativeP9Rxattrwalk {
    pub size: BigInt,
}

#[napi(object)]
pub struct NativeP9Txattrcreate {
    pub fid: u32,
    pub name: String,
    pub attr_size: BigInt,
    pub flags: u32,
}

#[napi(object)]
pub struct NativeP9Treaddir {
    pub fid: u32,
    pub offset: BigInt,
    pub count: u32,
}

#[napi(object)]
pub struct NativeP9Rreaddir {
    #[napi(ts_type = "Uint8Array")]
    pub data: Buffer,
}

#[napi(object)]
pub struct NativeP9Tfsync {
    pub fid: u32,
    pub datasync: u32,
}

#[napi(object)]
pub struct NativeP9Tlock {
    pub fid: u32,
    #[napi(js_name = "type")]
    pub type_: u8,
    pub flags: u32,
    pub start: BigInt,
    pub length: BigInt,
    pub proc_id: u32,
    pub client_id: String,
}

#[napi(object)]
pub struct NativeP9Rlock {
    pub status: u8,
}

#[napi(object)]
pub struct NativeP9Tgetlock {
    pub fid: u32,
    #[napi(js_name = "type")]
    pub type_: u8,
    pub start: BigInt,
    pub length: BigInt,
    pub proc_id: u32,
    pub client_id: String,
}

#[napi(object)]
pub struct NativeP9Rgetlock {
    #[napi(js_name = "type")]
    pub type_: u8,
    pub start: BigInt,
    pub length: BigInt,
    pub proc_id: u32,
    pub client_id: String,
}

fn qid_from_native(value: NativeP9Qid, name: &str) -> napi::Result<P9Qid> {
    Ok(P9Qid {
        type_: value.type_,
        version: value.version,
        path: bigint_to_u64(&value.path, name)?,
    })
}

fn qid_to_native(value: P9Qid) -> NativeP9Qid {
    NativeP9Qid {
        type_: value.type_,
        version: value.version,
        path: bigint(value.path),
    }
}

fn time_from_native(value: NativeP9Time, name: &str) -> napi::Result<P9Time> {
    Ok(P9Time {
        sec: bigint_to_u64(&value.sec, &format!("{name}.sec"))?,
        nsec: bigint_to_u64(&value.nsec, &format!("{name}.nsec"))?,
    })
}

fn time_to_native(value: P9Time) -> NativeP9Time {
    NativeP9Time {
        sec: bigint(value.sec),
        nsec: bigint(value.nsec),
    }
}

fn dirent_from_native(value: NativeP9Dirent, name: &str) -> napi::Result<P9Dirent> {
    Ok(P9Dirent {
        qid: qid_from_native(value.qid, &format!("{name}.qid"))?,
        offset: bigint_to_u64(&value.offset, &format!("{name}.offset"))?,
        type_: value.type_,
        name: value.name,
    })
}

fn dirent_to_native(value: P9Dirent) -> NativeP9Dirent {
    NativeP9Dirent {
        qid: qid_to_native(value.qid),
        offset: bigint(value.offset),
        type_: value.type_,
        name: value.name,
    }
}

fn tversion_from_native(value: NativeP9Tversion) -> Tversion {
    Tversion {
        msize: value.msize,
        version: value.version,
    }
}

fn tversion_to_native(value: Tversion) -> NativeP9Tversion {
    NativeP9Tversion {
        msize: value.msize,
        version: value.version,
    }
}

fn tauth_from_native(value: NativeP9Tauth) -> Tauth {
    Tauth {
        afid: value.afid,
        uname: value.uname,
        aname: value.aname,
        n_uname: value.n_uname,
    }
}

fn tauth_to_native(value: Tauth) -> NativeP9Tauth {
    NativeP9Tauth {
        afid: value.afid,
        uname: value.uname,
        aname: value.aname,
        n_uname: value.n_uname,
    }
}

fn rauth_to_native(value: Rauth) -> NativeP9Rauth {
    NativeP9Rauth {
        aqid: qid_to_native(value.aqid),
    }
}

fn tattach_from_native(value: NativeP9Tattach) -> Tattach {
    Tattach {
        fid: value.fid,
        afid: value.afid,
        uname: value.uname,
        aname: value.aname,
        n_uname: value.n_uname,
    }
}

fn tattach_to_native(value: Tattach) -> NativeP9Tattach {
    NativeP9Tattach {
        fid: value.fid,
        afid: value.afid,
        uname: value.uname,
        aname: value.aname,
        n_uname: value.n_uname,
    }
}

fn rattach_to_native(value: Rattach) -> NativeP9Rattach {
    NativeP9Rattach {
        qid: qid_to_native(value.qid),
    }
}

fn rlerror_to_native(value: Rlerror) -> NativeP9Rlerror {
    NativeP9Rlerror { ecode: value.ecode }
}

fn tflush_from_native(value: NativeP9Tflush) -> Tflush {
    Tflush {
        oldtag: value.oldtag,
    }
}

fn tflush_to_native(value: Tflush) -> NativeP9Tflush {
    NativeP9Tflush {
        oldtag: value.oldtag,
    }
}

fn twalk_from_native(value: NativeP9Twalk) -> Twalk {
    Twalk {
        fid: value.fid,
        newfid: value.newfid,
        wnames: value.wnames,
    }
}

fn twalk_to_native(value: Twalk) -> NativeP9Twalk {
    NativeP9Twalk {
        fid: value.fid,
        newfid: value.newfid,
        wnames: value.wnames,
    }
}

fn rwalk_to_native(value: Rwalk) -> NativeP9Rwalk {
    NativeP9Rwalk {
        wqids: value.wqids.into_iter().map(qid_to_native).collect(),
    }
}

fn tread_from_native(value: NativeP9Tread, name: &str) -> napi::Result<Tread> {
    Ok(Tread {
        fid: value.fid,
        offset: bigint_to_u64(&value.offset, &format!("{name}.offset"))?,
        count: value.count,
    })
}

fn tread_to_native(value: Tread) -> NativeP9Tread {
    NativeP9Tread {
        fid: value.fid,
        offset: bigint(value.offset),
        count: value.count,
    }
}

fn rread_to_native(value: Rread) -> NativeP9Rread {
    NativeP9Rread {
        data: buffer(value.data),
    }
}

fn twrite_from_native(value: NativeP9Twrite, name: &str) -> napi::Result<Twrite> {
    Ok(Twrite {
        fid: value.fid,
        offset: bigint_to_u64(&value.offset, &format!("{name}.offset"))?,
        data: value.data.as_ref().to_vec(),
    })
}

fn twrite_to_native(value: Twrite) -> NativeP9Twrite {
    NativeP9Twrite {
        fid: value.fid,
        offset: bigint(value.offset),
        data: buffer(value.data),
    }
}

fn rwrite_to_native(value: Rwrite) -> NativeP9Rwrite {
    NativeP9Rwrite { count: value.count }
}

fn fid_to_native(value: FidRequest) -> NativeP9FidRequest {
    NativeP9FidRequest { fid: value.fid }
}

fn fid_from_native(value: NativeP9FidRequest) -> FidRequest {
    FidRequest { fid: value.fid }
}

fn rstatfs_to_native(value: Rstatfs) -> NativeP9Rstatfs {
    NativeP9Rstatfs {
        type_: value.type_,
        bsize: value.bsize,
        blocks: bigint(value.blocks),
        bfree: bigint(value.bfree),
        bavail: bigint(value.bavail),
        files: bigint(value.files),
        ffree: bigint(value.ffree),
        fsid: bigint(value.fsid),
        namelen: value.namelen,
    }
}

fn tlopen_from_native(value: NativeP9Tlopen) -> Tlopen {
    Tlopen {
        fid: value.fid,
        flags: value.flags,
    }
}

fn tlopen_to_native(value: Tlopen) -> NativeP9Tlopen {
    NativeP9Tlopen {
        fid: value.fid,
        flags: value.flags,
    }
}

fn rlopen_from_native(value: NativeP9Rlopen, name: &str) -> napi::Result<Rlopen> {
    Ok(Rlopen {
        qid: qid_from_native(value.qid, &format!("{name}.qid"))?,
        iounit: value.iounit,
    })
}

fn rlopen_to_native(value: Rlopen) -> NativeP9Rlopen {
    NativeP9Rlopen {
        qid: qid_to_native(value.qid),
        iounit: value.iounit,
    }
}

fn tlcreate_from_native(value: NativeP9Tlcreate) -> Tlcreate {
    Tlcreate {
        fid: value.fid,
        name: value.name,
        flags: value.flags,
        mode: value.mode,
        gid: value.gid,
    }
}

fn tlcreate_to_native(value: Tlcreate) -> NativeP9Tlcreate {
    NativeP9Tlcreate {
        fid: value.fid,
        name: value.name,
        flags: value.flags,
        mode: value.mode,
        gid: value.gid,
    }
}

fn tsymlink_from_native(value: NativeP9Tsymlink) -> Tsymlink {
    Tsymlink {
        dfid: value.dfid,
        name: value.name,
        symtgt: value.symtgt,
        gid: value.gid,
    }
}

fn tsymlink_to_native(value: Tsymlink) -> NativeP9Tsymlink {
    NativeP9Tsymlink {
        dfid: value.dfid,
        name: value.name,
        symtgt: value.symtgt,
        gid: value.gid,
    }
}

fn qid_reply_from_native(value: NativeP9QidReply, name: &str) -> napi::Result<QidReply> {
    Ok(QidReply {
        qid: qid_from_native(value.qid, &format!("{name}.qid"))?,
    })
}

fn qid_reply_to_native(value: QidReply) -> NativeP9QidReply {
    NativeP9QidReply {
        qid: qid_to_native(value.qid),
    }
}

fn tmknod_from_native(value: NativeP9Tmknod) -> Tmknod {
    Tmknod {
        dfid: value.dfid,
        name: value.name,
        mode: value.mode,
        major: value.major,
        minor: value.minor,
        gid: value.gid,
    }
}

fn tmknod_to_native(value: Tmknod) -> NativeP9Tmknod {
    NativeP9Tmknod {
        dfid: value.dfid,
        name: value.name,
        mode: value.mode,
        major: value.major,
        minor: value.minor,
        gid: value.gid,
    }
}

fn tmkdir_from_native(value: NativeP9Tmkdir) -> Tmkdir {
    Tmkdir {
        dfid: value.dfid,
        name: value.name,
        mode: value.mode,
        gid: value.gid,
    }
}

fn tmkdir_to_native(value: Tmkdir) -> NativeP9Tmkdir {
    NativeP9Tmkdir {
        dfid: value.dfid,
        name: value.name,
        mode: value.mode,
        gid: value.gid,
    }
}

fn trename_from_native(value: NativeP9Trename) -> Trename {
    Trename {
        fid: value.fid,
        dfid: value.dfid,
        name: value.name,
    }
}

fn trename_to_native(value: Trename) -> NativeP9Trename {
    NativeP9Trename {
        fid: value.fid,
        dfid: value.dfid,
        name: value.name,
    }
}

fn trenameat_from_native(value: NativeP9Trenameat) -> Trenameat {
    Trenameat {
        olddirfid: value.olddirfid,
        oldname: value.oldname,
        newdirfid: value.newdirfid,
        newname: value.newname,
    }
}

fn trenameat_to_native(value: Trenameat) -> NativeP9Trenameat {
    NativeP9Trenameat {
        olddirfid: value.olddirfid,
        oldname: value.oldname,
        newdirfid: value.newdirfid,
        newname: value.newname,
    }
}

fn tunlinkat_from_native(value: NativeP9Tunlinkat) -> Tunlinkat {
    Tunlinkat {
        dirfid: value.dirfid,
        name: value.name,
        flags: value.flags,
    }
}

fn tunlinkat_to_native(value: Tunlinkat) -> NativeP9Tunlinkat {
    NativeP9Tunlinkat {
        dirfid: value.dirfid,
        name: value.name,
        flags: value.flags,
    }
}

fn tlink_from_native(value: NativeP9Tlink) -> Tlink {
    Tlink {
        dfid: value.dfid,
        fid: value.fid,
        name: value.name,
    }
}

fn tlink_to_native(value: Tlink) -> NativeP9Tlink {
    NativeP9Tlink {
        dfid: value.dfid,
        fid: value.fid,
        name: value.name,
    }
}

fn rreadlink_to_native(value: Rreadlink) -> NativeP9Rreadlink {
    NativeP9Rreadlink {
        target: value.target,
    }
}

fn tgetattr_from_native(value: NativeP9Tgetattr, name: &str) -> napi::Result<Tgetattr> {
    Ok(Tgetattr {
        fid: value.fid,
        request_mask: bigint_to_u64(&value.request_mask, &format!("{name}.requestMask"))?,
    })
}

fn tgetattr_to_native(value: Tgetattr) -> NativeP9Tgetattr {
    NativeP9Tgetattr {
        fid: value.fid,
        request_mask: bigint(value.request_mask),
    }
}

fn rgetattr_to_native(value: Rgetattr) -> NativeP9Rgetattr {
    NativeP9Rgetattr {
        valid: bigint(value.valid),
        qid: qid_to_native(value.qid),
        mode: value.mode,
        uid: value.uid,
        gid: value.gid,
        nlink: bigint(value.nlink),
        rdev: bigint(value.rdev),
        size: bigint(value.size),
        blksize: bigint(value.blksize),
        blocks: bigint(value.blocks),
        atime: time_to_native(value.atime),
        mtime: time_to_native(value.mtime),
        ctime: time_to_native(value.ctime),
        btime: time_to_native(value.btime),
        r#gen: bigint(value.r#gen),
        data_version: bigint(value.data_version),
    }
}

fn rgetattr_from_native(value: NativeP9Rgetattr, name: &str) -> napi::Result<Rgetattr> {
    Ok(Rgetattr {
        valid: bigint_to_u64(&value.valid, &format!("{name}.valid"))?,
        qid: qid_from_native(value.qid, &format!("{name}.qid"))?,
        mode: value.mode,
        uid: value.uid,
        gid: value.gid,
        nlink: bigint_to_u64(&value.nlink, &format!("{name}.nlink"))?,
        rdev: bigint_to_u64(&value.rdev, &format!("{name}.rdev"))?,
        size: bigint_to_u64(&value.size, &format!("{name}.size"))?,
        blksize: bigint_to_u64(&value.blksize, &format!("{name}.blksize"))?,
        blocks: bigint_to_u64(&value.blocks, &format!("{name}.blocks"))?,
        atime: time_from_native(value.atime, &format!("{name}.atime"))?,
        mtime: time_from_native(value.mtime, &format!("{name}.mtime"))?,
        ctime: time_from_native(value.ctime, &format!("{name}.ctime"))?,
        btime: time_from_native(value.btime, &format!("{name}.btime"))?,
        r#gen: bigint_to_u64(&value.r#gen, &format!("{name}.gen"))?,
        data_version: bigint_to_u64(&value.data_version, &format!("{name}.dataVersion"))?,
    })
}

fn tsetattr_from_native(value: NativeP9Tsetattr, name: &str) -> napi::Result<Tsetattr> {
    Ok(Tsetattr {
        fid: value.fid,
        valid: value.valid,
        mode: value.mode,
        uid: value.uid,
        gid: value.gid,
        size: bigint_to_u64(&value.size, &format!("{name}.size"))?,
        atime: time_from_native(value.atime, &format!("{name}.atime"))?,
        mtime: time_from_native(value.mtime, &format!("{name}.mtime"))?,
    })
}

fn tsetattr_to_native(value: Tsetattr) -> NativeP9Tsetattr {
    NativeP9Tsetattr {
        fid: value.fid,
        valid: value.valid,
        mode: value.mode,
        uid: value.uid,
        gid: value.gid,
        size: bigint(value.size),
        atime: time_to_native(value.atime),
        mtime: time_to_native(value.mtime),
    }
}

fn txattrwalk_from_native(value: NativeP9Txattrwalk) -> Txattrwalk {
    Txattrwalk {
        fid: value.fid,
        newfid: value.newfid,
        name: value.name,
    }
}

fn txattrwalk_to_native(value: Txattrwalk) -> NativeP9Txattrwalk {
    NativeP9Txattrwalk {
        fid: value.fid,
        newfid: value.newfid,
        name: value.name,
    }
}

fn rxattrwalk_to_native(value: Rxattrwalk) -> NativeP9Rxattrwalk {
    NativeP9Rxattrwalk {
        size: bigint(value.size),
    }
}

fn txattrcreate_from_native(value: NativeP9Txattrcreate, name: &str) -> napi::Result<Txattrcreate> {
    Ok(Txattrcreate {
        fid: value.fid,
        name: value.name,
        attr_size: bigint_to_u64(&value.attr_size, &format!("{name}.attrSize"))?,
        flags: value.flags,
    })
}

fn txattrcreate_to_native(value: Txattrcreate) -> NativeP9Txattrcreate {
    NativeP9Txattrcreate {
        fid: value.fid,
        name: value.name,
        attr_size: bigint(value.attr_size),
        flags: value.flags,
    }
}

fn treaddir_from_native(value: NativeP9Treaddir, name: &str) -> napi::Result<Treaddir> {
    Ok(Treaddir {
        fid: value.fid,
        offset: bigint_to_u64(&value.offset, &format!("{name}.offset"))?,
        count: value.count,
    })
}

fn treaddir_to_native(value: Treaddir) -> NativeP9Treaddir {
    NativeP9Treaddir {
        fid: value.fid,
        offset: bigint(value.offset),
        count: value.count,
    }
}

fn rreaddir_to_native(value: Rreaddir) -> NativeP9Rreaddir {
    NativeP9Rreaddir {
        data: buffer(value.data),
    }
}

fn tfsync_from_native(value: NativeP9Tfsync) -> Tfsync {
    Tfsync {
        fid: value.fid,
        datasync: value.datasync,
    }
}

fn tfsync_to_native(value: Tfsync) -> NativeP9Tfsync {
    NativeP9Tfsync {
        fid: value.fid,
        datasync: value.datasync,
    }
}

fn tlock_from_native(value: NativeP9Tlock, name: &str) -> napi::Result<mount_rs_9p::Tlock> {
    Ok(mount_rs_9p::Tlock {
        fid: value.fid,
        type_: value.type_,
        flags: value.flags,
        start: bigint_to_u64(&value.start, &format!("{name}.start"))?,
        length: bigint_to_u64(&value.length, &format!("{name}.length"))?,
        proc_id: value.proc_id,
        client_id: value.client_id,
    })
}

fn tlock_to_native(value: mount_rs_9p::Tlock) -> NativeP9Tlock {
    NativeP9Tlock {
        fid: value.fid,
        type_: value.type_,
        flags: value.flags,
        start: bigint(value.start),
        length: bigint(value.length),
        proc_id: value.proc_id,
        client_id: value.client_id,
    }
}

fn rlock_to_native(value: mount_rs_9p::Rlock) -> NativeP9Rlock {
    NativeP9Rlock {
        status: value.status,
    }
}

fn tgetlock_from_native(
    value: NativeP9Tgetlock,
    name: &str,
) -> napi::Result<mount_rs_9p::Tgetlock> {
    Ok(mount_rs_9p::Tgetlock {
        fid: value.fid,
        type_: value.type_,
        start: bigint_to_u64(&value.start, &format!("{name}.start"))?,
        length: bigint_to_u64(&value.length, &format!("{name}.length"))?,
        proc_id: value.proc_id,
        client_id: value.client_id,
    })
}

fn tgetlock_to_native(value: mount_rs_9p::Tgetlock) -> NativeP9Tgetlock {
    NativeP9Tgetlock {
        fid: value.fid,
        type_: value.type_,
        start: bigint(value.start),
        length: bigint(value.length),
        proc_id: value.proc_id,
        client_id: value.client_id,
    }
}

fn rgetlock_to_native(value: mount_rs_9p::Rgetlock) -> NativeP9Rgetlock {
    NativeP9Rgetlock {
        type_: value.type_,
        start: bigint(value.start),
        length: bigint(value.length),
        proc_id: value.proc_id,
        client_id: value.client_id,
    }
}

fn rgetlock_from_native(
    value: NativeP9Rgetlock,
    name: &str,
) -> napi::Result<mount_rs_9p::Rgetlock> {
    Ok(mount_rs_9p::Rgetlock {
        type_: value.type_,
        start: bigint_to_u64(&value.start, &format!("{name}.start"))?,
        length: bigint_to_u64(&value.length, &format!("{name}.length"))?,
        proc_id: value.proc_id,
        client_id: value.client_id,
    })
}

fn header_to_native(value: P9Header) -> NativeP9Header {
    NativeP9Header {
        size: value.size,
        type_: value.type_,
        tag: value.tag,
    }
}

fn header_from_native(value: NativeP9Header) -> P9Header {
    P9Header {
        size: value.size,
        type_: value.type_,
        tag: value.tag,
    }
}

/// A JS-owned reader that keeps the input bytes alive while delegating every
/// primitive and typed operation to `mount-rs-9p`.
#[napi]
pub struct NativeP9Reader {
    bytes: Vec<u8>,
    offset: usize,
}

impl NativeP9Reader {
    fn with_reader<T>(
        &mut self,
        operation: impl FnOnce(&mut P9Reader<'_>) -> Result<T, P9Error>,
    ) -> napi::Result<T> {
        let mut reader = P9Reader::with_offset(&self.bytes, self.offset).map_err(wire_error)?;
        let result = operation(&mut reader);
        self.offset = reader.offset();
        wire_result(result)
    }

    fn with_reader_value<T>(
        &self,
        operation: impl FnOnce(&mut P9Reader<'_>) -> Result<T, P9Error>,
    ) -> napi::Result<T> {
        let mut reader = P9Reader::with_offset(&self.bytes, self.offset).map_err(wire_error)?;
        wire_result(operation(&mut reader))
    }
}

#[napi]
impl NativeP9Reader {
    #[napi(constructor)]
    pub fn new(
        #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
        offset: Option<u32>,
    ) -> napi::Result<Self> {
        let bytes = bytes.as_ref().to_vec();
        let offset = offset.unwrap_or(0) as usize;
        if offset > bytes.len() {
            return Err(wire_error(P9Error::at(
                "reader offset is beyond the buffer",
                offset,
            )));
        }
        Ok(Self { bytes, offset })
    }

    #[napi(getter)]
    pub fn offset(&self) -> napi::Result<u32> {
        u32_len(self.offset, "reader offset")
    }

    #[napi(getter)]
    pub fn bytes(&self) -> Buffer {
        buffer(self.bytes.clone())
    }

    #[napi(getter)]
    pub fn remaining(&self) -> napi::Result<u32> {
        u32_len(
            self.bytes.len().saturating_sub(self.offset),
            "reader remaining",
        )
    }

    #[napi(getter)]
    pub fn at_end(&self) -> bool {
        self.offset == self.bytes.len()
    }

    #[napi]
    pub fn u8(&mut self, what: Option<String>) -> napi::Result<u8> {
        self.with_reader(|reader| reader.u8(what.as_deref().unwrap_or("uint8")))
    }

    #[napi]
    pub fn u16(&mut self, what: Option<String>) -> napi::Result<u16> {
        self.with_reader(|reader| reader.u16(what.as_deref().unwrap_or("uint16")))
    }

    #[napi]
    pub fn u32(&mut self, what: Option<String>) -> napi::Result<u32> {
        self.with_reader(|reader| reader.u32(what.as_deref().unwrap_or("uint32")))
    }

    #[napi]
    pub fn u64(&mut self, what: Option<String>) -> napi::Result<BigInt> {
        self.with_reader(|reader| reader.u64(what.as_deref().unwrap_or("uint64")))
            .map(bigint)
    }

    #[napi]
    pub fn string(&mut self, max: Option<u32>, what: Option<String>) -> napi::Result<String> {
        let max = max.unwrap_or(P9_MAX_STRING as u32) as usize;
        self.with_reader(|reader| reader.string_max(max, what.as_deref().unwrap_or("string")))
    }

    #[napi]
    pub fn qid(&mut self, what: Option<String>) -> napi::Result<NativeP9Qid> {
        self.with_reader(|reader| reader.qid(what.as_deref().unwrap_or("qid")))
            .map(qid_to_native)
    }

    #[napi]
    pub fn blob(&mut self, max: Option<u32>, what: Option<String>) -> napi::Result<Buffer> {
        let max = max.unwrap_or(P9_MAX_ITEM as u32) as usize;
        self.with_reader(|reader| reader.blob_max(max, what.as_deref().unwrap_or("data")))
            .map(buffer)
    }

    #[napi]
    pub fn raw(&mut self, count: u32, what: Option<String>) -> napi::Result<Buffer> {
        self.with_reader(|reader| reader.raw(count as usize, what.as_deref().unwrap_or("bytes")))
            .map(buffer)
    }

    #[napi]
    pub fn rest(&mut self) -> napi::Result<Buffer> {
        self.with_reader(|reader| reader.rest()).map(buffer)
    }

    #[napi]
    pub fn end(&self, what: Option<String>) -> napi::Result<()> {
        self.with_reader_value(|reader| reader.end(what.as_deref().unwrap_or("message")))
    }

    #[napi(js_name = "readHeader")]
    pub fn read_header(&mut self) -> napi::Result<NativeP9Header> {
        self.with_reader(read_header).map(header_to_native)
    }
}

#[napi]
impl NativeP9Reader {
    #[napi(js_name = "readTime")]
    pub fn read_time(&mut self, what: Option<String>) -> napi::Result<NativeP9Time> {
        let what = what.unwrap_or_else(|| "time".to_owned());
        self.with_reader(|reader| {
            Ok(P9Time {
                sec: reader.u64(&format!("{what}_sec"))?,
                nsec: reader.u64(&format!("{what}_nsec"))?,
            })
        })
        .map(time_to_native)
    }

    #[napi(js_name = "readTversion")]
    pub fn read_tversion(&mut self) -> napi::Result<NativeP9Tversion> {
        self.with_reader(read_tversion).map(tversion_to_native)
    }

    #[napi(js_name = "readRversion")]
    pub fn read_rversion(&mut self) -> napi::Result<NativeP9Tversion> {
        self.with_reader(read_rversion).map(tversion_to_native)
    }

    #[napi(js_name = "readTauth")]
    pub fn read_tauth(&mut self) -> napi::Result<NativeP9Tauth> {
        self.with_reader(read_tauth).map(tauth_to_native)
    }

    #[napi(js_name = "readRauth")]
    pub fn read_rauth(&mut self) -> napi::Result<NativeP9Rauth> {
        self.with_reader(read_rauth).map(rauth_to_native)
    }

    #[napi(js_name = "readTattach")]
    pub fn read_tattach(&mut self) -> napi::Result<NativeP9Tattach> {
        self.with_reader(read_tattach).map(tattach_to_native)
    }

    #[napi(js_name = "readRattach")]
    pub fn read_rattach(&mut self) -> napi::Result<NativeP9Rattach> {
        self.with_reader(read_rattach).map(rattach_to_native)
    }

    #[napi(js_name = "readRlerror")]
    pub fn read_rlerror(&mut self) -> napi::Result<NativeP9Rlerror> {
        self.with_reader(read_rlerror).map(rlerror_to_native)
    }

    #[napi(js_name = "readTflush")]
    pub fn read_tflush(&mut self) -> napi::Result<NativeP9Tflush> {
        self.with_reader(read_tflush).map(tflush_to_native)
    }

    #[napi(js_name = "readTwalk")]
    pub fn read_twalk(&mut self) -> napi::Result<NativeP9Twalk> {
        self.with_reader(read_twalk).map(twalk_to_native)
    }

    #[napi(js_name = "readRwalk")]
    pub fn read_rwalk(&mut self) -> napi::Result<NativeP9Rwalk> {
        self.with_reader(read_rwalk).map(rwalk_to_native)
    }

    #[napi(js_name = "readTread")]
    pub fn read_tread(&mut self) -> napi::Result<NativeP9Tread> {
        self.with_reader(read_tread).map(tread_to_native)
    }

    #[napi(js_name = "readRread")]
    pub fn read_rread(&mut self, max: Option<u32>) -> napi::Result<NativeP9Rread> {
        let max = max.unwrap_or(P9_MAX_ITEM as u32) as usize;
        self.with_reader(|reader| {
            Ok(Rread {
                data: reader.blob_max(max, "read data")?,
            })
        })
        .map(rread_to_native)
    }

    #[napi(js_name = "readTwrite")]
    pub fn read_twrite(&mut self, max: Option<u32>) -> napi::Result<NativeP9Twrite> {
        let max = max.unwrap_or(P9_MAX_ITEM as u32) as usize;
        self.with_reader(|reader| {
            Ok(Twrite {
                fid: reader.u32("fid")?,
                offset: reader.u64("offset")?,
                data: reader.blob_max(max, "write data")?,
            })
        })
        .map(twrite_to_native)
    }

    #[napi(js_name = "readRwrite")]
    pub fn read_rwrite(&mut self) -> napi::Result<NativeP9Rwrite> {
        self.with_reader(read_rwrite).map(rwrite_to_native)
    }

    #[napi(js_name = "readFidRequest")]
    pub fn read_fid_request(&mut self) -> napi::Result<NativeP9FidRequest> {
        self.with_reader(read_fid_request).map(fid_to_native)
    }

    #[napi(js_name = "readRstatfs")]
    pub fn read_rstatfs(&mut self) -> napi::Result<NativeP9Rstatfs> {
        self.with_reader(read_rstatfs).map(rstatfs_to_native)
    }

    #[napi(js_name = "readTlopen")]
    pub fn read_tlopen(&mut self) -> napi::Result<NativeP9Tlopen> {
        self.with_reader(read_tlopen).map(tlopen_to_native)
    }

    #[napi(js_name = "readRlopen")]
    pub fn read_rlopen(&mut self) -> napi::Result<NativeP9Rlopen> {
        self.with_reader(read_rlopen).map(rlopen_to_native)
    }

    #[napi(js_name = "readTlcreate")]
    pub fn read_tlcreate(&mut self) -> napi::Result<NativeP9Tlcreate> {
        self.with_reader(read_tlcreate).map(tlcreate_to_native)
    }

    #[napi(js_name = "readTsymlink")]
    pub fn read_tsymlink(&mut self) -> napi::Result<NativeP9Tsymlink> {
        self.with_reader(read_tsymlink).map(tsymlink_to_native)
    }

    #[napi(js_name = "readQidReply")]
    pub fn read_qid_reply(&mut self) -> napi::Result<NativeP9QidReply> {
        self.with_reader(read_qid_reply).map(qid_reply_to_native)
    }

    #[napi(js_name = "readTmknod")]
    pub fn read_tmknod(&mut self) -> napi::Result<NativeP9Tmknod> {
        self.with_reader(read_tmknod).map(tmknod_to_native)
    }

    #[napi(js_name = "readTmkdir")]
    pub fn read_tmkdir(&mut self) -> napi::Result<NativeP9Tmkdir> {
        self.with_reader(read_tmkdir).map(tmkdir_to_native)
    }

    #[napi(js_name = "readTrename")]
    pub fn read_trename(&mut self) -> napi::Result<NativeP9Trename> {
        self.with_reader(read_trename).map(trename_to_native)
    }

    #[napi(js_name = "readTrenameat")]
    pub fn read_trenameat(&mut self) -> napi::Result<NativeP9Trenameat> {
        self.with_reader(read_trenameat).map(trenameat_to_native)
    }

    #[napi(js_name = "readTunlinkat")]
    pub fn read_tunlinkat(&mut self) -> napi::Result<NativeP9Tunlinkat> {
        self.with_reader(read_tunlinkat).map(tunlinkat_to_native)
    }

    #[napi(js_name = "readTlink")]
    pub fn read_tlink(&mut self) -> napi::Result<NativeP9Tlink> {
        self.with_reader(read_tlink).map(tlink_to_native)
    }

    #[napi(js_name = "readRreadlink")]
    pub fn read_rreadlink(&mut self) -> napi::Result<NativeP9Rreadlink> {
        self.with_reader(read_rreadlink).map(rreadlink_to_native)
    }

    #[napi(js_name = "readTgetattr")]
    pub fn read_tgetattr(&mut self) -> napi::Result<NativeP9Tgetattr> {
        self.with_reader(read_tgetattr).map(tgetattr_to_native)
    }

    #[napi(js_name = "readRgetattr")]
    pub fn read_rgetattr(&mut self) -> napi::Result<NativeP9Rgetattr> {
        self.with_reader(read_rgetattr).map(rgetattr_to_native)
    }

    #[napi(js_name = "readTsetattr")]
    pub fn read_tsetattr(&mut self) -> napi::Result<NativeP9Tsetattr> {
        self.with_reader(read_tsetattr).map(tsetattr_to_native)
    }

    #[napi(js_name = "readTxattrwalk")]
    pub fn read_txattrwalk(&mut self) -> napi::Result<NativeP9Txattrwalk> {
        self.with_reader(read_txattrwalk).map(txattrwalk_to_native)
    }

    #[napi(js_name = "readRxattrwalk")]
    pub fn read_rxattrwalk(&mut self) -> napi::Result<NativeP9Rxattrwalk> {
        self.with_reader(read_rxattrwalk).map(rxattrwalk_to_native)
    }

    #[napi(js_name = "readTxattrcreate")]
    pub fn read_txattrcreate(&mut self) -> napi::Result<NativeP9Txattrcreate> {
        self.with_reader(read_txattrcreate)
            .map(txattrcreate_to_native)
    }

    #[napi(js_name = "readTreaddir")]
    pub fn read_treaddir(&mut self) -> napi::Result<NativeP9Treaddir> {
        self.with_reader(read_treaddir).map(treaddir_to_native)
    }

    #[napi(js_name = "readRreaddir")]
    pub fn read_rreaddir(&mut self, max: Option<u32>) -> napi::Result<NativeP9Rreaddir> {
        let max = max.unwrap_or(P9_MAX_ITEM as u32) as usize;
        self.with_reader(|reader| {
            Ok(Rreaddir {
                data: reader.blob_max(max, "readdir data")?,
            })
        })
        .map(rreaddir_to_native)
    }

    #[napi(js_name = "readDirent")]
    pub fn read_dirent(&mut self) -> napi::Result<NativeP9Dirent> {
        self.with_reader(read_dirent).map(dirent_to_native)
    }

    #[napi(js_name = "readDirents")]
    pub fn read_dirents(&mut self) -> napi::Result<Vec<NativeP9Dirent>> {
        self.with_reader(|reader| {
            let bytes = reader.rest()?;
            read_dirents(&bytes)
        })
        .map(|values| values.into_iter().map(dirent_to_native).collect())
    }

    #[napi(js_name = "readTfsync")]
    pub fn read_tfsync(&mut self) -> napi::Result<NativeP9Tfsync> {
        self.with_reader(read_tfsync).map(tfsync_to_native)
    }

    #[napi(js_name = "readTlock")]
    pub fn read_tlock(&mut self) -> napi::Result<NativeP9Tlock> {
        self.with_reader(read_tlock).map(tlock_to_native)
    }

    #[napi(js_name = "readRlock")]
    pub fn read_rlock(&mut self) -> napi::Result<NativeP9Rlock> {
        self.with_reader(read_rlock).map(rlock_to_native)
    }

    #[napi(js_name = "readTgetlock")]
    pub fn read_tgetlock(&mut self) -> napi::Result<NativeP9Tgetlock> {
        self.with_reader(read_tgetlock).map(tgetlock_to_native)
    }

    #[napi(js_name = "readRgetlock")]
    pub fn read_rgetlock(&mut self) -> napi::Result<NativeP9Rgetlock> {
        self.with_reader(read_rgetlock).map(rgetlock_to_native)
    }
}

/// A Rust-backed growable writer.  The postlude supplies the upstream
/// chainable return convention; the bytes and all scalar writes stay native.
#[napi]
pub struct NativeP9Writer {
    inner: P9Writer,
}

#[napi]
impl NativeP9Writer {
    #[napi(constructor)]
    pub fn new(capacity: Option<u32>) -> Self {
        Self {
            inner: P9Writer::new(capacity.unwrap_or(256) as usize),
        }
    }

    #[napi(getter)]
    pub fn length(&self) -> napi::Result<u32> {
        u32_len(self.inner.len(), "writer length")
    }

    #[napi]
    pub fn u8(&mut self, value: u8) {
        self.inner.u8(value);
    }

    #[napi]
    pub fn u16(&mut self, value: u16) {
        self.inner.u16(value);
    }

    #[napi]
    pub fn u32(&mut self, value: u32) {
        self.inner.u32(value);
    }

    #[napi]
    pub fn u64(&mut self, value: BigInt) -> napi::Result<()> {
        self.inner.u64(bigint_to_u64(&value, "uint64")?);
        Ok(())
    }

    #[napi]
    pub fn string(&mut self, value: String) -> napi::Result<()> {
        wire_result(self.inner.string(&value))
    }

    #[napi]
    pub fn qid(&mut self, value: NativeP9Qid) -> napi::Result<()> {
        self.inner.qid(qid_from_native(value, "qid")?);
        Ok(())
    }

    #[napi]
    pub fn blob(&mut self, #[napi(ts_arg_type = "Uint8Array")] value: Buffer) {
        self.inner.blob(value.as_ref());
    }

    #[napi]
    pub fn raw(&mut self, #[napi(ts_arg_type = "Uint8Array")] value: Buffer) {
        self.inner.raw(value.as_ref());
    }

    #[napi(js_name = "patchU32")]
    pub fn patch_u32(&mut self, at: u32, value: u32) -> napi::Result<()> {
        wire_result(self.inner.patch_u32(at as usize, value))
    }

    #[napi]
    pub fn bytes(&self) -> Buffer {
        buffer(self.inner.bytes())
    }

    #[napi(js_name = "writeHeader")]
    pub fn write_header(&mut self, header: NativeP9Header) {
        write_header(&mut self.inner, header_from_native(header));
    }
}

#[napi]
impl NativeP9Writer {
    #[napi(js_name = "writeTime")]
    pub fn write_time(&mut self, value: NativeP9Time) -> napi::Result<()> {
        let value = time_from_native(value, "time")?;
        self.inner.u64(value.sec);
        self.inner.u64(value.nsec);
        Ok(())
    }

    #[napi(js_name = "writeTversion")]
    pub fn write_tversion(&mut self, value: NativeP9Tversion) -> napi::Result<()> {
        wire_result(write_tversion(
            &mut self.inner,
            &tversion_from_native(value),
        ))
    }

    #[napi(js_name = "writeRversion")]
    pub fn write_rversion(&mut self, value: NativeP9Tversion) -> napi::Result<()> {
        wire_result(write_rversion(
            &mut self.inner,
            &tversion_from_native(value),
        ))
    }

    #[napi(js_name = "writeTauth")]
    pub fn write_tauth(&mut self, value: NativeP9Tauth) -> napi::Result<()> {
        wire_result(write_tauth(&mut self.inner, &tauth_from_native(value)))
    }

    #[napi(js_name = "writeRauth")]
    pub fn write_rauth(&mut self, value: NativeP9Rauth) -> napi::Result<()> {
        write_rauth(
            &mut self.inner,
            &Rauth {
                aqid: qid_from_native(value.aqid, "aqid")?,
            },
        );
        Ok(())
    }

    #[napi(js_name = "writeTattach")]
    pub fn write_tattach(&mut self, value: NativeP9Tattach) -> napi::Result<()> {
        wire_result(write_tattach(&mut self.inner, &tattach_from_native(value)))
    }

    #[napi(js_name = "writeRattach")]
    pub fn write_rattach(&mut self, value: NativeP9Rattach) -> napi::Result<()> {
        write_rattach(
            &mut self.inner,
            &Rattach {
                qid: qid_from_native(value.qid, "qid")?,
            },
        );
        Ok(())
    }

    #[napi(js_name = "writeRlerror")]
    pub fn write_rlerror(&mut self, value: NativeP9Rlerror) -> napi::Result<()> {
        write_rlerror(&mut self.inner, mount_rs_9p::Rlerror { ecode: value.ecode });
        Ok(())
    }

    #[napi(js_name = "writeTflush")]
    pub fn write_tflush(&mut self, value: NativeP9Tflush) -> napi::Result<()> {
        write_tflush(&mut self.inner, tflush_from_native(value));
        Ok(())
    }

    #[napi(js_name = "writeTwalk")]
    pub fn write_twalk(&mut self, value: NativeP9Twalk) -> napi::Result<()> {
        wire_result(write_twalk(&mut self.inner, &twalk_from_native(value)))
    }

    #[napi(js_name = "writeRwalk")]
    pub fn write_rwalk(&mut self, value: NativeP9Rwalk) -> napi::Result<()> {
        let wqids = value
            .wqids
            .into_iter()
            .enumerate()
            .map(|(index, qid)| qid_from_native(qid, &format!("wqids[{index}]")))
            .collect::<napi::Result<Vec<_>>>()?;
        wire_result(write_rwalk(&mut self.inner, &Rwalk { wqids }))
    }

    #[napi(js_name = "writeTread")]
    pub fn write_tread(&mut self, value: NativeP9Tread) -> napi::Result<()> {
        write_tread(&mut self.inner, tread_from_native(value, "tread")?);
        Ok(())
    }

    #[napi(js_name = "writeRread")]
    pub fn write_rread(&mut self, value: NativeP9Rread) -> napi::Result<()> {
        write_rread(
            &mut self.inner,
            &Rread {
                data: value.data.as_ref().to_vec(),
            },
        );
        Ok(())
    }

    #[napi(js_name = "writeTwrite")]
    pub fn write_twrite(&mut self, value: NativeP9Twrite) -> napi::Result<()> {
        write_twrite(&mut self.inner, &twrite_from_native(value, "twrite")?);
        Ok(())
    }

    #[napi(js_name = "writeRwrite")]
    pub fn write_rwrite(&mut self, value: NativeP9Rwrite) -> napi::Result<()> {
        write_rwrite(&mut self.inner, Rwrite { count: value.count });
        Ok(())
    }

    #[napi(js_name = "writeFidRequest")]
    pub fn write_fid_request(&mut self, value: NativeP9FidRequest) -> napi::Result<()> {
        write_fid_request(&mut self.inner, fid_from_native(value));
        Ok(())
    }

    #[napi(js_name = "writeRstatfs")]
    pub fn write_rstatfs(&mut self, value: NativeP9Rstatfs) -> napi::Result<()> {
        write_rstatfs(
            &mut self.inner,
            Rstatfs {
                type_: value.type_,
                bsize: value.bsize,
                blocks: bigint_to_u64(&value.blocks, "blocks")?,
                bfree: bigint_to_u64(&value.bfree, "bfree")?,
                bavail: bigint_to_u64(&value.bavail, "bavail")?,
                files: bigint_to_u64(&value.files, "files")?,
                ffree: bigint_to_u64(&value.ffree, "ffree")?,
                fsid: bigint_to_u64(&value.fsid, "fsid")?,
                namelen: value.namelen,
            },
        );
        Ok(())
    }

    #[napi(js_name = "writeTlopen")]
    pub fn write_tlopen(&mut self, value: NativeP9Tlopen) -> napi::Result<()> {
        write_tlopen(&mut self.inner, tlopen_from_native(value));
        Ok(())
    }

    #[napi(js_name = "writeRlopen")]
    pub fn write_rlopen(&mut self, value: NativeP9Rlopen) -> napi::Result<()> {
        write_rlopen(&mut self.inner, rlopen_from_native(value, "rlopen")?);
        Ok(())
    }

    #[napi(js_name = "writeTlcreate")]
    pub fn write_tlcreate(&mut self, value: NativeP9Tlcreate) -> napi::Result<()> {
        wire_result(write_tlcreate(
            &mut self.inner,
            &tlcreate_from_native(value),
        ))
    }

    #[napi(js_name = "writeTsymlink")]
    pub fn write_tsymlink(&mut self, value: NativeP9Tsymlink) -> napi::Result<()> {
        wire_result(write_tsymlink(
            &mut self.inner,
            &tsymlink_from_native(value),
        ))
    }

    #[napi(js_name = "writeQidReply")]
    pub fn write_qid_reply(&mut self, value: NativeP9QidReply) -> napi::Result<()> {
        write_qid_reply(&mut self.inner, qid_reply_from_native(value, "qidReply")?);
        Ok(())
    }

    #[napi(js_name = "writeTmknod")]
    pub fn write_tmknod(&mut self, value: NativeP9Tmknod) -> napi::Result<()> {
        wire_result(write_tmknod(&mut self.inner, &tmknod_from_native(value)))
    }

    #[napi(js_name = "writeTmkdir")]
    pub fn write_tmkdir(&mut self, value: NativeP9Tmkdir) -> napi::Result<()> {
        wire_result(write_tmkdir(&mut self.inner, &tmkdir_from_native(value)))
    }

    #[napi(js_name = "writeTrename")]
    pub fn write_trename(&mut self, value: NativeP9Trename) -> napi::Result<()> {
        wire_result(write_trename(&mut self.inner, &trename_from_native(value)))
    }

    #[napi(js_name = "writeTrenameat")]
    pub fn write_trenameat(&mut self, value: NativeP9Trenameat) -> napi::Result<()> {
        wire_result(write_trenameat(
            &mut self.inner,
            &trenameat_from_native(value),
        ))
    }

    #[napi(js_name = "writeTunlinkat")]
    pub fn write_tunlinkat(&mut self, value: NativeP9Tunlinkat) -> napi::Result<()> {
        wire_result(write_tunlinkat(
            &mut self.inner,
            &tunlinkat_from_native(value),
        ))
    }

    #[napi(js_name = "writeTlink")]
    pub fn write_tlink(&mut self, value: NativeP9Tlink) -> napi::Result<()> {
        wire_result(write_tlink(&mut self.inner, &tlink_from_native(value)))
    }

    #[napi(js_name = "writeRreadlink")]
    pub fn write_rreadlink(&mut self, value: NativeP9Rreadlink) -> napi::Result<()> {
        wire_result(write_rreadlink(
            &mut self.inner,
            &Rreadlink {
                target: value.target,
            },
        ))
    }

    #[napi(js_name = "writeTgetattr")]
    pub fn write_tgetattr(&mut self, value: NativeP9Tgetattr) -> napi::Result<()> {
        write_tgetattr(&mut self.inner, tgetattr_from_native(value, "tgetattr")?);
        Ok(())
    }

    #[napi(js_name = "writeRgetattr")]
    pub fn write_rgetattr(&mut self, value: NativeP9Rgetattr) -> napi::Result<()> {
        write_rgetattr(&mut self.inner, rgetattr_from_native(value, "rgetattr")?);
        Ok(())
    }

    #[napi(js_name = "writeTsetattr")]
    pub fn write_tsetattr(&mut self, value: NativeP9Tsetattr) -> napi::Result<()> {
        write_tsetattr(&mut self.inner, tsetattr_from_native(value, "tsetattr")?);
        Ok(())
    }

    #[napi(js_name = "writeTxattrwalk")]
    pub fn write_txattrwalk(&mut self, value: NativeP9Txattrwalk) -> napi::Result<()> {
        wire_result(write_txattrwalk(
            &mut self.inner,
            &txattrwalk_from_native(value),
        ))
    }

    #[napi(js_name = "writeRxattrwalk")]
    pub fn write_rxattrwalk(&mut self, value: NativeP9Rxattrwalk) -> napi::Result<()> {
        write_rxattrwalk(
            &mut self.inner,
            Rxattrwalk {
                size: bigint_to_u64(&value.size, "size")?,
            },
        );
        Ok(())
    }

    #[napi(js_name = "writeTxattrcreate")]
    pub fn write_txattrcreate(&mut self, value: NativeP9Txattrcreate) -> napi::Result<()> {
        wire_result(write_txattrcreate(
            &mut self.inner,
            &txattrcreate_from_native(value, "txattrcreate")?,
        ))
    }

    #[napi(js_name = "writeTreaddir")]
    pub fn write_treaddir(&mut self, value: NativeP9Treaddir) -> napi::Result<()> {
        write_treaddir(&mut self.inner, treaddir_from_native(value, "treaddir")?);
        Ok(())
    }

    #[napi(js_name = "writeRreaddir")]
    pub fn write_rreaddir(&mut self, value: NativeP9Rreaddir) -> napi::Result<()> {
        write_rreaddir(
            &mut self.inner,
            &Rreaddir {
                data: value.data.as_ref().to_vec(),
            },
        );
        Ok(())
    }

    #[napi(js_name = "writeDirent")]
    pub fn write_dirent(&mut self, value: NativeP9Dirent) -> napi::Result<()> {
        wire_result(write_dirent(
            &mut self.inner,
            &dirent_from_native(value, "dirent")?,
        ))
    }

    #[napi(js_name = "writeTfsync")]
    pub fn write_tfsync(&mut self, value: NativeP9Tfsync) -> napi::Result<()> {
        write_tfsync(&mut self.inner, tfsync_from_native(value));
        Ok(())
    }

    #[napi(js_name = "writeTlock")]
    pub fn write_tlock(&mut self, value: NativeP9Tlock) -> napi::Result<()> {
        wire_result(write_tlock(
            &mut self.inner,
            &tlock_from_native(value, "tlock")?,
        ))
    }

    #[napi(js_name = "writeRlock")]
    pub fn write_rlock(&mut self, value: NativeP9Rlock) -> napi::Result<()> {
        write_rlock(
            &mut self.inner,
            Rlock {
                status: value.status,
            },
        );
        Ok(())
    }

    #[napi(js_name = "writeTgetlock")]
    pub fn write_tgetlock(&mut self, value: NativeP9Tgetlock) -> napi::Result<()> {
        wire_result(write_tgetlock(
            &mut self.inner,
            &tgetlock_from_native(value, "tgetlock")?,
        ))
    }

    #[napi(js_name = "writeRgetlock")]
    pub fn write_rgetlock(&mut self, value: NativeP9Rgetlock) -> napi::Result<()> {
        wire_result(write_rgetlock(
            &mut self.inner,
            &rgetlock_from_native(value, "rgetlock")?,
        ))
    }
}

#[napi]
pub struct NativeP9FrameAssembler {
    inner: P9FrameAssembler,
}

#[napi]
impl NativeP9FrameAssembler {
    #[napi(constructor)]
    pub fn new(limit: Option<u32>) -> napi::Result<Self> {
        let limit = limit.unwrap_or(1024 * 1024) as usize;
        Ok(Self {
            inner: wire_result(P9FrameAssembler::new(limit))?,
        })
    }

    #[napi(getter)]
    pub fn limit(&self) -> napi::Result<u32> {
        u32_len(self.inner.limit(), "frame limit")
    }

    #[napi(setter)]
    pub fn set_limit(&mut self, limit: u32) -> napi::Result<()> {
        wire_result(self.inner.set_limit(limit as usize))
    }

    #[napi(getter)]
    pub fn pending(&self) -> napi::Result<u32> {
        u32_len(self.inner.pending(), "pending frame bytes")
    }

    #[napi(getter)]
    pub fn failed(&self) -> bool {
        self.inner.failed()
    }

    #[napi]
    pub fn reset(&mut self) {
        self.inner.reset();
    }

    #[napi]
    pub fn push(
        &mut self,
        #[napi(ts_arg_type = "Uint8Array")] chunk: Buffer,
    ) -> napi::Result<Vec<Buffer>> {
        wire_result(self.inner.push(chunk.as_ref()))
            .map(|frames| frames.into_iter().map(buffer).collect())
    }
}

#[napi]
pub fn native_p9_encode_message(
    type_: u8,
    tag: u16,
    #[napi(ts_arg_type = "Uint8Array")] body: Buffer,
    capacity: Option<u32>,
) -> napi::Result<Buffer> {
    let body = body.as_ref().to_vec();
    wire_result(encode_message(
        type_,
        tag,
        capacity.unwrap_or(128) as usize,
        |writer| {
            writer.raw(&body);
            Ok(())
        },
    ))
    .map(buffer)
}

#[napi]
pub fn native_p9_decode_message(
    #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
) -> napi::Result<NativeP9Message> {
    let bytes = bytes.as_ref();
    let (header, mut body) = wire_result(decode_message(bytes))?;
    Ok(NativeP9Message {
        header: header_to_native(header),
        body: buffer(body.rest().map_err(wire_error)?),
    })
}

#[napi]
pub fn native_p9_string_byte_length(value: String) -> napi::Result<u32> {
    u32_len(value.len(), "UTF-8 string byte length")
}

#[napi]
pub fn native_p9_dirent_size(value: String) -> napi::Result<u32> {
    u32_len(dirent_size(&value), "dirent size")
}

#[napi]
pub fn native_p9_read_dirents(
    #[napi(ts_arg_type = "Uint8Array")] bytes: Buffer,
) -> napi::Result<Vec<NativeP9Dirent>> {
    wire_result(read_dirents(bytes.as_ref()))
        .map(|values| values.into_iter().map(dirent_to_native).collect())
}

#[napi]
pub struct NativeP9DirentPacker {
    inner: P9DirentPacker,
}

#[napi]
impl NativeP9DirentPacker {
    #[napi(constructor)]
    pub fn new(max_size: u32) -> Self {
        Self {
            inner: P9DirentPacker::new(max_size as usize),
        }
    }

    #[napi(getter)]
    pub fn max_size(&self) -> napi::Result<u32> {
        u32_len(
            self.inner.size().saturating_add(self.inner.remaining()),
            "dirent packer max size",
        )
    }

    #[napi(getter)]
    pub fn size(&self) -> napi::Result<u32> {
        u32_len(self.inner.size(), "dirent packer size")
    }

    #[napi(getter)]
    pub fn count(&self) -> napi::Result<u32> {
        u32_len(self.inner.count(), "dirent packer count")
    }

    #[napi(getter)]
    pub fn remaining(&self) -> napi::Result<u32> {
        u32_len(self.inner.remaining(), "dirent packer remaining")
    }

    #[napi]
    pub fn add(&mut self, value: NativeP9Dirent) -> napi::Result<bool> {
        let value = dirent_from_native(value, "dirent")?;
        wire_result(self.inner.add(&value))
    }

    #[napi]
    pub fn bytes(&self) -> Buffer {
        buffer(self.inner.bytes())
    }
}
