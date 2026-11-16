use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::{Capabilities, DirEntry, FileHandle, FsDriver, FsError, Result, Stats};
use mount_rs_http::{DriveConfig, DriveRegistry, HttpServer, HttpServerError, HttpServerOptions};
use mount_rs_memfs::MemoryFs;
#[cfg(feature = "observability")]
use mount_rs_observability::{Telemetry, TelemetryConfig};
use mount_rs_sqlite_fs::open_sqlite_memory;
use reqwest::header::{
    AUTHORIZATION, CACHE_CONTROL, CONTENT_LENGTH, CONTENT_RANGE, RANGE, WWW_AUTHENTICATE,
    X_CONTENT_TYPE_OPTIONS,
};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;

const MEMORY_TOKEN: &str = "memory-secret";
const ALIAS_TOKEN: &str = "alias-secret";
const SQLITE_TOKEN: &str = "sqlite-secret";

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn regular_file_stats(size: u64) -> Stats {
    Stats {
        dev: 0,
        ino: 1,
        mode: mount_rs_core::S_IFREG | 0o644,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        size,
        blksize: 4096,
        blocks: 1,
        atime_ms: 0,
        mtime_ms: 0,
        ctime_ms: 0,
        birthtime_ms: 0,
    }
}

struct BlockingReadFs {
    closed: Arc<AtomicUsize>,
    block_close: bool,
}

struct BlockingReadHandle {
    closed: Arc<AtomicUsize>,
    block_close: bool,
}

#[async_trait]
impl FileHandle for BlockingReadHandle {
    async fn read(&self, _buffer: &mut [u8], _position: Option<u64>) -> Result<usize> {
        pending().await
    }

    async fn write(&self, _buffer: &[u8], _position: Option<u64>) -> Result<usize> {
        Err(FsError::enotsup("write"))
    }

    async fn stat(&self) -> Result<Stats> {
        Ok(regular_file_stats(1))
    }

    async fn truncate(&self, _length: u64) -> Result<()> {
        Err(FsError::enotsup("truncate"))
    }

    async fn close(&self) -> Result<()> {
        if self.block_close {
            pending::<()>().await;
        }
        self.closed.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }
}

#[async_trait]
impl FsDriver for BlockingReadFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            handles: true,
            ..Capabilities::default()
        }
    }

    async fn stat(&self, _path: &str) -> Result<Stats> {
        Ok(regular_file_stats(1))
    }

    async fn lstat(&self, _path: &str) -> Result<Stats> {
        Ok(regular_file_stats(1))
    }

    async fn readdir(&self, _path: &str) -> Result<Vec<DirEntry>> {
        Ok(Vec::new())
    }

    async fn open(&self, _path: &str, _flags: &str, _mode: u32) -> Result<Arc<dyn FileHandle>> {
        Ok(Arc::new(BlockingReadHandle {
            closed: Arc::clone(&self.closed),
            block_close: self.block_close,
        }))
    }
}

struct BlockingWriteFs {
    write_started: Arc<Notify>,
    closed: Arc<AtomicUsize>,
    close_notified: Arc<Notify>,
}

struct BlockingWriteHandle {
    write_started: Arc<Notify>,
    closed: Arc<AtomicUsize>,
    close_notified: Arc<Notify>,
}

#[async_trait]
impl FileHandle for BlockingWriteHandle {
    async fn read(&self, _buffer: &mut [u8], _position: Option<u64>) -> Result<usize> {
        Err(FsError::enotsup("read"))
    }

    async fn write(&self, _buffer: &[u8], _position: Option<u64>) -> Result<usize> {
        self.write_started.notify_waiters();
        pending().await
    }

    async fn stat(&self) -> Result<Stats> {
        Ok(regular_file_stats(0))
    }

    async fn truncate(&self, _length: u64) -> Result<()> {
        Err(FsError::enotsup("truncate"))
    }

    async fn close(&self) -> Result<()> {
        self.closed.fetch_add(1, Ordering::AcqRel);
        self.close_notified.notify_waiters();
        Ok(())
    }
}

#[async_trait]
impl FsDriver for BlockingWriteFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            handles: true,
            ..Capabilities::default()
        }
    }

    async fn stat(&self, _path: &str) -> Result<Stats> {
        Ok(regular_file_stats(0))
    }

    async fn lstat(&self, _path: &str) -> Result<Stats> {
        Ok(regular_file_stats(0))
    }

    async fn readdir(&self, _path: &str) -> Result<Vec<DirEntry>> {
        Ok(Vec::new())
    }

    async fn open(&self, _path: &str, _flags: &str, _mode: u32) -> Result<Arc<dyn FileHandle>> {
        Ok(Arc::new(BlockingWriteHandle {
            write_started: Arc::clone(&self.write_started),
            closed: Arc::clone(&self.closed),
            close_notified: Arc::clone(&self.close_notified),
        }))
    }
}

