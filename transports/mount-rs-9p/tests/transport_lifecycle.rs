use std::future::Future;
#[cfg(unix)]
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_9p::{
    FidOpenState, P9_GETATTR_BASIC, P9_NOFID, P9_NOTAG, P9_RATTACH, P9_RFLUSH, P9_RGETATTR,
    P9_RLERROR, P9_RVERSION, P9_TATTACH, P9_TFLUSH, P9_TGETATTR, P9_TVERSION, P9AttachOptions,
    P9Error, P9Server, P9ServerOptions, P9Writer, Tattach, Tflush, Tgetattr, Tversion,
    decode_message, encode_message, read_rattach, read_rlerror, read_rversion, write_tattach,
    write_tflush, write_tgetattr, write_tversion,
};
use mount_rs_core::{ErrorCode, FileHandle, FsError, OpenFlags, Result as FsResult, Stats};
use mount_rs_memfs::MemoryFs;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::{sleep, timeout};

fn frame<F>(type_: u8, tag: u16, write: F) -> Vec<u8>
where
    F: FnOnce(&mut P9Writer) -> Result<(), P9Error>,
{
    encode_message(type_, tag, 8192, write).expect("test message encodes")
}

fn version_request() -> Vec<u8> {
    frame(P9_TVERSION, P9_NOTAG, |writer| {
        write_tversion(
            writer,
            &Tversion {
                msize: 8192,
                version: "9P2000.L".to_owned(),
            },
        )
    })
}

fn attach_request() -> Vec<u8> {
    frame(P9_TATTACH, 1, |writer| {
        write_tattach(
            writer,
            &Tattach {
                fid: 1,
                afid: P9_NOFID,
                uname: "transport-test".to_owned(),
                aname: String::new(),
                n_uname: u32::MAX,
            },
        )
    })
}

async fn read_frame<R>(stream: &mut R) -> Vec<u8>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; 7];
    stream.read_exact(&mut header).await.expect("9P header");
    let size = u32::from_le_bytes(header[..4].try_into().expect("size bytes")) as usize;
    assert!(size >= header.len());
    let mut frame = Vec::with_capacity(size);
    frame.extend_from_slice(&header);
    frame.resize(size, 0);
    stream
        .read_exact(&mut frame[header.len()..])
        .await
        .expect("9P body");
    frame
}

async fn handshake<S>(stream: &mut S)
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    stream
        .write_all(&version_request())
        .await
        .expect("send version");
    let response = read_frame(stream).await;
    assert_eq!(
        decode_message(&response).expect("version response").0.type_,
        P9_RVERSION
    );
    let (_, mut body) = decode_message(&response).expect("version frame");
    read_rversion(&mut body).expect("decode Rversion");

    stream
        .write_all(&attach_request())
        .await
        .expect("send attach");
    let response = read_frame(stream).await;
    assert_eq!(
        decode_message(&response).expect("attach response").0.type_,
        P9_RATTACH
    );
    let (_, mut body) = decode_message(&response).expect("attach frame");
    read_rattach(&mut body).expect("decode Rattach");
}

#[cfg(unix)]
fn test_directory(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "mount-rs-9p-{label}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir(&directory).expect("create test directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, PermissionsExt::from_mode(0o700))
            .expect("make test directory private");
    }
    directory
}

