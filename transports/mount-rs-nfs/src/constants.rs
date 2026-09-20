//! NFSv3 and MOUNTv3 constants transcribed from RFC 1813.

pub const NFS_PROGRAM: u32 = 100_003;
pub const NFS_V3: u32 = 3;
pub const MOUNT_PROGRAM: u32 = 100_005;
pub const MOUNT_V3: u32 = 3;

pub const NFSPROC3_NULL: u32 = 0;
pub const NFSPROC3_GETATTR: u32 = 1;
pub const NFSPROC3_SETATTR: u32 = 2;
pub const NFSPROC3_LOOKUP: u32 = 3;
pub const NFSPROC3_ACCESS: u32 = 4;
pub const NFSPROC3_READLINK: u32 = 5;
pub const NFSPROC3_READ: u32 = 6;
pub const NFSPROC3_WRITE: u32 = 7;
pub const NFSPROC3_CREATE: u32 = 8;
pub const NFSPROC3_MKDIR: u32 = 9;
pub const NFSPROC3_SYMLINK: u32 = 10;
pub const NFSPROC3_MKNOD: u32 = 11;
pub const NFSPROC3_REMOVE: u32 = 12;
pub const NFSPROC3_RMDIR: u32 = 13;
pub const NFSPROC3_RENAME: u32 = 14;
pub const NFSPROC3_LINK: u32 = 15;
pub const NFSPROC3_READDIR: u32 = 16;
pub const NFSPROC3_READDIRPLUS: u32 = 17;
pub const NFSPROC3_FSSTAT: u32 = 18;
pub const NFSPROC3_FSINFO: u32 = 19;
pub const NFSPROC3_PATHCONF: u32 = 20;
pub const NFSPROC3_COMMIT: u32 = 21;

pub const MOUNTPROC3_NULL: u32 = 0;
pub const MOUNTPROC3_MNT: u32 = 1;
pub const MOUNTPROC3_DUMP: u32 = 2;
pub const MOUNTPROC3_UMNT: u32 = 3;
pub const MOUNTPROC3_UMNTALL: u32 = 4;
pub const MOUNTPROC3_EXPORT: u32 = 5;

pub const NFS3_FHSIZE: usize = 64;
pub const NFS3_COOKIEVERFSIZE: usize = 8;
pub const NFS3_CREATEVERFSIZE: usize = 8;
pub const NFS3_WRITEVERFSIZE: usize = 8;
pub const MNT3_FHSIZE: usize = 64;
pub const MNT3_PATHLEN: usize = 1024;
pub const MNT3_NAMELEN: usize = 255;

pub const NFS3_OK: u32 = 0;
pub const NFS3ERR_PERM: u32 = 1;
pub const NFS3ERR_NOENT: u32 = 2;
pub const NFS3ERR_IO: u32 = 5;
pub const NFS3ERR_NXIO: u32 = 6;
pub const NFS3ERR_ACCES: u32 = 13;
pub const NFS3ERR_EXIST: u32 = 17;
pub const NFS3ERR_XDEV: u32 = 18;
pub const NFS3ERR_NODEV: u32 = 19;
pub const NFS3ERR_NOTDIR: u32 = 20;
pub const NFS3ERR_ISDIR: u32 = 21;
pub const NFS3ERR_INVAL: u32 = 22;
pub const NFS3ERR_FBIG: u32 = 27;
pub const NFS3ERR_NOSPC: u32 = 28;
pub const NFS3ERR_ROFS: u32 = 30;
pub const NFS3ERR_MLINK: u32 = 31;
pub const NFS3ERR_NAMETOOLONG: u32 = 63;
pub const NFS3ERR_NOTEMPTY: u32 = 66;
pub const NFS3ERR_DQUOT: u32 = 69;
pub const NFS3ERR_STALE: u32 = 70;
pub const NFS3ERR_REMOTE: u32 = 71;
pub const NFS3ERR_BADHANDLE: u32 = 10_001;
pub const NFS3ERR_NOT_SYNC: u32 = 10_002;
pub const NFS3ERR_BAD_COOKIE: u32 = 10_003;
pub const NFS3ERR_NOTSUPP: u32 = 10_004;
pub const NFS3ERR_TOOSMALL: u32 = 10_005;
pub const NFS3ERR_SERVERFAULT: u32 = 10_006;
pub const NFS3ERR_BADTYPE: u32 = 10_007;
pub const NFS3ERR_JUKEBOX: u32 = 10_008;

