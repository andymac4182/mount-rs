//! A completed durable write whose actual TLS WebSocket reply is lost in transit.
//! The relay never replaces the production authenticator, dispatcher or driver.

use super::{Keys, token};
use futures_util::{SinkExt, StreamExt};
use mount_rs_core::{
    Stats,
    storage::{BlockStore, InodeModeState, MetadataStore, NodeData, compact::CompactSnapshot},
};
use mount_rs_remote_client::{
    connection::{ClientError, ConnectionTransport, RemoteConnection},
    credentials::CredentialSource,
    websocket::{CHUNK_BYTES, SUBPROTOCOL},
};
use mount_rs_remote_protocol::{
    Message, Operation, OperationName, PROTOCOL_VERSION,
    binary::{self, Header, Incoming, Kind},
};
use mount_rs_sdk::{
    Filesystem, SplitOptions, SqliteJournalMode, SqliteStorageOptions, StoreConfig,
};
use mount_rs_service::{
    auth::CatalogAuthenticator,
    catalog::{
        CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        SqliteCatalog,
    },
    dispatch::DriveDispatcher,
    websocket::WebSocketServer,
};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use serde_json::json;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream, UdpSocket},
    sync::oneshot,
    task::JoinHandle,
};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{
        Bytes, Message as WsMessage,
        client::IntoClientRequest,
        handshake::server::{Callback, ErrorResponse, Request, Response},
        protocol::WebSocketConfig,
    },
};

type TestResult<T> = Result<T, String>;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Selection {
    WebSocket,
    Auto,
}

impl Selection {
    fn label(self) -> &'static str {
        match self {
            Self::WebSocket => "websocket",
            Self::Auto => "auto",
        }
    }
}
#[derive(PartialEq, Eq)]
struct CompactCheckpoint {
    mode: InodeModeState,
    snapshot: CompactSnapshot,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Storage {
    Snapshot,
    Compact,
}

impl Storage {
    fn compact_options(database: &Path) -> SplitOptions {
        let mut options =
            SplitOptions::memory("reply-loss-compact", 4096).with_compact_inode_updates(true);
        let sqlite_options = SqliteStorageOptions {
            journal_mode: SqliteJournalMode::Wal,
        };
        options.metadata = StoreConfig::SqliteWithOptions {
            path: database.to_path_buf(),
            options: sqlite_options,
        };
        options.blocks = StoreConfig::SqliteWithOptions {
            path: database.with_file_name("blocks.sqlite"),
            options: sqlite_options,
        };
        options
    }

    fn definition(self, database: &Path) -> serde_json::Value {
        match self {
            Self::Snapshot => json!({"kind":"sqlite","database":database}),
            Self::Compact => json!({"kind":"splitstore","storage":{
                "metadata":{"kind":"sqlite","path":database,"journal_mode":"wal"},
                "blocks":{"kind":"sqlite","path":database.with_file_name("blocks.sqlite"),"journal_mode":"wal"},
                "chunk_size_bytes":4096,"compact_inode_updates":true
            }}),
        }
    }

    async fn open(self, database: &Path) -> TestResult<Filesystem> {
        let opened = match self {
            Self::Snapshot => Filesystem::sqlite(database).await,
            Self::Compact => Filesystem::split(Self::compact_options(database)).await,
        }
        .map_err(|_| "durable fixture SQLite open failed")?;
        ensure(
            self != Self::Compact || opened.persistent_eviction_allowed(),
            "actual compact fixture did not select durable MRC5 storage",
        )?;
        Ok(opened)
    }

    async fn compact_checkpoint(
        self,
        database: &Path,
        stats: &Stats,
    ) -> TestResult<Option<CompactCheckpoint>> {
        if self == Self::Snapshot {
            return Ok(None);
        }
        // The actual provider inspector returns Some only for persisted MRC5
        // and validates its physical metadata stamp and authority. It does not
        // enroll, publish, repair a marker or replay the intercepted write.
        let metadata = SqliteMetadataStore::open(database)
            .map_err(|_| "fresh compact metadata open failed")?;
        let mode = metadata
            .compact_inode_mode_state()
            .await
            .map_err(|_| "fresh compact mode inspection failed")?
            .ok_or("fresh metadata is not persisted MRC5")?;
        let blocks = SqliteBlockStore::open(database.with_file_name("blocks.sqlite"))
            .map_err(|_| "fresh compact block store open failed")?;
        blocks
            .verify_concurrent_backing(mode.backing)
            .await
            .map_err(|_| "fresh compact backing verification failed")?;
        let snapshot = metadata
            .load_compact_snapshot(mode.backing)
            .await
            .map_err(|_| "fresh compact snapshot read failed")?;
        ensure(
            snapshot.anchor.generation == mode.structural_generation
                && snapshot.anchor.members.len() == 2,
            "fresh compact anchor differs from exact fixture membership",
        )?;
        let namespace = snapshot
            .namespace()
            .map_err(|_| "fresh compact namespace validation failed")?;
        ensure(
            namespace.nodes.len() == 2
                && namespace.nodes.get(&stats.ino).is_some_and(|node| {
                    node.stats == *stats && matches!(node.data, NodeData::File(_))
                })
                && namespace.nodes.get(&namespace.root).is_some_and(|node| {
                    matches!(&node.data, NodeData::Directory { entries }
                        if entries.len() == 1 && entries[0].name == FILE.trim_start_matches('/')
                            && entries[0].inode == stats.ino)
                }),
            "fresh compact namespace or complete target metadata mismatch",
        )?;
        Ok(Some(CompactCheckpoint { mode, snapshot }))
    }
}

const STEP_TIMEOUT: Duration = Duration::from_secs(3);
const REPLY_HOLD_TIMEOUT: Duration = Duration::from_secs(10);
const WORK_TIMEOUT: Duration = Duration::from_secs(30);
const QUIET_WINDOW: Duration = Duration::from_millis(100);
const FILE: &str = "/reply-lost";
const MODE: u32 = 0o640;
const PAYLOAD_BYTES: usize = CHUNK_BYTES * 2 + 137;
const MAX_ENVELOPE_BODY: usize = 128 * 1024;

fn ensure(condition: bool, reason: &'static str) -> TestResult<()> {
    condition.then_some(()).ok_or_else(|| reason.into())
}

fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(CHUNK_BYTES * 2)
        .max_message_size(Some(CHUNK_BYTES))
        .max_frame_size(Some(CHUNK_BYTES))
}