async fn wait_for_no_connections(server: &P9Server) {
    timeout(Duration::from_secs(2), async {
        loop {
            if server.connection_count().expect("connection count") == 0 {
                return;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("connection task exits");
}

async fn wait_for_connections(server: &P9Server, expected: usize) {
    timeout(Duration::from_secs(2), async {
        loop {
            if server.connection_count().expect("connection count") == expected {
                return;
            }
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("connection count reaches expected value");
}

struct BlockingStatDriver {
    inner: MemoryFs,
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

struct BlockingCloseHandle {
    entered: Arc<Notify>,
    release: Arc<Notify>,
}

struct PartialReplyControl {
    stage: AtomicU8,
    prefix_written: Notify,
    stalled_waker: StdMutex<Option<Waker>>,
}

impl PartialReplyControl {
    fn new() -> Self {
        Self {
            stage: AtomicU8::new(0),
            prefix_written: Notify::new(),
            stalled_waker: StdMutex::new(None),
        }
    }

    fn release(&self) {
        self.stage.store(3, Ordering::Release);
        if let Some(waker) = self.stalled_waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}

struct PartialReplyStream {
    inner: tokio::io::DuplexStream,
    control: Arc<PartialReplyControl>,
}

impl AsyncRead for PartialReplyStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for PartialReplyStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if self.control.stage.load(Ordering::Acquire) == 1 {
            let written = std::task::ready!(Pin::new(&mut self.inner).poll_write(cx, &buf[..4]));
            if written.as_ref().is_ok_and(|written| *written > 0) {
                self.control.stage.store(2, Ordering::Release);
                self.control.prefix_written.notify_one();
            }
            return Poll::Ready(written);
        }
        if self.control.stage.load(Ordering::Acquire) == 2 {
            *self.control.stalled_waker.lock().unwrap() = Some(cx.waker().clone());
            if self.control.stage.load(Ordering::Acquire) == 2 {
                return Poll::Pending;
            }
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

impl FileHandle for BlockingCloseHandle {
    fn read<'a, 'b, 'async_trait>(
        &'a self,
        _buffer: &'b mut [u8],
        _position: Option<u64>,
    ) -> Pin<Box<dyn Future<Output = FsResult<usize>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Err(FsError::new(ErrorCode::Enosys)) })
    }

    fn write<'a, 'b, 'async_trait>(
        &'a self,
        _buffer: &'b [u8],
        _position: Option<u64>,
    ) -> Pin<Box<dyn Future<Output = FsResult<usize>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Err(FsError::new(ErrorCode::Enosys)) })
    }

    fn stat<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = FsResult<Stats>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Err(FsError::new(ErrorCode::Enosys)) })
    }

    fn truncate<'a, 'async_trait>(
        &'a self,
        _length: u64,
    ) -> Pin<Box<dyn Future<Output = FsResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async { Err(FsError::new(ErrorCode::Enosys)) })
    }

    fn close<'a, 'async_trait>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = FsResult<()>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let release = self.release.notified();
            tokio::pin!(release);
            release.as_mut().enable();
            self.entered.notify_one();
            release.await;
            Ok(())
        })
    }
}

impl mount_rs_core::FsDriver for BlockingStatDriver {
    fn capabilities(&self) -> mount_rs_core::Capabilities {
        self.inner.capabilities()
    }

    fn stat<'a, 'b, 'async_trait>(
        &'a self,
        path: &'b str,
    ) -> Pin<
        Box<dyn Future<Output = mount_rs_core::Result<mount_rs_core::Stats>> + Send + 'async_trait>,
    >
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.entered.notify_one();
            self.release.notified().await;
            self.inner.stat(path).await
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

#[tokio::test]
async fn attached_stream_serves_frames_and_closes_without_a_listener() {
    let server = Arc::new(P9Server::new(MemoryFs::empty(), P9ServerOptions::default()));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(
            server_stream,
            P9AttachOptions {
                peer: Some("attached-test".to_owned()),
                own: false,
            },
        )
        .expect("attach stream");
    assert_eq!(connection.peer.as_deref(), Some("attached-test"));
    assert_eq!(server.connection_count().expect("connection count"), 1);

    handshake(&mut client_stream).await;
    connection.close().await.expect("close attached connection");
    timeout(Duration::from_secs(2), connection.wait_closed())
        .await
        .expect("attached connection closes");
    wait_for_no_connections(&server).await;
    server.close().await.expect("close attach-only server");
}

#[tokio::test]
async fn request_before_coalesced_version_is_rejected_in_arrival_order() {
    let server = Arc::new(P9Server::new(
        MemoryFs::empty(),
        P9ServerOptions {
            max_in_flight: 1,
            ..Default::default()
        },
    ));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(server_stream, P9AttachOptions::default())
        .expect("attach one-read client");
    let burst = [&attach_request()[..], &version_request()[..]].concat();
    client_stream
        .write_all(&burst)
        .await
        .expect("send one burst");

    let first = timeout(Duration::from_secs(2), read_frame(&mut client_stream))
        .await
        .expect("first response");
    assert_eq!(decode_message(&first).unwrap().0.type_, P9_RLERROR);
    let (_, mut body) = decode_message(&first).unwrap();
    assert_eq!(
        read_rlerror(&mut body).unwrap().ecode,
        ErrorCode::Eproto.errno().unsigned_abs()
    );
    let second = timeout(Duration::from_secs(2), read_frame(&mut client_stream))
        .await
        .expect("version response");
    assert_eq!(decode_message(&second).unwrap().0.type_, P9_RVERSION);
    assert!(connection.session.fid_ids().is_empty());

    connection.close().await.unwrap();
    server.close().await.unwrap();
}

