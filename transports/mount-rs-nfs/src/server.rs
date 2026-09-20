//! TCP transport for [`crate::session::Nfs3Session`].
//!
//! RPC record marking is handled here; all protocol and filesystem behavior
//! remains in the byte-oriented session. The default bind address is loopback
//! with an ephemeral port, which keeps wire integration tests rootless on both
//! macOS and Linux. Kernel `mount(8)` use needs a caller-selected reachable
//! address and the host's own NFS client privileges; this server does not claim
//! to perform that native mount step.

use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};

use mount_rs_core::{FsDriver, Loopback};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, Semaphore, oneshot};
use tokio::task::{JoinHandle, JoinSet};

use crate::rpc::{DEFAULT_RECORD_LIMIT, RecordAssembler, frame_record};
use crate::session::{Nfs3Session, NfsRequestContext, NfsSessionOptions};

pub const DEFAULT_NFS_PORT: u16 = 2049;

#[derive(Debug, Clone)]
pub struct NfsServerOptions {
    pub bind: SocketAddr,
    pub session: NfsSessionOptions,
    pub record_limit: usize,
    /// Maximum number of RPC calls dispatched concurrently per connection.
    pub max_in_flight: usize,
    /// Accept peers other than loopback. AUTH_SYS is not a security boundary,
    /// so exporting a non-loopback bind is opt-in.
    pub allow_remote: bool,
}

impl Default for NfsServerOptions {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 0)),
            session: NfsSessionOptions::default(),
            record_limit: DEFAULT_RECORD_LIMIT,
            max_in_flight: 64,
            allow_remote: false,
        }
    }
}

pub struct NfsServer {
    session: Nfs3Session,
    options: NfsServerOptions,
    address: Arc<Mutex<Option<SocketAddr>>>,
    shutdown: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    accept_task: Arc<Mutex<Option<JoinHandle<()>>>>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

/// Construct an NFSv3/MOUNTv3 TCP server backed by an [`FsDriver`].
pub fn create_nfs_server<D>(driver: D, options: NfsServerOptions) -> NfsServer
where
    D: FsDriver + 'static,
{
    NfsServer::new(driver, options)
}

impl NfsServer {
    pub fn new<D>(driver: D, options: NfsServerOptions) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::from_loopback(Loopback::new(driver), options)
    }

    pub fn from_session(session: Nfs3Session, options: NfsServerOptions) -> Self {
        Self {
            session,
            options,
            address: Arc::new(Mutex::new(None)),
            shutdown: Arc::new(Mutex::new(None)),
            accept_task: Arc::new(Mutex::new(None)),
            connections: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn from_loopback(driver: Loopback, options: NfsServerOptions) -> Self {
        Self::from_session(
            Nfs3Session::from_loopback(driver, options.session.clone()),
            options,
        )
    }

    pub fn session(&self) -> &Nfs3Session {
        &self.session
    }

    pub fn local_addr(&self) -> Option<SocketAddr> {
        *self.address.lock().expect("NFS server address lock")
    }

    pub fn port(&self) -> Option<u16> {
        self.local_addr().map(|address| address.port())
    }

    pub async fn listen(&self) -> io::Result<SocketAddr> {
        if let Some(address) = self.local_addr() {
            return Ok(address);
        }
        let listener = TcpListener::bind(self.options.bind).await?;
        let address = listener.local_addr()?;
        *self.address.lock().expect("NFS server address lock") = Some(address);
        let (shutdown_sender, mut shutdown_receiver) = oneshot::channel();
        *self.shutdown.lock().expect("NFS shutdown lock") = Some(shutdown_sender);
        let session = self.session.clone();
        let record_limit = self.options.record_limit;
        let max_in_flight = self.options.max_in_flight.max(1);
        let allow_remote = self.options.allow_remote;
        let connections = self.connections.clone();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_receiver => break,
                    result = listener.accept() => {
                        let Ok((stream, peer)) = result else { break };
                        let session = session.clone();
                        let task = tokio::spawn(async move {
                            let _ = serve_connection(
                                stream,
                                peer,
                                session,
                                record_limit,
                                max_in_flight,
                                allow_remote,
                            )
                            .await;
                        });
                        connections.lock().expect("NFS connection lock").push(task);
                    }
                }
            }
        });
        *self.accept_task.lock().expect("NFS accept lock") = Some(task);
        Ok(address)
    }

    pub async fn close(&self) -> io::Result<()> {
        if let Some(sender) = self.shutdown.lock().expect("NFS shutdown lock").take() {
            let _ = sender.send(());
        }
        let accept_task = self.accept_task.lock().expect("NFS accept lock").take();
        if let Some(task) = accept_task {
            let _ = task.await;
        }
        for task in self
            .connections
            .lock()
            .expect("NFS connection lock")
            .drain(..)
        {
            task.abort();
        }
        self.session.destroy().await;
        Ok(())
    }
}

