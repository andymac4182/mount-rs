//! The behaviour other services rely on from `serve-http`: what a failed
//! drive looks like, what an interrupted upload leaves behind, how errors
//! are named, and how the server lets go of clients that stall.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, FsError, MkdirOptions, Result, Stats,
};
use mount_rs_http::{DriveConfig, DriveRegistry, HttpServer, HttpServerOptions};
use mount_rs_memfs::MemoryFs;
use mount_rs_sqlite_fs::open_sqlite_memory;
use reqwest::StatusCode;
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const TOKEN: &str = "launch-secret";
const RESERVED: &str = ".mount-rs-upload.";

/// A memory filesystem with the switches these tests need: it can fail
/// closed, it can hide that its `write_file` is atomic (so the server has to
/// publish through a temporary sibling), and it counts its open handles.
struct TestFs {
    inner: MemoryFs,
    atomic: bool,
    failed: AtomicBool,
    open_handles: Arc<AtomicUsize>,
}

impl TestFs {
    fn new(atomic: bool) -> Arc<Self> {
        Arc::new(Self {
            inner: MemoryFs::empty(),
            atomic,
            failed: AtomicBool::new(false),
            open_handles: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn check(&self) -> Result<()> {
        if self.failed.load(Ordering::Acquire) {
            // What a split-store drive answers once it has lost its lease.
            return Err(FsError::new(ErrorCode::Estale).with_message("writer lease lost"));
        }
        Ok(())
    }
}

struct CountedHandle {
    inner: Arc<dyn FileHandle>,
    open_handles: Arc<AtomicUsize>,
}

#[async_trait]
impl FileHandle for CountedHandle {
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        self.inner.read(buffer, position).await
    }
    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        // Let other requests in between the pieces of one upload.
        tokio::task::yield_now().await;
        self.inner.write(buffer, position).await
    }
    async fn stat(&self) -> Result<Stats> {
        self.inner.stat().await
    }
    async fn truncate(&self, length: u64) -> Result<()> {
        self.inner.truncate(length).await
    }
    async fn close(&self) -> Result<()> {
        self.open_handles.fetch_sub(1, Ordering::AcqRel);
        self.inner.close().await
    }
}

#[async_trait]
impl FsDriver for TestFs {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn health(&self) -> Result<()> {
        self.check()
    }
    fn write_file_is_atomic(&self) -> bool {
        self.atomic && self.inner.write_file_is_atomic()
    }
    async fn write_file(&self, path: &str, data: &[u8]) -> Result<()> {
        self.check()?;
        if self.atomic {
            self.inner.write_file(path, data).await
        } else {
            mount_rs_core::driver::write_file_through_handle(self, path, data).await
        }
    }
    async fn stat(&self, path: &str) -> Result<Stats> {
        self.check()?;
        self.inner.stat(path).await
    }
    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.check()?;
        self.inner.lstat(path).await
    }
    async fn readdir(&self, path: &str) -> Result<Vec<DirEntry>> {
        self.check()?;
        self.inner.readdir(path).await
    }
    async fn readdir_bounded(&self, path: &str, max_entries: usize) -> Result<Vec<DirEntry>> {
        self.check()?;
        self.inner.readdir_bounded(path, max_entries).await
    }
    async fn open(&self, path: &str, flags: &str, mode: u32) -> Result<Arc<dyn FileHandle>> {
        self.check()?;
        let inner = self.inner.open(path, flags, mode).await?;
        self.open_handles.fetch_add(1, Ordering::AcqRel);
        Ok(Arc::new(CountedHandle {
            inner,
            open_handles: Arc::clone(&self.open_handles),
        }))
    }
    async fn mkdir(&self, path: &str, options: MkdirOptions) -> Result<Option<String>> {
        self.check()?;
        self.inner.mkdir(path, options).await
    }
    async fn rmdir(&self, path: &str) -> Result<()> {
        self.check()?;
        self.inner.rmdir(path).await
    }
    async fn unlink(&self, path: &str) -> Result<()> {
        self.check()?;
        self.inner.unlink(path).await
    }
    async fn rename(&self, from: &str, to: &str) -> Result<()> {
        self.check()?;
        self.inner.rename(from, to).await
    }
    async fn chmod(&self, path: &str, mode: u32) -> Result<()> {
        self.check()?;
        self.inner.chmod(path, mode).await
    }
}

/// A drive on which every operation fails with one error.
struct ErrorFs(ErrorCode);

#[async_trait]
impl FsDriver for ErrorFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    async fn stat(&self, path: &str) -> Result<Stats> {
        Err(FsError::new(self.0).with_syscall("stat").with_path(path))
    }
    async fn lstat(&self, path: &str) -> Result<Stats> {
        self.stat(path).await
    }
    async fn readdir(&self, _path: &str) -> Result<Vec<DirEntry>> {
        Err(FsError::new(self.0))
    }
    async fn readdir_bounded(&self, _path: &str, _max_entries: usize) -> Result<Vec<DirEntry>> {
        Err(FsError::new(self.0))
    }
    async fn open(&self, _path: &str, _flags: &str, _mode: u32) -> Result<Arc<dyn FileHandle>> {
        Err(FsError::new(self.0))
    }
}

async fn start(drives: Vec<(&str, Arc<dyn FsDriver>)>, options: HttpServerOptions) -> HttpServer {
    let mut registry = DriveRegistry::new();
    for (id, driver) in drives {
        registry
            .register(DriveConfig::new(id, driver, TOKEN).expect("drive config"))
            .expect("drive registration");
    }
    HttpServer::start(registry, options)
        .await
        .expect("HTTP server")
}

fn bearer() -> String {
    format!("Bearer {TOKEN}")
}

async fn put(server: &HttpServer, drive: &str, path: &str, body: Vec<u8>) -> reqwest::Response {
    reqwest::Client::new()
        .put(format!("{}/v1/drives/{drive}/fs/{path}", server.url()))
        .header(AUTHORIZATION, bearer())
        .body(body)
        .send()
        .await
        .expect("PUT response")
}

async fn get(server: &HttpServer, route: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}{route}", server.url()))
        .header(AUTHORIZATION, bearer())
        .send()
        .await
        .expect("GET response")
}

/// The standard error body, checked for its shape, as `(code, message)`.
async fn error_of(response: reqwest::Response) -> (String, String) {
    let body: Value = response.json().await.expect("JSON error body");
    let error = body["error"].as_object().expect("error object");
    assert_eq!(body.as_object().expect("object").len(), 1, "{body}");
    assert_eq!(error.len(), 2, "{body}");
    let message = error["message"].as_str().expect("message").to_owned();
    assert!(!message.is_empty());
    (error["code"].as_str().expect("code").to_owned(), message)
}

async fn names(driver: &dyn FsDriver, path: &str) -> Vec<String> {
    let mut names: Vec<_> = driver
        .readdir(path)
        .await
        .expect("readdir")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    names.sort();
    names
}

async fn read_all(driver: &dyn FsDriver, path: &str) -> Vec<u8> {
    let handle = driver.open(path, "r", 0).await.expect("open");
    let size = handle.stat().await.expect("stat").size as usize;
    let mut content = vec![0; size];
    let mut offset = 0;
    while offset < size {
        let count = handle
            .read(&mut content[offset..], Some(offset as u64))
            .await
            .expect("read");
        assert_ne!(count, 0);
        offset += count;
    }
    handle.close().await.expect("close");
    content
}

async fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting until {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Open a connection and send the head of a `PUT` plus the first
/// `sent` bytes of a body declared as `declared` bytes long.
async fn partial_put(
    server: &HttpServer,
    drive: &str,
    path: &str,
    declared: usize,
    sent: usize,
) -> TcpStream {
    let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .expect("socket");
    let head = format!(
        "PUT /v1/drives/{drive}/fs/{path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n",
        bearer()
    );
    socket.write_all(head.as_bytes()).await.expect("head");
    socket.write_all(&vec![b'n'; sent]).await.expect("body");
    socket.flush().await.expect("flush");
    socket
}

async fn response_text(mut socket: TcpStream) -> String {
    let mut response = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), socket.read_to_end(&mut response))
        .await
        .expect("response before the deadline");
    String::from_utf8_lossy(&response).into_owned()
}