#[tokio::test]
async fn coalesced_renegotiation_discards_earlier_queued_request() {
    let server = Arc::new(P9Server::new(
        MemoryFs::empty(),
        P9ServerOptions {
            max_in_flight: 1,
            ..Default::default()
        },
    ));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(server_stream, P9AttachOptions::default())
        .unwrap();
    client_stream.write_all(&version_request()).await.unwrap();
    assert_eq!(
        decode_message(&read_frame(&mut client_stream).await)
            .unwrap()
            .0
            .type_,
        P9_RVERSION
    );
    let burst = [&attach_request()[..], &version_request()[..]].concat();
    client_stream.write_all(&burst).await.unwrap();

    let response = timeout(Duration::from_secs(2), read_frame(&mut client_stream))
        .await
        .expect("renegotiation response");
    assert_eq!(decode_message(&response).unwrap().0.type_, P9_RVERSION);
    assert!(connection.session.fid_ids().is_empty());
    assert_eq!(connection.session.stats().messages.get("Tattach"), None);

    connection.close().await.unwrap();
    server.close().await.unwrap();
}

#[tokio::test]
async fn renegotiation_aborts_stalled_old_request_and_replies_without_release() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let server = Arc::new(P9Server::new(
        BlockingStatDriver {
            inner: MemoryFs::empty(),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        P9ServerOptions::default(),
    ));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(server_stream, P9AttachOptions::default())
        .unwrap();
    client_stream.write_all(&version_request()).await.unwrap();
    assert_eq!(
        decode_message(&read_frame(&mut client_stream).await)
            .unwrap()
            .0
            .type_,
        P9_RVERSION
    );
    connection.session.fid_create(1, "/").unwrap();
    let blocked = frame(P9_TGETATTR, 7, |writer| {
        write_tgetattr(
            writer,
            Tgetattr {
                fid: 1,
                request_mask: P9_GETATTR_BASIC,
            },
        );
        Ok(())
    });
    client_stream.write_all(&blocked).await.unwrap();
    timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("old request enters blocked driver");
    client_stream.write_all(&version_request()).await.unwrap();

    let result = timeout(Duration::from_millis(250), read_frame(&mut client_stream)).await;
    if result.is_err() {
        connection.close().await.unwrap();
        release.notify_waiters();
        server.close().await.unwrap();
        panic!("Rversion waited for an outstanding old request");
    }
    assert_eq!(
        decode_message(&result.unwrap()).unwrap().0.type_,
        P9_RVERSION
    );
    assert!(connection.session.fid_ids().is_empty());
    assert_eq!(
        connection.session.inflight(),
        0,
        "version must drain old tags"
    );
    let flush = frame(P9_TFLUSH, 8, |writer| {
        write_tflush(writer, Tflush { oldtag: 7 });
        Ok(())
    });
    client_stream.write_all(&flush).await.unwrap();
    let response = timeout(Duration::from_millis(250), read_frame(&mut client_stream))
        .await
        .expect("flush after version must not wait on an aborted old tag");
    assert_eq!(decode_message(&response).unwrap().0.type_, P9_RFLUSH);
    connection.session.fid_create(1, "/").unwrap();
    client_stream.write_all(&blocked).await.unwrap();
    release.notify_one();
    let response = timeout(Duration::from_millis(250), read_frame(&mut client_stream))
        .await
        .expect("tag reuse after version completes");
    assert_eq!(decode_message(&response).unwrap().0.type_, P9_RGETATTR);
    connection.close().await.unwrap();
    release.notify_waiters();
    server.close().await.unwrap();
}

