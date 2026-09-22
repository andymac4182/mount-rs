//! NFSv4.1 COMMIT durability-barrier regression.
//!
//! This deliberately uses a fault-injecting driver wrapper.  MemoryFs makes
//! successful writes immediately visible, so a success-only wire test cannot
//! distinguish COMMIT from a stat-only implementation.

use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::Duration;

use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, FsError, Result, Stats,
};
use mount_rs_memfs::MemoryFs;
use mount_rs_nfs::v4::{
    CREATE_SESSION4_FLAG_CONN_BACK_CHAN, NFS_V4, NFS4_PROGRAM, NFS4ERR_IO, NFS4ERR_PERM, OP_COMMIT,
    OP_CREATE_SESSION, OP_EXCHANGE_ID, OP_GETFH, OP_OPEN, OP_PUTFH, OP_PUTROOTFH,
    OP_RECLAIM_COMPLETE, OP_SEQUENCE, OP_WRITE, UNSTABLE4,
};
use mount_rs_nfs::{
    NfsServer, NfsServerOptions, RecordAssembler, XdrReader, XdrWriter, decode_reply, encode_call,
    frame_record,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::runtime::Builder;

const CLAIM_NULL: u32 = 0;
const OPEN4_CREATE: u32 = 1;
const UNCHECKED4: u32 = 0;
const OPEN4_SHARE_ACCESS_BOTH: u32 = 3;

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Default)]
struct FaultControls {
    sync_calls: Arc<AtomicUsize>,
    close_calls: Arc<AtomicUsize>,
    fail_sync: Arc<AtomicBool>,
    fail_close: Arc<AtomicBool>,
}

struct CountingDriver {
    inner: MemoryFs,
    controls: FaultControls,
}

struct CountingHandle {
    inner: Arc<dyn FileHandle>,
    controls: FaultControls,
}

impl CountingDriver {
    fn new(inner: MemoryFs, controls: FaultControls) -> Self {
        Self { inner, controls }
    }
}

impl FsDriver for CountingDriver {
    fn capabilities(&self) -> Capabilities {
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
        Box::pin(async move {
            let inner = self.inner.open(path, flags, mode).await?;
            Ok(Arc::new(CountingHandle {
                inner,
                controls: self.controls.clone(),
            }) as Arc<dyn FileHandle>)
        })
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
            let inner = self.inner.open_flags(path, flags, mode).await?;
            Ok(Arc::new(CountingHandle {
                inner,
                controls: self.controls.clone(),
            }) as Arc<dyn FileHandle>)
        })
    }
}

impl FileHandle for CountingHandle {
    fn read<'a, 'b, 'async_trait>(
        &'a self,
        buffer: &'b mut [u8],
        position: Option<u64>,
    ) -> BoxFuture<'async_trait, Result<usize>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.read(buffer, position).await })
    }

    fn write<'a, 'b, 'async_trait>(
        &'a self,
        buffer: &'b [u8],
        position: Option<u64>,
    ) -> BoxFuture<'async_trait, Result<usize>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.write(buffer, position).await })
    }

    fn stat<'a, 'async_trait>(&'a self) -> BoxFuture<'async_trait, Result<Stats>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.stat().await })
    }

    fn truncate<'a, 'async_trait>(&'a self, length: u64) -> BoxFuture<'async_trait, Result<()>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.truncate(length).await })
    }

    fn sync<'a, 'async_trait>(&'a self) -> BoxFuture<'async_trait, Result<()>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.controls.sync_calls.fetch_add(1, Ordering::SeqCst);
            if self.controls.fail_sync.swap(false, Ordering::SeqCst) {
                return Err(FsError::new(ErrorCode::Eio));
            }
            self.inner.sync().await
        })
    }

    fn close<'a, 'async_trait>(&'a self) -> BoxFuture<'async_trait, Result<()>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.controls.close_calls.fetch_add(1, Ordering::SeqCst);
            if self.controls.fail_close.swap(false, Ordering::SeqCst) {
                return Err(FsError::new(ErrorCode::Eperm));
            }
            self.inner.close().await
        })
    }
}

#[derive(Clone)]
struct Client {
    session: [u8; 16],
    clientid: u64,
    sequence: u32,
}

async fn rpc(stream: &mut TcpStream, xid: u32, args: Vec<u8>) -> XdrReader<'static> {
    let call = encode_call(xid, NFS4_PROGRAM, NFS_V4, 1, None, None, &args);
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
    XdrReader::new(Box::leak(bytes.into_boxed_slice()))
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
        writer.u32(0);
        writer.u32(0);
        writer.bool(true);
    })
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