pub const NF3REG: u32 = 1;
pub const NF3DIR: u32 = 2;
pub const NF3BLK: u32 = 3;
pub const NF3CHR: u32 = 4;
pub const NF3LNK: u32 = 5;
pub const NF3SOCK: u32 = 6;
pub const NF3FIFO: u32 = 7;

pub const UNSTABLE: u32 = 0;
pub const DATA_SYNC: u32 = 1;
pub const FILE_SYNC: u32 = 2;

pub const CREATE_UNCHECKED: u32 = 0;
pub const CREATE_GUARDED: u32 = 1;
pub const CREATE_EXCLUSIVE: u32 = 2;

pub const DONT_CHANGE: u32 = 0;
pub const SET_TO_SERVER_TIME: u32 = 1;
pub const SET_TO_CLIENT_TIME: u32 = 2;

pub const ACCESS3_READ: u32 = 0x01;
pub const ACCESS3_LOOKUP: u32 = 0x02;
pub const ACCESS3_MODIFY: u32 = 0x04;
pub const ACCESS3_EXTEND: u32 = 0x08;
pub const ACCESS3_DELETE: u32 = 0x10;
pub const ACCESS3_EXECUTE: u32 = 0x20;
pub const ACCESS3_ALL: u32 = ACCESS3_READ
    | ACCESS3_LOOKUP
    | ACCESS3_MODIFY
    | ACCESS3_EXTEND
    | ACCESS3_DELETE
    | ACCESS3_EXECUTE;

pub const FSF3_LINK: u32 = 0x01;
pub const FSF3_SYMLINK: u32 = 0x02;
pub const FSF3_HOMOGENEOUS: u32 = 0x08;
pub const FSF3_CANSETTIME: u32 = 0x10;

pub const MNT3_OK: u32 = 0;
pub const MNT3ERR_PERM: u32 = 1;
pub const MNT3ERR_NOENT: u32 = 2;
pub const MNT3ERR_IO: u32 = 5;
pub const MNT3ERR_ACCES: u32 = 13;
pub const MNT3ERR_NOTDIR: u32 = 20;
pub const MNT3ERR_INVAL: u32 = 22;
pub const MNT3ERR_NAMETOOLONG: u32 = 63;
pub const MNT3ERR_NOTSUPP: u32 = 10_004;
pub const MNT3ERR_SERVERFAULT: u32 = 10_006;

pub fn procedure_name(program: u32, procedure: u32) -> String {
    let name = if program == NFS_PROGRAM {
        match procedure {
            NFSPROC3_NULL => "NULL",
            NFSPROC3_GETATTR => "GETATTR",
            NFSPROC3_SETATTR => "SETATTR",
            NFSPROC3_LOOKUP => "LOOKUP",
            NFSPROC3_ACCESS => "ACCESS",
            NFSPROC3_READLINK => "READLINK",
            NFSPROC3_READ => "READ",
            NFSPROC3_WRITE => "WRITE",
            NFSPROC3_CREATE => "CREATE",
            NFSPROC3_MKDIR => "MKDIR",
            NFSPROC3_SYMLINK => "SYMLINK",
            NFSPROC3_MKNOD => "MKNOD",
            NFSPROC3_REMOVE => "REMOVE",
            NFSPROC3_RMDIR => "RMDIR",
            NFSPROC3_RENAME => "RENAME",
            NFSPROC3_LINK => "LINK",
            NFSPROC3_READDIR => "READDIR",
            NFSPROC3_READDIRPLUS => "READDIRPLUS",
            NFSPROC3_FSSTAT => "FSSTAT",
            NFSPROC3_FSINFO => "FSINFO",
            NFSPROC3_PATHCONF => "PATHCONF",
            NFSPROC3_COMMIT => "COMMIT",
            _ => return format!("NFS:{procedure}"),
        }
    } else if program == MOUNT_PROGRAM {
        match procedure {
            MOUNTPROC3_NULL => "NULL",
            MOUNTPROC3_MNT => "MNT",
            MOUNTPROC3_DUMP => "DUMP",
            MOUNTPROC3_UMNT => "UMNT",
            MOUNTPROC3_UMNTALL => "UMNTALL",
            MOUNTPROC3_EXPORT => "EXPORT",
            _ => return format!("MOUNT:{procedure}"),
        }
    } else {
        return format!("prog {program} proc {procedure}");
    };
    name.to_owned()
}
