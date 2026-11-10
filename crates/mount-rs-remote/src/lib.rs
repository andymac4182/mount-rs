//! Client transport for the public session endpoint. No Redis, TiDB or block
//! provider configuration is accepted. Explicit shutdown is required; abandoned
//! sessions rely on the server's bounded idle-expiry lifecycle.
use mount_rs_core::{Capabilities, ErrorCode, FsError, Result};
use reqwest::{
    Client, Method, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use std::{
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    time::{Duration, Instant},
};
mod driver;
pub use driver::RemoteFs;

const MAX_REPLY: usize = 1024 * 1024;
const MAX_IO: usize = 65536;

/// Cumulative transport stages, excluding session creation. Snapshot deltas
/// require quiescent callers; concurrent calls are aggregated, not correlated.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct RemoteDiagnostics {
    /// Optional benchmark-only ingress timing, never used for request decisions.
    pub audit_ingress_samples: u64,
    pub audit_ingress_nanoseconds: u64,
    pub response_headers: u64,
    pub response_header_nanoseconds: u64,
    pub successful_bodies: u64,
    pub successful_body_nanoseconds: u64,
}
#[derive(Default)]
struct RemoteCounters {
    audit_samples: AtomicU64,
    audit_ns: AtomicU64,
    headers: AtomicU64,
    header_ns: AtomicU64,
    bodies: AtomicU64,
    body_ns: AtomicU64,
}
fn elapsed_nanoseconds(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX)
}
pub struct RemoteSession {
    counters: RemoteCounters,
    header_reads: AtomicBool,
    /// The owner accepts a whole-file write as one request.
    whole_file_writes: AtomicBool,
    max_io_bytes: AtomicUsize,
    client: Client,
    lifecycle: Url,
    handles: Url,
    token: HeaderValue,
    idle_timeout: Duration,
    closing: AtomicBool,
    uncertain: AtomicBool,
}
impl std::fmt::Debug for RemoteSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteSession")
            .field("credentials", &"<redacted>")
            .field("closing", &self.closing.load(Ordering::Acquire))
            .field("uncertain", &self.uncertain.load(Ordering::Acquire))
            .finish()
    }
}
fn invalid() -> FsError {
    FsError::new(ErrorCode::Einval).with_syscall("remote")
}
fn unknown() -> FsError {
    FsError::new(ErrorCode::Eio)
        .with_syscall("remote")
        .with_message("remote operation outcome unknown; request was not replayed")
}
fn endpoint(value: &str, volume: &str) -> Result<Url> {
    let mut url = Url::parse(value).map_err(|_| invalid())?;
    let local = matches!(url.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"));
    if (url.scheme() != "https" && !(url.scheme() == "http" && local))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
        || matches!(volume, "" | "." | "..")
        || volume.len() > 128
        || !volume
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(invalid());
    }
    url.set_path(&format!("/v1/drives/{volume}/sessions"));
    Ok(url)
}
#[cfg(feature = "body-timing")]
fn wall_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
}
#[cfg(feature = "body-timing")]
#[derive(Clone, Copy, Debug, Default)]
pub struct BodyTiming {
    /// Forwarding handler and its nested peer-header wait from an instrumented ingress.
    pub ingress_and_peer_ns: Option<(u64, u64)>,
    /// Route total, gate, registry, connection acquisition and stream opening.
    pub forwarding_stages_ns: [Option<u64>; 5],
    /// Owner stream queue, stream resolve, admission and application time.
    pub owner_stages_ns: [Option<u64>; 4],
    /// Same-host wall clock (Unix ns): client send, ingress handler start,
    /// owner handler start, owner response headers, ingress handler end,
    /// client response headers.
    pub walls_ns: [Option<u64>; 6],
    /// Public request send through observing response headers.
    pub headers: Duration,
    /// Time between polls of the pending public request.
    pub headers_pending_ns: u64,
    /// Pending time after an observed wake of the public request.
    pub headers_notified_ns: u64,
    /// Time from entering receive until the first nonempty chunk is available.
    pub first_chunk: Option<Duration>,
    /// Time from entering receive until the last nonempty chunk is available.
    pub last_chunk: Option<Duration>,
    /// Pending time between the first and last nonempty chunk, excluding EOF.
    pub inter_chunk_pending_ns: u64,
    /// Portion of inter-chunk pending time after the first observed wake.
    pub inter_chunk_notified_ns: u64,
    /// Time from entering receive until the body stream reports completion.
    pub end: Duration,
}