#[tokio::test]
async fn close_preempts_renegotiation_stalled_in_handle_close() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let server = Arc::new(P9Server::new(MemoryFs::empty(), P9ServerOptions::default()));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(server_stream, P9AttachOptions::default())
        .expect("attach close-stall client");
    client_stream.write_all(&version_request()).await.unwrap();
    assert_eq!(
        decode_message(&read_frame(&mut client_stream).await)
            .unwrap()
            .0
            .type_,
        P9_RVERSION
    );
    connection.session.fid_create(77, "/held").unwrap();
    connection
        .session
        .fid_set_open(
            77,
            Some(FidOpenState {
                flags: OpenFlags::READ_ONLY,
                wire_flags: 0,
                handle: Some(Arc::new(BlockingCloseHandle {
                    entered: Arc::clone(&entered),
                    release: Arc::clone(&release),
                })),
                directory: false,
                qid: None,
            }),
        )
        .unwrap();
    client_stream.write_all(&version_request()).await.unwrap();
    timeout(Duration::from_secs(1), entered.notified())
        .await
        .expect("renegotiation reaches handle close");

    let closing = connection.clone();
    let mut close_task = tokio::spawn(async move { closing.close().await });
    let close_result = timeout(Duration::from_millis(250), &mut close_task).await;
    if close_result.is_err() {
        release.notify_waiters();
        let _ = timeout(Duration::from_secs(2), close_task).await;
        panic!("connection close waited for a stalled version reset");
    }
    close_result.unwrap().unwrap().unwrap();
    server.close().await.unwrap();
}

#[tokio::test]
async fn renegotiation_finishes_a_started_old_reply_before_rversion() {
    let control = Arc::new(PartialReplyControl::new());
    let close_entered = Arc::new(Notify::new());
    let close_release = Arc::new(Notify::new());
    let server = Arc::new(P9Server::new(MemoryFs::empty(), P9ServerOptions::default()));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(
            PartialReplyStream {
                inner: server_stream,
                control: Arc::clone(&control),
            },
            P9AttachOptions::default(),
        )
        .unwrap();
    handshake(&mut client_stream).await;
    connection
        .session
        .fid_set_open(
            1,
            Some(FidOpenState {
                flags: OpenFlags::READ_ONLY,
                wire_flags: 0,
                handle: Some(Arc::new(BlockingCloseHandle {
                    entered: Arc::clone(&close_entered),
                    release: Arc::clone(&close_release),
                })),
                directory: false,
                qid: None,
            }),
        )
        .unwrap();
    control.stage.store(1, Ordering::Release);
    let getattr = frame(P9_TGETATTR, 7, |writer| {
        write_tgetattr(
            writer,
            Tgetattr {
                fid: 1,
                request_mask: P9_GETATTR_BASIC,
            },
        );
        Ok(())
    });
    client_stream.write_all(&getattr).await.unwrap();
    timeout(Duration::from_secs(1), control.prefix_written.notified())
        .await
        .expect("the old reply writes a prefix");
    client_stream.write_all(&version_request()).await.unwrap();
    timeout(Duration::from_secs(1), close_entered.notified())
        .await
        .expect("the version request passes the abort boundary");
    control.release();
    close_release.notify_waiters();

    let old_reply = timeout(Duration::from_millis(250), read_frame(&mut client_stream))
        .await
        .expect("the started old reply completes before Rversion");
    assert_eq!(decode_message(&old_reply).unwrap().0.type_, P9_RGETATTR);
    let version = timeout(Duration::from_millis(250), read_frame(&mut client_stream))
        .await
        .expect("Rversion follows the complete old frame");
    assert_eq!(decode_message(&version).unwrap().0.type_, P9_RVERSION);

    connection.close().await.unwrap();
    server.close().await.unwrap();
}

#[tokio::test]
async fn start_then_immediate_close_stops_the_accept_loop() {
    let server = Arc::new(
        P9Server::bind(MemoryFs::empty(), P9ServerOptions::default())
            .await
            .expect("bind TCP listener"),
    );
    let task = server.start().expect("start accept loop");

    server.close().await.expect("close server");
    timeout(Duration::from_secs(2), task)
        .await
        .expect("accept loop stops after close")
        .expect("accept loop joins")
        .expect("accept loop exits cleanly");
}

