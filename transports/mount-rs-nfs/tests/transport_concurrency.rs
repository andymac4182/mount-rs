//! Rootless NFSv3 pipelining and connection-task concurrency coverage.

use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mount_rs_core::{DirEntry, FileHandle, FsDriver, MemoryFs, Result, Stats};
use mount_rs_nfs::constants::{
    MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_MNT, MOUNTPROC3_NULL, NFS_PROGRAM, NFS_V3, NFS3_OK,
    NFSPROC3_GETATTR, NFSPROC3_LOOKUP,
};
use mount_rs_nfs::protocol::{
    DirOpArgs, read_getattr_res, read_lookup_res, read_mount_res, write_dir_op,
};
use mount_rs_nfs::rpc::RPC_SUCCESS;
use mount_rs_nfs::xdr::encode_xdr;
use mount_rs_nfs::{
    NfsServer, NfsServerOptions, RecordAssembler, decode_reply, encode_call, frame_record,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

struct CompletionGateDriver {
    inner: MemoryFs,
    block_slow: Arc<AtomicBool>,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

impl FsDriver for CompletionGateDriver {
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
            if self.block_slow.load(Ordering::Acquire) && path.ends_with("/slow.txt") {
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

async fn exchange(stream: &mut TcpStream, call: Vec<u8>) -> Vec<u8> {
    stream
        .write_all(&frame_record(&call).expect("frame NFS request"))
        .await
        .expect("write NFS request");
    let mut marker = [0_u8; 4];
    stream
        .read_exact(&mut marker)
        .await
        .expect("read NFS reply marker");
    let marker = u32::from_be_bytes(marker);
    assert_ne!(marker & 0x8000_0000, 0, "test server returned fragments");
    let mut record = vec![0_u8; (marker & 0x7fff_ffff) as usize];
    stream
        .read_exact(&mut record)
        .await
        .expect("read NFS reply record");
    record
}

async fn next_record(stream: &mut TcpStream, assembler: &mut RecordAssembler) -> Vec<u8> {
    let mut buffer = [0_u8; 4096];
    loop {
        let count = stream.read(&mut buffer).await.expect("read NFS reply");
        assert!(count > 0, "NFS server closed before the reply");
        if let Some(record) = assembler
            .push(&buffer[..count])
            .expect("assemble NFS reply")
            .into_iter()
            .next()
        {
            return record;
        }
    }
}

async fn lookup(stream: &mut TcpStream, xid: u32, root: &[u8], name: &str) -> Vec<u8> {
    let call = encode_call(
        xid,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_LOOKUP,
        None,
        None,
        &encode_xdr(|writer| {
            write_dir_op(
                writer,
                &DirOpArgs {
                    dir: root.to_vec(),
                    name: name.to_owned(),
                },
            )
        }),
    );
    let record = exchange(stream, call).await;
    let (reply, mut body) = decode_reply(&record).expect("decode LOOKUP reply");
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let result = read_lookup_res(&mut body).expect("decode LOOKUP result");
    body.end("LOOKUP reply").expect("consume LOOKUP reply");
    assert_eq!(result.status, NFS3_OK);
    result.object.expect("LOOKUP file handle")
}

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fast_nfs_reply_bypasses_a_blocked_rpc_on_one_connection() {
    let inner = MemoryFs::empty();
    inner
        .write_file("/slow.txt", b"slow")
        .await
        .expect("seed slow file");
    inner
        .write_file("/fast.txt", b"fast")
        .await
        .expect("seed fast file");
    let block_slow = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let server = NfsServer::new(
        CompletionGateDriver {
            inner,
            block_slow: Arc::clone(&block_slow),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        NfsServerOptions {
            max_in_flight: 2,
            ..NfsServerOptions::default()
        },
    );
    let address = server.listen().await.expect("listen NFS server");
    let mut stream = TcpStream::connect(address)
        .await
        .expect("connect NFS client");

    let mount_call = encode_call(
        10,
        MOUNT_PROGRAM,
        MOUNT_V3,
        MOUNTPROC3_MNT,
        None,
        None,
        &encode_xdr(|writer| writer.string("/")),
    );
    let record = exchange(&mut stream, mount_call).await;
    let (reply, mut body) = decode_reply(&record).expect("decode MOUNT reply");
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let mount = read_mount_res(&mut body).expect("decode MOUNT result");
    body.end("MOUNT reply").expect("consume MOUNT reply");
    assert_eq!(mount.status, NFS3_OK);
    let root = mount.fh.expect("MOUNT root handle");
    let slow = lookup(&mut stream, 11, &root, "slow.txt").await;
    let fast = lookup(&mut stream, 12, &root, "fast.txt").await;

    block_slow.store(true, Ordering::Release);
    let slow_call = encode_call(
        20,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_GETATTR,
        None,
        None,
        &encode_xdr(|writer| writer.var_opaque(&slow)),
    );
    let fast_call = encode_call(
        21,
        NFS_PROGRAM,
        NFS_V3,
        NFSPROC3_GETATTR,
        None,
        None,
        &encode_xdr(|writer| writer.var_opaque(&fast)),
    );
    let mut requests = frame_record(&slow_call).expect("frame slow GETATTR");
    requests.extend(frame_record(&fast_call).expect("frame fast GETATTR"));
    stream
        .write_all(&requests)
        .await
        .expect("write pipelined GETATTR requests");
    timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("slow GETATTR reaches the blocked backend");

    let mut assembler = RecordAssembler::default();
    let fast_record = timeout(
        Duration::from_millis(250),
        next_record(&mut stream, &mut assembler),
    )
    .await
    .expect("fast GETATTR reply bypasses the blocked request");
    let (reply, mut body) = decode_reply(&fast_record).expect("decode fast GETATTR reply");
    assert_eq!(reply.xid, 21);
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let fast_result = read_getattr_res(&mut body).expect("decode fast GETATTR result");
    body.end("fast GETATTR reply")
        .expect("consume fast GETATTR reply");
    assert_eq!(fast_result.status, NFS3_OK);

    release.notify_waiters();
    let slow_record = timeout(
        Duration::from_secs(2),
        next_record(&mut stream, &mut assembler),
    )
    .await
    .expect("slow GETATTR reply completes after release");
    let (reply, mut body) = decode_reply(&slow_record).expect("decode slow GETATTR reply");
    assert_eq!(reply.xid, 20);
    assert_eq!(reply.accept_stat, Some(RPC_SUCCESS));
    let slow_result = read_getattr_res(&mut body).expect("decode slow GETATTR result");
    body.end("slow GETATTR reply")
        .expect("consume slow GETATTR reply");
    assert_eq!(slow_result.status, NFS3_OK);

    stream.shutdown().await.expect("close NFS client");
    server.close().await.expect("close NFS server");
}
