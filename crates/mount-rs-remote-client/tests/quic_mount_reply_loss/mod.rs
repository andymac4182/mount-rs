//! A completed durable write whose actual TLS WebSocket reply is lost in transit.
//! The relay never replaces the production authenticator, dispatcher or driver.

use super::{Keys, token};
use futures_util::{SinkExt, StreamExt};
use mount_rs_core::Stats;
use mount_rs_remote_client::{
    connection::{ClientError, ConnectionTransport, RemoteConnection},
    credentials::CredentialSource,
    websocket::{CHUNK_BYTES, SUBPROTOCOL},
};
use mount_rs_remote_protocol::{
    Message, Operation, OperationName, PROTOCOL_VERSION,
    binary::{self, Header, Incoming, Kind},
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
use serde_json::json;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
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
        handshake::server::{ErrorResponse, Request, Response},
        protocol::WebSocketConfig,
    },
};

type TestResult<T> = Result<T, String>;
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
    held: oneshot::Sender<usize>,
    release: oneshot::Receiver<()>,
}

struct PrecloseReceipt {
    verified_bytes: usize,
    eof_count: usize,
}

async fn relay(
    listener: Arc<TcpListener>,
    acceptor: tokio_rustls::TlsAcceptor,
    address: SocketAddr,
    roots: rustls::RootCertStore,
    expected: Arc<Vec<u8>>,
    barrier: ReplyBarrier,
) -> TestResult<RelayReceipt> {
    let (tcp, _) = listener.accept().await.map_err(|_| "relay accept failed")?;
    tcp.set_nodelay(true)
        .map_err(|_| "relay TCP setup failed")?;
    let tls = acceptor
        .accept(tcp)
        .await
        .map_err(|_| "relay TLS accept failed")?;
    let mut downstream =
        tokio_tungstenite::accept_hdr_async_with_config(tls, upgrade, Some(socket_config()))
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
                // Production dispatch awaits PersistedHandle::write and the FULL
                // SQLite autocommit save before constructing this actual reply.
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
                tokio::time::timeout(REPLY_HOLD_TIMEOUT, barrier.release)
                    .await
                    .map_err(|_| "held reply release deadline exceeded")?
                    .map_err(|_| "held reply release was canceled")?;
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
    database: &Path,
    before: &Stats,
    payload: &[u8],
) -> TestResult<PrecloseReceipt> {
    let fresh = mount_rs_sdk::Filesystem::sqlite(database)
        .await
        .map_err(|_| "preclose fresh SQLite open failed")?;
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
        })
    }
    .await;
    // Existing snapshot + flags="r", stat and read never save. Neither handle
    // nor filesystem Drop persists. Do not call close/sync/shutdown on this
    // observer: close would write a snapshot while the original owner is live.
    drop((handle, driver, fresh));
    observation
}

pub(super) async fn run() -> TestResult<()> {
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
                            driver: json!({"kind":"sqlite","database":db}),
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
    let filesystem = mount_rs_sdk::Filesystem::sqlite(&db)
        .await
        .map_err(|_| "fixture SQLite open failed")?;
    filesystem
        .driver()
        .chmod("/", 0o777)
        .await
        .map_err(|_| "fixture filesystem root permissions failed")?;
    let memory = mount_rs_sdk::Filesystem::memory(Default::default());
    let mut dispatcher = DriveDispatcher::new(catalog.clone());
    dispatcher
        .register_definition(
            "red",
            "data",
            json!({"kind":"sqlite","database":db}),
            filesystem.driver(),
        )
        .map_err(|_| "fixture data registration failed")?;
    dispatcher
        .register_definition("red", "logs", json!({"kind":"memory"}), memory.driver())
        .map_err(|_| "fixture logs registration failed")?;
    let (_, jwt, jwk) = token();
    let token_path = directory.join("token.jwt");
    std::fs::write(&token_path, jwt).map_err(|_| "fixture token write failed")?;
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
            held: held_sender,
            release: release_receiver,
        },
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
        let connected = RemoteConnection::connect_with_transport(quic_address, "localhost", roots, "red".into(),
            CredentialSource::File(token_path), ConnectionTransport::WebSocket(relay_address))
            .await.map_err(|_| "relayed authenticated connection failed")?;
        connection = Some(connected.clone());
        ensure(connected.protocol_version() == PROTOCOL_VERSION, "client negotiated version mismatch")?;
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
            preclose_read_only_oracle(&db, &before, &payload)).await
            .map_err(|_| "preclose oracle deadline exceeded")??;
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
        let relay_receipt = join_owned(&mut relay_task).await??;
        ensure(relay_receipt.write_submissions == 1, "target write was submitted more than once")?;
        let mut packet = [0; 2048];
        tokio::select! {
            _ = listener.accept() => return Err("client reconnected to the relay after reply loss".into()),
            _ = udp.recv_from(&mut packet) => return Err("client switched to QUIC after reply loss".into()),
            _ = tokio::time::sleep(QUIET_WINDOW) => {},
        }
        Ok::<_, String>((before, relay_receipt, preclose))
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
    let storage_cleanup = filesystem
        .shutdown()
        .await
        .map_err(|_| "original SQLite shutdown failed");
    // SQLite shutdown is currently a no-op. After a successful close wait,
    // drop the original SDK owner before the fresh reader persists on close.
    // Unknown service ownership on failure never triggers directory removal.
    drop(filesystem);
    write_cleanup?;
    relay_cleanup?;
    server_close_wait?;
    server_waiter_cancel?;
    storage_cleanup?;
    let (before, relay_receipt, preclose) = work?;
    let reopened = mount_rs_sdk::Filesystem::sqlite(&db)
        .await
        .map_err(|_| "fresh SQLite reopen failed")?;
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
    let oracle_shutdown = reopened
        .shutdown()
        .await
        .map_err(|_| "fresh oracle shutdown failed");
    drop(reopened);
    oracle_close?;
    oracle_shutdown?;
    let verified_bytes = observation?;
    eprintln!(
        "MOUNT_RS_SQLITE_REPLY_LOSS {}",
        json!({
            "schema_version":1,"protocol_version":PROTOCOL_VERSION,"client_hellos":1,
            "denied_partition_hellos":1,"denied_untrusted_tls":1,"drive_permission_denials":2,
            "write_submissions":relay_receipt.write_submissions,"completed_write_replies":1,
            "held_reply":1,"preclose_metadata_checks":1,
            "preclose_verified_bytes":preclose.verified_bytes,"preclose_eof":preclose.eof_count,
            "suppressed_response_envelopes":1,"suppressed_response_bytes":relay_receipt.committed_reply_bytes,
            "completed_write_count":relay_receipt.committed_write_count,
            "suppressed_response_messages":relay_receipt.suppressed_messages,
            "downstream_write_response_messages":0,"uncertain_results":1,"fail_closed_followups":1,
            "replayed_write_submissions":0,"reconnects":0,"quic_datagrams":0,
            "no_contact_window_ms":QUIET_WINDOW.as_millis(),"oracle_metadata_checks":1,
            "oracle_verified_bytes":verified_bytes,"oracle_size_bytes":PAYLOAD_BYTES,"oracle_eof_count":0,
            "request_cleanup_completed":1,"relay_cleanup_completed":1,"oracle_cleanup_completed":1,
            "server_close_wait_completed":1,"process_cleanup_observed":0,
            "directory_retained":1,"directory_removed":0
        })
    );
    Ok(())
}