#[allow(clippy::result_large_err)]
fn upgrade(request: &Request, mut response: Response) -> Result<Response, ErrorResponse> {
    if request.uri().path() != "/mount-rs"
        || request
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok())
            != Some(SUBPROTOCOL)
    {
        let mut denial = ErrorResponse::new(Some("unsupported fixture protocol".into()));
        *denial.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::BAD_REQUEST;
        return Err(denial);
    }
    response
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", SUBPROTOCOL.parse().unwrap());
    Ok(response)
}

struct Envelope {
    header: Header,
    body: Vec<u8>,
    messages: Vec<Bytes>,
}

async fn next_binary<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> TestResult<Bytes> {
    loop {
        match socket.next().await {
            Some(Ok(WsMessage::Binary(bytes))) => return Ok(bytes),
            Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {
                socket
                    .flush()
                    .await
                    .map_err(|_| "relay ping flush failed")?;
            }
            _ => return Err("relay socket closed before a complete envelope".into()),
        }
    }
}

async fn envelope<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
) -> TestResult<Envelope> {
    tokio::time::timeout(STEP_TIMEOUT, async {
        let first = next_binary(socket).await?;
        let header = Header::decode(
            first
                .as_ref()
                .try_into()
                .map_err(|_| "relay header length mismatch")?,
        )
        .map_err(|_| "relay invalid MRB2 header")?;
        let length = header.control_len + header.payload_len;
        ensure(
            length <= MAX_ENVELOPE_BODY,
            "relay body exceeds fixture bound",
        )?;
        let mut messages = vec![first];
        let mut body = Vec::with_capacity(length);
        while body.len() < length {
            let chunk = next_binary(socket).await?;
            ensure(
                !chunk.is_empty() && chunk.len() <= length - body.len(),
                "relay body chunk length mismatch",
            )?;
            body.extend_from_slice(&chunk);
            messages.push(chunk);
        }
        let terminator = next_binary(socket).await?;
        ensure(terminator.is_empty(), "relay missing empty terminator")?;
        messages.push(terminator);
        Ok(Envelope {
            header,
            body,
            messages,
        })
    })
    .await
    .map_err(|_| "relay envelope deadline exceeded")?
}

async fn forward<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    envelope: &Envelope,
) -> TestResult<()> {
    for message in &envelope.messages {
        socket
            .send(WsMessage::Binary(message.clone()))
            .await
            .map_err(|_| "relay envelope forwarding failed")?;
    }
    Ok(())
}

fn control(envelope: &Envelope) -> TestResult<Message> {
    ensure(
        envelope.header.kind == Kind::Control,
        "relay expected control response",
    )?;
    serde_json::from_slice(&envelope.body).map_err(|_| "relay invalid control response".into())
}

struct RelayReceipt {
    write_submissions: u64,
    committed_reply_bytes: usize,
    committed_write_count: usize,
    suppressed_messages: usize,
}

struct ReplyBarrier {
    contacted: oneshot::Sender<Instant>,
    held: oneshot::Sender<usize>,
    release: oneshot::Receiver<()>,
}

/// Keep observing the caller while the complete durable reply remains owned.
/// A ready application message must win over an already-ready release signal.
async fn hold_reply_until_release<S: AsyncRead + AsyncWrite + Unpin>(
    downstream: &mut WebSocketStream<S>,
    mut release: oneshot::Receiver<()>,
) -> TestResult<()> {
    tokio::time::timeout(REPLY_HOLD_TIMEOUT, async {
        loop {
            tokio::select! {
                biased;
                message = downstream.next() => match message {
                    Some(Ok(WsMessage::Ping(_) | WsMessage::Pong(_))) => {
                        downstream.flush().await
                            .map_err(|_| "held reply protocol flush failed")?;
                        // Control traffic cannot keep this future continuously
                        // ready and prevent its original timeout being polled.
                        tokio::task::yield_now().await;
                    }
                    Some(Ok(WsMessage::Binary(_))) => {
                        return Err("application message arrived while the write reply was held".into());
                    }
                    _ => return Err("caller closed or sent invalid traffic while the write reply was held".into()),
                },
                released = &mut release => {
                    return released.map_err(|_| "held reply release was canceled".into());
                }
            }
        }
    })
    .await
    .map_err(|_| "held reply release deadline exceeded")?
}

struct DeferredCredential {
    path: PathBuf,
    token: String,
    issues: Arc<AtomicU64>,
}

struct RelayUpgrade(Option<DeferredCredential>);

impl Callback for RelayUpgrade {
    // Tungstenite requires this concrete HTTP error response in its callback.
    #[allow(clippy::result_large_err)]
    fn on_request(self, request: &Request, response: Response) -> Result<Response, ErrorResponse> {
        let response = upgrade(request, response)?;
        if let Some(credential) = self.0 {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let issued = options.open(&credential.path).and_then(|mut file| {
                std::io::Write::write_all(&mut file, credential.token.as_bytes())
            });
            if issued.is_err() {
                return Err(ErrorResponse::new(Some(
                    "deferred fixture credential unavailable".into(),
                )));
            }
            credential.issues.fetch_add(1, Ordering::Relaxed);
        }
        Ok(response)
    }
}

struct PrecloseReceipt {
    verified_bytes: usize,
    eof_count: usize,
    stats: Stats,
    compact_checkpoint: Option<CompactCheckpoint>,
}

