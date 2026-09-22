use std::sync::{Arc, Mutex};
use std::time::Duration;

use mount_rs_memfs::MemoryFs;
use mount_rs_nfs::frame_record;
use mount_rs_nfs::server::{NfsServer, NfsServerHooks, NfsTransportError, NfsTransportErrorKind};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

#[derive(Clone)]
struct Events {
    values: Arc<Mutex<Vec<NfsTransportError>>>,
    notify: Arc<Notify>,
}

impl Events {
    fn new() -> (Self, NfsServerHooks) {
        let values = Arc::new(Mutex::new(Vec::new()));
        let notify = Arc::new(Notify::new());
        let callback_values = Arc::clone(&values);
        let callback_notify = Arc::clone(&notify);
        let hooks = NfsServerHooks {
            on_transport_error: Some(Arc::new(move |error| {
                callback_values.lock().expect("NFS event lock").push(error);
                callback_notify.notify_waiters();
            })),
            on_error: None,
        };
        (Self { values, notify }, hooks)
    }

    fn snapshot(&self) -> Vec<NfsTransportError> {
        self.values.lock().expect("NFS event lock").clone()
    }

    async fn wait_for(&self, count: usize) {
        timeout(Duration::from_secs(2), async {
            loop {
                if self.snapshot().len() >= count {
                    return;
                }
                self.notify.notified().await;
            }
        })
        .await
        .expect("NFS transport hook callback");
    }
}

async fn start_server(record_limit: usize) -> (NfsServer, Events, std::net::SocketAddr) {
    let (events, hooks) = Events::new();
    let options = mount_rs_nfs::NfsServerOptions {
        record_limit,
        ..Default::default()
    };
    let server = NfsServer::new_with_hooks(MemoryFs::empty(), options, hooks);
    let address = server.listen().await.expect("listen NFS test server");
    (server, events, address)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_record_reports_exactly_once() {
    let (server, events, address) = start_server(64).await;
    let mut client = TcpStream::connect(address)
        .await
        .expect("connect malformed NFS client");
    let marker = (0x8000_0000_u32 | 65).to_be_bytes();
    client
        .write_all(&marker)
        .await
        .expect("send oversized record marker");

    events.wait_for(1).await;
    sleep(Duration::from_millis(20)).await;
    let observed = events.snapshot();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].kind, NfsTransportErrorKind::Record);
    assert_eq!(observed[0].peer.as_deref(), Some("127.0.0.1"));

    let _ = client.shutdown().await;
    server.close().await.expect("close NFS test server");
    assert_eq!(events.snapshot().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn orderly_disconnect_does_not_report() {
    let (server, events, address) = start_server(1024).await;
    let mut client = TcpStream::connect(address)
        .await
        .expect("connect clean NFS client");
    client.shutdown().await.expect("half-close NFS client");
    sleep(Duration::from_millis(50)).await;
    assert!(events.snapshot().is_empty());
    server.close().await.expect("close NFS test server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_decode_failure_is_not_a_transport_error() {
    let (server, events, address) = start_server(1024).await;
    let mut client = TcpStream::connect(address)
        .await
        .expect("connect malformed request client");
    client
        .write_all(&frame_record(&[1, 2, 3]).expect("frame malformed RPC body"))
        .await
        .expect("send malformed RPC body");
    sleep(Duration::from_millis(50)).await;
    assert!(events.snapshot().is_empty());
    client
        .shutdown()
        .await
        .expect("half-close malformed request");
    sleep(Duration::from_millis(20)).await;
    assert!(events.snapshot().is_empty());
    server.close().await.expect("close NFS test server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_server_close_does_not_report() {
    let (server, events, address) = start_server(1024).await;
    let _client = TcpStream::connect(address)
        .await
        .expect("connect before explicit NFS close");
    sleep(Duration::from_millis(20)).await;
    server.close().await.expect("explicitly close NFS server");
    sleep(Duration::from_millis(20)).await;
    assert!(events.snapshot().is_empty());
}