impl Drop for NfsServer {
    fn drop(&mut self) {
        if let Some(sender) = self.shutdown.lock().expect("NFS shutdown lock").take() {
            let _ = sender.send(());
        }
        if let Some(task) = self.accept_task.lock().expect("NFS accept lock").take() {
            task.abort();
        }
        for task in self
            .connections
            .lock()
            .expect("NFS connection lock")
            .drain(..)
        {
            task.abort();
        }
    }
}

async fn serve_connection(
    mut stream: TcpStream,
    peer: SocketAddr,
    session: Nfs3Session,
    record_limit: usize,
    max_in_flight: usize,
    allow_remote: bool,
) -> io::Result<()> {
    if !allow_remote && !is_loopback(peer.ip()) {
        stream.shutdown().await?;
        return Ok(());
    }
    stream.set_nodelay(true)?;
    let (mut reader, writer) = stream.into_split();
    let writer = Arc::new(AsyncMutex::new(writer));
    let permits = Arc::new(Semaphore::new(max_in_flight.max(1)));
    let mut workers = JoinSet::new();
    let mut assembler = RecordAssembler::new(record_limit);
    let mut buffer = vec![0_u8; 64 * 1024];
    let peer = peer.ip().to_string();
    loop {
        while workers.try_join_next().is_some() {}
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            workers.shutdown().await;
            return Ok(());
        }
        let records = match assembler.push(&buffer[..count]) {
            Ok(records) => records,
            Err(_) => {
                workers.shutdown().await;
                return Ok(());
            }
        };
        for record in records {
            let permit = permits
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| io::Error::other("NFS connection semaphore closed"))?;
            let session = session.clone();
            let writer = writer.clone();
            let peer = peer.clone();
            workers.spawn(async move {
                let _permit = permit;
                let Some(reply) = session
                    .handle_call(&record, NfsRequestContext { peer: Some(peer) })
                    .await
                else {
                    return;
                };
                let Ok(framed) = frame_record(&reply) else {
                    return;
                };
                let mut writer = writer.lock().await;
                let _ = writer.write_all(&framed).await;
            });
        }
    }
}

fn is_loopback(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => address.is_loopback(),
        IpAddr::V6(address) => {
            address.is_loopback() || address.to_ipv4().is_some_and(|mapped| mapped.is_loopback())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::constants::{MOUNT_PROGRAM, MOUNT_V3, MOUNTPROC3_NULL};
    use crate::rpc::{RecordAssembler, decode_reply, encode_call};
    use mount_rs_core::MemoryFs;

    #[tokio::test]
    async fn loopback_tcp_server_handles_rpc_without_native_mount() {
        let server = NfsServer::new(MemoryFs::empty(), NfsServerOptions::default());
        let address = server.listen().await.unwrap();
        let mut stream = TcpStream::connect(address).await.unwrap();
        let call = encode_call(
            42,
            MOUNT_PROGRAM,
            MOUNT_V3,
            MOUNTPROC3_NULL,
            None,
            None,
            &[],
        );
        stream
            .write_all(&frame_record(&call).unwrap())
            .await
            .unwrap();
        let mut assembler = RecordAssembler::default();
        let mut buffer = [0_u8; 256];
        let count = stream.read(&mut buffer).await.unwrap();
        let records = assembler.push(&buffer[..count]).unwrap();
        assert_eq!(records.len(), 1);
        let (reply, results) = decode_reply(&records[0]).unwrap();
        assert_eq!(reply.xid, 42);
        assert_eq!(reply.accept_stat, Some(0));
        results.end("NULL reply").unwrap();
        server.close().await.unwrap();
    }
}