async fn relay(
    listener: Arc<TcpListener>,
    acceptor: tokio_rustls::TlsAcceptor,
    address: SocketAddr,
    roots: rustls::RootCertStore,
    expected: Arc<Vec<u8>>,
    barrier: ReplyBarrier,
    credential: Option<DeferredCredential>,
) -> TestResult<RelayReceipt> {
    let (tcp, _) = listener.accept().await.map_err(|_| "relay accept failed")?;
    barrier
        .contacted
        .send(Instant::now())
        .map_err(|_| "relay contact observer disappeared")?;
    tcp.set_nodelay(true)
        .map_err(|_| "relay TCP setup failed")?;
    let tls = acceptor
        .accept(tcp)
        .await
        .map_err(|_| "relay TLS accept failed")?;
    let mut downstream = tokio_tungstenite::accept_hdr_async_with_config(
        tls,
        RelayUpgrade(credential),
        Some(socket_config()),
    )
    .await
    .map_err(|_| "relay WebSocket upgrade failed")?;
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "relay TLS versions failed")?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let tcp = TcpStream::connect(address)
        .await
        .map_err(|_| "relay upstream TCP failed")?;
    tcp.set_nodelay(true)
        .map_err(|_| "relay upstream TCP setup failed")?;
    let tls = tokio_rustls::TlsConnector::from(Arc::new(tls))
        .connect("localhost".try_into().unwrap(), tcp)
        .await
        .map_err(|_| "relay upstream TLS failed")?;
    let mut request = format!("wss://{address}/mount-rs")
        .into_client_request()
        .map_err(|_| "relay upstream request failed")?;
    request
        .headers_mut()
        .insert("Sec-WebSocket-Protocol", SUBPROTOCOL.parse().unwrap());
    let (mut upstream, response) =
        tokio_tungstenite::client_async_with_config(request, tls, Some(socket_config()))
            .await
            .map_err(|_| "relay upstream WebSocket upgrade failed")?;
    ensure(
        response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok())
            == Some(SUBPROTOCOL),
        "relay upstream subprotocol mismatch",
    )?;

    let hello = envelope(&mut downstream).await?;
    ensure(
        matches!(control(&hello)?, Message::ClientHello { version: PROTOCOL_VERSION, ref partition_id, .. } if partition_id == "red"),
        "relay client hello mismatch",
    )?;
    forward(&mut upstream, &hello).await?;
    let hello = envelope(&mut upstream).await?;
    ensure(
        matches!(
            control(&hello)?,
            Message::ServerHello {
                version: PROTOCOL_VERSION,
                ..
            }
        ),
        "relay server hello mismatch",
    )?;
    forward(&mut downstream, &hello).await?;

    let mut opened_handle = None;
    let mut write_submissions = 0;
    for _ in 0..16 {
        let request_envelope = envelope(&mut downstream).await?;
        let mut cursor = request_envelope.body.as_slice();
        let incoming = binary::read_body(&mut cursor, request_envelope.header)
            .await
            .map_err(|_| "relay request decode failed")?;
        ensure(cursor.is_empty(), "relay trailing request bytes")?;
        let (request_id, opening) = match incoming {
            Incoming::Write {
                request_id,
                request,
                data,
            } => {
                write_submissions += 1;
                ensure(
                    opened_handle == Some(request.handle)
                        && request.drive_id.as_ref() == "data"
                        && request.position == Some(0)
                        && data == *expected,
                    "relay target write differs from exact submission",
                )?;
                // Production dispatch awaits the actual durable driver write
                // before constructing this reply. The held fresh-store oracle
                // below must prove the commit before any service handle close.
                forward(&mut upstream, &request_envelope).await?;
                let reply = envelope(&mut upstream).await?;
                ensure(
                    reply.header.kind == Kind::WriteResult
                        && reply.header.request_id == request_id
                        && reply.header.count == expected.len()
                        && reply.body.is_empty(),
                    "relay did not receive a completed matching write reply",
                )?;
                // Keep both sockets and the service's write handle alive while
                // a fresh reader proves the commit independently. A handle
                // close also persists, so disconnecting before this observation
                // could conceal a missing persistence step in write().
                barrier
                    .held
                    .send(reply.header.count)
                    .map_err(|_| "held reply observer disappeared")?;
                hold_reply_until_release(&mut downstream, barrier.release).await?;
                // The complete production reply and terminator were received;
                // the observer has now explicitly released the fault. None of
                // these messages are forwarded to the original caller.
                drop((upstream, downstream));
                return Ok(RelayReceipt {
                    write_submissions,
                    committed_reply_bytes: binary::HEADER_BYTES + reply.body.len(),
                    committed_write_count: reply.header.count,
                    suppressed_messages: reply.messages.len(),
                });
            }
            Incoming::Control(Message::Request {
                request_id,
                operation,
                ..
            }) => {
                let opening = operation.name == OperationName::Open
                    && operation.body.get("path").and_then(|value| value.as_str()) == Some(FILE);
                (request_id, opening)
            }
            _ => return Err("relay unexpected request before the target write".into()),
        };
        forward(&mut upstream, &request_envelope).await?;
        let response = envelope(&mut upstream).await?;
        match control(&response)? {
            Message::Response {
                request_id: response_id,
                result,
            } if response_id == request_id => {
                if opening {
                    opened_handle = Some(
                        result
                            .map_err(|_| "relay target open rejected")?
                            .as_u64()
                            .ok_or("relay target open returned invalid handle")?,
                    );
                }
            }
            _ => return Err("relay response request ID mismatch".into()),
        }
        forward(&mut downstream, &response).await?;
    }
    Err("relay target write absent within request bound".into())
}

async fn join_owned<T>(slot: &mut Option<JoinHandle<T>>) -> TestResult<T> {
    let task = slot.as_mut().ok_or("owned task is missing")?;
    let result = tokio::time::timeout(STEP_TIMEOUT, task)
        .await
        .map_err(|_| "owned task deadline exceeded")?;
    let _ = slot.take();
    result.map_err(|_| "owned task failed".to_string())
}

async fn cancel_owned<T>(slot: &mut Option<JoinHandle<T>>) -> TestResult<()> {
    if let Some(mut task) = slot.take() {
        task.abort();
        let _ = tokio::time::timeout(STEP_TIMEOUT, &mut task)
            .await
            .map_err(|_| "owned task cancellation deadline exceeded")?;
    }
    Ok(())
}

async fn call(
    connection: &RemoteConnection,
    drive: &str,
    name: OperationName,
    body: serde_json::Value,
) -> Result<serde_json::Value, ClientError> {
    connection.request(drive, Operation { name, body }).await
}

