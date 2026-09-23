//! Rootless NFSv4.1 wire integration.
//!
//! This exercises the TCP record-marking server, the v4.1 client/session
//! handshake, slot sequencing, file handles, OPEN/READ/WRITE/CLOSE and
//! namespace cleanup. It also drives two independent v4.1 sessions through
//! concurrent file round trips. It deliberately does not invoke the host
//! kernel mount client; native mount prerequisites are platform- and
//! privilege-specific.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mount_rs_core::{DirEntry, ErrorCode, FileHandle, FsDriver, FsError, Result, Stats};
use mount_rs_host::HostFs;
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_nfs::constants::{
    CREATE_UNCHECKED, MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_MNT, NFS_PROGRAM, NFS_V3, NFS3_OK,
    NFS3ERR_NOENT, NFS3ERR_STALE, NFSPROC3_CREATE, NFSPROC3_GETATTR, NFSPROC3_LOOKUP,
    NFSPROC3_REMOVE, NFSPROC3_RENAME,
};
use mount_rs_nfs::protocol::{
    Create3args, DirOpArgs, Sattr3, read_create_res, read_getattr_res, read_lookup_res,
    read_mount_res, read_rename_res, read_wcc_res, write_create_args,
};
use mount_rs_nfs::v4::{
    ACCESS4_ALL, ACCESS4_DELETE, ACCESS4_EXTEND, ACCESS4_LOOKUP, ACCESS4_MODIFY, ACCESS4_READ,
    CLAIM_FH, CLAIM_NULL, CREATE_SESSION4_FLAG_CONN_BACK_CHAN, FATTR4_LEASE_TIME,
    NFS4ERR_BAD_HIGH_SLOT, NFS4ERR_BAD_STATEID, NFS4ERR_BADSESSION, NFS4ERR_DELAY, NFS4ERR_GRACE,
    NFS4ERR_NOSPC, NFS4ERR_REP_TOO_BIG_TO_CACHE, NFS4ERR_RESOURCE, NFS4ERR_RETRY_UNCACHED_REP,
    NFS4ERR_SEQ_FALSE_RETRY, NFS4ERR_SEQ_MISORDERED, NFS4ERR_SHARE_DENIED, NFS4ERR_STALE,
    NFS4ERR_TOO_MANY_OPS, NFS4ERR_TOOSMALL, OPEN4_CREATE, OPEN4_SHARE_ACCESS_BOTH, UNCHECKED4,
    UNSTABLE4,
};
use mount_rs_nfs::xdr::encode_xdr;
use mount_rs_nfs::{
    NFS_V4, NFS4_PROGRAM, Nfs4Clock, Nfs4IdMap, NfsServer, NfsServerOptions, OpaqueAuth,
    RecordAssembler, XdrReader, XdrWriter, auth_sys, decode_reply, encode_call, frame_record,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::runtime::Builder;
use tokio::sync::Notify;
use tokio::time::timeout;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

struct RenameSpyHostFs {
    inner: HostFs,
    rename_calls: Arc<AtomicU64>,
    lstat_enabled: Arc<AtomicBool>,
}

impl FsDriver for RenameSpyHostFs {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.inner.capabilities()
    }

    fn stat<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> BoxFuture<'async_trait, Result<Stats>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.stat(path).await })
    }

    fn lstat<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> BoxFuture<'async_trait, Result<Stats>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            if self.lstat_enabled.load(Ordering::Acquire) {
                self.inner.lstat(path).await
            } else {
                Err(FsError::new(ErrorCode::Enosys))
            }
        })
    }

    fn readdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> BoxFuture<'async_trait, Result<Vec<DirEntry>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.readdir(path).await })
    }

    fn open<'a, 'b, 'c, 'async_trait>(
        &'a self,
        path: &'b str,
        flags: &'c str,
        mode: u32,
    ) -> BoxFuture<'async_trait, Result<Arc<dyn FileHandle>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.open(path, flags, mode).await })
    }

    fn unlink<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> BoxFuture<'async_trait, Result<()>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.unlink(path).await })
    }

    fn rename<'a, 'b, 'c, 'async_trait>(
        &'a self,
        old_path: &'b str,
        new_path: &'c str,
    ) -> BoxFuture<'async_trait, Result<()>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.rename_calls.fetch_add(1, Ordering::AcqRel);
            self.inner.rename(old_path, new_path).await
        })
    }
}

struct GateStatDriver {
    inner: MemoryFs,
    block_once: Arc<AtomicBool>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl FsDriver for GateStatDriver {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.inner.capabilities()
    }

    fn stat<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> BoxFuture<'async_trait, Result<Stats>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            if path == "/" && self.block_once.swap(false, Ordering::AcqRel) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            self.inner.stat(path).await
        })
    }

    fn readdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> BoxFuture<'async_trait, Result<Vec<DirEntry>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.readdir(path).await })
    }

    fn open<'a, 'b, 'c, 'async_trait>(
        &'a self,
        path: &'b str,
        flags: &'c str,
        mode: u32,
    ) -> BoxFuture<'async_trait, Result<Arc<dyn FileHandle>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.open(path, flags, mode).await })
    }
}

struct GateUnlinkDriver {
    inner: MemoryFs,
    block_once: Arc<AtomicBool>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

struct GateTargetStatDriver {
    inner: MemoryFs,
    target_path: &'static str,
    block_on_stat: u64,
    armed: Arc<AtomicBool>,
    target_stats: Arc<AtomicU64>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

struct GateOneOpenDriver {
    inner: MemoryFs,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl FsDriver for GateOneOpenDriver {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.inner.capabilities()
    }

    fn stat<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> BoxFuture<'async_trait, Result<Stats>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.stat(path).await })
    }

    fn readdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> BoxFuture<'async_trait, Result<Vec<DirEntry>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.readdir(path).await })
    }

    fn open<'a, 'b, 'c, 'async_trait>(
        &'a self,
        path: &'b str,
        flags: &'c str,
        mode: u32,
    ) -> BoxFuture<'async_trait, Result<Arc<dyn FileHandle>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.open(path, flags, mode).await })
    }

    fn open_flags<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
        flags: mount_rs_core::OpenFlags,
        mode: u32,
    ) -> BoxFuture<'async_trait, Result<Arc<dyn FileHandle>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let handle = self.inner.open_flags(path, flags, mode).await?;
            if path == "/blocked-open.txt" {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(handle)
        })
    }
}

impl FsDriver for GateTargetStatDriver {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.inner.capabilities()
    }

    fn stat<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> BoxFuture<'async_trait, Result<Stats>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let stats = self.inner.stat(path).await?;
            if path == self.target_path
                && self.armed.load(Ordering::Acquire)
                && self.target_stats.fetch_add(1, Ordering::AcqRel) == self.block_on_stat
            {
                // Pause after the backend returns the target inode but before
                // the request binds it into the shared handle table.
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(stats)
        })
    }

    fn readdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> BoxFuture<'async_trait, Result<Vec<DirEntry>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.readdir(path).await })
    }

    fn open<'a, 'b, 'c, 'async_trait>(
        &'a self,
        path: &'b str,
        flags: &'c str,
        mode: u32,
    ) -> BoxFuture<'async_trait, Result<Arc<dyn FileHandle>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.open(path, flags, mode).await })
    }

    fn unlink<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> BoxFuture<'async_trait, Result<()>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.unlink(path).await })
    }
}

impl FsDriver for GateUnlinkDriver {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.inner.capabilities()
    }

    fn stat<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> BoxFuture<'async_trait, Result<Stats>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.stat(path).await })
    }

    fn readdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> BoxFuture<'async_trait, Result<Vec<DirEntry>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.readdir(path).await })
    }

    fn open<'a, 'b, 'c, 'async_trait>(
        &'a self,
        path: &'b str,
        flags: &'c str,
        mode: u32,
    ) -> BoxFuture<'async_trait, Result<Arc<dyn FileHandle>>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.open(path, flags, mode).await })
    }

    fn unlink<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> BoxFuture<'async_trait, Result<()>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.inner.unlink(path).await?;
            if self.block_once.swap(false, Ordering::AcqRel) {
                self.entered.notify_one();
                self.release.notified().await;
            }
            Ok(())
        })
    }
}

const OP_ACCESS: u32 = 3;
const OP_CLOSE: u32 = 4;
const OP_COMMIT: u32 = 5;
const OP_BACKCHANNEL_CTL: u32 = 40;
const OP_GETATTR: u32 = 9;
const OP_GETFH: u32 = 10;
const OP_LOCK: u32 = 12;
const OP_LOCKU: u32 = 14;
const OP_LOOKUP: u32 = 15;
const OP_OPEN: u32 = 18;
const OP_OPEN_DOWNGRADE: u32 = 21;
const OP_PUTFH: u32 = 22;
const OP_PUTROOTFH: u32 = 24;
const OP_READ: u32 = 25;
const OP_READDIR: u32 = 26;
const OP_READLINK: u32 = 27;
const OP_REMOVE: u32 = 28;
const OP_RENAME: u32 = 29;
const OP_SAVEFH: u32 = 32;
const OP_WRITE: u32 = 38;
const OP_EXCHANGE_ID: u32 = 42;
const OP_CREATE_SESSION: u32 = 43;
const OP_FREE_STATEID: u32 = 45;
const OP_RECLAIM_COMPLETE: u32 = 58;
const OP_SEQUENCE: u32 = 53;
const OP_TEST_STATEID: u32 = 55;

#[derive(Clone)]
struct Client {
    session: [u8; 16],
    clientid: u64,
    sequence: u32,
    slot: u32,
}

async fn rpc(stream: &mut TcpStream, xid: u32, args: Vec<u8>) -> XdrReader<'static> {
    rpc_with_credential(stream, xid, args, None).await
}

async fn rpc_with_credential(
    stream: &mut TcpStream,
    xid: u32,
    args: Vec<u8>,
    credential: Option<&OpaqueAuth>,
) -> XdrReader<'static> {
    rpc_call(stream, xid, NFS4_PROGRAM, NFS_V4, 1, args, credential).await
}

async fn rpc_call(
    stream: &mut TcpStream,
    xid: u32,
    program: u32,
    version: u32,
    procedure: u32,
    args: Vec<u8>,
    credential: Option<&OpaqueAuth>,
) -> XdrReader<'static> {
    let call = encode_call(xid, program, version, procedure, credential, None, &args);
    stream
        .write_all(&frame_record(&call).expect("frame RPC call"))
        .await
        .expect("write RPC call");
    let mut assembler = RecordAssembler::default();
    let mut buffer = [0_u8; 64 * 1024];
    let record = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let count = stream.read(&mut buffer).await.expect("read RPC reply");
            assert!(count > 0, "server closed before RPC reply");
            let records = assembler
                .push(&buffer[..count])
                .expect("assemble RPC reply");
            if let Some(record) = records.into_iter().next() {
                break record;
            }
        }
    })
    .await
    .expect("RPC reply timeout");
    let (reply, mut results) = decode_reply(&record).expect("decode RPC reply");
    assert_eq!(reply.xid, xid);
    assert_eq!(reply.accept_stat, Some(0));
    let bytes = results.rest();
    let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    XdrReader::new(bytes)
}

fn compound(tag: &str, operations: &[Vec<u8>]) -> Vec<u8> {
    let mut writer = XdrWriter::new();
    writer.string(tag);
    writer.u32(1);
    writer.u32(operations.len() as u32);
    for operation in operations {
        writer.raw(operation);
    }
    writer.into_bytes()
}

fn op(opcode: u32, body: impl FnOnce(&mut XdrWriter)) -> Vec<u8> {
    let mut writer = XdrWriter::new();
    writer.u32(opcode);
    body(&mut writer);
    writer.into_bytes()
}

fn sequence(client: &Client) -> Vec<u8> {
    sequence_with_cachethis(client, true)
}

fn sequence_with_cachethis(client: &Client, cachethis: bool) -> Vec<u8> {
    op(OP_SEQUENCE, |writer| {
        writer.fixed_opaque(&client.session, 16);
        writer.u32(client.sequence);
        writer.u32(client.slot);
        writer.u32(0);
        writer.bool(cachethis);
    })
}

fn parse_compound_header(reader: &mut XdrReader<'_>, expected_count: usize) {
    assert_eq!(parse_compound_status(reader, expected_count), 0);
}

fn parse_compound_status(reader: &mut XdrReader<'_>, expected_count: usize) -> u32 {
    let status = reader.u32("compound status").unwrap();
    let _ = reader.string(1024, "compound tag").unwrap();
    assert_eq!(
        reader.u32("compound result count").unwrap() as usize,
        expected_count
    );
    status
}

fn parse_result_header(reader: &mut XdrReader<'_>, expected_op: u32) {
    assert_eq!(parse_result_status(reader, expected_op), 0);
}

fn parse_result_status(reader: &mut XdrReader<'_>, expected_op: u32) -> u32 {
    assert_eq!(reader.u32("result op").unwrap(), expected_op);
    reader.u32("result status").unwrap()
}

fn consume_open_result(reader: &mut XdrReader<'_>, label: &str) -> Vec<u8> {
    parse_result_header(reader, OP_OPEN);
    let stateid = reader
        .fixed_opaque(16, &format!("{label} stateid"))
        .unwrap();
    let _ = reader.bool(&format!("{label} atomic")).unwrap();
    let _ = reader.u64(&format!("{label} change before")).unwrap();
    let _ = reader.u64(&format!("{label} change after")).unwrap();
    let _ = reader.u32(&format!("{label} result flags")).unwrap();
    let _ = reader
        .array(16, &format!("{label} attrset"), |reader| {
            reader.u32("attribute word")
        })
        .unwrap();
    assert_eq!(reader.u32(&format!("{label} delegation")).unwrap(), 0);
    stateid
}

fn consume_sequence_result(reader: &mut XdrReader<'_>, label: &str) {
    parse_result_header(reader, OP_SEQUENCE);
    let _ = reader
        .fixed_opaque(16, &format!("{label} sequence id"))
        .unwrap();
    let _ = reader.u32(&format!("{label} sequence number")).unwrap();
    let _ = reader.u32(&format!("{label} sequence slot")).unwrap();
    let _ = reader
        .u32(&format!("{label} sequence highest slot"))
        .unwrap();
    let _ = reader
        .u32(&format!("{label} sequence target slot"))
        .unwrap();
    let _ = reader
        .u32(&format!("{label} sequence status flags"))
        .unwrap();
}

fn parse_compound(reader: &mut XdrReader<'_>, expected: &[u32]) {
    parse_compound_header(reader, expected.len());
    for operation in expected {
        parse_result_header(reader, *operation);
    }
}

fn parse_exchange(mut reader: XdrReader<'_>) -> u64 {
    parse_compound(&mut reader, &[OP_EXCHANGE_ID]);
    let clientid = reader.u64("clientid").unwrap();
    assert_eq!(reader.u32("exchange sequence").unwrap(), 1);
    let _ = reader.u32("exchange flags").unwrap();
    assert_eq!(reader.u32("exchange state protect").unwrap(), 0);
    let _ = reader.u64("server owner minor id").unwrap();
    let _ = reader.var_opaque(1024, "server owner major id").unwrap();
    let _ = reader.var_opaque(1024, "server scope").unwrap();
    assert_eq!(reader.u32("implementation id count").unwrap(), 0);
    reader.end("exchange response").unwrap();
    clientid
}

fn channel(writer: &mut XdrWriter) {
    writer.u32(0);
    writer.u32(1 << 20);
    writer.u32(1 << 20);
    writer.u32(1 << 20);
    writer.u32(64);
    writer.u32(4);
    writer.u32(0);
}

fn parse_create_session(mut reader: XdrReader<'_>) -> [u8; 16] {
    parse_create_session_with_sequence(&mut reader, 1)
}

fn parse_create_session_with_sequence(reader: &mut XdrReader<'_>, sequence: u32) -> [u8; 16] {
    parse_compound(reader, &[OP_CREATE_SESSION]);
    let session: [u8; 16] = reader
        .fixed_opaque(16, "session id")
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(reader.u32("create session sequence").unwrap(), sequence);
    assert_eq!(reader.u32("create session flags").unwrap(), 0);
    for _ in 0..2 {
        let _ = reader.u32("headerpad").unwrap();
        let _ = reader.u32("max request").unwrap();
        let _ = reader.u32("max response").unwrap();
        let _ = reader.u32("max cached").unwrap();
        let _ = reader.u32("max operations").unwrap();
        let _ = reader.u32("max requests").unwrap();
        assert_eq!(reader.u32("rdma count").unwrap(), 0);
    }
    reader.end("create session response").unwrap();
    session
}

fn exchange_args_for(owner: &[u8]) -> Vec<u8> {
    op(OP_EXCHANGE_ID, |writer| {
        writer.fixed_opaque(b"v4-test!", 8);
        writer.var_opaque(owner);
        writer.u32(0);
        writer.u32(0);
        writer.u32(0);
    })
}

fn exchange_args() -> Vec<u8> {
    exchange_args_for(b"mount-rs-v4-wire")
}

fn create_session_args(clientid: u64) -> Vec<u8> {
    create_session_args_with_sequence(clientid, 1)
}

fn create_session_args_with_sequence(clientid: u64, sequence: u32) -> Vec<u8> {
    op(OP_CREATE_SESSION, |writer| {
        writer.u64(clientid);
        writer.u32(sequence);
        // Linux requests a callback channel during the normal v4.1 mount
        // handshake. The server must decline it in csr_flags, not reject the
        // otherwise valid CREATE_SESSION operation.
        writer.u32(CREATE_SESSION4_FLAG_CONN_BACK_CHAN);
        channel(writer);
        channel(writer);
        writer.u32(0);
        writer.u32(1);
        writer.u32(0);
    })
}

fn create_session_args_with_response_size(
    clientid: u64,
    sequence: u32,
    response_size: u32,
) -> Vec<u8> {
    op(OP_CREATE_SESSION, |writer| {
        writer.u64(clientid);
        writer.u32(sequence);
        writer.u32(CREATE_SESSION4_FLAG_CONN_BACK_CHAN);
        writer.u32(0);
        writer.u32(1 << 20);
        writer.u32(response_size);
        writer.u32(1 << 20);
        writer.u32(64);
        writer.u32(4);
        writer.u32(0);
        channel(writer);
        writer.u32(0);
        writer.u32(1);
        writer.u32(0);
    })
}

fn parse_channel_attrs(reader: &mut XdrReader<'_>, label: &str) -> [u32; 6] {
    let values = [
        reader.u32(&format!("{label} headerpad")).unwrap(),
        reader.u32(&format!("{label} max request")).unwrap(),
        reader.u32(&format!("{label} max response")).unwrap(),
        reader.u32(&format!("{label} max cached")).unwrap(),
        reader.u32(&format!("{label} max operations")).unwrap(),
        reader.u32(&format!("{label} max requests")).unwrap(),
    ];
    assert_eq!(reader.u32(&format!("{label} rdma count")).unwrap(), 0);
    values
}

fn empty_attrs(writer: &mut XdrWriter) {
    writer.u32(0);
    writer.u32(0);
}