fn parse_result_status(reader: &mut XdrReader<'_>, expected_op: u32) -> u32 {
    assert_eq!(reader.u32("result op").unwrap(), expected_op);
    reader.u32("result status").unwrap()
}

fn consume_sequence(reader: &mut XdrReader<'_>) {
    assert_eq!(parse_result_status(reader, OP_SEQUENCE), 0);
    let _ = reader.fixed_opaque(16, "sequence session id").unwrap();
    let _ = reader.u32("sequence number").unwrap();
    let _ = reader.u32("sequence slot").unwrap();
    let _ = reader.u32("sequence highest slot").unwrap();
    let _ = reader.u32("sequence target slot").unwrap();
    let _ = reader.u32("sequence status flags").unwrap();
}

fn exchange_args() -> Vec<u8> {
    op(OP_EXCHANGE_ID, |writer| {
        writer.fixed_opaque(b"barrier!", 8);
        writer.var_opaque(b"mount-rs-v4-commit-barrier");
        writer.u32(0);
        writer.u32(0);
        writer.u32(0);
    })
}

fn parse_exchange(mut reader: XdrReader<'_>) -> u64 {
    assert_eq!(parse_compound_status(&mut reader, 1), 0);
    assert_eq!(parse_result_status(&mut reader, OP_EXCHANGE_ID), 0);
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

fn create_session_args(clientid: u64) -> Vec<u8> {
    op(OP_CREATE_SESSION, |writer| {
        writer.u64(clientid);
        writer.u32(1);
        writer.u32(CREATE_SESSION4_FLAG_CONN_BACK_CHAN);
        channel(writer);
        channel(writer);
        writer.u32(0);
        writer.u32(1);
        writer.u32(0);
    })
}

fn parse_create_session(mut reader: XdrReader<'_>) -> [u8; 16] {
    assert_eq!(parse_compound_status(&mut reader, 1), 0);
    assert_eq!(parse_result_status(&mut reader, OP_CREATE_SESSION), 0);
    let session: [u8; 16] = reader
        .fixed_opaque(16, "session id")
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(reader.u32("create session sequence").unwrap(), 1);
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

fn parse_open(mut reader: XdrReader<'_>) -> ([u8; 16], Vec<u8>) {
    assert_eq!(parse_compound_status(&mut reader, 4), 0);
    consume_sequence(&mut reader);
    assert_eq!(parse_result_status(&mut reader, OP_PUTROOTFH), 0);
    assert_eq!(parse_result_status(&mut reader, OP_OPEN), 0);
    let stateid = reader.fixed_opaque(16, "open stateid").unwrap();
    assert_eq!(u32::from_be_bytes(stateid[..4].try_into().unwrap()), 1);
    let _ = reader.bool("open cinfo atomic").unwrap();
    let _ = reader.u64("open cinfo before").unwrap();
    let _ = reader.u64("open cinfo after").unwrap();
    let _ = reader.u32("open rflags").unwrap();
    let _ = reader.array(16, "open attrset", |reader| reader.u32("attrset word"));
    assert_eq!(reader.u32("open delegation").unwrap(), 0);
    assert_eq!(parse_result_status(&mut reader, OP_GETFH), 0);
    let handle = reader.var_opaque(128, "file handle").unwrap();
    reader.end("open response").unwrap();
    (stateid.try_into().unwrap(), handle)
}

fn parse_write(mut reader: XdrReader<'_>) {
    assert_eq!(parse_compound_status(&mut reader, 3), 0);
    consume_sequence(&mut reader);
    assert_eq!(parse_result_status(&mut reader, OP_PUTFH), 0);
    assert_eq!(parse_result_status(&mut reader, OP_WRITE), 0);
    assert_eq!(reader.u32("write count").unwrap(), 12);
    assert_eq!(reader.u32("write committed").unwrap(), UNSTABLE4);
    let _ = reader.fixed_opaque(8, "write verifier").unwrap();
    reader.end("write response").unwrap();
}

async fn commit(
    stream: &mut TcpStream,
    client: &mut Client,
    xid: u32,
    file_handle: &[u8],
    offset: u64,
    count: u32,
) -> u32 {
    client.sequence += 1;
    let mut reader = rpc(
        stream,
        xid,
        compound(
            "commit",
            &[
                sequence(client),
                op(OP_PUTFH, |writer| writer.var_opaque(file_handle)),
                op(OP_COMMIT, |writer| {
                    writer.u64(offset);
                    writer.u32(count);
                }),
            ],
        ),
    )
    .await;
    let status = parse_compound_status(&mut reader, 3);
    consume_sequence(&mut reader);
    assert_eq!(parse_result_status(&mut reader, OP_PUTFH), 0);
    let commit_status = parse_result_status(&mut reader, OP_COMMIT);
    assert_eq!(status, commit_status);
    if commit_status == 0 {
        let _ = reader.fixed_opaque(8, "commit verifier").unwrap();
    }
    reader.end("commit response").unwrap();
    commit_status
}

#[test]
fn nfs_v4_unstable_write_defers_flush_and_commit_propagates_barrier_faults() {
    std::thread::Builder::new()
        .name("nfs-v4-commit-barrier-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build COMMIT barrier test runtime")
                .block_on(async {
                    let controls = FaultControls::default();
                    let server = NfsServer::new(
                        CountingDriver::new(MemoryFs::empty(), controls.clone()),
                        NfsServerOptions::default(),
                    );
                    let address = server.listen().await.expect("listen NFS server");
                    let mut stream = TcpStream::connect(address)
                        .await
                        .expect("connect NFS client");

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
                    let mut client = Client {
                        session,
                        clientid,
                        sequence: 1,
                    };

                    let mut response = rpc(
                        &mut stream,
                        3,
                        compound(
                            "reclaim-complete",
                            &[
                                sequence(&client),
                                op(OP_RECLAIM_COMPLETE, |writer| writer.bool(false)),
                            ],
                        ),
                    )
                    .await;
                    assert_eq!(parse_compound_status(&mut response, 2), 0);
                    consume_sequence(&mut response);
                    assert_eq!(parse_result_status(&mut response, OP_RECLAIM_COMPLETE), 0);
                    response.end("reclaim-complete response").unwrap();
                    client.sequence += 1;

                    let open = op(OP_OPEN, |writer| {
                        writer.u32(0);
                        writer.u32(OPEN4_SHARE_ACCESS_BOTH);
                        writer.u32(0);
                        writer.u64(client.clientid);
                        writer.var_opaque(b"commit-barrier-owner");
                        writer.u32(OPEN4_CREATE);
                        writer.u32(UNCHECKED4);
                        writer.u32(0);
                        writer.u32(0);
                        writer.u32(CLAIM_NULL);
                        writer.string("commit-barrier-file");
                    });
                    let (stateid, file_handle) = parse_open(
                        rpc(
                            &mut stream,
                            4,
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
                        .await,
                    );

                    client.sequence += 1;
                    let response = rpc(
                        &mut stream,
                        5,
                        compound(
                            "unstable-write",
                            &[
                                sequence(&client),
                                op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                                op(OP_WRITE, |writer| {
                                    writer.fixed_opaque(&stateid, 16);
                                    writer.u64(0);
                                    writer.u32(UNSTABLE4);
                                    writer.var_opaque(b"barrier-data");
                                }),
                            ],
                        ),
                    )
                    .await;
                    parse_write(response);
                    assert_eq!(
                        controls.sync_calls.load(Ordering::SeqCst),
                        0,
                        "UNSTABLE4 WRITE must not flush"
                    );

                    controls.fail_sync.store(true, Ordering::SeqCst);
                    let close_before_sync_fault = controls.close_calls.load(Ordering::SeqCst);
                    assert_eq!(
                        commit(&mut stream, &mut client, 6, &file_handle, 4, 8).await,
                        NFS4ERR_IO,
                        "injected sync error must reach the COMMIT result"
                    );
                    assert_eq!(controls.sync_calls.load(Ordering::SeqCst), 1);
                    assert_eq!(
                        controls.close_calls.load(Ordering::SeqCst),
                        close_before_sync_fault + 1,
                        "COMMIT closes its handle even after sync fails"
                    );

                    controls.fail_close.store(true, Ordering::SeqCst);
                    let close_before_close_fault = controls.close_calls.load(Ordering::SeqCst);
                    assert_eq!(
                        commit(&mut stream, &mut client, 7, &file_handle, 4, 8).await,
                        NFS4ERR_PERM,
                        "injected close error must reach the COMMIT result"
                    );
                    assert_eq!(controls.sync_calls.load(Ordering::SeqCst), 2);
                    assert_eq!(
                        controls.close_calls.load(Ordering::SeqCst),
                        close_before_close_fault + 1
                    );

                    assert_eq!(
                        commit(&mut stream, &mut client, 8, &file_handle, 4, 8).await,
                        0,
                        "a later COMMIT succeeds after one-shot faults"
                    );
                    assert_eq!(controls.sync_calls.load(Ordering::SeqCst), 3);
                    server.close().await.expect("close NFS server");
                });
        })
        .expect("spawn COMMIT barrier test thread")
        .join()
        .expect("COMMIT barrier test thread panicked");
}
