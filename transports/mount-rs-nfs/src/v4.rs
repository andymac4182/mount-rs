//! NFSv4.1 (RFC 8881/RFC 5662) COMPOUND service.
//!
//! This module is intentionally byte-oriented like the v3 session.  It owns
//! the v4.1 client/session/slot state and translates the stateless portions of
//! COMPOUND into the shared `FsDriver` contract.  The implementation speaks
//! AUTH_NONE and AUTH_SYS over TCP, advertises no callback channel or
//! delegations, and uses the process-local NFS file-handle table as its
//! persistent-handle boundary.

use std::cmp::Ordering as SeqidOrdering;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use mount_rs_core::{
    ErrorCode, FileHandle, FsDriver, FsError, Loopback, MkdirOptions, OpenFlags, S_IFBLK, S_IFCHR,
    S_IFDIR, S_IFIFO, S_IFLNK, S_IFMT, S_IFREG, S_IFSOCK, Stats, StatsFs,
};

use crate::handles::{
    DirectorySnapshots, FileHandleTable, HandleEntry, cookie_verifier, same_verifier,
};
use crate::rpc::{
    AUTH_NONE, AUTH_SYS, AUTH_TOOWEAK, RPC_GARBAGE_ARGS, RPC_PROC_UNAVAIL, RPC_PROG_MISMATCH,
    RPC_PROG_UNAVAIL, RPC_VERSION, RpcCredentials, credentials_of, decode_call,
    encode_accept_error, encode_accepted_reply, encode_auth_error, encode_rpc_mismatch,
};
use crate::session::{
    NfsRequestContext, NfsSessionError, NfsSessionHooks, NfsSessionOptions, NfsSessionStats,
    SharedNfsState, SharedStats,
};
use crate::xdr::{XdrError, XdrReader, XdrWriter};

// ---------------------------------------------------------------------------
// RFC 8881 constants used by the codec and session
// ---------------------------------------------------------------------------

pub const NFS4_PROGRAM: u32 = 100_003;
pub const NFS_V4: u32 = 4;
pub const NFS4_MINOR_VERSION_1: u32 = 1;
pub const NFSPROC4_NULL: u32 = 0;
pub const NFSPROC4_COMPOUND: u32 = 1;

pub const NFS4_FHSIZE: usize = 128;
pub const NFS4_VERIFIER_SIZE: usize = 8;
pub const NFS4_SESSIONID_SIZE: usize = 16;
pub const NFS4_OTHER_SIZE: usize = 12;
pub const NFS4_OPAQUE_LIMIT: usize = 1024;
pub const NFS4_MAX_TAG: usize = 1024;
pub const NFS4_MAX_COMPOUND_OPS: usize = 4096;
pub const NFS4_MAX_COMPONENT: usize = 1024;
pub const NFS4_MAX_BITMAP_WORDS: usize = 16;
pub const NFS4_MAX_ATTRLIST: usize = 64 * 1024;
pub const NFS4_MAX_READDIR_ENTRIES: usize = 1 << 20;
pub const NFS4_MAX_SEC_PARMS: usize = 16;
pub const NFS4_MAX_AUTHSYS_GIDS: usize = 16;
pub const NFS4_MAX_TEST_STATEIDS: usize = 1024;

pub const NFS4_OK: u32 = 0;
pub const NFS4ERR_PERM: u32 = 1;
pub const NFS4ERR_NOENT: u32 = 2;
pub const NFS4ERR_IO: u32 = 5;
pub const NFS4ERR_ACCESS: u32 = 13;
pub const NFS4ERR_EXIST: u32 = 17;
pub const NFS4ERR_NOTDIR: u32 = 20;
pub const NFS4ERR_ISDIR: u32 = 21;
pub const NFS4ERR_INVAL: u32 = 22;
pub const NFS4ERR_NOSPC: u32 = 28;
pub const NFS4ERR_ROFS: u32 = 30;
pub const NFS4ERR_NAMETOOLONG: u32 = 63;
pub const NFS4ERR_NOTEMPTY: u32 = 66;
pub const NFS4ERR_STALE: u32 = 70;
pub const NFS4ERR_BADHANDLE: u32 = 10_001;
pub const NFS4ERR_BAD_COOKIE: u32 = 10_003;
pub const NFS4ERR_NOTSUPP: u32 = 10_004;
pub const NFS4ERR_TOOSMALL: u32 = 10_005;
pub const NFS4ERR_SERVERFAULT: u32 = 10_006;
pub const NFS4ERR_BADTYPE: u32 = 10_007;
pub const NFS4ERR_DELAY: u32 = 10_008;
pub const NFS4ERR_SAME: u32 = 10_009;
pub const NFS4ERR_DENIED: u32 = 10_010;
pub const NFS4ERR_LOCKED: u32 = 10_012;
pub const NFS4ERR_GRACE: u32 = 10_013;
pub const NFS4ERR_SHARE_DENIED: u32 = 10_015;
pub const NFS4ERR_NOFILEHANDLE: u32 = 10_020;
pub const NFS4ERR_RESTOREFH: u32 = 10_030;
pub const NFS4ERR_MINOR_VERS_MISMATCH: u32 = 10_021;
pub const NFS4ERR_STALE_CLIENTID: u32 = 10_022;
pub const NFS4ERR_OLD_STATEID: u32 = 10_024;
pub const NFS4ERR_BAD_STATEID: u32 = 10_025;
pub const NFS4ERR_BAD_SEQID: u32 = 10_026;
pub const NFS4ERR_OPENMODE: u32 = 10_038;
pub const NFS4ERR_NOT_SAME: u32 = 10_027;
pub const NFS4ERR_ATTRNOTSUPP: u32 = 10_032;
pub const NFS4ERR_NO_GRACE: u32 = 10_033;
pub const NFS4ERR_BADXDR: u32 = 10_036;
pub const NFS4ERR_BADOWNER: u32 = 10_039;
pub const NFS4ERR_OP_ILLEGAL: u32 = 10_044;
pub const NFS4ERR_BADSESSION: u32 = 10_052;
pub const NFS4ERR_BADSLOT: u32 = 10_053;
pub const NFS4ERR_SEQ_MISORDERED: u32 = 10_063;
pub const NFS4ERR_SEQUENCE_POS: u32 = 10_064;
pub const NFS4ERR_REP_TOO_BIG_TO_CACHE: u32 = 10_067;
pub const NFS4ERR_RETRY_UNCACHED_REP: u32 = 10_068;
pub const NFS4ERR_TOO_MANY_OPS: u32 = 10_070;
pub const NFS4ERR_OP_NOT_IN_SESSION: u32 = 10_071;
pub const NFS4ERR_SEQ_FALSE_RETRY: u32 = 10_076;
pub const NFS4ERR_NOT_ONLY_OP: u32 = 10_081;
pub const NFS4ERR_CONN_NOT_BOUND_TO_SESSION: u32 = 10_055;
pub const NFS4ERR_CLIENTID_BUSY: u32 = 10_074;
pub const NFS4ERR_LOCKS_HELD: u32 = 10_037;
pub const NFS4ERR_LOCK_RANGE: u32 = 10_028;
pub const NFS4ERR_LOCK_NOTSUPP: u32 = 10_043;
pub const NFS4ERR_SYMLINK: u32 = 10_029;
pub const NFS4ERR_WRONG_TYPE: u32 = 10_083;
pub const NFS4ERR_BAD_RANGE: u32 = 10_042;
pub const NFS4ERR_XDEV: u32 = 18;
pub const NFS4ERR_FBIG: u32 = 27;
pub const NFS4ERR_DQUOT: u32 = 69;
pub const NFS4ERR_BADNAME: u32 = 10_041;
pub const NFS4ERR_RESOURCE: u32 = 10_018;

const V4_TRACE_ENV: &str = "MOUNT_RS_NFS_V4_TRACE";
const V4_TRACE_EVENT_LIMIT: usize = 256;

static V4_TRACE_EVENTS: AtomicUsize = AtomicUsize::new(0);
static V4_TRACE_ENABLED: OnceLock<bool> = OnceLock::new();

fn v4_trace_enabled() -> bool {
    *V4_TRACE_ENABLED.get_or_init(|| {
        matches!(
            std::env::var(V4_TRACE_ENV).as_deref(),
            Ok("1") | Ok("true") | Ok("yes")
        )
    })
}

/// Emit bounded NFSv4 diagnostics for native-client interoperability work.
///
/// The trace deliberately contains only transport/protocol metadata: peer,
/// XID, minor version, operation numbers/names, and NFS statuses. It never
/// formats RPC credentials, path names, file handles, owners, or read/write
/// payloads. The event cap is process-wide so a misbehaving client cannot turn
/// an opt-in CI diagnostic into an unbounded log stream.
fn v4_trace(event: &str, peer: Option<&str>, xid: u32, details: impl fmt::Display) {
    if !v4_trace_enabled() {
        return;
    }
    let index = V4_TRACE_EVENTS.fetch_add(1, Ordering::Relaxed);
    if index < V4_TRACE_EVENT_LIMIT {
        eprintln!(
            "mount-rs-nfs: v4-trace event={event} peer={} xid={xid} {details}",
            peer.unwrap_or("unknown")
        );
    } else if index == V4_TRACE_EVENT_LIMIT {
        eprintln!(
            "mount-rs-nfs: v4-trace event=limit peer={} xid={xid} limit={V4_TRACE_EVENT_LIMIT}",
            peer.unwrap_or("unknown")
        );
    }
}

fn v4_op_name(op: u32) -> &'static str {
    match op {
        OP_ACCESS => "ACCESS",
        OP_CLOSE => "CLOSE",
        OP_COMMIT => "COMMIT",
        OP_CREATE => "CREATE",
        OP_GETATTR => "GETATTR",
        OP_GETFH => "GETFH",
        OP_LINK => "LINK",
        OP_LOOKUP => "LOOKUP",
        OP_LOOKUPP => "LOOKUPP",
        OP_LOCK => "LOCK",
        OP_LOCKT => "LOCKT",
        OP_LOCKU => "LOCKU",
        OP_NVERIFY => "NVERIFY",
        OP_OPEN => "OPEN",
        OP_OPEN_DOWNGRADE => "OPEN_DOWNGRADE",
        OP_PUTFH => "PUTFH",
        OP_PUTPUBFH => "PUTPUBFH",
        OP_PUTROOTFH => "PUTROOTFH",
        OP_READ => "READ",
        OP_READDIR => "READDIR",
        OP_READLINK => "READLINK",
        OP_REMOVE => "REMOVE",
        OP_RENAME => "RENAME",
        OP_RESTOREFH => "RESTOREFH",
        OP_SAVEFH => "SAVEFH",
        OP_SECINFO => "SECINFO",
        OP_SECINFO_NO_NAME => "SECINFO_NO_NAME",
        OP_SETATTR => "SETATTR",
        OP_VERIFY => "VERIFY",
        OP_WRITE => "WRITE",
        OP_BACKCHANNEL_CTL => "BACKCHANNEL_CTL",
        OP_BIND_CONN_TO_SESSION => "BIND_CONN_TO_SESSION",
        OP_EXCHANGE_ID => "EXCHANGE_ID",
        OP_CREATE_SESSION => "CREATE_SESSION",
        OP_DESTROY_SESSION => "DESTROY_SESSION",
        OP_FREE_STATEID => "FREE_STATEID",
        OP_DESTROY_CLIENTID => "DESTROY_CLIENTID",
        OP_SEQUENCE => "SEQUENCE",
        OP_TEST_STATEID => "TEST_STATEID",
        OP_RECLAIM_COMPLETE => "RECLAIM_COMPLETE",
        OP_ILLEGAL => "ILLEGAL",
        _ => "UNKNOWN",
    }
}

fn v4_opcodes(operations: &[Op]) -> String {
    operations
        .iter()
        .map(|operation| {
            let op = operation.opnum();
            format!("{op}:{}", v4_op_name(op))
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn v4_trace_compound_reply(
    peer: Option<&str>,
    xid: u32,
    status: u32,
    result_count: usize,
    cached: bool,
) {
    v4_trace(
        "compound-reply",
        peer,
        xid,
        format_args!("status={status} results={result_count} cached={cached}"),
    );
}

fn v4_trace_body_reply(peer: Option<&str>, xid: u32, body: &[u8], cached: bool) {
    let status = body
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_be_bytes)
        .unwrap_or(NFS4ERR_SERVERFAULT);
    let result_count = body
        .get(8..)
        .and_then(|bytes| bytes.get(..4))
        .and_then(|bytes| bytes.try_into().ok())
        .map(u32::from_be_bytes)
        .unwrap_or(0) as usize;
    v4_trace_compound_reply(peer, xid, status, result_count, cached);
}

pub const OP_ACCESS: u32 = 3;
pub const OP_CLOSE: u32 = 4;
pub const OP_COMMIT: u32 = 5;
pub const OP_CREATE: u32 = 6;
pub const OP_GETATTR: u32 = 9;
pub const OP_GETFH: u32 = 10;
pub const OP_LINK: u32 = 11;
pub const OP_LOOKUP: u32 = 15;
pub const OP_LOOKUPP: u32 = 16;
pub const OP_NVERIFY: u32 = 17;
pub const OP_OPEN: u32 = 18;
pub const OP_OPEN_DOWNGRADE: u32 = 21;
pub const OP_LOCK: u32 = 12;
pub const OP_LOCKT: u32 = 13;
pub const OP_LOCKU: u32 = 14;
pub const OP_PUTFH: u32 = 22;
pub const OP_PUTPUBFH: u32 = 23;
pub const OP_PUTROOTFH: u32 = 24;
pub const OP_READ: u32 = 25;
pub const OP_READDIR: u32 = 26;
pub const OP_READLINK: u32 = 27;
pub const OP_REMOVE: u32 = 28;
pub const OP_RENAME: u32 = 29;
pub const OP_RESTOREFH: u32 = 31;
pub const OP_SAVEFH: u32 = 32;
pub const OP_SECINFO: u32 = 33;
pub const OP_SETATTR: u32 = 34;
pub const OP_VERIFY: u32 = 37;
pub const OP_WRITE: u32 = 38;
pub const OP_BACKCHANNEL_CTL: u32 = 40;
pub const OP_BIND_CONN_TO_SESSION: u32 = 41;
pub const OP_EXCHANGE_ID: u32 = 42;
pub const OP_CREATE_SESSION: u32 = 43;
pub const OP_DESTROY_SESSION: u32 = 44;
pub const OP_FREE_STATEID: u32 = 45;
pub const OP_DESTROY_CLIENTID: u32 = 57;
pub const OP_SECINFO_NO_NAME: u32 = 52;
pub const OP_SEQUENCE: u32 = 53;
pub const OP_TEST_STATEID: u32 = 55;
pub const OP_RECLAIM_COMPLETE: u32 = 58;
pub const OP_ILLEGAL: u32 = 10_044;

// CREATE_SESSION4 csa_flags/csr_flags (RFC 8881 §18.36).  The server
// currently grants none of these optional capabilities: it has no persistent
// reply cache, callback channel, or RDMA transport.  A client may still ask
// for them; the result must mask them out rather than reject the request.
pub const CREATE_SESSION4_FLAG_PERSIST: u32 = 0x0000_0001;
pub const CREATE_SESSION4_FLAG_CONN_BACK_CHAN: u32 = 0x0000_0002;
pub const CREATE_SESSION4_FLAG_CONN_RDMA: u32 = 0x0000_0004;
const SERVER_CREATE_SESSION_FLAGS: u32 = 0;

pub const NF4REG: u32 = 1;
pub const NF4DIR: u32 = 2;
pub const NF4BLK: u32 = 3;
pub const NF4CHR: u32 = 4;
pub const NF4LNK: u32 = 5;
pub const NF4SOCK: u32 = 6;
pub const NF4FIFO: u32 = 7;
pub const NF4ATTRDIR: u32 = 8;
pub const NF4NAMEDATTR: u32 = 9;

pub const FATTR4_SUPPORTED_ATTRS: u32 = 0;
pub const FATTR4_TYPE: u32 = 1;
pub const FATTR4_FH_EXPIRE_TYPE: u32 = 2;
pub const FATTR4_CHANGE: u32 = 3;
pub const FATTR4_SIZE: u32 = 4;
pub const FATTR4_LINK_SUPPORT: u32 = 5;
pub const FATTR4_SYMLINK_SUPPORT: u32 = 6;
pub const FATTR4_NAMED_ATTR: u32 = 7;
pub const FATTR4_FSID: u32 = 8;
pub const FATTR4_UNIQUE_HANDLES: u32 = 9;
pub const FATTR4_LEASE_TIME: u32 = 10;
pub const FATTR4_RDATTR_ERROR: u32 = 11;
pub const FATTR4_FILEHANDLE: u32 = 19;
pub const FATTR4_FILEID: u32 = 20;
pub const FATTR4_FILES_AVAIL: u32 = 21;
pub const FATTR4_FILES_FREE: u32 = 22;
pub const FATTR4_FILES_TOTAL: u32 = 23;
pub const FATTR4_MAXFILESIZE: u32 = 27;
pub const FATTR4_MAXLINK: u32 = 28;
pub const FATTR4_MAXNAME: u32 = 29;
pub const FATTR4_MAXREAD: u32 = 30;
pub const FATTR4_MAXWRITE: u32 = 31;
pub const FATTR4_MODE: u32 = 33;
pub const FATTR4_NUMLINKS: u32 = 35;
pub const FATTR4_OWNER: u32 = 36;
pub const FATTR4_OWNER_GROUP: u32 = 37;
pub const FATTR4_RAWDEV: u32 = 41;
pub const FATTR4_SPACE_AVAIL: u32 = 42;
pub const FATTR4_SPACE_FREE: u32 = 43;
pub const FATTR4_SPACE_TOTAL: u32 = 44;
pub const FATTR4_SPACE_USED: u32 = 45;
pub const FATTR4_TIME_ACCESS: u32 = 47;
pub const FATTR4_TIME_ACCESS_SET: u32 = 48;
pub const FATTR4_TIME_DELTA: u32 = 51;
pub const FATTR4_TIME_METADATA: u32 = 52;
pub const FATTR4_TIME_MODIFY: u32 = 53;
pub const FATTR4_TIME_MODIFY_SET: u32 = 54;

pub const FH4_PERSISTENT: u32 = 0;
pub const ACCESS4_READ: u32 = 1;
pub const ACCESS4_LOOKUP: u32 = 2;
pub const ACCESS4_MODIFY: u32 = 4;
pub const ACCESS4_EXTEND: u32 = 8;
pub const ACCESS4_DELETE: u32 = 16;
pub const ACCESS4_EXECUTE: u32 = 32;
pub const ACCESS4_ALL: u32 = 63;
pub const FILE_SYNC4: u32 = 2;
pub const UNSTABLE4: u32 = 0;
pub const DATA_SYNC4: u32 = 1;
pub const SECINFO_STYLE4_CURRENT_FH: u32 = 0;
pub const SECINFO_STYLE4_PARENT: u32 = 1;
pub const UNCHECKED4: u32 = 0;
pub const GUARDED4: u32 = 1;
pub const EXCLUSIVE4: u32 = 2;
pub const EXCLUSIVE4_1: u32 = 3;
pub const OPEN4_NOCREATE: u32 = 0;
pub const OPEN4_CREATE: u32 = 1;
pub const OPEN4_SHARE_ACCESS_READ: u32 = 1;
pub const OPEN4_SHARE_ACCESS_WRITE: u32 = 2;
pub const OPEN4_SHARE_ACCESS_BOTH: u32 = 3;
pub const OPEN4_SHARE_DENY_NONE: u32 = 0;
pub const OPEN4_SHARE_DENY_READ: u32 = 1;
pub const OPEN4_SHARE_DENY_WRITE: u32 = 2;
pub const OPEN4_SHARE_DENY_BOTH: u32 = 3;
pub const OPEN4_SHARE_ACCESS_WANT_DELEG_MASK: u32 = 0xff00;
pub const OPEN4_SHARE_ACCESS_WANT_SIGNAL_DELEG_WHEN_RESRC_AVAIL: u32 = 0x0001_0000;
pub const OPEN4_SHARE_ACCESS_WANT_PUSH_DELEG_WHEN_UNCONTENDED: u32 = 0x0002_0000;
pub const OPEN_DELEGATE_NONE: u32 = 0;
pub const OPEN4_RESULT_LOCKTYPE_POSIX: u32 = 4;
pub const CLAIM_NULL: u32 = 0;
pub const CLAIM_PREVIOUS: u32 = 1;
pub const CLAIM_FH: u32 = 4;
pub const CLAIM_DELEG_PREV_FH: u32 = 6;
pub const CDFC4_FORE: u32 = 1;
pub const CDFC4_FORE_OR_BOTH: u32 = 3;
pub const CDFS4_FORE: u32 = 1;
pub const SP4_NONE: u32 = 0;
pub const EXCHGID4_FLAG_SUPP_MOVED_REFER: u32 = 0x0000_0001;
pub const EXCHGID4_FLAG_SUPP_MOVED_MIGR: u32 = 0x0000_0002;
pub const EXCHGID4_FLAG_BIND_PRINC_STATEID: u32 = 0x0000_0100;
pub const EXCHGID4_FLAG_USE_NON_PNFS: u32 = 0x0001_0000;
pub const EXCHGID4_FLAG_USE_PNFS_MDS: u32 = 0x0002_0000;
pub const EXCHGID4_FLAG_USE_PNFS_DS: u32 = 0x0004_0000;
pub const EXCHGID4_FLAG_UPD_CONFIRMED_REC_A: u32 = 0x4000_0000;
pub const EXCHGID4_FLAG_CONFIRMED_R: u32 = 0x8000_0000;
pub const READ_LT: u32 = 1;
pub const WRITE_LT: u32 = 2;
pub const READW_LT: u32 = 3;
pub const WRITEW_LT: u32 = 4;

const COOKIE_BASE: u64 = 3;
const DEFAULT_MAX_SLOTS: usize = 64;
const DEFAULT_MAX_READ: usize = 1024 * 1024;
const DEFAULT_MAX_WRITE: usize = 1024 * 1024;
const MAX_OFFSET: u64 = 9_007_199_254_740_991;
const MIN_RESPONSE_SIZE: u32 = 128;

// ---------------------------------------------------------------------------
// v4 wire values
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stateid4 {
    pub seqid: u32,
    pub other: [u8; NFS4_OTHER_SIZE],
}

impl Stateid4 {
    fn zero() -> Self {
        Self {
            seqid: 0,
            other: [0; NFS4_OTHER_SIZE],
        }
    }
}

#[derive(Debug, Clone, Default)]
struct AttrValues {
    file_type: Option<u32>,
    change: Option<u64>,
    fileid: Option<u64>,
    mode: Option<u32>,
    size: Option<u64>,
    owner: Option<String>,
    owner_group: Option<String>,
    rawdev: Option<(u32, u32)>,
    time_access: Option<(i64, u32)>,
    time_modify: Option<(i64, u32)>,
    time_access_set: Option<(u32, Option<(i64, u32)>)>,
    time_modify_set: Option<(u32, Option<(i64, u32)>)>,
}

#[derive(Debug, Clone)]
struct Fattr4 {
    mask: Vec<u32>,
    values: AttrValues,
    unsupported: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct ApplyAttrsOptions {
    skip_size: bool,
    skip_mode: bool,
}

#[derive(Debug, Clone)]
struct AppliedAttrs {
    status: u32,
    bits: Vec<u32>,
}

impl AppliedAttrs {
    fn failed(status: u32) -> Self {
        Self {
            status,
            bits: Vec::new(),
        }
    }

    fn with_status(status: u32, bits: Vec<u32>) -> Self {
        Self { status, bits }
    }
}

#[derive(Debug, Clone)]
struct OpenArgs {
    _seqid: u32,
    share_access: u32,
    share_deny: u32,
    owner: Vec<u8>,
    open_type: u32,
    create_mode: Option<u32>,
    create_attrs: Option<Fattr4>,
    create_verf: Option<[u8; 8]>,
    claim: u32,
    name: Option<String>,
}

#[derive(Debug, Clone)]
enum Op {
    Access(u32),
    Close(u32, Stateid4),
    Commit {
        offset: u64,
        count: u32,
    },
    Create {
        kind: u32,
        name: String,
        attrs: Fattr4,
        link: Option<String>,
        dev: Option<(u32, u32)>,
    },
    Getattr(Vec<u32>),
    Getfh,
    Link(String),
    Lookup(String),
    Lookupp,
    Lock {
        lock_type: u32,
        reclaim: bool,
        offset: u64,
        length: u64,
        locker: Locker4,
    },
    Lockt {
        lock_type: u32,
        offset: u64,
        length: u64,
        owner: Vec<u8>,
    },
    Locku {
        lock_type: u32,
        seqid: u32,
        stateid: Stateid4,
        offset: u64,
        length: u64,
    },
    Nverify(Fattr4),
    Open(OpenArgs),
    OpenDowngrade {
        stateid: Stateid4,
        seqid: u32,
        share_access: u32,
        share_deny: u32,
    },
    Putfh(Vec<u8>),
    Putrootfh,
    Putpubfh,
    Read(Stateid4, u64, u32),
    Readdir {
        cookie: u64,
        verifier: Vec<u8>,
        dircount: u32,
        maxcount: u32,
        attrs: Vec<u32>,
    },
    Readlink,
    Remove(String),
    Rename(String, String),
    Restorefh,
    Savefh,
    Secinfo(String),
    SecinfoNoName(u32),
    Setattr(Stateid4, Fattr4),
    Verify(Fattr4),
    Write(Stateid4, u64, u32, Vec<u8>),
    ExchangeId {
        verifier: Vec<u8>,
        owner: Vec<u8>,
        flags: u32,
        state_protect: u32,
    },
    CreateSession {
        clientid: u64,
        sequence: u32,
        flags: u32,
        fore: ChannelAttrs4,
        back: ChannelAttrs4,
    },
    BindConnToSession {
        sessionid: [u8; NFS4_SESSIONID_SIZE],
        direction: u32,
        use_rdma: bool,
    },
    DestroySession(Vec<u8>),
    DestroyClientid(u64),
    FreeStateid(Stateid4),
    Sequence {
        sessionid: [u8; NFS4_SESSIONID_SIZE],
        sequence: u32,
        slot: u32,
        highest: u32,
        cachethis: bool,
    },
    ReclaimComplete(bool),
    TestStateid(Vec<Stateid4>),
    BackchannelCtl,
    Unsupported(u32),
}

#[derive(Debug, Clone, Copy)]
struct ChannelAttrs4 {
    headerpadsize: u32,
    maxrequestsize: u32,
    maxresponsesize: u32,
    maxresponsesize_cached: u32,
    maxoperations: u32,
    maxrequests: u32,
}

#[derive(Debug, Clone)]
struct Cursor {
    current: Option<Vec<u8>>,
    saved: Option<Vec<u8>>,
    stateid: Stateid4,
    saved_stateid: Stateid4,
    clientid: Option<u64>,
    session: Option<[u8; NFS4_SESSIONID_SIZE]>,
}

impl Default for Cursor {
    fn default() -> Self {
        Self {
            current: None,
            saved: None,
            stateid: Stateid4::zero(),
            saved_stateid: Stateid4::zero(),
            clientid: None,
            session: None,
        }
    }
}

#[derive(Debug, Clone)]
struct ClientState {
    owner: Vec<u8>,
    verifier: Vec<u8>,
    id: u64,
    confirmed: bool,
    sequence: u32,
    renewed: Instant,
    reclaim_complete: bool,
    create_session_replay: Option<CreateSessionReplay>,
}

#[derive(Debug, Clone)]
struct CachedReply {
    sequence: u32,
    body: Vec<u8>,
    credentials: RpcCredentials,
}

#[derive(Debug, Clone)]
struct SessionState {
    id: [u8; NFS4_SESSIONID_SIZE],
    clientid: u64,
    next_sequence: Vec<u32>,
    in_flight: Vec<Option<u32>>,
    cached: Vec<Option<CachedReply>>,
    max_operations: u32,
    max_cached: usize,
}

struct InFlightSlot {
    state: Arc<Mutex<V4State>>,
    sessionid: [u8; NFS4_SESSIONID_SIZE],
    slot: usize,
    sequence: u32,
    completed: bool,
}

impl InFlightSlot {
    fn complete(&mut self) {
        self.completed = true;
    }
}

impl Drop for InFlightSlot {
    fn drop(&mut self) {
        if let Ok(mut state) = self.state.lock() {
            let Some(session) = state.sessions.get_mut(&self.sessionid) else {
                return;
            };
            if session.in_flight.get(self.slot) != Some(&Some(self.sequence)) {
                return;
            }
            if self.completed {
                session.in_flight[self.slot] = None;
            } else {
                // The request was canceled after slot admission but before a
                // reply could be cached. Fence the session: replaying this
                // sequence might re-execute a partially completed mutation.
                state.sessions.remove(&self.sessionid);
            }
        }
    }
}

#[derive(Debug, Default)]
struct V4State {
    seed: u32,
    next_clientid: u64,
    next_session: u64,
    clients: HashMap<u64, ClientState>,
    owners: HashMap<Vec<u8>, u64>,
    sessions: HashMap<[u8; NFS4_SESSIONID_SIZE], SessionState>,
    opens: HashMap<[u8; NFS4_OTHER_SIZE], OpenState>,
    locks: HashMap<[u8; NFS4_OTHER_SIZE], LockState>,
    exclusive_creates: HashMap<String, ExclusiveV4>,
}

#[derive(Debug, Clone)]
struct ExclusiveV4 {
    verifier: [u8; 8],
    attrset: Vec<u32>,
}

#[derive(Clone)]
struct OpenState {
    stateid: Stateid4,
    clientid: u64,
    handle_id: u64,
    file_id: u64,
    path: String,
    handle: Arc<dyn FileHandle>,
    access: u32,
    deny: u32,
    owner: Vec<u8>,
}

impl fmt::Debug for OpenState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OpenState")
            .field("stateid", &self.stateid)
            .field("clientid", &self.clientid)
            .field("file_id", &self.file_id)
            .field("path", &self.path)
            .field("access", &self.access)
            .field("deny", &self.deny)
            .field("owner", &self.owner)
            .finish()
    }
}

#[derive(Debug, Clone)]
struct Locker4 {
    new_lock_owner: bool,
    open_stateid: Option<Stateid4>,
    lock_stateid: Option<Stateid4>,
    owner: Vec<u8>,
}

#[derive(Debug, Clone)]
struct LockRange {
    offset: u64,
    length: u64,
    lock_type: u32,
}

#[derive(Debug, Clone)]
struct LockState {
    stateid: Stateid4,
    clientid: u64,
    file_id: u64,
    open_other: [u8; NFS4_OTHER_SIZE],
    owner: Vec<u8>,
    ranges: Vec<LockRange>,
}

fn read_stateid(reader: &mut XdrReader<'_>) -> Result<Stateid4, XdrError> {
    let seqid = reader.u32("stateid.seqid")?;
    let bytes = reader.fixed_opaque(NFS4_OTHER_SIZE, "stateid.other")?;
    Ok(Stateid4 {
        seqid,
        other: bytes.try_into().expect("fixed stateid size"),
    })
}

fn write_stateid(writer: &mut XdrWriter, stateid: &Stateid4) {
    writer.u32(stateid.seqid);
    writer.fixed_opaque(&stateid.other, NFS4_OTHER_SIZE);
}

fn read_bitmap(reader: &mut XdrReader<'_>) -> Result<Vec<u32>, XdrError> {
    reader.array(NFS4_MAX_BITMAP_WORDS, "bitmap4", |reader| {
        reader.u32("bitmap word")
    })
}

fn write_bitmap(writer: &mut XdrWriter, bitmap: &[u32]) {
    writer.array(bitmap, |writer, word| writer.u32(*word));
}

fn bitmap_body(bitmap: &[u32]) -> Vec<u8> {
    let mut writer = XdrWriter::with_capacity(4 + bitmap.len() * 4);
    write_bitmap(&mut writer, bitmap);
    writer.into_bytes()
}

fn bitmap_has(bitmap: &[u32], bit: u32) -> bool {
    bitmap
        .get((bit / 32) as usize)
        .is_some_and(|word| (word & (1 << (bit % 32))) != 0)
}

fn bitmap_of(bits: impl IntoIterator<Item = u32>) -> Vec<u32> {
    let mut words = Vec::new();
    for bit in bits {
        let index = (bit / 32) as usize;
        while words.len() <= index {
            words.push(0);
        }
        words[index] |= 1 << (bit % 32);
    }
    while words.last() == Some(&0) {
        words.pop();
    }
    words
}

fn bitmap_bits(bitmap: &[u32]) -> impl Iterator<Item = u32> + '_ {
    bitmap.iter().enumerate().flat_map(|(word_index, word)| {
        (0..32).filter_map(move |bit| {
            (*word & (1 << bit) != 0).then_some((word_index as u32) * 32 + bit)
        })
    })
}

