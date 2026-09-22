//! Cross-process NFSv3/v4.1 crash/restart evidence with a persistent host backend.
//!
//! The child server is terminated without running its async shutdown path.
//! The replacement server must still expose the file written through the NFS
//! wire and the namespace mutations completed before the crash. This qualifies
//! one-host process-crash recovery, not power-loss durability or durable NFSv4
//! lease, replay, or file-handle state.

#![cfg(unix)]

use std::io::{self, Write as _};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
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
use mount_rs_nfs::v4::{
    ACCESS4_READ, CLAIM_NULL, CREATE_SESSION4_FLAG_CONN_BACK_CHAN, FILE_SYNC4, NFS4ERR_BADSESSION,
    NFS4ERR_NOENT, NFS4ERR_STALE, OPEN4_CREATE, OPEN4_NOCREATE, OPEN4_SHARE_ACCESS_BOTH,
    UNCHECKED4,
};
use mount_rs_nfs::xdr::{XdrReader, XdrWriter, encode_xdr};
use mount_rs_nfs::{NFS_V4, NFS4_PROGRAM, NfsServer, NfsServerOptions};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader as AsyncBufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::time::timeout;

const CHILD_ENV: &str = "MOUNT_RS_NFS_PROCESS_CHILD";
const ROOT_ENV: &str = "MOUNT_RS_NFS_PROCESS_ROOT";
const TEST_NAME: &str = "nfs_v3_host_backend_survives_process_crash_and_restart";
const V4_TEST_NAME: &str = "nfs_v4_session_and_handles_are_process_local_after_process_crash";
static NEXT_ROOT_ID: AtomicU64 = AtomicU64::new(0);

const OP_GETFH: u32 = 10;
const OP_LOOKUP: u32 = 15;
const OP_OPEN: u32 = 18;
const OP_PUTFH: u32 = 22;
const OP_PUTROOTFH: u32 = 24;
const OP_READ: u32 = 25;
const OP_REMOVE: u32 = 28;
const OP_RENAME: u32 = 29;
const OP_SAVEFH: u32 = 32;
const OP_WRITE: u32 = 38;
const OP_EXCHANGE_ID: u32 = 42;
const OP_CREATE_SESSION: u32 = 43;
const OP_SEQUENCE: u32 = 53;
const OP_RECLAIM_COMPLETE: u32 = 58;
const V4_RECOVERY_FILE: &str = "v4-crash-recovered.txt";
const V4_REMOVED_FILE: &str = "v4-crash-removed.txt";
const V4_RENAME_FROM: &str = "v4-crash-rename-from.txt";
const V4_RENAME_TO: &str = "v4-crash-rename-to.txt";

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        loop {
            let serial = NEXT_ROOT_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "mount-rs-nfs-process-restart-{}-{nonce}-{serial}",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create process-restart backend root: {error}"),
            }
        }
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