async fn preclose_read_only_oracle(
    storage: Storage,
    database: &Path,
    before: &Stats,
    payload: &[u8],
) -> TestResult<PrecloseReceipt> {
    let fresh = storage.open(database).await?;
    let driver = fresh.driver();
    let handle = driver
        .open(FILE, "r", 0)
        .await
        .map_err(|_| "preclose oracle open failed")?;
    let observation = async {
        let stats = driver
            .stat(FILE)
            .await
            .map_err(|_| "preclose oracle stat failed")?;
        ensure(
            stats.is_file()
                && stats.mode == before.mode
                && stats.mode & 0o7777 == MODE
                && stats.ino == before.ino
                && stats.uid == before.uid
                && stats.gid == before.gid
                && stats.nlink == before.nlink
                && stats.size == payload.len() as u64,
            "preclose oracle metadata mismatch",
        )?;
        let compact_checkpoint = storage.compact_checkpoint(database, &stats).await?;
        let mut offset = 0;
        let mut buffer = [0; 4096];
        while offset < payload.len() {
            let wanted = buffer.len().min(payload.len() - offset);
            let count = handle
                .read(&mut buffer[..wanted], Some(offset as u64))
                .await
                .map_err(|_| "preclose oracle read failed")?;
            ensure(
                count != 0 && count <= wanted && buffer[..count] == payload[offset..offset + count],
                "preclose oracle full byte comparison failed",
            )?;
            offset += count;
        }
        let mut eof = [0xa5];
        let eof_count = handle
            .read(&mut eof, Some(payload.len() as u64))
            .await
            .map_err(|_| "preclose oracle EOF read failed")?;
        ensure(eof_count == 0, "preclose oracle has bytes after exact EOF")?;
        Ok::<_, String>(PrecloseReceipt {
            verified_bytes: offset,
            eof_count,
            stats,
            compact_checkpoint,
        })
    }
    .await;
    // The snapshot reader does not save; established shared compact reads
    // verify authority and read guards/blocks without recording atime. Neither
    // reader's Drop publishes. Do not close, sync or shutdown this observer
    // while the original writer and reply are held.
    drop((handle, driver, fresh));
    observation
}

fn drain_initial_datagrams(socket: &UdpSocket) -> TestResult<u64> {
    let mut packet = [0; 65_536];
    let mut count = 0;
    loop {
        match socket.try_recv_from(&mut packet) {
            Ok((bytes, source)) => {
                ensure(
                    bytes != 0 && source.ip().is_loopback(),
                    "invalid initial QUIC probe",
                )?;
                count += 1;
                ensure(count <= 32, "initial QUIC probes exceed fixture bound")?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(count),
            Err(_) => return Err("initial QUIC probe observation failed".into()),
        }
    }
}

async fn settle_initial_datagrams(socket: &UdpSocket) -> TestResult<u64> {
    tokio::time::timeout(STEP_TIMEOUT, async {
        let mut count = drain_initial_datagrams(socket)?;
        let mut packet = [0; 65_536];
        loop {
            match tokio::time::timeout(QUIET_WINDOW, socket.recv_from(&mut packet)).await {
                Err(_) => return Ok(count),
                Ok(Ok((bytes, source))) => {
                    ensure(
                        bytes != 0 && source.ip().is_loopback(),
                        "invalid retiring QUIC probe",
                    )?;
                    count += 1;
                    ensure(count <= 32, "retiring QUIC probes exceed fixture bound")?;
                }
                Ok(Err(_)) => return Err("initial QUIC settlement observation failed".into()),
            }
        }
    })
    .await
    .map_err(|_| "initial QUIC settlement deadline exceeded")?
}

async fn observe_without_quic_contact<T>(
    socket: &UdpSocket,
    observation: impl std::future::Future<Output = TestResult<T>>,
) -> TestResult<T> {
    let mut packet = [0; 2048];
    let observed = tokio::select! {
        biased;
        _ = socket.recv_from(&mut packet) => Err("client contacted QUIC after initial fallback settlement".into()),
        result = observation => result,
    }?;
    // Contact that becomes readable while the observation finishes is still a
    // failure. Never consume and classify a late packet as initial traffic.
    match socket.try_recv_from(&mut packet) {
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(observed),
        _ => Err("client contacted QUIC at the end of the observation window".into()),
    }
}

#[tokio::test]
async fn ready_quic_contact_cannot_be_hidden_by_completed_observation() {
    tokio::time::timeout(STEP_TIMEOUT, async {
        let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender
            .send_to(b"contact", socket.local_addr().unwrap())
            .await
            .unwrap();
        socket.readable().await.unwrap();
        assert!(
            observe_without_quic_contact(&socket, std::future::ready(Ok(())))
                .await
                .is_err()
        );
        assert!(
            observe_without_quic_contact(&socket, std::future::ready(Ok(())))
                .await
                .is_ok()
        );
    })
    .await
    .expect("bounded real ready-contact control");
}

async fn held_monitor_socket_pair()
-> TestResult<(WebSocketStream<TcpStream>, WebSocketStream<TcpStream>)> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "held monitor listener bind failed")?;
    let address = listener
        .local_addr()
        .map_err(|_| "held monitor address failed")?;
    let caller = async {
        let tcp = TcpStream::connect(address)
            .await
            .map_err(|_| "held monitor caller connect failed")?;
        let mut request = format!("ws://{address}/mount-rs")
            .into_client_request()
            .map_err(|_| "held monitor caller request failed")?;
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", SUBPROTOCOL.parse().unwrap());
        let (socket, response) =
            tokio_tungstenite::client_async_with_config(request, tcp, Some(socket_config()))
                .await
                .map_err(|_| "held monitor caller upgrade failed")?;
        ensure(
            response
                .headers()
                .get("Sec-WebSocket-Protocol")
                .and_then(|value| value.to_str().ok())
                == Some(SUBPROTOCOL),
            "held monitor caller subprotocol mismatch",
        )?;
        Ok::<_, String>(socket)
    };
    let downstream = async {
        let (tcp, _) = listener
            .accept()
            .await
            .map_err(|_| "held monitor accept failed")?;
        tokio_tungstenite::accept_hdr_async_with_config(tcp, upgrade, Some(socket_config()))
            .await
            .map_err(|_| "held monitor downstream upgrade failed".into())
    };
    let (caller, downstream) = tokio::try_join!(caller, downstream)?;
    Ok((downstream, caller))
}

async fn second_write_envelope() -> TestResult<Envelope> {
    let mut encoded = Vec::new();
    binary::write_request(
        &mut encoded,
        2,
        &binary::IoRequest {
            drive_id: "data",
            handle: 7,
            position: Some(0),
        },
        0,
        Some(&[0x51; 17]),
    )
    .await
    .map_err(|_| "second write encoding failed")?;
    let header = Header::decode(encoded[..binary::HEADER_BYTES].try_into().unwrap())
        .map_err(|_| "second write header invalid")?;
    ensure(
        header.kind == Kind::Write,
        "negative monitor control did not encode a write",
    )?;
    Ok(Envelope {
        header,
        body: encoded[binary::HEADER_BYTES..].to_vec(),
        messages: vec![
            encoded[..binary::HEADER_BYTES].to_vec().into(),
            encoded[binary::HEADER_BYTES..].to_vec().into(),
            Bytes::new(),
        ],
    })
}