#[cfg(feature = "body-timing")]
tokio::task_local! {
    /// Scope to one request to capture its completed response body.
    pub static BODY_TIMING: std::cell::Cell<Option<BodyTiming>>;
}

async fn receive(mut response: reqwest::Response) -> Result<Vec<u8>> {
    #[cfg(feature = "body-timing")]
    let started = Instant::now();
    #[cfg(feature = "body-timing")]
    let mut timing = BodyTiming::default();
    if response
        .content_length()
        .is_some_and(|length| length > MAX_REPLY as u64)
    {
        return Err(unknown());
    }
    let status = response.status();
    let mut bytes = Vec::new();
    #[cfg(feature = "body-timing")]
    let mut inter_chunk = mount_rs_core::wait_timing::WaitTiming::default();
    loop {
        #[cfg(feature = "body-timing")]
        let next = if timing.first_chunk.is_some() && BODY_TIMING.try_with(|_| ()).is_ok() {
            let (next, wait) = mount_rs_core::wait_timing::observe(response.chunk()).await;
            inter_chunk.pending_ns += wait.pending_ns;
            inter_chunk.notified_ns += wait.notified_ns;
            next
        } else {
            response.chunk().await
        };
        #[cfg(not(feature = "body-timing"))]
        let next = response.chunk().await;
        let Some(chunk) = next.map_err(|_| unknown())? else {
            break;
        };
        if chunk.len() > MAX_REPLY - bytes.len() {
            return Err(unknown());
        }
        #[cfg(feature = "body-timing")]
        if !chunk.is_empty() {
            let elapsed = started.elapsed();
            timing.first_chunk.get_or_insert(elapsed);
            timing.last_chunk = Some(elapsed);
            timing.inter_chunk_pending_ns = inter_chunk.pending_ns;
            timing.inter_chunk_notified_ns = inter_chunk.notified_ns;
        }
        bytes.extend_from_slice(&chunk);
    }
    #[cfg(feature = "body-timing")]
    {
        timing.end = started.elapsed();
        let _ = BODY_TIMING.try_with(|slot| slot.set(Some(timing)));
    }
    if status.is_success() {
        return Ok(bytes);
    }
    if status.is_client_error() || status.is_server_error() {
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| unknown())?;
        let code = value
            .get("error")
            .and_then(|v| v.get("code"))
            .and_then(|v| v.as_str())
            .and_then(error_code)
            .ok_or_else(unknown)?;
        return Err(FsError::new(code).with_syscall("remote"));
    }
    Err(unknown())
}
struct PendingRequest<'a> {
    uncertain: &'a AtomicBool,
    completed: bool,
}
impl Drop for PendingRequest<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.uncertain.store(true, Ordering::Release);
        }
    }
}
impl RemoteSession {
    /// Validate connection settings without credentials or network access.
    pub fn validate_endpoint(server: &str, volume: &str) -> Result<()> {
        endpoint(server, volume).map(|_| ())
    }

    pub async fn connect(
        server: &str,
        volume: &str,
        bearer: &str,
        timeout: Duration,
    ) -> Result<Self> {
        Self::connect_with_http2(server, volume, bearer, timeout, false).await
    }