async fn establish_v4_session(
    stream: &mut TcpStream,
    xid: u32,
    owner: &[u8],
) -> (u64, [u8; 16], Vec<u8>) {
    let exchange_record = exchange(
        stream,
        encode_call(
            xid,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound("crash-exchange", &[v4_exchange_args(owner)]),
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
    let root_handle = response.var_opaque(128, "NFSv4 root handle").unwrap();
    response.end("NFSv4 root response").unwrap();
    (clientid, session, root_handle)
}

async fn v4_reclaim_complete(stream: &mut TcpStream, xid: u32, session: &[u8; 16]) {
    let record = exchange(
        stream,
        encode_call(
            xid,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "crash-reclaim-complete",
                &[
                    v4_sequence(session, 2),
                    v4_op(OP_RECLAIM_COMPLETE, |writer| writer.bool(false)),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&record);
    assert_eq!(v4_compound_status(&mut response, 2), 0);
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_RECLAIM_COMPLETE), 0);
    response.end("NFSv4 reclaim-complete response").unwrap();
}

async fn v4_open_file(
    stream: &mut TcpStream,
    xid: u32,
    clientid: u64,
    session: &[u8; 16],
    sequence: u32,
    create: bool,
) -> ([u8; 16], Vec<u8>) {
    let open = v4_op(OP_OPEN, |writer| {
        writer.u32(0);
        writer.u32(if create {
            OPEN4_SHARE_ACCESS_BOTH
        } else {
            ACCESS4_READ
        });
        writer.u32(0);
        writer.u64(clientid);
        writer.var_opaque(b"crash-recovery-owner");
        writer.u32(if create { OPEN4_CREATE } else { OPEN4_NOCREATE });
        if create {
            writer.u32(UNCHECKED4);
            writer.u32(0);
            writer.u32(0);
        }
        writer.u32(CLAIM_NULL);
        writer.string(V4_RECOVERY_FILE);
    });
    let record = exchange(
        stream,
        encode_call(
            xid,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "crash-open",
                &[
                    v4_sequence(session, sequence),
                    v4_op(OP_PUTROOTFH, |_| {}),
                    open,
                    v4_op(OP_GETFH, |_| {}),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&record);
    assert_eq!(v4_compound_status(&mut response, 4), 0);
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_PUTROOTFH), 0);
    assert_eq!(v4_result_status(&mut response, OP_OPEN), 0);
    let stateid: [u8; 16] = response
        .fixed_opaque(16, "NFSv4 open stateid")
        .unwrap()
        .try_into()
        .unwrap();
    let _ = response.bool("NFSv4 open cinfo atomic").unwrap();
    let _ = response.u64("NFSv4 open cinfo before").unwrap();
    let _ = response.u64("NFSv4 open cinfo after").unwrap();
    let _ = response.u32("NFSv4 open flags").unwrap();
    let _ = response
        .array(16, "NFSv4 open attrset", |reader| {
            reader.u32("attribute word")
        })
        .unwrap();
    assert_eq!(response.u32("NFSv4 open delegation").unwrap(), 0);
    assert_eq!(v4_result_status(&mut response, OP_GETFH), 0);
    let handle = response.var_opaque(128, "NFSv4 file handle").unwrap();
    response.end("NFSv4 open response").unwrap();
    (stateid, handle)
}

async fn v4_write_file(
    stream: &mut TcpStream,
    session: &[u8; 16],
    sequence: u32,
    stateid: &[u8; 16],
    handle: &[u8],
    payload: &[u8],
) {
    let record = exchange(
        stream,
        encode_call(
            106,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "crash-write",
                &[
                    v4_sequence(session, sequence),
                    v4_op(OP_PUTFH, |writer| writer.var_opaque(handle)),
                    v4_op(OP_WRITE, |writer| {
                        writer.fixed_opaque(stateid, 16);
                        writer.u64(0);
                        writer.u32(FILE_SYNC4);
                        writer.var_opaque(payload);
                    }),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&record);
    assert_eq!(v4_compound_status(&mut response, 3), 0);
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_PUTFH), 0);
    assert_eq!(v4_result_status(&mut response, OP_WRITE), 0);
    assert_eq!(
        response.u32("NFSv4 written bytes").unwrap(),
        payload.len() as u32
    );
    assert_eq!(response.u32("NFSv4 committed level").unwrap(), FILE_SYNC4);
    let _ = response.fixed_opaque(8, "NFSv4 write verifier").unwrap();
    response.end("NFSv4 FILE_SYNC4 response").unwrap();
}

async fn v4_read_file(
    stream: &mut TcpStream,
    session: &[u8; 16],
    sequence: u32,
    stateid: &[u8; 16],
    handle: &[u8],
) -> Vec<u8> {
    let record = exchange(
        stream,
        encode_call(
            216,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "crash-read",
                &[
                    v4_sequence(session, sequence),
                    v4_op(OP_PUTFH, |writer| writer.var_opaque(handle)),
                    v4_op(OP_READ, |writer| {
                        writer.fixed_opaque(stateid, 16);
                        writer.u64(0);
                        writer.u32(256);
                    }),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&record);
    assert_eq!(v4_compound_status(&mut response, 3), 0);
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_PUTFH), 0);
    assert_eq!(v4_result_status(&mut response, OP_READ), 0);
    assert!(response.bool("NFSv4 read EOF").unwrap());
    let data = response.var_opaque(256, "NFSv4 recovered data").unwrap();
    response.end("NFSv4 recovered read response").unwrap();
    data
}

async fn v4_remove_file(stream: &mut TcpStream, session: &[u8; 16]) {
    let record = exchange(
        stream,
        encode_call(
            107,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "crash-remove",
                &[
                    v4_sequence(session, 5),
                    v4_op(OP_PUTROOTFH, |_| {}),
                    v4_op(OP_REMOVE, |writer| writer.string(V4_REMOVED_FILE)),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&record);
    assert_eq!(v4_compound_status(&mut response, 3), 0);
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_PUTROOTFH), 0);
    assert_eq!(v4_result_status(&mut response, OP_REMOVE), 0);
    let _ = response.bool("NFSv4 remove cinfo atomic").unwrap();
    let _ = response.u64("NFSv4 remove cinfo before").unwrap();
    let _ = response.u64("NFSv4 remove cinfo after").unwrap();
    response.end("NFSv4 remove response").unwrap();
}

async fn v4_rename_file(stream: &mut TcpStream, session: &[u8; 16]) {
    let record = exchange(
        stream,
        encode_call(
            108,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "crash-rename",
                &[
                    v4_sequence(session, 6),
                    v4_op(OP_PUTROOTFH, |_| {}),
                    v4_op(OP_SAVEFH, |_| {}),
                    v4_op(OP_RENAME, |writer| {
                        writer.string(V4_RENAME_FROM);
                        writer.string(V4_RENAME_TO);
                    }),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&record);
    assert_eq!(v4_compound_status(&mut response, 4), 0);
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_PUTROOTFH), 0);
    assert_eq!(v4_result_status(&mut response, OP_SAVEFH), 0);
    assert_eq!(v4_result_status(&mut response, OP_RENAME), 0);
    for directory in ["source", "target"] {
        let _ = response
            .bool(&format!("NFSv4 rename {directory} cinfo atomic"))
            .unwrap();
        let _ = response
            .u64(&format!("NFSv4 rename {directory} cinfo before"))
            .unwrap();
        let _ = response
            .u64(&format!("NFSv4 rename {directory} cinfo after"))
            .unwrap();
    }
    response.end("NFSv4 rename response").unwrap();
}

async fn v4_assert_lookup(
    stream: &mut TcpStream,
    session: &[u8; 16],
    sequence: u32,
    name: &str,
    expected_status: u32,
) {
    let record = exchange(
        stream,
        encode_call(
            210 + sequence,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "crash-namespace-lookup",
                &[
                    v4_sequence(session, sequence),
                    v4_op(OP_PUTROOTFH, |_| {}),
                    v4_op(OP_LOOKUP, |writer| writer.string(name)),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&record);
    assert_eq!(v4_compound_status(&mut response, 3), expected_status);
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_PUTROOTFH), 0);
    assert_eq!(v4_result_status(&mut response, OP_LOOKUP), expected_status);
    response.end("NFSv4 namespace lookup response").unwrap();
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

async fn v4_crash_recovery_body() {
    if std::env::var_os(CHILD_ENV).is_some() {
        child_server().await;
        return;
    }

    let root = TestRoot::new();
    std::fs::write(root.0.join(V4_REMOVED_FILE), b"removed before crash")
        .expect("seed NFSv4 namespace removal target");
    let rename_payload = b"renamed before crash";
    std::fs::write(root.0.join(V4_RENAME_FROM), rename_payload)
        .expect("seed NFSv4 namespace rename target");
    let seed = start_child_for(&root.0, "seed", V4_TEST_NAME).await;
    let mut first = TcpStream::connect(seed.address)
        .await
        .expect("connect seed NFSv4 server");
    let (clientid, session, root_handle) =
        establish_v4_session(&mut first, 101, b"crash-client-seed").await;
    v4_reclaim_complete(&mut first, 104, &session).await;
    let (stateid, file_handle) = v4_open_file(&mut first, 105, clientid, &session, 3, true).await;
    let payload = b"FILE_SYNC4 survives an NFSv4 server process crash";
    v4_write_file(&mut first, &session, 4, &stateid, &file_handle, payload).await;
    v4_remove_file(&mut first, &session).await;
    assert!(!root.0.join(V4_REMOVED_FILE).exists());
    v4_rename_file(&mut first, &session).await;
    assert!(!root.0.join(V4_RENAME_FROM).exists());
    assert_eq!(
        std::fs::read(root.0.join(V4_RENAME_TO)).unwrap(),
        rename_payload
    );
    first.shutdown().await.expect("close seed NFSv4 connection");
    drop(first);
    seed.crash().await;

    let replacement = start_child_for(&root.0, "replacement", V4_TEST_NAME).await;
    let mut second = TcpStream::connect(replacement.address)
        .await
        .expect("connect replacement NFSv4 server");
    let (replacement_clientid, replacement_session, _) =
        establish_v4_session(&mut second, 201, b"crash-client-replacement").await;
    v4_reclaim_complete(&mut second, 204, &replacement_session).await;
    let stale_record = exchange(
        &mut second,
        encode_call(
            111,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound("stale-session", &[v4_sequence(&session, 7)]),
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

    let stale_handle_record = exchange(
        &mut second,
        encode_call(
            212,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "stale-handle",
                &[
                    v4_sequence(&replacement_session, 3),
                    v4_op(OP_PUTFH, |writer| writer.var_opaque(&root_handle)),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&stale_handle_record);
    assert_eq!(
        v4_compound_status(&mut response, 2),
        NFS4ERR_STALE,
        "a replacement process must reject the old NFSv4 file handle"
    );
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_PUTFH), NFS4ERR_STALE);
    response.end("stale NFSv4 handle response").unwrap();
    let stale_file_record = exchange(
        &mut second,
        encode_call(
            213,
            NFS4_PROGRAM,
            NFS_V4,
            1,
            None,
            None,
            &v4_compound(
                "stale-file-handle",
                &[
                    v4_sequence(&replacement_session, 4),
                    v4_op(OP_PUTFH, |writer| writer.var_opaque(&file_handle)),
                ],
            ),
        ),
    )
    .await;
    let mut response = v4_reader(&stale_file_record);
    assert_eq!(v4_compound_status(&mut response, 2), NFS4ERR_STALE);
    v4_consume_sequence(&mut response);
    assert_eq!(v4_result_status(&mut response, OP_PUTFH), NFS4ERR_STALE);
    response.end("stale NFSv4 file handle response").unwrap();
    let (replacement_stateid, replacement_handle) = v4_open_file(
        &mut second,
        215,
        replacement_clientid,
        &replacement_session,
        5,
        false,
    )
    .await;
    assert_eq!(
        v4_read_file(
            &mut second,
            &replacement_session,
            6,
            &replacement_stateid,
            &replacement_handle,
        )
        .await,
        payload
    );
    v4_assert_lookup(
        &mut second,
        &replacement_session,
        7,
        V4_REMOVED_FILE,
        NFS4ERR_NOENT,
    )
    .await;
    v4_assert_lookup(
        &mut second,
        &replacement_session,
        8,
        V4_RENAME_FROM,
        NFS4ERR_NOENT,
    )
    .await;
    v4_assert_lookup(&mut second, &replacement_session, 9, V4_RENAME_TO, 0).await;
    second
        .shutdown()
        .await
        .expect("close replacement NFSv4 connection");
    drop(second);
    replacement.crash().await;
    assert_eq!(
        std::fs::read(root.0.join(V4_RECOVERY_FILE)).expect("read NFSv4 host file after crashes"),
        payload
    );
    assert!(!root.0.join(V4_REMOVED_FILE).exists());
    assert!(!root.0.join(V4_RENAME_FROM).exists());
    assert_eq!(
        std::fs::read(root.0.join(V4_RENAME_TO)).unwrap(),
        rename_payload
    );
}

#[test]
fn nfs_v4_session_and_handles_are_process_local_after_process_crash() {
    std::thread::Builder::new()
        .name("nfs-v4-process-restart-test".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_stack_size(8 * 1024 * 1024)
                .enable_all()
                .build()
                .expect("build NFSv4 restart runtime")
                .block_on(v4_crash_recovery_body());
        })
        .expect("spawn NFSv4 restart test thread")
        .join()
        .expect("NFSv4 restart test thread panicked");
}
