//! Cross-process NFSv3 crash/restart evidence with a persistent host backend.
//!
//! The child server is terminated without running its async shutdown path.
//! The replacement server must still expose the file written through the NFS
//! wire before the crash. This deliberately qualifies backend data recovery;
//! it does not claim durable NFSv4 lease, replay, or file-handle state.

#![cfg(unix)]

use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mount_rs_host::HostFs;
use mount_rs_nfs::constants::{
    CREATE_UNCHECKED, FILE_SYNC, MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_MNT, NFS_PROGRAM, NFS_V3,
    NFS3_OK, NFS3ERR_STALE, NFSPROC3_CREATE, NFSPROC3_LOOKUP, NFSPROC3_READ, NFSPROC3_WRITE,
};
use mount_rs_nfs::protocol::{
    Create3args, DirOpArgs, Read3args, Read3res, Sattr3, Write3args, read_create_res,
    read_lookup_res, read_mount_res, read_read_res, read_write_res, write_create_args,
    write_dir_op, write_read_args, write_write_args,
};
use mount_rs_nfs::rpc::{RPC_SUCCESS, RecordAssembler, decode_reply, encode_call, frame_record};
use mount_rs_nfs::v4::{CREATE_SESSION4_FLAG_CONN_BACK_CHAN, NFS4ERR_BADSESSION};
use mount_rs_nfs::xdr::{XdrReader, XdrWriter, encode_xdr};
use mount_rs_nfs::{NFS_V4, NFS4_PROGRAM, NfsServer, NfsServerOptions};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::timeout;

const CHILD_ENV: &str = "MOUNT_RS_NFS_PROCESS_CHILD";
const ROOT_ENV: &str = "MOUNT_RS_NFS_PROCESS_ROOT";
const TEST_NAME: &str = "nfs_v3_host_backend_survives_process_crash_and_restart";
const V4_TEST_NAME: &str = "nfs_v4_session_is_process_local_after_process_crash";

const OP_GETFH: u32 = 10;
const OP_PUTROOTFH: u32 = 24;
const OP_EXCHANGE_ID: u32 = 42;
const OP_CREATE_SESSION: u32 = 43;
const OP_SEQUENCE: u32 = 53;

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "mount-rs-nfs-process-restart-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create process-restart backend root");
        Self(path)
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ServerProcess {
    child: Child,
    address: SocketAddr,
}

impl ServerProcess {
    async fn crash(mut self) {
        self.child
            .kill()
            .await
            .expect("force-terminate NFS child server");
        let status = timeout(Duration::from_secs(5), self.child.wait())
            .await
            .expect("NFS child exits after forced termination")
            .expect("wait for NFS child");
        assert!(
            !status.success(),
            "forced NFS child termination must not be reported as graceful"
        );
    }
}

async fn start_child(root: &Path, role: &str) -> ServerProcess {
    start_child_for(root, role, TEST_NAME).await
}

async fn start_child_for(root: &Path, role: &str, test_name: &str) -> ServerProcess {
    let mut child = Command::new(std::env::current_exe().expect("current test executable"))
        .args(["--exact", test_name, "--nocapture"])
        .env(CHILD_ENV, role)
        .env(ROOT_ENV, root)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn NFS child server");
    let stdout = child.stdout.take().expect("NFS child stdout");
    let mut lines = AsyncBufReader::new(stdout).lines();
    let address = timeout(Duration::from_secs(5), async {
        loop {
            let line = lines.next_line().await?.ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "missing READY line")
            })?;
            let Some(port) = line.strip_prefix("READY ") else {
                continue;
            };
            let port = port
                .parse::<u16>()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            return Ok::<SocketAddr, io::Error>(SocketAddr::from(([127, 0, 0, 1], port)));
        }
    })
    .await
    .expect("NFS child becomes ready")
    .expect("NFS child readiness output");
    ServerProcess { child, address }
}

async fn exchange(stream: &mut TcpStream, call: Vec<u8>) -> Vec<u8> {
    stream
        .write_all(&frame_record(&call).expect("frame RPC call"))
        .await
        .expect("write RPC call");
    let mut assembler = RecordAssembler::default();
    let mut buffer = [0_u8; 64 * 1024];
    timeout(Duration::from_secs(5), async {
        loop {
            let count = stream.read(&mut buffer).await.expect("read RPC reply");
            assert!(count > 0, "server closed before RPC reply");
            if let Some(record) = assembler
                .push(&buffer[..count])
                .expect("assemble RPC reply")
                .into_iter()
                .next()
            {
                return record;
            }
        }
    })
    .await
    .expect("RPC reply timeout")
}

