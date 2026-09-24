//! TLS-authenticated QUIC sessions for one Partition per connection.

use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use mount_rs_remote_protocol::{Message, PROTOCOL_VERSION, WireError, read_frame, write_frame};
use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::sync::{Mutex, Semaphore};

use crate::dispatch::{DriveDispatcher, SessionIdentity};

const ALPN: &[u8] = b"mount-rs/1";
const MAX_STREAMS: usize = 256;

#[async_trait]
pub trait Authenticator: Send + Sync {
    async fn authenticate(&self, token: &str) -> Result<SessionIdentity, ()>;
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
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut tls = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let server_config =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(tls)?));
        let endpoint = quinn::Endpoint::server(server_config, address)?;
        let accept_endpoint = endpoint.clone();
        let task = tokio::spawn(async move {
            while let Some(incoming) = accept_endpoint.accept().await {
                let dispatcher = Arc::clone(&dispatcher);
                let authenticator = Arc::clone(&authenticator);
                tokio::spawn(async move {
                    if let Ok(connection) = incoming.await {
                        serve_connection(connection, dispatcher, authenticator).await;
                    }
                });
            }
        });
        Ok(Self { endpoint, task })
    }

    #[must_use]
    pub fn local_addr(&self) -> SocketAddr {
        self.endpoint.local_addr().expect("bound QUIC endpoint")
    }

    pub async fn close(self) {
        self.endpoint.close(0_u32.into(), b"server shutdown");
        self.task.abort();
        let _ = self.task.await;
        self.endpoint.wait_idle().await;
    }
}

async fn serve_connection(
    connection: quinn::Connection,
    dispatcher: Arc<DriveDispatcher>,
    authenticator: Arc<dyn Authenticator>,
) {
    let Ok((mut send, mut recv)) = connection.accept_bi().await else {
        return;
    };
    let Ok(Message::ClientHello {
        version,
        partition_id,
        bearer,
    }) = read_frame(&mut recv).await
    else {
        connection.close(1_u32.into(), b"invalid handshake");
        return;
    };
    if version != PROTOCOL_VERSION {
        connection.close(1_u32.into(), b"unsupported protocol");
        return;
    }
    let Ok(identity) = authenticator.authenticate(&bearer).await else {
        connection.close(1_u32.into(), b"authentication failed");
        return;
    };
    if identity.partition_id != partition_id {
        connection.close(1_u32.into(), b"authentication failed");
        return;
    }
    let response = Message::ServerHello {
        version: PROTOCOL_VERSION,
        session_id: format!("{}", connection.stable_id()),
    };
    if write_frame(&mut send, &response).await.is_err() {
        return;
    }
    let identity = Arc::new(Mutex::new(identity));
    let streams = Arc::new(Semaphore::new(MAX_STREAMS));
    while let Ok((send, recv)) = connection.accept_bi().await {
        let Ok(permit) = Arc::clone(&streams).acquire_owned().await else {
            break;
        };
        let identity = Arc::clone(&identity);
        let dispatcher = Arc::clone(&dispatcher);
        let authenticator = Arc::clone(&authenticator);
        tokio::spawn(async move {
            let _permit = permit;
            serve_stream(send, recv, identity, dispatcher, authenticator).await;
        });
    }
}

async fn serve_stream(
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
    identity: Arc<Mutex<SessionIdentity>>,
    dispatcher: Arc<DriveDispatcher>,
    authenticator: Arc<dyn Authenticator>,
) {
    let Ok(message) = read_frame(&mut recv).await else {
        return;
    };
    let response = match message {
        Message::Request {
            request_id,
            drive_id,
            operation,
        } => {
            let current = identity.lock().await.clone();
            let result = dispatcher.dispatch(&current, &drive_id, &operation).await;
            Message::Response { request_id, result }
        }
        Message::Renew { bearer } => {
            let Ok(next) = authenticator.authenticate(&bearer).await else {
                let _ = write_frame(&mut send, &denied()).await;
                return;
            };
            let mut current = identity.lock().await;
            if next.partition_id != current.partition_id
                || next.policy_id != current.policy_id
                || next.issuer != current.issuer
                || next.subject != current.subject
            {
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
    let _ = write_frame(&mut send, &response).await;
}

fn denied() -> Message {
    Message::Response {
        request_id: 0,
        result: Err(WireError {
            code: "EACCES".into(),
        }),
    }
}
