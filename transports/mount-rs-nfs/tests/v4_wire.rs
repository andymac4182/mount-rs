//! Rootless NFSv4.1 wire integration.
//!
//! This exercises the TCP record-marking server, the v4.1 client/session
//! handshake, slot sequencing, file handles, OPEN/READ/WRITE/CLOSE and
//! namespace cleanup. It also drives two independent v4.1 sessions through
//! concurrent file round trips. It deliberately does not invoke the host
//! kernel mount client; native mount prerequisites are platform- and
//! privilege-specific.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use mount_rs_core::{DirEntry, FileHandle, FsDriver, MemoryFs, Result, Stats};
use mount_rs_nfs::v4::{
    CLAIM_FH, CLAIM_NULL, CREATE_SESSION4_FLAG_CONN_BACK_CHAN, FATTR4_LEASE_TIME,
    NFS4ERR_BADSESSION, NFS4ERR_DELAY, NFS4ERR_GRACE, NFS4ERR_NOSPC, NFS4ERR_RESOURCE,
    NFS4ERR_SEQ_FALSE_RETRY, NFS4ERR_SEQ_MISORDERED, NFS4ERR_SHARE_DENIED, NFS4ERR_TOO_MANY_OPS,
    NFS4ERR_TOOSMALL, OPEN4_CREATE, OPEN4_SHARE_ACCESS_BOTH, UNCHECKED4, UNSTABLE4,
};
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

const OP_CLOSE: u32 = 4;
const OP_COMMIT: u32 = 5;
const OP_BACKCHANNEL_CTL: u32 = 40;
const OP_GETATTR: u32 = 9;
const OP_GETFH: u32 = 10;
const OP_LOCK: u32 = 12;
const OP_LOCKU: u32 = 14;
const OP_OPEN: u32 = 18;
const OP_OPEN_DOWNGRADE: u32 = 21;
const OP_PUTFH: u32 = 22;
const OP_PUTROOTFH: u32 = 24;
const OP_READ: u32 = 25;
const OP_REMOVE: u32 = 28;
const OP_WRITE: u32 = 38;
const OP_EXCHANGE_ID: u32 = 42;
const OP_CREATE_SESSION: u32 = 43;
const OP_FREE_STATEID: u32 = 45;
const OP_RECLAIM_COMPLETE: u32 = 58;
const OP_SEQUENCE: u32 = 53;

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
    let call = encode_call(xid, NFS4_PROGRAM, NFS_V4, 1, credential, None, &args);
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
    op(OP_SEQUENCE, |writer| {
        writer.fixed_opaque(&client.session, 16);
        writer.u32(client.sequence);
        writer.u32(client.slot);
        writer.u32(0);
        writer.bool(true);
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
                    let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
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

                    let _ = initial_stateid;
                    let _ = upgraded_stateid;
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn v4 share test thread")
        .join()
        .expect("v4 share test thread panicked");
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
