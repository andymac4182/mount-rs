use std::io;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use mount_rs_9p::server::{
    P9AttachOptions, P9Server, P9ServerHooks, P9TransportError, P9TransportErrorKind,
};
use mount_rs_9p::{P9_NOTAG, P9_TVERSION, Tversion, encode_message, write_tversion};
use mount_rs_memfs::MemoryFs;
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

struct FaultStream {
    input: Vec<u8>,
    offset: usize,
    read_error: Option<io::ErrorKind>,
    write_error: Option<io::ErrorKind>,
    flush_error: Option<io::ErrorKind>,
}

impl FaultStream {
    fn new(
        input: Vec<u8>,
        read_error: Option<io::ErrorKind>,
        write_error: Option<io::ErrorKind>,
        flush_error: Option<io::ErrorKind>,
    ) -> Self {
        Self {
            input,
            offset: 0,
            read_error,
            write_error,
            flush_error,
        }
    }
}

impl AsyncRead for FaultStream {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let stream = self.get_mut();
        if let Some(kind) = stream.read_error {
            return Poll::Ready(Err(io::Error::new(kind, "injected read failure")));
        }
        if stream.offset == stream.input.len() {
            return Poll::Pending;
        }
        let count = (stream.input.len() - stream.offset).min(buffer.remaining());
        buffer.put_slice(&stream.input[stream.offset..stream.offset + count]);
        stream.offset += count;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for FaultStream {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let stream = self.get_mut();
        if let Some(kind) = stream.write_error {
            return Poll::Ready(Err(io::Error::new(kind, "injected write failure")));
        }
        Poll::Ready(Ok(buffer.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let stream = self.get_mut();
        if let Some(kind) = stream.flush_error {
            return Poll::Ready(Err(io::Error::new(kind, "injected flush failure")));
        }
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn version_frame() -> Vec<u8> {
    encode_message(P9_TVERSION, P9_NOTAG, 8192, |writer| {
        write_tversion(
            writer,
            &Tversion {
                msize: 8192,
                version: "9P2000.L".to_owned(),
            },
        )
    })
    .expect("encode version request")
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

#[tokio::test]
async fn unexpected_read_error_reports_once() {
    let (events, hooks) = Events::new();
    let server = Arc::new(P9Server::new_with_hooks(
        MemoryFs::empty(),
        Default::default(),
        hooks,
    ));
    let connection = server
        .attach(
            FaultStream::new(Vec::new(), Some(io::ErrorKind::Other), None, None),
            P9AttachOptions {
                peer: Some("read-failure".to_owned()),
                own: true,
            },
        )
        .expect("attach read-failure stream");

    events.wait_for(1).await;
    timeout(Duration::from_secs(2), connection.wait_closed())
        .await
        .expect("read-failure connection closes");
    sleep(Duration::from_millis(20)).await;
    let observed = events.snapshot();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].kind, P9TransportErrorKind::Read);
    assert_eq!(observed[0].peer.as_deref(), Some("read-failure"));
    server.close().await.expect("close 9P test server");
}

#[tokio::test]
async fn unexpected_write_and_flush_errors_report_once() {
    for (label, write_error, flush_error) in [
        ("write-failure", Some(io::ErrorKind::Other), None),
        ("flush-failure", None, Some(io::ErrorKind::Other)),
    ] {
        let (events, hooks) = Events::new();
        let server = Arc::new(P9Server::new_with_hooks(
            MemoryFs::empty(),
            Default::default(),
            hooks,
        ));
        let connection = server
            .attach(
                FaultStream::new(version_frame(), None, write_error, flush_error),
                P9AttachOptions {
                    peer: Some(label.to_owned()),
                    own: true,
                },
            )
            .expect("attach write-failure stream");

        events.wait_for(1).await;
        timeout(Duration::from_secs(2), connection.wait_closed())
            .await
            .expect("write-failure connection closes");
        let observed = events.snapshot();
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].kind, P9TransportErrorKind::Write);
        assert_eq!(observed[0].peer.as_deref(), Some(label));
        server.close().await.expect("close 9P test server");
    }
}

#[tokio::test]
async fn expected_reset_and_broken_pipe_write_errors_are_silent() {
    let (events, hooks) = Events::new();
    let server = Arc::new(P9Server::new_with_hooks(
        MemoryFs::empty(),
        Default::default(),
        hooks,
    ));
    let reset_connection = server
        .attach(
            FaultStream::new(Vec::new(), Some(io::ErrorKind::ConnectionReset), None, None),
            P9AttachOptions {
                peer: Some("reset-read".to_owned()),
                own: true,
            },
        )
        .expect("attach reset-read stream");
    let broken_pipe_connection = server
        .attach(
            FaultStream::new(version_frame(), None, Some(io::ErrorKind::BrokenPipe), None),
            P9AttachOptions {
                peer: Some("broken-pipe-write".to_owned()),
                own: true,
            },
        )
        .expect("attach broken-pipe stream");
    let broken_pipe_flush_connection = server
        .attach(
            FaultStream::new(version_frame(), None, None, Some(io::ErrorKind::BrokenPipe)),
            P9AttachOptions {
                peer: Some("broken-pipe-flush".to_owned()),
                own: true,
            },
        )
        .expect("attach broken-pipe flush stream");

    for connection in [
        reset_connection,
        broken_pipe_connection,
        broken_pipe_flush_connection,
    ] {
        timeout(Duration::from_secs(2), connection.wait_closed())
            .await
            .expect("expected-disconnect connection closes");
    }
    sleep(Duration::from_millis(20)).await;
    assert!(events.snapshot().is_empty());
    server.close().await.expect("close 9P test server");
}

#[tokio::test]
async fn concurrent_response_failures_report_once() {
    let (events, hooks) = Events::new();
    let options = mount_rs_9p::P9ServerOptions {
        max_in_flight: 2,
        ..Default::default()
    };
    let server = Arc::new(P9Server::new_with_hooks(MemoryFs::empty(), options, hooks));
    let mut input = version_frame();
    input.extend_from_slice(&version_frame());
    let connection = server
        .attach(
            FaultStream::new(input, None, Some(io::ErrorKind::Other), None),
            P9AttachOptions {
                peer: Some("concurrent-write-failure".to_owned()),
                own: true,
            },
        )
        .expect("attach concurrent-failure stream");

    events.wait_for(1).await;
    timeout(Duration::from_secs(2), connection.wait_closed())
        .await
        .expect("concurrent-failure connection closes");
    sleep(Duration::from_millis(20)).await;
    assert_eq!(events.snapshot().len(), 1);
    server.close().await.expect("close 9P test server");
}

#[tokio::test]
async fn explicit_connection_close_is_quiet() {
    let (events, hooks) = Events::new();
    let server = Arc::new(P9Server::new_with_hooks(
        MemoryFs::empty(),
        Default::default(),
        hooks,
    ));
    let (server_stream, _client_stream) = tokio::io::duplex(1024);
    let connection = server
        .attach(
            server_stream,
            P9AttachOptions {
                peer: Some("explicit-close".to_owned()),
                own: false,
            },
        )
        .expect("attach explicit-close stream");

    connection.close().await.expect("close 9P connection");
    sleep(Duration::from_millis(20)).await;
    assert!(events.snapshot().is_empty());
    server.close().await.expect("close 9P test server");
}
