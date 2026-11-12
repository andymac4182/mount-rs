//! Native-driver adapter. A handle keeps no lock of its own: the owner's
//! per-handle gate orders cursor moves, mutations and close, and makes close
//! wait for every call it admitted. The client only remembers whether the
//! handle is open, so a closed handle is refused without a round trip.
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
const OPEN: u8 = 0;
const CLOSING: u8 = 1;
const CLOSED: u8 = 2;
struct RemoteHandle {
    session: Arc<RemoteSession>,
    id: u64,
    /// `OPEN` until a close is sent, `CLOSING` while one is in flight and
    /// `CLOSED` once the owner has answered it. Only `OPEN` admits a call.
    ///
    /// This is a hint, not the exclusion: a call that read `OPEN` can reach
    /// the owner after a close. The owner's handle gate decides that race.
    /// Either the call runs first and close waits for it, or close runs
    /// first and the call is answered `EBADF`. The cursor lives on the
    /// owner too, so nothing here is read and written back across a request.
    state: std::sync::atomic::AtomicU8,
}
/// Puts a handle whose close did not succeed back to `OPEN`, also when the
/// close future is dropped, so calls and a retried close are sent again.
struct Reopen<'a>(&'a std::sync::atomic::AtomicU8);
impl Drop for Reopen<'_> {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;
        // Fails, as it must, once any close has stored `CLOSED`.
        let _ = self
            .0
            .compare_exchange(CLOSING, OPEN, Ordering::AcqRel, Ordering::Acquire);
    }
}
impl RemoteHandle {
    fn new(session: Arc<RemoteSession>, id: u64) -> Self {
        Self {
            session,
            id,
            state: std::sync::atomic::AtomicU8::new(OPEN),
        }
    }
    fn check_open(&self) -> Result<()> {
        if self.state.load(std::sync::atomic::Ordering::Acquire) != OPEN {
            return Err(FsError::new(ErrorCode::Ebadf).with_syscall("remote"));
        }
        Ok(())
    }
    async fn mutation(&self, operation: Value) -> Result<()> {
        self.check_open()?;
        empty(&self.session, self.session.control(&operation).await?)
    }
}
#[async_trait]
impl FileHandle for RemoteHandle {
    fn fd(&self) -> Option<u64> {
        Some(self.id)
    }
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        self.check_open()?;
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
        self.check_open()?;
        self.session
            .write(
                self.id,
                &buffer[..buffer.len().min(self.session.max_io_bytes())],
                position,
            )
            .await
    }
    async fn stat(&self) -> Result<Stats> {
        self.check_open()?;
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
        use std::sync::atomic::Ordering;
        if self.state.load(Ordering::Acquire) == CLOSED {
            return Ok(());
        }
        // A close that finds another in flight does not wait here and is
        // not answered from its state: it is sent as well. The owner takes
        // the handle exclusively for each close, so this one is answered
        // only after the first has finished: `EBADF` or success once the
        // handle is gone, or the result of closing it again if the first
        // failed. No caller is told the handle is closed before it is.
        let _ = self
            .state
            .compare_exchange(OPEN, CLOSING, Ordering::AcqRel, Ordering::Acquire);
        let _reopen = Reopen(&self.state);
        match self
            .session
            .control(&json!({"operation":"close","handle":self.id}))
            .await
        {
            Ok(bytes) => empty(&self.session, bytes)?,
            Err(error) if error.code == ErrorCode::Ebadf => {}
            Err(error) => return Err(error),
        }
        self.state.store(CLOSED, Ordering::Release);
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
        Ok(Arc::new(RemoteHandle::new(self.session.clone(), id)))
    }
    async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        if self.session.write_file(path, data).await? {
            return Ok(());
        }
        mount_rs_core::driver::write_file_through_handle(self, path, data).await
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
    /// The handle has no lock: what orders a call against a close is the
    /// owner's per-handle gate. So this runs against the real owner session
    /// and checks what a caller of close relies on. When any close returns
    /// Ok the owner no longer has the handle, nothing admitted before it is
    /// still running there, and every write that was acknowledged is in the
    /// file. Two closes race each other and eight callers on every round.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn close_returns_only_after_the_owner_has_closed_the_handle() {
        use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
        use std::sync::atomic::{AtomicBool, Ordering};
        const WORKERS: usize = 8;
        let owner = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory("close-race", 4096))
                .await
                .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(owner.clone(), 2, 4, Duration::from_secs(60)).unwrap());
        let secret = "c".repeat(32);
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
        let remote = RemoteFs::connect(&server.url(), "volume", &secret, Duration::from_secs(10))
            .await
            .unwrap();
        let contents = |remote: RemoteFs| async move {
            let file = remote.open("/race", "r", 0).await.unwrap();
            let mut bytes = [0u8; WORKERS];
            let count = file.read(&mut bytes, Some(0)).await.unwrap();
            file.close().await.unwrap();
            (count, bytes)
        };
        for round in 1u8..=24 {
            let opened = remote.open("/race", "w+", 0o600).await.unwrap();
            let id = opened.fd().unwrap();
            let handle = Arc::new(RemoteHandle::new(remote.session.clone(), id));
            let started = Arc::new(tokio::sync::Barrier::new(WORKERS + 1));
            let stop = Arc::new(AtomicBool::new(false));
            let mut workers = Vec::new();
            for worker in 0..WORKERS {
                let handle = handle.clone();
                let started = started.clone();
                let stop = stop.clone();
                workers.push(tokio::spawn(async move {
                    // The last byte this worker was told is in the file.
                    let mut acknowledged = None;
                    let mut first = true;
                    for value in (1u8..=250).cycle() {
                        let written = handle.write(&[value], Some(worker as u64)).await;
                        if first {
                            first = false;
                            started.wait().await;
                        }
                        let mut byte = [0u8; 1];
                        // Positional and cursor reads, a stat, a sync and a
                        // truncate that changes nothing: every kind of call.
                        let results = [
                            written.map(|count| {
                                assert_eq!(count, 1);
                                acknowledged = Some(value);
                            }),
                            handle
                                .read(&mut byte, Some(worker as u64))
                                .await
                                .map(|_| ()),
                            handle.read(&mut byte, None).await.map(|_| ()),
                            handle.stat().await.map(|_| ()),
                            handle.datasync().await,
                            handle.truncate(WORKERS as u64).await,
                        ];
                        let mut refused = false;
                        for result in results {
                            match result {
                                Ok(()) => {}
                                Err(error) if error.code == ErrorCode::Ebadf => refused = true,
                                Err(error) => panic!("round {round}: {error:?}"),
                            }
                        }
                        if refused || stop.load(Ordering::Acquire) {
                            break;
                        }
                    }
                    acknowledged
                }));
            }
            started.wait().await;
            let mut closers = Vec::new();
            for _ in 0..2 {
                let handle = handle.clone();
                let remote = remote.clone();
                closers.push(tokio::spawn(async move {
                    handle.close().await.unwrap();
                    // Asked of the owner directly, past the handle's own
                    // state: at the moment close returned Ok, to whichever
                    // caller, the owner had already let go of the handle.
                    let stat = remote
                        .session
                        .control(&json!({"operation":"stat","handle":id}))
                        .await;
                    assert_eq!(stat.unwrap_err().code, ErrorCode::Ebadf, "round {round}");
                }));
            }
            for closer in closers {
                within(closer).await.unwrap();
            }
            // Workers are still running here: anything they send now must be
            // refused, so the file cannot change after close returned.
            let at_close = contents(remote.clone()).await;
            stop.store(true, Ordering::Release);
            let mut acknowledged = Vec::new();
            for worker in workers {
                acknowledged.push(within(worker).await.unwrap());
            }
            let settled = contents(remote.clone()).await;
            assert_eq!(at_close, settled, "round {round}: a call ran after close");
            assert_eq!(settled.0, WORKERS, "round {round}");
            for (worker, value) in acknowledged.into_iter().enumerate() {
                // The barrier let no close start before each first write
                // was answered, so every worker has one acknowledged.
                assert_eq!(
                    Some(settled.1[worker]),
                    value,
                    "round {round}: worker {worker} lost an acknowledged write"
                );
            }
            assert_eq!(
                handle.stat().await.unwrap_err().code,
                ErrorCode::Ebadf,
                "round {round}"
            );
            handle.close().await.unwrap();
            drop(opened);
        }
        remote.shutdown().await.unwrap();
        server.close().await.unwrap();
        registry.close().await.unwrap();
        owner.shutdown().await.unwrap();
    }

    /// A wait that fails the test instead of hanging it.
    async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(60), future)
            .await
            .expect("timed out")
    }

    /// Reads one request and returns its body.
    async fn owner_request(stream: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut bytes = Vec::new();
        let mut chunk = [0; 1024];
        loop {
            let count = stream.read(&mut chunk).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                let length = std::str::from_utf8(&bytes[..end])
                    .unwrap()
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    return String::from_utf8(bytes[end + 4..].to_vec()).unwrap();
                }
            }
        }
    }
    async fn owner_reply(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
        use tokio::io::AsyncWriteExt;
        let reply = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(reply.as_bytes()).await.unwrap();
    }
    async fn owner_session(listener: &tokio::net::TcpListener) {
        let (mut stream, _) = listener.accept().await.unwrap();
        owner_request(&mut stream).await;
        let body = json!({"protocol_version":1,"session":"a".repeat(64),"idle_timeout_ms":60000});
        owner_reply(&mut stream, "201 Created", &body.to_string()).await;
    }
    /// The next request, which must be a close of handle 7.
    async fn owner_close(listener: &tokio::net::TcpListener) -> tokio::net::TcpStream {
        let (mut stream, _) = listener.accept().await.unwrap();
        let request: Value = serde_json::from_str(&owner_request(&mut stream).await).unwrap();
        assert_eq!(request, json!({"operation":"close","handle":7}));
        stream
    }

    /// A scripted owner, so each answer to close is chosen. A refused close
    /// leaves the handle open and can be sent again; while one close is in
    /// flight a second is sent too, and neither returns before its own
    /// answer; a closed handle sends nothing more.
    #[tokio::test]
    async fn refused_close_is_retried_and_overlapping_closes_each_wait_for_the_owner() {
        use std::sync::atomic::Ordering;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (both_sent, both_arrived) = tokio::sync::oneshot::channel();
        let (answer_first, first) = tokio::sync::oneshot::channel::<()>();
        let (answer_second, second) = tokio::sync::oneshot::channel::<()>();
        let owner = tokio::spawn(async move {
            owner_session(&listener).await;
            let mut refused = owner_close(&listener).await;
            owner_reply(
                &mut refused,
                "409 Conflict",
                r#"{"error":{"code":"EBUSY"}}"#,
            )
            .await;
            let mut one = owner_close(&listener).await;
            let mut two = owner_close(&listener).await;
            both_sent.send(()).unwrap();
            first.await.unwrap();
            owner_reply(&mut one, "204 No Content", "").await;
            second.await.unwrap();
            // What the owner says to a close that waited out another.
            owner_reply(&mut two, "404 Not Found", r#"{"error":{"code":"EBADF"}}"#).await;
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err(),
                "a closed handle sent another request"
            );
        });
        let session = Arc::new(
            RemoteSession::connect(&endpoint, "volume", &"s".repeat(32), Duration::from_secs(5))
                .await
                .unwrap(),
        );
        let handle = Arc::new(RemoteHandle::new(session, 7));
        assert_eq!(handle.close().await.unwrap_err().code, ErrorCode::Ebusy);
        assert_eq!(handle.state.load(Ordering::Acquire), OPEN);
        let closes: Vec<_> = (0..2)
            .map(|_| {
                let handle = handle.clone();
                tokio::spawn(async move { handle.close().await })
            })
            .collect();
        within(both_arrived).await.unwrap();
        // Calls are refused here while a close is in flight.
        assert_eq!(handle.state.load(Ordering::Acquire), CLOSING);
        assert_eq!(handle.stat().await.unwrap_err().code, ErrorCode::Ebadf);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(closes.iter().all(|close| !close.is_finished()));
        answer_first.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while handle.state.load(Ordering::Acquire) != CLOSED {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            closes.iter().filter(|close| close.is_finished()).count(),
            1,
            "the second close returned before the owner answered it"
        );
        answer_second.send(()).unwrap();
        for close in closes {
            within(close).await.unwrap().unwrap();
        }
        assert_eq!(handle.state.load(Ordering::Acquire), CLOSED);
        handle.close().await.unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(
            handle.read(&mut byte, Some(0)).await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        assert_eq!(
            handle.write(b"x", None).await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        within(owner).await.unwrap();
    }

    /// A close whose caller stops waiting has an unknown outcome, so the
    /// handle must not be left refusing a retry as if it were closing.
    #[tokio::test]
    async fn cancelled_close_leaves_the_handle_open() {
        use std::sync::atomic::Ordering;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (sent, arrived) = tokio::sync::oneshot::channel();
        let (finish, finished) = tokio::sync::oneshot::channel::<()>();
        let owner = tokio::spawn(async move {
            owner_session(&listener).await;
            let stream = owner_close(&listener).await;
            sent.send(()).unwrap();
            finished.await.unwrap();
            drop(stream);
        });
        let session = Arc::new(
            RemoteSession::connect(&endpoint, "volume", &"s".repeat(32), Duration::from_secs(5))
                .await
                .unwrap(),
        );
        let handle = Arc::new(RemoteHandle::new(session, 7));
        let closing = handle.clone();
        let close = tokio::spawn(async move { closing.close().await });
        within(arrived).await.unwrap();
        assert_eq!(handle.state.load(Ordering::Acquire), CLOSING);
        close.abort();
        assert!(close.await.unwrap_err().is_cancelled());
        assert_eq!(handle.state.load(Ordering::Acquire), OPEN);
        handle.check_open().unwrap();
        finish.send(()).unwrap();
        within(owner).await.unwrap();
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
    async fn whole_file_writes_take_one_request_and_fall_back_when_they_cannot() {
        use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
        let owner = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory(
                "whole-file-test",
                4096,
            ))
            .await
            .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(owner.clone(), 2, 8, Duration::from_secs(60)).unwrap());
        let secret = "w".repeat(32);
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
        let read_back = |path: &'static str, length: usize| {
            let remote = remote.clone();
            async move {
                let handle = remote.open(path, "r", 0).await.unwrap();
                let mut bytes = vec![0; length + 1];
                let mut read = 0;
                loop {
                    let count = handle
                        .read(&mut bytes[read..], Some(read as u64))
                        .await
                        .unwrap();
                    if count == 0 {
                        break;
                    }
                    read += count;
                }
                handle.close().await.unwrap();
                bytes.truncate(read);
                bytes
            }
        };
        // The owner offers the request, so a small file is one round trip,
        // whatever its name contains.
        let odd = "/a b%\u{fc}\n.txt";
        assert!(remote.session.write_file(odd, b"first").await.unwrap());
        remote.write_file(odd, b"second!").await.unwrap();
        assert_eq!(read_back(odd, 7).await, b"second!");
        remote.write_file(odd, b"").await.unwrap();
        assert_eq!(remote.stat(odd).await.unwrap().size, 0);
        // More than one request can carry: nothing is sent that way, and the
        // write goes through a handle.
        let large = vec![b'x'; MAX_IO + 4];
        assert!(!remote.session.write_file("/large", &large).await.unwrap());
        assert_eq!(
            remote.stat("/large").await.unwrap_err().code,
            ErrorCode::Enoent
        );
        remote.write_file("/large", &large).await.unwrap();
        assert_eq!(read_back("/large", MAX_IO + 4).await, large);
        // An owner that does not offer it gets the handle sequence too.
        remote
            .session
            .whole_file_writes
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!remote.session.write_file("/old", b"old").await.unwrap());
        remote.write_file("/old", b"old owner").await.unwrap();
        assert_eq!(read_back("/old", 9).await, b"old owner");
        // Errors come back as the driver's own.
        remote
            .session
            .whole_file_writes
            .store(true, std::sync::atomic::Ordering::Relaxed);
        remote.mkdir("/dir", MkdirOptions::default()).await.unwrap();
        assert_eq!(
            remote.write_file("/dir", b"x").await.unwrap_err().code,
            ErrorCode::Eisdir
        );
        assert_eq!(
            remote
                .write_file("/missing/file", b"x")
                .await
                .unwrap_err()
                .code,
            ErrorCode::Enoent
        );
        assert!(remote.stat("/dir").await.unwrap().is_directory());
        remote.shutdown().await.unwrap();
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
