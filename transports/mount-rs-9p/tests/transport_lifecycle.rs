use std::future::Future;
#[cfg(unix)]
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
#[cfg(unix)]
use std::time::{SystemTime, UNIX_EPOCH};

use mount_rs_9p::{
    P9_NOFID, P9_NOTAG, P9_RATTACH, P9_RVERSION, P9_TATTACH, P9_TVERSION, P9AttachOptions, P9Error,
    P9Server, P9ServerOptions, P9Writer, Tattach, Tversion, decode_message, encode_message,
    read_rattach, read_rversion, write_tattach, write_tversion,
};
use mount_rs_core::MemoryFs;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
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