fn v4_op(opcode: u32, body: impl FnOnce(&mut XdrWriter)) -> Vec<u8> {
    let mut writer = XdrWriter::new();
    writer.u32(opcode);
    body(&mut writer);
    writer.into_bytes()
}

fn v4_compound(tag: &str, operations: &[Vec<u8>]) -> Vec<u8> {
    let mut writer = XdrWriter::new();
    writer.string(tag);
    writer.u32(1);
    writer.u32(operations.len() as u32);
    for operation in operations {
        writer.raw(operation);
    }
    writer.into_bytes()
}

fn v4_sequence(session: &[u8; 16], sequence: u32) -> Vec<u8> {
    v4_op(OP_SEQUENCE, |writer| {
        writer.fixed_opaque(session, 16);
        writer.u32(sequence);
        writer.u32(0);
        writer.u32(0);
        writer.bool(true);
    })
}

fn v4_channel(writer: &mut XdrWriter) {
    writer.u32(0);
    writer.u32(1 << 20);
    writer.u32(1 << 20);
    writer.u32(1 << 20);
    writer.u32(64);
    writer.u32(4);
    writer.u32(0);
}

fn v4_reader(record: &[u8]) -> XdrReader<'static> {
    let (reply, mut body) = decode_reply(record).expect("decode NFSv4 reply");
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    XdrReader::new(Box::leak(body.rest().to_vec().into_boxed_slice()))
}

fn v4_compound_status(reader: &mut XdrReader<'_>, expected_count: usize) -> u32 {
    let status = reader
        .u32("NFSv4 compound status")
        .expect("read compound status");
    let _ = reader
        .string(1024, "NFSv4 compound tag")
        .expect("read compound tag");
    assert_eq!(
        reader
            .u32("NFSv4 compound result count")
            .expect("read compound result count") as usize,
        expected_count
    );
    status
}

fn v4_result_status(reader: &mut XdrReader<'_>, expected_op: u32) -> u32 {
    assert_eq!(
        reader
            .u32("NFSv4 result operation")
            .expect("read result operation"),
        expected_op
    );
    reader
        .u32("NFSv4 result status")
        .expect("read result status")
}

fn v4_consume_sequence(reader: &mut XdrReader<'_>) {
    assert_eq!(v4_result_status(reader, OP_SEQUENCE), 0);
    let _ = reader
        .fixed_opaque(16, "NFSv4 sequence session")
        .expect("read sequence session");
    for label in [
        "sequence number",
        "sequence slot",
        "sequence highest slot",
        "sequence target slot",
        "sequence status flags",
    ] {
        let _ = reader.u32(label).expect("read sequence result");
    }
}

fn v4_exchange_args(owner: &[u8]) -> Vec<u8> {
    v4_op(OP_EXCHANGE_ID, |writer| {
        writer.fixed_opaque(b"v4-crash", 8);
        writer.var_opaque(owner);
        writer.u32(0);
        writer.u32(0);
        writer.u32(0);
    })
}

fn v4_create_session_args(clientid: u64) -> Vec<u8> {
    v4_op(OP_CREATE_SESSION, |writer| {
        writer.u64(clientid);
        writer.u32(1);
        writer.u32(CREATE_SESSION4_FLAG_CONN_BACK_CHAN);
        v4_channel(writer);
        v4_channel(writer);
        writer.u32(0);
        writer.u32(1);
        writer.u32(0);
    })
}