fn read_attrs(reader: &mut XdrReader<'_>) -> Result<Fattr4, XdrError> {
    let mask = read_bitmap(reader)?;
    let bytes = reader.var_opaque(NFS4_MAX_ATTRLIST, "attrlist4")?;
    let mut values = AttrValues::default();
    let mut attrs = XdrReader::new(&bytes);
    let mut unsupported = false;
    for bit in 0..(mask.len() * 32) as u32 {
        if !bitmap_has(&mask, bit) {
            continue;
        }
        match bit {
            FATTR4_SUPPORTED_ATTRS => {
                let _ = read_bitmap(&mut attrs)?;
            }
            FATTR4_TYPE => values.file_type = Some(attrs.u32("fattr4.type")?),
            FATTR4_FH_EXPIRE_TYPE => {
                let _ = attrs.u32("fattr4.fh_expire_type")?;
            }
            FATTR4_CHANGE => values.change = Some(attrs.u64("fattr4.change")?),
            FATTR4_MODE => values.mode = Some(attrs.u32("fattr4.mode")?),
            FATTR4_SIZE => values.size = Some(attrs.u64("fattr4.size")?),
            FATTR4_LINK_SUPPORT
            | FATTR4_SYMLINK_SUPPORT
            | FATTR4_NAMED_ATTR
            | FATTR4_UNIQUE_HANDLES => {
                let _ = attrs.bool("fattr4.bool")?;
            }
            FATTR4_FSID => {
                let _ = attrs.u64("fattr4.fsid.major")?;
                let _ = attrs.u64("fattr4.fsid.minor")?;
            }
            FATTR4_LEASE_TIME | FATTR4_RDATTR_ERROR | FATTR4_MAXLINK | FATTR4_NUMLINKS => {
                let _ = attrs.u32("fattr4.u32")?;
            }
            FATTR4_FILEHANDLE => {
                let _ = attrs.var_opaque(NFS4_FHSIZE, "fattr4.filehandle")?;
            }
            FATTR4_FILEID => values.fileid = Some(attrs.u64("fattr4.fileid")?),
            FATTR4_FILES_AVAIL | FATTR4_FILES_FREE | FATTR4_FILES_TOTAL | FATTR4_MAXFILESIZE
            | FATTR4_MAXREAD | FATTR4_MAXWRITE | FATTR4_SPACE_AVAIL | FATTR4_SPACE_FREE
            | FATTR4_SPACE_TOTAL | FATTR4_SPACE_USED => {
                let _ = attrs.u64("fattr4.u64")?;
            }
            FATTR4_MAXNAME => {
                let _ = attrs.u32("fattr4.maxname")?;
            }
            FATTR4_OWNER => values.owner = Some(attrs.string(NFS4_OPAQUE_LIMIT, "fattr4.owner")?),
            FATTR4_OWNER_GROUP => {
                values.owner_group = Some(attrs.string(NFS4_OPAQUE_LIMIT, "fattr4.owner_group")?)
            }
            FATTR4_RAWDEV => {
                values.rawdev = Some((
                    attrs.u32("fattr4.rawdev.specdata1")?,
                    attrs.u32("fattr4.rawdev.specdata2")?,
                ))
            }
            FATTR4_TIME_ACCESS => {
                values.time_access = Some((
                    attrs.i64("fattr4.time_access.seconds")?,
                    attrs.u32("fattr4.time_access.nseconds")?,
                ))
            }
            FATTR4_TIME_MODIFY => {
                values.time_modify = Some((
                    attrs.i64("fattr4.time_modify.seconds")?,
                    attrs.u32("fattr4.time_modify.nseconds")?,
                ))
            }
            FATTR4_TIME_METADATA => {
                let _ = attrs.i64("fattr4.time.seconds")?;
                let _ = attrs.u32("fattr4.time.nseconds")?;
            }
            FATTR4_TIME_DELTA => {
                let _ = attrs.i64("fattr4.time_delta.seconds")?;
                let _ = attrs.u32("fattr4.time_delta.nseconds")?;
            }
            FATTR4_TIME_ACCESS_SET => values.time_access_set = Some(read_set_time(&mut attrs)?),
            FATTR4_TIME_MODIFY_SET => values.time_modify_set = Some(read_set_time(&mut attrs)?),
            _ => {
                // Values in an attrlist are untagged. Once an unknown bit is
                // reached there is no safe way to find later fields. The
                // outer opaque remains framed, so SETATTR can return
                // ATTRNOTSUPP rather than desynchronizing COMPOUND.
                unsupported = true;
                break;
            }
        }
    }
    if !unsupported {
        attrs.end("attrlist4")?;
    }
    Ok(Fattr4 {
        mask,
        values,
        unsupported,
    })
}

fn read_set_time(reader: &mut XdrReader<'_>) -> Result<(u32, Option<(i64, u32)>), XdrError> {
    let how = reader.u32("settime4.how")?;
    let value = if how == 1 {
        Some((
            reader.i64("nfstime4.seconds")?,
            reader.u32("nfstime4.nseconds")?,
        ))
    } else {
        None
    };
    Ok((how, value))
}

fn write_time(writer: &mut XdrWriter, millis: i64) {
    let (seconds, nanos) = time_parts(millis);
    writer.i64(seconds);
    writer.u32(nanos);
}

fn time_parts(millis: i64) -> (i64, u32) {
    (
        millis.div_euclid(1000),
        millis.rem_euclid(1000) as u32 * 1_000_000,
    )
}

fn parse_numeric_owner(value: &str) -> Option<u32> {
    if value == "0"
        || !value.is_empty() && !value.starts_with('0') && value.bytes().all(|b| b.is_ascii_digit())
    {
        value.parse().ok()
    } else {
        None
    }
}

fn read_channel_attrs(reader: &mut XdrReader<'_>) -> Result<ChannelAttrs4, XdrError> {
    let attrs = ChannelAttrs4 {
        headerpadsize: reader.u32("channel.headerpadsize")?,
        maxrequestsize: reader.u32("channel.maxrequestsize")?,
        maxresponsesize: reader.u32("channel.maxresponsesize")?,
        maxresponsesize_cached: reader.u32("channel.maxresponsesize_cached")?,
        maxoperations: reader.u32("channel.maxoperations")?,
        maxrequests: reader.u32("channel.maxrequests")?,
    };
    let _ = reader.array(1, "channel.rdma_ird", |reader| reader.u32("rdma_ird"))?;
    Ok(attrs)
}

fn write_channel_attrs(writer: &mut XdrWriter, attrs: ChannelAttrs4) {
    writer.u32(attrs.headerpadsize);
    writer.u32(attrs.maxrequestsize);
    writer.u32(attrs.maxresponsesize);
    writer.u32(attrs.maxresponsesize_cached);
    writer.u32(attrs.maxoperations);
    writer.u32(attrs.maxrequests);
    writer.u32(0);
}

fn skip_callback_sec_parm(reader: &mut XdrReader<'_>) -> Result<(), XdrError> {
    match reader.u32("callback security flavor")? {
        AUTH_NONE => {}
        AUTH_SYS => {
            let _ = reader.u32("callback stamp")?;
            let _ = reader.string(255, "callback machine")?;
            let _ = reader.u32("callback uid")?;
            let _ = reader.u32("callback gid")?;
            let _ = reader.array(NFS4_MAX_AUTHSYS_GIDS, "callback gids", |reader| {
                reader.u32("gid")
            })?;
        }
        6 => {
            let _ = reader.u32("callback gss service")?;
            let _ = reader.var_opaque(1024, "callback gss server")?;
            let _ = reader.var_opaque(1024, "callback gss client")?;
        }
        _ => {
            return Err(XdrError::new(
                "unknown callback security flavor",
                reader.offset(),
            ));
        }
    }
    Ok(())
}

fn skip_callback_sec_parms(reader: &mut XdrReader<'_>, label: &str) -> Result<(), XdrError> {
    let count = reader.u32(label)? as usize;
    if count > NFS4_MAX_SEC_PARMS {
        return Err(XdrError::new(
            "too many callback security parms",
            reader.offset(),
        ));
    }
    for _ in 0..count {
        skip_callback_sec_parm(reader)?;
    }
    Ok(())
}

fn skip_state_protect(reader: &mut XdrReader<'_>) -> Result<u32, XdrError> {
    let how = reader.u32("state_protect.how")?;
    match how {
        SP4_NONE => Ok(how),
        1 => {
            let _ = read_bitmap(reader)?;
            let _ = read_bitmap(reader)?;
            Ok(how)
        }
        2 => {
            let _ = read_bitmap(reader)?;
            let _ = read_bitmap(reader)?;
            let _ = reader.array(64, "hash_algs", |reader| reader.var_opaque(1024, "oid"))?;
            let _ = reader.array(64, "encr_algs", |reader| reader.var_opaque(1024, "oid"))?;
            let _ = reader.u32("ssv.window")?;
            let _ = reader.u32("ssv.handles")?;
            Ok(how)
        }
        _ => Err(XdrError::new(
            "unknown state protection scheme",
            reader.offset(),
        )),
    }
}

fn read_open(reader: &mut XdrReader<'_>) -> Result<OpenArgs, XdrError> {
    let seqid = reader.u32("OPEN.seqid")?;
    let share_access = reader.u32("OPEN.share_access")?;
    let share_deny = reader.u32("OPEN.share_deny")?;
    let _owner_clientid = reader.u64("OPEN.owner.clientid")?;
    let owner = reader.var_opaque(NFS4_OPAQUE_LIMIT, "OPEN.owner.owner")?;
    let open_type = reader.u32("OPEN.openhow")?;
    let mut create_mode = None;
    let mut create_attrs = None;
    let mut create_verf = None;
    if open_type == OPEN4_CREATE {
        let mode = reader.u32("OPEN.createhow.mode")?;
        create_mode = Some(mode);
        match mode {
            UNCHECKED4 | GUARDED4 => create_attrs = Some(read_attrs(reader)?),
            EXCLUSIVE4 => {
                create_verf = Some(
                    reader
                        .fixed_opaque(8, "OPEN.createverf")?
                        .try_into()
                        .expect("verifier size"),
                )
            }
            EXCLUSIVE4_1 => {
                create_verf = Some(
                    reader
                        .fixed_opaque(8, "OPEN.createverf")?
                        .try_into()
                        .expect("verifier size"),
                );
                create_attrs = Some(read_attrs(reader)?);
            }
            _ => return Err(XdrError::new("unknown OPEN create mode", reader.offset())),
        }
    }
    let claim = reader.u32("OPEN.claim")?;
    let name = match claim {
        CLAIM_NULL => Some(reader.string(NFS4_MAX_COMPONENT, "OPEN.claim.file")?),
        CLAIM_PREVIOUS => {
            let _ = reader.u32("OPEN.claim.delegate_type")?;
            None
        }
        CLAIM_FH | CLAIM_DELEG_PREV_FH => None,
        _ => return Err(XdrError::new("unsupported OPEN claim", reader.offset())),
    };
    Ok(OpenArgs {
        _seqid: seqid,
        share_access,
        share_deny,
        owner,
        open_type,
        create_mode,
        create_attrs,
        create_verf,
        claim,
        name,
    })
}

fn read_lock_owner(reader: &mut XdrReader<'_>, label: &str) -> Result<Vec<u8>, XdrError> {
    let _ = reader.u64(&format!("{label}.clientid"))?;
    reader.var_opaque(NFS4_OPAQUE_LIMIT, &format!("{label}.owner"))
}

fn read_locker(reader: &mut XdrReader<'_>) -> Result<Locker4, XdrError> {
    if reader.bool("LOCK.locker.new_lock_owner")? {
        let _ = reader.u32("LOCK.open_owner.open_seqid")?;
        let open_stateid = read_stateid(reader)?;
        let _ = reader.u32("LOCK.open_owner.lock_seqid")?;
        let owner = read_lock_owner(reader, "LOCK.open_owner.lock_owner")?;
        Ok(Locker4 {
            new_lock_owner: true,
            open_stateid: Some(open_stateid),
            lock_stateid: None,
            owner,
        })
    } else {
        let lock_stateid = read_stateid(reader)?;
        let _ = reader.u32("LOCK.exist_owner.lock_seqid")?;
        Ok(Locker4 {
            new_lock_owner: false,
            open_stateid: None,
            lock_stateid: Some(lock_stateid),
            owner: Vec::new(),
        })
    }
}