#[tokio::test]
async fn close_stops_accept_loop_with_an_active_connection() {
    let server = Arc::new(
        P9Server::bind(MemoryFs::empty(), P9ServerOptions::default())
            .await
            .expect("bind TCP listener"),
    );
    let task = server.start().expect("start accept loop");
    let mut client = TcpStream::connect(server.local_addr().expect("server address"))
        .await
        .expect("connect TCP listener");
    wait_for_connections(&server, 1).await;

    server.close().await.expect("close server");
    timeout(Duration::from_secs(2), task)
        .await
        .expect("accept loop stops with active connection")
        .expect("accept loop joins")
        .expect("accept loop exits cleanly");
    wait_for_no_connections(&server).await;

    let mut byte = [0_u8; 1];
    let read = timeout(Duration::from_secs(2), client.read(&mut byte))
        .await
        .expect("closed server reaches TCP peer")
        .expect("read TCP peer");
    assert_eq!(read, 0);
}

#[tokio::test]
async fn shutdown_broadcasts_to_all_attached_connections() {
    let server = Arc::new(P9Server::new(MemoryFs::empty(), P9ServerOptions::default()));
    let mut connections = Vec::new();
    let mut peers = Vec::new();
    for index in 0..3 {
        let (server_stream, client_stream) = tokio::io::duplex(1024);
        peers.push(client_stream);
        connections.push(
            server
                .attach(
                    server_stream,
                    P9AttachOptions {
                        peer: Some(format!("shutdown-{index}")),
                        own: false,
                    },
                )
                .expect("attach connection"),
        );
    }
    assert_eq!(server.connection_count().expect("connection count"), 3);

    server.shutdown();
    for connection in &connections {
        timeout(Duration::from_secs(2), connection.wait_closed())
            .await
            .expect("shutdown closes every connection");
    }
    wait_for_no_connections(&server).await;
    drop(peers);
    server.close().await.expect("close attach-only server");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_interrupts_permit_wait_without_waiting_for_a_slow_request() {
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let server = Arc::new(P9Server::new(
        BlockingStatDriver {
            inner: MemoryFs::empty(),
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        P9ServerOptions {
            max_in_flight: 1,
            ..P9ServerOptions::default()
        },
    ));
    let (server_stream, mut client_stream) = tokio::io::duplex(64 * 1024);
    let connection = server
        .attach(
            server_stream,
            P9AttachOptions {
                peer: Some("slow-permit".to_owned()),
                own: false,
            },
        )
        .expect("attach slow-permit stream");

    client_stream
        .write_all(&version_request())
        .await
        .expect("send version");
    let version = read_frame(&mut client_stream).await;
    assert_eq!(
        decode_message(&version).expect("version response").0.type_,
        P9_RVERSION
    );

    let mut burst = attach_request();
    burst.extend_from_slice(&attach_request());
    client_stream
        .write_all(&burst)
        .await
        .expect("send blocked attach burst");
    timeout(Duration::from_secs(2), entered.notified())
        .await
        .expect("first attach reaches the blocked driver");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let server_for_close = Arc::clone(&server);
    let mut close_task = tokio::spawn(async move { server_for_close.close().await });
    let close_result = timeout(Duration::from_millis(250), &mut close_task).await;
    if close_result.is_err() {
        release.notify_waiters();
        let _ = timeout(Duration::from_secs(2), close_task).await;
        panic!("server close remained blocked on the in-flight permit");
    }
    release.notify_waiters();
    close_result
        .expect("close completed within the bounded shutdown window")
        .expect("close task joins")
        .expect("close succeeds");
    assert!(connection.is_closed());
}

#[cfg(unix)]
#[tokio::test]
async fn unix_listener_accepts_rootless_protocol_and_removes_socket_on_close() {
    use tokio::net::UnixStream;

    let directory = test_directory("unix");
    let socket = directory.join("9p.sock");
    let server = Arc::new(
        P9Server::bind_unix(MemoryFs::empty(), &socket, P9ServerOptions::default())
            .await
            .expect("bind Unix 9P listener"),
    );
    let serving = Arc::clone(&server);
    let task = tokio::spawn(async move { serving.serve().await });
    let mut client = UnixStream::connect(&socket)
        .await
        .expect("connect Unix 9P listener");

    handshake(&mut client).await;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&socket)
            .expect("socket metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    server.close().await.expect("close Unix 9P listener");
    timeout(Duration::from_secs(2), task)
        .await
        .expect("Unix serve task stops")
        .expect("Unix serve task joins")
        .expect("Unix serve task exits cleanly");
    assert!(!socket.exists(), "server close removes its Unix socket");
    let _ = client.shutdown().await;
    std::fs::remove_dir(directory).expect("remove test directory");
}
