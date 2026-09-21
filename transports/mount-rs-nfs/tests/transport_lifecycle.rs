//! NFS connection and server shutdown must cancel blocked request workers.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use mount_rs_core::{FsDriver, MemoryFs};
use mount_rs_nfs::constants::{MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_MNT};
use mount_rs_nfs::rpc::{encode_call, frame_record};
use mount_rs_nfs::xdr::encode_xdr;
use mount_rs_nfs::{NfsServer, NfsServerOptions};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

struct BlockingStatDriver {
    inner: MemoryFs,
    entered: Arc<Notify>,
    cancelled: Arc<AtomicUsize>,
}

struct CancellationProbe {
    cancelled: Arc<AtomicUsize>,
}

impl Drop for CancellationProbe {
    fn drop(&mut self) {
        self.cancelled.fetch_add(1, Ordering::AcqRel);
    }
}

impl FsDriver for BlockingStatDriver {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.inner.capabilities()
    }

    fn stat<'a, 'b, 'async_trait>(
        &'a self,
        _path: &'b str,
    ) -> Pin<
        Box<dyn Future<Output = mount_rs_core::Result<mount_rs_core::Stats>> + Send + 'async_trait>,
    >
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let _probe = CancellationProbe {
                cancelled: Arc::clone(&self.cancelled),
            };
            self.entered.notify_one();
            std::future::pending().await
        })
    }

    fn readdir<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<
        Box<
            dyn Future<Output = mount_rs_core::Result<Vec<mount_rs_core::DirEntry>>>
                + Send
                + 'async_trait,
        >,
    >
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
    ) -> Pin<
        Box<
            dyn Future<Output = mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>>>
                + Send
                + 'async_trait,
        >,
    >
    where
        'a: 'async_trait,
        'b: 'async_trait,
        'c: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move { self.inner.open(path, flags, mode).await })
    }
}

fn mount_request() -> Vec<u8> {
    frame_record(&encode_call(
        1,
        MOUNT_PROGRAM,
        MOUNT_V3,
        MOUNTPROC3_MNT,
        None,
        None,
        &encode_xdr(|writer| writer.string("/")),
    ))
    .expect("frame NFS MOUNT request")
}

async fn wait_for_connections(server: &NfsServer, expected: usize) {
    timeout(Duration::from_secs(2), async {
        loop {
            if server.connections() == expected {
                return;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("NFS connection count reaches expected value");
}

fn blocking_server(entered: &Arc<Notify>, cancelled: &Arc<AtomicUsize>) -> NfsServer {
    NfsServer::new(
        BlockingStatDriver {
            inner: MemoryFs::empty(),
            entered: Arc::clone(entered),
            cancelled: Arc::clone(cancelled),
        },
        NfsServerOptions::default(),
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_close_cancels_a_blocked_nfs_request() {
    let entered = Arc::new(Notify::new());
    let cancelled = Arc::new(AtomicUsize::new(0));
    let server = blocking_server(&entered, &cancelled);
    let address = server.listen().await.expect("listen NFS server");
    let mut peer = TcpStream::connect(address)
        .await
        .expect("connect NFS client");
    wait_for_connections(&server, 1).await;
    peer.write_all(&mount_request())
        .await
        .expect("send blocking MOUNT request");
    timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("MOUNT request reaches the blocked backend");

    let connection = server
        .clients()
        .expect("list NFS clients")
        .pop()
        .expect("blocked NFS client remains visible");
    timeout(Duration::from_millis(250), connection.close())
        .await
        .expect("connection close is bounded")
        .expect("connection close succeeds");
    assert!(connection.is_closed());
    assert_eq!(cancelled.load(Ordering::Acquire), 1);
    wait_for_connections(&server, 0).await;

    let _ = peer.shutdown().await;
    server.close().await.expect("close NFS server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn server_close_cancels_a_blocked_nfs_request() {
    let entered = Arc::new(Notify::new());
    let cancelled = Arc::new(AtomicUsize::new(0));
    let server = blocking_server(&entered, &cancelled);
    let address = server.listen().await.expect("listen NFS server");
    let mut peer = TcpStream::connect(address)
        .await
        .expect("connect NFS client");
    wait_for_connections(&server, 1).await;
    peer.write_all(&mount_request())
        .await
        .expect("send blocking MOUNT request");
    timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("MOUNT request reaches the blocked backend");

    let connection = server
        .clients()
        .expect("list NFS clients")
        .pop()
        .expect("blocked NFS client remains visible");
    timeout(Duration::from_millis(250), server.close())
        .await
        .expect("server close is bounded")
        .expect("server close succeeds");
    assert!(connection.is_closed());
    assert_eq!(cancelled.load(Ordering::Acquire), 1);
    assert_eq!(server.connections(), 0);

    let _ = peer.shutdown().await;
}
