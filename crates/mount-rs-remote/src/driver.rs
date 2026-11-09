//! Native-driver adapter. Per-handle locks serialize cursor operations and close.
//! No operation is retried; dropped handles rely on explicit session shutdown
//! or owner-side idle expiry rather than spawning untracked cleanup tasks.
#[cfg(test)]
use super::MAX_IO;
use super::RemoteSession;
use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, FsError, MkdirOptions, OpenFlags,
    Result, Stats, StatsFs,
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct RemoteFs {
    session: Arc<RemoteSession>,
    capabilities: Capabilities,
    utimens: bool,
}
impl RemoteFs {
    /// Enable the negotiated experimental header-only positional-read path.
    pub async fn enable_header_reads(&self) -> Result<()> {
        self.session.enable_header_reads().await
    }

    pub fn diagnostics(&self) -> super::RemoteDiagnostics {
        self.session.diagnostics()
    }

    pub async fn connect(
        server: &str,
        volume: &str,
        credential: &str,
        timeout: Duration,
    ) -> Result<Self> {
        Self::connect_with_http2(server, volume, credential, timeout, false).await
    }

    /// Explicit HTTP/2 transport selection for matched transport comparisons.
    pub async fn connect_with_http2(
        server: &str,
        volume: &str,
        credential: &str,
        timeout: Duration,
        http2: bool,
    ) -> Result<Self> {
        let session = Arc::new(
            RemoteSession::connect_with_http2(server, volume, credential, timeout, http2).await?,
        );
        match session.capabilities().await {
            Ok((capabilities, utimens)) => Ok(Self {
                session,
                capabilities,
                utimens,
            }),
            Err(error) => {
                let _ = session.close().await;
                Err(error)
            }
        }
    }
    /// Run in a caller-owned task and join before shutdown. Any failed refresh
    /// stops the loop; it never recreates a session or replays filesystem I/O.
    /// Dropping the stop sender also stops the worker.
    pub async fn run_keepalive(&self, mut stop: tokio::sync::watch::Receiver<bool>) -> Result<()> {
        let cadence = self.session.idle_timeout() / 3;
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            tokio::select! {
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() { return Ok(()); }
                }
                _ = tokio::time::sleep(cadence) => {
                    // Bound a stalled refresh inside the remaining idle window.
                    // A timed-out dispatched request poisons future admission.
                    let refreshed = tokio::time::timeout(cadence, self.session.capabilities()).await
                        .map_err(|_| self.session.protocol_error())
                        .and_then(|result| result);
                    if let Err(error) = refreshed {
                        // Even a well-formed denial (expired session, revoked
                        // credential or lost owner) ends this session locally.
                        // Preserve the refresh error for the supervisor while
                        // preventing new filesystem operations from dispatching.
                        self.session.protocol_error();
                        return Err(error);
                    }
                }
            }
        }
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.session.close().await
    }
    async fn path(&self, request: Value) -> Result<Vec<u8>> {
        self.session
            .control(&json!({"operation":"path","request":request}))
            .await
    }
    async fn metadata<T: DeserializeOwned>(&self, request: Value) -> Result<T> {
        decode(&self.session, &self.path(request).await?)
    }
    async fn mutate(&self, request: Value) -> Result<()> {
        empty(&self.session, self.path(request).await?)
    }
}
fn decode<T: DeserializeOwned>(session: &RemoteSession, bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|_| session.protocol_error())
}
fn empty(session: &RemoteSession, bytes: Vec<u8>) -> Result<()> {
    if bytes.is_empty() {
        Ok(())
    } else {
        Err(session.protocol_error())
    }
}
#[derive(Clone, Copy, PartialEq)]
enum HandleState {
    Open,
    Closing,
    Closed,
}
struct RemoteHandle {
    session: Arc<RemoteSession>,
    id: u64,
    state: RwLock<HandleState>,
}
impl RemoteHandle {
    async fn admitted(&self) -> Result<tokio::sync::RwLockWriteGuard<'_, HandleState>> {
        let guard = self.state.write().await;
        if *guard != HandleState::Open {
            return Err(FsError::new(ErrorCode::Ebadf).with_syscall("remote"));
        }
        Ok(guard)
    }
    async fn admitted_read(&self) -> Result<tokio::sync::RwLockReadGuard<'_, HandleState>> {
        let guard = self.state.read().await;
        if *guard != HandleState::Open {
            return Err(FsError::new(ErrorCode::Ebadf).with_syscall("remote"));
        }
        Ok(guard)
    }
    async fn mutation(&self, operation: Value) -> Result<()> {
        let _guard = self.admitted().await?;
        empty(&self.session, self.session.control(&operation).await?)
    }
}
#[async_trait]
impl FileHandle for RemoteHandle {
    fn fd(&self) -> Option<u64> {
        Some(self.id)
    }
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        let _exclusive = if position.is_none() {
            Some(self.admitted().await?)
        } else {
            None
        };
        let _shared = if position.is_some() {
            Some(self.admitted_read().await?)
        } else {
            None
        };
        let length = buffer.len().min(self.session.max_io_bytes());
        let bytes = if let Some(position) = position.filter(|_| {
            self.session
                .header_reads
                .load(std::sync::atomic::Ordering::Acquire)
        }) {
            self.session
                .read_at_headers(self.id, length, position)
                .await?
        } else {
            self.session.control(&json!({"operation":"read","handle":self.id,"length":length,"position":position})).await?
        };
        if bytes.len() > length {
            return Err(self.session.protocol_error());
        }
        buffer[..bytes.len()].copy_from_slice(&bytes);
        Ok(bytes.len())
    }
    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        let _guard = self.admitted().await?;
        self.session
            .write(
                self.id,
                &buffer[..buffer.len().min(self.session.max_io_bytes())],
                position,
            )
            .await
    }
    async fn stat(&self) -> Result<Stats> {
        let _guard = self.admitted_read().await?;
        decode(
            &self.session,
            &self
                .session
                .control(&json!({"operation":"stat","handle":self.id}))
                .await?,
        )
    }
    async fn truncate(&self, length: u64) -> Result<()> {
        self.mutation(json!({"operation":"truncate","handle":self.id,"length":length}))
            .await
    }
    async fn sync(&self) -> Result<()> {
        self.mutation(json!({"operation":"sync","handle":self.id,"data_only":false}))
            .await
    }
    async fn datasync(&self) -> Result<()> {
        self.mutation(json!({"operation":"sync","handle":self.id,"data_only":true}))
            .await
    }
    async fn close(&self) -> Result<()> {
        let mut state = self.state.write().await;
        if *state == HandleState::Closed {
            return Ok(());
        }
        *state = HandleState::Closing;
        match self
            .session
            .control(&json!({"operation":"close","handle":self.id}))
            .await
        {
            Ok(bytes) => empty(&self.session, bytes)?,
            Err(error) if error.code == ErrorCode::Ebadf => {}
            Err(error) => return Err(error),
        }
        *state = HandleState::Closed;
        Ok(())
    }
}
#[async_trait]
impl FsDriver for RemoteFs {
    fn capabilities(&self) -> Capabilities {
        self.capabilities
    }
    fn has_utimens(&self) -> bool {
        self.utimens
    }
    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.open_flags(path, OpenFlags::parse(flags, path)?, mode)
            .await
    }
    async fn open_flags(
        &self,
        path: &str,
        flags: OpenFlags,
        mode: u32,
    ) -> Result<Arc<dyn FileHandle>> {
        let bytes = self
            .session
            .control(&json!({"operation":"open","path":path,"mode":mode,
            "flags":{"read":flags.read,"write":flags.write,"create":flags.create,
            "truncate":flags.truncate,"append":flags.append,"exclusive":flags.exclusive}}))
            .await?;
        let reply: Value = decode(&self.session, &bytes)?;
        let id = reply
            .get("handle")
            .and_then(Value::as_u64)
            .filter(|id| *id > 0)
            .ok_or_else(|| self.session.protocol_error())?;
        Ok(Arc::new(RemoteHandle {
            session: self.session.clone(),
            id,
            state: RwLock::new(HandleState::Open),
        }))
    }
    async fn stat(&self, path: &str) -> Result<Stats> {
        self.metadata(json!({"action":"stat","path":path})).await
    }
    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.metadata(json!({"action":"lstat","path":path})).await
    }
    async fn statfs(&self, path: &str) -> Result<StatsFs> {
        self.metadata(json!({"action":"statfs","path":path})).await
    }
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.metadata(json!({"action":"readdir","path":path})).await
    }
    async fn readlink(&self, path: &str) -> Result<String> {
        self.metadata(json!({"action":"readlink","path":path}))
            .await
    }
    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        self.metadata(
            json!({"action":"mkdir","path":path,"recursive":options.recursive,"mode":options.mode}),
        )
        .await
    }
    async fn rmdir(&self, path: &str) -> Result<()> {
        self.mutate(json!({"action":"rmdir","path":path})).await
    }
    async fn unlink(&self, path: &str) -> Result<()> {
        self.mutate(json!({"action":"unlink","path":path})).await
    }
    async fn rename(&self, source: &str, destination: &str) -> Result<()> {
        self.mutate(json!({"action":"rename","source":source,"destination":destination}))
            .await
    }
    async fn link(&self, source: &str, destination: &str) -> Result<()> {
        self.mutate(json!({"action":"link","source":source,"destination":destination}))
            .await
    }
    async fn symlink(&self, target: &str, path: &str) -> Result<()> {
        self.mutate(json!({"action":"symlink","target":target,"path":path}))
            .await
    }
    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        self.mutate(json!({"action":"chmod","path":path,"mode":mode}))
            .await
    }
    async fn chown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.mutate(json!({"action":"chown","path":path,"uid":uid,"gid":gid}))
            .await
    }
    async fn lchown(&self, path: &str, uid: u32, gid: u32) -> Result<()> {
        self.mutate(json!({"action":"lchown","path":path,"uid":uid,"gid":gid}))
            .await
    }
    async fn truncate(&self, path: &str, length: u64) -> Result<()> {
        self.mutate(json!({"action":"truncate","path":path,"length":length}))
            .await
    }
    async fn utimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.mutate(json!({"action":"utimes","path":path,"atime_ms":atime_ms,"mtime_ms":mtime_ms}))
            .await
    }
    async fn lutimes(&self, path: &str, atime_ms: i64, mtime_ms: i64) -> Result<()> {
        self.mutate(json!({"action":"lutimes","path":path,"atime_ms":atime_ms,"mtime_ms":mtime_ms}))
            .await
    }
    async fn utimens(
        &self,
        path: &str,
        atime_ns: i128,
        mtime_ns: i128,
        follow_symlinks: bool,
    ) -> Result<()> {
        self.mutate(json!({"action":"utimens","path":path,"atime_ns":atime_ns.to_string(),"mtime_ns":mtime_ns.to_string(),"follow_symlinks":follow_symlinks})).await
    }
    async fn mknod(&self, path: &str, mode: u32, dev: u64) -> Result<()> {
        self.mutate(json!({"action":"mknod","path":path,"mode":mode,"dev":dev}))
            .await
    }
    async fn syncfs(&self) -> Result<()> {
        self.mutate(json!({"action":"syncfs"})).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn positional_admission_shares_rpc_reads_and_drains_before_close() {
        use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
        let owner = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory(
                "parallel-remote",
                4096,
            ))
            .await
            .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(owner.clone(), 2, 4, Duration::from_secs(60)).unwrap());
        let secret = "s".repeat(32);
        let handler = SessionHttpHandler::new(
            "volume",
            SessionCredentials::new(vec![("client".into(), secret.clone())]).unwrap(),
            registry.clone(),
        )
        .unwrap();
        let server =
            mount_rs_http::HttpServer::start_with_handler(Arc::new(handler), Default::default())
                .await
                .unwrap();
        let remote = RemoteFs::connect(&server.url(), "volume", &secret, Duration::from_secs(5))
            .await
            .unwrap();
        let opened = remote.open("/data", "w+", 0o600).await.unwrap();
        let handle = RemoteHandle {
            session: remote.session.clone(),
            id: opened.fd().unwrap(),
            state: RwLock::new(HandleState::Open),
        };
        handle.write(b"data", Some(0)).await.unwrap();
        let admitted = handle.admitted_read().await.unwrap();
        let mut bytes = [0; 4];
        for headers in [false, true] {
            if headers {
                remote.enable_header_reads().await.unwrap();
            }
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(1), handle.read(&mut bytes, Some(0)))
                    .await
                    .unwrap()
                    .unwrap(),
                4
            );
            assert_eq!(&bytes, b"data");
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), handle.stat())
                .await
                .unwrap()
                .unwrap()
                .size,
            4
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), handle.read(&mut bytes, None))
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), handle.write(b"x", Some(0)))
                .await
                .is_err()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), handle.close())
                .await
                .is_err()
        );
        drop(admitted);
        assert_eq!(handle.read(&mut bytes, None).await.unwrap(), 4);
        assert_eq!(&bytes, b"data");
        handle.close().await.unwrap();
        assert_eq!(
            handle.read(&mut bytes, Some(0)).await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        remote.shutdown().await.unwrap();
        server.close().await.unwrap();
        registry.close().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn independent_remote_writers_append_without_lost_or_torn_records() {
        use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
        let owner = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory(
                "remote-writers",
                4096,
            ))
            .await
            .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(owner.clone(), 4, 4, Duration::from_secs(60)).unwrap());
        let credentials = (0u32..4)
            .map(|i| (format!("writer-{i}"), i.to_string().repeat(32)))
            .collect();
        let handler = SessionHttpHandler::new(
            "volume",
            SessionCredentials::new(credentials).unwrap(),
            registry.clone(),
        )
        .unwrap();
        let server =
            mount_rs_http::HttpServer::start_with_handler(Arc::new(handler), Default::default())
                .await
                .unwrap();
        let barrier = Arc::new(tokio::sync::Barrier::new(4));
        let mut tasks = Vec::new();
        for writer in 0u32..4 {
            let remote = RemoteFs::connect(
                &server.url(),
                "volume",
                &writer.to_string().repeat(32),
                Duration::from_secs(5),
            )
            .await
            .unwrap();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                let handle = remote.open("/append", "a+", 0o600).await.unwrap();
                barrier.wait().await;
                for sequence in 0u32..32 {
                    let mut record = [0u8; 16];
                    record[..4].copy_from_slice(&writer.to_le_bytes());
                    record[4..8].copy_from_slice(&sequence.to_le_bytes());
                    record[8..].fill((writer * 32 + sequence) as u8);
                    assert_eq!(handle.write(&record, None).await.unwrap(), record.len());
                }
                handle.sync().await.unwrap();
                handle.close().await.unwrap();
                remote.shutdown().await.unwrap();
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        // Reuse freed session capacity and verify through the same remote API.
        let reader = RemoteFs::connect(
            &server.url(),
            "volume",
            &"0".repeat(32),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        let file = reader.open("/append", "r", 0).await.unwrap();
        assert_eq!(file.stat().await.unwrap().size, 4 * 32 * 16);
        let mut bytes = vec![0; 4 * 32 * 16];
        assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), bytes.len());
        let mut seen = std::collections::BTreeSet::new();
        for record in bytes.chunks_exact(16) {
            let writer = u32::from_le_bytes(record[..4].try_into().unwrap());
            let sequence = u32::from_le_bytes(record[4..8].try_into().unwrap());
            assert!(writer < 4 && sequence < 32);
            assert!(
                record[8..]
                    .iter()
                    .all(|byte| *byte == (writer * 32 + sequence) as u8)
            );
            assert!(seen.insert((writer, sequence)), "duplicate record");
        }
        assert_eq!(seen.len(), 128);
        file.close().await.unwrap();
        reader.shutdown().await.unwrap();
        server.close().await.unwrap();
        registry.close().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn keepalive_preserves_idle_handle_and_joins_on_stop() {
        use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
        let owner = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory("keepalive", 4096))
                .await
                .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(owner.clone(), 1, 4, Duration::from_secs(1)).unwrap());
        let secret = "k".repeat(32);
        let handler = SessionHttpHandler::new(
            "volume",
            SessionCredentials::new(vec![("client".into(), secret.clone())]).unwrap(),
            registry.clone(),
        )
        .unwrap();
        let server =
            mount_rs_http::HttpServer::start_with_handler(Arc::new(handler), Default::default())
                .await
                .unwrap();
        let remote = RemoteFs::connect(&server.url(), "volume", &secret, Duration::from_secs(2))
            .await
            .unwrap();
        let file = remote.open("/idle", "w+", 0o600).await.unwrap();
        let (stop, receiver) = tokio::sync::watch::channel(false);
        let worker_remote = remote.clone();
        let worker = tokio::spawn(async move { worker_remote.run_keepalive(receiver).await });
        for _ in 0..7 {
            tokio::time::sleep(Duration::from_millis(350)).await;
            assert_eq!(registry.expire_idle().await.unwrap(), 0);
        }
        assert_eq!(file.stat().await.unwrap().size, 0);
        stop.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        // Without the worker, the same idle session expires normally.
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert_eq!(registry.expire_idle().await.unwrap(), 1);
        let (_stop, receiver) = tokio::sync::watch::channel(false);
        let failure = tokio::time::timeout(Duration::from_secs(2), remote.run_keepalive(receiver))
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(failure.code, ErrorCode::Eacces);
        assert_eq!(remote.stat("/").await.unwrap_err().code, ErrorCode::Eio);
        // Explicit cleanup still reaches the server: it returns its denial,
        // rather than the local failed-session admission error.
        assert_eq!(remote.shutdown().await.unwrap_err().code, ErrorCode::Eacces);
        server.close().await.unwrap();
        registry.close().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn native_traits_preserve_partial_io_identity_and_path_semantics() {
        use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
        let owner = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory(
                "driver-test",
                4096,
            ))
            .await
            .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(owner.clone(), 2, 8, Duration::from_secs(60)).unwrap());
        let secret = "t".repeat(32);
        let handler = SessionHttpHandler::new(
            "volume",
            SessionCredentials::new(vec![("client".into(), secret.clone())]).unwrap(),
            registry.clone(),
        )
        .unwrap();
        let server =
            mount_rs_http::HttpServer::start_with_handler(Arc::new(handler), Default::default())
                .await
                .unwrap();
        let remote = RemoteFs::connect(&server.url(), "volume", &secret, Duration::from_secs(5))
            .await
            .unwrap();
        let driver: Arc<dyn FsDriver> = Arc::new(remote.clone());
        assert!(driver.capabilities().handles);
        driver.mkdir("/dir", MkdirOptions::default()).await.unwrap();
        let file = driver.open("/dir/file", "wx+", 0o600).await.unwrap();
        let payload = vec![b'x'; MAX_IO + 4];
        assert_eq!(file.write(&payload, None).await.unwrap(), MAX_IO);
        assert_eq!(file.write(&payload[MAX_IO..], None).await.unwrap(), 4);
        let mut bytes = vec![0; MAX_IO + 4];
        assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), MAX_IO);
        assert_eq!(&bytes[..MAX_IO], &payload[..MAX_IO]);
        let mut tail = [0; 8];
        assert_eq!(
            file.read(&mut tail, Some(MAX_IO as u64 - 2)).await.unwrap(),
            6
        );
        assert_eq!(&tail[..6], &[b'x'; 6]);
        driver.rename("/dir/file", "/dir/renamed").await.unwrap();
        driver.unlink("/dir/renamed").await.unwrap();
        let replacement = driver
            .open_flags(
                "/dir/file",
                OpenFlags {
                    read: true,
                    write: true,
                    create: true,
                    truncate: false,
                    append: false,
                    exclusive: false,
                },
                0o600,
            )
            .await
            .unwrap();
        replacement.write(b"new", Some(0)).await.unwrap();
        assert_ne!(
            file.stat().await.unwrap().ino,
            replacement.stat().await.unwrap().ino
        );
        assert_eq!(file.read(&mut tail, Some(0)).await.unwrap(), 8);
        assert_eq!(&tail, &[b'x'; 8]);
        file.truncate(3).await.unwrap();
        file.sync().await.unwrap();
        file.datasync().await.unwrap();
        assert_eq!(file.stat().await.unwrap().size, 3);
        assert_eq!(driver.stat("/dir/file").await.unwrap().size, 3);
        assert_eq!(driver.readdir("/dir").await.unwrap().len(), 1);
        driver.statfs("/").await.unwrap();
        driver.syncfs().await.unwrap();
        assert_eq!(
            driver.stat("/missing").await.unwrap_err().code,
            ErrorCode::Enoent
        );
        assert!(!driver.has_utimens());
        assert_eq!(
            driver
                .utimens("/dir/file", i128::MAX, i128::MIN, true)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Enosys
        );
        file.close().await.unwrap();
        file.close().await.unwrap();
        assert_eq!(file.stat().await.unwrap_err().code, ErrorCode::Ebadf);
        replacement.close().await.unwrap();
        driver.unlink("/dir/file").await.unwrap();
        driver.rmdir("/dir").await.unwrap();
        remote.shutdown().await.unwrap();
        server.close().await.unwrap();
        registry.close().await.unwrap();
        owner.shutdown().await.unwrap();
    }
}

#[cfg(test)]
#[path = "parallel_tests.rs"]
mod parallel_tests;
