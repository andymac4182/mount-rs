//! Real-TCP RPC program/version negotiation across the shared NFS router.

use mount_rs_core::MemoryFs;
use mount_rs_nfs::constants::{
    MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_NULL, NFS_PROGRAM, NFS_V3, NFSPROC3_NULL,
};
use mount_rs_nfs::rpc::{
    AUTH_TOOWEAK, MSG_ACCEPTED, MSG_DENIED, OpaqueAuth, RPC_AUTH_ERROR, RPC_MISMATCH,
    RPC_PROG_MISMATCH, RPC_PROG_UNAVAIL, RPC_SUCCESS, decode_reply, encode_call, frame_record,
};
use mount_rs_nfs::{NFS_V4, NfsServer, NfsServerOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

async fn exchange(stream: &mut TcpStream, call: &[u8]) -> Vec<u8> {
    stream
        .write_all(&frame_record(call).expect("frame RPC request"))
        .await
        .expect("write RPC request");
    let mut marker = [0_u8; 4];
    stream
        .read_exact(&mut marker)
        .await
        .expect("read RPC marker");
    let marker = u32::from_be_bytes(marker);
    assert_ne!(marker & 0x8000_0000, 0);
    let mut record = vec![0; (marker & 0x7fff_ffff) as usize];
    stream
        .read_exact(&mut record)
        .await
        .expect("read RPC reply");
    record
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shared_router_advertises_both_nfs_versions_and_keeps_mount_separate() {
    let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
    let address = server.listen().await.expect("listen NFS router");
    let mut stream = TcpStream::connect(address)
        .await
        .expect("connect NFS router");

    for (xid, program, version, status, low, high) in [
        (1, NFS_PROGRAM, 2, RPC_PROG_MISMATCH, Some(3), Some(4)),
        (2, NFS_PROGRAM, 5, RPC_PROG_MISMATCH, Some(3), Some(4)),
        (3, 100_099, 1, RPC_PROG_UNAVAIL, None, None),
        (4, MOUNT_PROGRAM, 2, RPC_PROG_MISMATCH, Some(3), Some(3)),
        (5, MOUNT_PROGRAM, MOUNT_V3, RPC_SUCCESS, None, None),
        (6, NFS_PROGRAM, NFS_V3, RPC_SUCCESS, None, None),
        (7, NFS_PROGRAM, NFS_V4, RPC_SUCCESS, None, None),
    ] {
        let procedure = if program == MOUNT_PROGRAM {
            MOUNTPROC3_NULL
        } else {
            NFSPROC3_NULL
        };
        let record = exchange(
            &mut stream,
            &encode_call(xid, program, version, procedure, None, None, &[]),
        )
        .await;
        let (reply, body) = decode_reply(&record).expect("decode routed RPC reply");
        assert_eq!(reply.xid, xid);
        assert_eq!(reply.reply_stat, MSG_ACCEPTED);
        assert_eq!(reply.accept_stat, Some(status));
        assert_eq!((reply.low, reply.high), (low, high));
        body.end("routed RPC reply").expect("no reply body");
    }

    let mut wrong_rpc = encode_call(8, NFS_PROGRAM, 5, NFSPROC3_NULL, None, None, &[]);
    wrong_rpc[8..12].copy_from_slice(&3_u32.to_be_bytes());
    let record = exchange(&mut stream, &wrong_rpc).await;
    let (reply, body) = decode_reply(&record).expect("decode RPC version refusal");
    assert_eq!(reply.reply_stat, MSG_DENIED);
    assert_eq!(reply.reject_stat, Some(RPC_MISMATCH));
    assert_eq!((reply.low, reply.high), (Some(2), Some(2)));
    body.end("RPC version refusal").expect("no reply body");

    let unsupported_auth = OpaqueAuth {
        flavor: 99,
        body: Vec::new(),
    };
    let record = exchange(
        &mut stream,
        &encode_call(
            9,
            NFS_PROGRAM,
            5,
            NFSPROC3_NULL,
            Some(&unsupported_auth),
            None,
            &[],
        ),
    )
    .await;
    let (reply, body) = decode_reply(&record).expect("decode auth refusal");
    assert_eq!(reply.reply_stat, MSG_DENIED);
    assert_eq!(reply.reject_stat, Some(RPC_AUTH_ERROR));
    assert_eq!(reply.auth_stat, Some(AUTH_TOOWEAK));
    body.end("auth refusal").expect("no reply body");

    let stats = server.session().stats();
    assert_eq!(stats.requests, 9);
    assert_eq!(stats.replies, 9);
    assert_eq!(stats.dropped, 0);
    server.close().await.expect("close NFS router");
}