fn parse_op(reader: &mut XdrReader<'_>) -> Result<Op, XdrError> {
    let op = reader.u32("argop4.op")?;
    Ok(match op {
        OP_ACCESS => Op::Access(reader.u32("ACCESS.access")?),
        OP_CLOSE => Op::Close(reader.u32("CLOSE.seqid")?, read_stateid(reader)?),
        OP_COMMIT => Op::Commit {
            offset: reader.u64("COMMIT.offset")?,
            count: reader.u32("COMMIT.count")?,
        },
        OP_CREATE => {
            let kind = reader.u32("CREATE.type")?;
            let mut link = None;
            let mut dev = None;
            match kind {
                NF4LNK => link = Some(reader.string(NFS4_MAX_ATTRLIST, "CREATE.linkdata")?),
                NF4BLK | NF4CHR => {
                    dev = Some((
                        reader.u32("CREATE.specdata1")?,
                        reader.u32("CREATE.specdata2")?,
                    ))
                }
                NF4REG | NF4DIR | NF4FIFO | NF4SOCK | NF4ATTRDIR | NF4NAMEDATTR => {}
                _ => {}
            }
            let name = reader.string(NFS4_MAX_COMPONENT, "CREATE.name")?;
            let attrs = read_attrs(reader)?;
            Op::Create {
                kind,
                name,
                attrs,
                link,
                dev,
            }
        }
        OP_GETATTR => Op::Getattr(read_bitmap(reader)?),
        OP_GETFH => Op::Getfh,
        OP_LINK => Op::Link(reader.string(NFS4_MAX_COMPONENT, "LINK.name")?),
        OP_LOOKUP => Op::Lookup(reader.string(NFS4_MAX_COMPONENT, "LOOKUP.name")?),
        OP_LOOKUPP => Op::Lookupp,
        OP_LOCK => Op::Lock {
            lock_type: reader.u32("LOCK.locktype")?,
            reclaim: reader.bool("LOCK.reclaim")?,
            offset: reader.u64("LOCK.offset")?,
            length: reader.u64("LOCK.length")?,
            locker: read_locker(reader)?,
        },
        OP_LOCKT => Op::Lockt {
            lock_type: reader.u32("LOCKT.locktype")?,
            offset: reader.u64("LOCKT.offset")?,
            length: reader.u64("LOCKT.length")?,
            owner: read_lock_owner(reader, "LOCKT.owner")?,
        },
        OP_LOCKU => Op::Locku {
            lock_type: reader.u32("LOCKU.locktype")?,
            seqid: reader.u32("LOCKU.seqid")?,
            stateid: read_stateid(reader)?,
            offset: reader.u64("LOCKU.offset")?,
            length: reader.u64("LOCKU.length")?,
        },
        OP_NVERIFY => Op::Nverify(read_attrs(reader)?),
        OP_OPEN => Op::Open(read_open(reader)?),
        OP_OPEN_DOWNGRADE => Op::OpenDowngrade {
            stateid: read_stateid(reader)?,
            seqid: reader.u32("OPEN_DOWNGRADE.seqid")?,
            share_access: reader.u32("OPEN_DOWNGRADE.share_access")?,
            share_deny: reader.u32("OPEN_DOWNGRADE.share_deny")?,
        },
        OP_PUTFH => Op::Putfh(reader.var_opaque(NFS4_FHSIZE, "PUTFH.handle")?),
        OP_PUTPUBFH => Op::Putpubfh,
        OP_PUTROOTFH => Op::Putrootfh,
        OP_READ => Op::Read(
            read_stateid(reader)?,
            reader.u64("READ.offset")?,
            reader.u32("READ.count")?,
        ),
        OP_READDIR => Op::Readdir {
            cookie: reader.u64("READDIR.cookie")?,
            verifier: reader.fixed_opaque(8, "READDIR.cookieverf")?,
            dircount: reader.u32("READDIR.dircount")?,
            maxcount: reader.u32("READDIR.maxcount")?,
            attrs: read_bitmap(reader)?,
        },
        OP_READLINK => Op::Readlink,
        OP_REMOVE => Op::Remove(reader.string(NFS4_MAX_COMPONENT, "REMOVE.name")?),
        OP_RENAME => Op::Rename(
            reader.string(NFS4_MAX_COMPONENT, "RENAME.oldname")?,
            reader.string(NFS4_MAX_COMPONENT, "RENAME.newname")?,
        ),
        OP_RESTOREFH => Op::Restorefh,
        OP_SAVEFH => Op::Savefh,
        OP_SECINFO => Op::Secinfo(reader.string(NFS4_MAX_COMPONENT, "SECINFO.name")?),
        OP_SECINFO_NO_NAME => Op::SecinfoNoName(reader.u32("SECINFO_NO_NAME.style")?),
        OP_SETATTR => Op::Setattr(read_stateid(reader)?, read_attrs(reader)?),
        OP_VERIFY => Op::Verify(read_attrs(reader)?),
        OP_WRITE => Op::Write(
            read_stateid(reader)?,
            reader.u64("WRITE.offset")?,
            reader.u32("WRITE.stable")?,
            reader.var_opaque(DEFAULT_MAX_WRITE, "WRITE.data")?,
        ),
        OP_EXCHANGE_ID => {
            let verifier = reader.fixed_opaque(8, "EXCHANGE_ID.verifier")?;
            let owner = reader.var_opaque(NFS4_OPAQUE_LIMIT, "EXCHANGE_ID.owner")?;
            let flags = reader.u32("EXCHANGE_ID.flags")?;
            let state_protect = skip_state_protect(reader)?;
            let count = reader.u32("EXCHANGE_ID.impl count")? as usize;
            if count > 1 {
                return Err(XdrError::new(
                    "too many implementation IDs",
                    reader.offset(),
                ));
            }
            for _ in 0..count {
                let _ = reader.string(NFS4_OPAQUE_LIMIT, "impl.domain")?;
                let _ = reader.string(NFS4_OPAQUE_LIMIT, "impl.name")?;
                let _ = reader.i64("impl.date.seconds")?;
                let _ = reader.u32("impl.date.nseconds")?;
            }
            Op::ExchangeId {
                verifier,
                owner,
                flags,
                state_protect,
            }
        }
        OP_CREATE_SESSION => {
            let clientid = reader.u64("CREATE_SESSION.clientid")?;
            let sequence = reader.u32("CREATE_SESSION.sequence")?;
            let flags = reader.u32("CREATE_SESSION.flags")?;
            let fore = read_channel_attrs(reader)?;
            let back = read_channel_attrs(reader)?;
            let _ = reader.u32("CREATE_SESSION.cb_program")?;
            skip_callback_sec_parms(reader, "CREATE_SESSION.sec_parms count")?;
            Op::CreateSession {
                clientid,
                sequence,
                flags,
                fore,
                back,
            }
        }
        OP_DESTROY_SESSION => {
            Op::DestroySession(reader.fixed_opaque(NFS4_SESSIONID_SIZE, "DESTROY_SESSION.id")?)
        }
        OP_DESTROY_CLIENTID => Op::DestroyClientid(reader.u64("DESTROY_CLIENTID.clientid")?),
        OP_FREE_STATEID => Op::FreeStateid(read_stateid(reader)?),
        OP_SEQUENCE => Op::Sequence {
            sessionid: reader
                .fixed_opaque(NFS4_SESSIONID_SIZE, "SEQUENCE.sessionid")?
                .try_into()
                .expect("session id"),
            sequence: reader.u32("SEQUENCE.sequence")?,
            slot: reader.u32("SEQUENCE.slot")?,
            highest: reader.u32("SEQUENCE.highest")?,
            cachethis: reader.bool("SEQUENCE.cachethis")?,
        },
        OP_RECLAIM_COMPLETE => Op::ReclaimComplete(reader.bool("RECLAIM_COMPLETE.one_fs")?),
        OP_TEST_STATEID => Op::TestStateid(reader.array(
            NFS4_MAX_TEST_STATEIDS,
            "TEST_STATEID.stateids",
            |reader| read_stateid(reader),
        )?),
        OP_BIND_CONN_TO_SESSION => Op::BindConnToSession {
            sessionid: reader
                .fixed_opaque(NFS4_SESSIONID_SIZE, "BIND_CONN_TO_SESSION.id")?
                .try_into()
                .expect("session id"),
            direction: reader.u32("BIND_CONN_TO_SESSION.direction")?,
            use_rdma: reader.bool("BIND_CONN_TO_SESSION.use_conn_in_rdma_mode")?,
        },
        OP_BACKCHANNEL_CTL => {
            let _ = reader.u32("BACKCHANNEL_CTL.cb_program")?;
            skip_callback_sec_parms(reader, "BACKCHANNEL_CTL.sec_parms count")?;
            Op::BackchannelCtl
        }
        _ => Op::Unsupported(op),
    })
}

// ---------------------------------------------------------------------------
// NFSv4.1 session and filesystem service
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct V4OpResult {
    op: u32,
    status: u32,
    body: Vec<u8>,
}

#[derive(Debug, Clone)]
struct CreateSessionReplay {
    sequence: u32,
    result: V4OpResult,
}

impl V4OpResult {
    fn new(op: u32, status: u32) -> Self {
        let body = if op == OP_SETATTR {
            // SETATTR4res always contains attrsset, including on failure.
            // Whole-request failures have not applied any attributes.
            let mut body = XdrWriter::with_capacity(4);
            write_bitmap(&mut body, &[]);
            body.into_bytes()
        } else {
            Vec::new()
        };
        Self { op, status, body }
    }

    fn with_body(op: u32, status: u32, body: Vec<u8>) -> Self {
        Self { op, status, body }
    }

    fn write(&self, writer: &mut XdrWriter) {
        writer.u32(self.op);
        writer.u32(self.status);
        writer.raw(&self.body);
    }
}

impl Op {
    fn may_mutate(&self) -> bool {
        !matches!(
            self,
            Self::Access(_)
                | Self::Getattr(_)
                | Self::Getfh
                | Self::Lookup(_)
                | Self::Lookupp
                | Self::Lockt { .. }
                | Self::Nverify(_)
                | Self::Putfh(_)
                | Self::Putrootfh
                | Self::Putpubfh
                | Self::Read(..)
                | Self::Readdir { .. }
                | Self::Readlink
                | Self::Restorefh
                | Self::Savefh
                | Self::Secinfo(_)
                | Self::SecinfoNoName(_)
                | Self::Sequence { .. }
                | Self::TestStateid(_)
                | Self::Verify(_)
                | Self::BackchannelCtl
                | Self::Unsupported(_)
        )
    }

    fn opnum(&self) -> u32 {
        match self {
            Self::Access(_) => OP_ACCESS,
            Self::Close(_, _) => OP_CLOSE,
            Self::Commit { .. } => OP_COMMIT,
            Self::Create { .. } => OP_CREATE,
            Self::Getattr(_) => OP_GETATTR,
            Self::Getfh => OP_GETFH,
            Self::Link(_) => OP_LINK,
            Self::Lookup(_) => OP_LOOKUP,
            Self::Lookupp => OP_LOOKUPP,
            Self::Lock { .. } => OP_LOCK,
            Self::Lockt { .. } => OP_LOCKT,
            Self::Locku { .. } => OP_LOCKU,
            Self::Nverify(_) => OP_NVERIFY,
            Self::Open(_) => OP_OPEN,
            Self::OpenDowngrade { .. } => OP_OPEN_DOWNGRADE,
            Self::Putfh(_) => OP_PUTFH,
            Self::Putrootfh => OP_PUTROOTFH,
            Self::Putpubfh => OP_PUTPUBFH,
            Self::Read(_, _, _) => OP_READ,
            Self::Readdir { .. } => OP_READDIR,
            Self::Readlink => OP_READLINK,
            Self::Remove(_) => OP_REMOVE,
            Self::Rename(_, _) => OP_RENAME,
            Self::Restorefh => OP_RESTOREFH,
            Self::Savefh => OP_SAVEFH,
            Self::Secinfo(_) => OP_SECINFO,
            Self::SecinfoNoName(_) => OP_SECINFO_NO_NAME,
            Self::Setattr(_, _) => OP_SETATTR,
            Self::Verify(_) => OP_VERIFY,
            Self::Write(_, _, _, _) => OP_WRITE,
            Self::ExchangeId { .. } => OP_EXCHANGE_ID,
            Self::CreateSession { .. } => OP_CREATE_SESSION,
            Self::BindConnToSession { .. } => OP_BIND_CONN_TO_SESSION,
            Self::DestroySession(_) => OP_DESTROY_SESSION,
            Self::DestroyClientid(_) => OP_DESTROY_CLIENTID,
            Self::FreeStateid(_) => OP_FREE_STATEID,
            Self::Sequence { .. } => OP_SEQUENCE,
            Self::ReclaimComplete(_) => OP_RECLAIM_COMPLETE,
            Self::TestStateid(_) => OP_TEST_STATEID,
            Self::BackchannelCtl => OP_BACKCHANNEL_CTL,
            Self::Unsupported(operation) => *operation,
        }
    }
}

fn compound_body(status: u32, tag: &str, results: &[V4OpResult]) -> Vec<u8> {
    let mut writer = XdrWriter::with_capacity(64 + tag.len());
    writer.u32(status);
    writer.string(tag);
    writer.u32(results.len() as u32);
    for result in results {
        result.write(&mut writer);
    }
    writer.into_bytes()
}

fn sequence_body(
    sessionid: [u8; NFS4_SESSIONID_SIZE],
    sequence: u32,
    slot: u32,
    highest: u32,
) -> Vec<u8> {
    let mut writer = XdrWriter::with_capacity(40);
    writer.fixed_opaque(&sessionid, NFS4_SESSIONID_SIZE);
    writer.u32(sequence);
    writer.u32(slot);
    writer.u32(highest);
    writer.u32(highest);
    writer.u32(0);
    writer.into_bytes()
}

fn invalid_stateid() -> Stateid4 {
    Stateid4 {
        seqid: u32::MAX,
        other: [0; NFS4_OTHER_SIZE],
    }
}

fn is_zero_stateid(stateid: &Stateid4) -> bool {
    stateid.seqid == 0 && stateid.other == [0; NFS4_OTHER_SIZE]
}

fn is_invalid_stateid(stateid: &Stateid4) -> bool {
    stateid.seqid == u32::MAX && stateid.other == [0; NFS4_OTHER_SIZE]
}

fn supported_attr(bit: u32) -> bool {
    matches!(
        bit,
        FATTR4_SUPPORTED_ATTRS
            | FATTR4_TYPE
            | FATTR4_FH_EXPIRE_TYPE
            | FATTR4_CHANGE
            | FATTR4_SIZE
            | FATTR4_LINK_SUPPORT
            | FATTR4_SYMLINK_SUPPORT
            | FATTR4_NAMED_ATTR
            | FATTR4_FSID
            | FATTR4_UNIQUE_HANDLES
            | FATTR4_LEASE_TIME
            | FATTR4_RDATTR_ERROR
            | FATTR4_FILEHANDLE
            | FATTR4_FILEID
            | FATTR4_FILES_AVAIL
            | FATTR4_FILES_FREE
            | FATTR4_FILES_TOTAL
            | FATTR4_MAXFILESIZE
            | FATTR4_MAXLINK
            | FATTR4_MAXNAME
            | FATTR4_MAXREAD
            | FATTR4_MAXWRITE
            | FATTR4_MODE
            | FATTR4_NUMLINKS
            | FATTR4_OWNER
            | FATTR4_OWNER_GROUP
            | FATTR4_RAWDEV
            | FATTR4_SPACE_AVAIL
            | FATTR4_SPACE_FREE
            | FATTR4_SPACE_TOTAL
            | FATTR4_SPACE_USED
            | FATTR4_TIME_ACCESS
            | FATTR4_TIME_ACCESS_SET
            | FATTR4_TIME_DELTA
            | FATTR4_TIME_METADATA
            | FATTR4_TIME_MODIFY
            | FATTR4_TIME_MODIFY_SET
    )
}

fn set_only_attr(bit: u32) -> bool {
    matches!(bit, FATTR4_TIME_ACCESS_SET | FATTR4_TIME_MODIFY_SET)
}

fn gettable_attr(bit: u32) -> bool {
    supported_attr(bit) && !set_only_attr(bit)
}

fn open_access_bits(value: u32) -> Option<u32> {
    let allowed = OPEN4_SHARE_ACCESS_BOTH
        | OPEN4_SHARE_ACCESS_WANT_DELEG_MASK
        | OPEN4_SHARE_ACCESS_WANT_SIGNAL_DELEG_WHEN_RESRC_AVAIL
        | OPEN4_SHARE_ACCESS_WANT_PUSH_DELEG_WHEN_UNCONTENDED;
    let access = value & OPEN4_SHARE_ACCESS_BOTH;
    (access != 0 && value & !allowed == 0).then_some(access)
}

fn supported_attrs(statfs: bool, times: bool) -> Vec<u32> {
    let mut attrs = vec![
        FATTR4_SUPPORTED_ATTRS,
        FATTR4_TYPE,
        FATTR4_FH_EXPIRE_TYPE,
        FATTR4_CHANGE,
        FATTR4_SIZE,
        FATTR4_LINK_SUPPORT,
        FATTR4_SYMLINK_SUPPORT,
        FATTR4_NAMED_ATTR,
        FATTR4_FSID,
        FATTR4_UNIQUE_HANDLES,
        FATTR4_LEASE_TIME,
        FATTR4_RDATTR_ERROR,
        FATTR4_FILEHANDLE,
        FATTR4_FILEID,
        FATTR4_MAXFILESIZE,
        FATTR4_MAXLINK,
        FATTR4_MAXNAME,
        FATTR4_MAXREAD,
        FATTR4_MAXWRITE,
        FATTR4_MODE,
        FATTR4_NUMLINKS,
        FATTR4_OWNER,
        FATTR4_OWNER_GROUP,
        FATTR4_RAWDEV,
        FATTR4_SPACE_USED,
        FATTR4_TIME_ACCESS,
        FATTR4_TIME_DELTA,
        FATTR4_TIME_METADATA,
        FATTR4_TIME_MODIFY,
    ];
    if statfs {
        attrs.extend([
            FATTR4_FILES_AVAIL,
            FATTR4_FILES_FREE,
            FATTR4_FILES_TOTAL,
            FATTR4_SPACE_AVAIL,
            FATTR4_SPACE_FREE,
            FATTR4_SPACE_TOTAL,
        ]);
    }
    if times {
        attrs.extend([FATTR4_TIME_ACCESS_SET, FATTR4_TIME_MODIFY_SET]);
    }
    bitmap_of(attrs)
}

fn advertised_attr(bit: u32, statfs: bool, times: bool) -> bool {
    supported_attr(bit)
        && (statfs
            || !matches!(
                bit,
                FATTR4_FILES_AVAIL
                    | FATTR4_FILES_FREE
                    | FATTR4_FILES_TOTAL
                    | FATTR4_SPACE_AVAIL
                    | FATTR4_SPACE_FREE
                    | FATTR4_SPACE_TOTAL
            ))
        && (times || !matches!(bit, FATTR4_TIME_ACCESS_SET | FATTR4_TIME_MODIFY_SET))
}

fn error_status(error: &FsError) -> u32 {
    match error.code {
        ErrorCode::Eperm => NFS4ERR_PERM,
        ErrorCode::Enoent => NFS4ERR_NOENT,
        ErrorCode::Eio => NFS4ERR_IO,
        ErrorCode::Eacces => NFS4ERR_ACCESS,
        ErrorCode::Eexist => NFS4ERR_EXIST,
        ErrorCode::Enotdir => NFS4ERR_NOTDIR,
        ErrorCode::Eisdir => NFS4ERR_ISDIR,
        ErrorCode::Einval => NFS4ERR_INVAL,
        ErrorCode::Enospc => NFS4ERR_NOSPC,
        ErrorCode::Erofs => NFS4ERR_ROFS,
        ErrorCode::Enametoolong => NFS4ERR_NAMETOOLONG,
        ErrorCode::Enotempty => NFS4ERR_NOTEMPTY,
        ErrorCode::Estale => NFS4ERR_STALE,
        ErrorCode::Enotsup | ErrorCode::Enosys => NFS4ERR_NOTSUPP,
        ErrorCode::Efbig => NFS4ERR_FBIG,
        ErrorCode::Edquot => NFS4ERR_DQUOT,
        ErrorCode::Exdev => NFS4ERR_XDEV,
        ErrorCode::Enodev | ErrorCode::Enxio => NFS4ERR_IO,
        _ => NFS4ERR_SERVERFAULT,
    }
}

fn type_of(stats: &Stats) -> u32 {
    match stats.mode & S_IFMT {
        S_IFREG => NF4REG,
        S_IFDIR => NF4DIR,
        S_IFBLK => NF4BLK,
        S_IFCHR => NF4CHR,
        S_IFLNK => NF4LNK,
        S_IFSOCK => NF4SOCK,
        S_IFIFO => NF4FIFO,
        _ => NF4REG,
    }
}

fn stat_change(stats: &Stats) -> u64 {
    let value = if stats.ctime_ms >= 0 {
        stats.ctime_ms as u64
    } else {
        0
    };
    value.saturating_mul(1_000_000)
}

fn join_path(directory: &str, name: &str) -> String {
    mount_rs_core::path::normalize_path(&format!("{directory}/{name}"))
}

fn check_name(name: &str) -> Result<(), FsError> {
    if name.is_empty() || name.contains('/') || name == "." || name == ".." {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message(format!("invalid NFSv4 directory entry name {name:?}")));
    }
    if name.len() > NFS4_MAX_COMPONENT {
        return Err(FsError::new(ErrorCode::Enametoolong));
    }
    Ok(())
}

fn offset(value: u64, syscall: &str) -> Result<u64, FsError> {
    if value > MAX_OFFSET {
        Err(FsError::new(ErrorCode::Einval).with_syscall(syscall))
    } else {
        Ok(value)
    }
}

#[derive(Clone)]
pub struct Nfs4Session {
    pub driver: Loopback,
    pub options: NfsSessionOptions,
    pub handles: FileHandleTable,
    pub write_verifier: [u8; 8],
    stats: SharedStats,
    snapshots: DirectorySnapshots,
    state: Arc<Mutex<V4State>>,
    destroyed: Arc<Mutex<bool>>,
    path_lock: Arc<tokio::sync::RwLock<()>>,
    hooks: NfsSessionHooks,
}