async fn drive_kinds() -> Vec<(&'static str, Arc<dyn FsDriver>)> {
    vec![
        ("atomic write_file", TestFs::new(true) as Arc<dyn FsDriver>),
        ("temporary sibling", TestFs::new(false)),
        (
            "sqlite",
            Arc::new(open_sqlite_memory().await.expect("sqlite")),
        ),
    ]
}

// ---------------------------------------------------------------- health

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_drive_that_failed_closed_answers_503_and_takes_readiness_down() {
    let failing = TestFs::new(true);
    let healthy: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let server = start(
        vec![("failing", failing.clone()), ("healthy", healthy)],
        HttpServerOptions::default(),
    )
    .await;
    for drive in ["failing", "healthy"] {
        assert_eq!(
            put(&server, drive, "doc", b"kept".to_vec()).await.status(),
            StatusCode::OK
        );
    }
    let ready = get(&server, "/readyz").await;
    assert_eq!(ready.status(), StatusCode::OK);
    assert_eq!(
        ready.json::<Value>().await.unwrap(),
        json!({"status": "ready", "drives": 2})
    );

    failing.failed.store(true, Ordering::Release);

    // The files are not gone; the drive is unavailable.
    for response in [
        get(&server, "/v1/drives/failing/fs/doc").await,
        get(&server, "/v1/drives/failing/fs/").await,
        get(&server, "/v1/drives/failing/entries/").await,
        get(&server, "/v1/drives/failing/stat/doc").await,
        put(&server, "failing", "doc", b"new".to_vec()).await,
        put(&server, "failing", "other", b"new".to_vec()).await,
    ] {
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(response.headers().get("retry-after").is_none());
        assert_eq!(error_of(response).await.0, "ESTALE");
    }

    let ready = get(&server, "/readyz").await;
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    // No drive ids: this body is served without authentication.
    assert_eq!(
        ready.json::<Value>().await.unwrap(),
        json!({"status": "not_ready", "drives": 2, "failed": 1})
    );
    let head = reqwest::Client::new()
        .head(format!("{}/readyz", server.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::SERVICE_UNAVAILABLE);
    // Liveness is about the process, not its drives.
    assert_eq!(get(&server, "/healthz").await.status(), StatusCode::OK);

    // The other drive is unaffected.
    let read = get(&server, "/v1/drives/healthy/fs/doc").await;
    assert_eq!(read.status(), StatusCode::OK);
    assert_eq!(read.bytes().await.unwrap().as_ref(), b"kept");
    assert_eq!(
        put(&server, "healthy", "doc", b"still writable".to_vec())
            .await
            .status(),
        StatusCode::OK
    );
    server.close().await.expect("close");
}

#[tokio::test]
async fn readiness_with_no_drives_is_not_ready() {
    let server = start(Vec::new(), HttpServerOptions::default()).await;
    let ready = get(&server, "/readyz").await;
    assert_eq!(ready.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        ready.json::<Value>().await.unwrap(),
        json!({"status": "not_ready", "drives": 0, "failed": 0})
    );
    server.close().await.expect("close");
}

// --------------------------------------------------------------- uploads

const PREVIOUS: &[u8] = b"the previous content, which an interrupted upload must not touch";

async fn assert_previous_content_intact(kind: &str, driver: &dyn FsDriver, when: &str) {
    assert_eq!(
        read_all(driver, "/doc").await,
        PREVIOUS,
        "{kind}: content after an upload {when}"
    );
    // Asked of the driver itself, so a hidden temporary would show.
    assert_eq!(
        names(driver, "/").await,
        ["doc"],
        "{kind}: names after an upload {when}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_abandoned_mid_body_leaves_the_previous_content() {
    for (kind, driver) in drive_kinds().await {
        let server = start(vec![("d", driver.clone())], HttpServerOptions::default()).await;
        assert_eq!(
            put(&server, "d", "doc", PREVIOUS.to_vec()).await.status(),
            StatusCode::OK
        );
        // More than one write buffer of the old streaming upload arrives
        // before the client goes away.
        let socket = partial_put(&server, "d", "doc", 400_000, 200_000).await;
        until("the upload is being received", || server.connections() == 1).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(socket);
        until("the connection is released", || server.connections() == 0).await;
        assert_previous_content_intact(kind, driver.as_ref(), "abandoned mid-body").await;
        let read = get(&server, "/v1/drives/d/fs/doc").await;
        assert_eq!(read.bytes().await.unwrap().as_ref(), PREVIOUS, "{kind}");
        server.close().await.expect("close");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_that_times_out_leaves_the_previous_content() {
    for (kind, driver) in drive_kinds().await {
        let server = start(
            vec![("d", driver.clone())],
            HttpServerOptions {
                request_timeout: Duration::from_millis(300),
                ..HttpServerOptions::default()
            },
        )
        .await;
        assert_eq!(
            put(&server, "d", "doc", PREVIOUS.to_vec()).await.status(),
            StatusCode::OK
        );
        let socket = partial_put(&server, "d", "doc", 400_000, 200_000).await;
        let response = response_text(socket).await;
        assert!(response.starts_with("HTTP/1.1 504"), "{kind}: {response}");
        assert!(
            response.contains(r#""code":"ETIMEDOUT""#),
            "{kind}: {response}"
        );
        until("the connection is released", || server.connections() == 0).await;
        assert_previous_content_intact(kind, driver.as_ref(), "that timed out").await;
        server.close().await.expect("close");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_interrupted_by_shutdown_leaves_the_previous_content() {
    for (kind, driver) in drive_kinds().await {
        let server = start(
            vec![("d", driver.clone())],
            HttpServerOptions {
                drain_timeout: Duration::from_secs(5),
                ..HttpServerOptions::default()
            },
        )
        .await;
        assert_eq!(
            put(&server, "d", "doc", PREVIOUS.to_vec()).await.status(),
            StatusCode::OK
        );
        let socket = partial_put(&server, "d", "doc", 400_000, 200_000).await;
        until("the upload is being received", || server.connections() == 1).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        // What SIGTERM does to the process during a deploy.
        server.close().await.expect("close drains the upload");
        let response = response_text(socket).await;
        assert!(response.starts_with("HTTP/1.1 503"), "{kind}: {response}");
        assert!(response.contains(r#""code":"EIO""#), "{kind}: {response}");
        assert!(response.contains("shutting down"), "{kind}: {response}");
        assert_previous_content_intact(kind, driver.as_ref(), "interrupted by shutdown").await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_uploads_to_one_path_leave_exactly_one_body() {
    for (kind, driver) in drive_kinds().await {
        let server =
            Arc::new(start(vec![("d", driver.clone())], HttpServerOptions::default()).await);
        for round in 0..4 {
            // Bodies of different lengths, each all one byte: any mixture
            // or prefix of them is not one of them.
            let bodies: Vec<Vec<u8>> = (0..8u8)
                .map(|writer| vec![b'a' + writer; 90_000 + usize::from(writer) * 41_003])
                .collect();
            let mut uploads = tokio::task::JoinSet::new();
            for body in bodies.clone() {
                let server = Arc::clone(&server);
                uploads.spawn(async move { put(&server, "d", "shared", body).await.status() });
            }
            while let Some(status) = uploads.join_next().await {
                assert_eq!(status.expect("upload task"), StatusCode::OK, "{kind}");
            }
            let stored = read_all(driver.as_ref(), "/shared").await;
            assert!(
                bodies.contains(&stored),
                "{kind} round {round}: {} bytes stored, starting {:?}, ending {:?}",
                stored.len(),
                stored.first().map(|byte| *byte as char),
                stored.last().map(|byte| *byte as char),
            );
            assert_eq!(names(driver.as_ref(), "/").await, ["shared"], "{kind}");
        }
        server.close().await.expect("close");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_publication_removes_its_temporary_and_keeps_the_target() {
    let driver = TestFs::new(false);
    let server = start(vec![("d", driver.clone())], HttpServerOptions::default()).await;
    driver
        .mkdir("/taken", MkdirOptions::default())
        .await
        .unwrap();
    driver
        .mkdir("/taken/inside", MkdirOptions::default())
        .await
        .unwrap();
    // A directory cannot be replaced by a file.
    let response = put(&server, "d", "taken", b"file".to_vec()).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(error_of(response).await.0, "EISDIR");
    // Nor can a file be created where there is no directory for it.
    let response = put(&server, "d", "absent/file", b"file".to_vec()).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_of(response).await.0, "ENOENT");
    assert_eq!(names(driver.as_ref(), "/").await, ["taken"]);
    assert_eq!(names(driver.as_ref(), "/taken").await, ["inside"]);
    assert_eq!(driver.open_handles.load(Ordering::Acquire), 0);

    // The answer keeps its shape.
    let response = put(&server, "d", "taken/new", b"four".to_vec()).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.json::<Value>().await.unwrap(),
        json!({"path": "/taken/new", "bytes": 4})
    );
    server.close().await.expect("close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reserved_names_cannot_be_reached_listed_or_left_behind() {
    let driver = TestFs::new(false);
    let server = start(vec![("d", driver.clone())], HttpServerOptions::default()).await;
    let client = reqwest::Client::new();
    let leftover = format!("{RESERVED}0123456789abcdef0123456789abcdef");

    // What a process killed mid-upload leaves: a sibling nobody renamed.
    driver.mkdir("/dir", MkdirOptions::default()).await.unwrap();
    driver
        .inner
        .write_file(&format!("/dir/{leftover}"), b"half an upload")
        .await
        .unwrap();

    for route in ["fs", "entries", "stat", "lstat"] {
        let response = get(&server, &format!("/v1/drives/d/{route}/dir/{leftover}")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{route}");
        assert_eq!(error_of(response).await.0, "EINVAL");
    }
    let response = put(&server, "d", &format!("dir/{leftover}"), b"mine".to_vec()).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    for (operation, body) in [
        ("mkdir", json!({"path": format!("/{RESERVED}d")})),
        (
            "rename",
            json!({"from": format!("/dir/{leftover}"), "to": "/stolen"}),
        ),
        (
            "rename",
            json!({"from": "/dir", "to": format!("/{RESERVED}x")}),
        ),
        (
            "truncate",
            json!({"path": format!("/dir/{leftover}"), "length": 0}),
        ),
    ] {
        let response = client
            .post(format!("{}/v1/drives/d/ops/{operation}", server.url()))
            .header(AUTHORIZATION, bearer())
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{operation}");
    }

    // Both listings leave it out.
    let entries: Value = get(&server, "/v1/drives/d/entries/dir")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(entries, json!([]));
    let directory: Value = get(&server, "/v1/drives/d/fs/dir")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(directory["entries"], json!([]));

    // A directory holding a client's file as well is still not empty, and
    // nothing is removed from it.
    assert_eq!(
        put(&server, "d", "dir/real", b"real".to_vec())
            .await
            .status(),
        StatusCode::OK
    );
    let delete = |path: &'static str| {
        client
            .delete(format!("{}/v1/drives/d/fs/{path}", server.url()))
            .header(AUTHORIZATION, bearer())
            .send()
    };
    let response = delete("dir").await.unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(error_of(response).await.0, "ENOTEMPTY");
    assert_eq!(
        names(driver.as_ref(), "/dir").await,
        [leftover.as_str(), "real"]
    );

    // Once only the leftover remains, the directory that lists as empty
    // can be deleted, and the leftover goes with it.
    assert_eq!(
        delete("dir/real").await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        delete("dir").await.unwrap().status(),
        StatusCode::NO_CONTENT
    );
    assert!(names(driver.as_ref(), "/").await.is_empty());

    // The drive root is refused as a permanent client error, not as a 503
    // that invites a retry.
    let response = client
        .delete(format!("{}/v1/drives/d/fs", server.url()))
        .header(AUTHORIZATION, bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(response.headers().get("retry-after").is_none());
    assert_eq!(error_of(response).await.0, "EINVAL");
    server.close().await.expect("close");
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_host_drive_upload_replaces_the_file_through_a_synced_sibling() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = std::env::temp_dir().join(format!(
        "mount-rs-http-host-{}-{:?}",
        std::process::id(),
        Instant::now()
    ));
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("sub/doc"), PREVIOUS).unwrap();
    std::fs::set_permissions(root.join("sub/doc"), std::fs::Permissions::from_mode(0o600)).unwrap();
    std::os::unix::fs::symlink("doc", root.join("sub/link")).unwrap();
    let before = std::fs::metadata(root.join("sub/doc")).unwrap();

    let driver: Arc<dyn FsDriver> = Arc::new(mount_rs_host::HostFs::new(&root));
    assert!(!driver.write_file_is_atomic());
    let server = start(vec![("host", driver)], HttpServerOptions::default()).await;

    // Interrupted: nothing changes and nothing is left beside the file.
    let socket = partial_put(&server, "host", "sub/doc", 400_000, 200_000).await;
    until("the upload is being received", || server.connections() == 1).await;
    drop(socket);
    until("the connection is released", || server.connections() == 0).await;
    let listing = |directory: &str| {
        let mut names: Vec<_> = std::fs::read_dir(root.join(directory))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    };
    assert_eq!(std::fs::read(root.join("sub/doc")).unwrap(), PREVIOUS);
    assert_eq!(listing("sub"), ["doc", "link"]);

    // Completed: a new file under the old name, with the old permissions.
    let body = vec![b'h'; 300_000];
    assert_eq!(
        put(&server, "host", "sub/doc", body.clone()).await.status(),
        StatusCode::OK
    );
    let after = std::fs::metadata(root.join("sub/doc")).unwrap();
    assert_eq!(std::fs::read(root.join("sub/doc")).unwrap(), body);
    assert_ne!(
        after.ino(),
        before.ino(),
        "the file was replaced, not rewritten"
    );
    assert_eq!(after.permissions().mode() & 0o7777, 0o600);
    assert_eq!(listing("sub"), ["doc", "link"]);

    // A new file, and a write through a link (which stays a link).
    assert_eq!(
        put(&server, "host", "sub/new", b"new".to_vec())
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        put(&server, "host", "sub/link", b"via link".to_vec())
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(std::fs::read(root.join("sub/doc")).unwrap(), b"via link");
    assert!(
        std::fs::symlink_metadata(root.join("sub/link"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(listing("sub"), ["doc", "link", "new"]);

    let response = put(&server, "host", "sub", b"file".to_vec()).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(error_of(response).await.0, "EISDIR");
    let response = put(&server, "host", "absent/file", b"file".to_vec()).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(listing(""), ["sub"]);

    server.close().await.expect("close");
    std::fs::remove_dir_all(&root).unwrap();
}

// ---------------------------------------------------------- upload budget

/// One upload's worth of budget, held by an upload that never finishes.
async fn budget_server(request_timeout: Duration) -> (HttpServer, Arc<dyn FsDriver>) {
    let driver: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let server = start(
        vec![("d", driver.clone())],
        HttpServerOptions {
            max_request_bytes: 256 * 1024,
            // Room for two of the largest uploads, whatever the connections.
            max_buffered_upload_bytes: Some(512 * 1024),
            max_connections: 64,
            request_timeout,
            ..HttpServerOptions::default()
        },
    )
    .await;
    (server, driver)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn uploads_over_the_memory_budget_wait_for_room_and_then_succeed() {
    let (server, driver) = budget_server(Duration::from_secs(20)).await;
    let server = Arc::new(server);
    // Two uploads take the whole budget and stall.
    let mut holders = Vec::new();
    for holder in 0..2 {
        let path = format!("holder-{holder}");
        holders.push(partial_put(&server, "d", &path, 256 * 1024, 1024).await);
    }
    until("both holders are being received", || {
        server.connections() == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Six more, each complete: within every other limit, over the budget.
    let mut waiting = tokio::task::JoinSet::new();
    for upload in 0..6u8 {
        let server = Arc::clone(&server);
        waiting.spawn(async move {
            let body = vec![b'0' + upload; 200 * 1024];
            put(&server, "d", &format!("waiting-{upload}"), body)
                .await
                .status()
        });
    }
    // They wait: none is refused, none is published.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(waiting.try_join_next().is_none(), "an upload did not wait");
    assert!(names(driver.as_ref(), "/").await.is_empty());

    // Room appears when a holder goes away, and all six get through it.
    drop(holders.pop());
    let mut finished = 0;
    while let Some(status) = tokio::time::timeout(Duration::from_secs(15), waiting.join_next())
        .await
        .expect("waiting uploads finish once there is room")
    {
        assert_eq!(status.expect("upload task"), StatusCode::OK);
        finished += 1;
    }
    assert_eq!(finished, 6);
    let stored = names(driver.as_ref(), "/").await;
    assert_eq!(stored.len(), 6, "{stored:?}");
    for upload in 0..6u8 {
        assert_eq!(
            read_all(driver.as_ref(), &format!("/waiting-{upload}")).await,
            vec![b'0' + upload; 200 * 1024]
        );
    }
    drop(holders);
    server.close().await.expect("close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn waiting_for_the_memory_budget_counts_against_the_request_timeout() {
    let timeout = Duration::from_millis(1500);
    let (server, driver) = budget_server(timeout).await;
    // The budget is taken at once and stays taken until these time out.
    let first = partial_put(&server, "d", "holder-0", 256 * 1024, 1024).await;
    let second = partial_put(&server, "d", "holder-1", 256 * 1024, 1024).await;
    until("both holders are being received", || {
        server.connections() == 2
    })
    .await;

    // Half the timeout later a third arrives and stalls as well. It gets
    // the budget only when the holders time out, and its own deadline is
    // then half a timeout away, not a whole one.
    tokio::time::sleep(timeout / 2).await;
    let started = Instant::now();
    let third = partial_put(&server, "d", "late", 256 * 1024, 1024).await;
    let response = response_text(third).await;
    let waited = started.elapsed();
    assert!(response.starts_with("HTTP/1.1 504"), "{response}");
    assert!(response.contains(r#""code":"ETIMEDOUT""#), "{response}");
    assert!(
        waited >= timeout - Duration::from_millis(100) && waited < timeout + timeout / 3,
        "the late upload was answered after {waited:?}; its timeout is {timeout:?}"
    );
    for holder in [first, second] {
        assert!(response_text(holder).await.starts_with("HTTP/1.1 504"));
    }
    assert!(names(driver.as_ref(), "/").await.is_empty());
    server.close().await.expect("close");
}

// ------------------------------------------------------------- downloads

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_client_that_stops_reading_a_download_is_released() {
    let driver = TestFs::new(true);
    // Far more than the socket buffers between the two ends can hold.
    driver
        .inner
        .write_file("/large", &vec![7; 64 * 1024 * 1024])
        .await
        .unwrap();
    let server = start(
        vec![("d", driver.clone())],
        HttpServerOptions {
            request_timeout: Duration::from_millis(300),
            ..HttpServerOptions::default()
        },
    )
    .await;
    let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .unwrap();
    socket
        .write_all(
            format!(
                "GET /v1/drives/d/fs/large HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\n\r\n",
                bearer()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    // The response starts, and then nobody reads it.
    let mut head = [0; 64];
    socket.read_exact(&mut head).await.unwrap();
    assert!(head.starts_with(b"HTTP/1.1 200"));
    until("the download holds a handle", || {
        driver.open_handles.load(Ordering::Acquire) == 1
    })
    .await;

    until("the stalled download gives up its file handle", || {
        driver.open_handles.load(Ordering::Acquire) == 0
    })
    .await;
    until("the stalled download gives up its connection", || {
        server.connections() == 0
    })
    .await;
    // The server stays responsive, and the cut-off client is told nothing
    // that looks like a complete file.
    assert_eq!(get(&server, "/healthz").await.status(), StatusCode::OK);
    let mut rest = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(10), socket.read_to_end(&mut rest)).await;
    assert!(rest.len() < 64 * 1024 * 1024);
    server.close().await.expect("close");
}

// -------------------------------------------------------------- shutdown

/// A drive whose `stat` never answers.
struct StuckFs {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl FsDriver for StuckFs {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    async fn stat(&self, _path: &str) -> Result<Stats> {
        self.entered.notify_one();
        std::future::pending().await
    }
    async fn readdir(&self, _path: &str) -> Result<Vec<DirEntry>> {
        Err(FsError::new(ErrorCode::Enotsup))
    }
    async fn open(&self, _path: &str, _flags: &str, _mode: u32) -> Result<Arc<dyn FileHandle>> {
        Err(FsError::new(ErrorCode::Enotsup))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_close_that_runs_out_of_drain_time_stops_what_is_still_being_served() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let server = start(
        vec![(
            "stuck",
            Arc::new(StuckFs {
                entered: Arc::clone(&entered),
            }) as Arc<dyn FsDriver>,
        )],
        HttpServerOptions {
            drain_timeout: Duration::from_millis(200),
            ..HttpServerOptions::default()
        },
    )
    .await;
    // A request the drive never answers: a graceful stop waits for it.
    let mut stuck = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .unwrap();
    stuck
        .write_all(
            format!(
                "GET /v1/drives/stuck/stat/file HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: {}\r\n\r\n",
                bearer()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered.notified())
        .await
        .expect("the request reached the drive");

    let closing = Instant::now();
    let closed = server.close().await;
    assert!(
        matches!(closed, Err(mount_rs_http::HttpServerError::Timeout)),
        "{closed:?}"
    );
    // The failure is reported, but nothing is left running behind it: the
    // caller may release the drives.
    assert!(closing.elapsed() < Duration::from_secs(5));
    assert_eq!(server.connections(), 0);
    let mut rest = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stuck.read_to_end(&mut rest))
        .await
        .expect("the connection was closed");
    assert!(rest.is_empty(), "{}", String::from_utf8_lossy(&rest));
}

// ------------------------------------------------------- authentication

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unknown_drive_is_indistinguishable_from_a_wrong_token() {
    let server = start(
        vec![("known", Arc::new(MemoryFs::empty()) as Arc<dyn FsDriver>)],
        HttpServerOptions::default(),
    )
    .await;
    let client = reqwest::Client::new();
    let answer = |method: reqwest::Method, drive: &str, route: &str, token: Option<&str>| {
        let mut request = client.request(
            method,
            format!("{}/v1/drives/{drive}/{route}", server.url()),
        );
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        async move {
            let response = request.send().await.unwrap();
            let status = response.status();
            let challenge = response
                .headers()
                .get("www-authenticate")
                .map(|value| value.to_str().unwrap().to_owned());
            (status, challenge, response.text().await.unwrap())
        }
    };
    for (method, route) in [
        (reqwest::Method::GET, "fs/file"),
        (reqwest::Method::PUT, "fs/file"),
        (reqwest::Method::DELETE, "fs/file"),
        (reqwest::Method::GET, "entries/"),
        (reqwest::Method::GET, "stat/file"),
        (reqwest::Method::POST, "ops/mkdir"),
    ] {
        let wrong_token = answer(method.clone(), "known", route, Some("wrong")).await;
        assert_eq!(wrong_token.0, StatusCode::UNAUTHORIZED);
        assert_eq!(wrong_token.1.as_deref(), Some("Bearer"));
        assert!(wrong_token.2.contains(r#""code":"EACCES""#));
        for token in [Some(TOKEN), Some("wrong"), None] {
            assert_eq!(
                answer(method.clone(), "unknown", route, token).await,
                wrong_token,
                "{method} {route} with token {token:?}"
            );
        }
    }
    server.close().await.expect("close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_bearer_scheme_is_matched_without_regard_to_case() {
    let server = start(
        vec![("d", Arc::new(MemoryFs::empty()) as Arc<dyn FsDriver>)],
        HttpServerOptions::default(),
    )
    .await;
    let client = reqwest::Client::new();
    let status = |authorization: String| {
        let request = client
            .get(format!("{}/v1/drives/d/entries/", server.url()))
            .header(AUTHORIZATION, authorization);
        async move { request.send().await.unwrap().status() }
    };
    for scheme in ["Bearer", "bearer", "BEARER", "bEaReR"] {
        assert_eq!(
            status(format!("{scheme} {TOKEN}")).await,
            StatusCode::OK,
            "{scheme}"
        );
        let discovery = client
            .get(format!("{}/v1/drives", server.url()))
            .header(AUTHORIZATION, format!("{scheme} {TOKEN}"))
            .send()
            .await
            .unwrap();
        assert_eq!(discovery.status(), StatusCode::OK, "{scheme}");
    }
    assert_eq!(status(format!("bearer   {TOKEN}")).await, StatusCode::OK);
    for rejected in [
        format!("Basic {TOKEN}"),
        format!("Bearer{TOKEN}"),
        format!("Bearer {TOKEN} extra"),
        format!("Bearer {}", TOKEN.to_uppercase()),
        "Bearer ".to_owned(),
        TOKEN.to_owned(),
    ] {
        assert_eq!(
            status(rejected.clone()).await,
            StatusCode::UNAUTHORIZED,
            "{rejected}"
        );
    }
    server.close().await.expect("close");
}

// ---------------------------------------------------------- error mapping

async fn assert_error_mapping(code: ErrorCode, status: u16, retry_after: Option<&str>) {
    let server = start(
        vec![("errors", Arc::new(ErrorFs(code)) as Arc<dyn FsDriver>)],
        HttpServerOptions::default(),
    )
    .await;
    for route in ["stat/thing", "fs/thing", "entries/thing"] {
        let response = get(&server, &format!("/v1/drives/errors/{route}")).await;
        assert_eq!(response.status().as_u16(), status, "{route}");
        assert_eq!(
            response
                .headers()
                .get("retry-after")
                .map(|value| value.to_str().unwrap()),
            retry_after,
            "{route}"
        );
        assert_eq!(
            response
                .headers()
                .get("content-type")
                .map(|value| value.to_str().unwrap()),
            Some("application/json")
        );
        assert_eq!(error_of(response).await.0, code.as_str(), "{route}");
    }
    server.close().await.expect("close");
}

macro_rules! error_mapping {
    ($($test:ident: $code:ident => $status:literal $(, retry_after = $retry:literal)?;)+) => {$(
        #[tokio::test]
        async fn $test() {
            #[allow(unused_mut, unused_assignments)]
            let mut retry_after = None;
            $(retry_after = Some($retry);)?
            assert_error_mapping(ErrorCode::$code, $status, retry_after).await;
        }
    )+};
}

error_mapping! {
    enoent_is_404: Enoent => 404;
    enotdir_is_404: Enotdir => 404;
    eexist_is_409: Eexist => 409;
    enotempty_is_409: Enotempty => 409;
    eisdir_is_409: Eisdir => 409;
    einval_is_400: Einval => 400;
    eloop_is_400: Eloop => 400;
    exdev_is_400: Exdev => 400;
    emlink_is_400: Emlink => 400;
    enxio_is_400: Enxio => 400;
    eacces_is_403: Eacces => 403;
    eperm_is_403: Eperm => 403;
    erofs_is_403: Erofs => 403;
    estale_is_503: Estale => 503;
    ebadf_is_503: Ebadf => 503;
    eagain_is_503_with_retry_after: Eagain => 503, retry_after = "1";
    ebusy_is_503_with_retry_after: Ebusy => 503, retry_after = "1";
    enospc_is_507: Enospc => 507;
    edquot_is_507: Edquot => 507;
    efbig_is_413: Efbig => 413;
    enametoolong_is_414: Enametoolong => 414;
    enotsup_is_501: Enotsup => 501;
    enosys_is_501: Enosys => 501;
    eio_is_500: Eio => 500;
    an_error_with_no_better_description_is_500: Enomem => 500;
}

#[tokio::test]
async fn a_request_the_server_ran_out_of_time_for_is_504_etimedout() {
    let server = start(
        vec![("d", Arc::new(MemoryFs::empty()) as Arc<dyn FsDriver>)],
        HttpServerOptions {
            request_timeout: Duration::from_millis(100),
            ..HttpServerOptions::default()
        },
    )
    .await;
    let response = response_text(partial_put(&server, "d", "slow", 10, 1).await).await;
    assert!(response.starts_with("HTTP/1.1 504"), "{response}");
    assert!(response.contains("connection: close"), "{response}");
    let body = response.split("\r\n\r\n").nth(1).expect("body");
    let body: Value = serde_json::from_str(body).expect("JSON error body");
    assert_eq!(body["error"]["code"], "ETIMEDOUT");
    assert!(!body["error"]["message"].as_str().unwrap().is_empty());
    server.close().await.expect("close");
}

#[tokio::test]
async fn an_unacceptable_listing_encoding_is_406_with_the_standard_body() {
    let server = start(
        vec![("d", Arc::new(MemoryFs::empty()) as Arc<dyn FsDriver>)],
        HttpServerOptions::default(),
    )
    .await;
    let response = reqwest::Client::new()
        .get(format!("{}/v1/drives/d/entries/", server.url()))
        .header(AUTHORIZATION, bearer())
        .header("accept-encoding", "br, identity;q=0")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_ACCEPTABLE);
    assert_eq!(
        response
            .headers()
            .get("vary")
            .map(|value| value.to_str().unwrap()),
        Some("Accept-Encoding")
    );
    assert_eq!(error_of(response).await.0, "ENOTSUP");
    server.close().await.expect("close");
}

#[tokio::test]
async fn a_request_refused_during_shutdown_is_503_with_the_standard_body() {
    use http_body_util::BodyExt;

    let mut registry = DriveRegistry::new();
    registry
        .register(DriveConfig::new("d", Arc::new(MemoryFs::empty()), TOKEN).unwrap())
        .unwrap();
    let server = HttpServer::unbound(registry, HttpServerOptions::default());
    server.close().await.expect("close");
    let empty = http_body_util::Empty::<bytes::Bytes>::new()
        .map_err(|never| -> mount_rs_http::BoxError { match never {} })
        .boxed_unsync();
    let request = hyper::Request::builder()
        .uri("/v1/drives/d/entries/")
        .header("authorization", bearer())
        .body(empty)
        .unwrap();
    let response = server.handle(request).await;
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(response.headers()["connection"], "close");
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["error"]["code"], "EIO");
    assert_eq!(body["error"]["message"], "server is shutting down");
}

// ------------------------------------------------------------------ mkdir

#[tokio::test]
async fn mkdir_takes_only_permission_bits_from_the_client() {
    let driver: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let server = start(vec![("d", driver.clone())], HttpServerOptions::default()).await;
    let response = reqwest::Client::new()
        .post(format!("{}/v1/drives/d/ops/mkdir", server.url()))
        .header(AUTHORIZATION, bearer())
        // Setuid, setgid and sticky on top of rwx for all, and a file type.
        .json(&json!({"path": "/made", "mode": 0o107_777}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let stats = driver.stat("/made").await.unwrap();
    assert!(stats.is_directory());
    assert_eq!(stats.mode & 0o7777, 0o777);
    server.close().await.expect("close");
}

// ------------------------------------------------------------ request ids

fn request_id_of(response: &reqwest::Response) -> String {
    let mut values = response.headers().get_all("x-request-id").iter();
    let id = values.next().expect("x-request-id header");
    assert!(values.next().is_none(), "one x-request-id header");
    id.to_str().expect("request id text").to_owned()
}

fn is_generated_request_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_response_carries_a_request_id_and_the_error_body_is_unchanged() {
    let driver: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let server = start(vec![("d", driver)], HttpServerOptions::default()).await;
    let client = reqwest::Client::new();
    let mut seen = std::collections::BTreeSet::new();

    for route in ["/healthz", "/readyz", "/v1/drives", "/v1/drives/d/entries"] {
        let response = get(&server, route).await;
        assert_eq!(response.status(), StatusCode::OK, "{route}");
        let id = request_id_of(&response);
        assert!(is_generated_request_id(&id), "{route}: {id}");
        assert!(seen.insert(id), "{route}: a request id was reused");
    }
    let written = put(&server, "d", "file", b"body".to_vec()).await;
    assert_eq!(written.status(), StatusCode::OK);
    assert!(seen.insert(request_id_of(&written)));

    // Errors name their request in the header only: the body keeps its
    // two fields.
    for (route, authorized, status, code) in [
        ("/v1/drives/d/fs/missing", true, 404, "ENOENT"),
        ("/v1/drives/d/fs/file", false, 401, "EACCES"),
        ("/v1/drives/d/fs/a%2Fb", true, 400, "EINVAL"),
        ("/v1/unknown", true, 404, "ENOENT"),
    ] {
        let mut request = client.get(format!("{}{route}", server.url()));
        if authorized {
            request = request.header(AUTHORIZATION, bearer());
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status().as_u16(), status, "{route}");
        let id = request_id_of(&response);
        assert!(is_generated_request_id(&id), "{route}: {id}");
        assert!(seen.insert(id), "{route}: a request id was reused");
        assert_eq!(error_of(response).await.0, code, "{route}");
    }

    let head = client
        .head(format!("{}/v1/drives/d/fs/file", server.url()))
        .header(AUTHORIZATION, bearer())
        .send()
        .await
        .unwrap();
    assert!(is_generated_request_id(&request_id_of(&head)));
    server.close().await.expect("close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_safe_client_request_id_is_echoed_and_any_other_is_replaced() {
    let driver: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let server = start(vec![("d", driver)], HttpServerOptions::default()).await;
    let client = reqwest::Client::new();
    let send = |values: Vec<String>| {
        let mut request = client.get(format!("{}/v1/drives/d/fs/missing", server.url()));
        for value in values {
            request = request.header("x-request-id", value);
        }
        async move {
            let response = request.send().await.unwrap();
            // With or without a token, the id comes back.
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            request_id_of(&response)
        }
    };

    let longest = "a".repeat(128);
    for accepted in [
        "a",
        "Req_01.trace-9",
        "0123456789abcdef0123456789abcdef",
        longest.as_str(),
    ] {
        assert_eq!(send(vec![accepted.to_owned()]).await, accepted);
    }
    let too_long = "a".repeat(129);
    for refused in [
        "",
        too_long.as_str(),
        "has space",
        "slash/inside",
        "colon:inside",
        "quote\"inside",
        "caf\u{e9}",
    ] {
        let id = send(vec![refused.to_owned()]).await;
        assert!(
            is_generated_request_id(&id),
            "{refused:?} came back as {id}"
        );
    }
    // Two values name no one request.
    let id = send(vec!["first".to_owned(), "second".to_owned()]).await;
    assert!(is_generated_request_id(&id), "{id}");
    server.close().await.expect("close");
}

/// What the CLI does with sessions or forwarding enabled: a handler in front
/// of the drive routes, which are a second, unbound application.
struct RecordsRequestId {
    application: HttpServer,
    seen: std::sync::Mutex<Vec<Vec<String>>>,
}

impl mount_rs_http::HttpRequestHandler for RecordsRequestId {
    fn handle(
        &self,
        request: hyper::Request<mount_rs_http::HttpBody>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = hyper::Response<mount_rs_http::HttpBody>> + Send + '_>,
    > {
        Box::pin(async move {
            let ids = request
                .headers()
                .get_all("x-request-id")
                .iter()
                .map(|value| value.to_str().unwrap().to_owned())
                .collect();
            self.seen.lock().unwrap().push(ids);
            self.application.handle(request).await
        })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_handler_is_given_the_request_id_the_response_carries() {
    let mut registry = DriveRegistry::new();
    registry
        .register(DriveConfig::new("d", Arc::new(MemoryFs::empty()), TOKEN).unwrap())
        .unwrap();
    let handler = Arc::new(RecordsRequestId {
        application: HttpServer::unbound(registry, HttpServerOptions::default()),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let server = HttpServer::start_with_handler(handler.clone(), HttpServerOptions::default())
        .await
        .expect("HTTP server");
    let client = reqwest::Client::new();
    let url = format!("{}/v1/drives/d/entries", server.url());

    let generated = client
        .get(&url)
        .header(AUTHORIZATION, bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(generated.status(), StatusCode::OK);
    let generated = request_id_of(&generated);
    assert!(is_generated_request_id(&generated), "{generated}");

    let echoed = client
        .get(&url)
        .header(AUTHORIZATION, bearer())
        .header("x-request-id", "caller-7")
        .send()
        .await
        .unwrap();
    assert_eq!(request_id_of(&echoed), "caller-7");

    // An unsafe value never reaches the handler, or a forwarding hop.
    let replaced = client
        .get(&url)
        .header(AUTHORIZATION, bearer())
        .header("x-request-id", "not safe")
        .header("x-request-id", "second")
        .send()
        .await
        .unwrap();
    let replaced = request_id_of(&replaced);
    assert!(is_generated_request_id(&replaced), "{replaced}");

    assert_eq!(
        *handler.seen.lock().unwrap(),
        vec![vec![generated], vec!["caller-7".to_owned()], vec![replaced]]
    );
    server.close().await.expect("close");
    handler.application.close().await.expect("close");
}

#[tokio::test]
async fn a_request_refused_during_shutdown_still_carries_its_request_id() {
    use http_body_util::BodyExt;

    let server = HttpServer::unbound(DriveRegistry::new(), HttpServerOptions::default());
    server.close().await.expect("close");
    let empty = http_body_util::Empty::<bytes::Bytes>::new()
        .map_err(|never| -> mount_rs_http::BoxError { match never {} })
        .boxed_unsync();
    let request = hyper::Request::builder()
        .uri("/healthz")
        .header("x-request-id", "probe-1")
        .body(empty)
        .unwrap();
    let response = server.handle(request).await;
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(response.headers()["x-request-id"], "probe-1");
}

// ------------------------------------------------------------- access log

/// One access record, kept as its owner would write it.
#[derive(Debug, Clone, PartialEq)]
struct Logged {
    request_id: String,
    method: String,
    route: &'static str,
    drive: Option<String>,
    status: Option<u16>,
    bytes_in: u64,
    bytes_out: u64,
}

#[derive(Debug, Default)]
struct CollectingLog(std::sync::Mutex<Vec<Logged>>);

impl mount_rs_http::AccessLog for CollectingLog {
    fn record(&self, record: &mount_rs_http::AccessRecord<'_>) {
        self.0.lock().unwrap().push(Logged {
            request_id: record.request_id.to_owned(),
            method: record.method.to_owned(),
            route: record.route,
            drive: record.drive.map(str::to_owned),
            status: record.status,
            bytes_in: record.bytes_in,
            bytes_out: record.bytes_out,
        });
    }
}

impl CollectingLog {
    /// The next record, in the order requests finished.
    async fn next(&self, seen: &mut usize) -> Logged {
        let index = *seen;
        until("an access record", || self.0.lock().unwrap().len() > index).await;
        *seen += 1;
        self.0.lock().unwrap()[index].clone()
    }
}

fn logged_options(log: &Arc<CollectingLog>) -> HttpServerOptions {
    HttpServerOptions {
        access_log: Some(log.clone()),
        ..HttpServerOptions::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_access_log_gets_one_record_per_request_without_paths_or_tokens() {
    let log = Arc::new(CollectingLog::default());
    let driver: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let server = start(vec![("d", driver)], logged_options(&log)).await;
    let client = reqwest::Client::new();
    let mut seen = 0;
    let fs = "/v1/drives/{id}/fs/{path}";

    let body = vec![7_u8; 3000];
    let written = client
        .put(format!("{}/v1/drives/d/fs/secret-name.txt", server.url()))
        .header(AUTHORIZATION, bearer())
        .header("x-request-id", "upload-1")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(written.status(), StatusCode::OK);
    let reply = written.bytes().await.unwrap();
    assert_eq!(
        log.next(&mut seen).await,
        Logged {
            request_id: "upload-1".to_owned(),
            method: "PUT".to_owned(),
            route: fs,
            drive: Some("d".to_owned()),
            status: Some(200),
            bytes_in: 3000,
            bytes_out: reply.len() as u64,
        }
    );

    // A download counts the bytes sent; the id is the one the response had.
    let read = get(&server, "/v1/drives/d/fs/secret-name.txt?download=1").await;
    let id = request_id_of(&read);
    assert_eq!(read.bytes().await.unwrap().len(), 3000);
    let record = log.next(&mut seen).await;
    assert_eq!(record.request_id, id);
    assert_eq!(
        (record.method.as_str(), record.route, record.status),
        ("GET", fs, Some(200))
    );
    assert_eq!((record.bytes_in, record.bytes_out), (0, 3000));

    // HEAD sends no body, whatever length it announces.
    let head = client
        .head(format!("{}/v1/drives/d/fs/secret-name.txt", server.url()))
        .header(AUTHORIZATION, bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(head.headers()["content-length"], "3000");
    let record = log.next(&mut seen).await;
    assert_eq!(
        (record.method.as_str(), record.status, record.bytes_out),
        ("HEAD", Some(200), 0)
    );

    // (request, route template, drive named, status)
    let cases: [(reqwest::RequestBuilder, &str, Option<&str>, u16); 9] = [
        (
            client.get(format!("{}/healthz", server.url())),
            "/healthz",
            None,
            200,
        ),
        (
            client
                .get(format!("{}/v1/drives", server.url()))
                .header(AUTHORIZATION, bearer()),
            "/v1/drives",
            None,
            200,
        ),
        // A wrong token on a configured drive names the drive: it is the
        // operator's own id, not something the client supplied.
        (
            client
                .get(format!("{}/v1/drives/d/fs/x", server.url()))
                .header(AUTHORIZATION, "Bearer wrong-secret-token"),
            fs,
            Some("d"),
            401,
        ),
        // An unknown drive id is client text and is not repeated.
        (
            client
                .get(format!("{}/v1/drives/guessed-drive/entries", server.url()))
                .header(AUTHORIZATION, bearer()),
            "/v1/drives/{id}/entries/{path}",
            None,
            401,
        ),
        (
            client
                .get(format!("{}/v1/drives/d/stat/secret-name.txt", server.url()))
                .header(AUTHORIZATION, bearer()),
            "/v1/drives/{id}/stat/{path}",
            Some("d"),
            200,
        ),
        (
            client
                .post(format!("{}/v1/drives/d/ops/mkdir", server.url()))
                .header(AUTHORIZATION, bearer())
                .json(&json!({"path": "/private-dir"})),
            "/v1/drives/{id}/ops/mkdir",
            Some("d"),
            200,
        ),
        (
            client
                .post(format!("{}/v1/drives/d/ops/chown-everything", server.url()))
                .header(AUTHORIZATION, bearer()),
            "/v1/drives/{id}/ops/{operation}",
            Some("d"),
            404,
        ),
        (
            client.get(format!("{}/v2/secret-route", server.url())),
            "unmatched",
            None,
            404,
        ),
        (
            client
                .get(format!("{}/v1/drives/d/fs/a%2Fb", server.url()))
                .header(AUTHORIZATION, bearer()),
            "invalid",
            None,
            400,
        ),
    ];
    for (request, route, drive, status) in cases {
        let response = request.send().await.unwrap();
        assert_eq!(response.status().as_u16(), status, "{route}");
        let id = request_id_of(&response);
        let _ = response.bytes().await;
        let record = log.next(&mut seen).await;
        assert_eq!(record.request_id, id, "{route}");
        assert_eq!(record.route, route);
        assert_eq!(record.drive.as_deref(), drive, "{route}");
        assert_eq!(record.status, Some(status), "{route}");
    }

    server.close().await.expect("close");
    // Nothing a client put in a URL, a query or a header was recorded.
    let everything = format!("{:?}", log.0.lock().unwrap());
    for private in [
        "secret-name",
        "private-dir",
        "guessed-drive",
        "chown-everything",
        "secret-route",
        "download",
        TOKEN,
        "wrong-secret-token",
        "Bearer",
    ] {
        assert!(!everything.contains(private), "{private} was recorded");
    }
    assert_eq!(log.0.lock().unwrap().len(), seen, "one record per request");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_behind_a_handler_is_recorded_once() {
    let log = Arc::new(CollectingLog::default());
    let mut registry = DriveRegistry::new();
    registry
        .register(DriveConfig::new("d", Arc::new(MemoryFs::empty()), TOKEN).unwrap())
        .unwrap();
    // As the CLI does, both applications are given the same options.
    let handler = Arc::new(RecordsRequestId {
        application: HttpServer::unbound(registry, logged_options(&log)),
        seen: std::sync::Mutex::new(Vec::new()),
    });
    let server = HttpServer::start_with_handler(handler.clone(), logged_options(&log))
        .await
        .expect("HTTP server");
    for _ in 0..3 {
        let response = get(&server, "/v1/drives/d/entries").await;
        assert_eq!(response.status(), StatusCode::OK);
        let _ = response.bytes().await;
    }
    server.close().await.expect("close");
    handler.application.close().await.expect("close");
    let records = log.0.lock().unwrap().clone();
    assert_eq!(records.len(), 3, "{records:?}");
    for (record, ids) in records.iter().zip(handler.seen.lock().unwrap().iter()) {
        assert_eq!(ids, &vec![record.request_id.clone()]);
        assert_eq!(record.route, "/v1/drives/{id}/entries/{path}");
        assert_eq!(record.status, Some(200));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_whose_client_leaves_before_the_response_is_recorded_without_a_status() {
    let log = Arc::new(CollectingLog::default());
    let entered = Arc::new(tokio::sync::Notify::new());
    let driver = Arc::new(StuckFs {
        entered: entered.clone(),
    });
    let server = start(vec![("d", driver)], logged_options(&log)).await;
    let mut socket = TcpStream::connect(("127.0.0.1", server.port()))
        .await
        .unwrap();
    let head = format!(
        "GET /v1/drives/d/stat/x HTTP/1.1\r\nHost: h\r\nAuthorization: {}\r\nX-Request-Id: gone-1\r\n\r\n",
        bearer()
    );
    socket.write_all(head.as_bytes()).await.unwrap();
    // The request is with the drive, which will never answer it.
    entered.notified().await;
    drop(socket);
    let mut seen = 0;
    let record = log.next(&mut seen).await;
    assert_eq!(
        record,
        Logged {
            request_id: "gone-1".to_owned(),
            method: "GET".to_owned(),
            route: "/v1/drives/{id}/stat/{path}",
            drive: Some("d".to_owned()),
            status: None,
            bytes_in: 0,
            bytes_out: 0,
        }
    );
    server.close().await.expect("close");
}

// ------------------------------------------------------ listing pagination

async fn make_files(driver: &dyn FsDriver, directory: &str, names: &[String]) {
    driver
        .mkdir(directory, MkdirOptions::default())
        .await
        .expect("directory");
    for name in names {
        driver
            .write_file(&format!("{directory}/{name}"), b"x")
            .await
            .expect("file");
    }
}

/// One page: its entry names and its `next_cursor`, with the shape checked.
async fn page_of(server: &HttpServer, route: &str) -> (Vec<String>, Option<String>) {
    let response = get(server, route).await;
    assert_eq!(response.status(), StatusCode::OK, "{route}");
    let body: Value = response.json().await.expect("page JSON");
    let page = body.as_object().expect("a page is an object");
    let names = page["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| {
            assert_eq!(entry.as_object().expect("entry").len(), 3, "{entry}");
            entry["name"].as_str().expect("name").to_owned()
        })
        .collect();
    let cursor = match page.get("next_cursor") {
        Some(cursor) => {
            assert_eq!(page.len(), 2, "{body}");
            Some(cursor.as_str().expect("next_cursor is a string").to_owned())
        }
        None => {
            assert_eq!(page.len(), 1, "{body}");
            None
        }
    };
    (names, cursor)
}

/// Follow `next_cursor` from the first page to the last.
async fn walk(server: &HttpServer, route: &str, limit: usize) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let query = match &cursor {
            Some(cursor) => format!("{route}?limit={limit}&cursor={cursor}"),
            None => format!("{route}?limit={limit}"),
        };
        let (names, next) = page_of(server, &query).await;
        assert!(names.len() <= limit, "{query}");
        pages.push(names);
        match next {
            Some(next) => cursor = Some(next),
            None => return pages,
        }
        assert!(pages.len() < 10_000, "the walk does not end");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_listing_without_limit_is_the_same_array_whatever_else_the_query_holds() {
    for (kind, driver) in drive_kinds().await {
        let names: Vec<String> = (0..7).map(|index| format!("f{index}")).collect();
        make_files(driver.as_ref(), "/dir", &names).await;
        let server = start(vec![("d", driver)], HttpServerOptions::default()).await;
        let plain = get(&server, "/v1/drives/d/entries/dir").await;
        assert_eq!(plain.status(), StatusCode::OK);
        let plain = plain.bytes().await.unwrap();
        let listed: Value = serde_json::from_slice(&plain).unwrap();
        assert_eq!(listed.as_array().expect("an array").len(), 7, "{kind}");
        // Only `limit` asks for a page. Anything else in the query, a
        // cursor included, is ignored as the query string always was.
        for query in [
            "?",
            "?cursor=v1.6630",
            "?cursor=garbage",
            "?x=1&y",
            "?Limit=2",
        ] {
            let same = get(&server, &format!("/v1/drives/d/entries/dir{query}")).await;
            assert_eq!(same.status(), StatusCode::OK, "{kind} {query}");
            assert_eq!(same.bytes().await.unwrap(), plain, "{kind} {query}");
        }
        server.close().await.expect("close");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paginated_listing_returns_every_entry_once_in_name_order() {
    for (kind, driver) in drive_kinds().await {
        // Names whose byte order differs from their creation order.
        let mut names: Vec<String> = (0..25).rev().map(|index| format!("n{index}")).collect();
        names.push("caf\u{e9} au lait".to_owned());
        names.push("Upper".to_owned());
        make_files(driver.as_ref(), "/dir", &names).await;
        let server = start(vec![("d", driver)], HttpServerOptions::default()).await;
        let mut sorted = names.clone();
        sorted.sort_unstable();

        for limit in [1, 10, 27, 28, 100_000] {
            let pages = walk(&server, "/v1/drives/d/entries/dir", limit).await;
            for page in &pages[..pages.len() - 1] {
                assert_eq!(page.len(), limit, "{kind}: only the last page is short");
            }
            assert_eq!(pages.concat(), sorted, "{kind} limit {limit}");
        }
        // The root lists the same way, and HEAD announces the page's length.
        let (root, next) = page_of(&server, "/v1/drives/d/entries?limit=5").await;
        assert_eq!((root, next), (vec!["dir".to_owned()], None), "{kind}");
        let route = format!("{}/v1/drives/d/entries/dir?limit=3", server.url());
        let client = reqwest::Client::new();
        let head = client
            .head(&route)
            .header(AUTHORIZATION, bearer())
            .send()
            .await
            .unwrap();
        let body = client
            .get(&route)
            .header(AUTHORIZATION, bearer())
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        assert_eq!(head.status(), StatusCode::OK);
        assert_eq!(
            head.headers()["content-length"],
            body.len().to_string().as_str(),
            "{kind}"
        );
        server.close().await.expect("close");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn entries_that_do_not_change_during_a_paginated_walk_appear_exactly_once() {
    for (kind, driver) in drive_kinds().await {
        let names: Vec<String> = (10..40).map(|index| format!("e{index}")).collect();
        make_files(driver.as_ref(), "/dir", &names).await;
        let server = start(vec![("d", driver.clone())], HttpServerOptions::default()).await;
        let route = "/v1/drives/d/entries/dir";

        let (first, cursor) = page_of(&server, &format!("{route}?limit=10")).await;
        assert_eq!(first, names[..10]);
        let cursor = cursor.expect("more pages");
        // Between two pages: one listed entry and one unlisted entry go,
        // and entries arrive before and after the cursor's position.
        driver.unlink("/dir/e12").await.unwrap();
        driver.unlink("/dir/e25").await.unwrap();
        driver.write_file("/dir/a-early", b"x").await.unwrap();
        driver.write_file("/dir/e205", b"x").await.unwrap();
        // The cursor's own entry goes too: the cursor is a position, not a
        // reference to an entry.
        driver.unlink("/dir/e19").await.unwrap();

        let mut rest = Vec::new();
        let mut cursor = Some(cursor);
        while let Some(token) = cursor {
            let (names, next) = page_of(&server, &format!("{route}?limit=10&cursor={token}")).await;
            rest.extend(names);
            cursor = next;
        }
        let mut expected: Vec<String> = names[10..]
            .iter()
            .filter(|name| *name != "e25")
            .cloned()
            .collect();
        // Created after the cursor's position: seen. Created before it: not.
        expected.push("e205".to_owned());
        expected.sort_unstable();
        assert_eq!(rest, expected, "{kind}");
        let mut all = first;
        all.extend(rest);
        let unique: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "{kind}: a name was repeated");
        server.close().await.expect("close");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_listing_too_large_for_one_response_can_be_read_in_pages() {
    let driver: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let names: Vec<String> = (100..200).map(|index| format!("entry-{index}")).collect();
    make_files(driver.as_ref(), "/big", &names).await;
    let server = start(
        vec![("d", driver.clone())],
        HttpServerOptions {
            max_response_bytes: 2048,
            max_directory_entries: 100,
            ..HttpServerOptions::default()
        },
    )
    .await;
    let route = "/v1/drives/d/entries/big";

    // As before: the whole listing does not fit one response.
    let whole = get(&server, route).await;
    assert_eq!(whole.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_of(whole).await.0, "EOVERFLOW");
    // In pages it does.
    let pages = walk(&server, route, 20).await;
    assert_eq!(pages.len(), 5);
    assert_eq!(pages.concat(), names);
    // A page is held to the same response limit as a listing.
    let page = get(&server, &format!("{route}?limit=100")).await;
    assert_eq!(page.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_of(page).await.0, "EOVERFLOW");

    // The entry limit is the size of directory the server will enumerate,
    // and paging does not lift it.
    driver.write_file("/big/one-more", b"x").await.unwrap();
    let over = get(&server, &format!("{route}?limit=20")).await;
    assert_eq!(over.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error_of(over).await.0, "EOVERFLOW");
    server.close().await.expect("close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bad_limit_or_cursor_is_400_einval() {
    let driver: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    make_files(driver.as_ref(), "/dir", &["a".to_owned(), "b".to_owned()]).await;
    let server = start(
        vec![("d", driver)],
        HttpServerOptions {
            max_directory_entries: 50,
            ..HttpServerOptions::default()
        },
    )
    .await;
    let route = "/v1/drives/d/entries/dir";
    for query in [
        "limit=",
        "limit=0",
        "limit=-1",
        "limit=01",
        "limit=1.5",
        "limit=many",
        "limit=%35",
        // Above `max_directory_entries`.
        "limit=51",
        "limit=1&limit=2",
        "limit=1&cursor=",
        "limit=1&cursor=a",
        "limit=1&cursor=v1.",
        "limit=1&cursor=v1.zz",
        "limit=1&cursor=v1.ff",
        "limit=1&cursor=v2.61",
        "limit=1&cursor=v1.61&cursor=v1.62",
    ] {
        let response = get(&server, &format!("{route}?{query}")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{query}");
        assert!(response.headers().get("connection").is_none(), "{query}");
        assert_eq!(error_of(response).await.0, "EINVAL", "{query}");
    }
    // The largest limit is accepted, and a cursor past the end is an empty
    // last page rather than an error.
    let (names, next) = page_of(&server, &format!("{route}?limit=50")).await;
    assert_eq!((names.len(), next), (2, None));
    let (names, next) = page_of(&server, &format!("{route}?limit=1&cursor=v1.7a7a")).await;
    assert_eq!((names.len(), next), (0, None));

    // The checks keep their order: token before method, method before query.
    let client = reqwest::Client::new();
    let unauthorized = client
        .get(format!("{}{route}?limit=0", server.url()))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    let wrong_method = client
        .post(format!("{}{route}?limit=0", server.url()))
        .header(AUTHORIZATION, bearer())
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_method.status(), StatusCode::METHOD_NOT_ALLOWED);
    // And a missing directory is still a 404 with a valid page request.
    let missing = get(&server, "/v1/drives/d/entries/nowhere?limit=5").await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    server.close().await.expect("close");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_page_is_compressed_like_any_listing() {
    let driver: Arc<dyn FsDriver> = Arc::new(MemoryFs::empty());
    let names: Vec<String> = (0..200)
        .map(|index| format!("compressible-{index:04}"))
        .collect();
    make_files(driver.as_ref(), "/dir", &names).await;
    let server = start(vec![("d", driver)], HttpServerOptions::default()).await;
    let response = reqwest::Client::new()
        .get(format!(
            "{}/v1/drives/d/entries/dir?limit=150",
            server.url()
        ))
        .header(AUTHORIZATION, bearer())
        .header("accept-encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-encoding"], "gzip");
    assert_eq!(response.headers()["vary"], "Accept-Encoding");
    let compressed = response.bytes().await.unwrap();
    let mut json = Vec::new();
    std::io::Read::read_to_end(
        &mut flate2::read::GzDecoder::new(compressed.as_ref()),
        &mut json,
    )
    .unwrap();
    let page: Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(page["entries"].as_array().unwrap().len(), 150);
    assert!(page["next_cursor"].is_string());
    server.close().await.expect("close");
}
