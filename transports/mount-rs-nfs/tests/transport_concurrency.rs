//! Rootless NFSv3 pipelining and connection-task concurrency coverage.

use std::collections::HashSet;
use std::time::Duration;

use mount_rs_core::MemoryFs;
use mount_rs_nfs::constants::{MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_NULL};
use mount_rs_nfs::{
    NfsServer, NfsServerOptions, RecordAssembler, decode_reply, encode_call, frame_record,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pipelined_nfs_v3_calls_complete_on_one_connection() {
    let server = NfsServer::new(
        MemoryFs::empty(),
        NfsServerOptions {
            max_in_flight: 4,
            ..NfsServerOptions::default()
        },
    );
    let address = server.listen().await.expect("listen NFS server");
    let mut stream = TcpStream::connect(address)
        .await
        .expect("connect NFS client");

    let mut requests = Vec::new();
    for xid in 1..=8 {
        let call = encode_call(
            xid,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_NULL,
            None,
            None,
            &[],
        );
        requests.extend(frame_record(&call).expect("frame NFS request"));
    }
    stream
        .write_all(&requests)
        .await
        .expect("write pipelined NFS requests");

    let mut assembler = RecordAssembler::default();
    let mut buffer = [0_u8; 4096];
    let mut xids = HashSet::new();
    timeout(Duration::from_secs(2), async {
        while xids.len() < 8 {
            let count = stream.read(&mut buffer).await.expect("read NFS replies");
            assert!(count > 0, "NFS server closed before all replies");
            for record in assembler
                .push(&buffer[..count])
                .expect("assemble NFS replies")
            {
                let (reply, results) = decode_reply(&record).expect("decode NFS reply");
                assert_eq!(reply.accept_stat, Some(0));
                results.end("pipelined NFS NULL reply").unwrap();
                assert!(xids.insert(reply.xid), "duplicate NFS reply xid");
            }
        }
    })
    .await
    .expect("pipelined NFS replies complete");
    assert_eq!(xids.len(), 8);
    assert_eq!(server.connections(), 1);

    stream.shutdown().await.expect("close NFS client");
    timeout(Duration::from_secs(2), async {
        while server.connections() != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("NFS connection task closes");
    server.close().await.expect("close NFS server");
    sleep(Duration::from_millis(10)).await;
    assert_eq!(server.connections(), 0);
}