async fn establish_v4_session(stream: &mut TcpStream, xid: u32) -> [u8; 16] {
    let exchange_record = exchange(
        stream,
        encode_call(
            xid,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound("crash-exchange", &[v4_exchange_args(b"crash-client")]),
        ),
    )
    .await;
    let mut response = v4_reader(&exchange_record);
    assert_eq!(v4_compound_status(&mut response, 1), 0);
    assert_eq!(v4_result_status(&mut response, OP_EXCHANGE_ID), 0);
    let clientid = response.u64("NFSv4 client id").expect("read client id");
    assert_eq!(response.u32("NFSv4 exchange sequence").unwrap(), 1);
    let _ = response.u32("NFSv4 exchange flags").unwrap();
    assert_eq!(response.u32("NFSv4 state protection").unwrap(), 0);
    let _ = response.u64("NFSv4 server owner minor id").unwrap();
    let _ = response
        .var_opaque(1024, "NFSv4 server owner major id")
        .unwrap();
    let _ = response.var_opaque(1024, "NFSv4 server scope").unwrap();
    assert_eq!(response.u32("NFSv4 implementation count").unwrap(), 0);
    response.end("NFSv4 exchange response").unwrap();

    let create_session_record = exchange(
        stream,
        encode_call(
            xid + 1,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound("crash-create-session", &[v4_create_session_args(clientid)]),
        ),
    )
    .await;
    let mut response = v4_reader(&create_session_record);
    assert_eq!(v4_compound_status(&mut response, 1), 0);
    assert_eq!(v4_result_status(&mut response, OP_CREATE_SESSION), 0);
    let session: [u8; 16] = response
        .fixed_opaque(16, "NFSv4 session id")
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(response.u32("NFSv4 create-session sequence").unwrap(), 1);
    assert_eq!(response.u32("NFSv4 create-session flags").unwrap(), 0);
    for _ in 0..2 {
        for label in [
            "headerpad",
            "max request",
            "max response",
            "max cached",
            "max operations",
            "max requests",
        ] {
            let _ = response.u32(label).unwrap();
        }
        assert_eq!(response.u32("rdma count").unwrap(), 0);
    }
    response.end("NFSv4 create-session response").unwrap();

    let root_record = exchange(
        stream,
        encode_call(
            xid + 2,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "crash-root",
                &[
                    v4_sequence(&session, 1),
                    v4_op(OP_PUTROOTFH, |_| {}),
                    v4_op(OP_GETFH, |_| {}),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&root_record);
    assert_eq!(v4_compound_status(&mut response, 3), 0);
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_PUTROOTFH), 0);
    assert_eq!(v4_result_status(&mut response, OP_GETFH), 0);
    let _ = response.var_opaque(128, "NFSv4 root handle").unwrap();
    response.end("NFSv4 root response").unwrap();
    session
}

async fn mount_root(stream: &mut TcpStream, xid: u32) -> Vec<u8> {
    let call = encode_call(
        xid,
        MOUNT_PROGRAM,
        MOUNT_V3,
        MOUNTPROC3_MNT,
        None,
        None,
        &encode_xdr(|writer| writer.string("/")),
    );
    let record = exchange(stream, call).await;
    let (reply, mut body) = decode_reply(&record).expect("decode MOUNT reply");
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let mount = read_mount_res(&mut body).expect("decode MOUNT response");
    body.end("MOUNT response").expect("consume MOUNT response");
    assert_eq!(mount.status, 0);
    mount.fh.expect("successful MOUNT root handle")
}

async fn create_file(stream: &mut TcpStream, xid: u32, root: &[u8]) -> Vec<u8> {
    let args = encode_xdr(|writer| {
        write_create_args(
            writer,
            &Create3args {
                where_: DirOpArgs {
                    dir: root.to_vec(),
                    name: "crash-recovered.txt".to_owned(),
                },
                mode: CREATE_UNCHECKED,
                attributes: Some(Sattr3 {
                    mode: Some(0o644),
                    ..Sattr3::default()
                }),
                verf: None,
            },
        )
    });
    let record = exchange(
        stream,
        encode_call(xid, NFS_PROGRAM, NFS_V3, NFSPROC3_CREATE, None, None, &args),
    )
    .await;
    let (reply, mut body) = decode_reply(&record).expect("decode CREATE reply");
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let created = read_create_res(&mut body).expect("decode CREATE response");
    body.end("CREATE response")
        .expect("consume CREATE response");
    assert_eq!(created.status, NFS3_OK);
    created.obj.expect("successful CREATE file handle")
}

async fn write_file(stream: &mut TcpStream, xid: u32, file: &[u8], data: &[u8]) {
    let args = encode_xdr(|writer| {
        write_write_args(
            writer,
            &Write3args {
                file: file.to_vec(),
                offset: 0,
                count: data.len() as u32,
                stable: FILE_SYNC,
                data: data.to_vec(),
            },
        )
    });
    let record = exchange(
        stream,
        encode_call(xid, NFS_PROGRAM, NFS_V3, NFSPROC3_WRITE, None, None, &args),
    )
    .await;
    let (reply, mut body) = decode_reply(&record).expect("decode WRITE reply");
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let written = read_write_res(&mut body).expect("decode WRITE response");
    body.end("WRITE response").expect("consume WRITE response");
    assert_eq!(written.status, NFS3_OK);
    assert_eq!(written.count, data.len() as u32);
    assert_eq!(written.committed, FILE_SYNC);
}

async fn lookup_file(stream: &mut TcpStream, xid: u32, root: &[u8]) -> Vec<u8> {
    let args = encode_xdr(|writer| {
        write_dir_op(
            writer,
            &DirOpArgs {
                dir: root.to_vec(),
                name: "crash-recovered.txt".to_owned(),
            },
        )
    });
    let record = exchange(
        stream,
        encode_call(xid, NFS_PROGRAM, NFS_V3, NFSPROC3_LOOKUP, None, None, &args),
    )
    .await;
    let (reply, mut body) = decode_reply(&record).expect("decode LOOKUP reply");
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let lookup = read_lookup_res(&mut body).expect("decode LOOKUP response");
    body.end("LOOKUP response")
        .expect("consume LOOKUP response");
    assert_eq!(lookup.status, NFS3_OK);
    lookup.object.expect("successful LOOKUP file handle")
}

