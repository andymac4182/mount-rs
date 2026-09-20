//! Symmetric 9P2000.L message codecs and stream framing.

use crate::constants::*;
use crate::wire::{P9Error, P9Qid, P9Reader, P9Writer, string_byte_length};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct P9Header {
    pub size: u32,
    pub type_: u8,
    pub tag: u16,
}

pub fn read_header(reader: &mut P9Reader<'_>) -> Result<P9Header, P9Error> {
    Ok(P9Header {
        size: reader.u32("size")?,
        type_: reader.u8("type")?,
        tag: reader.u16("tag")?,
    })
}

pub fn write_header(writer: &mut P9Writer, header: P9Header) {
    writer.u32(header.size);
    writer.u8(header.type_);
    writer.u16(header.tag);
}

pub fn encode_message<F>(type_: u8, tag: u16, capacity: usize, write: F) -> Result<Vec<u8>, P9Error>
where
    F: FnOnce(&mut P9Writer) -> Result<(), P9Error>,
{
    let mut writer = P9Writer::new(capacity);
    write_header(
        &mut writer,
        P9Header {
            size: 0,
            type_,
            tag,
        },
    );
    write(&mut writer)?;
    let size = u32::try_from(writer.len()).map_err(|_| {
        P9Error::new(format!(
            "{} is too large for the size field",
            message_name(type_)
        ))
    })?;
    writer.patch_u32(0, size)?;
    Ok(writer.into_bytes())
}

pub fn decode_message(bytes: &[u8]) -> Result<(P9Header, P9Reader<'_>), P9Error> {
    if bytes.len() < P9_HDRSZ {
        return Err(P9Error::new(format!(
            "truncated 9P header: need {P9_HDRSZ} bytes, {} left",
            bytes.len()
        )));
    }
    let mut reader = P9Reader::new(bytes);
    let header = read_header(&mut reader)?;
    if header.size as usize != bytes.len() {
        return Err(P9Error::new(format!(
            "{} says {} bytes but the frame is {}",
            message_name(header.type_),
            header.size,
            bytes.len()
        )));
    }
    Ok((header, reader))
}