    /// Select prior-knowledge HTTP/2 explicitly; no protocol fallback or retries.
    pub async fn connect_with_http2(
        server: &str,
        volume: &str,
        bearer: &str,
        timeout: Duration,
        http2: bool,
    ) -> Result<Self> {
        let lifecycle = endpoint(server, volume)?;
        if timeout.is_zero() || timeout > Duration::from_secs(300) {
            return Err(invalid());
        }
        let mut authorization =
            HeaderValue::from_str(&format!("Bearer {bearer}")).map_err(|_| invalid())?;
        authorization.set_sensitive(true);
        let mut headers = HeaderMap::new();
        headers.insert(AUTHORIZATION, authorization);
        let builder = Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .timeout(timeout);
        let builder = if http2 {
            builder.http2_prior_knowledge()
        } else {
            builder
        };
        let client = builder.build().map_err(|_| invalid())?;
        let response = client
            .post(lifecycle.clone())
            .body(Vec::new())
            .send()
            .await
            .map_err(|_| unknown())?;
        if http2 && response.version() != reqwest::Version::HTTP_2 {
            return Err(unknown());
        }
        let status = response.status();
        let bytes = receive(response).await?;
        if status != reqwest::StatusCode::CREATED {
            return Err(unknown());
        }
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| unknown())?;
        if value.get("protocol_version").and_then(|v| v.as_u64()) != Some(1) {
            return Err(unknown());
        }
        let idle_ms = value
            .get("idle_timeout_ms")
            .and_then(|v| v.as_u64())
            .filter(|ms| (1000..=3_600_000).contains(ms))
            .ok_or_else(unknown)?;
        let token = value
            .get("session")
            .and_then(|v| v.as_str())
            .ok_or_else(unknown)?;
        if token.len() != 64
            || !token
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(unknown());
        }
        let mut token = HeaderValue::from_str(token).map_err(|_| unknown())?;
        token.set_sensitive(true);
        let mut handles = lifecycle.clone();
        handles.set_path(&format!("{}/handles", lifecycle.path()));
        Ok(Self {
            max_io_bytes: AtomicUsize::new(MAX_IO),
            counters: RemoteCounters::default(),
            header_reads: AtomicBool::new(false),
            whole_file_writes: AtomicBool::new(false),
            client,
            lifecycle,
            handles,
            token,
            idle_timeout: Duration::from_millis(idle_ms),
            closing: AtomicBool::new(false),
            uncertain: AtomicBool::new(false),
        })
    }
    /// Request dispatch through response headers, followed by successful body
    /// reception. These include runtime/network waits, but exclude request
    /// construction, handle-lock admission and typed response decoding.
    pub fn diagnostics(&self) -> RemoteDiagnostics {
        RemoteDiagnostics {
            audit_ingress_samples: self.counters.audit_samples.load(Ordering::Relaxed),
            audit_ingress_nanoseconds: self.counters.audit_ns.load(Ordering::Relaxed),
            response_headers: self.counters.headers.load(Ordering::Relaxed),
            response_header_nanoseconds: self.counters.header_ns.load(Ordering::Relaxed),
            successful_bodies: self.counters.bodies.load(Ordering::Relaxed),
            successful_body_nanoseconds: self.counters.body_ns.load(Ordering::Relaxed),
        }
    }
    /// Server-advertised idle expiry. Keepalive callers must allow network and
    /// scheduling time within this window; this is not a lease extension promise.
    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    pub(crate) fn protocol_error(&self) -> FsError {
        self.uncertain.store(true, Ordering::Release);
        unknown()
    }
    fn admitted(&self) -> Result<()> {
        if self.closing.load(Ordering::Acquire) {
            return Err(FsError::new(ErrorCode::Ebadf).with_syscall("remote"));
        }
        if self.uncertain.load(Ordering::Acquire) {
            return Err(unknown());
        }
        Ok(())
    }
    async fn send(&self, request: reqwest::RequestBuilder, expected: &[u16]) -> Result<Vec<u8>> {
        let mut pending = PendingRequest {
            uncertain: &self.uncertain,
            completed: false,
        };
        let headers_start = Instant::now();
        #[cfg(feature = "body-timing")]
        let send_wall = wall_ns();
        #[cfg(feature = "body-timing")]
        let (sent, header_wait) = if BODY_TIMING.try_with(|_| ()).is_ok() {
            mount_rs_core::wait_timing::observe(request.send()).await
        } else {
            (request.send().await, Default::default())
        };
        #[cfg(not(feature = "body-timing"))]
        let sent = request.send().await;
        #[cfg(feature = "body-timing")]
        let headers = headers_start.elapsed();
        #[cfg(feature = "body-timing")]
        let headers_wall = wall_ns();
        let response = match sent {
            Ok(response) => response,
            Err(_) => {
                self.uncertain.store(true, Ordering::Release);
                return Err(unknown());
            }
        };
        // Only diagnostic arithmetic uses this bounded optional response hint.
        // Missing/malformed values do not affect protocol or filesystem behavior.
        if let Some(nanos) = response
            .headers()
            .get("x-mount-audit-ingress-ns")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v <= 300_000_000_000)
        {
            self.counters.audit_ns.fetch_add(nanos, Ordering::Relaxed);
            self.counters.audit_samples.fetch_add(1, Ordering::Relaxed);
        }
        #[cfg(feature = "body-timing")]
        let (ingress_and_peer_ns, forwarding_stages_ns, owner_stages_ns, walls_ns) = {
            let parse = |key| {
                response
                    .headers()
                    .get::<&str>(key)?
                    .to_str()
                    .ok()?
                    .parse::<u64>()
                    .ok()
            };
            (
                parse("x-mount-audit-ingress-ns").zip(parse("x-mount-audit-peer-headers-ns")),
                [
                    "x-mount-audit-route-ns",
                    "x-mount-audit-route-gate-ns",
                    "x-mount-audit-registry-ns",
                    "x-mount-audit-connection-ns",
                    "x-mount-audit-stream-ns",
                ]
                .map(parse),
                [
                    "x-mount-audit-owner-queued-ns",
                    "x-mount-audit-owner-resolve-ns",
                    "x-mount-audit-owner-admission-ns",
                    "x-mount-audit-owner-application-ns",
                ]
                .map(parse),
                [
                    Some(send_wall),
                    parse("x-mount-audit-ingress-start-wall-ns"),
                    parse("x-mount-audit-owner-start-wall-ns"),
                    parse("x-mount-audit-owner-respond-wall-ns"),
                    parse("x-mount-audit-ingress-end-wall-ns"),
                    Some(headers_wall),
                ],
            )
        };
        let status = response.status().as_u16();
        self.counters
            .header_ns
            .fetch_add(elapsed_nanoseconds(headers_start), Ordering::Relaxed);
        self.counters.headers.fetch_add(1, Ordering::Relaxed);
        let body_start = Instant::now();
        let result = receive(response).await;
        #[cfg(feature = "body-timing")]
        let _ = BODY_TIMING.try_with(|slot| {
            if let Some(mut timing) = slot.get() {
                timing.ingress_and_peer_ns = ingress_and_peer_ns;
                timing.forwarding_stages_ns = forwarding_stages_ns;
                timing.owner_stages_ns = owner_stages_ns;
                timing.walls_ns = walls_ns;
                timing.headers = headers;
                timing.headers_pending_ns = header_wait.pending_ns;
                timing.headers_notified_ns = header_wait.notified_ns;
                slot.set(Some(timing));
            }
        });
        if result.is_ok() {
            self.counters
                .body_ns
                .fetch_add(elapsed_nanoseconds(body_start), Ordering::Relaxed);
            self.counters.bodies.fetch_add(1, Ordering::Relaxed);
        }
        if result.is_ok() && !expected.contains(&status) {
            return Err(unknown());
        }
        pending.completed = true;
        // A valid filesystem EIO also conservatively closes admission. Do not
        // let later cursor operations hide an uncertain earlier outcome.
        if result.as_ref().is_err_and(|e| e.code == ErrorCode::Eio) {
            self.uncertain.store(true, Ordering::Release);
        }
        result
    }
    /// Opt-in experimental positional-read transport; requires server support.
    pub async fn enable_header_reads(&self) -> Result<()> {
        let reply = self
            .control(&serde_json::json!({"operation":"capabilities"}))
            .await?;
        let value: serde_json::Value =
            serde_json::from_slice(&reply).map_err(|_| self.protocol_error())?;
        if value
            .get("positional_read_headers")
            .and_then(|v| v.as_bool())
            != Some(true)
        {
            return Err(FsError::new(ErrorCode::Enosys).with_syscall("remote"));
        }
        self.header_reads.store(true, Ordering::Release);
        Ok(())
    }
    async fn read_at_headers(&self, handle: u64, length: usize, position: u64) -> Result<Vec<u8>> {
        self.admitted()?;
        if length > self.max_io_bytes() {
            return Err(FsError::new(ErrorCode::Efbig).with_syscall("read"));
        }
        self.send(
            self.client
                .get(self.handles.clone())
                .header("x-mount-session", self.token.clone())
                .header("x-mount-handle", handle)
                .header("x-mount-length", length)
                .header("x-mount-position", position),
            &[200],
        )
        .await
    }
    pub async fn control(&self, operation: &serde_json::Value) -> Result<Vec<u8>> {
        self.admitted()?;
        let bytes = serde_json::to_vec(operation).map_err(|_| invalid())?;
        if bytes.len() > 8192 {
            return Err(FsError::new(ErrorCode::Efbig).with_syscall("remote"));
        }
        self.send(
            self.client
                .post(self.handles.clone())
                .header("x-mount-session", self.token.clone())
                .body(bytes),
            &[200, 204],
        )
        .await
    }
    pub async fn write(&self, handle: u64, bytes: &[u8], position: Option<u64>) -> Result<usize> {
        self.admitted()?;
        if bytes.len() > self.max_io_bytes() {
            return Err(FsError::new(ErrorCode::Efbig).with_syscall("write"));
        }
        let mut request = self
            .client
            .put(self.handles.clone())
            .header("x-mount-session", self.token.clone())
            .header("x-mount-handle", handle)
            .body(bytes.to_vec());
        if let Some(position) = position {
            request = request.header("x-mount-position", position);
        }
        let reply = self.send(request, &[200]).await?;
        let parsed = serde_json::from_slice::<serde_json::Value>(&reply)
            .ok()
            .and_then(|v| v.get("written").and_then(|n| n.as_u64()))
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n <= bytes.len());
        match parsed {
            Some(count) => Ok(count),
            None => {
                self.uncertain.store(true, Ordering::Release);
                Err(unknown())
            }
        }
    }
    /// Replaces a file's contents in one request. `Ok(false)` means the
    /// owner does not offer that, or the bytes do not fit in one request, and
    /// nothing was sent: the caller writes through a handle instead.
    pub async fn write_file(&self, path: &str, bytes: &[u8]) -> Result<bool> {
        if !self.whole_file_writes.load(Ordering::Relaxed) || bytes.len() > self.max_io_bytes() {
            return Ok(false);
        }
        self.admitted()?;
        // Percent-encode everything a header value or a decoder could trip on.
        let mut encoded = String::with_capacity(path.len());
        for byte in path.bytes() {
            if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
                encoded.push(char::from(byte));
            } else {
                encoded.push_str(&format!("%{byte:02X}"));
            }
        }
        let request = self
            .client
            .put(self.handles.clone())
            .header("x-mount-session", self.token.clone())
            .header("x-mount-write-file", encoded)
            .body(bytes.to_vec());
        let reply = self.send(request, &[204]).await?;
        if !reply.is_empty() {
            return Err(self.protocol_error());
        }
        Ok(true)
    }
    pub(crate) fn max_io_bytes(&self) -> usize {
        self.max_io_bytes.load(Ordering::Relaxed)
    }
    pub async fn capabilities(&self) -> Result<(Capabilities, bool)> {
        let bytes = self
            .control(&serde_json::json!({"operation":"capabilities"}))
            .await?;
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| self.protocol_error())?;
        let max_io_bytes = value
            .get("max_io_bytes")
            .and_then(|v| v.as_u64())
            .filter(|value| (1..=MAX_IO as u64).contains(value))
            .ok_or_else(|| self.protocol_error())? as usize;
        let capabilities = serde_json::from_value(
            value
                .get("capabilities")
                .cloned()
                .ok_or_else(|| self.protocol_error())?,
        )
        .map_err(|_| self.protocol_error())?;
        let utimens = value
            .get("utimens")
            .and_then(|v| v.as_bool())
            .ok_or_else(|| self.protocol_error())?;
        self.max_io_bytes.store(max_io_bytes, Ordering::Relaxed);
        self.whole_file_writes.store(
            value.get("write_file").and_then(|v| v.as_bool()) == Some(true),
            Ordering::Relaxed,
        );
        Ok((capabilities, utimens))
    }
    /// Explicit cleanup remains possible after an uncertain request. No failed
    /// close is automatically retried; idle expiry remains the server fallback.
    pub async fn close(&self) -> Result<()> {
        self.closing.store(true, Ordering::Release);
        self.send(
            self.client
                .request(Method::DELETE, self.lifecycle.clone())
                .header("x-mount-session", self.token.clone())
                .body(Vec::new()),
            &[204],
        )
        .await
        .map(|_| ())
    }
}