impl Nfs4Session {
    pub fn new<D>(driver: D, options: NfsSessionOptions) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::from_loopback(Loopback::new(driver), options)
    }

    /// Construct a session with a request-level error hook.
    pub fn new_with_hooks<D>(driver: D, options: NfsSessionOptions, hooks: NfsSessionHooks) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::from_loopback_with_hooks(Loopback::new(driver), options, hooks)
    }

    pub fn from_arc(driver: Arc<dyn FsDriver>, options: NfsSessionOptions) -> Self {
        Self::from_loopback(Loopback::from_arc(driver), options)
    }

    pub fn from_loopback(driver: Loopback, options: NfsSessionOptions) -> Self {
        let shared = SharedNfsState::new(&options);
        Self::from_loopback_shared(driver, options, &shared)
    }

    /// Construct a loopback session with a request-level error hook.
    pub fn from_loopback_with_hooks(
        driver: Loopback,
        options: NfsSessionOptions,
        hooks: NfsSessionHooks,
    ) -> Self {
        let shared = SharedNfsState::new(&options);
        Self::from_loopback_shared_with_hooks(driver, options, &shared, hooks)
    }

    pub(crate) fn from_loopback_shared(
        driver: Loopback,
        options: NfsSessionOptions,
        shared: &SharedNfsState,
    ) -> Self {
        Self::from_loopback_shared_with_hooks(driver, options, shared, NfsSessionHooks::default())
    }

    pub(crate) fn from_loopback_shared_with_hooks(
        driver: Loopback,
        options: NfsSessionOptions,
        shared: &SharedNfsState,
        hooks: NfsSessionHooks,
    ) -> Self {
        let handles = shared.handles.clone();
        let write_verifier = handles.verifier();
        Self {
            driver,
            options: options.clone(),
            handles,
            write_verifier,
            stats: shared.stats.clone(),
            snapshots: DirectorySnapshots::new(options.snapshot_cache),
            state: Arc::new(Mutex::new(V4State {
                seed: options.nfs4.seed,
                next_clientid: 1,
                next_session: 1,
                ..V4State::default()
            })),
            destroyed: Arc::new(Mutex::new(false)),
            path_lock: Arc::clone(&shared.path_lock),
            hooks,
        }
    }

    pub fn stats(&self) -> NfsSessionStats {
        self.stats.0.lock().expect("NFS stats lock").clone()
    }

    pub fn destroyed(&self) -> bool {
        *self.destroyed.lock().expect("NFSv4 destroyed lock")
    }

    fn report_error(&self, error: NfsSessionError, call: Option<crate::rpc::RpcCall>) {
        self.hooks.report(error, call);
    }

    fn record_stat_error(&self) {
        let mut stats = self.stats.0.lock().expect("NFS stats lock");
        stats.errors = stats.errors.saturating_add(1);
    }

    fn record_status_error(&self, status: u32) {
        if status == NFS4_OK {
            return;
        }
        self.record_stat_error();
        self.report_error(NfsSessionError::status(status), None);
    }

    fn compound_error_body(&self, status: u32, tag: &str, results: &[V4OpResult]) -> Vec<u8> {
        self.record_status_error(status);
        compound_body(status, tag, results)
    }

    fn now(&self) -> Instant {
        self.options.nfs4.clock.now()
    }

    fn lease_duration(&self) -> Duration {
        Duration::from_secs(u64::from(self.options.nfs4.lease_seconds.max(1)))
    }

    fn expired(&self, renewed: Instant, now: Instant) -> bool {
        now.saturating_duration_since(renewed) >= self.lease_duration()
    }

    fn has_expired_clients(&self) -> bool {
        let now = self.now();
        self.state
            .lock()
            .expect("NFSv4 state lock")
            .clients
            .values()
            .any(|client| self.expired(client.renewed, now))
    }

    /// Remove clients whose leases have expired, including their sessions,
    /// locks, open states, and pinned backend handles.
    async fn expire_expired_clients(&self) -> usize {
        let (removed, handles) = {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            let now = self.now();
            let expired = state
                .clients
                .iter()
                .filter_map(|(clientid, client)| {
                    self.expired(client.renewed, now).then_some(*clientid)
                })
                .collect::<Vec<_>>();
            let mut handles = Vec::new();
            let mut removed = 0;
            for clientid in expired {
                if state.clients.remove(&clientid).is_none() {
                    continue;
                }
                state.owners.retain(|_, owner| *owner != clientid);
                state
                    .sessions
                    .retain(|_, session| session.clientid != clientid);
                state.locks.retain(|_, lock| lock.clientid != clientid);
                let open_ids = state
                    .opens
                    .iter()
                    .filter_map(|(other, open)| (open.clientid == clientid).then_some(*other))
                    .collect::<Vec<_>>();
                for other in open_ids {
                    if let Some(open) = state.opens.remove(&other) {
                        handles.push((open.handle_id, open.handle));
                    }
                }
                removed += 1;
            }
            (removed, handles)
        };
        for (handle_id, handle) in handles {
            self.handles.unpin(handle_id);
            let _ = handle.close().await;
        }
        removed
    }

    /// Sweep expired NFSv4 clients and release their process-local state.
    pub async fn sweep_expired(&self) -> usize {
        let _guard = self.path_lock.write().await;
        self.expire_expired_clients().await
    }

    pub async fn destroy(&self) {
        *self.destroyed.lock().expect("NFSv4 destroyed lock") = true;
        self.handles.clear();
        self.snapshots.clear();
        let open_handles = {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            state.clients.clear();
            state.owners.clear();
            state.sessions.clear();
            let open_handles = state
                .opens
                .drain()
                .map(|(_, open)| (open.handle_id, open.handle))
                .collect::<Vec<_>>();
            state.locks.clear();
            state.exclusive_creates.clear();
            open_handles
        };
        for (handle_id, handle) in open_handles {
            self.handles.unpin(handle_id);
            let _ = handle.close().await;
        }
    }

    /// Handle one unframed RPC message.  The v4 service accepts only the
    /// standard NULL and COMPOUND procedures and never treats a malformed RPC
    /// body as a filesystem operation.
    pub async fn handle_call(&self, message: &[u8], context: NfsRequestContext) -> Option<Vec<u8>> {
        self.record_request(message);
        let reply = self.handle_call_inner(message, context).await;
        let mut stats = self.stats.0.lock().expect("NFS stats lock");
        if reply.is_some() {
            stats.replies = stats.replies.saturating_add(1);
        } else {
            stats.dropped = stats.dropped.saturating_add(1);
        }
        reply
    }

    fn record_request(&self, message: &[u8]) {
        let mut stats = self.stats.0.lock().expect("NFS stats lock");
        stats.requests = stats.requests.saturating_add(1);
        let name = decode_call(message)
            .ok()
            .map(|(call, _)| match call.procedure {
                NFSPROC4_NULL => "NFS4:NULL".to_owned(),
                NFSPROC4_COMPOUND => "NFS4:COMPOUND".to_owned(),
                procedure => format!("NFS4:{procedure}"),
            });
        if let Some(name) = name {
            stats
                .procedures
                .entry(name)
                .and_modify(|count| *count = count.saturating_add(1))
                .or_insert(1);
        }
    }

    async fn handle_call_inner(
        &self,
        message: &[u8],
        context: NfsRequestContext,
    ) -> Option<Vec<u8>> {
        let peer = context.peer.as_deref();
        let (call, mut args) = match decode_call(message) {
            Ok(value) => value,
            Err(error) => {
                v4_trace(
                    "rpc-drop",
                    peer,
                    0,
                    format_args!(
                        "bytes={} decode_error_offset={}",
                        message.len(),
                        error.offset
                    ),
                );
                return None;
            }
        };
        v4_trace(
            "rpc-call",
            peer,
            call.xid,
            format_args!(
                "program={} version={} procedure={} args_bytes={}",
                call.program,
                call.version,
                call.procedure,
                args.remaining()
            ),
        );
        if call.rpc_version != RPC_VERSION {
            v4_trace(
                "rpc-reply",
                peer,
                call.xid,
                format_args!("status=rpc-version-mismatch"),
            );
            return Some(encode_rpc_mismatch(call.xid, RPC_VERSION, RPC_VERSION));
        }
        if call.cred.flavor != AUTH_NONE && call.cred.flavor != AUTH_SYS {
            v4_trace(
                "rpc-reply",
                peer,
                call.xid,
                format_args!("status=auth-too-weak"),
            );
            return Some(encode_auth_error(call.xid, AUTH_TOOWEAK));
        }
        if call.program != NFS4_PROGRAM {
            v4_trace(
                "rpc-reply",
                peer,
                call.xid,
                format_args!("status=program-unavailable"),
            );
            return Some(encode_accept_error(call.xid, RPC_PROG_UNAVAIL, None));
        }
        if call.version != NFS_V4 {
            v4_trace(
                "rpc-reply",
                peer,
                call.xid,
                format_args!("status=program-version-mismatch"),
            );
            return Some(encode_accept_error(
                call.xid,
                RPC_PROG_MISMATCH,
                Some((4, 4)),
            ));
        }
        if call.procedure == NFSPROC4_NULL {
            v4_trace("rpc-reply", peer, call.xid, format_args!("status=null"));
            return Some(encode_accepted_reply(call.xid, &[]));
        }
        if call.procedure != NFSPROC4_COMPOUND {
            v4_trace(
                "rpc-reply",
                peer,
                call.xid,
                format_args!("status=procedure-unavailable"),
            );
            return Some(encode_accept_error(call.xid, RPC_PROC_UNAVAIL, None));
        }
        if let Some((status, tag)) = self.busy_sequence_status(args) {
            v4_trace_compound_reply(peer, call.xid, status, 0, false);
            return Some(encode_accepted_reply(
                call.xid,
                &self.compound_error_body(status, &tag, &[]),
            ));
        }
        let credentials = credentials_of(&call.cred);
        let _guard = loop {
            // Keep ordinary compounds on the shared read path. Only an actual
            // expired lease needs the exclusive path-map gate for cleanup.
            let guard = self.path_lock.read().await;
            if !self.has_expired_clients() {
                break guard;
            }
            drop(guard);
            let expiry_guard = self.path_lock.write().await;
            self.expire_expired_clients().await;
            drop(expiry_guard);
        };
        match self
            .dispatch_compound(&mut args, &credentials, peer, call.xid)
            .await
        {
            Ok(body) => Some(encode_accepted_reply(call.xid, &body)),
            Err(error) => {
                v4_trace(
                    "compound-reply",
                    peer,
                    call.xid,
                    format_args!("status=rpc-garbage-args offset={}", error.offset),
                );
                self.record_stat_error();
                self.report_error(error.clone().into(), Some(call.clone()));
                Some(encode_accept_error(call.xid, RPC_GARBAGE_ARGS, None))
            }
        }
    }

    fn busy_sequence_status(&self, mut reader: XdrReader<'_>) -> Option<(u32, String)> {
        // Answer an already-running slot before the per-RPC lease sweep waits
        // for the original COMPOUND's path-lock read guard.
        let tag = reader.string(NFS4_MAX_TAG, "COMPOUND.tag").ok()?;
        let minor = reader.u32("COMPOUND.minorversion").ok()?;
        let count = reader.u32("COMPOUND.argarray count").ok()? as usize;
        if minor != NFS4_MINOR_VERSION_1 || count == 0 || count > NFS4_MAX_COMPOUND_OPS {
            return None;
        }
        let Op::Sequence {
            sessionid,
            sequence,
            slot,
            ..
        } = parse_op(&mut reader).ok()?
        else {
            return None;
        };
        for _ in 1..count {
            if matches!(parse_op(&mut reader).ok()?, Op::Unsupported(_)) {
                return None;
            }
        }
        reader.end("COMPOUND arguments").ok()?;
        let state = self.state.lock().expect("NFSv4 state lock");
        let active = state
            .sessions
            .get(&sessionid)?
            .in_flight
            .get(usize::try_from(slot).ok()?)
            .copied()
            .flatten()?;
        Some((
            if sequence == active {
                NFS4ERR_DELAY
            } else {
                NFS4ERR_SEQ_MISORDERED
            },
            tag,
        ))
    }

    async fn dispatch_compound(
        &self,
        reader: &mut XdrReader<'_>,
        credentials: &RpcCredentials,
        peer: Option<&str>,
        xid: u32,
    ) -> Result<Vec<u8>, XdrError> {
        let tag = reader.string(NFS4_MAX_TAG, "COMPOUND.tag")?;
        let minor = reader.u32("COMPOUND.minorversion")?;
        let count = reader.u32("COMPOUND.argarray count")? as usize;
        v4_trace(
            "compound-header",
            peer,
            xid,
            format_args!("minor={minor} declared_ops={count} tag_bytes={}", tag.len()),
        );
        if count > NFS4_MAX_COMPOUND_OPS {
            v4_trace_compound_reply(peer, xid, NFS4ERR_TOO_MANY_OPS, 0, false);
            return Ok(self.compound_error_body(NFS4ERR_TOO_MANY_OPS, &tag, &[]));
        }
        let mut operations = Vec::with_capacity(count);
        for _ in 0..count {
            let operation = match parse_op(reader) {
                Ok(operation) => operation,
                Err(error) => {
                    v4_trace(
                        "compound-parse-error",
                        peer,
                        xid,
                        format_args!("offset={}", error.offset),
                    );
                    return Err(error);
                }
            };
            let unsupported = matches!(operation, Op::Unsupported(_));
            operations.push(operation);
            if unsupported {
                break;
            }
        }
        if !reader.at_end()
            && !operations
                .iter()
                .any(|operation| matches!(operation, Op::Unsupported(_)))
            && let Err(error) = reader.end("COMPOUND arguments")
        {
            v4_trace(
                "compound-parse-error",
                peer,
                xid,
                format_args!("offset={}", error.offset),
            );
            return Err(error);
        }
        let opcodes = v4_opcodes(&operations);
        v4_trace(
            "compound-ops",
            peer,
            xid,
            format_args!(
                "minor={minor} decoded_ops={} opcodes={opcodes}",
                operations.len()
            ),
        );
        if minor != NFS4_MINOR_VERSION_1 {
            v4_trace_compound_reply(peer, xid, NFS4ERR_MINOR_VERS_MISMATCH, 0, false);
            return Ok(self.compound_error_body(NFS4ERR_MINOR_VERS_MISMATCH, &tag, &[]));
        }
        if operations.is_empty() {
            v4_trace_compound_reply(peer, xid, NFS4_OK, 0, false);
            return Ok(compound_body(NFS4_OK, &tag, &[]));
        }

        let first_is_sequence = matches!(operations.first(), Some(Op::Sequence { .. }));
        if !first_is_sequence {
            if operations.len() != 1 {
                v4_trace_compound_reply(peer, xid, NFS4ERR_NOT_ONLY_OP, 0, false);
                return Ok(self.compound_error_body(NFS4ERR_NOT_ONLY_OP, &tag, &[]));
            }
            if !is_sessionless(&operations[0]) {
                v4_trace_compound_reply(peer, xid, NFS4ERR_OP_NOT_IN_SESSION, 0, false);
                return Ok(self.compound_error_body(NFS4ERR_OP_NOT_IN_SESSION, &tag, &[]));
            }
            let result = self
                .execute_op(&operations[0], &mut Cursor::default(), credentials)
                .await;
            let status = result.status;
            self.record_status_error(status);
            v4_trace_compound_reply(peer, xid, status, 1, false);
            return Ok(compound_body(status, &tag, &[result]));
        }

        let Op::Sequence {
            sessionid,
            sequence,
            slot,
            highest,
            cachethis,
        } = &operations[0]
        else {
            unreachable!("sequence was checked above")
        };
        let (session, mut in_flight) = {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            let Some(session) = state.sessions.get(sessionid) else {
                v4_trace_compound_reply(peer, xid, NFS4ERR_BADSESSION, 0, false);
                drop(state);
                return Ok(self.compound_error_body(NFS4ERR_BADSESSION, &tag, &[]));
            };
            let slot_index = usize::try_from(*slot).unwrap_or(usize::MAX);
            if slot_index >= session.next_sequence.len() {
                v4_trace_compound_reply(peer, xid, NFS4ERR_BADSLOT, 0, false);
                drop(state);
                return Ok(self.compound_error_body(NFS4ERR_BADSLOT, &tag, &[]));
            }
            if let Some(active) = session.in_flight[slot_index] {
                let status = if *sequence == active {
                    NFS4ERR_DELAY
                } else {
                    NFS4ERR_SEQ_MISORDERED
                };
                v4_trace_compound_reply(peer, xid, status, 0, false);
                drop(state);
                return Ok(self.compound_error_body(status, &tag, &[]));
            }
            let expected = session.next_sequence[slot_index];
            let clientid = session.clientid;
            if *sequence == expected {
                let session = state
                    .sessions
                    .get_mut(sessionid)
                    .expect("validated session while holding state lock");
                session.next_sequence[slot_index] = expected.wrapping_add(1);
                session.in_flight[slot_index] = Some(*sequence);
                let in_flight = InFlightSlot {
                    state: Arc::clone(&self.state),
                    sessionid: *sessionid,
                    slot: slot_index,
                    sequence: *sequence,
                    completed: false,
                };
                if let Some(client) = state.clients.get_mut(&clientid) {
                    client.renewed = self.now();
                }
                (
                    state
                        .sessions
                        .get(sessionid)
                        .expect("validated session while holding state lock")
                        .clone(),
                    in_flight,
                )
            } else if let Some(cached) = state
                .sessions
                .get(sessionid)
                .and_then(|session| session.cached[slot_index].as_ref())
                .filter(|cached| cached.sequence == *sequence)
                .cloned()
            {
                // AUTH_SYS stamps/machine names may change on retransmission;
                // compare the decoded effective credentials, not raw bytes.
                if cached.credentials != *credentials {
                    v4_trace_compound_reply(peer, xid, NFS4ERR_SEQ_FALSE_RETRY, 1, false);
                    drop(state);
                    return Ok(self.compound_error_body(
                        NFS4ERR_SEQ_FALSE_RETRY,
                        &tag,
                        &[V4OpResult::new(OP_SEQUENCE, NFS4ERR_SEQ_FALSE_RETRY)],
                    ));
                }
                if let Some(client) = state.clients.get_mut(&clientid) {
                    client.renewed = self.now();
                }
                v4_trace_body_reply(peer, xid, &cached.body, true);
                return Ok(cached.body);
            } else {
                v4_trace_compound_reply(peer, xid, NFS4ERR_SEQ_MISORDERED, 0, false);
                drop(state);
                return Ok(self.compound_error_body(NFS4ERR_SEQ_MISORDERED, &tag, &[]));
            }
        };
        if *highest >= session.next_sequence.len() as u32 && *highest != 0 {
            v4_trace_compound_reply(peer, xid, NFS4ERR_BADSLOT, 0, false);
            in_flight.complete();
            return Ok(self.compound_error_body(NFS4ERR_BADSLOT, &tag, &[]));
        }
        if operations.len() > session.max_operations as usize {
            v4_trace_compound_reply(peer, xid, NFS4ERR_TOO_MANY_OPS, 0, false);
            in_flight.complete();
            return Ok(self.compound_error_body(NFS4ERR_TOO_MANY_OPS, &tag, &[]));
        }
        debug_assert_eq!(session.id, *sessionid);
        let mut cursor = Cursor {
            clientid: Some(session.clientid),
            session: Some(*sessionid),
            ..Cursor::default()
        };
        let sequence_result = V4OpResult::with_body(
            OP_SEQUENCE,
            NFS4_OK,
            sequence_body(
                *sessionid,
                *sequence,
                *slot,
                session.next_sequence.len().saturating_sub(1) as u32,
            ),
        );
        let mut results = vec![sequence_result];
        let mut status = NFS4_OK;
        let mut may_have_mutated = false;
        for operation in operations.iter().skip(1) {
            may_have_mutated |= operation.may_mutate();
            let mut result = self.execute_op(operation, &mut cursor, credentials).await;
            if *cachethis
                && result.status == NFS4_OK
                && matches!(
                    operation,
                    Op::Getattr(_) | Op::Getfh | Op::Read(..) | Op::Readdir { .. } | Op::Readlink
                )
            {
                // These read-only operations have no mutation to replay. If a
                // result would overflow a required cache,
                // retain earlier results and cache a bounded error here.
                let mut candidate = results.clone();
                candidate.push(result.clone());
                if compound_body(NFS4_OK, &tag, &candidate).len() > session.max_cached {
                    let error = V4OpResult::new(operation.opnum(), NFS4ERR_REP_TOO_BIG_TO_CACHE);
                    candidate.pop();
                    candidate.push(error.clone());
                    if compound_body(NFS4ERR_REP_TOO_BIG_TO_CACHE, &tag, &candidate).len()
                        <= session.max_cached
                    {
                        result = error;
                    }
                }
            }
            status = result.status;
            results.push(result);
            if status != NFS4_OK {
                self.record_status_error(status);
                v4_trace(
                    "compound-op-status",
                    peer,
                    xid,
                    format_args!(
                        "op={} name={} status={status}",
                        operation.opnum(),
                        v4_op_name(operation.opnum())
                    ),
                );
                break;
            }
        }
        let body = compound_body(status, &tag, &results);
        v4_trace_compound_reply(peer, xid, status, results.len(), false);
        // RFC 8881 permits caching the full reply even when sa_cachethis is
        // false. Keep bounded completed replies so a retry cannot re-execute
        // a mutation whose caller omitted the caching hint.
        let cached_body = if body.len() <= session.max_cached {
            Some(body.clone())
        } else if !*cachethis {
            // If the full uncached reply is too large, retain SEQUENCE plus
            // RETRY_UNCACHED_REP on the original second operation. Never
            // execute any operation from a retry of this slot/sequence.
            operations
                .get(1)
                .filter(|operation| !matches!(operation, Op::Unsupported(_)))
                .map(|operation| {
                    compound_body(
                        NFS4ERR_RETRY_UNCACHED_REP,
                        &tag,
                        &[
                            results[0].clone(),
                            V4OpResult::new(operation.opnum(), NFS4ERR_RETRY_UNCACHED_REP),
                        ],
                    )
                })
                .filter(|reply| reply.len() <= session.max_cached)
        } else {
            None
        };
        let reply_was_cached = cached_body.is_some();
        if let Some(cached_body) = cached_body {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            if let Some(session) = state.sessions.get_mut(sessionid) {
                let slot_index = *slot as usize;
                if slot_index < session.cached.len() {
                    session.cached[slot_index] = Some(CachedReply {
                        sequence: *sequence,
                        body: cached_body,
                        credentials: credentials.clone(),
                    });
                }
            }
        }
        if reply_was_cached || !may_have_mutated {
            in_flight.complete();
        }
        // An attempted mutation without a cacheable reply cannot be replayed
        // safely. Keep the slot incomplete so its drop fences the session,
        // including when the negotiated cache is too small for an error.
        Ok(body)
    }

    async fn execute_op(
        &self,
        operation: &Op,
        cursor: &mut Cursor,
        credentials: &RpcCredentials,
    ) -> V4OpResult {
        let op = operation.opnum();
        match operation {
            Op::ExchangeId { .. } => self.exchange_id(operation).await,
            Op::CreateSession { .. } => self.create_session(operation).await,
            Op::BindConnToSession {
                sessionid,
                direction,
                use_rdma,
            } => self.bind_conn_to_session(*sessionid, *direction, *use_rdma),
            Op::DestroySession(id) => self.destroy_session(id).await,
            Op::DestroyClientid(clientid) => self.destroy_clientid(*clientid).await,
            Op::FreeStateid(stateid) => self.free_stateid(stateid, cursor),
            Op::Sequence { .. } => V4OpResult::new(op, NFS4ERR_SEQUENCE_POS),
            Op::ReclaimComplete(one_fs) => self.reclaim_complete(*one_fs, cursor),
            Op::TestStateid(stateids) => self.test_stateid(stateids, cursor),
            Op::BackchannelCtl => V4OpResult::new(OP_BACKCHANNEL_CTL, NFS4ERR_INVAL),
            Op::Putrootfh => self.put_root(cursor, OP_PUTROOTFH),
            Op::Putpubfh => self.put_root(cursor, OP_PUTPUBFH),
            Op::Putfh(handle) => self.put_fh(handle, cursor),
            Op::Getfh => self.get_fh(cursor),
            Op::Savefh => self.save_fh(cursor),
            Op::Restorefh => self.restore_fh(cursor),
            Op::Lookup(name) => self.lookup(name, cursor).await,
            Op::Lookupp => self.lookupp(cursor).await,
            Op::Lock {
                lock_type,
                reclaim,
                offset,
                length,
                locker,
            } => {
                self.lock(*lock_type, *reclaim, *offset, *length, locker, cursor)
                    .await
            }
            Op::Lockt {
                lock_type,
                offset,
                length,
                owner,
            } => {
                self.lockt(*lock_type, *offset, *length, owner, cursor)
                    .await
            }
            Op::Locku {
                lock_type,
                seqid,
                stateid,
                offset,
                length,
            } => {
                self.locku(*lock_type, *seqid, stateid, *offset, *length, cursor)
                    .await
            }
            Op::Getattr(mask) => self.getattr(mask, cursor).await,
            Op::Setattr(stateid, attrs) => self.setattr(stateid, attrs, cursor).await,
            Op::Access(mask) => self.access(*mask, cursor, credentials).await,
            Op::Readlink => self.readlink(cursor).await,
            Op::Readdir {
                cookie,
                verifier,
                dircount,
                maxcount,
                attrs,
            } => {
                self.readdir(*cookie, verifier, *dircount, *maxcount, attrs, cursor)
                    .await
            }
            Op::Commit { offset, count } => self.commit(*offset, *count, cursor).await,
            Op::Create {
                kind,
                name,
                attrs,
                link,
                dev,
            } => {
                self.create(*kind, name, attrs, link.as_deref(), *dev, cursor)
                    .await
            }
            Op::Remove(name) => self.remove(name, cursor).await,
            Op::Rename(old, new) => self.rename(old, new, cursor).await,
            Op::Link(name) => self.link(name, cursor).await,
            Op::Open(args) => self.open(args, cursor).await,
            Op::OpenDowngrade {
                stateid,
                seqid,
                share_access,
                share_deny,
            } => self.open_downgrade(stateid, *seqid, *share_access, *share_deny, cursor),
            Op::Close(seqid, stateid) => self.close(*seqid, stateid, cursor).await,
            Op::Read(stateid, offset, count) => self.read(stateid, *offset, *count, cursor).await,
            Op::Write(stateid, offset, stable, data) => {
                self.write(stateid, *offset, *stable, data, cursor).await
            }
            Op::Secinfo(name) => self.secinfo(name, cursor).await,
            Op::SecinfoNoName(style) => self.secinfo_no_name(*style, cursor),
            Op::Verify(attrs) => self.verify(attrs, cursor, false).await,
            Op::Nverify(attrs) => self.verify(attrs, cursor, true).await,
            Op::Unsupported(operation) => V4OpResult::new(
                *operation,
                if *operation == OP_ILLEGAL {
                    NFS4ERR_OP_ILLEGAL
                } else {
                    NFS4ERR_NOTSUPP
                },
            ),
        }
    }

    async fn exchange_id(&self, operation: &Op) -> V4OpResult {
        let Op::ExchangeId {
            verifier,
            owner,
            flags,
            state_protect,
        } = operation
        else {
            unreachable!("EXCHANGE_ID operation variant")
        };
        const EXCHGID4_FLAG_ARG_MASK: u32 = EXCHGID4_FLAG_SUPP_MOVED_REFER
            | EXCHGID4_FLAG_SUPP_MOVED_MIGR
            | EXCHGID4_FLAG_BIND_PRINC_STATEID
            | EXCHGID4_FLAG_USE_NON_PNFS
            | EXCHGID4_FLAG_USE_PNFS_MDS
            | EXCHGID4_FLAG_USE_PNFS_DS
            | EXCHGID4_FLAG_UPD_CONFIRMED_REC_A;
        if flags & !EXCHGID4_FLAG_ARG_MASK != 0 {
            return V4OpResult::new(OP_EXCHANGE_ID, NFS4ERR_INVAL);
        }
        if *state_protect != SP4_NONE {
            return V4OpResult::new(OP_EXCHANGE_ID, NFS4ERR_INVAL);
        }
        if owner.is_empty() {
            return V4OpResult::new(OP_EXCHANGE_ID, NFS4ERR_BADOWNER);
        }
        let (clientid, confirmed) = {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            if let Some(existing) = state.owners.get(owner).copied()
                && let Some(client) = state.clients.get(&existing)
                && client.verifier == *verifier
            {
                (existing, client.confirmed)
            } else {
                let id = seeded_counter(state.seed, state.next_clientid);
                state.next_clientid = state.next_clientid.saturating_add(1).max(1);
                if let Some(previous) = state.owners.insert(owner.clone(), id) {
                    state.clients.remove(&previous);
                }
                state.clients.insert(
                    id,
                    ClientState {
                        owner: owner.clone(),
                        verifier: verifier.clone(),
                        id,
                        confirmed: false,
                        sequence: 1,
                        renewed: self.now(),
                        reclaim_complete: false,
                        create_session_replay: None,
                    },
                );
                (id, false)
            }
        };
        let mut body = XdrWriter::with_capacity(128 + owner.len());
        body.u64(clientid);
        body.u32(1);
        body.u32(
            EXCHGID4_FLAG_USE_NON_PNFS
                | if confirmed {
                    EXCHGID4_FLAG_CONFIRMED_R
                } else {
                    0
                },
        );
        body.u32(SP4_NONE);
        body.u64(0);
        body.var_opaque(&self.write_verifier);
        body.var_opaque(&self.write_verifier);
        body.u32(0);
        V4OpResult::with_body(OP_EXCHANGE_ID, NFS4_OK, body.into_bytes())
    }

    async fn create_session(&self, operation: &Op) -> V4OpResult {
        let Op::CreateSession {
            clientid,
            sequence,
            flags,
            fore,
            back,
        } = operation
        else {
            unreachable!("CREATE_SESSION operation variant")
        };
        // csa_flags are requests, not a promise that the server must grant.
        // The pinned TypeScript implementation accepts the optional request
        // flags and returns csr_flags=0 because this server implements no
        // persistent session cache, callback channel, or RDMA transport.
        let response_flags = *flags & SERVER_CREATE_SESSION_FLAGS;
        {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            let Some(client) = state.clients.get(clientid) else {
                return V4OpResult::new(OP_CREATE_SESSION, NFS4ERR_STALE_CLIENTID);
            };
            let _ = client.id;
            if let Some(replay) = client.create_session_replay.as_ref()
                && replay.sequence == *sequence
            {
                return replay.result.clone();
            }
            if client.sequence != *sequence {
                return V4OpResult::new(OP_CREATE_SESSION, NFS4ERR_SEQ_MISORDERED);
            }
            if fore.maxresponsesize < MIN_RESPONSE_SIZE {
                let result = V4OpResult::new(OP_CREATE_SESSION, NFS4ERR_TOOSMALL);
                let client = state
                    .clients
                    .get_mut(clientid)
                    .expect("validated client while holding state lock");
                client.sequence = client.sequence.wrapping_add(1);
                client.create_session_replay = Some(CreateSessionReplay {
                    sequence: *sequence,
                    result: result.clone(),
                });
                return result;
            }
            if state
                .sessions
                .values()
                .filter(|session| session.clientid == *clientid)
                .count()
                >= self.options.nfs4.max_sessions.max(1)
            {
                let result = V4OpResult::new(OP_CREATE_SESSION, NFS4ERR_NOSPC);
                let client = state
                    .clients
                    .get_mut(clientid)
                    .expect("validated client while holding state lock");
                client.sequence = client.sequence.wrapping_add(1);
                client.create_session_replay = Some(CreateSessionReplay {
                    sequence: *sequence,
                    result: result.clone(),
                });
                return result;
            }
            let client = state
                .clients
                .get_mut(clientid)
                .expect("validated client while holding state lock");
            client.confirmed = true;
            client.sequence = client.sequence.wrapping_add(1);
            client.renewed = self.now();
            let counter = state.next_session;
            state.next_session = state.next_session.saturating_add(1).max(1);
            let id = session_id(self.options.nfs4.seed, &self.write_verifier, counter);
            let max_fore_slots = self.options.nfs4.max_fore_slots.clamp(1, DEFAULT_MAX_SLOTS);
            let slots = usize::try_from(fore.maxrequests)
                .unwrap_or(max_fore_slots)
                .clamp(1, max_fore_slots);
            let max_operations_cap = self
                .options
                .nfs4
                .max_operations
                .clamp(1, NFS4_MAX_COMPOUND_OPS) as u32;
            let max_operations = fore.maxoperations.clamp(1, max_operations_cap);
            let max_request_size = self
                .options
                .nfs4
                .max_request_size
                .max(1)
                .min(u32::MAX as usize) as u32;
            let max_cached_response_size = self
                .options
                .nfs4
                .max_cached_response_size
                .min(u32::MAX as usize);
            let max_cached = usize::try_from(fore.maxresponsesize_cached)
                .unwrap_or(usize::MAX)
                .min(max_cached_response_size);
            state.sessions.insert(
                id,
                SessionState {
                    id,
                    clientid: *clientid,
                    next_sequence: vec![1; slots],
                    in_flight: vec![None; slots],
                    cached: vec![None; slots],
                    max_operations,
                    max_cached,
                },
            );
            let response_fore = ChannelAttrs4 {
                headerpadsize: 0,
                maxrequestsize: fore.maxrequestsize.min(max_request_size),
                maxresponsesize: fore.maxresponsesize.min(max_request_size),
                maxresponsesize_cached: fore.maxresponsesize_cached.min(max_cached as u32),
                maxoperations: max_operations,
                maxrequests: slots as u32,
            };
            let max_cached_response_size = self
                .options
                .nfs4
                .max_cached_response_size
                .min(u32::MAX as usize) as u32;
            let response_back = ChannelAttrs4 {
                headerpadsize: 0,
                maxrequestsize: back.maxrequestsize.min(max_request_size),
                maxresponsesize: back.maxresponsesize.min(max_request_size),
                maxresponsesize_cached: back.maxresponsesize_cached.min(max_cached_response_size),
                maxoperations: back.maxoperations,
                maxrequests: back.maxrequests,
            };
            let mut body = XdrWriter::with_capacity(128);
            body.fixed_opaque(&id, NFS4_SESSIONID_SIZE);
            body.u32(*sequence);
            body.u32(response_flags);
            write_channel_attrs(&mut body, response_fore);
            write_channel_attrs(&mut body, response_back);
            let result = V4OpResult::with_body(OP_CREATE_SESSION, NFS4_OK, body.into_bytes());
            let client = state
                .clients
                .get_mut(clientid)
                .expect("validated client while holding state lock");
            client.create_session_replay = Some(CreateSessionReplay {
                sequence: *sequence,
                result: result.clone(),
            });
            result
        }
    }

    async fn destroy_session(&self, raw_id: &[u8]) -> V4OpResult {
        let id: [u8; NFS4_SESSIONID_SIZE] = match raw_id.try_into() {
            Ok(id) => id,
            Err(_) => return V4OpResult::new(OP_DESTROY_SESSION, NFS4ERR_BADSESSION),
        };
        let status = if self
            .state
            .lock()
            .expect("NFSv4 state lock")
            .sessions
            .remove(&id)
            .is_some()
        {
            NFS4_OK
        } else {
            NFS4ERR_BADSESSION
        };
        V4OpResult::new(OP_DESTROY_SESSION, status)
    }

    fn bind_conn_to_session(
        &self,
        sessionid: [u8; NFS4_SESSIONID_SIZE],
        direction: u32,
        use_rdma: bool,
    ) -> V4OpResult {
        if use_rdma {
            return V4OpResult::new(OP_BIND_CONN_TO_SESSION, NFS4ERR_NOTSUPP);
        }
        if direction != CDFC4_FORE && direction != CDFC4_FORE_OR_BOTH {
            return V4OpResult::new(OP_BIND_CONN_TO_SESSION, NFS4ERR_INVAL);
        }
        let status = if self
            .state
            .lock()
            .expect("NFSv4 state lock")
            .sessions
            .contains_key(&sessionid)
        {
            NFS4_OK
        } else {
            NFS4ERR_BADSESSION
        };
        if status != NFS4_OK {
            return V4OpResult::new(OP_BIND_CONN_TO_SESSION, status);
        }
        let mut body = XdrWriter::with_capacity(24);
        body.fixed_opaque(&sessionid, NFS4_SESSIONID_SIZE);
        body.u32(CDFS4_FORE);
        body.bool(false);
        V4OpResult::with_body(OP_BIND_CONN_TO_SESSION, NFS4_OK, body.into_bytes())
    }

    async fn destroy_clientid(&self, clientid: u64) -> V4OpResult {
        let status = {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            if state
                .sessions
                .values()
                .any(|session| session.clientid == clientid)
                || state.locks.values().any(|lock| lock.clientid == clientid)
            {
                NFS4ERR_CLIENTID_BUSY
            } else if let Some(client) = state.clients.remove(&clientid) {
                state.owners.remove(&client.owner);
                NFS4_OK
            } else {
                NFS4ERR_STALE_CLIENTID
            }
        };
        V4OpResult::new(OP_DESTROY_CLIENTID, status)
    }

    fn reclaim_complete(&self, _one_fs: bool, cursor: &Cursor) -> V4OpResult {
        let Some(clientid) = cursor.clientid else {
            return V4OpResult::new(OP_RECLAIM_COMPLETE, NFS4ERR_OP_NOT_IN_SESSION);
        };
        let status = {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            if let Some(client) = state.clients.get_mut(&clientid) {
                client.reclaim_complete = true;
                NFS4_OK
            } else {
                NFS4ERR_STALE_CLIENTID
            }
        };
        V4OpResult::new(OP_RECLAIM_COMPLETE, status)
    }

    fn test_stateid(&self, stateids: &[Stateid4], cursor: &Cursor) -> V4OpResult {
        let Some(clientid) = cursor.clientid else {
            return V4OpResult::new(OP_TEST_STATEID, NFS4ERR_OP_NOT_IN_SESSION);
        };
        let state = self.state.lock().expect("NFSv4 state lock");
        let mut body = XdrWriter::with_capacity(4 + stateids.len() * 4);
        body.u32(stateids.len() as u32);
        for stateid in stateids {
            let status = if is_zero_stateid(stateid) {
                NFS4_OK
            } else if let Some(open) = state.opens.get(&stateid.other)
                && open.clientid == clientid
                && open.stateid.seqid == stateid.seqid
            {
                NFS4_OK
            } else if let Some(lock) = state.locks.get(&stateid.other)
                && lock.clientid == clientid
                && lock.stateid.seqid == stateid.seqid
            {
                NFS4_OK
            } else {
                NFS4ERR_BAD_STATEID
            };
            body.u32(status);
        }
        V4OpResult::with_body(OP_TEST_STATEID, NFS4_OK, body.into_bytes())
    }

    fn free_stateid(&self, stateid: &Stateid4, cursor: &Cursor) -> V4OpResult {
        let Some(clientid) = cursor.clientid else {
            return V4OpResult::new(OP_FREE_STATEID, NFS4ERR_OP_NOT_IN_SESSION);
        };
        let mut state = self.state.lock().expect("NFSv4 state lock");
        if let Some(open) = state.opens.get(&stateid.other) {
            if open.clientid != clientid || open.stateid.seqid != stateid.seqid {
                return V4OpResult::new(OP_FREE_STATEID, NFS4ERR_BAD_STATEID);
            }
            return V4OpResult::new(OP_FREE_STATEID, NFS4ERR_LOCKS_HELD);
        }
        let Some(lock) = state.locks.get(&stateid.other) else {
            return V4OpResult::new(OP_FREE_STATEID, NFS4ERR_BAD_STATEID);
        };
        if lock.clientid != clientid || lock.stateid.seqid != stateid.seqid {
            return V4OpResult::new(OP_FREE_STATEID, NFS4ERR_BAD_STATEID);
        }
        if !lock.ranges.is_empty() {
            return V4OpResult::new(OP_FREE_STATEID, NFS4ERR_LOCKS_HELD);
        }
        state.locks.remove(&stateid.other);
        V4OpResult::new(OP_FREE_STATEID, NFS4_OK)
    }

    fn open_downgrade(
        &self,
        stateid: &Stateid4,
        _seqid: u32,
        share_access: u32,
        share_deny: u32,
        cursor: &mut Cursor,
    ) -> V4OpResult {
        let Some(access) = open_access_bits(share_access) else {
            return V4OpResult::new(OP_OPEN_DOWNGRADE, NFS4ERR_INVAL);
        };
        if share_deny > OPEN4_SHARE_DENY_BOTH {
            return V4OpResult::new(OP_OPEN_DOWNGRADE, NFS4ERR_INVAL);
        }
        if let Err(status) = self.validate_stateid(stateid, cursor, None) {
            return V4OpResult::new(OP_OPEN_DOWNGRADE, status);
        }
        let mut state = self.state.lock().expect("NFSv4 state lock");
        let Some(open) = state.opens.get_mut(&stateid.other) else {
            return V4OpResult::new(OP_OPEN_DOWNGRADE, NFS4ERR_BAD_STATEID);
        };
        if access & !open.access != 0 || share_deny & !open.deny != 0 {
            return V4OpResult::new(OP_OPEN_DOWNGRADE, NFS4ERR_INVAL);
        }
        open.access = access;
        open.deny = share_deny;
        open.stateid.seqid = bump_stateid_seq(open.stateid.seqid);
        let result = open.stateid.clone();
        cursor.stateid = result.clone();
        let mut body = XdrWriter::with_capacity(16);
        write_stateid(&mut body, &result);
        V4OpResult::with_body(OP_OPEN_DOWNGRADE, NFS4_OK, body.into_bytes())
    }

    fn put_root(&self, cursor: &mut Cursor, operation: u32) -> V4OpResult {
        let root = self.handles.root();
        cursor.current = Some(self.handles.encode(&root));
        cursor.stateid = Stateid4::zero();
        V4OpResult::new(operation, NFS4_OK)
    }

    fn put_fh(&self, handle: &[u8], cursor: &mut Cursor) -> V4OpResult {
        match self.handles.decode(handle) {
            Ok(_) => {
                cursor.current = Some(handle.to_vec());
                cursor.stateid = Stateid4::zero();
                V4OpResult::new(OP_PUTFH, NFS4_OK)
            }
            Err(error) if error.code == ErrorCode::Estale => {
                V4OpResult::new(OP_PUTFH, NFS4ERR_STALE)
            }
            Err(_) => V4OpResult::new(OP_PUTFH, NFS4ERR_BADHANDLE),
        }
    }

    fn get_fh(&self, cursor: &Cursor) -> V4OpResult {
        let Some(handle) = cursor.current.as_ref() else {
            return V4OpResult::new(OP_GETFH, NFS4ERR_NOFILEHANDLE);
        };
        let mut body = XdrWriter::with_capacity(4 + handle.len());
        body.var_opaque(handle);
        V4OpResult::with_body(OP_GETFH, NFS4_OK, body.into_bytes())
    }

    fn save_fh(&self, cursor: &mut Cursor) -> V4OpResult {
        let Some(handle) = cursor.current.clone() else {
            return V4OpResult::new(OP_SAVEFH, NFS4ERR_NOFILEHANDLE);
        };
        cursor.saved = Some(handle);
        cursor.saved_stateid = cursor.stateid.clone();
        V4OpResult::new(OP_SAVEFH, NFS4_OK)
    }

    fn restore_fh(&self, cursor: &mut Cursor) -> V4OpResult {
        let Some(handle) = cursor.saved.clone() else {
            return V4OpResult::new(OP_RESTOREFH, NFS4ERR_NOFILEHANDLE);
        };
        cursor.current = Some(handle);
        cursor.stateid = cursor.saved_stateid.clone();
        V4OpResult::new(OP_RESTOREFH, NFS4_OK)
    }

    async fn stat_of(&self, path: &str) -> Result<Stats, FsError> {
        match self.driver.lstat(path).await {
            Ok(stats) => Ok(stats),
            Err(error) if error.code == ErrorCode::Enosys => self.driver.stat(path).await,
            Err(error) => Err(error),
        }
    }

    /// Translate a uid/gid into the RFC 8881 owner string representation.
    /// Numeric output is the deliberate fallback when the configured map has
    /// no name for this id.
    fn owner_name(&self, id: u32, group: bool) -> String {
        let Some(map) = self.options.nfs4.idmap.as_ref() else {
            return id.to_string();
        };
        let Some(name) = map.resolve_name(id, group) else {
            return id.to_string();
        };
        if name.is_empty() {
            return id.to_string();
        }
        match map.domain() {
            Some(domain) if !domain.is_empty() && !name.contains('@') => {
                format!("{name}@{domain}")
            }
            _ => name.to_owned(),
        }
    }

    /// Translate an incoming RFC 8881 owner string into a uid/gid.
    /// Numeric values are accepted first, including when a map is configured,
    /// so a client can echo the server's numeric fallback representation.
    fn owner_id(&self, owner: &str, group: bool) -> Result<u32, u32> {
        if let Some(id) = parse_numeric_owner(owner) {
            return Ok(id);
        }
        let Some(map) = self.options.nfs4.idmap.as_ref() else {
            return Err(NFS4ERR_BADOWNER);
        };
        let mut name = owner;
        if let Some(domain) = map.domain().filter(|domain| !domain.is_empty()) {
            let Some((local, suffix)) = owner.rsplit_once('@') else {
                return Err(NFS4ERR_BADOWNER);
            };
            if suffix != domain {
                return Err(NFS4ERR_BADOWNER);
            }
            name = local;
        }
        map.resolve_id(name, group).ok_or(NFS4ERR_BADOWNER)
    }

    fn attrs_equal(&self, stats: &Stats, attrs: &Fattr4) -> bool {
        bitmap_bits(&attrs.mask).all(|bit| match bit {
            FATTR4_TYPE => attrs.values.file_type == Some(type_of(stats)),
            FATTR4_SIZE => attrs.values.size == Some(stats.size),
            FATTR4_MODE => attrs
                .values
                .mode
                .is_some_and(|mode| stats.mode & 0o7777 == mode & 0o7777),
            FATTR4_OWNER => attrs
                .values
                .owner
                .as_deref()
                .is_some_and(|owner| owner == self.owner_name(stats.uid, false)),
            FATTR4_OWNER_GROUP => attrs
                .values
                .owner_group
                .as_deref()
                .is_some_and(|group| group == self.owner_name(stats.gid, true)),
            FATTR4_RAWDEV => {
                attrs.values.rawdev == Some(((stats.rdev >> 32) as u32, stats.rdev as u32))
            }
            FATTR4_CHANGE => attrs.values.change == Some(stat_change(stats)),
            FATTR4_FILEID => attrs.values.fileid == Some(stats.ino),
            FATTR4_TIME_ACCESS => attrs
                .values
                .time_access
                .is_some_and(|(seconds, nanos)| nfstime_ms(seconds, nanos) == stats.atime_ms),
            FATTR4_TIME_MODIFY => attrs
                .values
                .time_modify
                .is_some_and(|(seconds, nanos)| nfstime_ms(seconds, nanos) == stats.mtime_ms),
            _ => true,
        })
    }

    fn current_path(&self, cursor: &Cursor) -> Result<String, FsError> {
        cursor
            .current
            .as_deref()
            .ok_or_else(|| FsError::new(ErrorCode::Estale))
            .and_then(|handle| self.handles.resolve(handle))
    }

    async fn attr_bytes(&self, path: &str, requested: &[u32]) -> Result<Vec<u8>, u32> {
        let stats = self
            .stat_of(path)
            .await
            .map_err(|error| error_status(&error))?;
        let entry = self.handles.bind(path, &stats);
        self.attr_bytes_for(Some(path), &entry, &stats, requested)
            .await
    }

    async fn attr_bytes_for(
        &self,
        path: Option<&str>,
        entry: &HandleEntry,
        stats: &Stats,
        requested: &[u32],
    ) -> Result<Vec<u8>, u32> {
        if bitmap_bits(requested).any(set_only_attr) {
            return Err(NFS4ERR_INVAL);
        }
        let capabilities = self.driver.capabilities;
        // GETATTR and READDIR are reply-direction operations.  RFC 8881
        // requires unsupported read attributes to be omitted from the result,
        // while a set-only attribute is an explicit INVAL above.  In
        // particular, Linux clients commonly ask for mounted_on_fileid and
        // ACL-related bits even when the server does not advertise them.
        let requested_bits: Vec<u32> = bitmap_bits(requested)
            .filter(|bit| {
                gettable_attr(*bit)
                    && advertised_attr(*bit, capabilities.statfs, capabilities.times)
            })
            .collect();
        let requested_mask = bitmap_of(requested_bits.iter().copied());
        let fs = if let Some(path) = path {
            self.driver.statfs(path).await.ok()
        } else {
            None
        }
        .unwrap_or_else(|| StatsFs {
            filesystem_type: 0,
            block_size: stats.blksize.max(1),
            blocks: 0,
            blocks_free: 0,
            blocks_available: 0,
            files: 0,
            files_free: 0,
        });
        let mut values = XdrWriter::with_capacity(256);
        for bit in requested_bits {
            match bit {
                FATTR4_SUPPORTED_ATTRS => write_bitmap(
                    &mut values,
                    &supported_attrs(capabilities.statfs, capabilities.times),
                ),
                FATTR4_TYPE => values.u32(type_of(stats)),
                FATTR4_FH_EXPIRE_TYPE => values.u32(FH4_PERSISTENT),
                FATTR4_CHANGE => values.u64(stat_change(stats)),
                FATTR4_SIZE => values.u64(stats.size),
                FATTR4_LINK_SUPPORT => values.bool(self.driver.capabilities.hardlinks),
                FATTR4_SYMLINK_SUPPORT => values.bool(self.driver.capabilities.symlinks),
                FATTR4_NAMED_ATTR => values.bool(false),
                FATTR4_FSID => {
                    values.u64(stats.dev);
                    values.u64(0);
                }
                FATTR4_UNIQUE_HANDLES => values.bool(true),
                FATTR4_LEASE_TIME => values.u32(self.options.nfs4.lease_seconds.max(1)),
                FATTR4_RDATTR_ERROR => values.u32(NFS4_OK),
                FATTR4_FILEHANDLE => values.var_opaque(&self.handles.encode(entry)),
                FATTR4_FILEID => values.u64(entry.fileid),
                FATTR4_FILES_AVAIL => values.u64(fs.blocks_available),
                FATTR4_FILES_FREE => values.u64(fs.blocks_free),
                FATTR4_FILES_TOTAL => values.u64(fs.blocks),
                FATTR4_MAXFILESIZE => values.u64(MAX_OFFSET),
                FATTR4_MAXLINK => values.u32(u32::MAX),
                FATTR4_MAXNAME => values.u32(NFS4_MAX_COMPONENT as u32),
                FATTR4_MAXREAD => values.u64(DEFAULT_MAX_READ as u64),
                FATTR4_MAXWRITE => values.u64(DEFAULT_MAX_WRITE as u64),
                FATTR4_MODE => values.u32(stats.mode & 0o7777),
                FATTR4_NUMLINKS => values.u32(stats.nlink.min(u32::MAX as u64) as u32),
                FATTR4_OWNER => values.string(&self.owner_name(stats.uid, false)),
                FATTR4_OWNER_GROUP => values.string(&self.owner_name(stats.gid, true)),
                FATTR4_RAWDEV => {
                    values.u32((stats.rdev >> 32) as u32);
                    values.u32(stats.rdev as u32);
                }
                FATTR4_SPACE_AVAIL => values.u64(fs.blocks_available.saturating_mul(fs.block_size)),
                FATTR4_SPACE_FREE => values.u64(fs.blocks_free.saturating_mul(fs.block_size)),
                FATTR4_SPACE_TOTAL => values.u64(fs.blocks.saturating_mul(fs.block_size)),
                FATTR4_SPACE_USED => values.u64(stats.blocks.saturating_mul(512)),
                FATTR4_TIME_ACCESS => write_time(&mut values, stats.atime_ms),
                FATTR4_TIME_DELTA => write_time(&mut values, 0),
                FATTR4_TIME_METADATA => write_time(&mut values, stats.ctime_ms),
                FATTR4_TIME_MODIFY => write_time(&mut values, stats.mtime_ms),
                _ => unreachable!("supported attribute table is exhaustive"),
            }
        }
        let mut result = XdrWriter::with_capacity(values.len() + 64);
        write_bitmap(&mut result, &requested_mask);
        result.var_opaque(&values.into_bytes());
        Ok(result.into_bytes())
    }

    fn open_handle_for_file_id(&self, file_id: u64) -> Option<Arc<dyn FileHandle>> {
        self.state
            .lock()
            .expect("NFSv4 state lock")
            .opens
            .values()
            .find(|open| open.file_id == file_id)
            .map(|open| open.handle.clone())
    }

    async fn lookup(&self, name: &str, cursor: &mut Cursor) -> V4OpResult {
        let directory = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_LOOKUP, error_status(&error)),
        };
        if let Err(error) = check_name(name) {
            return V4OpResult::new(OP_LOOKUP, error_status(&error));
        }
        let path = join_path(&directory, name);
        match self.stat_of(&path).await {
            Ok(stats) => {
                let entry = self.handles.bind(&path, &stats);
                cursor.current = Some(self.handles.encode(&entry));
                cursor.stateid = Stateid4::zero();
                V4OpResult::new(OP_LOOKUP, NFS4_OK)
            }
            Err(error) => V4OpResult::new(OP_LOOKUP, error_status(&error)),
        }
    }

    async fn lookupp(&self, cursor: &mut Cursor) -> V4OpResult {
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_LOOKUPP, error_status(&error)),
        };
        if path == "/" {
            return V4OpResult::new(OP_LOOKUPP, NFS4ERR_NOENT);
        }
        let parent = mount_rs_core::path::dirname(&path);
        match self.stat_of(&parent).await {
            Ok(stats) => {
                let entry = self.handles.bind(&parent, &stats);
                cursor.current = Some(self.handles.encode(&entry));
                cursor.stateid = Stateid4::zero();
                V4OpResult::new(OP_LOOKUPP, NFS4_OK)
            }
            Err(error) => V4OpResult::new(OP_LOOKUPP, error_status(&error)),
        }
    }

    async fn getattr(&self, mask: &[u32], cursor: &Cursor) -> V4OpResult {
        match self.current_path(cursor) {
            Ok(path) => match self.attr_bytes(&path, mask).await {
                Ok(body) => V4OpResult::with_body(OP_GETATTR, NFS4_OK, body),
                Err(status) => V4OpResult::new(OP_GETATTR, status),
            },
            Err(path_error) => {
                let Some(handle) = cursor.current.as_deref() else {
                    return V4OpResult::new(OP_GETATTR, error_status(&path_error));
                };
                let entry = match self.handles.decode(handle) {
                    Ok(entry) => entry,
                    Err(_) => return V4OpResult::new(OP_GETATTR, error_status(&path_error)),
                };
                let Some(open_handle) = self.open_handle_for_file_id(entry.fileid) else {
                    return V4OpResult::new(OP_GETATTR, error_status(&path_error));
                };
                let stats = match open_handle.stat().await {
                    Ok(stats) => stats,
                    Err(error) => return V4OpResult::new(OP_GETATTR, error_status(&error)),
                };
                match self.attr_bytes_for(None, &entry, &stats, mask).await {
                    Ok(body) => V4OpResult::with_body(OP_GETATTR, NFS4_OK, body),
                    Err(status) => V4OpResult::new(OP_GETATTR, status),
                }
            }
        }
    }

    async fn access(
        &self,
        requested: u32,
        cursor: &Cursor,
        credentials: &RpcCredentials,
    ) -> V4OpResult {
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_ACCESS, error_status(&error)),
        };
        let stats = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => return V4OpResult::new(OP_ACCESS, error_status(&error)),
        };
        let allowed = allowed_access4(&stats, credentials);
        let supported = requested & ACCESS4_ALL;
        let access = allowed & supported;
        let mut body = XdrWriter::with_capacity(8);
        body.u32(supported);
        body.u32(access);
        V4OpResult::with_body(OP_ACCESS, NFS4_OK, body.into_bytes())
    }

    async fn readlink(&self, cursor: &Cursor) -> V4OpResult {
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_READLINK, error_status(&error)),
        };
        match self.driver.readlink(&path).await {
            Ok(target) => {
                let mut body = XdrWriter::with_capacity(target.len() + 4);
                body.string(&target);
                V4OpResult::with_body(OP_READLINK, NFS4_OK, body.into_bytes())
            }
            Err(error) => V4OpResult::new(OP_READLINK, error_status(&error)),
        }
    }

    async fn readdir(
        &self,
        cookie: u64,
        verifier: &[u8],
        _dircount: u32,
        maxcount: u32,
        attrs: &[u32],
        cursor: &Cursor,
    ) -> V4OpResult {
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_READDIR, error_status(&error)),
        };
        let stats = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => return V4OpResult::new(OP_READDIR, error_status(&error)),
        };
        if stats.mode & S_IFMT != S_IFDIR {
            return V4OpResult::new(OP_READDIR, NFS4ERR_NOTDIR);
        }
        if bitmap_bits(attrs).any(set_only_attr) {
            return V4OpResult::new(OP_READDIR, NFS4ERR_INVAL);
        }
        let entry = self.handles.bind(&path, &stats);
        let snapshot = if cookie == 0 {
            let entries = match self.driver.readdir(&path).await {
                Ok(entries) => entries,
                Err(error) => return V4OpResult::new(OP_READDIR, error_status(&error)),
            };
            self.snapshots.set(
                entry.id,
                entries.into_iter().map(|entry| entry.name).collect(),
            )
        } else {
            if cookie < COOKIE_BASE {
                return V4OpResult::new(OP_READDIR, NFS4ERR_BAD_COOKIE);
            }
            match self.snapshots.get(entry.id) {
                Some(snapshot) if same_verifier(&snapshot.verifier, verifier) => snapshot,
                _ => {
                    let entries = match self.driver.readdir(&path).await {
                        Ok(entries) => entries,
                        Err(error) => return V4OpResult::new(OP_READDIR, error_status(&error)),
                    };
                    let names: Vec<String> = entries.into_iter().map(|entry| entry.name).collect();
                    if !same_verifier(&cookie_verifier(&names), verifier) {
                        return V4OpResult::new(OP_READDIR, NFS4ERR_BAD_COOKIE);
                    }
                    self.snapshots.set(entry.id, names)
                }
            }
        };
        let from = if cookie == 0 {
            0
        } else {
            usize::try_from(cookie.saturating_sub(COOKIE_BASE - 1)).unwrap_or(usize::MAX)
        };
        if from > snapshot.names.len() {
            return V4OpResult::new(OP_READDIR, NFS4ERR_BAD_COOKIE);
        }
        let budget = usize::try_from(maxcount).unwrap_or(usize::MAX);
        let mut body = XdrWriter::with_capacity(budget.clamp(32, 64 * 1024));
        body.fixed_opaque(&snapshot.verifier, NFS4_VERIFIER_SIZE);
        let mut any = false;
        let mut next = from;
        for (index, name) in snapshot.names.iter().enumerate().skip(from) {
            let child_path = join_path(&path, name);
            let child_stats = match self.stat_of(&child_path).await {
                Ok(stats) => stats,
                Err(_) => continue,
            };
            let _child = self.handles.bind(&child_path, &child_stats);
            let child_attrs = match self.attr_bytes(&child_path, attrs).await {
                Ok(bytes) => bytes,
                Err(status) => return V4OpResult::new(OP_READDIR, status),
            };
            let mut item = XdrWriter::with_capacity(name.len() + child_attrs.len() + 24);
            item.bool(true);
            item.u64(COOKIE_BASE.saturating_add(index as u64));
            item.string(name);
            item.raw(&child_attrs);
            if any && body.len().saturating_add(item.len()).saturating_add(4) > budget {
                break;
            }
            if !any && body.len().saturating_add(item.len()).saturating_add(4) > budget {
                return V4OpResult::new(OP_READDIR, NFS4ERR_TOOSMALL);
            }
            body.raw(&item.into_bytes());
            any = true;
            next = index.saturating_add(1);
        }
        body.bool(false);
        body.bool(next >= snapshot.names.len());
        V4OpResult::with_body(OP_READDIR, NFS4_OK, body.into_bytes())
    }

    async fn commit(&self, _offset: u64, _count: u32, cursor: &Cursor) -> V4OpResult {
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_COMMIT, error_status(&error)),
        };
        // WRITE may have acknowledged UNSTABLE4, so COMMIT must provide the
        // FileHandle::sync barrier even though offset/count only select the
        // committed range at the protocol level.  Resolve the path first so a
        // stale current filehandle remains an error before opening it.
        if let Err(error) = self.stat_of(&path).await {
            return V4OpResult::new(OP_COMMIT, error_status(&error));
        }
        let handle = match self.driver.open_flags(&path, OpenFlags::READ_ONLY, 0).await {
            Ok(handle) => handle,
            Err(error) => return V4OpResult::new(OP_COMMIT, error_status(&error)),
        };
        let sync_result = handle.sync().await;
        let close_result = handle.close().await;
        if let Err(error) = sync_result {
            return V4OpResult::new(OP_COMMIT, error_status(&error));
        }
        if let Err(error) = close_result {
            return V4OpResult::new(OP_COMMIT, error_status(&error));
        }
        let mut body = XdrWriter::with_capacity(8);
        body.fixed_opaque(&self.write_verifier, NFS4_VERIFIER_SIZE);
        V4OpResult::with_body(OP_COMMIT, NFS4_OK, body.into_bytes())
    }

    async fn create(
        &self,
        kind: u32,
        name: &str,
        attrs: &Fattr4,
        link: Option<&str>,
        dev: Option<(u32, u32)>,
        cursor: &mut Cursor,
    ) -> V4OpResult {
        let parent = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_CREATE, error_status(&error)),
        };
        if let Err(error) = check_name(name) {
            return V4OpResult::new(OP_CREATE, error_status(&error));
        }
        if attrs.unsupported {
            return V4OpResult::new(OP_CREATE, NFS4ERR_ATTRNOTSUPP);
        }
        let path = join_path(&parent, name);
        let before = self
            .stat_of(&parent)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        let mode = attrs.values.mode.unwrap_or(0o777) & 0o7777;
        let operation = match kind {
            NF4DIR => self
                .driver
                .mkdir(
                    &path,
                    MkdirOptions {
                        recursive: false,
                        mode: Some(mode & 0o7777),
                    },
                )
                .await
                .map(|_| ()),
            NF4REG | NF4ATTRDIR | NF4NAMEDATTR => {
                return V4OpResult::new(OP_CREATE, NFS4ERR_BADTYPE);
            }
            NF4LNK => self.driver.symlink(link.unwrap_or_default(), &path).await,
            NF4BLK | NF4CHR => {
                let Some((major, minor)) = dev else {
                    return V4OpResult::new(OP_CREATE, NFS4ERR_INVAL);
                };
                self.driver
                    .mknod(
                        &path,
                        (if kind == NF4BLK { S_IFBLK } else { S_IFCHR }) | mode,
                        (u64::from(major) << 8) | u64::from(minor & 0xff),
                    )
                    .await
            }
            NF4FIFO => self.driver.mknod(&path, S_IFIFO | mode, 0).await,
            NF4SOCK => self.driver.mknod(&path, S_IFSOCK | mode, 0).await,
            _ => return V4OpResult::new(OP_CREATE, NFS4ERR_BADTYPE),
        };
        if let Err(error) = operation {
            return V4OpResult::new(OP_CREATE, error_status(&error));
        }
        let applied = self
            .apply_attrs_with_options(
                &path,
                attrs,
                ApplyAttrsOptions {
                    skip_size: true,
                    skip_mode: kind == NF4LNK,
                },
            )
            .await;
        if applied.status != NFS4_OK {
            return V4OpResult::new(OP_CREATE, applied.status);
        }
        let mut applied = applied.bits;
        if kind != NF4LNK && attrs.values.mode.is_some() {
            applied.push(FATTR4_MODE);
        }
        let after = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => return V4OpResult::new(OP_CREATE, error_status(&error)),
        };
        let entry = self.handles.bind(&path, &after);
        cursor.current = Some(self.handles.encode(&entry));
        cursor.stateid = Stateid4::zero();
        let mut body = XdrWriter::with_capacity(32);
        write_change_info(&mut body, before, Some(stat_change(&after)));
        write_bitmap(&mut body, &applied);
        V4OpResult::with_body(OP_CREATE, NFS4_OK, body.into_bytes())
    }

    async fn remove(&self, name: &str, cursor: &mut Cursor) -> V4OpResult {
        let parent = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_REMOVE, error_status(&error)),
        };
        if let Err(error) = check_name(name) {
            return V4OpResult::new(OP_REMOVE, error_status(&error));
        }
        let path = join_path(&parent, name);
        let before = self
            .stat_of(&parent)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        let target = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => return V4OpResult::new(OP_REMOVE, error_status(&error)),
        };
        let target_entry = self.handles.bind(&path, &target);
        let result = if target.mode & S_IFMT == S_IFDIR {
            self.driver.rmdir(&path).await
        } else {
            self.driver.unlink(&path).await
        };
        if let Err(error) = result {
            return V4OpResult::new(OP_REMOVE, error_status(&error));
        }
        if target.mode & S_IFMT == S_IFDIR {
            self.handles.forget(&path);
        } else if self.open_handle_for_file_id(target_entry.fileid).is_some() {
            // Keep the opaque filehandle and backend descriptor alive for an
            // outstanding OPEN; the namespace name is gone, so resolution by
            // path must still fail while READ/GETATTR use the held handle.
            self.handles.orphan(&path);
        } else {
            self.handles.forget(&path);
        }
        self.forget_exclusive(&path);
        self.invalidate(&parent);
        let after = self
            .stat_of(&parent)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        let mut body = XdrWriter::with_capacity(24);
        write_change_info(&mut body, before, after);
        V4OpResult::with_body(OP_REMOVE, NFS4_OK, body.into_bytes())
    }

    async fn rename(&self, old: &str, new: &str, cursor: &mut Cursor) -> V4OpResult {
        let source_dir = match cursor.saved.as_deref() {
            Some(handle) => match self.handles.resolve(handle) {
                Ok(path) => path,
                Err(error) => return V4OpResult::new(OP_RENAME, error_status(&error)),
            },
            None => return V4OpResult::new(OP_RENAME, NFS4ERR_NOFILEHANDLE),
        };
        let target_dir = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_RENAME, error_status(&error)),
        };
        if let Err(error) = check_name(old).and(check_name(new)) {
            return V4OpResult::new(OP_RENAME, error_status(&error));
        }
        let from = join_path(&source_dir, old);
        let to = join_path(&target_dir, new);
        let source_before = self
            .stat_of(&source_dir)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        let target_before = self
            .stat_of(&target_dir)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        if let Err(error) = self.driver.rename(&from, &to).await {
            return V4OpResult::new(OP_RENAME, error_status(&error));
        }
        self.handles.remap(&from, &to);
        self.remap_exclusive(&from, &to);
        self.invalidate(&source_dir);
        self.invalidate(&target_dir);
        let source_after = self
            .stat_of(&source_dir)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        let target_after = self
            .stat_of(&target_dir)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        let mut body = XdrWriter::with_capacity(48);
        write_change_info(&mut body, source_before, source_after);
        write_change_info(&mut body, target_before, target_after);
        V4OpResult::with_body(OP_RENAME, NFS4_OK, body.into_bytes())
    }

    async fn link(&self, name: &str, cursor: &mut Cursor) -> V4OpResult {
        let source = match cursor.saved.as_deref() {
            Some(handle) => match self.handles.resolve(handle) {
                Ok(path) => path,
                Err(error) => return V4OpResult::new(OP_LINK, error_status(&error)),
            },
            None => return V4OpResult::new(OP_LINK, NFS4ERR_NOFILEHANDLE),
        };
        let target_dir = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_LINK, error_status(&error)),
        };
        if let Err(error) = check_name(name) {
            return V4OpResult::new(OP_LINK, error_status(&error));
        }
        let target = join_path(&target_dir, name);
        let before = self
            .stat_of(&target_dir)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        if let Err(error) = self.driver.link(&source, &target).await {
            return V4OpResult::new(OP_LINK, error_status(&error));
        }
        self.invalidate(&target_dir);
        if let Ok(stats) = self.stat_of(&target).await {
            self.handles.bind(&target, &stats);
        }
        let after = self
            .stat_of(&target_dir)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        let mut body = XdrWriter::with_capacity(24);
        write_change_info(&mut body, before, after);
        V4OpResult::with_body(OP_LINK, NFS4_OK, body.into_bytes())
    }

    fn invalidate(&self, path: &str) {
        if let Some(entry) = self.handles.at(path) {
            self.snapshots.invalidate(entry.id);
        }
    }

    fn forget_exclusive(&self, path: &str) {
        let mut state = self.state.lock().expect("NFSv4 state lock");
        state.exclusive_creates.retain(|candidate, _| {
            candidate != path && !candidate.starts_with(&format!("{path}/"))
        });
    }

    fn remap_exclusive(&self, old_path: &str, new_path: &str) {
        let mut state = self.state.lock().expect("NFSv4 state lock");
        let prefix = format!("{old_path}/");
        let moved: Vec<(String, ExclusiveV4)> = state
            .exclusive_creates
            .iter()
            .filter(|(path, _)| *path == old_path || path.starts_with(&prefix))
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect();
        state
            .exclusive_creates
            .retain(|path, _| *path != old_path && !path.starts_with(&prefix) && *path != new_path);
        for (path, entry) in moved {
            let suffix = path.strip_prefix(old_path).unwrap_or_default();
            state
                .exclusive_creates
                .insert(format!("{new_path}{suffix}"), entry);
        }
    }

    async fn apply_attrs(&self, path: &str, attrs: &Fattr4) -> Result<Vec<u32>, u32> {
        let applied = self
            .apply_attrs_with_options(path, attrs, ApplyAttrsOptions::default())
            .await;
        if applied.status == NFS4_OK {
            Ok(bitmap_of(applied.bits))
        } else {
            Err(applied.status)
        }
    }

    async fn apply_attrs_with_options(
        &self,
        path: &str,
        attrs: &Fattr4,
        options: ApplyAttrsOptions,
    ) -> AppliedAttrs {
        if attrs.unsupported {
            return AppliedAttrs::failed(NFS4ERR_ATTRNOTSUPP);
        }
        for bit in bitmap_bits(&attrs.mask) {
            if !supported_attr(bit) {
                return AppliedAttrs::failed(NFS4ERR_ATTRNOTSUPP);
            }
            if !settable_attr(bit) {
                return AppliedAttrs::failed(NFS4ERR_INVAL);
            }
        }
        let current = match self.stat_of(path).await {
            Ok(current) => current,
            Err(error) => return AppliedAttrs::failed(error_status(&error)),
        };
        let mut applied = Vec::new();
        if !options.skip_mode
            && let Some(mode) = attrs.values.mode
        {
            if let Err(error) = self.driver.chmod(path, mode & 0o7777).await {
                return AppliedAttrs::with_status(error_status(&error), applied);
            }
            applied.push(FATTR4_MODE);
        }
        if attrs.values.owner.is_some() || attrs.values.owner_group.is_some() {
            let uid = match attrs.values.owner.as_deref() {
                Some(owner) => match self.owner_id(owner, false) {
                    Ok(uid) => uid,
                    Err(status) => return AppliedAttrs::with_status(status, applied),
                },
                None => current.uid,
            };
            let gid = match attrs.values.owner_group.as_deref() {
                Some(group) => match self.owner_id(group, true) {
                    Ok(gid) => gid,
                    Err(status) => return AppliedAttrs::with_status(status, applied),
                },
                None => current.gid,
            };
            let result = match self.driver.lchown(path, uid, gid).await {
                Ok(()) => Ok(()),
                Err(error) if error.code == ErrorCode::Enosys => {
                    self.driver.chown(path, uid, gid).await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                return AppliedAttrs::with_status(error_status(&error), applied);
            }
            if attrs.values.owner.is_some() {
                applied.push(FATTR4_OWNER);
            }
            if attrs.values.owner_group.is_some() {
                applied.push(FATTR4_OWNER_GROUP);
            }
        }
        if !options.skip_size
            && let Some(size) = attrs.values.size
        {
            let size = match offset(size, "truncate") {
                Ok(size) => size,
                Err(error) => return AppliedAttrs::with_status(error_status(&error), applied),
            };
            if let Err(error) = self.driver.truncate(path, size).await {
                return AppliedAttrs::with_status(error_status(&error), applied);
            }
            applied.push(FATTR4_SIZE);
        }
        let access = match requested_time(attrs.values.time_access_set) {
            Ok(access) => access,
            Err(status) => return AppliedAttrs::with_status(status, applied),
        };
        let modify = match requested_time(attrs.values.time_modify_set) {
            Ok(modify) => modify,
            Err(status) => return AppliedAttrs::with_status(status, applied),
        };
        if access.is_some() || modify.is_some() {
            let latest = match self.stat_of(path).await {
                Ok(latest) => latest,
                Err(error) => return AppliedAttrs::with_status(error_status(&error), applied),
            };
            let atime = access.unwrap_or(latest.atime_ms);
            let mtime = modify.unwrap_or(latest.mtime_ms);
            let result = match self.driver.lutimes(path, atime, mtime).await {
                Ok(()) => Ok(()),
                Err(error) if error.code == ErrorCode::Enosys => {
                    self.driver.utimes(path, atime, mtime).await
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                return AppliedAttrs::with_status(error_status(&error), applied);
            }
            if access.is_some() {
                applied.push(FATTR4_TIME_ACCESS_SET);
            }
            if modify.is_some() {
                applied.push(FATTR4_TIME_MODIFY_SET);
            }
        }
        AppliedAttrs::with_status(NFS4_OK, applied)
    }

    async fn setattr(&self, stateid: &Stateid4, attrs: &Fattr4, cursor: &Cursor) -> V4OpResult {
        if attrs.values.size.is_some()
            && let Err(status) =
                self.validate_stateid(stateid, cursor, Some(OPEN4_SHARE_ACCESS_WRITE))
        {
            return V4OpResult::with_body(OP_SETATTR, status, bitmap_body(&[]));
        }
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => {
                return V4OpResult::with_body(OP_SETATTR, error_status(&error), bitmap_body(&[]));
            }
        };
        let applied = self
            .apply_attrs_with_options(&path, attrs, ApplyAttrsOptions::default())
            .await;
        if applied.status == NFS4_OK {
            self.forget_exclusive(&path);
        }
        V4OpResult::with_body(
            OP_SETATTR,
            applied.status,
            bitmap_body(&bitmap_of(applied.bits)),
        )
    }

    async fn verify(&self, attrs: &Fattr4, cursor: &Cursor, negated: bool) -> V4OpResult {
        if attrs.unsupported || bitmap_bits(&attrs.mask).any(|bit| !supported_attr(bit)) {
            return V4OpResult::new(
                if negated { OP_NVERIFY } else { OP_VERIFY },
                NFS4ERR_ATTRNOTSUPP,
            );
        }
        if bitmap_bits(&attrs.mask).any(set_only_attr) {
            return V4OpResult::new(if negated { OP_NVERIFY } else { OP_VERIFY }, NFS4ERR_INVAL);
        }
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => {
                return V4OpResult::new(
                    if negated { OP_NVERIFY } else { OP_VERIFY },
                    error_status(&error),
                );
            }
        };
        let stats = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => {
                return V4OpResult::new(
                    if negated { OP_NVERIFY } else { OP_VERIFY },
                    error_status(&error),
                );
            }
        };
        let same = self.attrs_equal(&stats, attrs);
        let status = if negated {
            if same { NFS4ERR_NOT_SAME } else { NFS4_OK }
        } else if same {
            NFS4_OK
        } else {
            NFS4ERR_NOT_SAME
        };
        V4OpResult::new(if negated { OP_NVERIFY } else { OP_VERIFY }, status)
    }

    fn validate_stateid(
        &self,
        stateid: &Stateid4,
        cursor: &Cursor,
        required_access: Option<u32>,
    ) -> Result<Option<OpenState>, u32> {
        if is_zero_stateid(stateid) {
            return Ok(None);
        }
        if is_invalid_stateid(stateid) {
            return Err(NFS4ERR_BAD_STATEID);
        }
        let state = self.state.lock().expect("NFSv4 state lock");
        let Some(open) = state.opens.get(&stateid.other).cloned() else {
            return Err(NFS4ERR_BAD_STATEID);
        };
        if cursor.clientid != Some(open.clientid) {
            return Err(NFS4ERR_BAD_STATEID);
        }
        if stateid.seqid != 0 {
            match compare_stateid_seqid(stateid.seqid, open.stateid.seqid) {
                SeqidOrdering::Greater => return Err(NFS4ERR_BAD_STATEID),
                SeqidOrdering::Less => return Err(NFS4ERR_OLD_STATEID),
                SeqidOrdering::Equal => {}
            }
        }
        if let Some(access) = required_access
            && open.access & access == 0
        {
            return Err(NFS4ERR_OPENMODE);
        }
        if let Some(handle) = cursor.current.as_deref()
            && let Ok(entry) = self.handles.decode(handle)
            && entry.fileid != open.file_id
        {
            return Err(NFS4ERR_BAD_STATEID);
        }
        let _ = cursor.session;
        Ok(Some(open))
    }

    /// Take a byte-range lock for the current session's client and open.
    ///
    /// The state table is deliberately process-local, matching the rest of
    /// this crate's v4 lease model.  The wire operation is nevertheless real:
    /// ranges conflict by lock owner, denied replies carry the conflicting
    /// owner, and LOCKU/FREE_STATEID/CLOSE observe the resulting state.
    async fn lock(
        &self,
        lock_type: u32,
        reclaim: bool,
        offset: u64,
        length: u64,
        locker: &Locker4,
        cursor: &mut Cursor,
    ) -> V4OpResult {
        let Some(access) = lock_access(lock_type) else {
            return V4OpResult::new(OP_LOCK, NFS4ERR_LOCK_NOTSUPP);
        };
        if reclaim {
            return V4OpResult::new(OP_LOCK, NFS4ERR_NO_GRACE);
        }
        if !valid_lock_range(offset, length) {
            return V4OpResult::new(OP_LOCK, NFS4ERR_INVAL);
        }
        let Some(clientid) = cursor.clientid else {
            return V4OpResult::new(OP_LOCK, NFS4ERR_OP_NOT_IN_SESSION);
        };
        {
            let state = self.state.lock().expect("NFSv4 state lock");
            let Some(client) = state.clients.get(&clientid) else {
                return V4OpResult::new(OP_LOCK, NFS4ERR_STALE_CLIENTID);
            };
            if self.options.nfs4.require_reclaim_complete && !client.reclaim_complete {
                return V4OpResult::new(OP_LOCK, NFS4ERR_GRACE);
            }
        }
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_LOCK, error_status(&error)),
        };
        let stats = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => return V4OpResult::new(OP_LOCK, error_status(&error)),
        };
        if let Err(status) = regular_file_status(&stats) {
            return V4OpResult::new(OP_LOCK, status);
        }
        let entry = self.handles.bind(&path, &stats);

        let (owner, open_other, target_key) = if locker.new_lock_owner {
            let Some(open_stateid) = locker.open_stateid.as_ref() else {
                return V4OpResult::new(OP_LOCK, NFS4ERR_BAD_STATEID);
            };
            let open = match self.validate_stateid(open_stateid, cursor, Some(access)) {
                Ok(Some(open)) => open,
                Ok(None) | Err(NFS4ERR_BAD_STATEID) => {
                    return V4OpResult::new(OP_LOCK, NFS4ERR_BAD_STATEID);
                }
                Err(status) => return V4OpResult::new(OP_LOCK, status),
            };
            if open.file_id != entry.fileid {
                return V4OpResult::new(OP_LOCK, NFS4ERR_BAD_STATEID);
            }
            let state = self.state.lock().expect("NFSv4 state lock");
            let target_key = state
                .locks
                .values()
                .find(|lock| {
                    lock.clientid == clientid
                        && lock.file_id == entry.fileid
                        && lock.open_other == open.stateid.other
                        && lock.owner == locker.owner
                })
                .map(|lock| lock.stateid.other);
            (locker.owner.clone(), open.stateid.other, target_key)
        } else {
            let Some(lock_stateid) = locker.lock_stateid.as_ref() else {
                return V4OpResult::new(OP_LOCK, NFS4ERR_BAD_STATEID);
            };
            if is_zero_stateid(lock_stateid) || is_invalid_stateid(lock_stateid) {
                return V4OpResult::new(OP_LOCK, NFS4ERR_BAD_STATEID);
            }
            let state = self.state.lock().expect("NFSv4 state lock");
            let Some(lock) = state.locks.get(&lock_stateid.other) else {
                return V4OpResult::new(OP_LOCK, NFS4ERR_BAD_STATEID);
            };
            if lock.clientid != clientid || lock.file_id != entry.fileid {
                return V4OpResult::new(OP_LOCK, NFS4ERR_BAD_STATEID);
            }
            if lock_stateid.seqid != lock.stateid.seqid {
                return V4OpResult::new(
                    OP_LOCK,
                    if lock_stateid.seqid < lock.stateid.seqid {
                        NFS4ERR_OLD_STATEID
                    } else {
                        NFS4ERR_BAD_STATEID
                    },
                );
            }
            (
                lock.owner.clone(),
                lock.open_other,
                Some(lock.stateid.other),
            )
        };

        let mut state = self.state.lock().expect("NFSv4 state lock");
        let denied = state.locks.values().find_map(|held| {
            if held.file_id != entry.fileid || (held.clientid == clientid && held.owner == owner) {
                return None;
            }
            held.ranges.iter().find_map(|range| {
                (lock_overlaps(range, offset, length) && lock_conflicts(range.lock_type, access))
                    .then(|| (range.clone(), held.clientid, held.owner.clone()))
            })
        });
        if let Some((range, held_clientid, held_owner)) = denied {
            return V4OpResult::with_body(
                OP_LOCK,
                NFS4ERR_DENIED,
                lock_denied_body(&range, held_clientid, &held_owner),
            );
        }
        let granted_ranges = state
            .locks
            .values()
            .filter(|lock| lock.file_id == entry.fileid)
            .map(|lock| lock.ranges.len())
            .sum::<usize>();
        if granted_ranges >= self.options.nfs4.max_locks_per_file.max(1) {
            return V4OpResult::new(OP_LOCK, NFS4ERR_RESOURCE);
        }

        // A granted range replaces the requesting lock owner's overlapping
        // ranges, including ranges formerly assigned through another OPEN.
        // This is the POSIX-style ownership rule used by the upstream oracle.
        for held in state.locks.values_mut() {
            if held.file_id == entry.fileid && held.clientid == clientid && held.owner == owner {
                held.ranges = held
                    .ranges
                    .iter()
                    .flat_map(|range| subtract_lock_range(range, offset, length))
                    .collect();
            }
        }

        let range = LockRange {
            offset,
            length,
            lock_type: access,
        };
        let result = if let Some(target_key) = target_key {
            let Some(target) = state.locks.get_mut(&target_key) else {
                return V4OpResult::new(OP_LOCK, NFS4ERR_BAD_STATEID);
            };
            target.stateid.seqid = bump_stateid_seq(target.stateid.seqid);
            target.ranges.push(range);
            coalesce_lock_ranges(&mut target.ranges);
            target.stateid.clone()
        } else {
            let stateid = lock_stateid(1, clientid, entry.fileid, &owner);
            state.locks.insert(
                stateid.other,
                LockState {
                    stateid: stateid.clone(),
                    clientid,
                    file_id: entry.fileid,
                    open_other,
                    owner,
                    ranges: vec![range],
                },
            );
            stateid
        };
        cursor.stateid = result.clone();
        let mut body = XdrWriter::with_capacity(16);
        write_stateid(&mut body, &result);
        V4OpResult::with_body(OP_LOCK, NFS4_OK, body.into_bytes())
    }

    /// Test a byte-range lock without taking it.  The clientid in the wire
    /// lock_owner is intentionally ignored; v4.1 derives identity from the
    /// SEQUENCE session, as the upstream protocol implementation does.
    async fn lockt(
        &self,
        lock_type: u32,
        offset: u64,
        length: u64,
        owner: &[u8],
        cursor: &Cursor,
    ) -> V4OpResult {
        let Some(access) = lock_access(lock_type) else {
            return V4OpResult::new(OP_LOCKT, NFS4ERR_LOCK_NOTSUPP);
        };
        if !valid_lock_range(offset, length) {
            return V4OpResult::new(OP_LOCKT, NFS4ERR_INVAL);
        }
        let Some(clientid) = cursor.clientid else {
            return V4OpResult::new(OP_LOCKT, NFS4ERR_OP_NOT_IN_SESSION);
        };
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_LOCKT, error_status(&error)),
        };
        let stats = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => return V4OpResult::new(OP_LOCKT, error_status(&error)),
        };
        if let Err(status) = regular_file_status(&stats) {
            return V4OpResult::new(OP_LOCKT, status);
        }
        let entry = self.handles.bind(&path, &stats);
        let state = self.state.lock().expect("NFSv4 state lock");
        let denied = state.locks.values().find_map(|held| {
            if held.file_id != entry.fileid || (held.clientid == clientid && held.owner == owner) {
                return None;
            }
            held.ranges.iter().find_map(|range| {
                (lock_overlaps(range, offset, length) && lock_conflicts(range.lock_type, access))
                    .then(|| (range.clone(), held.clientid, held.owner.clone()))
            })
        });
        match denied {
            Some((range, held_clientid, held_owner)) => V4OpResult::with_body(
                OP_LOCKT,
                NFS4ERR_DENIED,
                lock_denied_body(&range, held_clientid, &held_owner),
            ),
            None => V4OpResult::new(OP_LOCKT, NFS4_OK),
        }
    }

    /// Release exactly one range from a lock state.  The wire lock type and
    /// seqid are deliberately ignored for v4.1 LOCKU; the stateid identifies
    /// the lock owner and the range is the only release selector.
    async fn locku(
        &self,
        _lock_type: u32,
        _seqid: u32,
        stateid: &Stateid4,
        offset: u64,
        length: u64,
        cursor: &mut Cursor,
    ) -> V4OpResult {
        if !valid_lock_range(offset, length) {
            return V4OpResult::new(OP_LOCKU, NFS4ERR_INVAL);
        }
        let Some(clientid) = cursor.clientid else {
            return V4OpResult::new(OP_LOCKU, NFS4ERR_OP_NOT_IN_SESSION);
        };
        let path = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_LOCKU, error_status(&error)),
        };
        let stats = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => return V4OpResult::new(OP_LOCKU, error_status(&error)),
        };
        if let Err(status) = regular_file_status(&stats) {
            return V4OpResult::new(OP_LOCKU, status);
        }
        let entry = self.handles.bind(&path, &stats);
        if is_zero_stateid(stateid) || is_invalid_stateid(stateid) {
            return V4OpResult::new(OP_LOCKU, NFS4ERR_BAD_STATEID);
        }
        let mut state = self.state.lock().expect("NFSv4 state lock");
        let Some(lock) = state.locks.get_mut(&stateid.other) else {
            return V4OpResult::new(OP_LOCKU, NFS4ERR_BAD_STATEID);
        };
        if lock.clientid != clientid || lock.file_id != entry.fileid {
            return V4OpResult::new(OP_LOCKU, NFS4ERR_BAD_STATEID);
        }
        if stateid.seqid != lock.stateid.seqid {
            return V4OpResult::new(
                OP_LOCKU,
                if stateid.seqid < lock.stateid.seqid {
                    NFS4ERR_OLD_STATEID
                } else {
                    NFS4ERR_BAD_STATEID
                },
            );
        }
        lock.ranges = lock
            .ranges
            .iter()
            .flat_map(|range| subtract_lock_range(range, offset, length))
            .collect();
        lock.stateid.seqid = bump_stateid_seq(lock.stateid.seqid);
        let result = lock.stateid.clone();
        cursor.stateid = result.clone();
        let mut body = XdrWriter::with_capacity(16);
        write_stateid(&mut body, &result);
        V4OpResult::with_body(OP_LOCKU, NFS4_OK, body.into_bytes())
    }

    async fn open(&self, args: &OpenArgs, cursor: &mut Cursor) -> V4OpResult {
        let Some(access) = open_access_bits(args.share_access) else {
            return V4OpResult::new(OP_OPEN, NFS4ERR_INVAL);
        };
        if args.share_deny > OPEN4_SHARE_DENY_BOTH {
            return V4OpResult::new(OP_OPEN, NFS4ERR_INVAL);
        }
        let Some(clientid) = cursor.clientid else {
            return V4OpResult::new(OP_OPEN, NFS4ERR_OP_NOT_IN_SESSION);
        };
        {
            let state = self.state.lock().expect("NFSv4 state lock");
            let Some(client) = state.clients.get(&clientid) else {
                return V4OpResult::new(OP_OPEN, NFS4ERR_STALE_CLIENTID);
            };
            if self.options.nfs4.require_reclaim_complete && !client.reclaim_complete {
                return V4OpResult::new(OP_OPEN, NFS4ERR_GRACE);
            }
        }
        let path = if args.claim == CLAIM_NULL {
            let directory = match self.current_path(cursor) {
                Ok(path) => path,
                Err(error) => return V4OpResult::new(OP_OPEN, error_status(&error)),
            };
            let Some(name) = args.name.as_deref() else {
                return V4OpResult::new(OP_OPEN, NFS4ERR_INVAL);
            };
            if let Err(error) = check_name(name) {
                return V4OpResult::new(OP_OPEN, error_status(&error));
            }
            join_path(&directory, name)
        } else if matches!(args.claim, CLAIM_FH | CLAIM_DELEG_PREV_FH) {
            match self.current_path(cursor) {
                Ok(path) => path,
                Err(error) => return V4OpResult::new(OP_OPEN, error_status(&error)),
            }
        } else {
            return V4OpResult::new(OP_OPEN, NFS4ERR_NOTSUPP);
        };
        let parent = mount_rs_core::path::dirname(&path);
        let before = self
            .stat_of(&parent)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        let existing = self.stat_of(&path).await.ok();
        let exclusive = matches!(args.create_mode, Some(EXCLUSIVE4 | EXCLUSIVE4_1));
        let remembered_attrset = if exclusive && existing.is_some() {
            let Some(verifier) = args.create_verf else {
                return V4OpResult::new(OP_OPEN, NFS4ERR_INVAL);
            };
            self.state
                .lock()
                .expect("NFSv4 state lock")
                .exclusive_creates
                .get(&path)
                .filter(|entry| entry.verifier == verifier)
                .map(|entry| entry.attrset.clone())
        } else {
            None
        };
        if exclusive && existing.is_some() && remembered_attrset.is_none() {
            return V4OpResult::new(OP_OPEN, NFS4ERR_EXIST);
        }
        let mut should_create = false;
        if args.open_type == OPEN4_NOCREATE {
            if existing.is_none() {
                return V4OpResult::new(OP_OPEN, NFS4ERR_NOENT);
            }
        } else if args.open_type == OPEN4_CREATE {
            match args.create_mode.unwrap_or(UNCHECKED4) {
                GUARDED4 if existing.is_some() => return V4OpResult::new(OP_OPEN, NFS4ERR_EXIST),
                EXCLUSIVE4 | EXCLUSIVE4_1 if existing.is_none() => should_create = true,
                UNCHECKED4 if existing.is_none() => should_create = true,
                _ => {}
            }
        } else {
            return V4OpResult::new(OP_OPEN, NFS4ERR_INVAL);
        }
        let mode = args
            .create_attrs
            .as_ref()
            .and_then(|attrs| attrs.values.mode)
            .unwrap_or(0o666);
        let flags = OpenFlags {
            read: access & OPEN4_SHARE_ACCESS_READ != 0,
            write: access & OPEN4_SHARE_ACCESS_WRITE != 0,
            create: should_create,
            truncate: false,
            append: false,
            exclusive: matches!(args.create_mode, Some(EXCLUSIVE4 | EXCLUSIVE4_1)) && should_create,
        };
        let mut handle = Some(
            match self.driver.open_flags(&path, flags, mode & 0o7777).await {
                Ok(handle) => handle,
                Err(error) => return V4OpResult::new(OP_OPEN, error_status(&error)),
            },
        );
        let mut attrset = remembered_attrset.unwrap_or_default();
        if let Some(attrs) = args.create_attrs.as_ref()
            && should_create
        {
            attrset = match self.apply_attrs(&path, attrs).await {
                Ok(applied) => applied,
                Err(status) => {
                    if let Some(handle) = handle.take() {
                        let _ = handle.close().await;
                    }
                    return V4OpResult::new(OP_OPEN, status);
                }
            };
        } else if !should_create
            && args.create_mode == Some(UNCHECKED4)
            && args
                .create_attrs
                .as_ref()
                .and_then(|attrs| attrs.values.size)
                == Some(0)
        {
            if let Err(error) = self.driver.truncate(&path, 0).await {
                if let Some(handle) = handle.take() {
                    let _ = handle.close().await;
                }
                return V4OpResult::new(OP_OPEN, error_status(&error));
            }
            attrset = bitmap_of([FATTR4_SIZE]);
        }
        let stats = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => {
                if let Some(handle) = handle.take() {
                    let _ = handle.close().await;
                }
                return V4OpResult::new(OP_OPEN, error_status(&error));
            }
        };
        let entry = self.handles.bind(&path, &stats);
        // Pin before taking the state lock: a concurrent v3 lookup may bind
        // more names while this OPEN is checking share conflicts.
        self.handles.pin(entry.id);
        let share_conflict = self
            .state
            .lock()
            .expect("NFSv4 state lock")
            .opens
            .values()
            .any(|open| {
                open.file_id == entry.fileid
                    && !(open.clientid == clientid && open.owner == args.owner)
                    && (open.deny & access != 0 || args.share_deny & open.access != 0)
            });
        if share_conflict {
            self.handles.unpin(entry.id);
            if let Some(handle) = handle.take() {
                let _ = handle.close().await;
            }
            return V4OpResult::new(OP_OPEN, NFS4ERR_SHARE_DENIED);
        }
        let (stateid, open_limit_reached) = {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            let existing_key = state
                .opens
                .iter()
                .find(|(_, open)| {
                    open.clientid == clientid
                        && open.file_id == entry.fileid
                        && open.owner == args.owner
                })
                .map(|(key, _)| *key);
            if let Some(key) = existing_key {
                self.handles.unpin(entry.id);
                let open = state
                    .opens
                    .get_mut(&key)
                    .expect("open state found while holding state lock");
                open.access |= access;
                open.deny |= args.share_deny;
                open.stateid.seqid = bump_stateid_seq(open.stateid.seqid);
                (Some(open.stateid.clone()), false)
            } else {
                let open_count = state
                    .opens
                    .values()
                    .filter(|open| open.file_id == entry.fileid)
                    .count();
                if open_count >= self.options.nfs4.max_opens_per_file.max(1) {
                    (None, true)
                } else {
                    let stateid = open_stateid(1, clientid, entry.fileid, &args.owner);
                    if should_create
                        && exclusive
                        && let Some(verifier) = args.create_verf
                    {
                        state.exclusive_creates.insert(
                            path.clone(),
                            ExclusiveV4 {
                                verifier,
                                attrset: attrset.clone(),
                            },
                        );
                    }
                    state.opens.insert(
                        stateid.other,
                        OpenState {
                            stateid: stateid.clone(),
                            clientid,
                            handle_id: entry.id,
                            file_id: entry.fileid,
                            path: path.clone(),
                            handle: handle.take().expect("new open backend handle"),
                            access,
                            deny: args.share_deny,
                            owner: args.owner.clone(),
                        },
                    );
                    (Some(stateid), false)
                }
            }
        };
        if open_limit_reached {
            self.handles.unpin(entry.id);
            if let Some(handle) = handle.take() {
                let _ = handle.close().await;
            }
            return V4OpResult::new(OP_OPEN, NFS4ERR_RESOURCE);
        }
        let stateid = stateid.expect("open state unless the per-file limit was reached");
        if let Some(handle) = handle {
            let _ = handle.close().await;
        }
        cursor.current = Some(self.handles.encode(&entry));
        cursor.stateid = stateid.clone();
        let after = self
            .stat_of(&parent)
            .await
            .ok()
            .map(|stats| stat_change(&stats));
        let mut body = XdrWriter::with_capacity(64);
        write_stateid(&mut body, &stateid);
        write_change_info(&mut body, before, after);
        body.u32(OPEN4_RESULT_LOCKTYPE_POSIX);
        write_bitmap(&mut body, &attrset);
        body.u32(OPEN_DELEGATE_NONE);
        V4OpResult::with_body(OP_OPEN, NFS4_OK, body.into_bytes())
    }

    async fn close(&self, _seqid: u32, stateid: &Stateid4, cursor: &mut Cursor) -> V4OpResult {
        if let Err(status) = self.validate_stateid(stateid, cursor, None) {
            return V4OpResult::new(OP_CLOSE, status);
        }
        let handle = {
            let mut state = self.state.lock().expect("NFSv4 state lock");
            let Some(open) = state.opens.get(&stateid.other).cloned() else {
                return V4OpResult::new(OP_CLOSE, NFS4ERR_BAD_STATEID);
            };
            if state.locks.values().any(|lock| {
                lock.clientid == open.clientid
                    && lock.file_id == open.file_id
                    && lock.open_other == open.stateid.other
                    && !lock.ranges.is_empty()
            }) {
                return V4OpResult::new(OP_CLOSE, NFS4ERR_LOCKS_HELD);
            }
            state.locks.retain(|_, lock| {
                !(lock.clientid == open.clientid
                    && lock.file_id == open.file_id
                    && lock.open_other == open.stateid.other)
            });
            state
                .opens
                .remove(&stateid.other)
                .map(|open| (open.handle_id, open.handle))
        };
        if let Some((handle_id, handle)) = handle {
            let result = handle.close().await;
            self.handles.unpin(handle_id);
            if let Err(error) = result {
                return V4OpResult::new(OP_CLOSE, error_status(&error));
            }
        }
        cursor.stateid = Stateid4::zero();
        let mut body = XdrWriter::with_capacity(16);
        write_stateid(&mut body, &invalid_stateid());
        V4OpResult::with_body(OP_CLOSE, NFS4_OK, body.into_bytes())
    }

    async fn read(
        &self,
        stateid: &Stateid4,
        requested_offset: u64,
        requested_count: u32,
        cursor: &Cursor,
    ) -> V4OpResult {
        let open = match self.validate_stateid(stateid, cursor, Some(OPEN4_SHARE_ACCESS_READ)) {
            Ok(open) => open,
            Err(status) => return V4OpResult::new(OP_READ, status),
        };
        let (path, held_handle) = match open {
            Some(open) => (open.path, Some(open.handle)),
            None => (
                match self.current_path(cursor) {
                    Ok(path) => path,
                    Err(error) => return V4OpResult::new(OP_READ, error_status(&error)),
                },
                None,
            ),
        };
        let offset = match offset(requested_offset, "read") {
            Ok(offset) => offset,
            Err(error) => return V4OpResult::new(OP_READ, error_status(&error)),
        };
        let count = usize::try_from(requested_count)
            .unwrap_or(DEFAULT_MAX_READ)
            .min(DEFAULT_MAX_READ)
            .min(self.options.rtmax);
        let close_after = held_handle.is_none();
        let handle = match held_handle {
            Some(handle) => handle,
            None => match self.driver.open_flags(&path, OpenFlags::READ_ONLY, 0).await {
                Ok(handle) => handle,
                Err(error) => return V4OpResult::new(OP_READ, error_status(&error)),
            },
        };
        let mut data = vec![0_u8; count];
        let result = async {
            let read = handle.read(&mut data, Some(offset)).await?;
            data.truncate(read.min(data.len()));
            let size = handle.stat().await.ok().map(|stats| stats.size);
            Ok::<Option<u64>, FsError>(size)
        }
        .await;
        if close_after {
            let _ = handle.close().await;
        }
        let size = match result {
            Ok(size) => size,
            Err(error) => return V4OpResult::new(OP_READ, error_status(&error)),
        };
        let size = match size {
            Some(size) => Some(size),
            None => self.stat_of(&path).await.ok().map(|stats| stats.size),
        };
        let eof = size.is_some_and(|size| offset.saturating_add(data.len() as u64) >= size);
        let mut body = XdrWriter::with_capacity(data.len() + 8);
        body.bool(eof);
        body.var_opaque(&data);
        V4OpResult::with_body(OP_READ, NFS4_OK, body.into_bytes())
    }

    async fn write(
        &self,
        stateid: &Stateid4,
        requested_offset: u64,
        stable: u32,
        data: &[u8],
        cursor: &Cursor,
    ) -> V4OpResult {
        let open = match self.validate_stateid(stateid, cursor, Some(OPEN4_SHARE_ACCESS_WRITE)) {
            Ok(open) => open,
            Err(status) => return V4OpResult::new(OP_WRITE, status),
        };
        let (path, held_handle) = match open {
            Some(open) => (open.path, Some(open.handle)),
            None => (
                match self.current_path(cursor) {
                    Ok(path) => path,
                    Err(error) => return V4OpResult::new(OP_WRITE, error_status(&error)),
                },
                None,
            ),
        };
        if stable > FILE_SYNC4 {
            return V4OpResult::new(OP_WRITE, NFS4ERR_INVAL);
        }
        let offset = match offset(requested_offset, "write") {
            Ok(offset) => offset,
            Err(error) => return V4OpResult::new(OP_WRITE, error_status(&error)),
        };
        let count = data.len().min(self.options.wtmax).min(DEFAULT_MAX_WRITE);
        let flags = OpenFlags {
            read: false,
            write: true,
            create: false,
            truncate: false,
            append: false,
            exclusive: false,
        };
        let close_after = held_handle.is_none();
        let handle = match held_handle {
            Some(handle) => handle,
            None => match self.driver.open_flags(&path, flags, 0).await {
                Ok(handle) => handle,
                Err(error) => return V4OpResult::new(OP_WRITE, error_status(&error)),
            },
        };
        let result = handle.write(&data[..count], Some(offset)).await;
        let written = match result {
            Ok(written) => written.min(count),
            Err(error) => {
                if close_after {
                    let _ = handle.close().await;
                }
                return V4OpResult::new(OP_WRITE, error_status(&error));
            }
        };
        let sync_result = match stable {
            FILE_SYNC4 => handle.sync().await,
            DATA_SYNC4 => handle.datasync().await,
            UNSTABLE4 => Ok(()),
            _ => unreachable!("stable was checked above"),
        };
        if close_after {
            let _ = handle.close().await;
        }
        if let Err(error) = sync_result {
            return V4OpResult::new(OP_WRITE, error_status(&error));
        }
        let mut body = XdrWriter::with_capacity(32);
        body.u32(written as u32);
        body.u32(if stable == UNSTABLE4 {
            UNSTABLE4
        } else {
            FILE_SYNC4
        });
        body.fixed_opaque(&self.write_verifier, NFS4_VERIFIER_SIZE);
        V4OpResult::with_body(OP_WRITE, NFS4_OK, body.into_bytes())
    }

    async fn secinfo(&self, name: &str, cursor: &Cursor) -> V4OpResult {
        let directory = match self.current_path(cursor) {
            Ok(path) => path,
            Err(error) => return V4OpResult::new(OP_SECINFO, error_status(&error)),
        };
        if let Err(error) = check_name(name) {
            return V4OpResult::new(OP_SECINFO, error_status(&error));
        }
        let path = join_path(&directory, name);
        if let Err(error) = self.stat_of(&path).await {
            return V4OpResult::new(OP_SECINFO, error_status(&error));
        }
        V4OpResult::with_body(OP_SECINFO, NFS4_OK, security_flavors())
    }

    fn secinfo_no_name(&self, style: u32, cursor: &Cursor) -> V4OpResult {
        if style != SECINFO_STYLE4_CURRENT_FH && style != SECINFO_STYLE4_PARENT {
            return V4OpResult::new(OP_SECINFO_NO_NAME, NFS4ERR_INVAL);
        }
        if cursor.current.is_none() {
            return V4OpResult::new(OP_SECINFO_NO_NAME, NFS4ERR_NOFILEHANDLE);
        }
        V4OpResult::with_body(OP_SECINFO_NO_NAME, NFS4_OK, security_flavors())
    }
}

