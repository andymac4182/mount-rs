use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use mount_rs_9p::server::{
    P9AttachOptions, P9Server, P9ServerHooks, P9TransportError, P9TransportErrorKind,
};
use mount_rs_core::MemoryFs;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

#[derive(Clone)]
struct Events {
    values: Arc<Mutex<Vec<P9TransportError>>>,
    notify: Arc<Notify>,
}

impl Events {
    fn new() -> (Self, P9ServerHooks) {
        let values = Arc::new(Mutex::new(Vec::new()));
        let notify = Arc::new(Notify::new());
        let callback_values = Arc::clone(&values);
        let callback_notify = Arc::clone(&notify);
        let hooks = P9ServerHooks {
            on_transport_error: Some(Arc::new(move |error| {
                callback_values.lock().expect("9P event lock").push(error);
                callback_notify.notify_waiters();
            })),
        };
        (Self { values, notify }, hooks)
    }

    fn snapshot(&self) -> Vec<P9TransportError> {
        self.values.lock().expect("9P event lock").clone()
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
        .expect("9P transport hook callback");
    }
}

#[tokio::test]
async fn malformed_frame_reports_exactly_once() {
    let (events, hooks) = Events::new();
    let server = Arc::new(P9Server::new_with_hooks(
        MemoryFs::empty(),
        Default::default(),
        hooks,
    ));
    let (server_stream, mut client_stream) = tokio::io::duplex(1024);
    let connection = server
        .attach(
            server_stream,
            P9AttachOptions {
                peer: Some("malformed-client".to_owned()),
                own: false,
            },
        )
        .expect("attach malformed client");

    client_stream
        .write_all(&[4, 0, 0, 0, 0, 0, 0])
        .await
        .expect("send malformed frame size");
    timeout(Duration::from_secs(2), connection.wait_closed())
        .await
        .expect("malformed connection closes");
    events.wait_for(1).await;
    sleep(Duration::from_millis(20)).await;

    let observed = events.snapshot();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].kind, P9TransportErrorKind::Frame);
    assert_eq!(observed[0].peer.as_deref(), Some("malformed-client"));

    drop(client_stream);
    server.close().await.expect("close 9P test server");
    assert_eq!(events.snapshot().len(), 1);
}

#[tokio::test]
async fn orderly_disconnect_does_not_report() {
    let (events, hooks) = Events::new();
    let server = Arc::new(P9Server::new_with_hooks(
        MemoryFs::empty(),
        Default::default(),
        hooks,
    ));
    let (server_stream, client_stream) = tokio::io::duplex(1024);
    let connection = server
        .attach(
            server_stream,
            P9AttachOptions {
                peer: Some("clean-client".to_owned()),
                own: false,
            },
        )
        .expect("attach clean client");

    drop(client_stream);
    timeout(Duration::from_secs(2), connection.wait_closed())
        .await
        .expect("clean connection closes");
    sleep(Duration::from_millis(20)).await;
    assert!(events.snapshot().is_empty());
    server.close().await.expect("close 9P test server");
}

struct ResetReadStream;

impl AsyncRead for ResetReadStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let _ = self;
        Poll::Ready(Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "expected test reset",
        )))
    }
}

impl AsyncWrite for ResetReadStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        _buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let _ = self;
        Poll::Ready(Ok(0))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let _ = self;
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let _ = self;
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn expected_connection_reset_does_not_report() {
    let (events, hooks) = Events::new();
    let server = Arc::new(P9Server::new_with_hooks(
        MemoryFs::empty(),
        Default::default(),
        hooks,
    ));
    let connection = server
        .attach(
            ResetReadStream,
            P9AttachOptions {
                peer: Some("reset-client".to_owned()),
                own: false,
            },
        )
        .expect("attach reset client");

    timeout(Duration::from_secs(2), connection.wait_closed())
        .await
        .expect("reset connection closes");
    sleep(Duration::from_millis(20)).await;
    assert!(events.snapshot().is_empty());
    server.close().await.expect("close 9P test server");
}