async fn test_server() -> HttpServer {
    let memory = Arc::new(MemoryFs::empty());
    let sqlite = Arc::new(open_sqlite_memory().await.expect("sqlite driver"));
    let mut registry = DriveRegistry::new();
    registry
        .register(
            DriveConfig::new("memory", memory.clone(), MEMORY_TOKEN).expect("memory drive config"),
        )
        .expect("memory drive registration");
    registry
        .register(
            DriveConfig::new("memory-alias", memory.clone(), ALIAS_TOKEN)
                .expect("shared memory drive config"),
        )
        .expect("shared memory drive registration");
    registry
        .register(
            DriveConfig::new("sqlite", sqlite.clone(), SQLITE_TOKEN).expect("sqlite drive config"),
        )
        .expect("sqlite drive registration");
    HttpServer::start(
        registry,
        HttpServerOptions {
            max_request_bytes: 1024 * 1024,
            read_chunk_bytes: 7,
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server")
}

#[cfg(feature = "observability")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observability_records_success_and_bounded_http_errors() {
    let telemetry = Telemetry::new(TelemetryConfig::enabled("mount-rs-http-test"));
    let memory = Arc::new(MemoryFs::empty());
    let mut registry = DriveRegistry::new();
    registry
        .register(DriveConfig::new("memory", memory, MEMORY_TOKEN).expect("memory drive config"))
        .expect("memory drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions::default().with_telemetry(telemetry.clone()),
    )
    .await
    .expect("HTTP server");

    let client = reqwest::Client::new();
    let base = server.url();
    let discovery = client
        .get(format!("{base}/v1/drives"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .header(
            "traceparent",
            "00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01",
        )
        .send()
        .await
        .expect("discovery response");
    assert_eq!(discovery.status(), reqwest::StatusCode::OK);

    let missing = client
        .get(format!("{base}/v1/drives/missing/file"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("missing-drive response");
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    let snapshot = telemetry.snapshot();
    assert_eq!(snapshot.operations, 2);
    assert_eq!(snapshot.successes, 1);
    assert_eq!(snapshot.errors, 1);

    server.close().await.expect("server close");
}

#[cfg(feature = "observability")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observability_records_request_timeout_as_http_error() {
    let write_started = Arc::new(Notify::new());
    let closed = Arc::new(AtomicUsize::new(0));
    let close_notified = Arc::new(Notify::new());
    let mut registry = DriveRegistry::new();
    registry
        .register(
            DriveConfig::new(
                "memory",
                Arc::new(BlockingWriteFs {
                    write_started,
                    closed: Arc::clone(&closed),
                    close_notified,
                }),
                MEMORY_TOKEN,
            )
            .expect("drive config"),
        )
        .expect("drive registration");
    let telemetry = Telemetry::new(TelemetryConfig::enabled("mount-rs-http-timeout-test"));
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            request_timeout: Duration::from_millis(50),
            ..HttpServerOptions::default()
        }
        .with_telemetry(telemetry.clone()),
    )
    .await
    .expect("HTTP server");

    let response = reqwest::Client::new()
        .put(format!("{}/v1/drives/memory/fs/slow", server.url()))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .body("payload")
        .send()
        .await
        .expect("timed-out PUT response");
    assert_eq!(response.status(), reqwest::StatusCode::GATEWAY_TIMEOUT);

    let snapshot = telemetry.snapshot();
    assert_eq!(snapshot.operations, 1);
    assert_eq!(snapshot.successes, 0);
    assert_eq!(snapshot.errors, 1);

    server.close().await.expect("server close");
    assert_eq!(closed.load(Ordering::Acquire), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn health_and_readiness_are_unauthenticated_and_bounded() {
    let server = test_server().await;
    let client = reqwest::Client::new();
    let base = server.url();

    let health = client
        .get(format!("{base}/healthz"))
        .send()
        .await
        .expect("health response");
    assert_eq!(health.status(), reqwest::StatusCode::OK);
    assert_eq!(
        health
            .headers()
            .get(CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        Some("no-store")
    );
    assert_eq!(
        health
            .headers()
            .get(X_CONTENT_TYPE_OPTIONS)
            .and_then(|value| value.to_str().ok()),
        Some("nosniff")
    );
    assert_eq!(
        health
            .json::<serde_json::Value>()
            .await
            .expect("health JSON")["status"],
        "ok"
    );

    let readiness = client
        .get(format!("{base}/readyz"))
        .send()
        .await
        .expect("readiness response");
    assert_eq!(readiness.status(), reqwest::StatusCode::OK);
    let readiness_body = readiness
        .json::<serde_json::Value>()
        .await
        .expect("readiness JSON");
    assert_eq!(readiness_body["status"], "ready");
    assert_eq!(readiness_body["drives"], 3);

    let method = client
        .post(format!("{base}/healthz"))
        .send()
        .await
        .expect("health method response");
    assert_eq!(method.status(), reqwest::StatusCode::METHOD_NOT_ALLOWED);

    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_registry_is_not_ready_but_is_healthy() {
    let server = HttpServer::start(DriveRegistry::new(), HttpServerOptions::default())
        .await
        .expect("HTTP server");
    let client = reqwest::Client::new();
    let base = server.url();

    assert_eq!(
        client
            .get(format!("{base}/healthz"))
            .send()
            .await
            .expect("health response")
            .status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .expect("readiness response")
            .status(),
        reqwest::StatusCode::SERVICE_UNAVAILABLE
    );

    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_http_exposes_isolated_memory_and_sqlite_drives() {
    let server = test_server().await;
    let client = reqwest::Client::new();
    let base = server.url();

    let unauthenticated_discovery = client
        .get(format!("{base}/v1/drives"))
        .send()
        .await
        .expect("unauthenticated discovery response");
    assert_eq!(
        unauthenticated_discovery.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let discovery = client
        .get(format!("{base}/v1/drives"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("discovery response");
    assert_eq!(discovery.status(), reqwest::StatusCode::OK);
    let discovery_body = discovery.text().await.expect("discovery body");
    let discovered_ids: Vec<String> =
        serde_json::from_str::<Vec<serde_json::Value>>(&discovery_body)
            .expect("discovery JSON")
            .into_iter()
            .map(|drive| drive["id"].as_str().expect("drive id").to_owned())
            .collect();
    assert_eq!(discovered_ids, vec!["memory"]);
    assert!(!discovery_body.contains(MEMORY_TOKEN));
    assert!(!discovery_body.contains(SQLITE_TOKEN));

    let invalid_discovery = client
        .get(format!("{base}/v1/drives"))
        .header(AUTHORIZATION, bearer("wrong-secret"))
        .send()
        .await
        .expect("invalid discovery response");
    assert_eq!(
        invalid_discovery.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let unauthorized = client
        .get(format!("{base}/v1/drives/memory/fs"))
        .send()
        .await
        .expect("unauthorized response");
    assert_eq!(unauthorized.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok()),
        Some("Bearer")
    );

    let cross_drive_token = client
        .get(format!("{base}/v1/drives/sqlite/fs"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("cross-drive authorization response");
    assert_eq!(
        cross_drive_token.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );

    let payload: Vec<u8> = (0..=255).cycle().take(128 * 1024).collect();
    let memory_file = format!("{base}/v1/drives/memory/fs/payload.bin");
    let put = client
        .put(&memory_file)
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .body(payload.clone())
        .send()
        .await
        .expect("memory PUT");
    assert_eq!(put.status(), reqwest::StatusCode::OK);
    assert_eq!(
        put.json::<serde_json::Value>().await.expect("PUT JSON")["bytes"],
        payload.len()
    );

    let get = client
        .get(&memory_file)
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("memory GET");
    assert_eq!(get.status(), reqwest::StatusCode::OK);
    assert_eq!(get.bytes().await.expect("memory body").as_ref(), payload);

    let shared_get = client
        .get(format!("{base}/v1/drives/memory-alias/fs/payload.bin"))
        .header(AUTHORIZATION, bearer(ALIAS_TOKEN))
        .send()
        .await
        .expect("shared-driver GET");
    assert_eq!(shared_get.status(), reqwest::StatusCode::OK);
    assert_eq!(
        shared_get
            .bytes()
            .await
            .expect("shared-driver body")
            .as_ref(),
        payload
    );

    let concurrent_a = client
        .put(format!("{base}/v1/drives/memory/fs/concurrent-a"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .body("a")
        .send();
    let concurrent_b = client
        .put(format!("{base}/v1/drives/memory/fs/concurrent-b"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .body("b")
        .send();
    let (concurrent_a, concurrent_b) = tokio::join!(concurrent_a, concurrent_b);
    assert_eq!(
        concurrent_a.expect("concurrent PUT A").status(),
        reqwest::StatusCode::OK
    );
    assert_eq!(
        concurrent_b.expect("concurrent PUT B").status(),
        reqwest::StatusCode::OK
    );

    let range = client
        .get(&memory_file)
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .header(RANGE, "bytes=7-23")
        .send()
        .await
        .expect("range GET");
    assert_eq!(range.status(), reqwest::StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        range
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("bytes 7-23/131072")
    );
    assert_eq!(
        range.bytes().await.expect("range body").as_ref(),
        &payload[7..24]
    );

    let head = client
        .head(&memory_file)
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("HEAD response");
    assert_eq!(head.status(), reqwest::StatusCode::OK);
    assert_eq!(
        head.headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok()),
        Some("131072")
    );
    assert!(head.bytes().await.expect("HEAD body").is_empty());

    let entries = client
        .get(format!("{base}/v1/drives/memory/entries"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("entries response");
    assert_eq!(entries.status(), reqwest::StatusCode::OK);
    assert!(
        entries
            .text()
            .await
            .expect("entries body")
            .contains("payload.bin")
    );

    let mkdir = client
        .post(format!("{base}/v1/drives/memory/ops/mkdir"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .json(&json!({"path": "/nested/dir", "recursive": true}))
        .send()
        .await
        .expect("mkdir response");
    assert_eq!(mkdir.status(), reqwest::StatusCode::OK);

    let rename = client
        .post(format!("{base}/v1/drives/memory/ops/rename"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .json(&json!({"from": "/payload.bin", "to": "/nested/dir/moved.bin"}))
        .send()
        .await
        .expect("rename response");
    assert_eq!(rename.status(), reqwest::StatusCode::OK);

    let truncate = client
        .post(format!("{base}/v1/drives/memory/ops/truncate"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .json(&json!({"path": "/nested/dir/moved.bin", "length": 9}))
        .send()
        .await
        .expect("truncate response");
    assert_eq!(truncate.status(), reqwest::StatusCode::OK);
    let truncated = client
        .get(format!("{base}/v1/drives/memory/fs/nested/dir/moved.bin"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("truncated GET");
    assert_eq!(truncated.bytes().await.expect("truncated body").len(), 9);

    let sqlite_file = format!("{base}/v1/drives/sqlite/fs/record.bin");
    let sqlite_put = client
        .put(&sqlite_file)
        .header(AUTHORIZATION, bearer(SQLITE_TOKEN))
        .body("sqlite-owned")
        .send()
        .await
        .expect("sqlite PUT");
    assert_eq!(sqlite_put.status(), reqwest::StatusCode::OK);
    let sqlite_get = client
        .get(&sqlite_file)
        .header(AUTHORIZATION, bearer(SQLITE_TOKEN))
        .send()
        .await
        .expect("sqlite GET");
    assert_eq!(
        sqlite_get.text().await.expect("sqlite body"),
        "sqlite-owned"
    );

    let isolated = client
        .get(format!("{base}/v1/drives/sqlite/fs/nested/dir/moved.bin"))
        .header(AUTHORIZATION, bearer(SQLITE_TOKEN))
        .send()
        .await
        .expect("isolated GET");
    assert_eq!(isolated.status(), reqwest::StatusCode::NOT_FOUND);

    let traversal = client
        .get(format!("{base}/v1/drives/memory/fs/%2e%2e/escape"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("traversal response");
    assert!(matches!(
        traversal.status(),
        reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::NOT_FOUND
    ));

    let json_traversal = client
        .post(format!("{base}/v1/drives/memory/ops/mkdir"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .json(&json!({"path": "/../escape", "recursive": true}))
        .send()
        .await
        .expect("JSON traversal response");
    assert_eq!(json_traversal.status(), reqwest::StatusCode::BAD_REQUEST);

    let invalid_range = client
        .get(format!("{base}/v1/drives/memory/fs/nested/dir/moved.bin"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .header(RANGE, "bytes=0-1,3-4")
        .send()
        .await
        .expect("invalid range response");
    assert_eq!(
        invalid_range.status(),
        reqwest::StatusCode::RANGE_NOT_SATISFIABLE
    );
    assert_eq!(
        invalid_range
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("bytes */9")
    );

    let unknown_drive = client
        .get(format!("{base}/v1/drives/unknown/fs"))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("unknown drive response");
    // Indistinguishable from a wrong token: drive ids are not enumerable.
    assert_eq!(unknown_drive.status(), reqwest::StatusCode::UNAUTHORIZED);

    let deleted = client
        .delete(format!("{base}/v1/drives/sqlite/fs/record.bin"))
        .header(AUTHORIZATION, bearer(SQLITE_TOKEN))
        .send()
        .await
        .expect("delete response");
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    server.close().await.expect("server close");
}

/// A client that sends its whole body before reading the reply, as Node's
/// `fetch` and Python's `http.client` do, must get the refusal. Closing as
/// soon as the reply is written resets the connection under the client's
/// write, which then fails with EPIPE and never reads the 413 or 401.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_upload_is_answered_after_its_body_is_sent() {
    let memory = Arc::new(MemoryFs::empty());
    let mut registry = DriveRegistry::new();
    registry
        .register(DriveConfig::new("memory", memory, MEMORY_TOKEN).expect("drive config"))
        .expect("drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            max_request_bytes: 1024,
            max_upload_bytes: 1024,
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");
    const BODY: usize = 32 << 20;
    for (token, expected) in [
        (MEMORY_TOKEN, "HTTP/1.1 413"),
        ("not-the-token", "HTTP/1.1 401"),
    ] {
        let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
            .await
            .expect("upload socket");
        let head = format!(
            "PUT /v1/drives/memory/fs/big HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nContent-Length: {BODY}\r\nConnection: close\r\n\r\n",
            bearer(token)
        );
        socket
            .write_all(head.as_bytes())
            .await
            .expect("request head");
        socket
            .write_all(&vec![0_u8; BODY])
            .await
            .unwrap_or_else(|error| panic!("{expected}: the body write failed: {error}"));
        let mut response = Vec::new();
        socket
            .read_to_end(&mut response)
            .await
            .expect("refusal response");
        let response = String::from_utf8_lossy(&response);
        assert!(response.starts_with(expected), "{expected}: {response:.80}");
    }
}

/// Answers at once without reading the request body, as a router or
/// forwarder that refuses a request might.
struct AnswersWithoutReading(AtomicUsize);

impl mount_rs_http::HttpRequestHandler for AnswersWithoutReading {
    fn handle(
        &self,
        _request: hyper::Request<mount_rs_http::HttpBody>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = hyper::Response<mount_rs_http::HttpBody>> + Send + '_>,
    > {
        use http_body_util::BodyExt;
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {
            hyper::Response::new(
                http_body_util::Full::new(bytes::Bytes::new())
                    .map_err(|never| match never {})
                    .boxed_unsync(),
            )
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handler_server_answers_an_oversized_upload_after_its_body_is_sent() {
    let handler = Arc::new(AnswersWithoutReading(AtomicUsize::new(0)));
    let server = HttpServer::start_with_handler(
        handler.clone(),
        HttpServerOptions {
            max_request_bytes: 1024,
            max_upload_bytes: 1024,
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");
    const BODY: usize = 32 << 20;
    let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .expect("upload socket");
    let head = format!(
        "PUT /v1/drives/remote/fs/big HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nContent-Length: {BODY}\r\nConnection: close\r\n\r\n",
        bearer(MEMORY_TOKEN)
    );
    socket
        .write_all(head.as_bytes())
        .await
        .expect("request head");
    socket
        .write_all(&vec![0_u8; BODY])
        .await
        .unwrap_or_else(|error| panic!("the body write failed: {error}"));
    let mut response = Vec::new();
    socket
        .read_to_end(&mut response)
        .await
        .expect("refusal response");
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 413"), "{response:.80}");
    assert_eq!(handler.0.load(Ordering::SeqCst), 0, "the handler saw it");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_body_limit_is_enforced_without_unbounded_buffering() {
    let memory = Arc::new(MemoryFs::empty());
    let mut registry = DriveRegistry::new();
    registry
        .register(DriveConfig::new("memory", memory, MEMORY_TOKEN).expect("drive config"))
        .expect("drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            max_request_bytes: 32,
            max_upload_bytes: 32,
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");
    let response = reqwest::Client::new()
        .put(format!("{}/v1/drives/memory/fs/too-large", server.url()))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .body(vec![0_u8; 33])
        .send()
        .await
        .expect("limited PUT");
    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response
            .headers()
            .get("connection")
            .and_then(|value| value.to_str().ok()),
        Some("close")
    );

    let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .expect("chunked PUT socket");
    let overflow_chunk = "x".repeat(32);
    let raw_request = format!(
        "PUT /v1/drives/memory/fs/chunked-overflow HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nhello\r\n20\r\n{}\r\n0\r\n\r\n",
        bearer(MEMORY_TOKEN),
        overflow_chunk
    );
    socket
        .write_all(raw_request.as_bytes())
        .await
        .expect("chunked PUT request");
    let mut raw_response = Vec::new();
    socket
        .read_to_end(&mut raw_response)
        .await
        .expect("chunked PUT response");
    let raw_response = String::from_utf8_lossy(&raw_response);
    assert!(raw_response.starts_with("HTTP/1.1 413"));
    assert!(raw_response.contains("\"code\":\"EFBIG\""));

    let partial = reqwest::Client::new()
        .get(format!(
            "{}/v1/drives/memory/fs/chunked-overflow",
            server.url()
        ))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("partial PUT inspection");
    // A rejected upload publishes nothing, not its accepted prefix.
    assert_eq!(partial.status(), reqwest::StatusCode::NOT_FOUND);
    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stalled_request_body_is_terminated_by_the_request_timeout() {
    let mut registry = DriveRegistry::new();
    registry
        .register(
            DriveConfig::new("memory", Arc::new(MemoryFs::empty()), MEMORY_TOKEN)
                .expect("drive config"),
        )
        .expect("drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            request_timeout: Duration::from_millis(50),
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");

    let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .expect("stalled PUT socket");
    socket
        .write_all(
            format!(
                "PUT /v1/drives/memory/fs/stalled HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nx",
                bearer(MEMORY_TOKEN)
            )
            .as_bytes(),
        )
        .await
        .expect("partial PUT request");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), socket.read_to_end(&mut response))
        .await
        .expect("stalled PUT timeout response")
        .expect("stalled PUT response read");
    let response = String::from_utf8_lossy(&response);
    assert!(response.starts_with("HTTP/1.1 504"), "response: {response}");
    assert!(response.contains("\"code\":\"ETIMEDOUT\""));
    assert!(response.contains("connection: close"));

    server.close().await.expect("server close");
}

/// A server whose uploads stream past `max_request_bytes`, over a memory
/// drive (atomic `write_file`) and a SQLite drive (written through a sibling).
async fn streaming_server(
    request_timeout: Duration,
    max_upload_bytes: u64,
) -> (HttpServer, Arc<dyn FsDriver>, Arc<dyn FsDriver>) {
    let memory: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let sqlite: Arc<dyn FsDriver> = Arc::new(open_sqlite_memory().await.expect("sqlite driver"));
    let mut registry = DriveRegistry::new();
    for (id, driver) in [("memory", &memory), ("sqlite", &sqlite)] {
        registry
            .register(DriveConfig::new(id, driver.clone(), MEMORY_TOKEN).expect("drive config"))
            .expect("drive registration");
    }
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            max_request_bytes: 1024,
            max_upload_bytes,
            request_timeout,
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");
    (server, memory, sqlite)
}

/// The names in a drive's root, the server's own temporary names included.
async fn root_names(driver: &Arc<dyn FsDriver>) -> Vec<String> {
    let mut names: Vec<String> = driver
        .readdir("/")
        .await
        .expect("root listing")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort();
    names
}

async fn read_all(driver: &Arc<dyn FsDriver>, path: &str) -> Vec<u8> {
    let file = driver.open(path, "r", 0).await.expect("open for read");
    let mut out = Vec::new();
    let mut buffer = vec![0_u8; 65536];
    loop {
        let count = file
            .read(&mut buffer, Some(out.len() as u64))
            .await
            .expect("read");
        if count == 0 {
            break;
        }
        out.extend_from_slice(&buffer[..count]);
    }
    file.close().await.expect("close");
    out
}

/// Send a raw request and read the whole reply.
async fn raw_exchange(port: u16, head: &str, body: &[u8]) -> String {
    let mut socket = TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("upload socket");
    socket
        .write_all(head.as_bytes())
        .await
        .expect("request head");
    socket.write_all(body).await.expect("request body");
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), socket.read_to_end(&mut response))
        .await
        .expect("response in time")
        .expect("response read");
    String::from_utf8_lossy(&response).into_owned()
}

fn chunked(body: &[u8], piece: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for part in body.chunks(piece) {
        out.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes());
        out.extend_from_slice(part);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"0\r\n\r\n");
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uploads_larger_than_the_request_limit_stream_to_storage() {
    let (server, memory, sqlite) = streaming_server(Duration::from_secs(10), 1 << 20).await;
    let client = reqwest::Client::new();
    for (drive, driver) in [("memory", &memory), ("sqlite", &sqlite)] {
        // Declared length above the request limit, then an overwrite.
        for size in [200_000_usize, 300_001] {
            let body: Vec<u8> = (0..size).map(|index| (index * 31 % 251) as u8).collect();
            let response = client
                .put(format!("{}/v1/drives/{drive}/fs/big.bin", server.url()))
                .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
                .body(body.clone())
                .send()
                .await
                .expect("streamed PUT");
            assert_eq!(response.status(), reqwest::StatusCode::OK, "{drive} {size}");
            let reply: serde_json::Value = response.json().await.expect("PUT reply");
            assert_eq!(reply["bytes"], size, "{drive}");
            let read = client
                .get(format!("{}/v1/drives/{drive}/fs/big.bin", server.url()))
                .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
                .send()
                .await
                .expect("GET")
                .bytes()
                .await
                .expect("GET body");
            assert!(read.as_ref() == body.as_slice(), "{drive} {size} content");
        }
        // No declared length: received in memory up to the request limit,
        // then spilled to storage.
        let body: Vec<u8> = (0..70_000).map(|index| (index % 253) as u8).collect();
        let head = format!(
            "PUT /v1/drives/{drive}/fs/chunked.bin HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            bearer(MEMORY_TOKEN)
        );
        let response = raw_exchange(server.port(), &head, &chunked(&body, 4000)).await;
        assert!(
            response.starts_with("HTTP/1.1 200"),
            "{drive}: {response:.120}"
        );
        let read = client
            .get(format!("{}/v1/drives/{drive}/fs/chunked.bin", server.url()))
            .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
            .send()
            .await
            .expect("GET")
            .bytes()
            .await
            .expect("GET body");
        assert!(read.as_ref() == body.as_slice(), "{drive} chunked content");
        assert_eq!(
            root_names(driver).await,
            ["big.bin", "chunked.bin"],
            "{drive} left a temporary file"
        );
    }
    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_over_the_upload_limit_leaves_the_target_unchanged() {
    let (server, memory, sqlite) = streaming_server(Duration::from_secs(10), 64 * 1024).await;
    let client = reqwest::Client::new();
    for (drive, driver) in [("memory", &memory), ("sqlite", &sqlite)] {
        driver.write_file("/keep", b"original").await.expect("seed");
        // Declared too large: refused before anything is written.
        let response = client
            .put(format!("{}/v1/drives/{drive}/fs/keep", server.url()))
            .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
            .body(vec![7_u8; 64 * 1024 + 1])
            .send()
            .await
            .expect("oversized PUT");
        assert_eq!(
            response.status(),
            reqwest::StatusCode::PAYLOAD_TOO_LARGE,
            "{drive}"
        );
        // No declared length: refused once it grows past the limit, after
        // part of it was written to a temporary file.
        let head = format!(
            "PUT /v1/drives/{drive}/fs/keep HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            bearer(MEMORY_TOKEN)
        );
        let response =
            raw_exchange(server.port(), &head, &chunked(&vec![9_u8; 100_000], 8192)).await;
        assert!(
            response.starts_with("HTTP/1.1 413"),
            "{drive}: {response:.120}"
        );
        let mut names = Vec::new();
        for _ in 0..100 {
            names = root_names(driver).await;
            if names == ["keep"] {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(names, ["keep"], "{drive} left a temporary file");
        assert_eq!(read_all(driver, "/keep").await, b"original", "{drive}");
    }
    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_streamed_upload_may_outlast_the_request_timeout_while_it_progresses() {
    let (server, memory, _) = streaming_server(Duration::from_millis(300), 1 << 20).await;
    const PIECES: usize = 12;
    const PIECE: usize = 4096;
    let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .expect("upload socket");
    socket
        .write_all(
            format!(
                "PUT /v1/drives/memory/fs/slow.bin HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                bearer(MEMORY_TOKEN),
                PIECES * PIECE
            )
            .as_bytes(),
        )
        .await
        .expect("request head");
    let started = std::time::Instant::now();
    for index in 0..PIECES {
        tokio::time::sleep(Duration::from_millis(100)).await;
        socket
            .write_all(&[index as u8; PIECE])
            .await
            .expect("request body piece");
    }
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.expect("response");
    let response = String::from_utf8_lossy(&response);
    assert!(started.elapsed() > Duration::from_millis(1000));
    assert!(response.starts_with("HTTP/1.1 200"), "{response:.120}");
    let stored = read_all(&memory, "/slow.bin").await;
    assert_eq!(stored.len(), PIECES * PIECE);
    assert!(
        stored
            .chunks(PIECE)
            .enumerate()
            .all(|(index, piece)| piece.iter().all(|byte| *byte == index as u8))
    );
    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_streamed_upload_times_out_and_leaves_nothing() {
    let (server, memory, sqlite) = streaming_server(Duration::from_millis(200), 1 << 20).await;
    for (drive, driver) in [("memory", &memory), ("sqlite", &sqlite)] {
        let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
            .await
            .expect("upload socket");
        socket
            .write_all(
                format!(
                    "PUT /v1/drives/{drive}/fs/stalled HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nContent-Length: 100000\r\nConnection: close\r\n\r\n",
                    bearer(MEMORY_TOKEN)
                )
                .as_bytes(),
            )
            .await
            .expect("request head");
        socket
            .write_all(&[1_u8; 20_000])
            .await
            .expect("part of the body");
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), socket.read_to_end(&mut response))
            .await
            .expect("stalled upload answered")
            .expect("response read");
        let response = String::from_utf8_lossy(&response);
        assert!(
            response.starts_with("HTTP/1.1 504"),
            "{drive}: {response:.120}"
        );
        assert!(response.contains("\"code\":\"ETIMEDOUT\""), "{drive}");
        let mut names = vec![String::from("unchecked")];
        for _ in 0..100 {
            names = root_names(driver).await;
            if names.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(names.is_empty(), "{drive} left {names:?}");
    }
    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directory_entry_limit_bounds_both_listing_routes() {
    let mut registry = DriveRegistry::new();
    registry
        .register(
            DriveConfig::new("memory", Arc::new(MemoryFs::empty()), MEMORY_TOKEN)
                .expect("drive config"),
        )
        .expect("drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            max_directory_entries: 2,
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");
    let client = reqwest::Client::new();
    for name in ["one", "two", "three"] {
        let response = client
            .put(format!("{}/v1/drives/memory/fs/{name}", server.url()))
            .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
            .body(name.to_owned())
            .send()
            .await
            .expect("file creation response");
        assert_eq!(response.status(), reqwest::StatusCode::OK);
    }

    for path in ["entries/", "fs"] {
        let response = client
            .get(format!("{}/v1/drives/memory/{path}", server.url()))
            .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
            .send()
            .await
            .expect("directory listing response");
        assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response
                .headers()
                .get("connection")
                .and_then(|value| value.to_str().ok()),
            Some("close")
        );
    }

    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn default_limits_list_a_directory_of_a_hundred_thousand_files() {
    const FILES: usize = 100_000;
    let fs = Arc::new(MemoryFs::empty());
    fs.mkdir("/big", mount_rs_core::MkdirOptions::default())
        .await
        .expect("directory");
    for index in 0..FILES {
        fs.write_file(&format!("/big/file-{index:07}.bin"), b"x")
            .await
            .expect("file");
    }
    let mut registry = DriveRegistry::new();
    registry
        .register(DriveConfig::new("memory", fs, MEMORY_TOKEN).expect("drive config"))
        .expect("drive registration");
    let server = HttpServer::start(registry, HttpServerOptions::default())
        .await
        .expect("HTTP server");
    let client = reqwest::Client::new();
    for path in ["entries/big", "fs/big"] {
        let response = client
            .get(format!("{}/v1/drives/memory/{path}", server.url()))
            .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
            .send()
            .await
            .expect("directory listing response");
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{path}");
        let body: serde_json::Value = response.json().await.expect("listing JSON");
        let entries = body
            .as_array()
            .or_else(|| body["entries"].as_array())
            .expect("entries");
        assert_eq!(entries.len(), FILES, "{path}");
    }
    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn directory_response_byte_limit_bounds_serialized_metadata() {
    let mut registry = DriveRegistry::new();
    registry
        .register(
            DriveConfig::new("memory", Arc::new(MemoryFs::empty()), MEMORY_TOKEN)
                .expect("drive config"),
        )
        .expect("drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            max_response_bytes: 32,
            max_directory_entries: 100,
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");
    let client = reqwest::Client::new();
    let response = client
        .put(format!(
            "{}/v1/drives/memory/fs/a-long-enough-file-name",
            server.url()
        ))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .body("content")
        .send()
        .await
        .expect("file creation response");
    assert_eq!(response.status(), reqwest::StatusCode::OK);

    let response = client
        .get(format!("{}/v1/drives/memory/entries/", server.url()))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("directory listing response");
    assert_eq!(response.status(), reqwest::StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        response
            .headers()
            .get("connection")
            .and_then(|value| value.to_str().ok()),
        Some("close")
    );

    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn connection_limit_rejects_excess_idle_connections() {
    let mut registry = DriveRegistry::new();
    registry
        .register(
            DriveConfig::new("memory", Arc::new(MemoryFs::empty()), MEMORY_TOKEN)
                .expect("drive config"),
        )
        .expect("drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            max_connections: 1,
            request_timeout: Duration::from_secs(5),
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");

    let mut first = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .expect("first connection");
    first
        .write_all(
            format!(
                "PUT /v1/drives/memory/fs/held HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nContent-Length: 2\r\nConnection: close\r\n\r\nx",
                bearer(MEMORY_TOKEN)
            )
            .as_bytes(),
        )
        .await
        .expect("held request");
    tokio::time::timeout(Duration::from_secs(1), async {
        while server.connections() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first connection did not become active");

    let mut second = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .expect("second connection");
    second
        .write_all(
            format!(
                "GET /v1/drives HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nConnection: close\r\n\r\n",
                bearer(MEMORY_TOKEN)
            )
            .as_bytes(),
        )
        .await
        .expect("second request");
    let mut response = Vec::new();
    let read_result =
        tokio::time::timeout(Duration::from_secs(1), second.read_to_end(&mut response))
            .await
            .expect("excess connection was not closed");
    assert!(
        read_result.is_err() || response.is_empty(),
        "response: {response:?}"
    );

    drop(first);
    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn immediate_close_is_race_free_and_idempotent() {
    for _ in 0..32 {
        let mut registry = DriveRegistry::new();
        registry
            .register(
                DriveConfig::new("memory", Arc::new(MemoryFs::empty()), MEMORY_TOKEN)
                    .expect("drive config"),
            )
            .expect("drive registration");
        let server = HttpServer::start(
            registry,
            HttpServerOptions {
                drain_timeout: Duration::from_millis(100),
                ..HttpServerOptions::default()
            },
        )
        .await
        .expect("HTTP server");
        tokio::time::timeout(Duration::from_secs(1), server.close())
            .await
            .expect("first close timeout")
            .expect("first close");
        server.close().await.expect("repeat close");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_cancels_and_drains_a_blocked_stream_producer() {
    let closed = Arc::new(AtomicUsize::new(0));
    let mut registry = DriveRegistry::new();
    registry
        .register(
            DriveConfig::new(
                "blocked-read",
                Arc::new(BlockingReadFs {
                    closed: Arc::clone(&closed),
                    block_close: false,
                }),
                MEMORY_TOKEN,
            )
            .expect("drive config"),
        )
        .expect("drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            drain_timeout: Duration::from_secs(1),
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");
    let response = reqwest::Client::new()
        .get(format!("{}/v1/drives/blocked-read/fs/file", server.url()))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("blocked stream response");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    drop(response);
    tokio::time::timeout(Duration::from_secs(2), server.close())
        .await
        .expect("blocked stream close timeout")
        .expect("blocked stream close");
    assert_eq!(closed.load(Ordering::Acquire), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn timed_out_close_retains_state_for_a_repeat_attempt() {
    let mut registry = DriveRegistry::new();
    registry
        .register(
            DriveConfig::new(
                "blocked-close",
                Arc::new(BlockingReadFs {
                    closed: Arc::new(AtomicUsize::new(0)),
                    block_close: true,
                }),
                MEMORY_TOKEN,
            )
            .expect("drive config"),
        )
        .expect("drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            drain_timeout: Duration::from_millis(50),
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");
    let response = reqwest::Client::new()
        .get(format!("{}/v1/drives/blocked-close/fs/file", server.url()))
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .send()
        .await
        .expect("blocked close response");
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    drop(response);

    assert!(matches!(
        server.close().await,
        Err(HttpServerError::Timeout)
    ));
    assert!(matches!(
        server.close().await,
        Err(HttpServerError::Timeout)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_cleans_up_a_blocked_put_handle() {
    let write_started = Arc::new(Notify::new());
    let closed = Arc::new(AtomicUsize::new(0));
    let close_notified = Arc::new(Notify::new());
    let mut registry = DriveRegistry::new();
    registry
        .register(
            DriveConfig::new(
                "blocked-write",
                Arc::new(BlockingWriteFs {
                    write_started: Arc::clone(&write_started),
                    closed: Arc::clone(&closed),
                    close_notified: Arc::clone(&close_notified),
                }),
                MEMORY_TOKEN,
            )
            .expect("drive config"),
        )
        .expect("drive registration");
    let server = HttpServer::start(
        registry,
        HttpServerOptions {
            drain_timeout: Duration::from_secs(1),
            ..HttpServerOptions::default()
        },
    )
    .await
    .expect("HTTP server");

    let started = write_started.notified();
    tokio::pin!(started);
    started.as_mut().enable();
    let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .expect("blocked PUT socket");
    socket
        .write_all(
            format!(
                "PUT /v1/drives/blocked-write/fs/file HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nContent-Length: 1\r\n\r\nx",
                bearer(MEMORY_TOKEN)
            )
            .as_bytes(),
        )
        .await
        .expect("blocked PUT request");
    tokio::time::timeout(Duration::from_secs(1), &mut started)
        .await
        .expect("blocked write did not start");

    let closed_wait = close_notified.notified();
    tokio::pin!(closed_wait);
    closed_wait.as_mut().enable();
    drop(socket);
    if closed.load(Ordering::Acquire) == 0 {
        tokio::time::timeout(Duration::from_secs(2), &mut closed_wait)
            .await
            .expect("blocked PUT cleanup timeout");
    }
    assert_eq!(closed.load(Ordering::Acquire), 1);
    server.close().await.expect("server close");
}

#[tokio::test]
async fn typed_metadata_reads_preserve_symlink_semantics_and_authentication() {
    let memory = Arc::new(MemoryFs::empty());
    let file = memory.open("/data", "w", 0o640).await.unwrap();
    file.write(b"contents", Some(0)).await.unwrap();
    file.close().await.unwrap();
    memory.symlink("/data", "/link").await.unwrap();
    let mut registry = DriveRegistry::new();
    registry
        .register(DriveConfig::new("memory", memory, MEMORY_TOKEN).unwrap())
        .unwrap();
    let server = HttpServer::start(registry, HttpServerOptions::default())
        .await
        .unwrap();
    let client = reqwest::Client::new();
    for operation in ["stat", "lstat", "statfs", "readlink"] {
        let url = format!("{}/v1/drives/memory/{operation}/link", server.url());
        assert_eq!(client.get(&url).send().await.unwrap().status(), 401);
        let response = client
            .get(&url)
            .bearer_auth(MEMORY_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let value: serde_json::Value = response.json().await.unwrap();
        match operation {
            "stat" => {
                let stats: Stats = serde_json::from_value(value).unwrap();
                assert!(stats.is_file());
                assert_eq!(stats.size, 8);
            }
            "lstat" => {
                let stats: Stats = serde_json::from_value(value).unwrap();
                assert!(stats.is_symbolic_link());
            }
            "readlink" => assert_eq!(value, "/data"),
            "statfs" => {
                let _: mount_rs_core::StatsFs = serde_json::from_value(value).unwrap();
            }
            _ => unreachable!(),
        }
        let head = client
            .head(&url)
            .bearer_auth(MEMORY_TOKEN)
            .send()
            .await
            .unwrap();
        assert_eq!(head.status(), 200);
        assert!(head.bytes().await.unwrap().is_empty());
        assert_eq!(
            client
                .post(&url)
                .bearer_auth(MEMORY_TOKEN)
                .send()
                .await
                .unwrap()
                .status(),
            405
        );
    }
    let missing = client
        .get(format!("{}/v1/drives/memory/stat/missing", server.url()))
        .bearer_auth(MEMORY_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    assert_eq!(
        missing.json::<serde_json::Value>().await.unwrap()["error"]["code"],
        "ENOENT"
    );
    server.close().await.unwrap();
}

struct ReplaceBeforeOpenFs {
    inner: MemoryFs,
    replaced: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl FsDriver for ReplaceBeforeOpenFs {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    async fn stat(&self, path: &str) -> Result<Stats> {
        self.inner.stat(path).await
    }
    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.inner.lstat(path).await
    }
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.inner.readdir(path).await
    }
    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        if !self.replaced.swap(true, Ordering::SeqCst) {
            self.inner.rename(path, "/old").await?;
            let new = self.inner.open(path, "w", 0o600).await?;
            new.write(b"new", Some(0)).await?;
            new.close().await?;
        }
        self.inner.open(path, flags, mode).await
    }
}

#[tokio::test]
async fn get_headers_and_ranges_use_open_inode_after_path_replacement() {
    for ranged in [false, true] {
        let inner = MemoryFs::empty();
        let file = inner.open("/file", "w", 0o600).await.unwrap();
        file.write(b"old longer contents", Some(0)).await.unwrap();
        file.close().await.unwrap();
        let mut registry = DriveRegistry::new();
        registry
            .register(
                DriveConfig::new(
                    "race",
                    Arc::new(ReplaceBeforeOpenFs {
                        inner,
                        replaced: std::sync::atomic::AtomicBool::new(false),
                    }),
                    MEMORY_TOKEN,
                )
                .unwrap(),
            )
            .unwrap();
        let server = HttpServer::start(registry, HttpServerOptions::default())
            .await
            .unwrap();
        let mut request = reqwest::Client::new()
            .get(format!("{}/v1/drives/race/fs/file", server.url()))
            .bearer_auth(MEMORY_TOKEN)
            .timeout(Duration::from_secs(3));
        if ranged {
            request = request.header(RANGE, "bytes=0-4095");
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status().as_u16(), if ranged { 206 } else { 200 });
        assert_eq!(response.headers()[CONTENT_LENGTH], "3");
        if ranged {
            assert_eq!(response.headers()[CONTENT_RANGE], "bytes 0-2/3");
        }
        assert_eq!(response.bytes().await.unwrap().as_ref(), b"new");
        server.close().await.unwrap();
    }
}

#[tokio::test]
async fn rejected_range_closes_the_handle_opened_for_representation_metadata() {
    // Built outside the deadline: in a workspace-wide build feature
    // unification gives reqwest the native root store, and loading the
    // system keychain has been measured at 0.4-5.2 s on macOS.
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        for range in ["bytes=2-3", "not-a-range"] {
            let closed = Arc::new(AtomicUsize::new(0));
            let mut registry = DriveRegistry::new();
            registry
                .register(
                    DriveConfig::new(
                        "range",
                        Arc::new(BlockingReadFs {
                            closed: closed.clone(),
                            block_close: false,
                        }),
                        MEMORY_TOKEN,
                    )
                    .unwrap(),
                )
                .unwrap();
            let server = HttpServer::start(registry, HttpServerOptions::default())
                .await
                .unwrap();
            let response = client
                .get(format!("{}/v1/drives/range/fs/file", server.url()))
                .bearer_auth(MEMORY_TOKEN)
                .header(RANGE, range)
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 416);
            response.bytes().await.unwrap();
            server.close().await.unwrap();
            assert_eq!(closed.load(Ordering::SeqCst), 1);
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn negotiated_listing_head_preserves_encoded_length_over_http() {
    use std::io::Read;
    let server = test_server().await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/drives/memory/entries", server.url());
    let get = client
        .get(&url)
        .bearer_auth(MEMORY_TOKEN)
        .header("Accept-Encoding", "gzip, identity;q=0")
        .send()
        .await
        .unwrap();
    assert_eq!(get.status(), reqwest::StatusCode::OK);
    assert_eq!(get.headers()["content-encoding"], "gzip");
    assert_eq!(get.headers()["vary"], "Accept-Encoding");
    let length = get.headers()[CONTENT_LENGTH].clone();
    let bytes = get.bytes().await.unwrap();
    assert_eq!(
        length.to_str().unwrap().parse::<usize>().unwrap(),
        bytes.len()
    );
    let mut decoded = Vec::new();
    flate2::read::GzDecoder::new(bytes.as_ref())
        .read_to_end(&mut decoded)
        .unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&decoded).unwrap(),
        json!([])
    );
    let head = client
        .head(&url)
        .bearer_auth(MEMORY_TOKEN)
        .header("Accept-Encoding", "gzip, identity;q=0")
        .send()
        .await
        .unwrap();
    assert_eq!(head.headers()[CONTENT_LENGTH], length);
    assert_eq!(head.headers()["content-encoding"], "gzip");
    assert!(head.bytes().await.unwrap().is_empty());
    let rejected = client
        .get(&url)
        .bearer_auth(MEMORY_TOKEN)
        .header("Accept-Encoding", "*;q=0")
        .send()
        .await
        .unwrap();
    assert_eq!(rejected.status(), reqwest::StatusCode::NOT_ACCEPTABLE);
    server.close().await.unwrap();
}

/// A split-store drive, whose files have content tags, and a memory drive,
/// whose files fall back to metadata tags.
async fn tagged_server() -> (HttpServer, Arc<dyn FsDriver>) {
    let split: Arc<dyn FsDriver> = Arc::new(
        mount_rs_chunked::ChunkedFs::open(
            mount_rs_sqlite::SqliteMetadataStore::in_memory().expect("metadata store"),
            mount_rs_sqlite::SqliteBlockStore::in_memory().expect("block store"),
            mount_rs_chunked::ChunkedOptions::fixed("http-etag", 4096).expect("options"),
        )
        .await
        .expect("split store"),
    );
    let memory: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let mut registry = DriveRegistry::new();
    for (id, driver) in [("split", &split), ("memory", &memory)] {
        registry
            .register(DriveConfig::new(id, driver.clone(), MEMORY_TOKEN).expect("drive config"))
            .expect("drive registration");
    }
    let server = HttpServer::start(registry, HttpServerOptions::default())
        .await
        .expect("HTTP server");
    (server, split)
}

fn etag_of(response: &reqwest::Response) -> String {
    response
        .headers()
        .get(reqwest::header::ETAG)
        .expect("ETag header")
        .to_str()
        .expect("ASCII ETag")
        .to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn etags_change_with_content_and_answer_conditional_requests() {
    let (server, _split) = tagged_server().await;
    let client = reqwest::Client::new();
    for drive in ["split", "memory"] {
        let url = format!("{}/v1/drives/{drive}/fs/file.txt", server.url());
        let put = |body: &'static str, condition: Option<(&'static str, String)>| {
            let mut request = client
                .put(&url)
                .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
                .body(body);
            if let Some((name, value)) = condition {
                request = request.header(name, value);
            }
            request.send()
        };
        let get = |condition: Option<(&'static str, String)>| {
            let mut request = client.get(&url).header(AUTHORIZATION, bearer(MEMORY_TOKEN));
            if let Some((name, value)) = condition {
                request = request.header(name, value);
            }
            request.send()
        };

        // Only create: the path is free.
        let created = put("first", Some(("if-none-match", "*".into())))
            .await
            .expect("create");
        assert_eq!(created.status(), reqwest::StatusCode::OK, "{drive}");
        let first = etag_of(&created);
        assert!(
            first.starts_with('"') && first.ends_with('"'),
            "{drive} {first}"
        );
        let refused = put("again", Some(("if-none-match", "*".into())))
            .await
            .expect("create again");
        assert_eq!(
            refused.status(),
            reqwest::StatusCode::PRECONDITION_FAILED,
            "{drive}"
        );

        let read = get(None).await.expect("GET");
        assert_eq!(etag_of(&read), first, "{drive}");
        assert_eq!(read.text().await.expect("body"), "first");
        let head = client
            .head(&url)
            .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
            .send()
            .await
            .expect("HEAD");
        assert_eq!(etag_of(&head), first, "{drive}");
        for tag in [
            first.clone(),
            format!("W/{first}"),
            format!("\"x\", {first}"),
        ] {
            let cached = get(Some(("if-none-match", tag))).await.expect("revalidate");
            assert_eq!(
                cached.status(),
                reqwest::StatusCode::NOT_MODIFIED,
                "{drive}"
            );
            assert_eq!(etag_of(&cached), first, "{drive}");
            assert!(cached.bytes().await.expect("304 body").is_empty());
        }

        if drive == "memory" {
            // Metadata tags change with the modification time, which has a
            // resolution of a millisecond.
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Same size, new bytes: the split store's tag changes even within
        // the same millisecond.
        let second = put("other", None).await.expect("overwrite");
        let second = etag_of(&second);
        assert_ne!(second, first, "{drive}");
        assert_eq!(
            get(Some(("if-none-match", first.clone())))
                .await
                .expect("stale revalidation")
                .status(),
            reqwest::StatusCode::OK,
            "{drive}"
        );

        // A write based on a stale tag is refused and changes nothing.
        let stale = put("lost!", Some(("if-match", first.clone())))
            .await
            .expect("stale write");
        assert_eq!(
            stale.status(),
            reqwest::StatusCode::PRECONDITION_FAILED,
            "{drive}"
        );
        let weak = put("lost!", Some(("if-match", format!("W/{second}"))))
            .await
            .expect("weak write");
        assert_eq!(
            weak.status(),
            reqwest::StatusCode::PRECONDITION_FAILED,
            "{drive}"
        );
        assert_eq!(get(None).await.unwrap().text().await.unwrap(), "other");
        let current = put("third", Some(("if-match", second.clone())))
            .await
            .expect("current write");
        assert_eq!(current.status(), reqwest::StatusCode::OK, "{drive}");
        let third = etag_of(&current);

        // A range is honoured only while If-Range names the current tag.
        let ranged = |tag: String| {
            client
                .get(&url)
                .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
                .header(RANGE, "bytes=0-1")
                .header("if-range", tag)
                .send()
        };
        let partial = ranged(third.clone()).await.expect("If-Range current");
        assert_eq!(
            partial.status(),
            reqwest::StatusCode::PARTIAL_CONTENT,
            "{drive}"
        );
        assert_eq!(partial.text().await.unwrap(), "th");
        let whole = ranged(second.clone()).await.expect("If-Range stale");
        assert_eq!(whole.status(), reqwest::StatusCode::OK, "{drive}");
        assert_eq!(whole.text().await.unwrap(), "third");
        assert_eq!(
            get(Some(("if-match", second.clone())))
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::PRECONDITION_FAILED,
            "{drive}"
        );

        // Deletes too.
        let delete = |tag: String| {
            client
                .delete(&url)
                .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
                .header("if-match", tag)
                .send()
        };
        assert_eq!(
            delete(second).await.unwrap().status(),
            reqwest::StatusCode::PRECONDITION_FAILED,
            "{drive}"
        );
        assert_eq!(
            delete(third.clone()).await.unwrap().status(),
            reqwest::StatusCode::NO_CONTENT,
            "{drive}"
        );
        assert_eq!(
            put("gone", Some(("if-match", "*".into())))
                .await
                .unwrap()
                .status(),
            reqwest::StatusCode::PRECONDITION_FAILED,
            "{drive}"
        );
    }
    server.close().await.expect("server close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_writes_conditioned_on_one_tag_have_one_winner() {
    let (server, split) = tagged_server().await;
    let client = reqwest::Client::new();
    let url = format!("{}/v1/drives/split/fs/race.txt", server.url());
    let base = client
        .put(&url)
        .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
        .body("base")
        .send()
        .await
        .expect("base write");
    let base = etag_of(&base);
    let writers = (0..16).map(|writer| {
        let request = client
            .put(&url)
            .header(AUTHORIZATION, bearer(MEMORY_TOKEN))
            .header("if-match", base.clone())
            .body(format!("writer {writer:02}"));
        async move { (writer, request.send().await.expect("racing write").status()) }
    });
    let results = spawn_all(writers).await;
    let winners: Vec<_> = results
        .iter()
        .filter(|(_, status)| *status == reqwest::StatusCode::OK)
        .map(|(writer, _)| *writer)
        .collect();
    assert_eq!(winners.len(), 1, "{results:?}");
    assert!(
        results
            .iter()
            .all(|(_, status)| *status == reqwest::StatusCode::OK
                || *status == reqwest::StatusCode::PRECONDITION_FAILED),
        "{results:?}"
    );
    assert_eq!(
        read_all(&split, "/race.txt").await,
        format!("writer {:02}", winners[0]).into_bytes()
    );
    server.close().await.expect("server close");
}

/// Run futures as tasks, all at once, and collect their outputs in order.
async fn spawn_all<F>(futures: impl Iterator<Item = F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futures.map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.expect("task"));
    }
    out
}