#[tokio::test]
async fn held_reply_monitor_rejects_second_write_even_when_release_is_ready() {
    tokio::time::timeout(STEP_TIMEOUT, async {
        for release_ready in [false, true] {
            let (mut downstream, mut caller) = held_monitor_socket_pair().await?;
            let write = second_write_envelope().await?;
            let (release, receiver) = oneshot::channel();
            let mut release = Some(release);
            let observed = if release_ready {
                forward(&mut caller, &write).await?;
                // The handshake has finished before sending this unfragmented
                // masked 32-byte header. Peek keeps its complete actual frame
                // queued, making both branches ready without consuming replay.
                let mut frame = [0; binary::HEADER_BYTES + 6];
                loop {
                    let available = downstream
                        .get_ref()
                        .peek(&mut frame)
                        .await
                        .map_err(|_| "buffered second write observation failed")?;
                    ensure(
                        available != 0,
                        "second write socket ended before the header",
                    )?;
                    if available == frame.len() {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                ensure(
                    frame[0] == 0x82 && frame[1] == 0xa0,
                    "negative monitor control did not queue its complete masked binary header",
                )?;
                release
                    .take()
                    .ok_or("negative monitor release missing")?
                    .send(())
                    .map_err(|_| "negative monitor release receiver disappeared")?;
                hold_reply_until_release(&mut downstream, receiver).await
            } else {
                // Poll the real monitor while the caller sends its second
                // complete write. Keep the release sender owned and pending.
                let (observed, sent) = tokio::join!(
                    hold_reply_until_release(&mut downstream, receiver),
                    forward(&mut caller, &write),
                );
                sent?;
                observed
            };
            ensure(
                matches!(observed, Err(ref error)
                if error == "application message arrived while the write reply was held"),
                "held monitor missed a second write or returned the wrong failure",
            )?;
            drop((release, caller, downstream));
        }
        Ok::<_, String>(())
    })
    .await
    .expect("bounded real second-write monitor control")
    .unwrap();
}

#[tokio::test]
async fn held_reply_monitor_flushes_ping_and_accepts_pong_before_release() {
    tokio::time::timeout(STEP_TIMEOUT, async {
        let (mut downstream, mut caller) = held_monitor_socket_pair().await?;
        let (release, receiver) = oneshot::channel();
        let control = async {
            caller
                .send(WsMessage::Pong(Bytes::from_static(b"prior")))
                .await
                .map_err(|_| "held monitor control pong failed")?;
            caller
                .send(WsMessage::Ping(Bytes::from_static(b"release")))
                .await
                .map_err(|_| "held monitor control ping failed")?;
            ensure(
                matches!(caller.next().await, Some(Ok(WsMessage::Pong(bytes)))
                if bytes.as_ref() == b"release"),
                "held monitor did not flush the actual protocol pong",
            )?;
            release
                .send(())
                .map_err(|_| "held monitor control release failed")?;
            Ok::<_, String>(())
        };
        let (observed, control) =
            tokio::join!(hold_reply_until_release(&mut downstream, receiver), control,);
        observed?;
        control?;
        drop((caller, downstream));
        Ok::<_, String>(())
    })
    .await
    .expect("bounded real ping/pong monitor control")
    .unwrap();
}

pub(super) async fn run(selection: Selection) -> TestResult<()> {
    run_with_storage(selection, Storage::Snapshot).await
}

#[cfg(any(target_os = "macos", all(target_os = "linux", target_env = "gnu")))]
pub(super) async fn run_compact_auto() -> TestResult<()> {
    run_with_storage(Selection::Auto, Storage::Compact).await
}

async fn run_with_storage(selection: Selection, storage: Storage) -> TestResult<()> {
    ensure(
        std::env::var("MOUNT_RS_REMOTE_SQLITE_REPLY_LOSS").as_deref() == Ok("1"),
        "durable reply loss control requires the explicit owned runtime gate",
    )?;
    let gate_directory = PathBuf::from(
        std::env::var_os("TMPDIR")
            .ok_or("durable reply loss control requires a private owned TMPDIR")?,
    );
    let gate_metadata =
        std::fs::metadata(&gate_directory).map_err(|_| "owned gate TMPDIR is unavailable")?;
    ensure(
        gate_metadata.is_dir(),
        "owned gate TMPDIR is not a directory",
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        ensure(
            gate_metadata.permissions().mode() & 0o777 == 0o700,
            "owned gate TMPDIR must have private permissions",
        )?;
    }
    let directory = tempfile::Builder::new()
        .prefix("mount-rs-sqlite-reply-loss-")
        .tempdir_in(&gate_directory)
        .map_err(|_| "fixture directory failed")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "fixture directory permissions failed")?;
    }
    // No automatic directory removal is permitted once asynchronous owners can
    // exist. Even a canceled close waiter can leave the actual service and its
    // PendingClose tasks alive. The root gate owns the process barrier and may
    // remove this retained directory only after reap, group absence and EOF.
    let directory = directory.keep();
    let mut owner_options = std::fs::OpenOptions::new();
    owner_options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        owner_options.mode(0o600);
    }
    let mut owner = owner_options
        .open(directory.join("owner.json"))
        .map_err(|_| "private fixture ownership record failed")?;
    std::io::Write::write_all(
        &mut owner,
        &serde_json::to_vec(&json!({
            "pid":std::process::id(),"directory":directory,
            "directory_retained":1,"directory_removed":0
        }))
        .map_err(|_| "private fixture ownership encoding failed")?,
    )
    .map_err(|_| "private fixture ownership write failed")?;
    drop(owner);
    let db = directory.join("drive.sqlite");
    let catalog = Arc::new(
        SqliteCatalog::open(directory.join("catalog.sqlite"))
            .await
            .map_err(|_| "fixture catalog open failed")?,
    );
    let mut snapshot = CatalogSnapshot::empty();
    for partition in ["red", "blue"] {
        snapshot.partitions.insert(
            partition.into(),
            PartitionDefinition {
                drives: BTreeMap::from([
                    (
                        "data".into(),
                        DriveDefinition {
                            driver: storage.definition(&db),
                        },
                    ),
                    (
                        "logs".into(),
                        DriveDefinition {
                            driver: json!({"kind":"memory"}),
                        },
                    ),
                ]),
            },
        );
    }
    snapshot.issuer_policies.insert(
        "oidc".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    snapshot.grants.insert(
        "workload".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            drives: BTreeMap::from([
                ("data".into(), Permission::Write),
                ("logs".into(), Permission::Read),
            ]),
            claim_conditions: BTreeMap::from([("/repository_id".into(), "repo-1".into())]),
        },
    );
    catalog
        .compare_and_swap(0, snapshot)
        .await
        .map_err(|_| "fixture catalog publication failed")?;
    let filesystem = storage.open(&db).await?;
    let opened_backing = filesystem.concurrent_backing_id();
    filesystem
        .driver()
        .chmod("/", 0o777)
        .await
        .map_err(|_| "fixture filesystem root permissions failed")?;
    let memory = mount_rs_sdk::Filesystem::memory(Default::default());
    let mut dispatcher = DriveDispatcher::new(catalog.clone());
    dispatcher
        .register_definition("red", "data", storage.definition(&db), filesystem.driver())
        .map_err(|_| "fixture data registration failed")?;
    dispatcher
        .register_definition("red", "logs", json!({"kind":"memory"}), memory.driver())
        .map_err(|_| "fixture logs registration failed")?;
    let (_, jwt, jwk) = token();
    let token_path = directory.join("token.jwt");
    std::fs::write(&token_path, &jwt).map_err(|_| "fixture token write failed")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&token_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| "fixture token permissions failed")?;
    }
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .map_err(|_| "fixture certificate failed")?;
    let cert = certificate.cert.der().clone();
    let key: rustls::pki_types::PrivateKeyDer<'static> =
        rustls::pki_types::PrivatePkcs8KeyDer::from(certificate.signing_key.serialize_der()).into();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "fixture TLS versions failed")?
    .with_no_client_auth()
    .with_single_cert(vec![cert.clone()], key.clone_key())
    .map_err(|_| "fixture relay TLS configuration failed")?;
    let mut roots = rustls::RootCertStore::empty();
    roots
        .add(cert.clone())
        .map_err(|_| "fixture roots failed")?;
    let listener = Arc::new(
        TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "relay bind failed")?,
    );
    let relay_address = listener.local_addr().map_err(|_| "relay address failed")?;
    let udp = UdpSocket::bind("127.0.0.1:0")
        .await
        .map_err(|_| "QUIC trap bind failed")?;
    let quic_address = udp.local_addr().map_err(|_| "QUIC trap address failed")?;
    let payload = Arc::new(
        (0..PAYLOAD_BYTES)
            .map(|index| ((index * 29 + index / 251) % 256) as u8)
            .collect::<Vec<_>>(),
    );
    let server = WebSocketServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        vec![cert],
        key,
        Arc::new(dispatcher),
        Arc::new(CatalogAuthenticator::with_key_source(
            catalog,
            Arc::new(Keys(jwk)),
        )),
    )
    .await
    .map_err(|_| "actual WebSocket server bind failed")?;
    let actual_address = server.local_addr();
    let credential_issues = Arc::new(AtomicU64::new(0));
    let deferred_token_path = directory.join("after-fallback.jwt");
    let credential = (selection == Selection::Auto).then(|| DeferredCredential {
        path: deferred_token_path.clone(),
        token: jwt,
        issues: credential_issues.clone(),
    });
    let (contact_sender, contact_receiver) = oneshot::channel();
    let (held_sender, held_receiver) = oneshot::channel();
    let (release_sender, release_receiver) = oneshot::channel();
    let mut release_reply = Some(release_sender);
    let mut relay_task = Some(tokio::spawn(relay(
        listener.clone(),
        tokio_rustls::TlsAcceptor::from(Arc::new(tls)),
        server.local_addr(),
        roots.clone(),
        payload.clone(),
        ReplyBarrier {
            contacted: contact_sender,
            held: held_sender,
            release: release_receiver,
        },
        credential,
    )));
    let mut write_task = None;
    let mut connection = None;
    let work = tokio::time::timeout(WORK_TIMEOUT, async {
        ensure(matches!(RemoteConnection::connect_with_transport(quic_address, "localhost", roots.clone(), "blue".into(),
            CredentialSource::File(token_path.clone()), ConnectionTransport::WebSocket(actual_address)).await,
            Err(ClientError::Authentication)), "cross partition hello was not denied")?;
        ensure(matches!(RemoteConnection::connect_with_transport(quic_address, "localhost", rustls::RootCertStore::empty(), "red".into(),
            CredentialSource::File(token_path.clone()), ConnectionTransport::WebSocket(actual_address)).await,
            Err(ClientError::Authentication)), "untrusted TLS root was not denied")?;
        let (transport, credentials) = match selection {
            Selection::WebSocket => (ConnectionTransport::WebSocket(relay_address), CredentialSource::File(token_path)),
            Selection::Auto => {
                ensure(!deferred_token_path.exists(), "Auto credential existed before selection")?;
                (ConnectionTransport::Auto { websocket: relay_address }, CredentialSource::File(deferred_token_path.clone()))
            }
        };
        let selection_started = Instant::now();
        let establishment = RemoteConnection::connect_with_transport(quic_address, "localhost", roots, "red".into(), credentials, transport);
        tokio::pin!(establishment);
        let mut initial_quic_datagrams = 0;
        if selection == Selection::Auto {
            let mut packet = [0; 65_536];
            tokio::select! {
                _ = &mut establishment => return Err("Auto establishment ended before an initial QUIC probe".into()),
                result = udp.recv_from(&mut packet) => {
                    let (bytes, source) = result.map_err(|_| "initial Auto QUIC probe unavailable")?;
                    ensure(bytes >= 1200 && source.ip().is_loopback(), "initial Auto QUIC probe invalid")?;
                    initial_quic_datagrams = 1;
                }
            }
            ensure(!deferred_token_path.exists() && credential_issues.load(Ordering::Relaxed) == 0,
                "credential issued before unavailable QUIC attempt")?;
        }
        let connected = establishment.await.map_err(|_| "relayed authenticated connection failed")?;
        connection = Some(connected.clone());
        let websocket_contact = contact_receiver.await.map_err(|_| "relay contact observation missing")?;
        let contact_elapsed = websocket_contact.checked_duration_since(selection_started)
            .ok_or("relay was contacted before selection started")?;
        ensure(selection != Selection::Auto || contact_elapsed >= Duration::from_secs(3),
            "Auto contacted WebSocket before its existing QUIC deadline")?;
        ensure(credential_issues.load(Ordering::Relaxed) == u64::from(selection == Selection::Auto),
            "deferred credential issue count differs from selected transport")?;
        initial_quic_datagrams += settle_initial_datagrams(&udp).await?;
        ensure(initial_quic_datagrams <= 32, "total initial QUIC probes exceed fixture bound")?;
        ensure((selection == Selection::Auto) == (initial_quic_datagrams > 0),
            "selected Auto control did not make a real initial QUIC attempt")?;
        ensure(connected.protocol_version() == PROTOCOL_VERSION, "client negotiated version mismatch")?;
        // Arm before any filesystem request. This receive remains polled
        // through the held reply, independent preclose oracle and loss window.
        let io_and_loss = async {
        let capabilities = call(&connected, "data", OperationName::Capabilities, json!({})).await
            .map_err(|_| "durable capabilities request failed")?;
        ensure(capabilities["durable_writes"] == true, "SQLite driver lacks durable writes")?;
        ensure(matches!(call(&connected, "blue/data", OperationName::Stat, json!({"path":"/"})).await,
            Err(ClientError::Remote(code)) if code == "EACCES"), "cross partition permission not enforced")?;
        ensure(matches!(call(&connected, "logs", OperationName::Write, json!({"path":"/denied","data":[1]})).await,
            Err(ClientError::Remote(code)) if code == "EACCES"), "read only drive permission not enforced")?;
        let handle = call(&connected, "data", OperationName::Open, json!({"path":FILE,"flags":"w+","mode":MODE})).await
            .map_err(|_| "target open failed")?.as_u64().ok_or("target handle invalid")?;
        let before: Stats = serde_json::from_value(call(&connected, "data", OperationName::HandleStat, json!({"handle":handle})).await
            .map_err(|_| "target metadata request failed")?).map_err(|_| "target metadata invalid")?;
        ensure(before.is_file() && before.mode & 0o7777 == MODE && before.size == 0,
            "target initial metadata mismatch")?;
        let compact_before = storage.compact_checkpoint(&db, &before).await?;
        ensure(compact_before.as_ref().map(|checkpoint| checkpoint.mode.backing) == opened_backing,
            "fresh compact authority differs from the actual writer")?;
        write_task = Some(tokio::spawn({
            let payload = payload.clone();
            let connected = connected.clone();
            async move { connected.write("data", handle, Some(0), &payload).await }
        }));
        let held_count = tokio::time::timeout(STEP_TIMEOUT, held_receiver).await
            .map_err(|_| "completed write reply notification deadline exceeded")?
            .map_err(|_| "completed write reply notification was canceled")?;
        ensure(held_count == payload.len(), "held write reply count mismatch")?;
        ensure(write_task.as_ref().is_some_and(|task| !task.is_finished())
            && relay_task.as_ref().is_some_and(|task| !task.is_finished()),
            "caller or relay completed before the preclose oracle")?;
        let preclose = tokio::time::timeout(STEP_TIMEOUT,
            preclose_read_only_oracle(storage, &db, &before, &payload)).await
            .map_err(|_| "preclose oracle deadline exceeded")??;
        ensure(preclose.compact_checkpoint.as_ref().map(|checkpoint| checkpoint.mode)
            == compact_before.as_ref().map(|checkpoint| checkpoint.mode),
            "durable write changed the compact backing or structural generation")?;
        ensure(write_task.as_ref().is_some_and(|task| !task.is_finished())
            && relay_task.as_ref().is_some_and(|task| !task.is_finished()),
            "caller or relay completed during the preclose oracle")?;
        release_reply.take().ok_or("held reply release is missing")?
            .send(()).map_err(|_| "held reply relay disappeared before release")?;
        ensure(matches!(join_owned(&mut write_task).await?, Err(ClientError::Transport | ClientError::Protocol)),
            "lost wire reply did not leave the caller uncertain")?;
        ensure(matches!(tokio::time::timeout(STEP_TIMEOUT,
            call(&connected, "data", OperationName::Stat, json!({"path":FILE}))).await
            .map_err(|_| "failed connection did not fail closed promptly")?, Err(ClientError::Transport)),
            "failed connection accepted a subsequent request")?;
        ensure(matches!(connected.write("data", handle, Some(0), &[1]).await, Err(ClientError::Transport)),
            "failed connection accepted a subsequent binary write")?;
        let mut unread = [0xa5; 17];
        ensure(matches!(connected.read("data", handle, Some(0), &mut unread).await, Err(ClientError::Transport))
            && unread == [0xa5; 17], "failed connection accepted or modified a subsequent binary read")?;
        ensure(credential_issues.load(Ordering::Relaxed) == u64::from(selection == Selection::Auto),
            "failed session reissued credentials")?;
        let relay_receipt = join_owned(&mut relay_task).await??;
        ensure(relay_receipt.write_submissions == 1, "target write was submitted more than once")?;
        tokio::select! {
            biased;
            _ = listener.accept() => return Err("client reconnected to the relay after reply loss".into()),
            _ = tokio::time::sleep(QUIET_WINDOW) => {},
        }
        Ok::<_, String>((before, relay_receipt, preclose, initial_quic_datagrams, contact_elapsed))
        };
        observe_without_quic_contact(&udp, io_and_loss).await
    }).await.map_err(|_| "durable reply loss work deadline exceeded".to_owned()).and_then(|value| value);
    // An error or deadline first drops any preclose observer, then releases or
    // cancels the held relay through the same owned cleanup path. It never
    // produces a successful durability receipt.
    if let Some(release) = release_reply.take() {
        let _ = release.send(());
    }
    if let Some(connection) = connection.take() {
        connection.close();
    }
    let write_cleanup = cancel_owned(&mut write_task).await;
    let relay_cleanup = cancel_owned(&mut relay_task).await;
    let mut server_close = Some(tokio::spawn(server.close()));
    let server_close_wait = join_owned(&mut server_close)
        .await
        .map_err(|_| "actual server close completion unobserved; fixture directory retained");
    // Aborting this wrapper only ends its waiter. It does not prove the actual
    // service or separately spawned handle closes released storage ownership.
    let server_waiter_cancel = cancel_owned(&mut server_close).await;
    let storage_cleanup = if storage == Storage::Snapshot {
        filesystem
            .shutdown()
            .await
            .map_err(|_| "original SQLite shutdown failed")
    } else if server_close_wait.is_ok() {
        // Compact shutdown takes lifecycle/gate ownership. Start it only after
        // actual service close is acknowledged and retain the existing bound.
        tokio::time::timeout(STEP_TIMEOUT, filesystem.shutdown())
            .await
            .map_err(|_| "original compact shutdown deadline exceeded")
            .and_then(|result| result.map_err(|_| "original compact shutdown failed"))
    } else {
        Err("actual service close unobserved; compact fixture retained")
    };
    // Drop the original SDK owner before the fresh reader persists on close.
    // Unknown service ownership on failure never triggers directory removal.
    drop(filesystem);
    write_cleanup?;
    relay_cleanup?;
    server_close_wait?;
    server_waiter_cancel?;
    storage_cleanup?;
    let (before, relay_receipt, preclose, initial_quic_datagrams, contact_elapsed) = work?;
    let reopened = storage.open(&db).await?;
    let driver = reopened.driver();
    let handle = driver
        .open(FILE, "r", 0)
        .await
        .map_err(|_| "fresh oracle open failed")?;
    let observation = tokio::time::timeout(STEP_TIMEOUT, async {
        let stats = driver
            .stat(FILE)
            .await
            .map_err(|_| "fresh oracle stat failed")?;
        ensure(
            stats.is_file()
                && stats.mode == before.mode
                && stats.mode & 0o7777 == MODE
                && stats.ino == before.ino
                && stats.uid == before.uid
                && stats.gid == before.gid
                && stats.nlink == before.nlink
                && stats.size == payload.len() as u64,
            "fresh oracle metadata mismatch",
        )?;
        if storage == Storage::Compact {
            ensure(
                stats == preclose.stats,
                "fresh compact full metadata differs from held commit",
            )?;
            ensure(
                storage.compact_checkpoint(&db, &stats).await? == preclose.compact_checkpoint,
                "fresh compact publication differs from the held durable commit",
            )?;
        }
        let mut offset = 0;
        let mut buffer = [0; 4096];
        while offset < payload.len() {
            let wanted = buffer.len().min(payload.len() - offset);
            let count = handle
                .read(&mut buffer[..wanted], Some(offset as u64))
                .await
                .map_err(|_| "fresh oracle read failed")?;
            ensure(
                count != 0 && count <= wanted && buffer[..count] == payload[offset..offset + count],
                "fresh oracle full byte comparison failed",
            )?;
            offset += count;
        }
        let mut eof = [0xa5];
        ensure(
            handle
                .read(&mut eof, Some(payload.len() as u64))
                .await
                .map_err(|_| "fresh oracle EOF read failed")?
                == 0,
            "fresh oracle has bytes after exact EOF",
        )?;
        Ok::<_, String>(offset)
    })
    .await
    .map_err(|_| "fresh oracle deadline exceeded".to_owned())
    .and_then(|value| value);
    let oracle_close = tokio::time::timeout(STEP_TIMEOUT, handle.close())
        .await
        .map_err(|_| "fresh oracle handle cleanup deadline exceeded".to_owned())
        .and_then(|result| result.map_err(|_| "fresh oracle handle cleanup failed".to_owned()));
    drop((handle, driver));
    let oracle_shutdown = if storage == Storage::Snapshot {
        reopened
            .shutdown()
            .await
            .map_err(|_| "fresh oracle shutdown failed")
    } else {
        tokio::time::timeout(STEP_TIMEOUT, reopened.shutdown())
            .await
            .map_err(|_| "fresh compact oracle shutdown deadline exceeded")
            .and_then(|result| result.map_err(|_| "fresh compact oracle shutdown failed"))
    };
    drop(reopened);
    oracle_close?;
    oracle_shutdown?;
    let verified_bytes = observation?;
    let mut receipt = json!({
        "schema_version":2,"connection_selection":selection.label(),
        "initial_quic_datagrams":initial_quic_datagrams,"initial_quic_responses":0,
        "websocket_contact_elapsed_us":contact_elapsed.as_micros(),
        "initial_quic_settlement_quiet_ms":QUIET_WINDOW.as_millis(),
        "deferred_credential_issues":credential_issues.load(Ordering::Relaxed),
        "protocol_version":PROTOCOL_VERSION,"client_hellos":1,
        "denied_partition_hellos":1,"denied_untrusted_tls":1,"drive_permission_denials":2,
        "write_submissions":relay_receipt.write_submissions,"completed_write_replies":1,
        "held_reply":1,"preclose_metadata_checks":1,
        "preclose_verified_bytes":preclose.verified_bytes,"preclose_eof":preclose.eof_count,
        "suppressed_response_envelopes":1,"suppressed_response_bytes":relay_receipt.committed_reply_bytes,
        "completed_write_count":relay_receipt.committed_write_count,
        "suppressed_response_messages":relay_receipt.suppressed_messages,
        "downstream_write_response_messages":0,"uncertain_results":1,"fail_closed_followups":3,
        "replayed_write_submissions":0,"reconnects":0,"quic_datagrams":0,
        "quic_observation_scope":"after_initial_settlement_through_post_loss_quiet",
        "no_contact_window_ms":QUIET_WINDOW.as_millis(),"oracle_metadata_checks":1,
        "oracle_verified_bytes":verified_bytes,"oracle_size_bytes":PAYLOAD_BYTES,"oracle_eof_count":0,
        "request_cleanup_completed":1,"relay_cleanup_completed":1,"oracle_cleanup_completed":1,
        "server_close_wait_completed":1,"process_cleanup_observed":0,
        "directory_retained":1,"directory_removed":0
    });
    if storage == Storage::Compact {
        receipt["schema_version"] = json!(3);
        receipt["storage_mode"] = json!("MRC5");
        receipt["metadata_provider"] = json!("sqlite");
        receipt["block_provider"] = json!("sqlite");
        receipt["configured_cli_child"] = json!(0);
        receipt["compact_metadata_checkpoints"] = json!(3);
        receipt["compact_backing_and_generation_preserved"] = json!(1);
        receipt["compact_publication_preserved_from_held_commit"] = json!(1);
        receipt["full_metadata_preserved_from_held_commit"] = json!(1);
        eprintln!("MOUNT_RS_COMPACT_SQLITE_REPLY_LOSS {receipt}");
    } else {
        eprintln!("MOUNT_RS_SQLITE_REPLY_LOSS {receipt}");
    }
    Ok(())
}