async fn read_file_response(stream: &mut TcpStream, xid: u32, file: &[u8]) -> Read3res {
    let args = encode_xdr(|writer| {
        write_read_args(
            writer,
            &Read3args {
                file: file.to_vec(),
                offset: 0,
                count: 128,
            },
        )
    });
    let record = exchange(
        stream,
        encode_call(xid, NFS_PROGRAM, NFS_V3, NFSPROC3_READ, None, None, &args),
    )
    .await;
    let (reply, mut body) = decode_reply(&record).expect("decode READ reply");
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let read = read_read_res(&mut body, 128).expect("decode READ response");
    body.end("READ response").expect("consume READ response");
    read
}

async fn read_file(stream: &mut TcpStream, xid: u32, file: &[u8]) -> Vec<u8> {
    let read = read_file_response(stream, xid, file).await;
    assert_eq!(read.status, NFS3_OK);
    read.data
}

async fn read_file_status(stream: &mut TcpStream, xid: u32, file: &[u8]) -> u32 {
    read_file_response(stream, xid, file).await.status
}

async fn child_server() {
    let role = std::env::var(CHILD_ENV).expect("child server role");
    let root = std::env::var_os(ROOT_ENV).expect("child server backend root");
    let server = NfsServer::new(HostFs::new(root), NfsServerOptions::default());
    let address = server.listen().await.expect("listen child NFS server");
    println!("READY {}", address.port());
    std::io::stdout().flush().expect("flush child readiness");
    assert!(role == "seed" || role == "replacement");
    std::future::pending::<()>().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nfs_v3_host_backend_survives_process_crash_and_restart() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child_server().await;
        return;
    }

    let root = TestRoot::new();
    let seed = start_child(&root.0, "seed").await;
    let mut first = TcpStream::connect(seed.address)
        .await
        .expect("connect seed NFS server");
    let root_handle = mount_root(&mut first, 1).await;
    let file_handle = create_file(&mut first, 2, &root_handle).await;
    let payload = b"survives an NFS server process crash";
    write_file(&mut first, 3, &file_handle, payload).await;
    first.shutdown().await.expect("close seed NFS connection");
    drop(first);
    seed.crash().await;

    let replacement = start_child(&root.0, "replacement").await;
    let mut second = TcpStream::connect(replacement.address)
        .await
        .expect("connect replacement NFS server");
    let replacement_root = mount_root(&mut second, 11).await;
    assert_eq!(
        read_file_status(&mut second, 12, &file_handle).await,
        NFS3ERR_STALE,
        "a file handle from the crashed server must not cross the replacement boundary"
    );
    let replacement_file = lookup_file(&mut second, 13, &replacement_root).await;
    assert_eq!(read_file(&mut second, 14, &replacement_file).await, payload);
    second
        .shutdown()
        .await
        .expect("close replacement NFS connection");
    drop(second);
    replacement.crash().await;

    assert_eq!(
        std::fs::read(root.0.join("crash-recovered.txt")).expect("read persisted backend file"),
        payload
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn nfs_v4_session_is_process_local_after_process_crash() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child_server().await;
        return;
    }

    let root = TestRoot::new();
    let seed = start_child_for(&root.0, "seed", V4_TEST_NAME).await;
    let mut first = TcpStream::connect(seed.address)
        .await
        .expect("connect seed NFSv4 server");
    let session = establish_v4_session(&mut first, 101).await;
    first.shutdown().await.expect("close seed NFSv4 connection");
    drop(first);
    seed.crash().await;

    let replacement = start_child_for(&root.0, "replacement", V4_TEST_NAME).await;
    let mut second = TcpStream::connect(replacement.address)
        .await
        .expect("connect replacement NFSv4 server");
    let stale_record = exchange(
        &mut second,
        encode_call(
            111,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound("stale-session", &[v4_sequence(&session, 2)]),
        ),
    )
    .await;
    let mut response = v4_reader(&stale_record);
    assert_eq!(
        v4_compound_status(&mut response, 0),
        NFS4ERR_BADSESSION,
        "a replacement process must reject the old NFSv4 session before dispatch"
    );
    response.end("stale NFSv4 session response").unwrap();
    second
        .shutdown()
        .await
        .expect("close replacement NFSv4 connection");
    drop(second);
    replacement.crash().await;
}
