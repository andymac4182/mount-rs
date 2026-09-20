//! Rootless NFSv4.1 wire integration.
//!
//! This exercises the TCP record-marking server, the v4.1 client/session
//! handshake, slot sequencing, file handles, OPEN/READ/WRITE/CLOSE and
//! namespace cleanup. It deliberately does not invoke the host kernel mount
//! client; native mount prerequisites are platform- and privilege-specific.

use std::time::Duration;

use mount_rs_core::MemoryFs;
use mount_rs_nfs::v4::CREATE_SESSION4_FLAG_CONN_BACK_CHAN;
use mount_rs_nfs::{
    NFS_V4, NFS4_PROGRAM, NfsServer, NfsServerOptions, RecordAssembler, XdrReader, XdrWriter,
    decode_reply, encode_call, frame_record,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::runtime::Builder;

const OP_CLOSE: u32 = 4;
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
    parse_compound(&mut reader, &[OP_CREATE_SESSION]);
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

fn exchange_args() -> Vec<u8> {
    op(OP_EXCHANGE_ID, |writer| {
        writer.fixed_opaque(b"v4-test!", 8);
        writer.var_opaque(b"mount-rs-v4-wire");
        writer.u32(0);
        writer.u32(0);
        writer.u32(0);
    })
}

fn create_session_args(clientid: u64) -> Vec<u8> {
    op(OP_CREATE_SESSION, |writer| {
        writer.u64(clientid);
        writer.u32(1);
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
                    let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
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
                                    writer.u32(1 << 1);
                                    writer.u32((1 << 9) | (1 << 23));
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
                        vec![2, 1 << 9]
                    );
                    assert_eq!(
                        response.var_opaque(128, "getattr values").unwrap().len(),
                        12
                    );
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
                    let open = op(OP_OPEN, |writer| {
                        writer.u32(1);
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
                        6,
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
                        writer.u32(2);
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
                    assert_eq!(response.u32("write committed").unwrap(), 2);
                    let _ = response.fixed_opaque(8, "write verifier").unwrap();
                    response.end("write response").unwrap();

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
                    let mut response = rpc(
                        &mut stream,
                        9,
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
                        11,
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
                        12,
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
                        13,
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
                        14,
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
                        15,
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
                        16,
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