pub fn decode_message_as<T, F>(bytes: &[u8], read: F) -> Result<(P9Header, T), P9Error>
where
    F: FnOnce(&mut P9Reader<'_>) -> Result<T, P9Error>,
{
    let (header, mut body) = decode_message(bytes)?;
    let value = read(&mut body)?;
    body.end(&message_name(header.type_))?;
    Ok((header, value))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct P9Time {
    pub sec: u64,
    pub nsec: u64,
}

fn write_time(writer: &mut P9Writer, value: P9Time) {
    writer.u64(value.sec);
    writer.u64(value.nsec);
}

fn read_time(reader: &mut P9Reader<'_>, what: &str) -> Result<P9Time, P9Error> {
    Ok(P9Time {
        sec: reader.u64(&format!("{what}_sec"))?,
        nsec: reader.u64(&format!("{what}_nsec"))?,
    })
}

fn check_elements(count: usize, what: &str) -> Result<usize, P9Error> {
    if count > P9_MAXWELEM {
        Err(P9Error::new(format!(
            "{what} is {count}, over the {P9_MAXWELEM}-element limit"
        )))
    } else {
        Ok(count)
    }
}

fn read_element_count(reader: &mut P9Reader<'_>, what: &str) -> Result<usize, P9Error> {
    check_elements(reader.u16(what)? as usize, what)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tversion {
    pub msize: u32,
    pub version: String,
}
pub type Rversion = Tversion;

pub fn write_tversion(writer: &mut P9Writer, value: &Tversion) -> Result<(), P9Error> {
    writer.u32(value.msize);
    writer.string(&value.version)
}
pub fn read_tversion(reader: &mut P9Reader<'_>) -> Result<Tversion, P9Error> {
    Ok(Tversion {
        msize: reader.u32("msize")?,
        version: reader.string("version")?,
    })
}
pub fn write_rversion(writer: &mut P9Writer, value: &Rversion) -> Result<(), P9Error> {
    write_tversion(writer, value)
}
pub fn read_rversion(reader: &mut P9Reader<'_>) -> Result<Rversion, P9Error> {
    read_tversion(reader)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tauth {
    pub afid: u32,
    pub uname: String,
    pub aname: String,
    pub n_uname: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rauth {
    pub aqid: P9Qid,
}
pub fn write_tauth(writer: &mut P9Writer, value: &Tauth) -> Result<(), P9Error> {
    writer.u32(value.afid);
    writer.string(&value.uname)?;
    writer.string(&value.aname)?;
    writer.u32(value.n_uname);
    Ok(())
}
pub fn read_tauth(reader: &mut P9Reader<'_>) -> Result<Tauth, P9Error> {
    Ok(Tauth {
        afid: reader.u32("afid")?,
        uname: reader.string("uname")?,
        aname: reader.string("aname")?,
        n_uname: reader.u32("n_uname")?,
    })
}
pub fn write_rauth(writer: &mut P9Writer, value: &Rauth) {
    writer.qid(value.aqid);
}
pub fn read_rauth(reader: &mut P9Reader<'_>) -> Result<Rauth, P9Error> {
    Ok(Rauth {
        aqid: reader.qid("aqid")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tattach {
    pub fid: u32,
    pub afid: u32,
    pub uname: String,
    pub aname: String,
    pub n_uname: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rattach {
    pub qid: P9Qid,
}
pub fn write_tattach(writer: &mut P9Writer, value: &Tattach) -> Result<(), P9Error> {
    writer.u32(value.fid);
    writer.u32(value.afid);
    writer.string(&value.uname)?;
    writer.string(&value.aname)?;
    writer.u32(value.n_uname);
    Ok(())
}
pub fn read_tattach(reader: &mut P9Reader<'_>) -> Result<Tattach, P9Error> {
    Ok(Tattach {
        fid: reader.u32("fid")?,
        afid: reader.u32("afid")?,
        uname: reader.string("uname")?,
        aname: reader.string("aname")?,
        n_uname: reader.u32("n_uname")?,
    })
}
pub fn write_rattach(writer: &mut P9Writer, value: &Rattach) {
    writer.qid(value.qid);
}
pub fn read_rattach(reader: &mut P9Reader<'_>) -> Result<Rattach, P9Error> {
    Ok(Rattach {
        qid: reader.qid("qid")?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rlerror {
    pub ecode: u32,
}
pub fn write_rlerror(writer: &mut P9Writer, value: Rlerror) {
    writer.u32(value.ecode);
}
pub fn read_rlerror(reader: &mut P9Reader<'_>) -> Result<Rlerror, P9Error> {
    Ok(Rlerror {
        ecode: reader.u32("ecode")?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tflush {
    pub oldtag: u16,
}
pub fn write_tflush(writer: &mut P9Writer, value: Tflush) {
    writer.u16(value.oldtag);
}
pub fn read_tflush(reader: &mut P9Reader<'_>) -> Result<Tflush, P9Error> {
    Ok(Tflush {
        oldtag: reader.u16("oldtag")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Twalk {
    pub fid: u32,
    pub newfid: u32,
    pub wnames: Vec<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rwalk {
    pub wqids: Vec<P9Qid>,
}
pub fn write_twalk(writer: &mut P9Writer, value: &Twalk) -> Result<(), P9Error> {
    writer.u32(value.fid);
    writer.u32(value.newfid);
    writer.u16(check_elements(value.wnames.len(), "nwname")? as u16);
    for name in &value.wnames {
        writer.string(name)?;
    }
    Ok(())
}
pub fn read_twalk(reader: &mut P9Reader<'_>) -> Result<Twalk, P9Error> {
    let fid = reader.u32("fid")?;
    let newfid = reader.u32("newfid")?;
    let count = read_element_count(reader, "nwname")?;
    let mut wnames = Vec::with_capacity(count);
    for index in 0..count {
        wnames.push(reader.string(&format!("wname[{index}]"))?);
    }
    Ok(Twalk {
        fid,
        newfid,
        wnames,
    })
}
pub fn write_rwalk(writer: &mut P9Writer, value: &Rwalk) -> Result<(), P9Error> {
    writer.u16(check_elements(value.wqids.len(), "nwqid")? as u16);
    for qid in &value.wqids {
        writer.qid(*qid);
    }
    Ok(())
}
pub fn read_rwalk(reader: &mut P9Reader<'_>) -> Result<Rwalk, P9Error> {
    let count = read_element_count(reader, "nwqid")?;
    let mut wqids = Vec::with_capacity(count);
    for index in 0..count {
        wqids.push(reader.qid(&format!("wqid[{index}]"))?);
    }
    Ok(Rwalk { wqids })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tread {
    pub fid: u32,
    pub offset: u64,
    pub count: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rread {
    pub data: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Twrite {
    pub fid: u32,
    pub offset: u64,
    pub data: Vec<u8>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rwrite {
    pub count: u32,
}
pub fn write_tread(writer: &mut P9Writer, value: Tread) {
    writer.u32(value.fid);
    writer.u64(value.offset);
    writer.u32(value.count);
}
pub fn read_tread(reader: &mut P9Reader<'_>) -> Result<Tread, P9Error> {
    Ok(Tread {
        fid: reader.u32("fid")?,
        offset: reader.u64("offset")?,
        count: reader.u32("count")?,
    })
}
pub fn write_rread(writer: &mut P9Writer, value: &Rread) {
    writer.blob(&value.data);
}
pub fn read_rread(reader: &mut P9Reader<'_>) -> Result<Rread, P9Error> {
    Ok(Rread {
        data: reader.blob("read data")?,
    })
}
pub fn write_twrite(writer: &mut P9Writer, value: &Twrite) {
    writer.u32(value.fid);
    writer.u64(value.offset);
    writer.blob(&value.data);
}
pub fn read_twrite(reader: &mut P9Reader<'_>) -> Result<Twrite, P9Error> {
    Ok(Twrite {
        fid: reader.u32("fid")?,
        offset: reader.u64("offset")?,
        data: reader.blob("write data")?,
    })
}
pub fn write_rwrite(writer: &mut P9Writer, value: Rwrite) {
    writer.u32(value.count);
}
pub fn read_rwrite(reader: &mut P9Reader<'_>) -> Result<Rwrite, P9Error> {
    Ok(Rwrite {
        count: reader.u32("count")?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FidRequest {
    pub fid: u32,
}
pub fn write_fid_request(writer: &mut P9Writer, value: FidRequest) {
    writer.u32(value.fid);
}
pub fn read_fid_request(reader: &mut P9Reader<'_>) -> Result<FidRequest, P9Error> {
    Ok(FidRequest {
        fid: reader.u32("fid")?,
    })
}

// The .L protocol gives these fid-only messages distinct names even though
// their wire body is the same. Keep the aliases explicit so callers can use
// the upstream operation vocabulary without weakening the shared codec.
pub fn write_tclunk(writer: &mut P9Writer, value: FidRequest) {
    write_fid_request(writer, value);
}

pub fn read_tclunk(reader: &mut P9Reader<'_>) -> Result<FidRequest, P9Error> {
    read_fid_request(reader)
}

pub fn write_tremove(writer: &mut P9Writer, value: FidRequest) {
    write_fid_request(writer, value);
}

pub fn read_tremove(reader: &mut P9Reader<'_>) -> Result<FidRequest, P9Error> {
    read_fid_request(reader)
}

pub fn write_tstatfs(writer: &mut P9Writer, value: FidRequest) {
    write_fid_request(writer, value);
}

pub fn read_tstatfs(reader: &mut P9Reader<'_>) -> Result<FidRequest, P9Error> {
    read_fid_request(reader)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rstatfs {
    pub type_: u32,
    pub bsize: u32,
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub fsid: u64,
    pub namelen: u32,
}
pub fn write_rstatfs(writer: &mut P9Writer, value: Rstatfs) {
    writer.u32(value.type_);
    writer.u32(value.bsize);
    writer.u64(value.blocks);
    writer.u64(value.bfree);
    writer.u64(value.bavail);
    writer.u64(value.files);
    writer.u64(value.ffree);
    writer.u64(value.fsid);
    writer.u32(value.namelen);
}
pub fn read_rstatfs(reader: &mut P9Reader<'_>) -> Result<Rstatfs, P9Error> {
    Ok(Rstatfs {
        type_: reader.u32("type")?,
        bsize: reader.u32("bsize")?,
        blocks: reader.u64("blocks")?,
        bfree: reader.u64("bfree")?,
        bavail: reader.u64("bavail")?,
        files: reader.u64("files")?,
        ffree: reader.u64("ffree")?,
        fsid: reader.u64("fsid")?,
        namelen: reader.u32("namelen")?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tlopen {
    pub fid: u32,
    pub flags: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rlopen {
    pub qid: P9Qid,
    pub iounit: u32,
}
pub type Rlcreate = Rlopen;
pub fn write_tlopen(writer: &mut P9Writer, value: Tlopen) {
    writer.u32(value.fid);
    writer.u32(value.flags);
}
pub fn read_tlopen(reader: &mut P9Reader<'_>) -> Result<Tlopen, P9Error> {
    Ok(Tlopen {
        fid: reader.u32("fid")?,
        flags: reader.u32("flags")?,
    })
}
pub fn write_rlopen(writer: &mut P9Writer, value: Rlopen) {
    writer.qid(value.qid);
    writer.u32(value.iounit);
}
pub fn read_rlopen(reader: &mut P9Reader<'_>) -> Result<Rlopen, P9Error> {
    Ok(Rlopen {
        qid: reader.qid("qid")?,
        iounit: reader.u32("iounit")?,
    })
}

pub fn write_rlcreate(writer: &mut P9Writer, value: Rlcreate) {
    write_rlopen(writer, value);
}

pub fn read_rlcreate(reader: &mut P9Reader<'_>) -> Result<Rlcreate, P9Error> {
    read_rlopen(reader)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlcreate {
    pub fid: u32,
    pub name: String,
    pub flags: u32,
    pub mode: u32,
    pub gid: u32,
}
pub fn write_tlcreate(writer: &mut P9Writer, value: &Tlcreate) -> Result<(), P9Error> {
    writer.u32(value.fid);
    writer.string(&value.name)?;
    writer.u32(value.flags);
    writer.u32(value.mode);
    writer.u32(value.gid);
    Ok(())
}
pub fn read_tlcreate(reader: &mut P9Reader<'_>) -> Result<Tlcreate, P9Error> {
    Ok(Tlcreate {
        fid: reader.u32("fid")?,
        name: reader.string("name")?,
        flags: reader.u32("flags")?,
        mode: reader.u32("mode")?,
        gid: reader.u32("gid")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tsymlink {
    pub dfid: u32,
    pub name: String,
    pub symtgt: String,
    pub gid: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QidReply {
    pub qid: P9Qid,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tmknod {
    pub dfid: u32,
    pub name: String,
    pub mode: u32,
    pub major: u32,
    pub minor: u32,
    pub gid: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tmkdir {
    pub dfid: u32,
    pub name: String,
    pub mode: u32,
    pub gid: u32,
}

pub fn write_qid_reply(writer: &mut P9Writer, value: QidReply) {
    writer.qid(value.qid);
}
pub fn read_qid_reply(reader: &mut P9Reader<'_>) -> Result<QidReply, P9Error> {
    Ok(QidReply {
        qid: reader.qid("qid")?,
    })
}

pub fn write_rsymlink(writer: &mut P9Writer, value: QidReply) {
    write_qid_reply(writer, value);
}

pub fn read_rsymlink(reader: &mut P9Reader<'_>) -> Result<QidReply, P9Error> {
    read_qid_reply(reader)
}

pub fn write_rmknod(writer: &mut P9Writer, value: QidReply) {
    write_qid_reply(writer, value);
}

pub fn read_rmknod(reader: &mut P9Reader<'_>) -> Result<QidReply, P9Error> {
    read_qid_reply(reader)
}

pub fn write_rmkdir(writer: &mut P9Writer, value: QidReply) {
    write_qid_reply(writer, value);
}

pub fn read_rmkdir(reader: &mut P9Reader<'_>) -> Result<QidReply, P9Error> {
    read_qid_reply(reader)
}
pub fn write_tsymlink(writer: &mut P9Writer, value: &Tsymlink) -> Result<(), P9Error> {
    writer.u32(value.dfid);
    writer.string(&value.name)?;
    writer.string(&value.symtgt)?;
    writer.u32(value.gid);
    Ok(())
}
pub fn read_tsymlink(reader: &mut P9Reader<'_>) -> Result<Tsymlink, P9Error> {
    Ok(Tsymlink {
        dfid: reader.u32("dfid")?,
        name: reader.string("name")?,
        symtgt: reader.string("symtgt")?,
        gid: reader.u32("gid")?,
    })
}
pub fn write_tmknod(writer: &mut P9Writer, value: &Tmknod) -> Result<(), P9Error> {
    writer.u32(value.dfid);
    writer.string(&value.name)?;
    writer.u32(value.mode);
    writer.u32(value.major);
    writer.u32(value.minor);
    writer.u32(value.gid);
    Ok(())
}
pub fn read_tmknod(reader: &mut P9Reader<'_>) -> Result<Tmknod, P9Error> {
    Ok(Tmknod {
        dfid: reader.u32("dfid")?,
        name: reader.string("name")?,
        mode: reader.u32("mode")?,
        major: reader.u32("major")?,
        minor: reader.u32("minor")?,
        gid: reader.u32("gid")?,
    })
}
pub fn write_tmkdir(writer: &mut P9Writer, value: &Tmkdir) -> Result<(), P9Error> {
    writer.u32(value.dfid);
    writer.string(&value.name)?;
    writer.u32(value.mode);
    writer.u32(value.gid);
    Ok(())
}
pub fn read_tmkdir(reader: &mut P9Reader<'_>) -> Result<Tmkdir, P9Error> {
    Ok(Tmkdir {
        dfid: reader.u32("dfid")?,
        name: reader.string("name")?,
        mode: reader.u32("mode")?,
        gid: reader.u32("gid")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trename {
    pub fid: u32,
    pub dfid: u32,
    pub name: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trenameat {
    pub olddirfid: u32,
    pub oldname: String,
    pub newdirfid: u32,
    pub newname: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tunlinkat {
    pub dirfid: u32,
    pub name: String,
    pub flags: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlink {
    pub dfid: u32,
    pub fid: u32,
    pub name: String,
}

pub fn write_trename(writer: &mut P9Writer, value: &Trename) -> Result<(), P9Error> {
    writer.u32(value.fid);
    writer.u32(value.dfid);
    writer.string(&value.name)
}
pub fn read_trename(reader: &mut P9Reader<'_>) -> Result<Trename, P9Error> {
    Ok(Trename {
        fid: reader.u32("fid")?,
        dfid: reader.u32("dfid")?,
        name: reader.string("name")?,
    })
}
pub fn write_trenameat(writer: &mut P9Writer, value: &Trenameat) -> Result<(), P9Error> {
    writer.u32(value.olddirfid);
    writer.string(&value.oldname)?;
    writer.u32(value.newdirfid);
    writer.string(&value.newname)?;
    Ok(())
}
pub fn read_trenameat(reader: &mut P9Reader<'_>) -> Result<Trenameat, P9Error> {
    Ok(Trenameat {
        olddirfid: reader.u32("olddirfid")?,
        oldname: reader.string("oldname")?,
        newdirfid: reader.u32("newdirfid")?,
        newname: reader.string("newname")?,
    })
}
pub fn write_tunlinkat(writer: &mut P9Writer, value: &Tunlinkat) -> Result<(), P9Error> {
    writer.u32(value.dirfid);
    writer.string(&value.name)?;
    writer.u32(value.flags);
    Ok(())
}
pub fn read_tunlinkat(reader: &mut P9Reader<'_>) -> Result<Tunlinkat, P9Error> {
    Ok(Tunlinkat {
        dirfid: reader.u32("dirfid")?,
        name: reader.string("name")?,
        flags: reader.u32("flags")?,
    })
}
pub fn write_tlink(writer: &mut P9Writer, value: &Tlink) -> Result<(), P9Error> {
    writer.u32(value.dfid);
    writer.u32(value.fid);
    writer.string(&value.name)
}
pub fn read_tlink(reader: &mut P9Reader<'_>) -> Result<Tlink, P9Error> {
    Ok(Tlink {
        dfid: reader.u32("dfid")?,
        fid: reader.u32("fid")?,
        name: reader.string("name")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rreadlink {
    pub target: String,
}
pub fn write_rreadlink(writer: &mut P9Writer, value: &Rreadlink) -> Result<(), P9Error> {
    writer.string(&value.target)
}
pub fn read_rreadlink(reader: &mut P9Reader<'_>) -> Result<Rreadlink, P9Error> {
    Ok(Rreadlink {
        target: reader.string("target")?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tgetattr {
    pub fid: u32,
    pub request_mask: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgetattr {
    pub valid: u64,
    pub qid: P9Qid,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub nlink: u64,
    pub rdev: u64,
    pub size: u64,
    pub blksize: u64,
    pub blocks: u64,
    pub atime: P9Time,
    pub mtime: P9Time,
    pub ctime: P9Time,
    pub btime: P9Time,
    pub r#gen: u64,
    pub data_version: u64,
}
pub fn write_tgetattr(writer: &mut P9Writer, value: Tgetattr) {
    writer.u32(value.fid);
    writer.u64(value.request_mask);
}
pub fn read_tgetattr(reader: &mut P9Reader<'_>) -> Result<Tgetattr, P9Error> {
    Ok(Tgetattr {
        fid: reader.u32("fid")?,
        request_mask: reader.u64("request_mask")?,
    })
}
pub fn write_rgetattr(writer: &mut P9Writer, value: Rgetattr) {
    writer.u64(value.valid);
    writer.qid(value.qid);
    writer.u32(value.mode);
    writer.u32(value.uid);
    writer.u32(value.gid);
    writer.u64(value.nlink);
    writer.u64(value.rdev);
    writer.u64(value.size);
    writer.u64(value.blksize);
    writer.u64(value.blocks);
    write_time(writer, value.atime);
    write_time(writer, value.mtime);
    write_time(writer, value.ctime);
    write_time(writer, value.btime);
    writer.u64(value.r#gen);
    writer.u64(value.data_version);
}
pub fn read_rgetattr(reader: &mut P9Reader<'_>) -> Result<Rgetattr, P9Error> {
    Ok(Rgetattr {
        valid: reader.u64("valid")?,
        qid: reader.qid("qid")?,
        mode: reader.u32("mode")?,
        uid: reader.u32("uid")?,
        gid: reader.u32("gid")?,
        nlink: reader.u64("nlink")?,
        rdev: reader.u64("rdev")?,
        size: reader.u64("size")?,
        blksize: reader.u64("blksize")?,
        blocks: reader.u64("blocks")?,
        atime: read_time(reader, "atime")?,
        mtime: read_time(reader, "mtime")?,
        ctime: read_time(reader, "ctime")?,
        btime: read_time(reader, "btime")?,
        r#gen: reader.u64("gen")?,
        data_version: reader.u64("data_version")?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tsetattr {
    pub fid: u32,
    pub valid: u32,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub atime: P9Time,
    pub mtime: P9Time,
}
pub fn write_tsetattr(writer: &mut P9Writer, value: Tsetattr) {
    writer.u32(value.fid);
    writer.u32(value.valid);
    writer.u32(value.mode);
    writer.u32(value.uid);
    writer.u32(value.gid);
    writer.u64(value.size);
    write_time(writer, value.atime);
    write_time(writer, value.mtime);
}
pub fn read_tsetattr(reader: &mut P9Reader<'_>) -> Result<Tsetattr, P9Error> {
    Ok(Tsetattr {
        fid: reader.u32("fid")?,
        valid: reader.u32("valid")?,
        mode: reader.u32("mode")?,
        uid: reader.u32("uid")?,
        gid: reader.u32("gid")?,
        size: reader.u64("size")?,
        atime: read_time(reader, "atime")?,
        mtime: read_time(reader, "mtime")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Txattrwalk {
    pub fid: u32,
    pub newfid: u32,
    pub name: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rxattrwalk {
    pub size: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Txattrcreate {
    pub fid: u32,
    pub name: String,
    pub attr_size: u64,
    pub flags: u32,
}
pub fn write_txattrwalk(writer: &mut P9Writer, value: &Txattrwalk) -> Result<(), P9Error> {
    writer.u32(value.fid);
    writer.u32(value.newfid);
    writer.string(&value.name)
}
pub fn read_txattrwalk(reader: &mut P9Reader<'_>) -> Result<Txattrwalk, P9Error> {
    Ok(Txattrwalk {
        fid: reader.u32("fid")?,
        newfid: reader.u32("newfid")?,
        name: reader.string("name")?,
    })
}
pub fn write_rxattrwalk(writer: &mut P9Writer, value: Rxattrwalk) {
    writer.u64(value.size);
}
pub fn read_rxattrwalk(reader: &mut P9Reader<'_>) -> Result<Rxattrwalk, P9Error> {
    Ok(Rxattrwalk {
        size: reader.u64("size")?,
    })
}
pub fn write_txattrcreate(writer: &mut P9Writer, value: &Txattrcreate) -> Result<(), P9Error> {
    writer.u32(value.fid);
    writer.string(&value.name)?;
    writer.u64(value.attr_size);
    writer.u32(value.flags);
    Ok(())
}
pub fn read_txattrcreate(reader: &mut P9Reader<'_>) -> Result<Txattrcreate, P9Error> {
    Ok(Txattrcreate {
        fid: reader.u32("fid")?,
        name: reader.string("name")?,
        attr_size: reader.u64("attr_size")?,
        flags: reader.u32("flags")?,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Treaddir {
    pub fid: u32,
    pub offset: u64,
    pub count: u32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rreaddir {
    pub data: Vec<u8>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P9Dirent {
    pub qid: P9Qid,
    pub offset: u64,
    pub type_: u8,
    pub name: String,
}
pub fn write_treaddir(writer: &mut P9Writer, value: Treaddir) {
    writer.u32(value.fid);
    writer.u64(value.offset);
    writer.u32(value.count);
}
pub fn read_treaddir(reader: &mut P9Reader<'_>) -> Result<Treaddir, P9Error> {
    Ok(Treaddir {
        fid: reader.u32("fid")?,
        offset: reader.u64("offset")?,
        count: reader.u32("count")?,
    })
}
pub fn write_rreaddir(writer: &mut P9Writer, value: &Rreaddir) {
    writer.blob(&value.data);
}
pub fn read_rreaddir(reader: &mut P9Reader<'_>) -> Result<Rreaddir, P9Error> {
    Ok(Rreaddir {
        data: reader.blob("readdir data")?,
    })
}
pub fn dirent_size(name: &str) -> usize {
    P9_QID_SIZE + 8 + 1 + 2 + string_byte_length(name)
}
pub fn write_dirent(writer: &mut P9Writer, value: &P9Dirent) -> Result<(), P9Error> {
    writer.qid(value.qid);
    writer.u64(value.offset);
    writer.u8(value.type_);
    writer.string(&value.name)
}
pub fn read_dirent(reader: &mut P9Reader<'_>) -> Result<P9Dirent, P9Error> {
    Ok(P9Dirent {
        qid: reader.qid("dirent qid")?,
        offset: reader.u64("dirent offset")?,
        type_: reader.u8("dirent type")?,
        name: reader.string("dirent name")?,
    })
}
pub fn read_dirents(data: &[u8]) -> Result<Vec<P9Dirent>, P9Error> {
    let mut reader = P9Reader::new(data);
    let mut result = Vec::new();
    while !reader.at_end() {
        result.push(read_dirent(&mut reader)?);
    }
    Ok(result)
}

#[derive(Debug, Clone)]
pub struct P9DirentPacker {
    max_size: usize,
    writer: P9Writer,
    count: usize,
}
impl P9DirentPacker {
    pub fn new(max_size: usize) -> Self {
        Self {
            max_size,
            writer: P9Writer::new(256),
            count: 0,
        }
    }
    pub fn size(&self) -> usize {
        self.writer.len()
    }
    pub fn count(&self) -> usize {
        self.count
    }
    pub fn remaining(&self) -> usize {
        self.max_size.saturating_sub(self.writer.len())
    }
    pub fn add(&mut self, value: &P9Dirent) -> Result<bool, P9Error> {
        let size = dirent_size(&value.name);
        if string_byte_length(&value.name) > P9_MAX_STRING {
            return Err(P9Error::new(format!(
                "dirent name is {} bytes, over the 16-bit count",
                string_byte_length(&value.name)
            )));
        }
        if size > self.remaining() {
            return Ok(false);
        }
        write_dirent(&mut self.writer, value)?;
        self.count += 1;
        Ok(true)
    }
    pub fn bytes(&self) -> Vec<u8> {
        self.writer.bytes()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tfsync {
    pub fid: u32,
    pub datasync: u32,
}
pub fn write_tfsync(writer: &mut P9Writer, value: Tfsync) {
    writer.u32(value.fid);
    writer.u32(value.datasync);
}
pub fn read_tfsync(reader: &mut P9Reader<'_>) -> Result<Tfsync, P9Error> {
    Ok(Tfsync {
        fid: reader.u32("fid")?,
        datasync: reader.u32("datasync")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tlock {
    pub fid: u32,
    pub type_: u8,
    pub flags: u32,
    pub start: u64,
    pub length: u64,
    pub proc_id: u32,
    pub client_id: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rlock {
    pub status: u8,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tgetlock {
    pub fid: u32,
    pub type_: u8,
    pub start: u64,
    pub length: u64,
    pub proc_id: u32,
    pub client_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rgetlock {
    pub type_: u8,
    pub start: u64,
    pub length: u64,
    pub proc_id: u32,
    pub client_id: String,
}
pub fn write_tlock(writer: &mut P9Writer, value: &Tlock) -> Result<(), P9Error> {
    writer.u32(value.fid);
    writer.u8(value.type_);
    writer.u32(value.flags);
    writer.u64(value.start);
    writer.u64(value.length);
    writer.u32(value.proc_id);
    writer.string(&value.client_id)
}
pub fn read_tlock(reader: &mut P9Reader<'_>) -> Result<Tlock, P9Error> {
    Ok(Tlock {
        fid: reader.u32("fid")?,
        type_: reader.u8("type")?,
        flags: reader.u32("flags")?,
        start: reader.u64("start")?,
        length: reader.u64("length")?,
        proc_id: reader.u32("proc_id")?,
        client_id: reader.string("client_id")?,
    })
}
pub fn write_rlock(writer: &mut P9Writer, value: Rlock) {
    writer.u8(value.status);
}
pub fn read_rlock(reader: &mut P9Reader<'_>) -> Result<Rlock, P9Error> {
    Ok(Rlock {
        status: reader.u8("status")?,
    })
}
pub fn write_tgetlock(writer: &mut P9Writer, value: &Tgetlock) -> Result<(), P9Error> {
    writer.u32(value.fid);
    writer.u8(value.type_);
    writer.u64(value.start);
    writer.u64(value.length);
    writer.u32(value.proc_id);
    writer.string(&value.client_id)
}
pub fn read_tgetlock(reader: &mut P9Reader<'_>) -> Result<Tgetlock, P9Error> {
    Ok(Tgetlock {
        fid: reader.u32("fid")?,
        type_: reader.u8("type")?,
        start: reader.u64("start")?,
        length: reader.u64("length")?,
        proc_id: reader.u32("proc_id")?,
        client_id: reader.string("client_id")?,
    })
}
pub fn write_rgetlock(writer: &mut P9Writer, value: &Rgetlock) -> Result<(), P9Error> {
    writer.u8(value.type_);
    writer.u64(value.start);
    writer.u64(value.length);
    writer.u32(value.proc_id);
    writer.string(&value.client_id)
}
pub fn read_rgetlock(reader: &mut P9Reader<'_>) -> Result<Rgetlock, P9Error> {
    Ok(Rgetlock {
        type_: reader.u8("type")?,
        start: reader.u64("start")?,
        length: reader.u64("length")?,
        proc_id: reader.u32("proc_id")?,
        client_id: reader.string("client_id")?,
    })
}

/// Stream reassembly with a terminal framing error.
#[derive(Debug, Clone)]
pub struct P9FrameAssembler {
    buffer: Vec<u8>,
    limit: usize,
    failed: bool,
}

impl P9FrameAssembler {
    pub fn new(limit: usize) -> Result<Self, P9Error> {
        if limit < P9_HDRSZ {
            return Err(P9Error::new(format!(
                "frame limit {limit} is below the {P9_HDRSZ}-byte header"
            )));
        }
        Ok(Self {
            buffer: Vec::new(),
            limit,
            failed: false,
        })
    }

    pub fn limit(&self) -> usize {
        self.limit
    }

    pub fn set_limit(&mut self, limit: usize) -> Result<(), P9Error> {
        if limit < P9_HDRSZ {
            return Err(P9Error::new(format!(
                "frame limit {limit} is below the {P9_HDRSZ}-byte header"
            )));
        }
        self.limit = limit;
        Ok(())
    }

    pub fn pending(&self) -> usize {
        self.buffer.len()
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn reset(&mut self) {
        self.buffer.clear();
        self.failed = false;
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, P9Error> {
        if self.failed {
            return Err(P9Error::new(
                "frame stream is unusable after a framing error",
            ));
        }
        let result = self.parse(chunk);
        if result.is_err() {
            self.failed = true;
            self.buffer.clear();
        }
        result
    }

    fn parse(&mut self, chunk: &[u8]) -> Result<Vec<Vec<u8>>, P9Error> {
        let mut frames = Vec::new();
        if self.buffer.is_empty() {
            let mut at = 0;
            while chunk.len().saturating_sub(at) >= P9_HDRSZ {
                let size =
                    u32::from_le_bytes([chunk[at], chunk[at + 1], chunk[at + 2], chunk[at + 3]])
                        as usize;
                self.check_size(size, at)?;
                if chunk.len() - at < size {
                    self.buffer.extend_from_slice(&chunk[at..]);
                    return Ok(frames);
                }
                frames.push(chunk[at..at + size].to_vec());
                at += size;
            }
            if at < chunk.len() {
                self.buffer.extend_from_slice(&chunk[at..]);
            }
            return Ok(frames);
        }

        self.buffer.extend_from_slice(chunk);
        loop {
            if self.buffer.len() < P9_HDRSZ {
                break;
            }
            let size = u32::from_le_bytes([
                self.buffer[0],
                self.buffer[1],
                self.buffer[2],
                self.buffer[3],
            ]) as usize;
            self.check_size(size, 0)?;
            if self.buffer.len() < size {
                break;
            }
            frames.push(self.buffer.drain(..size).collect());
        }
        Ok(frames)
    }

    fn check_size(&self, size: usize, offset: usize) -> Result<(), P9Error> {
        if size < P9_HDRSZ {
            return Err(P9Error::at(
                format!("frame size {size} is below the {P9_HDRSZ}-byte header"),
                offset,
            ));
        }
        if size > self.limit {
            return Err(P9Error::at(
                format!(
                    "frame of {size} bytes exceeds the {}-byte limit",
                    self.limit
                ),
                offset,
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_fixture_has_protocol_header_and_body() {
        let frame = encode_message(P9_TVERSION, P9_NOTAG, 32, |writer| {
            write_tversion(
                writer,
                &Tversion {
                    msize: 8192,
                    version: P9_VERSION_DOTL.to_owned(),
                },
            )
        })
        .unwrap();
        assert_eq!(
            frame.len(),
            u32::from_le_bytes(frame[..4].try_into().unwrap()) as usize
        );
        let (header, value) = decode_message_as(&frame, read_tversion).unwrap();
        assert_eq!(header.type_, P9_TVERSION);
        assert_eq!(header.tag, P9_NOTAG);
        assert_eq!(value.version, P9_VERSION_DOTL);
        assert_eq!(value.msize, 8192);
    }

    #[test]
    fn assembler_handles_split_and_coalesced_frames() {
        let first = encode_message(P9_RCLUNK, 1, 8, |_| Ok(())).unwrap();
        let second = encode_message(P9_RCLUNK, 2, 8, |_| Ok(())).unwrap();
        let mut assembler = P9FrameAssembler::new(4096).unwrap();
        assert!(assembler.push(&first[..3]).unwrap().is_empty());
        let frames = assembler
            .push(&[&first[3..], &second[..]].concat())
            .unwrap();
        assert_eq!(frames, vec![first, second]);
    }

    #[test]
    fn dirent_packer_does_not_partially_write_an_entry() {
        let qid = P9Qid {
            type_: P9_QTFILE,
            version: 0,
            path: 1,
        };
        let entry = P9Dirent {
            qid,
            offset: 1,
            type_: 8,
            name: "x".to_owned(),
        };
        let mut packer = P9DirentPacker::new(dirent_size("x") - 1);
        assert!(!packer.add(&entry).unwrap());
        assert_eq!(packer.size(), 0);
    }
}
