//! TLS-authenticated QUIC sessions for one Partition per connection.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use mount_rs_remote_protocol::{Message, PROTOCOL_VERSION, WireError, read_frame, write_frame};
use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::{Mutex, Semaphore};

use crate::dispatch::{DriveDispatcher, SessionHandles, SessionIdentity};

const ALPN: &[u8] = b"mount-rs/1";
const MAX_STREAMS: usize = 32;

#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(&self, token: &str, partition_id: &str) -> Result<SessionIdentity, ()>;
}

pub struct RemoteServer {
    endpoint: quinn::Endpoint,
    task: tokio::task::JoinHandle<()>,
}

impl RemoteServer {
    pub async fn bind(
        address: SocketAddr,
        certs: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        dispatcher: Arc<DriveDispatcher>,
        authenticator: Arc<dyn Authenticator>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certs, key)?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut server_config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
        let transport = Arc::get_mut(&mut server_config.transport).expect("new transport config");
        transport.max_concurrent_bidi_streams((MAX_STREAMS as u32).into());
        transport.max_concurrent_uni_streams(0_u32.into());
        transport.max_idle_timeout(Some(std::time::Duration::from_secs(60).try_into()?));
        transport.keep_alive_interval(Some(std::time::Duration::from_secs(15)));
        let endpoint = quinn::Endpoint::server(server_config, address)?;
        let accept_endpoint = endpoint.clone();
        let connections = Arc::new(Semaphore::new(128));
        let task = tokio::spawn(async move {
            let mut sessions = tokio::task::JoinSet::new();
            while let Some(incoming) = accept_endpoint.accept().await {
                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                    incoming.refuse();
                    continue;
                };
                let dispatcher = Arc::clone(&dispatcher);
                let authenticator = Arc::clone(&authenticator);
                while sessions.try_join_next().is_some() {}
                sessions.spawn(async move {
                    let _permit = permit;
                    if let Ok(Ok(connection)) =
                        tokio::time::timeout(std::time::Duration::from_secs(10), incoming).await
                    {
                        serve_connection(connection, dispatcher, authenticator).await;
                    }
                });
            }
            while sessions.join_next().await.is_some() {}
        });
        Ok(Self { endpoint, task })
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.endpoint.local_addr().expect("bound QUIC endpoint")
    }

    pub async fn close(self) {
        self.endpoint.close(0_u32.into(), b"server shutdown");
        let _ = self.task.await;
        self.endpoint.wait_idle().await;
    }
}

async fn serve_connection(
    connection: quinn::Connection,
    dispatcher: Arc<DriveDispatcher>,
    authenticator: Arc<dyn Authenticator>,
) {
    let Ok(Ok((mut send, mut recv))) =
        tokio::time::timeout(std::time::Duration::from_secs(10), connection.accept_bi()).await
    else {
        return;
    };
    let Ok(Ok(Message::ClientHello {
        version,
        partition_id,
        bearer,
    })) = tokio::time::timeout(std::time::Duration::from_secs(10), read_frame(&mut recv)).await
    else {
        connection.close(1_u32.into(), b"invalid handshake");
        return;
    };
    if version != PROTOCOL_VERSION {
        connection.close(1_u32.into(), b"unsupported protocol");
        return;
    }
    let Ok(Ok(identity)) = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        authenticator.authenticate(&bearer, &partition_id),
    )
    .await
    else {
        eprintln!("remote authentication denied");
        connection.close(1_u32.into(), b"authentication failed");
        return;
    };
    if identity.partition_id != partition_id {
        connection.close(1_u32.into(), b"authentication failed");
        return;
    }
    let response = Message::ServerHello {
        version: PROTOCOL_VERSION,
        session_id: session_id(),
    };
    if write_frame(&mut send, &response).await.is_err() {
        return;
    }
    let identity = Arc::new(Mutex::new(identity));
    let handles = Arc::new(SessionHandles::default());
    let mut tasks = tokio::task::JoinSet::new();
    let streams = Arc::new(Semaphore::new(MAX_STREAMS));
    while let Ok((send, recv)) = connection.accept_bi().await {
        let Ok(permit) = Arc::clone(&streams).acquire_owned().await else {
            break;
        };
        let identity = Arc::clone(&identity);
        let dispatcher = Arc::clone(&dispatcher);
        let authenticator = Arc::clone(&authenticator);
        let handles = Arc::clone(&handles);
        let stream_connection = connection.clone();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            let _permit = permit;
            serve_stream(
                send,
                recv,
                identity,
                dispatcher,
                authenticator,
                handles,
                stream_connection,
            )
            .await;
        });
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    handles.close_all().await;
}

async fn serve_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    identity: Arc<Mutex<SessionIdentity>>,
    dispatcher: Arc<DriveDispatcher>,
    authenticator: Arc<dyn Authenticator>,
    handles: Arc<SessionHandles>,
    connection: quinn::Connection,
) {
    let Ok(Ok(message)) =
        tokio::time::timeout(std::time::Duration::from_secs(10), read_frame(&mut recv)).await
    else {
        return;
    };
    let response = match message {
        Message::Request {
            request_id,
            drive_id,
            operation,
        } => {
            if request_id == 0 {
                connection.close(1_u32.into(), b"invalid request id");
                return;
            }
            let current = identity.lock().await.clone();
            let result = match tokio::time::timeout(
                std::time::Duration::from_secs(30),
                dispatcher.dispatch_request(&current, &drive_id, &operation, &handles, request_id),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => {
                    eprintln!(
                        "{}",
                        serde_json::json!({"event":"remote_denial","request_id":request_id,"outcome":"timeout"})
                    );
                    connection.close(1_u32.into(), b"operation timed out");
                    return;
                }
            };
            if let Err(error) = &result {
                eprintln!(
                    "{}",
                    serde_json::json!({"event":"remote_denial","partition_id":current.partition_id,"drive_id":drive_id,"policy_id":current.policy_id,"operation":operation.name,"request_id":request_id,"outcome":error.code})
                );
            }
            Message::Response { request_id, result }
        }
        Message::Renew { bearer } => {
            let partition = identity.lock().await.partition_id.clone();
            let Ok(Ok(next)) = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                authenticator.authenticate(&bearer, &partition),
            )
            .await
            else {
                identity.lock().await.expires_at = 0;
                connection.close(1_u32.into(), b"authentication failed");
                return;
            };
            let mut current = identity.lock().await;
            if next.partition_id != current.partition_id
                || next.policy_id != current.policy_id
                || next.issuer != current.issuer
                || next.subject != current.subject
                || !dispatcher.renewal_matches(&current, &next).await
            {
                current.expires_at = 0;
                connection.close(1_u32.into(), b"identity changed");
                denied()
            } else {
                *current = next;
                Message::ServerHello {
                    version: PROTOCOL_VERSION,
                    session_id: "renewed".into(),
                }
            }
        }
        _ => denied(),
    };
    let _ = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        write_frame(&mut send, &response),
    )
    .await;
    let _ = send.finish();
}

fn denied() -> Message {
    Message::Response {
        request_id: 0,
        result: Err(WireError {
            code: "EACCES".into(),
        }),
    }
}

fn session_id() -> String {
    use ring::rand::SecureRandom;
    let mut bytes = [0_u8; 16];
    if ring::rand::SystemRandom::new().fill(&mut bytes).is_err() {
        return String::new();
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