fn error_code(value: &str) -> Option<ErrorCode> {
    use ErrorCode::*;
    Some(match value {
        "EPERM" => Eperm,
        "ENOENT" => Enoent,
        "EINTR" => Eintr,
        "EIO" => Eio,
        "ENXIO" => Enxio,
        "EBADF" => Ebadf,
        "EAGAIN" => Eagain,
        "ENOMEM" => Enomem,
        "EACCES" => Eacces,
        "EBUSY" => Ebusy,
        "EEXIST" => Eexist,
        "EXDEV" => Exdev,
        "ENODEV" => Enodev,
        "ENOTDIR" => Enotdir,
        "EISDIR" => Eisdir,
        "EINVAL" => Einval,
        "ENFILE" => Enfile,
        "EMFILE" => Emfile,
        "EFBIG" => Efbig,
        "ENOSPC" => Enospc,
        "ESPIPE" => Espipe,
        "EROFS" => Erofs,
        "EMLINK" => Emlink,
        "ERANGE" => Erange,
        "ENAMETOOLONG" => Enametoolong,
        "ENOSYS" => Enosys,
        "ENOTEMPTY" => Enotempty,
        "ELOOP" => Eloop,
        "ENODATA" => Enodata,
        "EPROTO" => Eproto,
        "EOVERFLOW" => Eoverflow,
        "ENOTSUP" => Enotsup,
        "ESTALE" => Estale,
        "EDQUOT" => Edquot,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    async fn read_request(stream: &mut tokio::net::TcpStream) {
        use tokio::io::AsyncReadExt;
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut bytes = Vec::new();
            let mut chunk = [0; 1024];
            loop {
                let count = stream.read(&mut chunk).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&chunk[..count]);
                assert!(bytes.len() <= 16384);
                if let Some(end) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&bytes[..end]).unwrap();
                    let length = headers
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
        })
        .await
        .unwrap();
    }
    async fn create_reply(stream: &mut tokio::net::TcpStream) {
        use tokio::io::AsyncWriteExt;
        let body = serde_json::json!({"protocol_version":1,"session":"a".repeat(64),"idle_timeout_ms":60000}).to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    }
    #[cfg(feature = "body-timing")]
    #[tokio::test]
    async fn records_chunk_arrival_and_completion_for_the_scoped_request() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndata")
                .await
                .unwrap();
        });
        let response = reqwest::get(url).await.unwrap();
        BODY_TIMING
            .scope(std::cell::Cell::new(None), async {
                assert_eq!(receive(response).await.unwrap(), b"data");
                let timing = BODY_TIMING.with(|slot| slot.get().unwrap());
                assert!(timing.first_chunk.unwrap() <= timing.last_chunk.unwrap());
                assert!(timing.last_chunk.unwrap() <= timing.end);
            })
            .await;
        server.await.unwrap();
    }
    #[tokio::test]
    async fn dispatched_cancellation_closes_admission_without_another_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (received, dispatched) = tokio::sync::oneshot::channel();
        let (release, cleanup) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            create_reply(&mut stream).await;
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            received.send(()).unwrap();
            cleanup.await.unwrap();
            drop(stream);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let remote = Arc::new(
            RemoteSession::connect(&endpoint, "volume", &"s".repeat(32), Duration::from_secs(2))
                .await
                .unwrap(),
        );
        let task_remote = remote.clone();
        let task = tokio::spawn(async move {
            task_remote
                .control(&serde_json::json!({"operation":"read","handle":1,"length":1}))
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), dispatched)
            .await
            .unwrap()
            .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            remote
                .control(&serde_json::json!({"operation":"capabilities"}))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        release.send(()).unwrap();
        server.await.unwrap();
    }
    #[tokio::test]
    async fn truncated_response_fails_and_prevents_followup_requests() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            create_reply(&mut stream).await;
            drop(stream);
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\nshort",
                )
                .await
                .unwrap();
            drop(stream);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let remote =
            RemoteSession::connect(&endpoint, "volume", &"s".repeat(32), Duration::from_secs(2))
                .await
                .unwrap();
        assert_eq!(
            remote
                .control(&serde_json::json!({"operation":"read","handle":1,"length":8}))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        assert_eq!(
            remote
                .control(&serde_json::json!({"operation":"capabilities"}))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eio
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn redirect_is_not_followed_or_retried() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            read_request(&mut stream).await;
            stream.write_all(b"HTTP/1.1 307 Temporary Redirect\r\nLocation: /unexpected\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            drop(stream);
            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let error =
            RemoteSession::connect(&endpoint, "volume", &"s".repeat(32), Duration::from_secs(2))
                .await
                .unwrap_err();
        assert_eq!(error.code, ErrorCode::Eio);
        server.await.unwrap();
    }

    #[test]
    fn endpoint_and_cancellation_rules_fail_closed() {
        for url in [
            "http://example.com",
            "https://user:secret@example.com",
            "https://example.com/?token=secret",
            "https://example.com/prefix",
        ] {
            assert!(endpoint(url, "volume").is_err());
        }
        for volume in [".", "..", "../volume"] {
            assert!(endpoint("https://example.com", volume).is_err());
        }
        assert_eq!(
            endpoint("https://example.com", "drive.1").unwrap().path(),
            "/v1/drives/drive.1/sessions"
        );
        assert!(endpoint("http://127.0.0.1:1234", "volume").is_ok());
        let uncertain = AtomicBool::new(false);
        drop(PendingRequest {
            uncertain: &uncertain,
            completed: false,
        });
        assert!(uncertain.load(Ordering::Acquire));
        assert_eq!(error_code("ENOENT"), Some(ErrorCode::Enoent));
        assert_eq!(error_code("NOT_AN_ERRNO"), None);
    }
    #[tokio::test]
    async fn public_endpoint_round_trip_preserves_handles_and_filesystem_errors() {
        session_roundtrip(false).await;
    }
    #[tokio::test]
    async fn http2_round_trip_preserves_handles_and_filesystem_errors() {
        session_roundtrip(true).await;
    }
    async fn session_roundtrip(http2: bool) {
        use mount_rs_cluster::{SessionCredentials, SessionHttpHandler, VolumeSessions};
        let fs = Arc::new(
            mount_rs_sdk::Filesystem::split(mount_rs_sdk::SplitOptions::memory("remote", 4096))
                .await
                .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(fs.clone(), 2, 4, Duration::from_secs(60)).unwrap());
        let secret = "s".repeat(32);
        let handler = SessionHttpHandler::new(
            "volume",
            SessionCredentials::new(vec![("client".into(), secret.clone())]).unwrap(),
            registry.clone(),
        )
        .unwrap();
        let server = mount_rs_http::HttpServer::start_with_handler(
            Arc::new(handler),
            mount_rs_http::HttpServerOptions {
                http2_prior_knowledge: http2,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let remote = RemoteSession::connect_with_http2(
            &server.url(),
            "volume",
            &secret,
            Duration::from_secs(5),
            http2,
        )
        .await
        .unwrap();
        assert!(!format!("{remote:?}").contains(&secret));
        assert_eq!(remote.idle_timeout(), Duration::from_secs(60));
        assert_eq!(remote.diagnostics().response_headers, 0);
        let (capabilities, _) = remote.capabilities().await.unwrap();
        assert!(capabilities.handles);
        let measured = remote.diagnostics();
        assert_eq!(measured.response_headers, 1);
        assert_eq!(measured.successful_bodies, 1);
        let missing = remote.control(&serde_json::json!({"operation":"path","request":{"action":"stat","path":"/missing"}})).await.unwrap_err();
        assert_eq!(missing.code, ErrorCode::Enoent);
        let failed = remote.diagnostics();
        assert_eq!(failed.response_headers, 2);
        assert_eq!(failed.successful_bodies, 1);
        assert!(failed.response_header_nanoseconds >= measured.response_header_nanoseconds);
        assert_eq!(
            failed.successful_body_nanoseconds,
            measured.successful_body_nanoseconds
        );
        let reply = remote.control(&serde_json::json!({"operation":"open","path":"/file","mode":384,
            "flags":{"read":true,"write":true,"create":true,"truncate":true,"append":false,"exclusive":false}})).await.unwrap();
        let handle = serde_json::from_slice::<serde_json::Value>(&reply).unwrap()["handle"]
            .as_u64()
            .unwrap();
        assert_eq!(remote.write(handle, b"original", Some(0)).await.unwrap(), 8);
        remote.control(&serde_json::json!({"operation":"path","request":{"action":"rename","source":"/file","destination":"/renamed"}})).await.unwrap();
        remote.control(&serde_json::json!({"operation":"path","request":{"action":"unlink","path":"/renamed"}})).await.unwrap();
        let bytes = remote
            .control(
                &serde_json::json!({"operation":"read","handle":handle,"length":8,"position":0}),
            )
            .await
            .unwrap();
        assert_eq!(bytes, b"original");
        remote.enable_header_reads().await.unwrap();
        assert_eq!(remote.read_at_headers(handle, 3, 2).await.unwrap(), b"igi");
        let cursor = remote
            .control(&serde_json::json!({"operation":"read", "handle":handle,
            "length":2,"position":null}))
            .await
            .unwrap();
        assert_eq!(cursor, b"or");
        let invalid_get = remote
            .client
            .get(remote.handles.clone())
            .header("x-mount-session", remote.token.clone())
            .header("x-mount-handle", handle)
            .header("x-mount-length", 3);
        assert_eq!(
            remote.send(invalid_get, &[200]).await.unwrap_err().code,
            ErrorCode::Einval
        );
        let payload_get = remote
            .client
            .get(remote.handles.clone())
            .header("x-mount-session", remote.token.clone())
            .header("x-mount-handle", handle)
            .header("x-mount-length", 3)
            .header("x-mount-position", 0)
            .body("unexpected");
        assert_eq!(
            remote.send(payload_get, &[200]).await.unwrap_err().code,
            ErrorCode::Efbig
        );
        assert_eq!(remote.read_at_headers(handle, 3, 2).await.unwrap(), b"igi");
        assert_eq!(
            remote
                .write(handle, &vec![0; 65537], None)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Efbig
        );
        remote.close().await.unwrap();
        assert_eq!(
            remote
                .control(&serde_json::json!({"operation":"stat","handle":handle}))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Ebadf
        );
        server.close().await.unwrap();
        registry.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }
}
