use std::future::pending;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, FileHandle, FsDriver, FsError, MemoryFs, Result, Stats,
};
use mount_rs_http::{DriveConfig, DriveRegistry, HttpServer, HttpServerError, HttpServerOptions};
#[cfg(feature = "observability")]
use mount_rs_observability::{Telemetry, TelemetryConfig};
use mount_rs_sqlite::open_sqlite_memory;
use reqwest::header::{AUTHORIZATION, CONTENT_LENGTH, CONTENT_RANGE, RANGE, WWW_AUTHENTICATE};
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
    assert_eq!(unknown_drive.status(), reqwest::StatusCode::NOT_FOUND);

    let deleted = client
        .delete(format!("{base}/v1/drives/sqlite/fs/record.bin"))
        .header(AUTHORIZATION, bearer(SQLITE_TOKEN))
        .send()
        .await
        .expect("delete response");
    assert_eq!(deleted.status(), reqwest::StatusCode::NO_CONTENT);

    server.close().await.expect("server close");
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
    assert_eq!(partial.status(), reqwest::StatusCode::OK);
    assert_eq!(
        partial.bytes().await.expect("partial PUT body").as_ref(),
        b"hello"
    );
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
    assert!(response.starts_with("HTTP/1.1 408"), "response: {response}");
    assert!(response.contains("\"code\":\"EIO\""));
    assert!(response.contains("connection: close"));

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