fn parse_sequence_and_handle(mut reader: XdrReader<'_>) -> (Vec<u8>, Vec<u8>) {
    parse_compound_header(&mut reader, 3);
    parse_result_header(&mut reader, OP_SEQUENCE);
    let _ = reader.fixed_opaque(16, "sequence session id").unwrap();
    let _ = reader.u32("sequence id").unwrap();
    let _ = reader.u32("sequence slot").unwrap();
    let _ = reader.u32("sequence highest slot").unwrap();
    let _ = reader.u32("sequence target slot").unwrap();
    let _ = reader.u32("sequence status flags").unwrap();
    parse_result_header(&mut reader, OP_PUTROOTFH);
    parse_result_header(&mut reader, OP_GETFH);
    let handle = reader.var_opaque(128, "root handle").unwrap();
    reader.end("root handle response").unwrap();
    (handle, Vec::new())
}

async fn connect_v4_client(
    address: std::net::SocketAddr,
    xid: u32,
    owner: &[u8],
) -> (TcpStream, Client) {
    let mut stream = TcpStream::connect(address)
        .await
        .expect("connect concurrent NFS client");
    let clientid = parse_exchange(
        rpc(
            &mut stream,
            xid,
            compound("concurrent-exchange", &[exchange_args_for(owner)]),
        )
        .await,
    );
    let session = parse_create_session(
        rpc(
            &mut stream,
            xid + 1,
            compound(
                "concurrent-create-session",
                &[create_session_args(clientid)],
            ),
        )
        .await,
    );
    let mut client = Client {
        session,
        clientid,
        sequence: 1,
        slot: 0,
    };
    let mut response = rpc(
        &mut stream,
        xid + 2,
        compound(
            "concurrent-reclaim-complete",
            &[
                sequence(&client),
                op(OP_RECLAIM_COMPLETE, |writer| writer.bool(false)),
            ],
        ),
    )
    .await;
    parse_compound_header(&mut response, 2);
    consume_sequence_result(&mut response, "concurrent reclaim");
    parse_result_header(&mut response, OP_RECLAIM_COMPLETE);
    response.end("concurrent reclaim response").unwrap();
    client.sequence += 1;
    (stream, client)
}

async fn concurrent_file_round_trip(
    stream: &mut TcpStream,
    client: &mut Client,
    xid: &mut u32,
    owner: &[u8],
    name: &str,
    payload: &[u8],
) -> Vec<u8> {
    let open = op(OP_OPEN, |writer| {
        writer.u32(0);
        writer.u32(OPEN4_SHARE_ACCESS_BOTH);
        writer.u32(0);
        writer.u64(client.clientid);
        writer.var_opaque(owner);
        writer.u32(OPEN4_CREATE);
        writer.u32(UNCHECKED4);
        empty_attrs(writer);
        writer.u32(CLAIM_NULL);
        writer.string(name);
    });
    let mut response = rpc(
        stream,
        *xid,
        compound(
            "concurrent-open",
            &[
                sequence(client),
                op(OP_PUTROOTFH, |_| {}),
                open,
                op(OP_GETFH, |_| {}),
            ],
        ),
    )
    .await;
    *xid += 1;
    client.sequence += 1;
    parse_compound_header(&mut response, 4);
    consume_sequence_result(&mut response, "concurrent open");
    parse_result_header(&mut response, OP_PUTROOTFH);
    parse_result_header(&mut response, OP_OPEN);
    let stateid: [u8; 16] = response
        .fixed_opaque(16, "concurrent open stateid")
        .unwrap()
        .try_into()
        .unwrap();
    let _ = response.bool("concurrent open cinfo atomic").unwrap();
    let _ = response.u64("concurrent open cinfo before").unwrap();
    let _ = response.u64("concurrent open cinfo after").unwrap();
    let _ = response.u32("concurrent open rflags").unwrap();
    let _ = response.array(16, "concurrent open attrset", |reader| {
        reader.u32("attrset word")
    });
    assert_eq!(response.u32("concurrent open delegation").unwrap(), 0);
    parse_result_header(&mut response, OP_GETFH);
    let file_handle = response.var_opaque(128, "concurrent file handle").unwrap();
    response.end("concurrent open response").unwrap();

    let write = op(OP_WRITE, |writer| {
        writer.fixed_opaque(&stateid, 16);
        writer.u64(0);
        writer.u32(UNSTABLE4);
        writer.var_opaque(payload);
    });
    let mut response = rpc(
        stream,
        *xid,
        compound(
            "concurrent-write",
            &[
                sequence(client),
                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                write,
            ],
        ),
    )
    .await;
    *xid += 1;
    client.sequence += 1;
    parse_compound_header(&mut response, 3);
    consume_sequence_result(&mut response, "concurrent write");
    parse_result_header(&mut response, OP_PUTFH);
    parse_result_header(&mut response, OP_WRITE);
    assert_eq!(
        response.u32("concurrent write count").unwrap(),
        payload.len() as u32
    );
    assert_eq!(
        response.u32("concurrent write committed").unwrap(),
        UNSTABLE4
    );
    let _ = response
        .fixed_opaque(8, "concurrent write verifier")
        .unwrap();
    response.end("concurrent write response").unwrap();

    let read = op(OP_READ, |writer| {
        writer.fixed_opaque(&stateid, 16);
        writer.u64(0);
        writer.u32(128);
    });
    let mut response = rpc(
        stream,
        *xid,
        compound(
            "concurrent-read",
            &[
                sequence(client),
                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                read,
            ],
        ),
    )
    .await;
    *xid += 1;
    client.sequence += 1;
    parse_compound_header(&mut response, 3);
    consume_sequence_result(&mut response, "concurrent read");
    parse_result_header(&mut response, OP_PUTFH);
    parse_result_header(&mut response, OP_READ);
    let _ = response.bool("concurrent read eof").unwrap();
    let data = response.var_opaque(128, "concurrent read data").unwrap();
    response.end("concurrent read response").unwrap();
    data
}

