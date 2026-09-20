//! NFSv3/MOUNTv3 XDR structures from RFC 1813.

use mount_rs_core::{ErrorCode, Stats};

use crate::constants::*;
use crate::xdr::{XdrError, XdrReader, XdrWriter, xdr_align};

pub fn nfs_status_of(error: &mount_rs_core::FsError) -> u32 {
    match error.code {
        ErrorCode::Eperm => NFS3ERR_PERM,
        ErrorCode::Enoent => NFS3ERR_NOENT,
        ErrorCode::Eio => NFS3ERR_IO,
        ErrorCode::Enxio => NFS3ERR_NXIO,
        ErrorCode::Eacces => NFS3ERR_ACCES,
        ErrorCode::Eexist => NFS3ERR_EXIST,
        ErrorCode::Exdev => NFS3ERR_XDEV,
        ErrorCode::Enodev => NFS3ERR_NODEV,
        ErrorCode::Enotdir => NFS3ERR_NOTDIR,
        ErrorCode::Eisdir => NFS3ERR_ISDIR,
        ErrorCode::Einval => NFS3ERR_INVAL,
        ErrorCode::Efbig => NFS3ERR_FBIG,
        ErrorCode::Enospc => NFS3ERR_NOSPC,
        ErrorCode::Erofs => NFS3ERR_ROFS,
        ErrorCode::Emlink => NFS3ERR_MLINK,
        ErrorCode::Enametoolong => NFS3ERR_NAMETOOLONG,
        ErrorCode::Enotempty => NFS3ERR_NOTEMPTY,
        ErrorCode::Edquot => NFS3ERR_DQUOT,
        ErrorCode::Estale => NFS3ERR_STALE,
        ErrorCode::Enosys | ErrorCode::Enotsup => NFS3ERR_NOTSUPP,
        ErrorCode::Eloop => NFS3ERR_INVAL,
        _ => NFS3ERR_IO,
    }
}

/// Map an NFSv3 status back to the POSIX-shaped error namespace used by the
/// core driver. Unknown protocol statuses conservatively become `EIO`.
pub fn error_code_of_status(status: u32) -> ErrorCode {
    match status {
        NFS3ERR_PERM => ErrorCode::Eperm,
        NFS3ERR_NOENT => ErrorCode::Enoent,
        NFS3ERR_IO | NFS3ERR_SERVERFAULT => ErrorCode::Eio,
        NFS3ERR_NXIO => ErrorCode::Enxio,
        NFS3ERR_ACCES => ErrorCode::Eacces,
        NFS3ERR_EXIST => ErrorCode::Eexist,
        NFS3ERR_XDEV => ErrorCode::Exdev,
        NFS3ERR_NODEV => ErrorCode::Enodev,
        NFS3ERR_NOTDIR => ErrorCode::Enotdir,
        NFS3ERR_ISDIR => ErrorCode::Eisdir,
        NFS3ERR_INVAL => ErrorCode::Einval,
        NFS3ERR_FBIG => ErrorCode::Efbig,
        NFS3ERR_NOSPC => ErrorCode::Enospc,
        NFS3ERR_ROFS => ErrorCode::Erofs,
        NFS3ERR_MLINK => ErrorCode::Emlink,
        NFS3ERR_NAMETOOLONG => ErrorCode::Enametoolong,
        NFS3ERR_NOTEMPTY => ErrorCode::Enotempty,
        NFS3ERR_DQUOT => ErrorCode::Edquot,
        NFS3ERR_STALE | NFS3ERR_BADHANDLE => ErrorCode::Estale,
        NFS3ERR_NOTSUPP => ErrorCode::Enotsup,
        _ => ErrorCode::Eio,
    }
}

/// Return the positive errno number corresponding to an NFSv3 status.
pub fn errno_of_status(status: u32) -> i32 {
    error_code_of_status(status).errno()
}