fn is_sessionless(operation: &Op) -> bool {
    matches!(
        operation,
        Op::ExchangeId { .. }
            | Op::CreateSession { .. }
            | Op::DestroySession(_)
            | Op::DestroyClientid(_)
            | Op::BindConnToSession { .. }
    )
}

fn write_change_info(writer: &mut XdrWriter, before: Option<u64>, after: Option<u64>) {
    writer.bool(before.is_some() && after.is_some());
    writer.u64(before.unwrap_or_default());
    writer.u64(after.unwrap_or_default());
}

fn security_flavors() -> Vec<u8> {
    let mut writer = XdrWriter::with_capacity(16);
    writer.u32(2);
    writer.u32(AUTH_NONE);
    writer.u32(AUTH_SYS);
    writer.into_bytes()
}

fn settable_attr(bit: u32) -> bool {
    matches!(
        bit,
        FATTR4_MODE
            | FATTR4_SIZE
            | FATTR4_OWNER
            | FATTR4_OWNER_GROUP
            | FATTR4_TIME_ACCESS_SET
            | FATTR4_TIME_MODIFY_SET
    )
}

fn requested_time(value: Option<(u32, Option<(i64, u32)>)>) -> Result<Option<i64>, u32> {
    match value {
        Some((0, _)) => Ok(Some(mount_rs_core::types::now_ms())),
        Some((1, Some((seconds, nanos)))) => {
            let millis = i128::from(seconds)
                .saturating_mul(1000)
                .saturating_add(i128::from(nanos) / 1_000_000);
            i64::try_from(millis).map(Some).map_err(|_| NFS4ERR_INVAL)
        }
        Some((1, None)) => Err(NFS4ERR_INVAL),
        Some(_) => Err(NFS4ERR_INVAL),
        None => Ok(None),
    }
}