#[test]
fn nfs_v3_and_v4_share_wire_handle_lifetime() {
    std::thread::Builder::new()
        .name("nfs-cross-version-handle-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
                    let address = server.listen().await.unwrap();
                    let mut v3_stream = TcpStream::connect(address).await.unwrap();
                    let mut mount = rpc_call(
                        &mut v3_stream,
                        100,
                        MOUNT_PROGRAM,
                        MOUNT_V3,
                        MOUNTPROC3_MNT,
                        encode_xdr(|writer| writer.string("/")),
                        None,
                    )
                    .await;
                    let mount_result = read_mount_res(&mut mount).unwrap();
                    mount.end("v3 MOUNT response").unwrap();
                    assert_eq!(mount_result.status, NFS3_OK);
                    let v3_root = mount_result.fh.unwrap();

                    let create = Create3args {
                        where_: DirOpArgs {
                            dir: v3_root.clone(),
                            name: "cross-version.txt".to_owned(),
                        },
                        mode: CREATE_UNCHECKED,
                        attributes: Some(Sattr3 {
                            mode: Some(0o644),
                            ..Sattr3::default()
                        }),
                        verf: None,
                    };
                    let mut created = rpc_call(
                        &mut v3_stream,
                        101,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_CREATE,
                        encode_xdr(|writer| write_create_args(writer, &create)),
                        None,
                    )
                    .await;
                    let create_result = read_create_res(&mut created).unwrap();
                    created.end("v3 CREATE response").unwrap();
                    assert_eq!(create_result.status, NFS3_OK);
                    let v3_file = create_result.obj.unwrap();

                    let (mut v4_stream, mut client) =
                        connect_v4_client(address, 200, b"cross-version-owner").await;
                    let mut handles = rpc(
                        &mut v4_stream,
                        203,
                        compound(
                            "shared-handles",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_GETFH, |_| {}),
                                op(OP_PUTFH, |writer| writer.var_opaque(&v3_root)),
                                op(OP_GETFH, |_| {}),
                                op(OP_LOOKUP, |writer| writer.string("cross-version.txt")),
                                op(OP_GETFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    client.sequence += 1;
                    parse_compound_header(&mut handles, 7);
                    consume_sequence_result(&mut handles, "cross-version");
                    parse_result_header(&mut handles, OP_PUTROOTFH);
                    parse_result_header(&mut handles, OP_GETFH);
                    let v4_root = handles.var_opaque(128, "v4 root handle").unwrap();
                    assert_eq!(v4_root, v3_root);
                    parse_result_header(&mut handles, OP_PUTFH);
                    parse_result_header(&mut handles, OP_GETFH);
                    assert_eq!(handles.var_opaque(128, "v3 root via v4").unwrap(), v3_root);
                    parse_result_header(&mut handles, OP_LOOKUP);
                    parse_result_header(&mut handles, OP_GETFH);
                    assert_eq!(handles.var_opaque(128, "v3 file via v4").unwrap(), v3_file);
                    handles.end("shared handles response").unwrap();

                    let mut removed = rpc(
                        &mut v4_stream,
                        204,
                        compound(
                            "cross-version-remove",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&v3_root)),
                                op(OP_REMOVE, |writer| writer.string("cross-version.txt")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut removed, 3);
                    consume_sequence_result(&mut removed, "cross-version remove");
                    parse_result_header(&mut removed, OP_PUTFH);
                    parse_result_header(&mut removed, OP_REMOVE);
                    let _ = removed.bool("remove atomic").unwrap();
                    let _ = removed.u64("remove before").unwrap();
                    let _ = removed.u64("remove after").unwrap();
                    removed.end("cross-version remove response").unwrap();
                    client.sequence += 1;

                    let mut old_handle = rpc_call(
                        &mut v3_stream,
                        102,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_GETATTR,
                        encode_xdr(|writer| writer.var_opaque(&v3_file)),
                        None,
                    )
                    .await;
                    assert_eq!(
                        read_getattr_res(&mut old_handle).unwrap().status,
                        NFS3ERR_STALE
                    );
                    old_handle.end("stale v3 handle response").unwrap();

                    let open = op(OP_OPEN, |writer| {
                        writer.u32(0);
                        writer.u32(OPEN4_SHARE_ACCESS_BOTH);
                        writer.u32(0);
                        writer.u64(client.clientid);
                        writer.var_opaque(b"cross-version-open-owner");
                        writer.u32(OPEN4_CREATE);
                        writer.u32(UNCHECKED4);
                        empty_attrs(writer);
                        writer.u32(CLAIM_NULL);
                        writer.string("v4-created.txt");
                    });
                    let mut opened = rpc(
                        &mut v4_stream,
                        205,
                        compound(
                            "v4-create-v3-lookup",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                open,
                                op(OP_GETFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    client.sequence += 1;
                    parse_compound_header(&mut opened, 4);
                    consume_sequence_result(&mut opened, "v4 create");
                    parse_result_header(&mut opened, OP_PUTROOTFH);
                    parse_result_header(&mut opened, OP_OPEN);
                    let v4_stateid: [u8; 16] = opened
                        .fixed_opaque(16, "v4 open stateid")
                        .unwrap()
                        .try_into()
                        .unwrap();
                    let _ = opened.bool("v4 open atomic").unwrap();
                    let _ = opened.u64("v4 open before").unwrap();
                    let _ = opened.u64("v4 open after").unwrap();
                    let _ = opened.u32("v4 open flags").unwrap();
                    let _ = opened
                        .array(16, "v4 open attrset", |reader| reader.u32("attr word"))
                        .unwrap();
                    assert_eq!(opened.u32("v4 open delegation").unwrap(), 0);
                    parse_result_header(&mut opened, OP_GETFH);
                    let v4_file = opened.var_opaque(128, "v4-created handle").unwrap();
                    opened.end("v4 create response").unwrap();

                    let mut looked_up = rpc_call(
                        &mut v3_stream,
                        103,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_LOOKUP,
                        encode_xdr(|writer| {
                            writer.var_opaque(&v3_root);
                            writer.string("v4-created.txt");
                        }),
                        None,
                    )
                    .await;
                    let lookup_result = read_lookup_res(&mut looked_up).unwrap();
                    looked_up.end("v3 LOOKUP response").unwrap();
                    assert_eq!(lookup_result.status, NFS3_OK);
                    assert_eq!(lookup_result.object.unwrap(), v4_file);

                    let payload = b"v4 open survives v3 unlink";
                    let mut written = rpc(
                        &mut v4_stream,
                        206,
                        compound(
                            "write-before-v3-unlink",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&v4_file)),
                                op(OP_WRITE, |writer| {
                                    writer.fixed_opaque(&v4_stateid, 16);
                                    writer.u64(0);
                                    writer.u32(UNSTABLE4);
                                    writer.var_opaque(payload);
                                }),
                            ],
                        ),
                    )
                    .await;
                    client.sequence += 1;
                    parse_compound_header(&mut written, 3);
                    consume_sequence_result(&mut written, "v4 write before unlink");
                    parse_result_header(&mut written, OP_PUTFH);
                    parse_result_header(&mut written, OP_WRITE);
                    assert_eq!(written.u32("v4 write count").unwrap(), payload.len() as u32);
                    let _ = written.u32("v4 write stability").unwrap();
                    let _ = written.fixed_opaque(8, "v4 write verifier").unwrap();
                    written.end("v4 write response").unwrap();

                    let mut unlinked = rpc_call(
                        &mut v3_stream,
                        104,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_REMOVE,
                        encode_xdr(|writer| {
                            writer.var_opaque(&v3_root);
                            writer.string("v4-created.txt");
                        }),
                        None,
                    )
                    .await;
                    assert_eq!(read_wcc_res(&mut unlinked).unwrap().status, NFS3_OK);
                    unlinked.end("v3 REMOVE response").unwrap();

                    let mut missing = rpc_call(
                        &mut v3_stream,
                        105,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_LOOKUP,
                        encode_xdr(|writer| {
                            writer.var_opaque(&v3_root);
                            writer.string("v4-created.txt");
                        }),
                        None,
                    )
                    .await;
                    assert_eq!(read_lookup_res(&mut missing).unwrap().status, NFS3ERR_NOENT);
                    missing.end("v3 removed-name LOOKUP response").unwrap();

                    let mut read = rpc(
                        &mut v4_stream,
                        207,
                        compound(
                            "read-after-v3-unlink",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&v4_file)),
                                op(OP_READ, |writer| {
                                    writer.fixed_opaque(&v4_stateid, 16);
                                    writer.u64(0);
                                    writer.u32(payload.len() as u32);
                                }),
                            ],
                        ),
                    )
                    .await;
                    client.sequence += 1;
                    parse_compound_header(&mut read, 3);
                    consume_sequence_result(&mut read, "v4 read after unlink");
                    parse_result_header(&mut read, OP_PUTFH);
                    parse_result_header(&mut read, OP_READ);
                    assert!(read.bool("v4 read eof").unwrap());
                    assert_eq!(read.var_opaque(128, "v4 read payload").unwrap(), payload);
                    read.end("v4 held-open read response").unwrap();

                    let test_stateid = op(OP_TEST_STATEID, |writer| {
                        writer.u32(1);
                        writer.fixed_opaque(&v4_stateid, 16);
                    });
                    let mut live_state = rpc(
                        &mut v4_stream,
                        208,
                        compound(
                            "test-open-after-v3-unlink",
                            &[sequence(&client), test_stateid.clone()],
                        ),
                    )
                    .await;
                    client.sequence += 1;
                    parse_compound_header(&mut live_state, 2);
                    consume_sequence_result(&mut live_state, "live unlinked state");
                    parse_result_header(&mut live_state, OP_TEST_STATEID);
                    assert_eq!(live_state.u32("live state count").unwrap(), 1);
                    assert_eq!(live_state.u32("live state status").unwrap(), 0);
                    live_state.end("live unlinked state response").unwrap();

                    let mut closed = rpc(
                        &mut v4_stream,
                        209,
                        compound(
                            "close-after-v3-unlink",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&v4_file)),
                                op(OP_CLOSE, |writer| {
                                    writer.u32(1);
                                    writer.fixed_opaque(&v4_stateid, 16);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut closed, 3);
                    consume_sequence_result(&mut closed, "v4 close after unlink");
                    parse_result_header(&mut closed, OP_PUTFH);
                    parse_result_header(&mut closed, OP_CLOSE);
                    let _ = closed.fixed_opaque(16, "v4 close stateid").unwrap();
                    closed.end("v4 close response").unwrap();
                    client.sequence += 1;

                    let mut retired_state = rpc(
                        &mut v4_stream,
                        210,
                        compound(
                            "test-closed-after-v3-unlink",
                            &[sequence(&client), test_stateid],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut retired_state, 2);
                    consume_sequence_result(&mut retired_state, "closed unlinked state");
                    parse_result_header(&mut retired_state, OP_TEST_STATEID);
                    assert_eq!(retired_state.u32("retired state count").unwrap(), 1);
                    assert_eq!(
                        retired_state.u32("retired state status").unwrap(),
                        NFS4ERR_BAD_STATEID
                    );
                    retired_state
                        .end("retired unlinked state response")
                        .unwrap();
                    server.close().await.unwrap();
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nfs_v4_open_racing_v3_unlink_keeps_the_shared_handle_pathless() {
    std::thread::Builder::new()
        .name("nfs-cross-version-open-unlink-race".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let inner = MemoryFs::empty();
                    let payload = b"opened before v3 unlink";
                    inner.write_file("/raced-open.txt", payload).await.unwrap();
                    let armed = Arc::new(AtomicBool::new(false));
                    let target_stats = Arc::new(AtomicU64::new(0));
                    let entered = Arc::new(Notify::new());
                    let release = Arc::new(Notify::new());
                    let server = NfsServer::new(
                        GateTargetStatDriver {
                            inner,
                            target_path: "/raced-open.txt",
                            block_on_stat: 1,
                            armed: Arc::clone(&armed),
                            target_stats: Arc::clone(&target_stats),
                            entered: Arc::clone(&entered),
                            release: Arc::clone(&release),
                        },
                        NfsServerOptions::default(),
                    );
                    let address = server.listen().await.unwrap();
                    let mut v3_stream = TcpStream::connect(address).await.unwrap();
                    let mut mount = rpc_call(
                        &mut v3_stream,
                        301,
                        MOUNT_PROGRAM,
                        MOUNT_V3,
                        MOUNTPROC3_MNT,
                        encode_xdr(|writer| writer.string("/")),
                        None,
                    )
                    .await;
                    let root = read_mount_res(&mut mount).unwrap().fh.unwrap();
                    mount.end("race MOUNT response").unwrap();
                    let mut lookup = rpc_call(
                        &mut v3_stream,
                        302,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_LOOKUP,
                        encode_xdr(|writer| {
                            writer.var_opaque(&root);
                            writer.string("raced-open.txt");
                        }),
                        None,
                    )
                    .await;
                    let original = read_lookup_res(&mut lookup).unwrap().object.unwrap();
                    lookup.end("race v3 LOOKUP response").unwrap();

                    let (mut v4_stream, client) =
                        connect_v4_client(address, 401, b"raced-open-owner").await;
                    armed.store(true, Ordering::Release);
                    let open_task = tokio::spawn(async move {
                        let opened = rpc(
                            &mut v4_stream,
                            404,
                            compound(
                                "raced-open",
                                &[
                                    sequence(&client),
                                    op(OP_PUTROOTFH, |_| {}),
                                    op(OP_OPEN, |writer| {
                                        writer.u32(0);
                                        writer.u32(OPEN4_SHARE_ACCESS_BOTH);
                                        writer.u32(0);
                                        writer.u64(client.clientid);
                                        writer.var_opaque(b"raced-open-state-owner");
                                        writer.u32(0);
                                        writer.u32(CLAIM_NULL);
                                        writer.string("raced-open.txt");
                                    }),
                                    op(OP_GETFH, |_| {}),
                                ],
                            ),
                        )
                        .await;
                        (v4_stream, client, opened)
                    });
                    timeout(Duration::from_millis(500), entered.notified())
                        .await
                        .expect("v4 OPEN reached its final post-open stat");

                    let mut remove_task = tokio::spawn(async move {
                        let mut removed = rpc_call(
                            &mut v3_stream,
                            303,
                            NFS_PROGRAM,
                            NFS_V3,
                            NFSPROC3_REMOVE,
                            encode_xdr(|writer| {
                                writer.var_opaque(&root);
                                writer.string("raced-open.txt");
                            }),
                            None,
                        )
                        .await;
                        let status = read_wcc_res(&mut removed).unwrap().status;
                        removed.end("raced v3 REMOVE response").unwrap();
                        let mut missing = rpc_call(
                            &mut v3_stream,
                            304,
                            NFS_PROGRAM,
                            NFS_V3,
                            NFSPROC3_LOOKUP,
                            encode_xdr(|writer| {
                                writer.var_opaque(&root);
                                writer.string("raced-open.txt");
                            }),
                            None,
                        )
                        .await;
                        assert_eq!(read_lookup_res(&mut missing).unwrap().status, NFS3ERR_NOENT);
                        missing.end("raced removed-name LOOKUP response").unwrap();
                        status
                    });
                    let completed_before_open =
                        timeout(Duration::from_millis(100), &mut remove_task)
                            .await
                            .is_ok();
                    release.notify_one();

                    let (mut v4_stream, mut client, mut opened) = open_task.await.unwrap();
                    parse_compound_header(&mut opened, 4);
                    consume_sequence_result(&mut opened, "raced open");
                    parse_result_header(&mut opened, OP_PUTROOTFH);
                    let stateid = consume_open_result(&mut opened, "raced open");
                    parse_result_header(&mut opened, OP_GETFH);
                    let current = opened.var_opaque(128, "raced open file handle").unwrap();
                    opened.end("raced open response").unwrap();
                    assert!(
                        !completed_before_open,
                        "v3 REMOVE completed while v4 OPEN was still binding its handle"
                    );
                    assert_eq!(remove_task.await.unwrap(), NFS3_OK);
                    assert_eq!(
                        current, original,
                        "a held OPEN must not rebind an unlinked name as a new file handle"
                    );

                    client.sequence += 1;
                    let mut read = rpc(
                        &mut v4_stream,
                        405,
                        compound(
                            "raced-open-read",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&current)),
                                op(OP_READ, |writer| {
                                    writer.fixed_opaque(&stateid, 16);
                                    writer.u64(0);
                                    writer.u32(payload.len() as u32);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut read, 3);
                    consume_sequence_result(&mut read, "raced open read");
                    parse_result_header(&mut read, OP_PUTFH);
                    parse_result_header(&mut read, OP_READ);
                    assert!(read.bool("raced open read eof").unwrap());
                    assert_eq!(
                        read.var_opaque(128, "raced open read data").unwrap(),
                        payload
                    );
                    read.end("raced open read response").unwrap();
                    server.close().await.unwrap();
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nfs_v4_open_survives_v3_rename_over_its_name() {
    struct HostRoot(PathBuf);

    impl Drop for HostRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.join("source.txt"));
            let _ = std::fs::remove_file(self.0.join("destination.txt"));
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    std::thread::Builder::new()
        .name("nfs-cross-version-rename-over-open".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let nonce = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("system clock after Unix epoch")
                        .as_nanos();
                    let host_root = (0..32)
                        .find_map(|attempt| {
                            let path = std::env::temp_dir().join(format!(
                                "mount-rs-nfs-rename-over-open-{}-{nonce}-{attempt}",
                                std::process::id()
                            ));
                            match std::fs::create_dir(&path) {
                                Ok(()) => Some(HostRoot(path)),
                                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                                    None
                                }
                                Err(error) => panic!("create host-backed NFS root: {error}"),
                            }
                        })
                        .expect("claim a unique host-backed NFS root");
                    let old_payload = b"held destination";
                    std::fs::write(host_root.0.join("destination.txt"), old_payload).unwrap();
                    std::fs::write(host_root.0.join("source.txt"), b"replacement source").unwrap();
                    let server =
                        NfsServer::new(HostFs::new(&host_root.0), NfsServerOptions::default());
                    let address = server.listen().await.unwrap();
                    let mut v3_stream = TcpStream::connect(address).await.unwrap();
                    let mut mount = rpc_call(
                        &mut v3_stream,
                        901,
                        MOUNT_PROGRAM,
                        MOUNT_V3,
                        MOUNTPROC3_MNT,
                        encode_xdr(|writer| writer.string("/")),
                        None,
                    )
                    .await;
                    let root = read_mount_res(&mut mount).unwrap().fh.unwrap();
                    mount.end("rename-over MOUNT response").unwrap();
                    let (mut v4_stream, mut client) =
                        connect_v4_client(address, 910, b"rename-over-open-owner").await;
                    let mut opened = rpc(
                        &mut v4_stream,
                        913,
                        compound(
                            "open-rename-destination",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_OPEN, |writer| {
                                    writer.u32(0);
                                    writer.u32(OPEN4_SHARE_ACCESS_BOTH);
                                    writer.u32(0);
                                    writer.u64(client.clientid);
                                    writer.var_opaque(b"rename-over-state-owner");
                                    writer.u32(0);
                                    writer.u32(CLAIM_NULL);
                                    writer.string("destination.txt");
                                }),
                                op(OP_GETFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    client.sequence += 1;
                    parse_compound_header(&mut opened, 4);
                    consume_sequence_result(&mut opened, "open rename destination");
                    parse_result_header(&mut opened, OP_PUTROOTFH);
                    let stateid = consume_open_result(&mut opened, "open rename destination");
                    parse_result_header(&mut opened, OP_GETFH);
                    let old_handle = opened.var_opaque(128, "held destination handle").unwrap();
                    opened.end("open rename destination response").unwrap();

                    let mut renamed = rpc_call(
                        &mut v3_stream,
                        902,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_RENAME,
                        encode_xdr(|writer| {
                            writer.var_opaque(&root);
                            writer.string("source.txt");
                            writer.var_opaque(&root);
                            writer.string("destination.txt");
                        }),
                        None,
                    )
                    .await;
                    assert_eq!(read_rename_res(&mut renamed).unwrap().status, NFS3_OK);
                    renamed.end("v3 rename-over response").unwrap();
                    assert_eq!(
                        std::fs::read(host_root.0.join("destination.txt")).unwrap(),
                        b"replacement source"
                    );
                    assert!(!host_root.0.join("source.txt").exists());
                    let mut replacement = rpc_call(
                        &mut v3_stream,
                        903,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_LOOKUP,
                        encode_xdr(|writer| {
                            writer.var_opaque(&root);
                            writer.string("destination.txt");
                        }),
                        None,
                    )
                    .await;
                    let new_handle = read_lookup_res(&mut replacement).unwrap().object.unwrap();
                    replacement.end("replacement LOOKUP response").unwrap();
                    assert_ne!(old_handle, new_handle);

                    let mut read = rpc(
                        &mut v4_stream,
                        914,
                        compound(
                            "read-renamed-over-open",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&old_handle)),
                                op(OP_READ, |writer| {
                                    writer.fixed_opaque(&stateid, 16);
                                    writer.u64(0);
                                    writer.u32(old_payload.len() as u32);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut read, 3);
                    consume_sequence_result(&mut read, "read replaced destination");
                    parse_result_header(&mut read, OP_PUTFH);
                    parse_result_header(&mut read, OP_READ);
                    assert!(read.bool("replaced destination eof").unwrap());
                    assert_eq!(
                        read.var_opaque(128, "replaced destination bytes").unwrap(),
                        old_payload
                    );
                    read.end("read replaced destination response").unwrap();

                    client.sequence += 1;
                    let mut closed = rpc(
                        &mut v4_stream,
                        915,
                        compound(
                            "close-replaced-destination",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&old_handle)),
                                op(OP_CLOSE, |writer| {
                                    writer.u32(1);
                                    writer.fixed_opaque(&stateid, 16);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut closed, 3);
                    consume_sequence_result(&mut closed, "close replaced destination");
                    parse_result_header(&mut closed, OP_PUTFH);
                    parse_result_header(&mut closed, OP_CLOSE);
                    let _ = closed.fixed_opaque(16, "close replaced stateid").unwrap();
                    closed.end("close replaced destination response").unwrap();

                    client.sequence += 1;
                    let mut retired = rpc(
                        &mut v4_stream,
                        916,
                        compound(
                            "retired-replaced-destination",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&old_handle)),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut retired, 2), NFS4ERR_STALE);
                    consume_sequence_result(&mut retired, "retired replaced destination");
                    assert_eq!(parse_result_status(&mut retired, OP_PUTFH), NFS4ERR_STALE);
                    retired
                        .end("retired replaced destination response")
                        .unwrap();

                    client.sequence += 1;
                    let mut current = rpc(
                        &mut v4_stream,
                        917,
                        compound(
                            "replacement-handle-live",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&new_handle)),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut current, 2);
                    consume_sequence_result(&mut current, "replacement handle live");
                    parse_result_header(&mut current, OP_PUTFH);
                    current.end("replacement handle live response").unwrap();
                    server.close().await.unwrap();
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nfs_v3_rename_same_inode_preserves_source_handle_after_alias_remove() {
    struct HostRoot(PathBuf);

    impl Drop for HostRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.join("source.txt"));
            let _ = std::fs::remove_file(self.0.join("alias.txt"));
            let _ = std::fs::remove_file(self.0.join("source-link"));
            let _ = std::fs::remove_file(self.0.join("alias-link"));
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    std::thread::Builder::new()
        .name("nfs-v3-hardlink-rename-noop".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let nonce = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("system clock after Unix epoch")
                        .as_nanos();
                    let host_root = (0..32)
                        .find_map(|attempt| {
                            let path = std::env::temp_dir().join(format!(
                                "mount-rs-nfs-hardlink-rename-{}-{nonce}-{attempt}",
                                std::process::id()
                            ));
                            match std::fs::create_dir(&path) {
                                Ok(()) => Some(HostRoot(path)),
                                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                                    None
                                }
                                Err(error) => panic!("create host-backed NFS root: {error}"),
                            }
                        })
                        .expect("claim a unique host-backed NFS root");
                    std::fs::write(host_root.0.join("source.txt"), b"same inode").unwrap();
                    std::fs::hard_link(
                        host_root.0.join("source.txt"),
                        host_root.0.join("alias.txt"),
                    )
                    .unwrap();
                    let rename_calls = Arc::new(AtomicU64::new(0));
                    let lstat_enabled = Arc::new(AtomicBool::new(true));
                    let server = NfsServer::new(
                        RenameSpyHostFs {
                            inner: HostFs::new(&host_root.0),
                            rename_calls: Arc::clone(&rename_calls),
                            lstat_enabled: Arc::clone(&lstat_enabled),
                        },
                        NfsServerOptions::default(),
                    );
                    let address = server.listen().await.unwrap();
                    let mut stream = TcpStream::connect(address).await.unwrap();
                    let mut mount = rpc_call(
                        &mut stream,
                        1001,
                        MOUNT_PROGRAM,
                        MOUNT_V3,
                        MOUNTPROC3_MNT,
                        encode_xdr(|writer| writer.string("/")),
                        None,
                    )
                    .await;
                    let root = read_mount_res(&mut mount).unwrap().fh.unwrap();
                    mount.end("hardlink MOUNT response").unwrap();
                    let mut lookup = rpc_call(
                        &mut stream,
                        1002,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_LOOKUP,
                        encode_xdr(|writer| {
                            writer.var_opaque(&root);
                            writer.string("source.txt");
                        }),
                        None,
                    )
                    .await;
                    let source = read_lookup_res(&mut lookup).unwrap().object.unwrap();
                    lookup.end("source LOOKUP response").unwrap();
                    let mut renamed = rpc_call(
                        &mut stream,
                        1003,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_RENAME,
                        encode_xdr(|writer| {
                            writer.var_opaque(&root);
                            writer.string("source.txt");
                            writer.var_opaque(&root);
                            writer.string("alias.txt");
                        }),
                        None,
                    )
                    .await;
                    assert_eq!(read_rename_res(&mut renamed).unwrap().status, NFS3_OK);
                    renamed.end("same-inode RENAME response").unwrap();
                    let mut removed = rpc_call(
                        &mut stream,
                        1004,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_REMOVE,
                        encode_xdr(|writer| {
                            writer.var_opaque(&root);
                            writer.string("alias.txt");
                        }),
                        None,
                    )
                    .await;
                    assert_eq!(read_wcc_res(&mut removed).unwrap().status, NFS3_OK);
                    removed.end("alias REMOVE response").unwrap();
                    let mut getattr = rpc_call(
                        &mut stream,
                        1005,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_GETATTR,
                        encode_xdr(|writer| writer.var_opaque(&source)),
                        None,
                    )
                    .await;
                    assert_eq!(read_getattr_res(&mut getattr).unwrap().status, NFS3_OK);
                    getattr.end("source GETATTR after alias REMOVE").unwrap();
                    let mut remaining = rpc_call(
                        &mut stream,
                        1006,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_LOOKUP,
                        encode_xdr(|writer| {
                            writer.var_opaque(&root);
                            writer.string("source.txt");
                        }),
                        None,
                    )
                    .await;
                    assert_eq!(
                        read_lookup_res(&mut remaining).unwrap().object.unwrap(),
                        source
                    );
                    remaining.end("source LOOKUP after alias REMOVE").unwrap();
                    assert_eq!(
                        std::fs::read(host_root.0.join("source.txt")).unwrap(),
                        b"same inode"
                    );
                    assert!(!host_root.0.join("alias.txt").exists());
                    assert_eq!(rename_calls.load(Ordering::Acquire), 0);
                    let mut missing_rename = rpc_call(
                        &mut stream,
                        1007,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_RENAME,
                        encode_xdr(|writer| {
                            writer.var_opaque(&root);
                            writer.string("missing.txt");
                            writer.var_opaque(&root);
                            writer.string("missing.txt");
                        }),
                        None,
                    )
                    .await;
                    assert_eq!(
                        read_rename_res(&mut missing_rename).unwrap().status,
                        NFS3ERR_NOENT
                    );
                    missing_rename
                        .end("missing same-name RENAME response")
                        .unwrap();
                    assert_eq!(rename_calls.load(Ordering::Acquire), 1);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::{MetadataExt, symlink};

                        symlink("source.txt", host_root.0.join("source-link")).unwrap();
                        symlink("source.txt", host_root.0.join("alias-link")).unwrap();
                        assert_eq!(
                            std::fs::metadata(host_root.0.join("source-link"))
                                .unwrap()
                                .ino(),
                            std::fs::metadata(host_root.0.join("alias-link"))
                                .unwrap()
                                .ino()
                        );
                        lstat_enabled.store(false, Ordering::Release);
                        let mut symlink_rename = rpc_call(
                            &mut stream,
                            1008,
                            NFS_PROGRAM,
                            NFS_V3,
                            NFSPROC3_RENAME,
                            encode_xdr(|writer| {
                                writer.var_opaque(&root);
                                writer.string("source-link");
                                writer.var_opaque(&root);
                                writer.string("alias-link");
                            }),
                            None,
                        )
                        .await;
                        assert_eq!(
                            read_rename_res(&mut symlink_rename).unwrap().status,
                            NFS3_OK
                        );
                        symlink_rename.end("symlink RENAME response").unwrap();
                        assert_eq!(rename_calls.load(Ordering::Acquire), 2);
                        assert!(!host_root.0.join("source-link").exists());
                        assert_eq!(
                            std::fs::read_link(host_root.0.join("alias-link")).unwrap(),
                            PathBuf::from("source.txt")
                        );
                    }
                    server.close().await.unwrap();
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nfs_v4_rename_same_inode_preserves_source_handle_after_alias_remove() {
    std::thread::Builder::new()
        .name("nfs-v4-hardlink-rename-noop".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let inner = MemoryFs::empty();
                    inner
                        .write_file("/source.txt", b"same inode")
                        .await
                        .unwrap();
                    inner.link("/source.txt", "/alias.txt").await.unwrap();
                    let server = NfsServer::new(inner, NfsServerOptions::default());
                    let address = server.listen().await.unwrap();
                    let (mut stream, mut client) =
                        connect_v4_client(address, 1101, b"same-inode-rename-owner").await;
                    let mut source_reply = rpc(
                        &mut stream,
                        1104,
                        compound(
                            "source-before-same-inode-rename",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_LOOKUP, |writer| writer.string("source.txt")),
                                op(OP_GETFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut source_reply, 4);
                    consume_sequence_result(&mut source_reply, "source before rename");
                    parse_result_header(&mut source_reply, OP_PUTROOTFH);
                    parse_result_header(&mut source_reply, OP_LOOKUP);
                    parse_result_header(&mut source_reply, OP_GETFH);
                    let source = source_reply
                        .var_opaque(128, "original source handle")
                        .unwrap();
                    source_reply.end("source before rename response").unwrap();

                    client.sequence += 1;
                    let mut renamed = rpc(
                        &mut stream,
                        1105,
                        compound(
                            "same-inode-v4-rename",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_SAVEFH, |_| {}),
                                op(OP_RENAME, |writer| {
                                    writer.string("source.txt");
                                    writer.string("alias.txt");
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut renamed, 4);
                    consume_sequence_result(&mut renamed, "same-inode v4 rename");
                    parse_result_header(&mut renamed, OP_PUTROOTFH);
                    parse_result_header(&mut renamed, OP_SAVEFH);
                    parse_result_header(&mut renamed, OP_RENAME);
                    for _ in 0..2 {
                        let _ = renamed.bool("rename change atomic").unwrap();
                        let _ = renamed.u64("rename change before").unwrap();
                        let _ = renamed.u64("rename change after").unwrap();
                    }
                    renamed.end("same-inode v4 rename response").unwrap();

                    client.sequence += 1;
                    let mut removed = rpc(
                        &mut stream,
                        1106,
                        compound(
                            "remove-hardlink-alias",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("alias.txt")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut removed, 3);
                    consume_sequence_result(&mut removed, "remove hardlink alias");
                    parse_result_header(&mut removed, OP_PUTROOTFH);
                    parse_result_header(&mut removed, OP_REMOVE);
                    let _ = removed.bool("remove change atomic").unwrap();
                    let _ = removed.u64("remove change before").unwrap();
                    let _ = removed.u64("remove change after").unwrap();
                    removed.end("remove hardlink alias response").unwrap();

                    client.sequence += 1;
                    let mut remaining = rpc(
                        &mut stream,
                        1107,
                        compound(
                            "source-after-alias-remove",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&source)),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_LOOKUP, |writer| writer.string("source.txt")),
                                op(OP_GETFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut remaining, 5);
                    consume_sequence_result(&mut remaining, "source after alias removal");
                    parse_result_header(&mut remaining, OP_PUTFH);
                    parse_result_header(&mut remaining, OP_PUTROOTFH);
                    parse_result_header(&mut remaining, OP_LOOKUP);
                    parse_result_header(&mut remaining, OP_GETFH);
                    assert_eq!(
                        remaining
                            .var_opaque(128, "surviving source handle")
                            .unwrap(),
                        source
                    );
                    remaining
                        .end("source after alias removal response")
                        .unwrap();
                    server.close().await.unwrap();
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nfs_v4_remove_waits_for_v3_lookup_handle_binding() {
    std::thread::Builder::new()
        .name("nfs-v4-remove-v3-lookup-race".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let inner = MemoryFs::empty();
                    inner
                        .write_file("/raced-lookup.txt", b"lookup-before-remove")
                        .await
                        .unwrap();
                    let armed = Arc::new(AtomicBool::new(false));
                    let target_stats = Arc::new(AtomicU64::new(0));
                    let entered = Arc::new(Notify::new());
                    let release = Arc::new(Notify::new());
                    let server = NfsServer::new(
                        GateTargetStatDriver {
                            inner,
                            target_path: "/raced-lookup.txt",
                            block_on_stat: 0,
                            armed: Arc::clone(&armed),
                            target_stats: Arc::clone(&target_stats),
                            entered: Arc::clone(&entered),
                            release: Arc::clone(&release),
                        },
                        NfsServerOptions::default(),
                    );
                    let address = server.listen().await.unwrap();
                    let mut v3_stream = TcpStream::connect(address).await.unwrap();
                    let mut mount = rpc_call(
                        &mut v3_stream,
                        701,
                        MOUNT_PROGRAM,
                        MOUNT_V3,
                        MOUNTPROC3_MNT,
                        encode_xdr(|writer| writer.string("/")),
                        None,
                    )
                    .await;
                    let root = read_mount_res(&mut mount).unwrap().fh.unwrap();
                    mount.end("v3 lookup race MOUNT response").unwrap();
                    let (mut v4_stream, client) =
                        connect_v4_client(address, 801, b"v4-remove-race-owner").await;
                    armed.store(true, Ordering::Release);
                    let lookup_root = root.clone();
                    let lookup_task = tokio::spawn(async move {
                        let mut lookup = rpc_call(
                            &mut v3_stream,
                            702,
                            NFS_PROGRAM,
                            NFS_V3,
                            NFSPROC3_LOOKUP,
                            encode_xdr(|writer| {
                                writer.var_opaque(&lookup_root);
                                writer.string("raced-lookup.txt");
                            }),
                            None,
                        )
                        .await;
                        let result = read_lookup_res(&mut lookup).unwrap();
                        lookup.end("raced v3 LOOKUP response").unwrap();
                        (v3_stream, result)
                    });
                    timeout(Duration::from_millis(500), entered.notified())
                        .await
                        .expect("v3 LOOKUP reached its backend stat");
                    let mut remove_task = tokio::spawn(async move {
                        rpc(
                            &mut v4_stream,
                            804,
                            compound(
                                "v4-remove-during-v3-lookup",
                                &[
                                    sequence(&client),
                                    op(OP_PUTROOTFH, |_| {}),
                                    op(OP_REMOVE, |writer| writer.string("raced-lookup.txt")),
                                ],
                            ),
                        )
                        .await
                    });
                    let completed_before_lookup =
                        timeout(Duration::from_millis(100), &mut remove_task)
                            .await
                            .is_ok();
                    release.notify_one();
                    let (mut v3_stream, lookup_result) = lookup_task.await.unwrap();
                    assert_eq!(lookup_result.status, NFS3_OK);
                    let old_handle = lookup_result.object.unwrap();
                    assert!(
                        !completed_before_lookup,
                        "v4 REMOVE completed before v3 LOOKUP bound its observed inode"
                    );
                    let mut removed = remove_task.await.unwrap();
                    parse_compound_header(&mut removed, 3);
                    consume_sequence_result(&mut removed, "v4 remove after lookup");
                    parse_result_header(&mut removed, OP_PUTROOTFH);
                    parse_result_header(&mut removed, OP_REMOVE);
                    let _ = removed.bool("v4 remove change atomic").unwrap();
                    let _ = removed.u64("v4 remove change before").unwrap();
                    let _ = removed.u64("v4 remove change after").unwrap();
                    removed.end("v4 remove after lookup response").unwrap();

                    let mut stale = rpc_call(
                        &mut v3_stream,
                        703,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_GETATTR,
                        encode_xdr(|writer| writer.var_opaque(&old_handle)),
                        None,
                    )
                    .await;
                    assert_eq!(read_getattr_res(&mut stale).unwrap().status, NFS3ERR_STALE);
                    stale.end("v3 stale handle after v4 REMOVE").unwrap();
                    let mut missing = rpc_call(
                        &mut v3_stream,
                        704,
                        NFS_PROGRAM,
                        NFS_V3,
                        NFSPROC3_LOOKUP,
                        encode_xdr(|writer| {
                            writer.var_opaque(&root);
                            writer.string("raced-lookup.txt");
                        }),
                        None,
                    )
                    .await;
                    assert_eq!(read_lookup_res(&mut missing).unwrap().status, NFS3ERR_NOENT);
                    missing
                        .end("v3 removed-name LOOKUP after v4 REMOVE")
                        .unwrap();
                    server.close().await.unwrap();
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nfs_v4_1_tcp_session_and_file_round_trip_is_rootless() {
    std::thread::Builder::new()
        .name("nfs-v4-wire-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 wire test runtime")
                .block_on(async {
                    let mut options = NfsServerOptions::default();
                    const SEED: u32 = 0x1020_3040;
                    options.session.nfs4.seed = SEED;
                    options.session.nfs4.max_locks_per_file = 1;
                    options.session.nfs4.idmap = Some(
                        Nfs4IdMap::new(Some("example.test".to_owned()))
                            .with_user("root", 0)
                            .with_group("root", 0),
                    );
                    let server = NfsServer::new(MemoryFs::empty(), options);
                    let address = server.listen().await.expect("listen rootless NFS server");
                    let mut stream = TcpStream::connect(address)
                        .await
                        .expect("connect NFS server");

                    let clientid = parse_exchange(
                        rpc(&mut stream, 1, compound("exchange", &[exchange_args()])).await,
                    );
                    let session = parse_create_session(
                        rpc(
                            &mut stream,
                            2,
                            compound("create-session", &[create_session_args(clientid)]),
                        )
                        .await,
                    );
                    assert_eq!(clientid >> 32, u64::from(SEED));
                    assert_eq!(u32::from_be_bytes(session[..4].try_into().unwrap()), SEED);
                    let mut client = Client {
                        session,
                        clientid,
                        sequence: 1,
                        slot: 0,
                    };
                    let (_root_handle, _) = parse_sequence_and_handle(
                        rpc(
                            &mut stream,
                            3,
                            compound(
                                "root",
                                &[
                                    sequence(&client),
                                    op(OP_PUTROOTFH, |_| {}),
                                    op(OP_GETFH, |_| {}),
                                ],
                            ),
                        )
                        .await,
                    );

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        4,
                        compound(
                            "getattr",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_GETATTR, |writer| {
                                    writer.u32(2);
                                    writer.u32((1 << 1) | (1 << 10));
                                    writer.u32((1 << 4) | (1 << 5) | (1 << 9) | (1 << 23));
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    parse_result_header(&mut response, OP_SEQUENCE);
                    let _ = response.fixed_opaque(16, "getattr sequence id").unwrap();
                    let _ = response.u32("getattr sequence number").unwrap();
                    let _ = response.u32("getattr sequence slot").unwrap();
                    let _ = response.u32("getattr highest slot").unwrap();
                    let _ = response.u32("getattr target slot").unwrap();
                    let _ = response.u32("getattr status flags").unwrap();
                    parse_result_header(&mut response, OP_PUTROOTFH);
                    parse_result_header(&mut response, OP_GETATTR);
                    assert_eq!(
                        response
                            .array(16, "getattr mask", |reader| reader.u32("mask word"))
                            .unwrap(),
                        vec![(1 << 1) | (1 << 10), (1 << 4) | (1 << 5) | (1 << 9)]
                    );
                    let values = response.var_opaque(128, "getattr values").unwrap();
                    let mut values = XdrReader::new(&values);
                    assert_eq!(values.u32("fh expire type").unwrap(), 2);
                    assert_eq!(values.u32("lease value").unwrap(), 90);
                    assert_eq!(
                        values.string(128, "owner value").unwrap(),
                        "root@example.test"
                    );
                    assert_eq!(
                        values.string(128, "owner group value").unwrap(),
                        "root@example.test"
                    );
                    let _ = values.u32("rawdev major").unwrap();
                    let _ = values.u32("rawdev minor").unwrap();
                    values.end("getattr values").unwrap();
                    response.end("getattr response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        5,
                        compound(
                            "backchannel",
                            &[
                                sequence(&client),
                                op(OP_BACKCHANNEL_CTL, |writer| {
                                    writer.u32(0);
                                    writer.u32(1);
                                    writer.u32(0);
                                }),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 2), 22);
                    parse_result_header(&mut response, OP_SEQUENCE);
                    let _ = response
                        .fixed_opaque(16, "backchannel sequence id")
                        .unwrap();
                    let _ = response.u32("backchannel sequence number").unwrap();
                    let _ = response.u32("backchannel sequence slot").unwrap();
                    let _ = response.u32("backchannel highest slot").unwrap();
                    let _ = response.u32("backchannel target slot").unwrap();
                    let _ = response.u32("backchannel status flags").unwrap();
                    assert_eq!(
                        parse_result_status(&mut response, OP_BACKCHANNEL_CTL),
                        22,
                        "a server without a callback channel returns NFS4ERR_INVAL"
                    );
                    response.end("backchannel response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        60,
                        compound(
                            "open-before-reclaim",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_OPEN, |writer| {
                                    writer.u32(0);
                                    writer.u32(3);
                                    writer.u32(0);
                                    writer.u64(client.clientid);
                                    writer.var_opaque(b"open-before-reclaim");
                                    writer.u32(1);
                                    writer.u32(0);
                                    empty_attrs(writer);
                                    writer.u32(0);
                                    writer.string("wire-file");
                                }),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 3), NFS4ERR_GRACE);
                    consume_sequence_result(&mut response, "open before reclaim");
                    parse_result_header(&mut response, OP_PUTROOTFH);
                    assert_eq!(parse_result_status(&mut response, OP_OPEN), NFS4ERR_GRACE);
                    response.end("open before reclaim response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        61,
                        compound(
                            "lock-before-reclaim",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_LOCK, |writer| {
                                    writer.u32(1);
                                    writer.bool(false);
                                    writer.u64(0);
                                    writer.u64(4);
                                    writer.bool(true);
                                    writer.u32(0);
                                    writer.fixed_opaque(&[0; 16], 16);
                                    writer.u32(0);
                                    writer.u64(client.clientid);
                                    writer.var_opaque(b"lock-before-reclaim");
                                }),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 3), NFS4ERR_GRACE);
                    consume_sequence_result(&mut response, "lock before reclaim");
                    parse_result_header(&mut response, OP_PUTROOTFH);
                    assert_eq!(parse_result_status(&mut response, OP_LOCK), NFS4ERR_GRACE);
                    response.end("lock before reclaim response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        6,
                        compound(
                            "reclaim-complete",
                            &[
                                sequence(&client),
                                op(OP_RECLAIM_COMPLETE, |writer| writer.bool(false)),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 2);
                    consume_sequence_result(&mut response, "reclaim");
                    parse_result_header(&mut response, OP_RECLAIM_COMPLETE);
                    response.end("reclaim response").unwrap();

                    client.sequence += 1;
                    let open = op(OP_OPEN, |writer| {
                        // OPEN's owner seqid is independent from the returned
                        // stateid seqid. Linux starts a new owner at zero,
                        // while RFC 8881 requires the first stateid seqid to
                        // be one.
                        writer.u32(0);
                        writer.u32(3);
                        writer.u32(0);
                        writer.u64(client.clientid);
                        writer.var_opaque(b"open-owner");
                        writer.u32(1);
                        writer.u32(0);
                        empty_attrs(writer);
                        writer.u32(0);
                        writer.string("wire-file");
                    });
                    let mut response = rpc(
                        &mut stream,
                        7,
                        compound(
                            "open",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                open,
                                op(OP_GETFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 4);
                    parse_result_header(&mut response, OP_SEQUENCE);
                    let _ = response.fixed_opaque(16, "open sequence id").unwrap();
                    let _ = response.u32("open sequence number").unwrap();
                    let _ = response.u32("open sequence slot").unwrap();
                    let _ = response.u32("open highest slot").unwrap();
                    let _ = response.u32("open target slot").unwrap();
                    let _ = response.u32("open status flags").unwrap();
                    parse_result_header(&mut response, OP_PUTROOTFH);
                    parse_result_header(&mut response, OP_OPEN);
                    let stateid = response.fixed_opaque(16, "open stateid").unwrap();
                    assert_eq!(
                        u32::from_be_bytes(stateid[..4].try_into().unwrap()),
                        1,
                        "a newly created OPEN stateid starts at seqid one"
                    );
                    let _ = response.bool("open cinfo atomic").unwrap();
                    let _ = response.u64("open cinfo before").unwrap();
                    let _ = response.u64("open cinfo after").unwrap();
                    let _ = response.u32("open rflags").unwrap();
                    let _ = response.u32("open attrset words").unwrap();
                    assert_eq!(response.u32("open delegation").unwrap(), 0);
                    parse_result_header(&mut response, OP_GETFH);
                    let file_handle = response.var_opaque(128, "file handle").unwrap();
                    response.end("open response").unwrap();

                    client.sequence += 1;
                    let write = op(OP_WRITE, |writer| {
                        writer.fixed_opaque(&stateid, 16);
                        writer.u64(0);
                        writer.u32(UNSTABLE4);
                        writer.var_opaque(b"nfs v4.1 wire\n");
                    });
                    let mut response = rpc(
                        &mut stream,
                        6,
                        compound(
                            "write",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                write,
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    parse_result_header(&mut response, OP_SEQUENCE);
                    let _ = response.fixed_opaque(16, "write sequence id").unwrap();
                    let _ = response.u32("write sequence number").unwrap();
                    let _ = response.u32("write sequence slot").unwrap();
                    let _ = response.u32("write highest slot").unwrap();
                    let _ = response.u32("write target slot").unwrap();
                    let _ = response.u32("write status flags").unwrap();
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_WRITE);
                    assert_eq!(response.u32("write count").unwrap(), 14);
                    assert_eq!(response.u32("write committed").unwrap(), UNSTABLE4);
                    let _ = response.fixed_opaque(8, "write verifier").unwrap();
                    response.end("write response").unwrap();

                    // UNSTABLE4 is deliberately followed by COMMIT with a
                    // non-zero range. This exercises both COMMIT argument
                    // consumption and the FileHandle::sync barrier.
                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        18,
                        compound(
                            "commit",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_COMMIT, |writer| {
                                    writer.u64(4);
                                    writer.u32(10);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "commit");
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_COMMIT);
                    let _ = response.fixed_opaque(8, "commit verifier").unwrap();
                    response.end("commit response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        7,
                        compound(
                            "open-downgrade",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_OPEN_DOWNGRADE, |writer| {
                                    writer.fixed_opaque(&stateid, 16);
                                    writer.u32(0);
                                    writer.u32(1);
                                    writer.u32(0);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    parse_result_header(&mut response, OP_SEQUENCE);
                    let _ = response.fixed_opaque(16, "downgrade sequence id").unwrap();
                    let _ = response.u32("downgrade sequence number").unwrap();
                    let _ = response.u32("downgrade sequence slot").unwrap();
                    let _ = response.u32("downgrade highest slot").unwrap();
                    let _ = response.u32("downgrade target slot").unwrap();
                    let _ = response.u32("downgrade status flags").unwrap();
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_OPEN_DOWNGRADE);
                    let downgraded_stateid =
                        response.fixed_opaque(16, "downgraded stateid").unwrap();
                    response.end("open downgrade response").unwrap();

                    client.sequence += 1;
                    let read = op(OP_READ, |writer| {
                        writer.fixed_opaque(&downgraded_stateid, 16);
                        writer.u64(0);
                        writer.u32(64);
                    });
                    let mut response = rpc(
                        &mut stream,
                        8,
                        compound(
                            "read",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                read,
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    parse_result_header(&mut response, OP_SEQUENCE);
                    let _ = response.fixed_opaque(16, "read sequence id").unwrap();
                    let _ = response.u32("read sequence number").unwrap();
                    let _ = response.u32("read sequence slot").unwrap();
                    let _ = response.u32("read highest slot").unwrap();
                    let _ = response.u32("read target slot").unwrap();
                    let _ = response.u32("read status flags").unwrap();
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_READ);
                    let _ = response.bool("read eof").unwrap();
                    assert_eq!(
                        response.var_opaque(1024, "read data").unwrap(),
                        b"nfs v4.1 wire\n"
                    );
                    response.end("read response").unwrap();

                    client.sequence += 1;
                    let lock_one = op(OP_LOCK, |writer| {
                        writer.u32(1);
                        writer.bool(false);
                        writer.u64(0);
                        writer.u64(4);
                        writer.bool(true);
                        writer.u32(0);
                        writer.fixed_opaque(&downgraded_stateid, 16);
                        writer.u32(0);
                        writer.u64(client.clientid);
                        writer.var_opaque(b"lock-owner-1");
                    });
                    let mut response = rpc(
                        &mut stream,
                        10,
                        compound(
                            "lock",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                lock_one,
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "lock");
                    parse_result_header(&mut response, OP_PUTFH);
                    assert_eq!(parse_result_status(&mut response, OP_LOCK), 0);
                    let lock_stateid = response.fixed_opaque(16, "lock stateid").unwrap();
                    response.end("lock response").unwrap();

                    // maxLocksPerFile applies to every granted range, even
                    // when an existing lock stateid is being extended.
                    client.sequence += 1;
                    let lock_limit = op(OP_LOCK, |writer| {
                        writer.u32(1);
                        writer.bool(false);
                        writer.u64(8);
                        writer.u64(4);
                        writer.bool(false);
                        writer.fixed_opaque(&lock_stateid, 16);
                        writer.u32(0);
                    });
                    let mut response = rpc(
                        &mut stream,
                        11,
                        compound(
                            "lock-limit",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                lock_limit,
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 3), NFS4ERR_RESOURCE);
                    consume_sequence_result(&mut response, "lock-limit");
                    parse_result_header(&mut response, OP_PUTFH);
                    assert_eq!(
                        parse_result_status(&mut response, OP_LOCK),
                        NFS4ERR_RESOURCE
                    );
                    response.end("lock limit response").unwrap();

                    client.sequence += 1;
                    let open_two = op(OP_OPEN, |writer| {
                        writer.u32(1);
                        writer.u32(2);
                        writer.u32(0);
                        writer.u64(client.clientid);
                        writer.var_opaque(b"open-owner-2");
                        writer.u32(0);
                        writer.u32(4);
                    });
                    let mut response = rpc(
                        &mut stream,
                        12,
                        compound(
                            "open-two",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                open_two,
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "open-two");
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_OPEN);
                    let open_two_stateid =
                        response.fixed_opaque(16, "second open stateid").unwrap();
                    let _ = response.bool("second open cinfo atomic").unwrap();
                    let _ = response.u64("second open cinfo before").unwrap();
                    let _ = response.u64("second open cinfo after").unwrap();
                    let _ = response.u32("second open rflags").unwrap();
                    let _ = response.array(16, "second open attrset", |reader| {
                        reader.u32("attrset word")
                    });
                    assert_eq!(response.u32("second open delegation").unwrap(), 0);
                    response.end("second open response").unwrap();

                    client.sequence += 1;
                    let lock_two = op(OP_LOCK, |writer| {
                        writer.u32(2);
                        writer.bool(false);
                        writer.u64(0);
                        writer.u64(4);
                        writer.bool(true);
                        writer.u32(0);
                        writer.fixed_opaque(&open_two_stateid, 16);
                        writer.u32(0);
                        writer.u64(client.clientid);
                        writer.var_opaque(b"lock-owner-2");
                    });
                    let mut response = rpc(
                        &mut stream,
                        13,
                        compound(
                            "lock-denied",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                lock_two,
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 3), 10_010);
                    consume_sequence_result(&mut response, "lock-denied");
                    parse_result_header(&mut response, OP_PUTFH);
                    assert_eq!(parse_result_status(&mut response, OP_LOCK), 10_010);
                    assert_eq!(response.u64("denied offset").unwrap(), 0);
                    assert_eq!(response.u64("denied length").unwrap(), 4);
                    assert_eq!(response.u32("denied type").unwrap(), 1);
                    assert_eq!(response.u64("denied clientid").unwrap(), client.clientid);
                    assert_eq!(
                        response.var_opaque(1024, "denied owner").unwrap(),
                        b"lock-owner-1"
                    );
                    response.end("lock denied response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        14,
                        compound(
                            "unlock",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_LOCKU, |writer| {
                                    writer.u32(1);
                                    writer.u32(0);
                                    writer.fixed_opaque(&lock_stateid, 16);
                                    writer.u64(0);
                                    writer.u64(4);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "unlock");
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_LOCKU);
                    let unlocked_stateid = response.fixed_opaque(16, "unlocked stateid").unwrap();
                    response.end("unlock response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        15,
                        compound(
                            "free-lock-state",
                            &[
                                sequence(&client),
                                op(OP_FREE_STATEID, |writer| {
                                    writer.fixed_opaque(&unlocked_stateid, 16)
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 2);
                    consume_sequence_result(&mut response, "free-lock-state");
                    parse_result_header(&mut response, OP_FREE_STATEID);
                    response.end("free lock state response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        16,
                        compound(
                            "close-two",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_CLOSE, |writer| {
                                    writer.u32(1);
                                    writer.fixed_opaque(&open_two_stateid, 16);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "close-two");
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_CLOSE);
                    let _ = response.fixed_opaque(16, "close-two stateid").unwrap();
                    response.end("close-two response").unwrap();

                    client.sequence += 1;
                    let close = op(OP_CLOSE, |writer| {
                        writer.u32(1);
                        writer.fixed_opaque(&downgraded_stateid, 16);
                    });
                    let mut response = rpc(
                        &mut stream,
                        17,
                        compound(
                            "close",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                close,
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    parse_result_header(&mut response, OP_SEQUENCE);
                    let _ = response.fixed_opaque(16, "close sequence id").unwrap();
                    let _ = response.u32("close sequence number").unwrap();
                    let _ = response.u32("close sequence slot").unwrap();
                    let _ = response.u32("close highest slot").unwrap();
                    let _ = response.u32("close target slot").unwrap();
                    let _ = response.u32("close status flags").unwrap();
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_CLOSE);
                    let _ = response.fixed_opaque(16, "close stateid").unwrap();
                    response.end("close response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        17,
                        compound(
                            "remove",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("wire-file")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    parse_result_header(&mut response, OP_SEQUENCE);
                    let _ = response.fixed_opaque(16, "remove sequence id").unwrap();
                    let _ = response.u32("remove sequence number").unwrap();
                    let _ = response.u32("remove sequence slot").unwrap();
                    let _ = response.u32("remove highest slot").unwrap();
                    let _ = response.u32("remove target slot").unwrap();
                    let _ = response.u32("remove status flags").unwrap();
                    parse_result_header(&mut response, OP_PUTROOTFH);
                    parse_result_header(&mut response, OP_REMOVE);
                    let _ = response.bool("remove cinfo atomic").unwrap();
                    let _ = response.u64("remove cinfo before").unwrap();
                    let _ = response.u64("remove cinfo after").unwrap();
                    response.end("remove response").unwrap();
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn v4 wire test thread")
        .join()
        .expect("v4 wire test thread panicked");
}

#[test]
fn nfs_v4_anonymous_access_does_not_inherit_root_mode() {
    std::thread::Builder::new()
        .name("nfs-v4-anonymous-access-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 anonymous access test runtime")
                .block_on(async {
                    let driver = MemoryFs::new(MemoryOptions {
                        root_mode: 0o700,
                        ..MemoryOptions::default()
                    });
                    let server = NfsServer::new(driver, NfsServerOptions::default());
                    let address = server.listen().await.expect("listen NFS server");
                    let (mut stream, mut client) =
                        connect_v4_client(address, 700, b"anonymous-access-client").await;

                    for (credential, expected_access) in [
                        (None, 0),
                        (Some(auth_sys(1000, 1000, "other-user")), 0),
                        (
                            Some(auth_sys(0, 0, "root-user")),
                            ACCESS4_READ
                                | ACCESS4_LOOKUP
                                | ACCESS4_MODIFY
                                | ACCESS4_EXTEND
                                | ACCESS4_DELETE,
                        ),
                    ] {
                        let mut response = rpc_with_credential(
                            &mut stream,
                            701 + client.sequence,
                            compound(
                                "root-access",
                                &[
                                    sequence(&client),
                                    op(OP_PUTROOTFH, |_| {}),
                                    op(OP_ACCESS, |writer| writer.u32(ACCESS4_ALL)),
                                ],
                            ),
                            credential.as_ref(),
                        )
                        .await;
                        parse_compound_header(&mut response, 3);
                        consume_sequence_result(&mut response, "access sequence");
                        parse_result_header(&mut response, OP_PUTROOTFH);
                        parse_result_header(&mut response, OP_ACCESS);
                        assert_eq!(response.u32("supported ACCESS bits").unwrap(), ACCESS4_ALL);
                        assert_eq!(
                            response.u32("granted ACCESS bits").unwrap(),
                            expected_access,
                            "AUTH_NONE and nonowners must not gain root-only access"
                        );
                        response.end("ACCESS response").unwrap();
                        client.sequence += 1;
                    }

                    stream.shutdown().await.expect("close NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn anonymous access test thread")
        .join()
        .expect("anonymous access test thread panicked");
}

#[test]
fn nfs_v4_session_survives_transport_reconnect() {
    std::thread::Builder::new()
        .name("nfs-v4-reconnect-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 reconnect test runtime")
                .block_on(async {
                    let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
                    let address = server.listen().await.expect("listen rootless NFS server");
                    let mut first = TcpStream::connect(address)
                        .await
                        .expect("connect first NFS transport");
                    let clientid = parse_exchange(
                        rpc(&mut first, 201, compound("exchange", &[exchange_args()])).await,
                    );
                    let session = parse_create_session(
                        rpc(
                            &mut first,
                            202,
                            compound("create-session", &[create_session_args(clientid)]),
                        )
                        .await,
                    );
                    let mut client = Client {
                        session,
                        clientid,
                        sequence: 1,
                        slot: 0,
                    };
                    parse_sequence_and_handle(
                        rpc(
                            &mut first,
                            203,
                            compound(
                                "initial-root",
                                &[
                                    sequence(&client),
                                    op(OP_PUTROOTFH, |_| {}),
                                    op(OP_GETFH, |_| {}),
                                ],
                            ),
                        )
                        .await,
                    );

                    first.shutdown().await.expect("close first NFS transport");
                    timeout(Duration::from_secs(2), async {
                        while server.connections() != 0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("first NFS transport closes");

                    let mut second = TcpStream::connect(address)
                        .await
                        .expect("connect replacement NFS transport");
                    client.sequence += 1;
                    let mut response = rpc(
                        &mut second,
                        204,
                        compound(
                            "reconnected-root",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_GETFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "reconnected sequence");
                    parse_result_header(&mut response, OP_PUTROOTFH);
                    parse_result_header(&mut response, OP_GETFH);
                    let _ = response.var_opaque(128, "reconnected root handle").unwrap();
                    response.end("reconnected root response").unwrap();

                    second
                        .shutdown()
                        .await
                        .expect("close replacement NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn v4 reconnect test thread")
        .join()
        .expect("v4 reconnect test thread panicked");
}

#[test]
fn nfs_v4_cached_remove_reply_survives_tcp_reconnect_without_reexecution() {
    std::thread::Builder::new()
        .name("nfs-v4-replay-reconnect-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 replay test runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver.write_file("/replay-first", b"first").await.unwrap();
                    driver
                        .write_file("/replay-second", b"second")
                        .await
                        .unwrap();
                    let server = NfsServer::new(driver.clone(), NfsServerOptions::default());
                    let address = server.listen().await.expect("listen replay NFS server");
                    let (mut first, mut client) =
                        connect_v4_client(address, 601, b"replay-reconnect-client").await;
                    let original_user = auth_sys(1000, 1000, "replay-client");
                    let same_user_new_machine = auth_sys(1000, 1000, "replay-reconnected");
                    let different_user = auth_sys(2000, 2000, "replay-client");

                    let mut response = rpc_with_credential(
                        &mut first,
                        604,
                        compound(
                            "remove-first",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("replay-first")),
                            ],
                        ),
                        Some(&original_user),
                    )
                    .await;
                    let first_body = response.rest();
                    let mut parsed = XdrReader::new(&first_body);
                    parse_compound_header(&mut parsed, 3);
                    consume_sequence_result(&mut parsed, "initial remove");
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_REMOVE);
                    let _ = parsed.bool("remove change atomic").unwrap();
                    let _ = parsed.u64("remove change before").unwrap();
                    let _ = parsed.u64("remove change after").unwrap();
                    parsed.end("initial remove response").unwrap();
                    assert!(driver.stat("/replay-first").await.is_err());
                    assert!(driver.stat("/replay-second").await.is_ok());

                    first
                        .shutdown()
                        .await
                        .expect("close first replay transport");
                    timeout(Duration::from_secs(2), async {
                        while server.connections() != 0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("first replay transport closes");
                    let mut second = TcpStream::connect(address)
                        .await
                        .expect("connect replacement replay transport");
                    let mut foreign_retry = rpc_with_credential(
                        &mut second,
                        605,
                        compound(
                            "foreign-retry",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("replay-second")),
                            ],
                        ),
                        Some(&different_user),
                    )
                    .await;
                    assert_eq!(
                        parse_compound_status(&mut foreign_retry, 1),
                        NFS4ERR_SEQ_FALSE_RETRY
                    );
                    assert_eq!(
                        parse_result_status(&mut foreign_retry, OP_SEQUENCE),
                        NFS4ERR_SEQ_FALSE_RETRY
                    );
                    foreign_retry.end("foreign replay response").unwrap();
                    assert!(driver.stat("/replay-second").await.is_ok());

                    // A changed target is intentional: the same slot/sequence
                    // can replay the old body for the same effective user,
                    // never executing the new REMOVE.
                    let mut replay = rpc_with_credential(
                        &mut second,
                        606,
                        compound(
                            "altered-retry",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("replay-second")),
                            ],
                        ),
                        Some(&same_user_new_machine),
                    )
                    .await;
                    assert_eq!(replay.rest(), first_body);
                    assert!(driver.stat("/replay-second").await.is_ok());

                    client.sequence += 1;
                    let mut response = rpc_with_credential(
                        &mut second,
                        607,
                        compound(
                            "fresh-remove",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("replay-second")),
                            ],
                        ),
                        Some(&original_user),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "fresh remove");
                    parse_result_header(&mut response, OP_PUTROOTFH);
                    parse_result_header(&mut response, OP_REMOVE);
                    let _ = response.bool("fresh remove change atomic").unwrap();
                    let _ = response.u64("fresh remove change before").unwrap();
                    let _ = response.u64("fresh remove change after").unwrap();
                    response.end("fresh remove response").unwrap();
                    assert!(driver.stat("/replay-second").await.is_err());

                    second.shutdown().await.expect("close replay transport");
                    server.close().await.expect("close replay NFS server");
                });
        })
        .expect("spawn v4 replay test thread")
        .join()
        .expect("v4 replay test thread panicked");
}

#[test]
fn nfs_v4_uncached_hint_still_replays_small_completed_mutation() {
    std::thread::Builder::new()
        .name("nfs-v4-uncached-hint-replay-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build uncached-hint replay runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver
                        .write_file("/uncached-first", b"first")
                        .await
                        .unwrap();
                    driver
                        .write_file("/uncached-second", b"second")
                        .await
                        .unwrap();
                    let server = NfsServer::new(driver.clone(), NfsServerOptions::default());
                    let address = server.listen().await.expect("listen NFS server");
                    let (mut stream, mut client) =
                        connect_v4_client(address, 801, b"uncached-hint-client").await;

                    let mut response = rpc(
                        &mut stream,
                        804,
                        compound(
                            "uncached-first",
                            &[
                                sequence_with_cachethis(&client, false),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("uncached-first")),
                            ],
                        ),
                    )
                    .await;
                    let first_body = response.rest();
                    let mut first = XdrReader::new(&first_body);
                    parse_compound_header(&mut first, 3);
                    consume_sequence_result(&mut first, "uncached-hint first sequence");
                    parse_result_header(&mut first, OP_PUTROOTFH);
                    parse_result_header(&mut first, OP_REMOVE);
                    let _ = first.bool("remove change atomic").unwrap();
                    let _ = first.u64("remove change before").unwrap();
                    let _ = first.u64("remove change after").unwrap();
                    first.end("uncached-hint first response").unwrap();
                    assert!(driver.stat("/uncached-first").await.is_err());

                    // The changed target must not execute, even though the
                    // original SEQUENCE did not request full reply caching.
                    let mut retry = rpc(
                        &mut stream,
                        805,
                        compound(
                            "uncached-retry",
                            &[
                                sequence_with_cachethis(&client, false),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("uncached-second")),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(retry.rest(), first_body);
                    assert!(driver.stat("/uncached-second").await.is_ok());

                    client.sequence += 1;
                    let mut fresh = rpc(
                        &mut stream,
                        806,
                        compound(
                            "uncached-fresh",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("uncached-second")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut fresh, 3);
                    consume_sequence_result(&mut fresh, "uncached-hint fresh sequence");
                    parse_result_header(&mut fresh, OP_PUTROOTFH);
                    parse_result_header(&mut fresh, OP_REMOVE);
                    let _ = fresh.bool("fresh remove change atomic").unwrap();
                    let _ = fresh.u64("fresh remove change before").unwrap();
                    let _ = fresh.u64("fresh remove change after").unwrap();
                    fresh.end("uncached-hint fresh response").unwrap();
                    assert!(driver.stat("/uncached-second").await.is_err());

                    stream.shutdown().await.expect("close NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn uncached-hint replay test thread")
        .join()
        .expect("uncached-hint replay test thread panicked");
}

#[test]
fn nfs_v4_oversized_uncached_reply_retries_without_repeating_mutation() {
    std::thread::Builder::new()
        .name("nfs-v4-oversized-uncached-replay-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build oversized replay runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver.write_file("/large-first", b"first").await.unwrap();
                    driver.write_file("/large-second", b"second").await.unwrap();
                    for index in 0..16 {
                        driver
                            .write_file(&format!("/listed-{index:02}"), b"entry")
                            .await
                            .unwrap();
                    }
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.max_cached_response_size = 128;
                    let server = NfsServer::new(driver.clone(), options);
                    let address = server.listen().await.expect("listen NFS server");
                    let (mut stream, mut client) =
                        connect_v4_client(address, 901, b"oversized-replay-client").await;
                    let read_dir = op(OP_READDIR, |writer| {
                        writer.u64(0);
                        writer.fixed_opaque(&[0; 8], 8);
                        writer.u32(4096);
                        writer.u32(4096);
                        writer.u32(0);
                    });

                    let mut original = rpc(
                        &mut stream,
                        904,
                        compound(
                            "oversized-original",
                            &[
                                sequence_with_cachethis(&client, false),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("large-first")),
                                read_dir,
                            ],
                        ),
                    )
                    .await;
                    let original_body = original.rest();
                    assert!(original_body.len() > 128, "reply exceeds cached limit");
                    let mut parsed = XdrReader::new(&original_body);
                    parse_compound_header(&mut parsed, 4);
                    consume_sequence_result(&mut parsed, "oversized original sequence");
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_REMOVE);
                    assert!(driver.stat("/large-first").await.is_err());
                    assert!(driver.stat("/large-second").await.is_ok());

                    let mut retry = rpc(
                        &mut stream,
                        905,
                        compound(
                            "oversized-retry",
                            &[
                                sequence_with_cachethis(&client, false),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("large-second")),
                            ],
                        ),
                    )
                    .await;
                    let marker = retry.rest();
                    assert!(marker.len() <= 128, "retry marker fits cached limit");
                    let mut retry = XdrReader::new(&marker);
                    assert_eq!(
                        parse_compound_status(&mut retry, 2),
                        NFS4ERR_RETRY_UNCACHED_REP
                    );
                    consume_sequence_result(&mut retry, "oversized retry sequence");
                    assert_eq!(
                        parse_result_status(&mut retry, OP_PUTROOTFH),
                        NFS4ERR_RETRY_UNCACHED_REP
                    );
                    retry.end("oversized retry response").unwrap();
                    assert!(driver.stat("/large-second").await.is_ok());

                    let mut repeated = rpc(
                        &mut stream,
                        906,
                        compound(
                            "oversized-repeated-retry",
                            &[
                                sequence_with_cachethis(&client, false),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("large-second")),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(repeated.rest(), marker);
                    assert!(driver.stat("/large-second").await.is_ok());

                    client.sequence += 1;
                    let mut fresh = rpc(
                        &mut stream,
                        907,
                        compound(
                            "oversized-fresh",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("large-second")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut fresh, 3);
                    consume_sequence_result(&mut fresh, "oversized fresh sequence");
                    parse_result_header(&mut fresh, OP_PUTROOTFH);
                    parse_result_header(&mut fresh, OP_REMOVE);
                    let _ = fresh.bool("fresh remove change atomic").unwrap();
                    let _ = fresh.u64("fresh remove change before").unwrap();
                    let _ = fresh.u64("fresh remove change after").unwrap();
                    fresh.end("oversized fresh response").unwrap();
                    assert!(driver.stat("/large-second").await.is_err());

                    stream.shutdown().await.expect("close NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn oversized replay test thread")
        .join()
        .expect("oversized replay test thread panicked");
}

#[test]
fn nfs_v4_tiny_cache_fences_uncacheable_completed_mutation() {
    std::thread::Builder::new()
        .name("nfs-v4-tiny-cache-fence-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build tiny-cache runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver.write_file("/tiny-first", b"first").await.unwrap();
                    driver.write_file("/tiny-second", b"second").await.unwrap();
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.max_cached_response_size = 96;
                    let server = NfsServer::new(driver.clone(), options);
                    let address = server.listen().await.expect("listen NFS server");
                    let (mut stream, client) =
                        connect_v4_client(address, 1501, b"tiny-cache-client").await;

                    let mut original = rpc(
                        &mut stream,
                        1504,
                        compound(
                            "tiny-cache",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("tiny-first")),
                            ],
                        ),
                    )
                    .await;
                    let original_body = original.rest();
                    assert!(original_body.len() > 96, "reply exceeds tiny cache");
                    let mut parsed = XdrReader::new(&original_body);
                    parse_compound_header(&mut parsed, 3);
                    consume_sequence_result(&mut parsed, "tiny-cache sequence");
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_REMOVE);
                    assert!(driver.stat("/tiny-first").await.is_err());

                    let mut retry = rpc(
                        &mut stream,
                        1505,
                        compound(
                            "tiny-retry",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("tiny-second")),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut retry, 0), NFS4ERR_BADSESSION);
                    retry.end("tiny-cache retry").unwrap();
                    assert!(driver.stat("/tiny-second").await.is_ok());

                    stream.shutdown().await.expect("close NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn tiny-cache test thread")
        .join()
        .expect("tiny-cache test thread panicked");
}

#[test]
fn nfs_v4_cache_required_oversized_getfh_preserves_mutation_reply() {
    std::thread::Builder::new()
        .name("nfs-v4-cache-required-oversized-getfh-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build oversized GETFH runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver.write_file("/fh-first", b"first").await.unwrap();
                    driver.write_file("/fh-second", b"second").await.unwrap();
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.max_cached_response_size = 112;
                    let server = NfsServer::new(driver.clone(), options);
                    let address = server.listen().await.expect("listen NFS server");
                    let (mut stream, mut client) =
                        connect_v4_client(address, 1401, b"oversized-getfh-client").await;

                    let mut original = rpc(
                        &mut stream,
                        1404,
                        compound(
                            "fh-big",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("fh-first")),
                                op(OP_GETFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    let original_body = original.rest();
                    assert!(original_body.len() <= 112, "response must be cacheable");
                    let mut parsed = XdrReader::new(&original_body);
                    assert_eq!(
                        parse_compound_status(&mut parsed, 4),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    consume_sequence_result(&mut parsed, "oversized GETFH sequence");
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_REMOVE);
                    let _ = parsed.bool("remove change atomic").unwrap();
                    let _ = parsed.u64("remove change before").unwrap();
                    let _ = parsed.u64("remove change after").unwrap();
                    assert_eq!(
                        parse_result_status(&mut parsed, OP_GETFH),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    parsed.end("oversized GETFH response").unwrap();
                    assert!(driver.stat("/fh-first").await.is_err());
                    assert!(driver.stat("/fh-second").await.is_ok());

                    let mut retry = rpc(
                        &mut stream,
                        1405,
                        compound(
                            "changed-fh-target",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("fh-second")),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(retry.rest(), original_body);
                    assert!(driver.stat("/fh-second").await.is_ok());

                    client.sequence += 1;
                    let mut fresh = rpc(
                        &mut stream,
                        1406,
                        compound(
                            "fresh-fh-sequence",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("fh-second")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut fresh, 3);
                    consume_sequence_result(&mut fresh, "fresh GETFH sequence");
                    parse_result_header(&mut fresh, OP_PUTROOTFH);
                    parse_result_header(&mut fresh, OP_REMOVE);
                    assert!(driver.stat("/fh-second").await.is_err());

                    stream.shutdown().await.expect("close NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn oversized GETFH test thread")
        .join()
        .expect("oversized GETFH test thread panicked");
}

#[test]
fn nfs_v4_cache_required_oversized_getattr_preserves_mutation_reply() {
    std::thread::Builder::new()
        .name("nfs-v4-cache-required-oversized-getattr-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build oversized GETATTR runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver.write_file("/attr-first", b"first").await.unwrap();
                    driver.write_file("/attr-second", b"second").await.unwrap();
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.max_cached_response_size = 160;
                    let server = NfsServer::new(driver.clone(), options);
                    let address = server.listen().await.expect("listen NFS server");
                    let (mut stream, mut client) =
                        connect_v4_client(address, 1301, b"oversized-getattr-client").await;

                    let mut original = rpc(
                        &mut stream,
                        1304,
                        compound(
                            "attr-big",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("attr-first")),
                                op(OP_GETATTR, |writer| {
                                    writer.u32(2);
                                    writer.u32(u32::MAX);
                                    writer.u32(!((1_u32 << 16) | (1_u32 << 22)));
                                }),
                            ],
                        ),
                    )
                    .await;
                    let original_body = original.rest();
                    assert!(original_body.len() <= 160, "response must be cacheable");
                    let mut parsed = XdrReader::new(&original_body);
                    assert_eq!(
                        parse_compound_status(&mut parsed, 4),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    consume_sequence_result(&mut parsed, "oversized GETATTR sequence");
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_REMOVE);
                    let _ = parsed.bool("remove change atomic").unwrap();
                    let _ = parsed.u64("remove change before").unwrap();
                    let _ = parsed.u64("remove change after").unwrap();
                    assert_eq!(
                        parse_result_status(&mut parsed, OP_GETATTR),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    parsed.end("oversized GETATTR response").unwrap();
                    assert!(driver.stat("/attr-first").await.is_err());
                    assert!(driver.stat("/attr-second").await.is_ok());

                    let mut retry = rpc(
                        &mut stream,
                        1305,
                        compound(
                            "changed-attr-target",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("attr-second")),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(retry.rest(), original_body);
                    assert!(driver.stat("/attr-second").await.is_ok());

                    client.sequence += 1;
                    let mut fresh = rpc(
                        &mut stream,
                        1306,
                        compound(
                            "fresh-attr-sequence",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("attr-second")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut fresh, 3);
                    consume_sequence_result(&mut fresh, "fresh GETATTR sequence");
                    parse_result_header(&mut fresh, OP_PUTROOTFH);
                    parse_result_header(&mut fresh, OP_REMOVE);
                    assert!(driver.stat("/attr-second").await.is_err());

                    stream.shutdown().await.expect("close NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn oversized GETATTR test thread")
        .join()
        .expect("oversized GETATTR test thread panicked");
}

#[test]
fn nfs_v4_cache_required_oversized_readlink_preserves_mutation_reply() {
    std::thread::Builder::new()
        .name("nfs-v4-cache-required-oversized-readlink-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build oversized READLINK runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver
                        .symlink(&"x".repeat(512), "/long-link")
                        .await
                        .unwrap();
                    driver.write_file("/link-first", b"first").await.unwrap();
                    driver.write_file("/link-second", b"second").await.unwrap();
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.max_cached_response_size = 256;
                    let server = NfsServer::new(driver.clone(), options);
                    let address = server.listen().await.expect("listen NFS server");
                    let (mut stream, mut client) =
                        connect_v4_client(address, 1201, b"oversized-readlink-client").await;

                    let mut original = rpc(
                        &mut stream,
                        1204,
                        compound(
                            "link-big",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("link-first")),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_LOOKUP, |writer| writer.string("long-link")),
                                op(OP_READLINK, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    let original_body = original.rest();
                    assert!(original_body.len() <= 256, "response must be cacheable");
                    let mut parsed = XdrReader::new(&original_body);
                    assert_eq!(
                        parse_compound_status(&mut parsed, 6),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    consume_sequence_result(&mut parsed, "oversized READLINK sequence");
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_REMOVE);
                    let _ = parsed.bool("remove change atomic").unwrap();
                    let _ = parsed.u64("remove change before").unwrap();
                    let _ = parsed.u64("remove change after").unwrap();
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_LOOKUP);
                    assert_eq!(
                        parse_result_status(&mut parsed, OP_READLINK),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    parsed.end("oversized READLINK response").unwrap();
                    assert!(driver.stat("/link-first").await.is_err());
                    assert!(driver.stat("/link-second").await.is_ok());

                    let mut retry = rpc(
                        &mut stream,
                        1205,
                        compound(
                            "changed-link-target",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("link-second")),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(retry.rest(), original_body);
                    assert!(driver.stat("/link-second").await.is_ok());

                    client.sequence += 1;
                    let mut fresh = rpc(
                        &mut stream,
                        1206,
                        compound(
                            "fresh-link-sequence",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("link-second")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut fresh, 3);
                    consume_sequence_result(&mut fresh, "fresh READLINK sequence");
                    parse_result_header(&mut fresh, OP_PUTROOTFH);
                    parse_result_header(&mut fresh, OP_REMOVE);
                    assert!(driver.stat("/link-second").await.is_err());

                    stream.shutdown().await.expect("close NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn oversized READLINK test thread")
        .join()
        .expect("oversized READLINK test thread panicked");
}

#[test]
fn nfs_v4_cache_required_oversized_read_preserves_mutation_reply() {
    std::thread::Builder::new()
        .name("nfs-v4-cache-required-oversized-read-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build oversized READ runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver
                        .write_file("/read-target", &[b'x'; 512])
                        .await
                        .unwrap();
                    driver.write_file("/read-first", b"first").await.unwrap();
                    driver.write_file("/read-second", b"second").await.unwrap();
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.max_cached_response_size = 256;
                    let server = NfsServer::new(driver.clone(), options);
                    let address = server.listen().await.expect("listen NFS server");
                    let (mut stream, mut client) =
                        connect_v4_client(address, 1101, b"oversized-read-client").await;

                    let mut original = rpc(
                        &mut stream,
                        1104,
                        compound(
                            "read-big",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("read-first")),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_LOOKUP, |writer| writer.string("read-target")),
                                op(OP_READ, |writer| {
                                    writer.fixed_opaque(&[0; 16], 16);
                                    writer.u64(0);
                                    writer.u32(512);
                                }),
                            ],
                        ),
                    )
                    .await;
                    let original_body = original.rest();
                    assert!(original_body.len() <= 256, "response must be cacheable");
                    let mut parsed = XdrReader::new(&original_body);
                    assert_eq!(
                        parse_compound_status(&mut parsed, 6),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    consume_sequence_result(&mut parsed, "oversized READ sequence");
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_REMOVE);
                    let _ = parsed.bool("remove change atomic").unwrap();
                    let _ = parsed.u64("remove change before").unwrap();
                    let _ = parsed.u64("remove change after").unwrap();
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_LOOKUP);
                    assert_eq!(
                        parse_result_status(&mut parsed, OP_READ),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    parsed.end("oversized READ response").unwrap();
                    assert!(driver.stat("/read-first").await.is_err());
                    assert!(driver.stat("/read-second").await.is_ok());

                    let mut retry = rpc(
                        &mut stream,
                        1105,
                        compound(
                            "changed-read-target",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("read-second")),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(retry.rest(), original_body);
                    assert!(driver.stat("/read-second").await.is_ok());

                    client.sequence += 1;
                    let mut fresh = rpc(
                        &mut stream,
                        1106,
                        compound(
                            "fresh-read-sequence",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("read-second")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut fresh, 3);
                    consume_sequence_result(&mut fresh, "fresh READ sequence");
                    parse_result_header(&mut fresh, OP_PUTROOTFH);
                    parse_result_header(&mut fresh, OP_REMOVE);
                    assert!(driver.stat("/read-second").await.is_err());

                    stream.shutdown().await.expect("close NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn oversized READ test thread")
        .join()
        .expect("oversized READ test thread panicked");
}

#[test]
fn nfs_v4_cache_required_oversized_readdir_preserves_mutation_reply() {
    std::thread::Builder::new()
        .name("nfs-v4-cache-required-oversized-reply-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build cache-required replay runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver.write_file("/cached-first", b"first").await.unwrap();
                    driver
                        .write_file("/cached-second", b"second")
                        .await
                        .unwrap();
                    for index in 0..16 {
                        driver
                            .write_file(&format!("/cached-entry-{index:02}"), b"entry")
                            .await
                            .unwrap();
                    }
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.max_cached_response_size = 128;
                    let server = NfsServer::new(driver.clone(), options);
                    let address = server.listen().await.expect("listen NFS server");
                    let (mut stream, mut client) =
                        connect_v4_client(address, 1001, b"cache-required-client").await;
                    let read_dir = op(OP_READDIR, |writer| {
                        writer.u64(0);
                        writer.fixed_opaque(&[0; 8], 8);
                        writer.u32(4096);
                        writer.u32(4096);
                        writer.u32(0);
                    });

                    let mut original = rpc(
                        &mut stream,
                        1004,
                        compound(
                            "big",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("cached-first")),
                                read_dir,
                            ],
                        ),
                    )
                    .await;
                    let original_body = original.rest();
                    assert!(original_body.len() <= 128, "response must be cacheable");
                    let mut parsed = XdrReader::new(&original_body);
                    assert_eq!(
                        parse_compound_status(&mut parsed, 4),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    consume_sequence_result(&mut parsed, "cache-required original sequence");
                    parse_result_header(&mut parsed, OP_PUTROOTFH);
                    parse_result_header(&mut parsed, OP_REMOVE);
                    let _ = parsed.bool("remove change atomic").unwrap();
                    let _ = parsed.u64("remove change before").unwrap();
                    let _ = parsed.u64("remove change after").unwrap();
                    assert_eq!(
                        parse_result_status(&mut parsed, OP_READDIR),
                        NFS4ERR_REP_TOO_BIG_TO_CACHE
                    );
                    parsed.end("cache-required oversized response").unwrap();
                    assert!(driver.stat("/cached-first").await.is_err());
                    assert!(driver.stat("/cached-second").await.is_ok());

                    let mut retry = rpc(
                        &mut stream,
                        1005,
                        compound(
                            "changed-target",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("cached-second")),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(retry.rest(), original_body);
                    assert!(driver.stat("/cached-second").await.is_ok());

                    client.sequence += 1;
                    let mut fresh = rpc(
                        &mut stream,
                        1006,
                        compound(
                            "fresh",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string("cached-second")),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut fresh, 3);
                    consume_sequence_result(&mut fresh, "cache-required fresh sequence");
                    parse_result_header(&mut fresh, OP_PUTROOTFH);
                    parse_result_header(&mut fresh, OP_REMOVE);
                    let _ = fresh.bool("fresh remove change atomic").unwrap();
                    let _ = fresh.u64("fresh remove change before").unwrap();
                    let _ = fresh.u64("fresh remove change after").unwrap();
                    fresh.end("cache-required fresh response").unwrap();
                    assert!(driver.stat("/cached-second").await.is_err());

                    stream.shutdown().await.expect("close NFS transport");
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn cache-required replay test thread")
        .join()
        .expect("cache-required replay test thread panicked");
}

#[test]
fn nfs_v4_busy_slot_delays_retry_and_rejects_next_sequence() {
    std::thread::Builder::new()
        .name("nfs-v4-busy-slot-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 busy-slot test runtime")
                .block_on(async {
                    let block_once = Arc::new(AtomicBool::new(false));
                    let entered = Arc::new(Notify::new());
                    let release = Arc::new(Notify::new());
                    let server = NfsServer::new(
                        GateStatDriver {
                            inner: MemoryFs::empty(),
                            block_once: Arc::clone(&block_once),
                            entered: Arc::clone(&entered),
                            release: Arc::clone(&release),
                        },
                        NfsServerOptions::default(),
                    );
                    let address = server.listen().await.expect("listen busy-slot NFS server");
                    let (mut first, client) =
                        connect_v4_client(address, 701, b"busy-slot-client").await;
                    let second = TcpStream::connect(address)
                        .await
                        .expect("connect second busy-slot transport");
                    let getattr = |client: &Client| {
                        compound(
                            "busy-slot-getattr",
                            &[
                                sequence(client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_GETATTR, |writer| writer.u32(0)),
                            ],
                        )
                    };
                    block_once.store(true, Ordering::Release);
                    let first_args = getattr(&client);
                    let first_task = tokio::spawn(async move {
                        let mut response = rpc(&mut first, 704, first_args).await;
                        response.rest()
                    });
                    timeout(Duration::from_secs(2), entered.notified())
                        .await
                        .expect("first SEQUENCE reaches blocked backend");

                    let requests_before_retry = server.v4_session().stats().requests;
                    let retry_args = getattr(&client);
                    let retry_task = tokio::spawn(async move {
                        let mut second = second;
                        let mut reply = rpc(&mut second, 705, retry_args).await;
                        (second, reply.rest())
                    });
                    timeout(Duration::from_secs(2), async {
                        while server.v4_session().stats().requests == requests_before_retry {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("retry reaches the NFSv4 server while original is blocked");
                    let (mut second, delay_body) = timeout(Duration::from_millis(250), retry_task)
                        .await
                        .expect("in-flight retry receives a bounded SEQUENCE reply")
                        .expect("in-flight retry task succeeds");
                    let mut delayed = XdrReader::new(&delay_body);
                    assert_eq!(parse_compound_status(&mut delayed, 0), NFS4ERR_DELAY);
                    delayed.end("in-flight retry response").unwrap();
                    let mut next_client = client.clone();
                    next_client.sequence += 1;
                    let mut premature = timeout(
                        Duration::from_millis(250),
                        rpc(&mut second, 706, getattr(&next_client)),
                    )
                    .await
                    .expect("premature next sequence receives a bounded reply");
                    assert_eq!(
                        parse_compound_status(&mut premature, 0),
                        NFS4ERR_SEQ_MISORDERED
                    );
                    premature.end("premature sequence response").unwrap();
                    release.notify_one();
                    let first_body = timeout(Duration::from_secs(2), first_task)
                        .await
                        .expect("blocked original request completes")
                        .expect("original request task succeeds");
                    let mut original = XdrReader::new(&first_body);
                    parse_compound_header(&mut original, 3);
                    let mut replay = rpc(&mut second, 707, getattr(&client)).await;
                    assert_eq!(replay.rest(), first_body);
                    let mut fresh = rpc(&mut second, 708, getattr(&next_client)).await;
                    parse_compound_header(&mut fresh, 3);
                    second.shutdown().await.expect("close busy-slot transport");
                    server.close().await.expect("close busy-slot NFS server");
                });
        })
        .expect("spawn v4 busy-slot test thread")
        .join()
        .expect("v4 busy-slot test thread panicked");
}

#[test]
fn nfs_v4_canceled_mutation_fences_uncached_session_for_recovery() {
    std::thread::Builder::new()
        .name("nfs-v4-canceled-compound-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 canceled-compound test runtime")
                .block_on(async {
                    let driver = MemoryFs::empty();
                    driver
                        .write_file("/canceled-first", b"first")
                        .await
                        .unwrap();
                    driver
                        .write_file("/canceled-second", b"second")
                        .await
                        .unwrap();
                    let block_once = Arc::new(AtomicBool::new(false));
                    let entered = Arc::new(Notify::new());
                    let release = Arc::new(Notify::new());
                    let server = NfsServer::new(
                        GateUnlinkDriver {
                            inner: driver.clone(),
                            block_once: Arc::clone(&block_once),
                            entered: Arc::clone(&entered),
                            release,
                        },
                        NfsServerOptions::default(),
                    );
                    let address = server.listen().await.expect("listen canceled NFS server");
                    let (mut first, client) =
                        connect_v4_client(address, 741, b"canceled-compound-client").await;
                    let first_connection = server
                        .clients()
                        .expect("list canceled-compound connections")
                        .into_iter()
                        .next()
                        .expect("first canceled-compound connection");
                    let remove = |client: &Client, target: &str| {
                        compound(
                            "canceled-remove",
                            &[
                                sequence(client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_REMOVE, |writer| writer.string(target)),
                            ],
                        )
                    };
                    block_once.store(true, Ordering::Release);
                    let request = encode_call(
                        744,
                        NFS4_PROGRAM,
                        NFS_V4,
                        1,
                        None,
                        None,
                        &remove(&client, "canceled-first"),
                    );
                    first
                        .write_all(&frame_record(&request).expect("frame canceled COMPOUND"))
                        .await
                        .expect("send canceled COMPOUND");
                    timeout(Duration::from_secs(2), entered.notified())
                        .await
                        .expect("canceled REMOVE deletes the first file and stalls");
                    timeout(Duration::from_secs(2), first_connection.close())
                        .await
                        .expect("connection close cancels blocked COMPOUND")
                        .expect("close canceled connection");
                    drop(first);
                    assert!(driver.stat("/canceled-first").await.is_err());
                    assert!(driver.stat("/canceled-second").await.is_ok());

                    let mut second = TcpStream::connect(address)
                        .await
                        .expect("connect replacement transport");
                    let mut old_retry =
                        rpc(&mut second, 745, remove(&client, "canceled-second")).await;
                    assert_eq!(parse_compound_status(&mut old_retry, 0), NFS4ERR_BADSESSION);
                    old_retry.end("canceled-session retry").unwrap();
                    assert!(driver.stat("/canceled-second").await.is_ok());

                    let mut replacement_reply = rpc(
                        &mut second,
                        746,
                        compound(
                            "replacement-session",
                            &[create_session_args_with_sequence(client.clientid, 2)],
                        ),
                    )
                    .await;
                    let replacement = parse_create_session_with_sequence(&mut replacement_reply, 2);
                    assert_ne!(replacement, client.session);
                    let replacement_client = Client {
                        session: replacement,
                        clientid: client.clientid,
                        sequence: 1,
                        slot: 0,
                    };
                    let mut recovered = rpc(
                        &mut second,
                        747,
                        remove(&replacement_client, "canceled-second"),
                    )
                    .await;
                    parse_compound_header(&mut recovered, 3);
                    assert!(driver.stat("/canceled-second").await.is_err());
                    second
                        .shutdown()
                        .await
                        .expect("close replacement transport");
                    server.close().await.expect("close canceled NFS server");
                });
        })
        .expect("spawn v4 canceled-compound test thread")
        .join()
        .expect("v4 canceled-compound test thread panicked");
}

#[test]
fn nfs_v4_independent_slots_overlap_while_one_backend_call_is_blocked() {
    std::thread::Builder::new()
        .name("nfs-v4-independent-slots-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 independent-slots test runtime")
                .block_on(async {
                    let block_once = Arc::new(AtomicBool::new(false));
                    let entered = Arc::new(Notify::new());
                    let release = Arc::new(Notify::new());
                    let server = NfsServer::new(
                        GateStatDriver {
                            inner: MemoryFs::empty(),
                            block_once: Arc::clone(&block_once),
                            entered: Arc::clone(&entered),
                            release: Arc::clone(&release),
                        },
                        NfsServerOptions::default(),
                    );
                    let address = server.listen().await.expect("listen two-slot NFS server");
                    let (mut first, slot_zero) =
                        connect_v4_client(address, 721, b"two-slot-client").await;
                    let mut second = TcpStream::connect(address)
                        .await
                        .expect("connect second two-slot transport");
                    let mut slot_one = slot_zero.clone();
                    slot_one.slot = 1;
                    slot_one.sequence = 1;
                    let getattr = |client: &Client| {
                        compound(
                            "two-slot-getattr",
                            &[
                                sequence(client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_GETATTR, |writer| writer.u32(0)),
                            ],
                        )
                    };
                    block_once.store(true, Ordering::Release);
                    let first_args = getattr(&slot_zero);
                    let first_task = tokio::spawn(async move {
                        let mut reply = rpc(&mut first, 724, first_args).await;
                        reply.rest()
                    });
                    timeout(Duration::from_secs(2), entered.notified())
                        .await
                        .expect("slot zero reaches blocked backend");

                    let mut independent = timeout(
                        Duration::from_millis(250),
                        rpc(&mut second, 725, getattr(&slot_one)),
                    )
                    .await
                    .expect("independent slot completes while slot zero is blocked");
                    parse_compound_header(&mut independent, 3);
                    assert!(
                        !first_task.is_finished(),
                        "slot zero remains blocked when slot one completes"
                    );
                    release.notify_one();
                    let first_body = timeout(Duration::from_secs(2), first_task)
                        .await
                        .expect("blocked slot zero completes")
                        .expect("slot zero task succeeds");
                    parse_compound_header(&mut XdrReader::new(&first_body), 3);
                    second.shutdown().await.expect("close two-slot transport");
                    server.close().await.expect("close two-slot NFS server");
                });
        })
        .expect("spawn v4 independent-slots test thread")
        .join()
        .expect("v4 independent-slots test thread panicked");
}

#[test]
fn nfs_v4_expired_lease_waits_for_blocked_slot_before_sweeping() {
    std::thread::Builder::new()
        .name("nfs-v4-expired-busy-slot-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 expired busy-slot test runtime")
                .block_on(async {
                    let ticks = Arc::new(AtomicU64::new(0));
                    let base = Instant::now();
                    let clock_ticks = Arc::clone(&ticks);
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.lease_seconds = 1;
                    options.session.nfs4.clock = Nfs4Clock::from_fn(move || {
                        base + Duration::from_secs(clock_ticks.load(Ordering::Acquire))
                    });
                    let block_once = Arc::new(AtomicBool::new(false));
                    let entered = Arc::new(Notify::new());
                    let release = Arc::new(Notify::new());
                    let server = NfsServer::new(
                        GateStatDriver {
                            inner: MemoryFs::empty(),
                            block_once: Arc::clone(&block_once),
                            entered: Arc::clone(&entered),
                            release: Arc::clone(&release),
                        },
                        options,
                    );
                    let address = server
                        .listen()
                        .await
                        .expect("listen expired-slot NFS server");
                    let (mut first, slot_zero) =
                        connect_v4_client(address, 731, b"expired-slot-client").await;
                    let mut second = TcpStream::connect(address)
                        .await
                        .expect("connect second expired-slot transport");
                    let mut slot_one = slot_zero.clone();
                    slot_one.slot = 1;
                    slot_one.sequence = 1;
                    let getattr = |client: &Client| {
                        compound(
                            "expired-slot-getattr",
                            &[
                                sequence(client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_GETATTR, |writer| writer.u32(0)),
                            ],
                        )
                    };
                    block_once.store(true, Ordering::Release);
                    let first_args = getattr(&slot_zero);
                    let first_task = tokio::spawn(async move {
                        let mut reply = rpc(&mut first, 734, first_args).await;
                        reply.rest()
                    });
                    timeout(Duration::from_secs(2), entered.notified())
                        .await
                        .expect("slot zero reaches blocked backend before lease expiry");
                    ticks.store(1, Ordering::Release);

                    let requests_before_second = server.v4_session().stats().requests;
                    let second_args = getattr(&slot_one);
                    let mut second_task = tokio::spawn(async move {
                        let reply = rpc(&mut second, 735, second_args).await;
                        (second, reply)
                    });
                    timeout(Duration::from_secs(2), async {
                        while server.v4_session().stats().requests == requests_before_second {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("expired slot-one request reaches the server");
                    assert!(
                        timeout(Duration::from_millis(25), &mut second_task)
                            .await
                            .is_err(),
                        "lease sweep must wait for the blocked slot"
                    );
                    release.notify_one();
                    let first_body = timeout(Duration::from_secs(2), first_task)
                        .await
                        .expect("blocked slot zero completes")
                        .expect("slot zero task succeeds");
                    parse_compound_header(&mut XdrReader::new(&first_body), 3);
                    let (mut second, mut expired) = timeout(Duration::from_secs(2), second_task)
                        .await
                        .expect("expired slot one receives a response")
                        .expect("slot one task succeeds");
                    assert_eq!(parse_compound_status(&mut expired, 0), NFS4ERR_BADSESSION);
                    expired.end("expired slot-one response").unwrap();
                    assert_eq!(server.v4_session().sweep_expired().await, 0);
                    second
                        .shutdown()
                        .await
                        .expect("close expired-slot transport");
                    server.close().await.expect("close expired-slot NFS server");
                });
        })
        .expect("spawn v4 expired busy-slot test thread")
        .join()
        .expect("v4 expired busy-slot test thread panicked");
}

#[test]
fn nfs_v4_session_state_is_process_local_after_server_restart() {
    std::thread::Builder::new()
        .name("nfs-v4-restart-boundary-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 restart boundary test runtime")
                .block_on(async {
                    // Reuse the backend across server instances to isolate the
                    // boundary: filesystem ownership can outlive the server,
                    // but NFSv4 session/lease/replay state cannot.
                    let driver = MemoryFs::empty();
                    let server = NfsServer::new(driver.clone(), NfsServerOptions::default());
                    let address = server.listen().await.expect("listen first NFS server");
                    let mut first = TcpStream::connect(address)
                        .await
                        .expect("connect first NFS server");
                    let clientid = parse_exchange(
                        rpc(&mut first, 301, compound("exchange", &[exchange_args()])).await,
                    );
                    let session = parse_create_session(
                        rpc(
                            &mut first,
                            302,
                            compound("create-session", &[create_session_args(clientid)]),
                        )
                        .await,
                    );
                    let mut client = Client {
                        session,
                        clientid,
                        sequence: 1,
                        slot: 0,
                    };
                    parse_sequence_and_handle(
                        rpc(
                            &mut first,
                            303,
                            compound(
                                "initial-root",
                                &[
                                    sequence(&client),
                                    op(OP_PUTROOTFH, |_| {}),
                                    op(OP_GETFH, |_| {}),
                                ],
                            ),
                        )
                        .await,
                    );

                    first.shutdown().await.expect("close first NFS transport");
                    server.close().await.expect("close first NFS server");
                    assert!(server.v4_session().destroyed());

                    let restarted = NfsServer::new(driver, NfsServerOptions::default());
                    let restarted_address = restarted
                        .listen()
                        .await
                        .expect("listen restarted NFS server");
                    let mut second = TcpStream::connect(restarted_address)
                        .await
                        .expect("connect restarted NFS server");
                    client.sequence += 1;
                    let mut response = rpc(
                        &mut second,
                        304,
                        compound("stale-session", &[sequence(&client)]),
                    )
                    .await;
                    assert_eq!(
                        parse_compound_status(&mut response, 0),
                        NFS4ERR_BADSESSION,
                        "a restarted server rejects the old session before dispatch"
                    );
                    response.end("stale session response").unwrap();

                    second
                        .shutdown()
                        .await
                        .expect("close restarted NFS transport");
                    restarted.close().await.expect("close restarted NFS server");
                });
        })
        .expect("spawn v4 restart boundary test thread")
        .join()
        .expect("v4 restart boundary test thread panicked");
}

#[test]
fn nfs_v4_expired_lease_sweeps_session_state() {
    std::thread::Builder::new()
        .name("nfs-v4-lease-expiry-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 lease expiry test runtime")
                .block_on(async {
                    let ticks = Arc::new(AtomicU64::new(0));
                    let base = Instant::now();
                    let clock_ticks = Arc::clone(&ticks);
                    let clock = Nfs4Clock::from_fn(move || {
                        base + Duration::from_secs(clock_ticks.load(Ordering::Acquire))
                    });
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.lease_seconds = 1;
                    options.session.nfs4.clock = clock;
                    let server = NfsServer::new(MemoryFs::empty(), options);
                    let address = server.listen().await.expect("listen lease test server");
                    let mut stream = TcpStream::connect(address)
                        .await
                        .expect("connect lease test client");
                    let clientid = parse_exchange(
                        rpc(&mut stream, 501, compound("exchange", &[exchange_args()])).await,
                    );
                    let session = parse_create_session(
                        rpc(
                            &mut stream,
                            502,
                            compound("create-session", &[create_session_args(clientid)]),
                        )
                        .await,
                    );
                    let client = Client {
                        session,
                        clientid,
                        sequence: 1,
                        slot: 0,
                    };
                    parse_sequence_and_handle(
                        rpc(
                            &mut stream,
                            503,
                            compound(
                                "lease-renewal",
                                &[
                                    sequence(&client),
                                    op(OP_PUTROOTFH, |_| {}),
                                    op(OP_GETFH, |_| {}),
                                ],
                            ),
                        )
                        .await,
                    );

                    ticks.store(1, Ordering::Release);
                    let mut response = rpc(
                        &mut stream,
                        504,
                        compound("expired-session", &[sequence(&client)]),
                    )
                    .await;
                    assert_eq!(
                        parse_compound_status(&mut response, 0),
                        NFS4ERR_BADSESSION,
                        "dispatch sweeps an expired lease before the next request"
                    );
                    response.end("expired session response").unwrap();
                    assert_eq!(server.v4_session().sweep_expired().await, 0);

                    let second_clientid = parse_exchange(
                        rpc(
                            &mut stream,
                            505,
                            compound(
                                "exchange-second-client",
                                &[exchange_args_for(b"mount-rs-v4-expired")],
                            ),
                        )
                        .await,
                    );
                    let second_session = parse_create_session(
                        rpc(
                            &mut stream,
                            506,
                            compound(
                                "create-second-session",
                                &[create_session_args(second_clientid)],
                            ),
                        )
                        .await,
                    );
                    let second_client = Client {
                        session: second_session,
                        clientid: second_clientid,
                        sequence: 1,
                        slot: 0,
                    };
                    ticks.store(2, Ordering::Release);
                    assert_eq!(server.v4_session().sweep_expired().await, 1);
                    let mut response = rpc(
                        &mut stream,
                        507,
                        compound("explicitly-expired-session", &[sequence(&second_client)]),
                    )
                    .await;
                    assert_eq!(
                        parse_compound_status(&mut response, 0),
                        NFS4ERR_BADSESSION,
                        "an explicit sweep removes the session before the next request"
                    );
                    response.end("explicitly expired session response").unwrap();

                    stream.shutdown().await.expect("close lease test client");
                    server.close().await.expect("close lease test server");
                });
        })
        .expect("spawn v4 lease expiry test thread")
        .join()
        .expect("v4 lease expiry test thread panicked");
}

#[test]
fn nfs_v4_state_limits_are_advertised_and_enforced() {
    std::thread::Builder::new()
        .name("nfs-v4-state-limits-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 state-limit test runtime")
                .block_on(async {
                    let mut options = NfsServerOptions::default();
                    options.session.nfs4.lease_seconds = 7;
                    options.session.nfs4.max_sessions = 1;
                    options.session.nfs4.max_fore_slots = 1;
                    options.session.nfs4.max_operations = 3;
                    options.session.nfs4.max_request_size = 4096;
                    options.session.nfs4.max_cached_response_size = 32;
                    let server = NfsServer::new(MemoryFs::empty(), options);
                    let address = server.listen().await.expect("listen rootless NFS server");
                    let mut stream = TcpStream::connect(address)
                        .await
                        .expect("connect NFS server");

                    let clientid = parse_exchange(
                        rpc(&mut stream, 401, compound("exchange", &[exchange_args()])).await,
                    );
                    let mut response = rpc(
                        &mut stream,
                        402,
                        compound("create-session", &[create_session_args(clientid)]),
                    )
                    .await;
                    parse_compound_header(&mut response, 1);
                    parse_result_header(&mut response, OP_CREATE_SESSION);
                    let session: [u8; 16] = response
                        .fixed_opaque(16, "limited session id")
                        .unwrap()
                        .try_into()
                        .unwrap();
                    assert_eq!(response.u32("limited session sequence").unwrap(), 1);
                    assert_eq!(response.u32("limited session flags").unwrap(), 0);
                    let fore = parse_channel_attrs(&mut response, "limited fore");
                    let back = parse_channel_attrs(&mut response, "limited back");
                    assert_eq!(fore, [0, 4096, 4096, 32, 3, 1]);
                    assert_eq!(back[1..4], [4096, 4096, 32]);
                    response.end("limited create session response").unwrap();

                    let small_clientid = parse_exchange(
                        rpc(
                            &mut stream,
                            403,
                            compound(
                                "exchange-small-response",
                                &[exchange_args_for(b"mount-rs-v4-small-response")],
                            ),
                        )
                        .await,
                    );
                    let mut response = rpc(
                        &mut stream,
                        404,
                        compound(
                            "too-small-response",
                            &[create_session_args_with_response_size(
                                small_clientid,
                                1,
                                64,
                            )],
                        ),
                    )
                    .await;
                    assert_eq!(
                        parse_compound_status(&mut response, 1),
                        NFS4ERR_TOOSMALL,
                        "CREATE_SESSION rejects a fore-channel response too small for SEQUENCE"
                    );
                    assert_eq!(
                        parse_result_status(&mut response, OP_CREATE_SESSION),
                        NFS4ERR_TOOSMALL
                    );
                    response.end("too-small response").unwrap();

                    let mut response = rpc(
                        &mut stream,
                        405,
                        compound(
                            "too-small-replay",
                            &[create_session_args_with_response_size(
                                small_clientid,
                                1,
                                1 << 20,
                            )],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 1), NFS4ERR_TOOSMALL);
                    assert_eq!(
                        parse_result_status(&mut response, OP_CREATE_SESSION),
                        NFS4ERR_TOOSMALL
                    );
                    response.end("too-small replay response").unwrap();

                    let mut small_session_reply = rpc(
                        &mut stream,
                        406,
                        compound(
                            "too-small-retry",
                            &[create_session_args_with_sequence(small_clientid, 2)],
                        ),
                    )
                    .await;
                    let _small_session =
                        parse_create_session_with_sequence(&mut small_session_reply, 2);

                    let mut response = rpc(
                        &mut stream,
                        407,
                        compound(
                            "second-session",
                            &[create_session_args_with_sequence(clientid, 2)],
                        ),
                    )
                    .await;
                    assert_eq!(
                        parse_compound_status(&mut response, 1),
                        NFS4ERR_NOSPC,
                        "maxSessions rejects a second session for one client"
                    );
                    assert_eq!(
                        parse_result_status(&mut response, OP_CREATE_SESSION),
                        NFS4ERR_NOSPC
                    );
                    response.end("second session response").unwrap();

                    let client = Client {
                        session,
                        clientid,
                        sequence: 1,
                        slot: 0,
                    };
                    let mut response = rpc(
                        &mut stream,
                        404,
                        compound(
                            "lease",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_GETATTR, |writer| {
                                    writer.u32(1);
                                    writer.u32(1 << FATTR4_LEASE_TIME);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "lease");
                    parse_result_header(&mut response, OP_PUTROOTFH);
                    parse_result_header(&mut response, OP_GETATTR);
                    assert_eq!(
                        response
                            .array(16, "lease attribute mask", |reader| reader.u32("mask word"))
                            .unwrap(),
                        vec![1 << FATTR4_LEASE_TIME]
                    );
                    let lease = response.var_opaque(16, "lease attribute value").unwrap();
                    assert_eq!(
                        u32::from_be_bytes(lease.try_into().unwrap()),
                        7,
                        "leaseSeconds is visible through FATTR4_LEASE_TIME"
                    );
                    response.end("lease response").unwrap();

                    let mut client = client;
                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        405,
                        compound(
                            "too-many-ops",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                op(OP_GETFH, |_| {}),
                                op(OP_PUTROOTFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(
                        parse_compound_status(&mut response, 0),
                        NFS4ERR_TOO_MANY_OPS,
                        "maxOperations rejects an oversized stateful COMPOUND"
                    );
                    response.end("too many ops response").unwrap();

                    // A failed SEQUENCE must not consume the slot. Reuse the
                    // same sequence for a valid request after each rejection.
                    let mut response = rpc(
                        &mut stream,
                        406,
                        compound("valid-after-too-many", &[sequence(&client)]),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 1), 0);
                    consume_sequence_result(&mut response, "valid after too many");
                    response.end("valid after too many response").unwrap();

                    client.sequence += 1;
                    let high_slot = op(OP_SEQUENCE, |writer| {
                        writer.fixed_opaque(&client.session, 16);
                        writer.u32(client.sequence);
                        writer.u32(client.slot);
                        writer.u32(1); // The one-slot session enforces highest 0.
                        writer.bool(true);
                    });
                    let mut response =
                        rpc(&mut stream, 407, compound("bad-high-slot", &[high_slot])).await;
                    assert_eq!(
                        parse_compound_status(&mut response, 0),
                        NFS4ERR_BAD_HIGH_SLOT
                    );
                    response.end("bad high slot response").unwrap();

                    let mut response = rpc(
                        &mut stream,
                        408,
                        compound("valid-after-high-slot", &[sequence(&client)]),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 1), 0);
                    consume_sequence_result(&mut response, "valid after high slot");
                    response.end("valid after high slot response").unwrap();

                    stream.shutdown().await.expect("close state-limit client");
                    server.close().await.expect("close state-limit server");
                });
        })
        .expect("spawn v4 state-limit test thread")
        .join()
        .expect("v4 state-limit test thread panicked");
}

#[test]
fn nfs_v4_open_same_owner_upgrades_but_cross_client_is_denied() {
    struct HostRoot(PathBuf);

    impl Drop for HostRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.join("share-file"));
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    std::thread::Builder::new()
        .name("nfs-v4-open-share-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 share test runtime")
                .block_on(async {
                    let nonce = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .expect("system clock after Unix epoch")
                        .as_nanos();
                    let root = (0..32)
                        .find_map(|attempt| {
                            let path = std::env::temp_dir().join(format!(
                                "mount-rs-nfs-open-upgrade-{}-{nonce}-{attempt}",
                                std::process::id()
                            ));
                            match std::fs::create_dir(&path) {
                                Ok(()) => Some(HostRoot(path)),
                                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                                    None
                                }
                                Err(error) => panic!("create host-backed NFS root: {error}"),
                            }
                        })
                        .expect("claim a unique host-backed NFS root");
                    let server = NfsServer::new(HostFs::new(&root.0), NfsServerOptions::default());
                    let address = server.listen().await.expect("listen rootless NFS server");
                    let mut stream = TcpStream::connect(address)
                        .await
                        .expect("connect first NFS client");
                    let clientid = parse_exchange(
                        rpc(&mut stream, 101, compound("exchange", &[exchange_args()])).await,
                    );
                    let session = parse_create_session(
                        rpc(
                            &mut stream,
                            102,
                            compound("create-session", &[create_session_args(clientid)]),
                        )
                        .await,
                    );
                    let mut client = Client {
                        session,
                        clientid,
                        sequence: 1,
                        slot: 0,
                    };
                    parse_sequence_and_handle(
                        rpc(
                            &mut stream,
                            103,
                            compound(
                                "root",
                                &[
                                    sequence(&client),
                                    op(OP_PUTROOTFH, |_| {}),
                                    op(OP_GETFH, |_| {}),
                                ],
                            ),
                        )
                        .await,
                    );

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        100,
                        compound(
                            "reclaim-complete",
                            &[
                                sequence(&client),
                                op(OP_RECLAIM_COMPLETE, |writer| writer.bool(false)),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 2);
                    consume_sequence_result(&mut response, "reclaim");
                    parse_result_header(&mut response, OP_RECLAIM_COMPLETE);
                    response.end("reclaim response").unwrap();

                    client.sequence += 1;
                    let open = op(OP_OPEN, |writer| {
                        writer.u32(0);
                        writer.u32(1);
                        writer.u32(0);
                        writer.u64(client.clientid);
                        writer.var_opaque(b"same-owner");
                        writer.u32(1);
                        writer.u32(0);
                        empty_attrs(writer);
                        writer.u32(0);
                        writer.string("share-file");
                    });
                    let mut response = rpc(
                        &mut stream,
                        104,
                        compound(
                            "open",
                            &[
                                sequence(&client),
                                op(OP_PUTROOTFH, |_| {}),
                                open,
                                op(OP_GETFH, |_| {}),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 4);
                    consume_sequence_result(&mut response, "initial open");
                    parse_result_header(&mut response, OP_PUTROOTFH);
                    parse_result_header(&mut response, OP_OPEN);
                    let initial_stateid = response.fixed_opaque(16, "initial stateid").unwrap();
                    assert_eq!(
                        u32::from_be_bytes(initial_stateid[..4].try_into().unwrap()),
                        1
                    );
                    let _ = response.bool("initial cinfo atomic").unwrap();
                    let _ = response.u64("initial cinfo before").unwrap();
                    let _ = response.u64("initial cinfo after").unwrap();
                    let _ = response.u32("initial rflags").unwrap();
                    let _ =
                        response.array(16, "initial attrset", |reader| reader.u32("attrset word"));
                    assert_eq!(response.u32("initial delegation").unwrap(), 0);
                    parse_result_header(&mut response, OP_GETFH);
                    let file_handle = response.var_opaque(128, "share file handle").unwrap();
                    response.end("initial open response").unwrap();

                    // The same client and open owner may widen the existing
                    // reservation.  The resulting stateid is the same state
                    // with a fresh seqid, rather than SHARE_DENIED.
                    client.sequence += 1;
                    let same_owner_upgrade = op(OP_OPEN, |writer| {
                        writer.u32(0);
                        writer.u32(3);
                        writer.u32(1);
                        writer.u64(client.clientid);
                        writer.var_opaque(b"same-owner");
                        writer.u32(0);
                        writer.u32(CLAIM_FH);
                    });
                    let mut response = rpc(
                        &mut stream,
                        105,
                        compound(
                            "same-owner-upgrade",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                same_owner_upgrade,
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "same-owner upgrade");
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_OPEN);
                    let upgraded_stateid = response.fixed_opaque(16, "upgraded stateid").unwrap();
                    assert_eq!(
                        u32::from_be_bytes(upgraded_stateid[..4].try_into().unwrap()),
                        2,
                        "same-owner OPEN widens the state and bumps its seqid"
                    );
                    let _ = response.bool("upgrade cinfo atomic").unwrap();
                    let _ = response.u64("upgrade cinfo before").unwrap();
                    let _ = response.u64("upgrade cinfo after").unwrap();
                    let _ = response.u32("upgrade rflags").unwrap();
                    let _ =
                        response.array(16, "upgrade attrset", |reader| reader.u32("attrset word"));
                    assert_eq!(response.u32("upgrade delegation").unwrap(), 0);
                    response.end("same-owner upgrade response").unwrap();

                    // HostFs uses real open flags. Widening only the NFS state
                    // while keeping the original read-only descriptor would
                    // make this WRITE fail despite the successful OPEN.
                    client.sequence += 1;
                    let payload = b"upgraded owner may write";
                    let mut response = rpc(
                        &mut stream,
                        106,
                        compound(
                            "write-after-upgrade",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_WRITE, |writer| {
                                    writer.fixed_opaque(&upgraded_stateid, 16);
                                    writer.u64(0);
                                    writer.u32(UNSTABLE4);
                                    writer.var_opaque(payload);
                                }),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 3), 0);
                    consume_sequence_result(&mut response, "write after upgrade");
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_WRITE);
                    assert_eq!(
                        response.u32("upgraded write count").unwrap(),
                        payload.len() as u32
                    );
                    let _ = response.u32("upgraded write stability").unwrap();
                    let _ = response.fixed_opaque(8, "upgraded write verifier").unwrap();
                    response.end("write after upgrade response").unwrap();
                    assert_eq!(std::fs::read(root.0.join("share-file")).unwrap(), payload);

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        107,
                        compound(
                            "read-after-upgrade",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_READ, |writer| {
                                    writer.fixed_opaque(&upgraded_stateid, 16);
                                    writer.u64(0);
                                    writer.u32(payload.len() as u32);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "read after upgrade");
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_READ);
                    assert!(response.bool("upgraded read eof").unwrap());
                    assert_eq!(
                        response.var_opaque(128, "upgraded read data").unwrap(),
                        payload
                    );
                    response.end("read after upgrade response").unwrap();

                    let mut stream_two = TcpStream::connect(address)
                        .await
                        .expect("connect second NFS client");
                    let clientid_two = parse_exchange(
                        rpc(
                            &mut stream_two,
                            201,
                            compound(
                                "exchange-two",
                                &[exchange_args_for(b"mount-rs-v4-wire-two")],
                            ),
                        )
                        .await,
                    );
                    let session_two = parse_create_session(
                        rpc(
                            &mut stream_two,
                            202,
                            compound("create-session-two", &[create_session_args(clientid_two)]),
                        )
                        .await,
                    );
                    let mut client_two = Client {
                        session: session_two,
                        clientid: clientid_two,
                        sequence: 1,
                        slot: 0,
                    };
                    parse_sequence_and_handle(
                        rpc(
                            &mut stream_two,
                            203,
                            compound(
                                "root-two",
                                &[
                                    sequence(&client_two),
                                    op(OP_PUTROOTFH, |_| {}),
                                    op(OP_GETFH, |_| {}),
                                ],
                            ),
                        )
                        .await,
                    );

                    client_two.sequence += 1;
                    let mut response = rpc(
                        &mut stream_two,
                        200,
                        compound(
                            "reclaim-complete-two",
                            &[
                                sequence(&client_two),
                                op(OP_RECLAIM_COMPLETE, |writer| writer.bool(false)),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 2);
                    consume_sequence_result(&mut response, "reclaim-two");
                    parse_result_header(&mut response, OP_RECLAIM_COMPLETE);
                    response.end("reclaim-two response").unwrap();

                    client_two.sequence += 1;
                    let cross_client_open = op(OP_OPEN, |writer| {
                        writer.u32(0);
                        writer.u32(1);
                        writer.u32(0);
                        writer.u64(client_two.clientid);
                        writer.var_opaque(b"same-owner");
                        writer.u32(0);
                        writer.u32(CLAIM_FH);
                    });
                    let mut response = rpc(
                        &mut stream_two,
                        204,
                        compound(
                            "cross-client-deny",
                            &[
                                sequence(&client_two),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                cross_client_open,
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(
                        parse_compound_status(&mut response, 3),
                        NFS4ERR_SHARE_DENIED
                    );
                    consume_sequence_result(&mut response, "cross-client deny");
                    parse_result_header(&mut response, OP_PUTFH);
                    assert_eq!(
                        parse_result_status(&mut response, OP_OPEN),
                        NFS4ERR_SHARE_DENIED,
                        "a different client cannot bypass the upgraded deny reservation"
                    );
                    response.end("cross-client deny response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        108,
                        compound(
                            "close-upgraded-open",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_CLOSE, |writer| {
                                    writer.u32(1);
                                    writer.fixed_opaque(&upgraded_stateid, 16);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "close upgraded open");
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_CLOSE);
                    let _ = response.fixed_opaque(16, "closed stateid").unwrap();
                    response.end("close upgraded open response").unwrap();

                    // Reverse the upgrade direction on the same host file:
                    // O_WRONLY first, then a same-owner OPEN adds READ.
                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        109,
                        compound(
                            "write-only-open",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_OPEN, |writer| {
                                    writer.u32(0);
                                    writer.u32(2);
                                    writer.u32(0);
                                    writer.u64(client.clientid);
                                    writer.var_opaque(b"reverse-owner");
                                    writer.u32(0);
                                    writer.u32(CLAIM_FH);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "write-only open");
                    parse_result_header(&mut response, OP_PUTFH);
                    let _write_only_stateid = consume_open_result(&mut response, "write-only");
                    response.end("write-only open response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        110,
                        compound(
                            "read-upgrade",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_OPEN, |writer| {
                                    writer.u32(0);
                                    writer.u32(1);
                                    writer.u32(0);
                                    writer.u64(client.clientid);
                                    writer.var_opaque(b"reverse-owner");
                                    writer.u32(0);
                                    writer.u32(CLAIM_FH);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "read upgrade");
                    parse_result_header(&mut response, OP_PUTFH);
                    let read_upgraded_stateid = consume_open_result(&mut response, "read-upgraded");
                    response.end("read upgrade response").unwrap();

                    client.sequence += 1;
                    let mut response = rpc(
                        &mut stream,
                        111,
                        compound(
                            "read-after-write-only-upgrade",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_READ, |writer| {
                                    writer.fixed_opaque(&read_upgraded_stateid, 16);
                                    writer.u64(0);
                                    writer.u32(payload.len() as u32);
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_compound_header(&mut response, 3);
                    consume_sequence_result(&mut response, "reverse upgrade read");
                    parse_result_header(&mut response, OP_PUTFH);
                    parse_result_header(&mut response, OP_READ);
                    assert!(response.bool("reverse upgrade eof").unwrap());
                    assert_eq!(
                        response.var_opaque(128, "reverse upgrade data").unwrap(),
                        payload
                    );
                    response.end("reverse upgrade read response").unwrap();

                    let _ = initial_stateid;
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn v4 share test thread")
        .join()
        .expect("v4 share test thread panicked");
}

#[test]
fn nfs_v4_independent_opens_progress_while_one_backend_open_is_blocked() {
    std::thread::Builder::new()
        .name("nfs-v4-independent-opens".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let entered = Arc::new(Notify::new());
                    let release = Arc::new(Notify::new());
                    let server = NfsServer::new(
                        GateOneOpenDriver {
                            inner: MemoryFs::empty(),
                            entered: Arc::clone(&entered),
                            release: Arc::clone(&release),
                        },
                        NfsServerOptions::default(),
                    );
                    let address = server.listen().await.unwrap();
                    let (mut first_stream, mut first_client) =
                        connect_v4_client(address, 501, b"blocked-open-client").await;
                    let (mut second_stream, mut second_client) =
                        connect_v4_client(address, 601, b"independent-open-client").await;
                    let first = tokio::spawn(async move {
                        let mut xid = 504;
                        concurrent_file_round_trip(
                            &mut first_stream,
                            &mut first_client,
                            &mut xid,
                            b"blocked-open-owner",
                            "blocked-open.txt",
                            b"blocked open payload",
                        )
                        .await
                    });
                    timeout(Duration::from_millis(500), entered.notified())
                        .await
                        .expect("first OPEN reached blocked backend");
                    let mut xid = 604;
                    timeout(
                        Duration::from_millis(250),
                        concurrent_file_round_trip(
                            &mut second_stream,
                            &mut second_client,
                            &mut xid,
                            b"independent-open-owner",
                            "independent-open.txt",
                            b"independent open payload",
                        ),
                    )
                    .await
                    .expect("an independent OPEN must complete before the blocked one is released");
                    release.notify_one();
                    first.await.expect("blocked OPEN task");
                    server.close().await.unwrap();
                });
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn nfs_v4_clients_round_trip_distinct_files_concurrently() {
    std::thread::Builder::new()
        .name("nfs-v4-concurrency-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(4)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build v4 concurrency test runtime")
                .block_on(async {
                    let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
                    let address = server.listen().await.expect("listen concurrent NFS server");
                    let (mut first, mut first_client) =
                        connect_v4_client(address, 401, b"concurrent-client-one").await;
                    let (mut second, mut second_client) =
                        connect_v4_client(address, 501, b"concurrent-client-two").await;
                    assert_eq!(server.connections(), 2);

                    let mut first_xid = 403;
                    let mut second_xid = 503;
                    let (first_data, second_data) = tokio::join!(
                        concurrent_file_round_trip(
                            &mut first,
                            &mut first_client,
                            &mut first_xid,
                            b"concurrent-owner-one",
                            "concurrent-one.txt",
                            b"first concurrent payload",
                        ),
                        concurrent_file_round_trip(
                            &mut second,
                            &mut second_client,
                            &mut second_xid,
                            b"concurrent-owner-two",
                            "concurrent-two.txt",
                            b"second concurrent payload",
                        ),
                    );
                    assert_eq!(first_data, b"first concurrent payload");
                    assert_eq!(second_data, b"second concurrent payload");

                    first
                        .shutdown()
                        .await
                        .expect("close first concurrent client");
                    second
                        .shutdown()
                        .await
                        .expect("close second concurrent client");
                    timeout(Duration::from_secs(2), async {
                        while server.connections() != 0 {
                            tokio::task::yield_now().await;
                        }
                    })
                    .await
                    .expect("concurrent NFS clients close");
                    server.close().await.expect("close concurrent NFS server");
                });
        })
        .expect("spawn v4 concurrency test thread")
        .join()
        .expect("v4 concurrency test thread panicked");
}