pub fn status_name(status: u32) -> String {
    if status == NFS3_OK {
        return "NFS3_OK".to_owned();
    }
    let name = match status {
        NFS3ERR_PERM => "EPERM",
        NFS3ERR_NOENT => "ENOENT",
        NFS3ERR_IO => "EIO",
        NFS3ERR_NXIO => "ENXIO",
        NFS3ERR_ACCES => "EACCES",
        NFS3ERR_EXIST => "EEXIST",
        NFS3ERR_XDEV => "EXDEV",
        NFS3ERR_NODEV => "ENODEV",
        NFS3ERR_NOTDIR => "ENOTDIR",
        NFS3ERR_ISDIR => "EISDIR",
        NFS3ERR_INVAL => "EINVAL",
        NFS3ERR_FBIG => "EFBIG",
        NFS3ERR_NOSPC => "ENOSPC",
        NFS3ERR_ROFS => "EROFS",
        NFS3ERR_MLINK => "EMLINK",
        NFS3ERR_NAMETOOLONG => "ENAMETOOLONG",
        NFS3ERR_NOTEMPTY => "ENOTEMPTY",
        NFS3ERR_DQUOT => "EDQUOT",
        NFS3ERR_STALE | NFS3ERR_BADHANDLE => "ESTALE",
        NFS3ERR_NOTSUPP => "ENOTSUP",
        NFS3ERR_SERVERFAULT => "EIO",
        _ => return format!("nfsstat3 {status}"),
    };
    format!("NFS3ERR({name})")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NfsTime3 {
    pub seconds: u32,
    pub nseconds: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpecData3 {
    pub major: u32,
    pub minor: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fattr3 {
    pub r#type: u32,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub used: u64,
    pub rdev: SpecData3,
    pub fsid: u64,
    pub fileid: u64,
    pub atime: NfsTime3,
    pub mtime: NfsTime3,
    pub ctime: NfsTime3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WccAttr {
    pub size: u64,
    pub mtime: NfsTime3,
    pub ctime: NfsTime3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WccData {
    pub before: Option<WccAttr>,
    pub after: Option<Fattr3>,
}

pub fn write_time(writer: &mut XdrWriter, time: NfsTime3) {
    writer.u32(time.seconds);
    writer.u32(time.nseconds);
}

pub fn read_time(reader: &mut XdrReader<'_>, what: &str) -> Result<NfsTime3, XdrError> {
    Ok(NfsTime3 {
        seconds: reader.u32(&format!("{what}.seconds"))?,
        nseconds: reader.u32(&format!("{what}.nseconds"))?,
    })
}

pub fn write_spec_data(writer: &mut XdrWriter, spec: SpecData3) {
    writer.u32(spec.major);
    writer.u32(spec.minor);
}

pub fn read_spec_data(reader: &mut XdrReader<'_>) -> Result<SpecData3, XdrError> {
    Ok(SpecData3 {
        major: reader.u32("specdata3.major")?,
        minor: reader.u32("specdata3.minor")?,
    })
}

pub fn write_fattr(writer: &mut XdrWriter, attr: &Fattr3) {
    writer.u32(attr.r#type);
    writer.u32(attr.mode);
    writer.u32(attr.nlink);
    writer.u32(attr.uid);
    writer.u32(attr.gid);
    writer.u64(attr.size);
    writer.u64(attr.used);
    write_spec_data(writer, attr.rdev);
    writer.u64(attr.fsid);
    writer.u64(attr.fileid);
    write_time(writer, attr.atime);
    write_time(writer, attr.mtime);
    write_time(writer, attr.ctime);
}

pub fn read_fattr(reader: &mut XdrReader<'_>) -> Result<Fattr3, XdrError> {
    Ok(Fattr3 {
        r#type: reader.u32("fattr3.type")?,
        mode: reader.u32("fattr3.mode")?,
        nlink: reader.u32("fattr3.nlink")?,
        uid: reader.u32("fattr3.uid")?,
        gid: reader.u32("fattr3.gid")?,
        size: reader.u64("fattr3.size")?,
        used: reader.u64("fattr3.used")?,
        rdev: read_spec_data(reader)?,
        fsid: reader.u64("fattr3.fsid")?,
        fileid: reader.u64("fattr3.fileid")?,
        atime: read_time(reader, "fattr3.atime")?,
        mtime: read_time(reader, "fattr3.mtime")?,
        ctime: read_time(reader, "fattr3.ctime")?,
    })
}

pub fn write_post_op_attr(writer: &mut XdrWriter, attr: Option<&Fattr3>) {
    writer.bool(attr.is_some());
    if let Some(attr) = attr {
        write_fattr(writer, attr);
    }
}

pub fn read_post_op_attr(reader: &mut XdrReader<'_>) -> Result<Option<Fattr3>, XdrError> {
    reader.optional("post_op_attr", read_fattr)
}

pub fn write_post_op_fh(writer: &mut XdrWriter, handle: Option<&[u8]>) {
    writer.bool(handle.is_some());
    if let Some(handle) = handle {
        writer.var_opaque(handle);
    }
}

pub fn read_post_op_fh(reader: &mut XdrReader<'_>) -> Result<Option<Vec<u8>>, XdrError> {
    reader.optional("post_op_fh3", |reader| {
        reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")
    })
}

pub fn write_wcc_data(writer: &mut XdrWriter, wcc: &WccData) {
    writer.bool(wcc.before.is_some());
    if let Some(before) = &wcc.before {
        writer.u64(before.size);
        write_time(writer, before.mtime);
        write_time(writer, before.ctime);
    }
    write_post_op_attr(writer, wcc.after.as_ref());
}

pub fn read_wcc_data(reader: &mut XdrReader<'_>) -> Result<WccData, XdrError> {
    let before = reader.optional("pre_op_attr", |reader| {
        Ok(WccAttr {
            size: reader.u64("wcc_attr.size")?,
            mtime: read_time(reader, "wcc_attr.mtime")?,
            ctime: read_time(reader, "wcc_attr.ctime")?,
        })
    })?;
    Ok(WccData {
        before,
        after: read_post_op_attr(reader)?,
    })
}

pub fn wcc_attr_of(stats: &Stats) -> WccAttr {
    WccAttr {
        size: stats.size,
        mtime: to_time(stats.mtime_ms),
        ctime: to_time(stats.ctime_ms),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SetTime3 {
    pub how: u32,
    pub time: Option<NfsTime3>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sattr3 {
    pub mode: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
    pub size: Option<u64>,
    pub atime: SetTime3,
    pub mtime: SetTime3,
}

fn write_set_time(writer: &mut XdrWriter, value: &SetTime3) {
    writer.u32(value.how);
    if value.how == SET_TO_CLIENT_TIME {
        write_time(
            writer,
            value.time.unwrap_or(NfsTime3 {
                seconds: 0,
                nseconds: 0,
            }),
        );
    }
}

fn read_set_time(reader: &mut XdrReader<'_>, what: &str) -> Result<SetTime3, XdrError> {
    let how = reader.u32(&format!("{what}.how"))?;
    let time = if how == SET_TO_CLIENT_TIME {
        Some(read_time(reader, what)?)
    } else {
        None
    };
    Ok(SetTime3 { how, time })
}

pub fn write_sattr(writer: &mut XdrWriter, attr: &Sattr3) {
    writer.optional(attr.mode.as_ref(), |writer, value| writer.u32(*value));
    writer.optional(attr.uid.as_ref(), |writer, value| writer.u32(*value));
    writer.optional(attr.gid.as_ref(), |writer, value| writer.u32(*value));
    writer.optional(attr.size.as_ref(), |writer, value| writer.u64(*value));
    write_set_time(writer, &attr.atime);
    write_set_time(writer, &attr.mtime);
}

pub fn read_sattr(reader: &mut XdrReader<'_>) -> Result<Sattr3, XdrError> {
    Ok(Sattr3 {
        mode: reader.optional("set_mode3", |reader| reader.u32("sattr3.mode"))?,
        uid: reader.optional("set_uid3", |reader| reader.u32("sattr3.uid"))?,
        gid: reader.optional("set_gid3", |reader| reader.u32("sattr3.gid"))?,
        size: reader.optional("set_size3", |reader| reader.u64("sattr3.size"))?,
        atime: read_set_time(reader, "sattr3.atime")?,
        mtime: read_set_time(reader, "sattr3.mtime")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirOpArgs {
    pub dir: Vec<u8>,
    pub name: String,
}

pub fn write_dir_op(writer: &mut XdrWriter, args: &DirOpArgs) {
    writer.var_opaque(&args.dir);
    writer.string(&args.name);
}

pub fn read_dir_op(reader: &mut XdrReader<'_>) -> Result<DirOpArgs, XdrError> {
    Ok(DirOpArgs {
        dir: reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?,
        name: reader.string(MNT3_NAMELEN, "filename3")?,
    })
}

pub fn to_time(ms: i64) -> NfsTime3 {
    if ms < 0 {
        return NfsTime3 {
            seconds: 0,
            nseconds: 0,
        };
    }
    let seconds = (ms / 1000).min(u32::MAX as i64) as u32;
    let nseconds = ((ms % 1000) * 1_000_000).clamp(0, 999_999_999) as u32;
    NfsTime3 { seconds, nseconds }
}

pub fn from_time(time: NfsTime3) -> i64 {
    i64::from(time.seconds) * 1000 + i64::from(time.nseconds) / 1_000_000
}

pub fn ftype_of(mode: u32) -> u32 {
    match mode & mount_rs_core::types::S_IFMT {
        mount_rs_core::types::S_IFDIR => NF3DIR,
        mount_rs_core::types::S_IFLNK => NF3LNK,
        mount_rs_core::types::S_IFBLK => NF3BLK,
        mount_rs_core::types::S_IFCHR => NF3CHR,
        mount_rs_core::types::S_IFSOCK => NF3SOCK,
        mount_rs_core::types::S_IFIFO => NF3FIFO,
        _ => NF3REG,
    }
}

pub fn mode_type_of(kind: u32) -> u32 {
    match kind {
        NF3DIR => mount_rs_core::types::S_IFDIR,
        NF3LNK => mount_rs_core::types::S_IFLNK,
        NF3BLK => mount_rs_core::types::S_IFBLK,
        NF3CHR => mount_rs_core::types::S_IFCHR,
        NF3SOCK => mount_rs_core::types::S_IFSOCK,
        NF3FIFO => mount_rs_core::types::S_IFIFO,
        _ => mount_rs_core::types::S_IFREG,
    }
}

pub fn fattr_of(stats: &Stats, fileid: u64) -> Fattr3 {
    Fattr3 {
        r#type: ftype_of(stats.mode),
        mode: stats.mode & 0o7777,
        nlink: stats.nlink.min(u32::MAX as u64) as u32,
        uid: stats.uid,
        gid: stats.gid,
        size: stats.size,
        used: if stats.blocks > 0 {
            stats.blocks.saturating_mul(512)
        } else {
            stats.size
        },
        rdev: SpecData3 {
            major: ((stats.rdev >> 8) & 0x00ff_ffff) as u32,
            minor: (stats.rdev & 0xff) as u32,
        },
        fsid: stats.dev,
        fileid,
        atime: to_time(stats.atime_ms),
        mtime: to_time(stats.mtime_ms),
        ctime: to_time(stats.ctime_ms),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WccRes {
    pub status: u32,
    pub wcc: WccData,
}

pub fn write_wcc_res(writer: &mut XdrWriter, res: &WccRes) {
    writer.u32(res.status);
    write_wcc_data(writer, &res.wcc);
}

pub fn read_wcc_res(reader: &mut XdrReader<'_>) -> Result<WccRes, XdrError> {
    Ok(WccRes {
        status: reader.u32("nfsstat3")?,
        wcc: read_wcc_data(reader)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRes {
    pub status: u32,
    pub obj: Option<Vec<u8>>,
    pub obj_attributes: Option<Fattr3>,
    pub dir_wcc: WccData,
}

pub fn write_create_res(writer: &mut XdrWriter, res: &CreateRes) {
    writer.u32(res.status);
    if res.status == NFS3_OK {
        write_post_op_fh(writer, res.obj.as_deref());
        write_post_op_attr(writer, res.obj_attributes.as_ref());
    }
    write_wcc_data(writer, &res.dir_wcc);
}

pub fn read_create_res(reader: &mut XdrReader<'_>) -> Result<CreateRes, XdrError> {
    let status = reader.u32("nfsstat3")?;
    if status != NFS3_OK {
        return Ok(CreateRes {
            status,
            obj: None,
            obj_attributes: None,
            dir_wcc: read_wcc_data(reader)?,
        });
    }
    Ok(CreateRes {
        status,
        obj: read_post_op_fh(reader)?,
        obj_attributes: read_post_op_attr(reader)?,
        dir_wcc: read_wcc_data(reader)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Getattr3res {
    pub status: u32,
    pub attributes: Option<Fattr3>,
}

pub fn write_getattr_res(writer: &mut XdrWriter, res: &Getattr3res) {
    writer.u32(res.status);
    if res.status == NFS3_OK {
        write_fattr(
            writer,
            res.attributes.as_ref().expect("successful GETATTR attr"),
        );
    }
}

pub fn read_getattr_res(reader: &mut XdrReader<'_>) -> Result<Getattr3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    Ok(Getattr3res {
        status,
        attributes: if status == NFS3_OK {
            Some(read_fattr(reader)?)
        } else {
            None
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setattr3args {
    pub object: Vec<u8>,
    pub attributes: Sattr3,
    pub guard: Option<NfsTime3>,
}

pub fn write_setattr_args(writer: &mut XdrWriter, args: &Setattr3args) {
    writer.var_opaque(&args.object);
    write_sattr(writer, &args.attributes);
    writer.optional(args.guard.as_ref(), |writer, value| {
        write_time(writer, *value)
    });
}

pub fn read_setattr_args(reader: &mut XdrReader<'_>) -> Result<Setattr3args, XdrError> {
    Ok(Setattr3args {
        object: reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?,
        attributes: read_sattr(reader)?,
        guard: reader.optional("sattr_guard3", |reader| read_time(reader, "guard ctime"))?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lookup3res {
    pub status: u32,
    pub object: Option<Vec<u8>>,
    pub obj_attributes: Option<Fattr3>,
    pub dir_attributes: Option<Fattr3>,
}

pub fn write_lookup_res(writer: &mut XdrWriter, res: &Lookup3res) {
    writer.u32(res.status);
    if res.status == NFS3_OK {
        writer.var_opaque(res.object.as_ref().expect("successful LOOKUP handle"));
        write_post_op_attr(writer, res.obj_attributes.as_ref());
    }
    write_post_op_attr(writer, res.dir_attributes.as_ref());
}

pub fn read_lookup_res(reader: &mut XdrReader<'_>) -> Result<Lookup3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    if status != NFS3_OK {
        return Ok(Lookup3res {
            status,
            object: None,
            obj_attributes: None,
            dir_attributes: read_post_op_attr(reader)?,
        });
    }
    Ok(Lookup3res {
        status,
        object: Some(reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?),
        obj_attributes: read_post_op_attr(reader)?,
        dir_attributes: read_post_op_attr(reader)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access3args {
    pub object: Vec<u8>,
    pub access: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Access3res {
    pub status: u32,
    pub attributes: Option<Fattr3>,
    pub access: u32,
}

pub fn write_access_args(writer: &mut XdrWriter, args: &Access3args) {
    writer.var_opaque(&args.object);
    writer.u32(args.access);
}

pub fn read_access_args(reader: &mut XdrReader<'_>) -> Result<Access3args, XdrError> {
    Ok(Access3args {
        object: reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?,
        access: reader.u32("access")?,
    })
}

pub fn write_access_res(writer: &mut XdrWriter, res: &Access3res) {
    writer.u32(res.status);
    write_post_op_attr(writer, res.attributes.as_ref());
    if res.status == NFS3_OK {
        writer.u32(res.access);
    }
}

pub fn read_access_res(reader: &mut XdrReader<'_>) -> Result<Access3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let attributes = read_post_op_attr(reader)?;
    let access = if status == NFS3_OK {
        reader.u32("access")?
    } else {
        0
    };
    Ok(Access3res {
        status,
        attributes,
        access,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readlink3res {
    pub status: u32,
    pub attributes: Option<Fattr3>,
    pub target: Option<String>,
}

pub fn write_readlink_res(writer: &mut XdrWriter, res: &Readlink3res) {
    writer.u32(res.status);
    write_post_op_attr(writer, res.attributes.as_ref());
    if res.status == NFS3_OK {
        writer.string(res.target.as_deref().unwrap_or_default());
    }
}

pub fn read_readlink_res(reader: &mut XdrReader<'_>) -> Result<Readlink3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let attributes = read_post_op_attr(reader)?;
    Ok(Readlink3res {
        status,
        attributes,
        target: if status == NFS3_OK {
            Some(reader.string(4096, "nfspath3")?)
        } else {
            None
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read3args {
    pub file: Vec<u8>,
    pub offset: u64,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Read3res {
    pub status: u32,
    pub attributes: Option<Fattr3>,
    pub count: u32,
    pub eof: bool,
    pub data: Vec<u8>,
}

pub fn write_read_args(writer: &mut XdrWriter, args: &Read3args) {
    writer.var_opaque(&args.file);
    writer.u64(args.offset);
    writer.u32(args.count);
}

pub fn read_read_args(reader: &mut XdrReader<'_>) -> Result<Read3args, XdrError> {
    Ok(Read3args {
        file: reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?,
        offset: reader.u64("offset3")?,
        count: reader.u32("count3")?,
    })
}

pub fn write_read_res(writer: &mut XdrWriter, res: &Read3res) {
    writer.u32(res.status);
    write_post_op_attr(writer, res.attributes.as_ref());
    if res.status == NFS3_OK {
        writer.u32(res.count);
        writer.bool(res.eof);
        writer.var_opaque(&res.data);
    }
}

pub fn read_read_res(reader: &mut XdrReader<'_>, max: usize) -> Result<Read3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let attributes = read_post_op_attr(reader)?;
    if status != NFS3_OK {
        return Ok(Read3res {
            status,
            attributes,
            count: 0,
            eof: false,
            data: Vec::new(),
        });
    }
    let count = reader.u32("count3")?;
    let eof = reader.bool("eof")?;
    let data = reader.var_opaque(max, "read data")?;
    if data.len() != count as usize {
        return Err(XdrError::new(
            format!("READ says {count} bytes but carries {}", data.len()),
            reader.offset(),
        ));
    }
    Ok(Read3res {
        status,
        attributes,
        count,
        eof,
        data,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write3args {
    pub file: Vec<u8>,
    pub offset: u64,
    pub count: u32,
    pub stable: u32,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Write3res {
    pub status: u32,
    pub wcc: WccData,
    pub count: u32,
    pub committed: u32,
    pub verf: Vec<u8>,
}

pub fn write_write_args(writer: &mut XdrWriter, args: &Write3args) {
    writer.var_opaque(&args.file);
    writer.u64(args.offset);
    writer.u32(args.count);
    writer.u32(args.stable);
    writer.var_opaque(&args.data);
}

pub fn read_write_args(reader: &mut XdrReader<'_>, max: usize) -> Result<Write3args, XdrError> {
    let args = Write3args {
        file: reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?,
        offset: reader.u64("offset3")?,
        count: reader.u32("count3")?,
        stable: reader.u32("stable_how")?,
        data: reader.var_opaque(max, "write data")?,
    };
    if args.data.len() != args.count as usize {
        return Err(XdrError::new(
            format!(
                "WRITE says {} bytes but carries {}",
                args.count,
                args.data.len()
            ),
            reader.offset(),
        ));
    }
    Ok(args)
}

pub fn write_write_res(writer: &mut XdrWriter, res: &Write3res) {
    writer.u32(res.status);
    write_wcc_data(writer, &res.wcc);
    if res.status == NFS3_OK {
        writer.u32(res.count);
        writer.u32(res.committed);
        writer.fixed_opaque(&res.verf, NFS3_WRITEVERFSIZE);
    }
}

pub fn read_write_res(reader: &mut XdrReader<'_>) -> Result<Write3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let wcc = read_wcc_data(reader)?;
    if status != NFS3_OK {
        return Ok(Write3res {
            status,
            wcc,
            count: 0,
            committed: UNSTABLE,
            verf: vec![0; NFS3_WRITEVERFSIZE],
        });
    }
    Ok(Write3res {
        status,
        wcc,
        count: reader.u32("count3")?,
        committed: reader.u32("committed")?,
        verf: reader.fixed_opaque(NFS3_WRITEVERFSIZE, "writeverf3")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Create3args {
    pub where_: DirOpArgs,
    pub mode: u32,
    pub attributes: Option<Sattr3>,
    pub verf: Option<Vec<u8>>,
}

pub fn write_create_args(writer: &mut XdrWriter, args: &Create3args) {
    write_dir_op(writer, &args.where_);
    writer.u32(args.mode);
    if args.mode == CREATE_EXCLUSIVE {
        writer.fixed_opaque(
            args.verf.as_deref().unwrap_or(&[0; NFS3_CREATEVERFSIZE]),
            NFS3_CREATEVERFSIZE,
        );
    } else {
        write_sattr(
            writer,
            args.attributes.as_ref().unwrap_or(&Sattr3::default()),
        );
    }
}

pub fn read_create_args(reader: &mut XdrReader<'_>) -> Result<Create3args, XdrError> {
    let where_ = read_dir_op(reader)?;
    let mode = reader.u32("createmode3")?;
    if mode == CREATE_EXCLUSIVE {
        Ok(Create3args {
            where_,
            mode,
            attributes: None,
            verf: Some(reader.fixed_opaque(NFS3_CREATEVERFSIZE, "createverf3")?),
        })
    } else {
        Ok(Create3args {
            where_,
            mode,
            attributes: Some(read_sattr(reader)?),
            verf: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mkdir3args {
    pub where_: DirOpArgs,
    pub attributes: Sattr3,
}

pub fn write_mkdir_args(writer: &mut XdrWriter, args: &Mkdir3args) {
    write_dir_op(writer, &args.where_);
    write_sattr(writer, &args.attributes);
}

pub fn read_mkdir_args(reader: &mut XdrReader<'_>) -> Result<Mkdir3args, XdrError> {
    Ok(Mkdir3args {
        where_: read_dir_op(reader)?,
        attributes: read_sattr(reader)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symlink3args {
    pub where_: DirOpArgs,
    pub attributes: Sattr3,
    pub target: String,
}

pub fn write_symlink_args(writer: &mut XdrWriter, args: &Symlink3args) {
    write_dir_op(writer, &args.where_);
    write_sattr(writer, &args.attributes);
    writer.string(&args.target);
}

pub fn read_symlink_args(reader: &mut XdrReader<'_>) -> Result<Symlink3args, XdrError> {
    Ok(Symlink3args {
        where_: read_dir_op(reader)?,
        attributes: read_sattr(reader)?,
        target: reader.string(4096, "nfspath3")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mknod3args {
    pub where_: DirOpArgs,
    pub r#type: u32,
    pub attributes: Option<Sattr3>,
    pub spec: Option<SpecData3>,
}

pub fn write_mknod_args(writer: &mut XdrWriter, args: &Mknod3args) {
    write_dir_op(writer, &args.where_);
    writer.u32(args.r#type);
    if args.r#type == NF3CHR || args.r#type == NF3BLK {
        write_sattr(
            writer,
            args.attributes.as_ref().unwrap_or(&Sattr3::default()),
        );
        write_spec_data(
            writer,
            args.spec.unwrap_or(SpecData3 { major: 0, minor: 0 }),
        );
    } else if args.r#type == NF3SOCK || args.r#type == NF3FIFO {
        write_sattr(
            writer,
            args.attributes.as_ref().unwrap_or(&Sattr3::default()),
        );
    }
}

pub fn read_mknod_args(reader: &mut XdrReader<'_>) -> Result<Mknod3args, XdrError> {
    let where_ = read_dir_op(reader)?;
    let r#type = reader.u32("ftype3")?;
    if r#type == NF3CHR || r#type == NF3BLK {
        Ok(Mknod3args {
            where_,
            r#type,
            attributes: Some(read_sattr(reader)?),
            spec: Some(read_spec_data(reader)?),
        })
    } else if r#type == NF3SOCK || r#type == NF3FIFO {
        Ok(Mknod3args {
            where_,
            r#type,
            attributes: Some(read_sattr(reader)?),
            spec: None,
        })
    } else {
        Ok(Mknod3args {
            where_,
            r#type,
            attributes: None,
            spec: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename3args {
    pub from: DirOpArgs,
    pub to: DirOpArgs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rename3res {
    pub status: u32,
    pub from_wcc: WccData,
    pub to_wcc: WccData,
}

pub fn write_rename_args(writer: &mut XdrWriter, args: &Rename3args) {
    write_dir_op(writer, &args.from);
    write_dir_op(writer, &args.to);
}

pub fn read_rename_args(reader: &mut XdrReader<'_>) -> Result<Rename3args, XdrError> {
    Ok(Rename3args {
        from: read_dir_op(reader)?,
        to: read_dir_op(reader)?,
    })
}

pub fn write_rename_res(writer: &mut XdrWriter, res: &Rename3res) {
    writer.u32(res.status);
    write_wcc_data(writer, &res.from_wcc);
    write_wcc_data(writer, &res.to_wcc);
}

pub fn read_rename_res(reader: &mut XdrReader<'_>) -> Result<Rename3res, XdrError> {
    Ok(Rename3res {
        status: reader.u32("nfsstat3")?,
        from_wcc: read_wcc_data(reader)?,
        to_wcc: read_wcc_data(reader)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link3args {
    pub file: Vec<u8>,
    pub link: DirOpArgs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link3res {
    pub status: u32,
    pub attributes: Option<Fattr3>,
    pub linkdir_wcc: WccData,
}

pub fn write_link_args(writer: &mut XdrWriter, args: &Link3args) {
    writer.var_opaque(&args.file);
    write_dir_op(writer, &args.link);
}

pub fn read_link_args(reader: &mut XdrReader<'_>) -> Result<Link3args, XdrError> {
    Ok(Link3args {
        file: reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?,
        link: read_dir_op(reader)?,
    })
}

pub fn write_link_res(writer: &mut XdrWriter, res: &Link3res) {
    writer.u32(res.status);
    write_post_op_attr(writer, res.attributes.as_ref());
    write_wcc_data(writer, &res.linkdir_wcc);
}

pub fn read_link_res(reader: &mut XdrReader<'_>) -> Result<Link3res, XdrError> {
    Ok(Link3res {
        status: reader.u32("nfsstat3")?,
        attributes: read_post_op_attr(reader)?,
        linkdir_wcc: read_wcc_data(reader)?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readdir3args {
    pub dir: Vec<u8>,
    pub cookie: u64,
    pub cookieverf: Vec<u8>,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry3 {
    pub fileid: u64,
    pub name: String,
    pub cookie: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readdir3res {
    pub status: u32,
    pub dir_attributes: Option<Fattr3>,
    pub cookieverf: Vec<u8>,
    pub entries: Vec<Entry3>,
    pub eof: bool,
}

pub fn write_readdir_args(writer: &mut XdrWriter, args: &Readdir3args) {
    writer.var_opaque(&args.dir);
    writer.u64(args.cookie);
    writer.fixed_opaque(&args.cookieverf, NFS3_COOKIEVERFSIZE);
    writer.u32(args.count);
}

pub fn read_readdir_args(reader: &mut XdrReader<'_>) -> Result<Readdir3args, XdrError> {
    Ok(Readdir3args {
        dir: reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?,
        cookie: reader.u64("cookie3")?,
        cookieverf: reader.fixed_opaque(NFS3_COOKIEVERFSIZE, "cookieverf3")?,
        count: reader.u32("count3")?,
    })
}

pub fn write_readdir_res(writer: &mut XdrWriter, res: &Readdir3res) {
    writer.u32(res.status);
    write_post_op_attr(writer, res.dir_attributes.as_ref());
    if res.status != NFS3_OK {
        return;
    }
    writer.fixed_opaque(&res.cookieverf, NFS3_COOKIEVERFSIZE);
    writer.list(&res.entries, |writer, entry| {
        writer.u64(entry.fileid);
        writer.string(&entry.name);
        writer.u64(entry.cookie);
    });
    writer.bool(res.eof);
}

pub fn read_readdir_res(reader: &mut XdrReader<'_>) -> Result<Readdir3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let dir_attributes = read_post_op_attr(reader)?;
    if status != NFS3_OK {
        return Ok(Readdir3res {
            status,
            dir_attributes,
            cookieverf: vec![0; NFS3_COOKIEVERFSIZE],
            entries: Vec::new(),
            eof: false,
        });
    }
    let cookieverf = reader.fixed_opaque(NFS3_COOKIEVERFSIZE, "cookieverf3")?;
    let entries = reader.list(1 << 20, "dirlist3", |reader| {
        Ok(Entry3 {
            fileid: reader.u64("entry3.fileid")?,
            name: reader.string(1024, "entry3.name")?,
            cookie: reader.u64("entry3.cookie")?,
        })
    })?;
    Ok(Readdir3res {
        status,
        dir_attributes,
        cookieverf,
        entries,
        eof: reader.bool("eof")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readdirplus3args {
    pub dir: Vec<u8>,
    pub cookie: u64,
    pub cookieverf: Vec<u8>,
    pub dircount: u32,
    pub maxcount: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryPlus3 {
    pub fileid: u64,
    pub name: String,
    pub cookie: u64,
    pub attributes: Option<Fattr3>,
    pub handle: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readdirplus3res {
    pub status: u32,
    pub dir_attributes: Option<Fattr3>,
    pub cookieverf: Vec<u8>,
    pub entries: Vec<EntryPlus3>,
    pub eof: bool,
}

pub fn write_readdirplus_args(writer: &mut XdrWriter, args: &Readdirplus3args) {
    writer.var_opaque(&args.dir);
    writer.u64(args.cookie);
    writer.fixed_opaque(&args.cookieverf, NFS3_COOKIEVERFSIZE);
    writer.u32(args.dircount);
    writer.u32(args.maxcount);
}

pub fn read_readdirplus_args(reader: &mut XdrReader<'_>) -> Result<Readdirplus3args, XdrError> {
    Ok(Readdirplus3args {
        dir: reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?,
        cookie: reader.u64("cookie3")?,
        cookieverf: reader.fixed_opaque(NFS3_COOKIEVERFSIZE, "cookieverf3")?,
        dircount: reader.u32("dircount")?,
        maxcount: reader.u32("maxcount")?,
    })
}

pub fn write_readdirplus_res(writer: &mut XdrWriter, res: &Readdirplus3res) {
    writer.u32(res.status);
    write_post_op_attr(writer, res.dir_attributes.as_ref());
    if res.status != NFS3_OK {
        return;
    }
    writer.fixed_opaque(&res.cookieverf, NFS3_COOKIEVERFSIZE);
    writer.list(&res.entries, |writer, entry| {
        writer.u64(entry.fileid);
        writer.string(&entry.name);
        writer.u64(entry.cookie);
        write_post_op_attr(writer, entry.attributes.as_ref());
        write_post_op_fh(writer, entry.handle.as_deref());
    });
    writer.bool(res.eof);
}

pub fn read_readdirplus_res(reader: &mut XdrReader<'_>) -> Result<Readdirplus3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let dir_attributes = read_post_op_attr(reader)?;
    if status != NFS3_OK {
        return Ok(Readdirplus3res {
            status,
            dir_attributes,
            cookieverf: vec![0; NFS3_COOKIEVERFSIZE],
            entries: Vec::new(),
            eof: false,
        });
    }
    let cookieverf = reader.fixed_opaque(NFS3_COOKIEVERFSIZE, "cookieverf3")?;
    let entries = reader.list(1 << 20, "dirlistplus3", |reader| {
        Ok(EntryPlus3 {
            fileid: reader.u64("entryplus3.fileid")?,
            name: reader.string(1024, "entryplus3.name")?,
            cookie: reader.u64("entryplus3.cookie")?,
            attributes: read_post_op_attr(reader)?,
            handle: read_post_op_fh(reader)?,
        })
    })?;
    Ok(Readdirplus3res {
        status,
        dir_attributes,
        cookieverf,
        entries,
        eof: reader.bool("eof")?,
    })
}

pub const FATTR3_SIZE: usize = 84;

pub fn entry_size(name_bytes: usize) -> usize {
    4 + 8 + 4 + xdr_align(name_bytes) + 8
}

pub fn entry_plus_size(name_bytes: usize, fh_bytes: usize, has_attrs: bool) -> usize {
    entry_size(name_bytes)
        + if has_attrs { 4 + FATTR3_SIZE } else { 4 }
        + 4
        + 4
        + xdr_align(fh_bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fsstat3res {
    pub status: u32,
    pub attributes: Option<Fattr3>,
    pub tbytes: u64,
    pub fbytes: u64,
    pub abytes: u64,
    pub tfiles: u64,
    pub ffiles: u64,
    pub afiles: u64,
    pub invarsec: u32,
}

pub fn write_fsstat_res(writer: &mut XdrWriter, res: &Fsstat3res) {
    writer.u32(res.status);
    write_post_op_attr(writer, res.attributes.as_ref());
    if res.status == NFS3_OK {
        writer.u64(res.tbytes);
        writer.u64(res.fbytes);
        writer.u64(res.abytes);
        writer.u64(res.tfiles);
        writer.u64(res.ffiles);
        writer.u64(res.afiles);
        writer.u32(res.invarsec);
    }
}

pub fn read_fsstat_res(reader: &mut XdrReader<'_>) -> Result<Fsstat3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let attributes = read_post_op_attr(reader)?;
    if status != NFS3_OK {
        return Ok(Fsstat3res {
            status,
            attributes,
            tbytes: 0,
            fbytes: 0,
            abytes: 0,
            tfiles: 0,
            ffiles: 0,
            afiles: 0,
            invarsec: 0,
        });
    }
    Ok(Fsstat3res {
        status,
        attributes,
        tbytes: reader.u64("tbytes")?,
        fbytes: reader.u64("fbytes")?,
        abytes: reader.u64("abytes")?,
        tfiles: reader.u64("tfiles")?,
        ffiles: reader.u64("ffiles")?,
        afiles: reader.u64("afiles")?,
        invarsec: reader.u32("invarsec")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fsinfo3res {
    pub status: u32,
    pub attributes: Option<Fattr3>,
    pub rtmax: u32,
    pub rtpref: u32,
    pub rtmult: u32,
    pub wtmax: u32,
    pub wtpref: u32,
    pub wtmult: u32,
    pub dtpref: u32,
    pub maxfilesize: u64,
    pub time_delta: NfsTime3,
    pub properties: u32,
}

pub fn write_fsinfo_res(writer: &mut XdrWriter, res: &Fsinfo3res) {
    writer.u32(res.status);
    write_post_op_attr(writer, res.attributes.as_ref());
    if res.status == NFS3_OK {
        writer.u32(res.rtmax);
        writer.u32(res.rtpref);
        writer.u32(res.rtmult);
        writer.u32(res.wtmax);
        writer.u32(res.wtpref);
        writer.u32(res.wtmult);
        writer.u32(res.dtpref);
        writer.u64(res.maxfilesize);
        write_time(writer, res.time_delta);
        writer.u32(res.properties);
    }
}

pub fn read_fsinfo_res(reader: &mut XdrReader<'_>) -> Result<Fsinfo3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let attributes = read_post_op_attr(reader)?;
    if status != NFS3_OK {
        return Ok(Fsinfo3res {
            status,
            attributes,
            rtmax: 0,
            rtpref: 0,
            rtmult: 0,
            wtmax: 0,
            wtpref: 0,
            wtmult: 0,
            dtpref: 0,
            maxfilesize: 0,
            time_delta: NfsTime3 {
                seconds: 0,
                nseconds: 0,
            },
            properties: 0,
        });
    }
    Ok(Fsinfo3res {
        status,
        attributes,
        rtmax: reader.u32("rtmax")?,
        rtpref: reader.u32("rtpref")?,
        rtmult: reader.u32("rtmult")?,
        wtmax: reader.u32("wtmax")?,
        wtpref: reader.u32("wtpref")?,
        wtmult: reader.u32("wtmult")?,
        dtpref: reader.u32("dtpref")?,
        maxfilesize: reader.u64("maxfilesize")?,
        time_delta: read_time(reader, "time_delta")?,
        properties: reader.u32("properties")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pathconf3res {
    pub status: u32,
    pub attributes: Option<Fattr3>,
    pub linkmax: u32,
    pub name_max: u32,
    pub no_trunc: bool,
    pub chown_restricted: bool,
    pub case_insensitive: bool,
    pub case_preserving: bool,
}

pub fn write_pathconf_res(writer: &mut XdrWriter, res: &Pathconf3res) {
    writer.u32(res.status);
    write_post_op_attr(writer, res.attributes.as_ref());
    if res.status == NFS3_OK {
        writer.u32(res.linkmax);
        writer.u32(res.name_max);
        writer.bool(res.no_trunc);
        writer.bool(res.chown_restricted);
        writer.bool(res.case_insensitive);
        writer.bool(res.case_preserving);
    }
}

pub fn read_pathconf_res(reader: &mut XdrReader<'_>) -> Result<Pathconf3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let attributes = read_post_op_attr(reader)?;
    if status != NFS3_OK {
        return Ok(Pathconf3res {
            status,
            attributes,
            linkmax: 0,
            name_max: 0,
            no_trunc: false,
            chown_restricted: false,
            case_insensitive: false,
            case_preserving: false,
        });
    }
    Ok(Pathconf3res {
        status,
        attributes,
        linkmax: reader.u32("linkmax")?,
        name_max: reader.u32("name_max")?,
        no_trunc: reader.bool("no_trunc")?,
        chown_restricted: reader.bool("chown_restricted")?,
        case_insensitive: reader.bool("case_insensitive")?,
        case_preserving: reader.bool("case_preserving")?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit3args {
    pub file: Vec<u8>,
    pub offset: u64,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit3res {
    pub status: u32,
    pub wcc: WccData,
    pub verf: Vec<u8>,
}

pub fn write_commit_args(writer: &mut XdrWriter, args: &Commit3args) {
    writer.var_opaque(&args.file);
    writer.u64(args.offset);
    writer.u32(args.count);
}

pub fn read_commit_args(reader: &mut XdrReader<'_>) -> Result<Commit3args, XdrError> {
    Ok(Commit3args {
        file: reader.var_opaque(NFS3_FHSIZE, "nfs_fh3")?,
        offset: reader.u64("offset3")?,
        count: reader.u32("count3")?,
    })
}

pub fn write_commit_res(writer: &mut XdrWriter, res: &Commit3res) {
    writer.u32(res.status);
    write_wcc_data(writer, &res.wcc);
    if res.status == NFS3_OK {
        writer.fixed_opaque(&res.verf, NFS3_WRITEVERFSIZE);
    }
}

pub fn read_commit_res(reader: &mut XdrReader<'_>) -> Result<Commit3res, XdrError> {
    let status = reader.u32("nfsstat3")?;
    let wcc = read_wcc_data(reader)?;
    Ok(Commit3res {
        status,
        wcc,
        verf: if status == NFS3_OK {
            reader.fixed_opaque(NFS3_WRITEVERFSIZE, "writeverf3")?
        } else {
            vec![0; NFS3_WRITEVERFSIZE]
        },
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mountres3 {
    pub status: u32,
    pub fh: Option<Vec<u8>>,
    pub auth_flavors: Vec<u32>,
}

pub fn write_mount_res(writer: &mut XdrWriter, res: &Mountres3) {
    writer.u32(res.status);
    if res.status == MNT3_OK {
        writer.var_opaque(res.fh.as_ref().expect("successful MNT handle"));
        writer.array(&res.auth_flavors, |writer, flavor| writer.u32(*flavor));
    }
}

pub fn read_mount_res(reader: &mut XdrReader<'_>) -> Result<Mountres3, XdrError> {
    let status = reader.u32("mountstat3")?;
    if status != MNT3_OK {
        return Ok(Mountres3 {
            status,
            fh: None,
            auth_flavors: Vec::new(),
        });
    }
    Ok(Mountres3 {
        status,
        fh: Some(reader.var_opaque(MNT3_FHSIZE, "fhandle3")?),
        auth_flavors: reader.array(64, "auth_flavors", |reader| reader.u32("auth_flavor"))?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountEntry3 {
    pub hostname: String,
    pub directory: String,
}

pub fn write_mount_list(writer: &mut XdrWriter, entries: &[MountEntry3]) {
    writer.list(entries, |writer, entry| {
        writer.string(&entry.hostname);
        writer.string(&entry.directory);
    });
}

pub fn read_mount_list(reader: &mut XdrReader<'_>) -> Result<Vec<MountEntry3>, XdrError> {
    reader.list(1 << 16, "mountlist", |reader| {
        Ok(MountEntry3 {
            hostname: reader.string(255, "ml_hostname")?,
            directory: reader.string(MNT3_PATHLEN, "ml_directory")?,
        })
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportEntry3 {
    pub directory: String,
    pub groups: Vec<String>,
}

pub fn write_export_list(writer: &mut XdrWriter, entries: &[ExportEntry3]) {
    writer.list(entries, |writer, entry| {
        writer.string(&entry.directory);
        writer.list(&entry.groups, |writer, group| writer.string(group));
    });
}

pub fn read_export_list(reader: &mut XdrReader<'_>) -> Result<Vec<ExportEntry3>, XdrError> {
    reader.list(1 << 16, "exports", |reader| {
        Ok(ExportEntry3 {
            directory: reader.string(MNT3_PATHLEN, "ex_dir")?,
            groups: reader.list(1 << 12, "groups", |reader| reader.string(255, "gr_name"))?,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_stats() -> Stats {
        Stats {
            dev: 0x0102_0304,
            ino: 0x1122_3344,
            mode: mount_rs_core::types::S_IFDIR | 0o751,
            nlink: 3,
            uid: 1000,
            gid: 100,
            rdev: 0,
            size: 4096,
            blksize: 4096,
            blocks: 8,
            atime_ms: 1_700_000_000_123,
            mtime_ms: 1_700_000_005_234,
            ctime_ms: 1_700_000_010_345,
            birthtime_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn fattr_has_type_separate_from_permission_mode() {
        let attr = fattr_of(&sample_stats(), 0x1122_3344);
        assert_eq!(attr.r#type, NF3DIR);
        assert_eq!(attr.mode, 0o751);
        let mut writer = XdrWriter::new();
        write_fattr(&mut writer, &attr);
        assert_eq!(writer.len(), FATTR3_SIZE);
        let bytes = writer.into_bytes();
        let mut reader = XdrReader::new(&bytes);
        assert_eq!(read_fattr(&mut reader).unwrap(), attr);
        reader.end("fattr").unwrap();
    }

    #[test]
    fn write_data_and_create_args_are_wire_symmetric() {
        let args = Write3args {
            file: vec![1, 2, 3],
            offset: 0x0102_0304_0506_0708,
            count: 4,
            stable: FILE_SYNC,
            data: vec![9, 8, 7, 6],
        };
        let mut writer = XdrWriter::new();
        write_write_args(&mut writer, &args);
        let bytes = writer.into_bytes();
        let mut reader = XdrReader::new(&bytes);
        assert_eq!(read_write_args(&mut reader, 32).unwrap(), args);
        reader.end("write args").unwrap();

        let create = Create3args {
            where_: DirOpArgs {
                dir: vec![1],
                name: "hello".to_owned(),
            },
            mode: CREATE_EXCLUSIVE,
            attributes: None,
            verf: Some(vec![1, 2, 3, 4, 5, 6, 7, 8]),
        };
        let mut writer = XdrWriter::new();
        write_create_args(&mut writer, &create);
        let bytes = writer.into_bytes();
        let mut reader = XdrReader::new(&bytes);
        assert_eq!(read_create_args(&mut reader).unwrap(), create);
    }
}