fn nfstime_ms(seconds: i64, nanos: u32) -> i64 {
    let millis = i128::from(seconds)
        .saturating_mul(1000)
        .saturating_add(i128::from(nanos) / 1_000_000);
    millis.clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn lock_access(lock_type: u32) -> Option<u32> {
    match lock_type {
        READ_LT | READW_LT => Some(READ_LT),
        WRITE_LT | WRITEW_LT => Some(WRITE_LT),
        _ => None,
    }
}

fn lock_end(offset: u64, length: u64) -> u128 {
    if length == u64::MAX {
        1_u128 << 64
    } else {
        u128::from(offset) + u128::from(length)
    }
}

fn lock_overlaps(left: &LockRange, right_offset: u64, right_length: u64) -> bool {
    u128::from(left.offset) < lock_end(right_offset, right_length)
        && u128::from(right_offset) < lock_end(left.offset, left.length)
}

fn lock_conflicts(left: u32, right: u32) -> bool {
    left == WRITE_LT || right == WRITE_LT
}

fn valid_lock_range(offset: u64, length: u64) -> bool {
    length != 0
        && (length == u64::MAX || (u128::from(offset) + u128::from(length) <= u128::from(u64::MAX)))
}

fn subtract_lock_range(
    range: &LockRange,
    requested_offset: u64,
    requested_length: u64,
) -> Vec<LockRange> {
    if !lock_overlaps(range, requested_offset, requested_length) {
        return vec![range.clone()];
    }
    let start = u128::from(range.offset);
    let end = lock_end(range.offset, range.length);
    let requested_start = u128::from(requested_offset);
    let requested_end = lock_end(requested_offset, requested_length);
    let mut result = Vec::with_capacity(2);
    if start < requested_start {
        result.push(LockRange {
            offset: range.offset,
            length: lock_length(start, requested_start),
            lock_type: range.lock_type,
        });
    }
    if end > requested_end {
        result.push(LockRange {
            offset: requested_end as u64,
            length: lock_length(requested_end, end),
            lock_type: range.lock_type,
        });
    }
    result
}

fn lock_length(start: u128, end: u128) -> u64 {
    if end == 1_u128 << 64 {
        u64::MAX
    } else {
        end.saturating_sub(start) as u64
    }
}

fn coalesce_lock_ranges(ranges: &mut Vec<LockRange>) {
    ranges.sort_by_key(|range| range.offset);
    let mut merged: Vec<LockRange> = Vec::with_capacity(ranges.len());
    for range in ranges.drain(..) {
        if let Some(previous) = merged.last_mut()
            && previous.lock_type == range.lock_type
            && lock_end(previous.offset, previous.length) >= u128::from(range.offset)
        {
            let end = lock_end(previous.offset, previous.length)
                .max(lock_end(range.offset, range.length));
            previous.length = lock_length(u128::from(previous.offset), end);
        } else {
            merged.push(range);
        }
    }
    *ranges = merged;
}

fn regular_file_status(stats: &Stats) -> Result<(), u32> {
    match stats.mode & S_IFMT {
        S_IFREG => Ok(()),
        S_IFDIR => Err(NFS4ERR_ISDIR),
        S_IFLNK => Err(NFS4ERR_SYMLINK),
        _ => Err(NFS4ERR_WRONG_TYPE),
    }
}

fn lock_denied_body(range: &LockRange, clientid: u64, owner: &[u8]) -> Vec<u8> {
    let mut body = XdrWriter::with_capacity(32 + owner.len());
    body.u64(range.offset);
    body.u64(range.length);
    body.u32(range.lock_type);
    body.u64(clientid);
    body.var_opaque(owner);
    body.into_bytes()
}

fn bump_stateid_seq(seqid: u32) -> u32 {
    if seqid == u32::MAX { 1 } else { seqid + 1 }
}

/// Compare stateid seqids using the serial-number arithmetic in RFC 7530
/// Section 9.1.3. Stateid seqids advance from UINT32_MAX to one, and the
/// half-range boundary is deliberately treated as older rather than newer.
fn compare_stateid_seqid(requested: u32, current: u32) -> SeqidOrdering {
    match requested.wrapping_sub(current) {
        0 => SeqidOrdering::Equal,
        0x8000_0000.. => SeqidOrdering::Less,
        _ => SeqidOrdering::Greater,
    }
}

fn lock_stateid(seqid: u32, clientid: u64, fileid: u64, owner: &[u8]) -> Stateid4 {
    let mut stateid = owner_stateid(seqid, clientid, fileid, owner);
    stateid.other[0] ^= 0x80;
    stateid
}

fn open_stateid(seqid: u32, clientid: u64, fileid: u64, owner: &[u8]) -> Stateid4 {
    owner_stateid(seqid, clientid, fileid, owner)
}

fn owner_stateid(seqid: u32, clientid: u64, fileid: u64, owner: &[u8]) -> Stateid4 {
    let mut stateid = make_stateid(seqid, clientid, fileid);
    let mut hash = 2_166_136_261_u32;
    for byte in owner {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    stateid.other[8..].copy_from_slice(&hash.to_be_bytes());
    stateid
}

fn make_stateid(seqid: u32, clientid: u64, fileid: u64) -> Stateid4 {
    let mut other = [0_u8; NFS4_OTHER_SIZE];
    other[..8].copy_from_slice(&clientid.to_be_bytes());
    other[8..].copy_from_slice(&(fileid as u32).to_be_bytes());
    Stateid4 { seqid, other }
}

/// Fold the configured boot seed into the high half of an identity while
/// retaining a monotonic counter in the low half.
fn seeded_counter(seed: u32, counter: u64) -> u64 {
    (u64::from(seed) << 32) | (counter & u64::from(u32::MAX))
}

/// Build a session identity from the configured seed and both halves of the
/// write verifier, with the session counter in the remaining bytes.
fn session_id(seed: u32, write_verifier: &[u8; 8], counter: u64) -> [u8; NFS4_SESSIONID_SIZE] {
    let mut id = [0_u8; NFS4_SESSIONID_SIZE];
    id[..4].copy_from_slice(&seed.to_be_bytes());
    let verifier_tag = u32::from_be_bytes(
        write_verifier[..4]
            .try_into()
            .expect("the write verifier has an upper half"),
    ) ^ u32::from_be_bytes(
        write_verifier[4..]
            .try_into()
            .expect("the write verifier has a lower half"),
    )
    .rotate_left(13);
    id[4..8].copy_from_slice(&verifier_tag.to_be_bytes());
    id[8..].copy_from_slice(&counter.to_be_bytes());
    id
}

fn allowed_access4(stats: &Stats, credentials: &RpcCredentials) -> u32 {
    let is_dir = stats.mode & S_IFMT == S_IFDIR;
    let mode = stats.mode & 0o777;
    let uid = credentials.uid.unwrap_or(0);
    let gid = credentials.gid.unwrap_or(0);
    let root = uid == 0;
    let bits = if root {
        0b111
    } else if uid == stats.uid {
        (mode >> 6) & 0b111
    } else if gid == stats.gid || credentials.gids.contains(&stats.gid) {
        (mode >> 3) & 0b111
    } else {
        mode & 0b111
    };
    let read = bits & 0b100 != 0;
    let write = bits & 0b010 != 0;
    let execute = if root {
        mode & 0o111 != 0 || is_dir
    } else {
        bits & 0b001 != 0
    };
    (if read { ACCESS4_READ } else { 0 })
        | (if is_dir && execute { ACCESS4_LOOKUP } else { 0 })
        | (if write { ACCESS4_MODIFY } else { 0 })
        | (if write { ACCESS4_EXTEND } else { 0 })
        | (if is_dir && write { ACCESS4_DELETE } else { 0 })
        | (if !is_dir && execute {
            ACCESS4_EXECUTE
        } else {
            0
        })
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use mount_rs_core::MemoryFs;

    use super::{Nfs4Session, SeqidOrdering, bump_stateid_seq, compare_stateid_seqid, session_id};
    use crate::{
        NFS_V4, NFS4_PROGRAM, Nfs4IdMap, NfsSessionError, NfsSessionHooks, NfsSessionOptions,
        decode_reply, encode_call,
    };

    #[test]
    fn id_map_uses_numeric_fallback_and_separate_user_group_namespaces() {
        let map = Nfs4IdMap::new(Some("example.test".to_owned()))
            .with_user("alice", 1000)
            .with_group("staff", 1000);

        assert_eq!(map.domain(), Some("example.test"));
        assert_eq!(map.name_of(1000, false), Some("alice"));
        assert_eq!(map.name_of(1000, true), Some("staff"));
        assert_eq!(map.id_of("alice", false), Some(1000));
        assert_eq!(map.id_of("staff", true), Some(1000));
        assert_eq!(map.name_of(1001, false), None);
        assert_eq!(map.id_of("staff", false), None);
    }

    #[test]
    fn owner_translation_qualifies_names_and_rejects_other_domains() {
        let mut options = NfsSessionOptions::default();
        options.nfs4.idmap = Some(
            Nfs4IdMap::new(Some("example.test".to_owned()))
                .with_user("alice", 1000)
                .with_group("staff", 1000),
        );
        let session = Nfs4Session::new(MemoryFs::empty(), options);

        assert_eq!(session.owner_name(1000, false), "alice@example.test");
        assert_eq!(session.owner_name(1001, false), "1001");
        assert_eq!(session.owner_id("alice@example.test", false), Ok(1000));
        assert_eq!(session.owner_id("1000", false), Ok(1000));
        assert_eq!(
            session.owner_id("alice@other.test", false),
            Err(super::NFS4ERR_BADOWNER)
        );
        assert_eq!(
            session.owner_id("missing@example.test", false),
            Err(super::NFS4ERR_BADOWNER)
        );
    }

    #[test]
    fn owner_translation_callbacks_cover_user_group_and_panic_boundaries() {
        let mut options = NfsSessionOptions::default();
        options.nfs4.idmap = Some(
            Nfs4IdMap::new(Some("example.test".to_owned()))
                .with_name_of(|id, group| match (id, group) {
                    (1000, false) => Some("alice".to_owned()),
                    (1000, true) => Some("staff".to_owned()),
                    _ => None,
                })
                .with_id_of(|name, group| match (name, group) {
                    ("alice", false) => Some(1000),
                    ("staff", true) => Some(1000),
                    _ => None,
                }),
        );
        let session = Nfs4Session::new(MemoryFs::empty(), options);

        assert_eq!(session.owner_name(1000, false), "alice@example.test");
        assert_eq!(session.owner_name(1000, true), "staff@example.test");
        assert_eq!(session.owner_name(1001, false), "1001");
        assert_eq!(session.owner_id("alice@example.test", false), Ok(1000));
        assert_eq!(session.owner_id("staff@example.test", true), Ok(1000));
        assert_eq!(
            session.owner_id("missing@example.test", false),
            Err(super::NFS4ERR_BADOWNER)
        );

        let panicking = Nfs4IdMap::new(None)
            .with_name_of(|_, _| panic!("name callback panic"))
            .with_id_of(|_, _| panic!("id callback panic"));
        assert_eq!(panicking.resolve_name(1000, false), None);
        assert_eq!(panicking.resolve_id("alice", false), None);
    }

    #[test]
    fn stateid_seqids_use_serial_arithmetic_at_wrap() {
        assert_eq!(bump_stateid_seq(u32::MAX), 1);
        assert_eq!(compare_stateid_seqid(1, u32::MAX), SeqidOrdering::Greater);
        assert_eq!(compare_stateid_seqid(u32::MAX, 1), SeqidOrdering::Less);
        assert_eq!(
            compare_stateid_seqid(0x8000_0001, 1),
            SeqidOrdering::Less,
            "the half-range boundary is treated as older"
        );
        assert_eq!(compare_stateid_seqid(17, 17), SeqidOrdering::Equal);
    }

    #[tokio::test]
    async fn session_slot_sequence_wraps_to_zero() {
        let session = Nfs4Session::new(MemoryFs::empty(), NfsSessionOptions::default());
        let id = [7_u8; super::NFS4_SESSIONID_SIZE];
        session.state.lock().unwrap().sessions.insert(
            id,
            super::SessionState {
                id,
                clientid: 1,
                next_sequence: vec![u32::MAX],
                in_flight: vec![None],
                cached: vec![None],
                max_operations: 1,
                max_cached: 1024,
            },
        );
        let credentials = crate::rpc::RpcCredentials {
            flavor: crate::rpc::AUTH_NONE,
            uid: None,
            gid: None,
            gids: Vec::new(),
        };
        for (sequence, expected_next) in [(u32::MAX, 0), (0, 1)] {
            let mut writer = crate::XdrWriter::new();
            writer.string("slot-wrap");
            writer.u32(super::NFS4_MINOR_VERSION_1);
            writer.u32(1);
            writer.u32(super::OP_SEQUENCE);
            writer.fixed_opaque(&id, super::NFS4_SESSIONID_SIZE);
            writer.u32(sequence);
            writer.u32(0);
            writer.u32(0);
            writer.bool(true);
            let body = writer.into_bytes();
            let mut reader = crate::XdrReader::new(&body);
            let reply = session
                .dispatch_compound(&mut reader, &credentials, None, sequence)
                .await
                .expect("dispatch wraparound SEQUENCE");
            assert_eq!(
                crate::XdrReader::new(&reply).u32("status").unwrap(),
                super::NFS4_OK
            );
            let state = session.state.lock().unwrap();
            let slot = &state.sessions[&id];
            assert_eq!(slot.next_sequence[0], expected_next);
            assert_eq!(slot.in_flight[0], None);
        }
    }

    #[test]
    fn session_ids_include_both_write_verifier_halves() {
        let first = session_id(7, &[0x12, 0x34, 0x56, 0x78, 0, 0, 0, 1], 9);
        let second = session_id(7, &[0x12, 0x34, 0x56, 0x78, 0, 0, 0, 2], 9);

        assert_eq!(&first[..4], &second[..4]);
        assert_eq!(&first[8..], &second[8..]);
        assert_ne!(&first[4..8], &second[4..8]);
    }

    #[tokio::test]
    async fn request_errors_report_decoded_call_and_survive_hook_panics() {
        let observed = Arc::new(Mutex::new(Vec::<(String, Option<u32>)>::new()));
        let callback_observed = Arc::clone(&observed);
        let hooks = NfsSessionHooks {
            on_error: Some(Arc::new(move |error: NfsSessionError, call| {
                callback_observed
                    .lock()
                    .expect("NFS session error lock")
                    .push((error.message, call.map(|call| call.xid)));
                panic!("request-level hook must not take down the session");
            })),
        };
        let session =
            Nfs4Session::new_with_hooks(MemoryFs::empty(), NfsSessionOptions::default(), hooks);
        let request = encode_call(
            45,
            NFS4_PROGRAM,
            NFS_V4,
            super::NFSPROC4_COMPOUND,
            None,
            None,
            &[],
        );
        let reply = session
            .handle_call(&request, crate::NfsRequestContext::default())
            .await
            .expect("decoded malformed request still receives a reply");
        let (reply, _) = decode_reply(&reply).expect("decode RPC error reply");
        assert_eq!(reply.accept_stat, Some(crate::rpc::RPC_GARBAGE_ARGS));
        assert_eq!(session.stats().errors, 1);
        let observed = observed.lock().expect("NFS session error lock");
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].1, Some(45));
        assert!(observed[0].0.contains("COMPOUND.tag"));
    }
}
