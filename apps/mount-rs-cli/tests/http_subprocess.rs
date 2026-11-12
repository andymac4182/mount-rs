use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::time::Instant;

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

static TEMP_ROOT_COUNTER: AtomicU64 = AtomicU64::new(0);

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _value)| key.eq_ignore_ascii_case(name))
            .map(|(_key, value)| value.as_str())
    }
}

struct HttpChild {
    child: Child,
    stdout: Receiver<String>,
    stderr: Receiver<String>,
    readers: Vec<JoinHandle<()>>,
    root: PathBuf,
}

impl HttpChild {
    fn start(config: &Path, root: PathBuf) -> Self {
        Self::start_command(Command::new(env!("CARGO_BIN_EXE_mount-rs")), config, root)
    }

    fn start_command(mut command: Command, config: &Path, root: PathBuf) -> Self {
        let mut child = command
            .args(["serve-http", "--config"])
            .arg(config)
            .env("MOUNT_RS_HTTP_MEMORY_TOKEN", "memory-test-token")
            .env("MOUNT_RS_HTTP_SQLITE_TOKEN", "sqlite-test-token")
            .env("MOUNT_RS_HTTP_SPLIT_TOKEN", "splitstore-test-token")
            .env(
                "MOUNT_RS_HTTP_SESSION_TOKEN",
                "session-token-01234567890123456789",
            )
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn mount-rs HTTP subprocess");

        let stdout = child.stdout.take().expect("capture HTTP stdout");
        let stderr = child.stderr.take().expect("capture HTTP stderr");
        let (stdout_sender, stdout_receiver) = mpsc::channel();
        let (stderr_sender, stderr_receiver) = mpsc::channel();
        let stdout_reader = thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = stdout_sender.send(line);
            }
        });
        let stderr_reader = thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = stderr_sender.send(line);
            }
        });
        Self {
            child,
            stdout: stdout_receiver,
            stderr: stderr_receiver,
            readers: vec![stdout_reader, stderr_reader],
            root,
        }
    }

    fn ready(&self) -> (SocketAddr, String) {
        // A freshly linked CLI can take longer to start on hosted macOS.
        // Keep the subprocess startup bound separate from request deadlines.
        self.ready_with_timeout(Duration::from_secs(30))
    }

    fn ready_with_timeout(&self, wait: Duration) -> (SocketAddr, String) {
        let line = self.stdout.recv_timeout(wait).unwrap_or_else(|error| {
            let stderr = self.stderr.try_iter().collect::<Vec<_>>();
            panic!("HTTP subprocess did not announce readiness: {error}; stderr={stderr:?}");
        });
        let url = line
            .split_whitespace()
            .find(|part| part.starts_with("http://"))
            .expect("HTTP readiness line did not contain a URL");
        let address = url
            .strip_prefix("http://")
            .expect("HTTP URL scheme")
            .parse()
            .expect("HTTP readiness URL address");
        (address, line)
    }

    fn kill_owned(&mut self) -> ExitStatus {
        // This is intentionally a forced termination. The portable test only
        // relies on wait proving that its owned child is gone; it makes no
        // graceful-shutdown claim on platforms without a portable SIGINT API.
        self.child.kill().expect("kill HTTP subprocess");
        self.child.wait().expect("wait for killed HTTP subprocess")
    }

    #[cfg(unix)]
    fn signal(&mut self, signal: libc::c_int) {
        // SAFETY: the child process was spawned by this test and its PID is
        // not reused while it is still running.
        let result = unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
        assert_eq!(result, 0, "signal HTTP subprocess");
    }

    #[cfg(unix)]
    fn wait_for_exit(&mut self, timeout: Duration) -> ExitStatus {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll HTTP subprocess") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "HTTP subprocess did not shut down within {timeout:?}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(unix)]
    fn output(&mut self) -> (Vec<String>, Vec<String>) {
        for reader in self.readers.drain(..) {
            reader.join().expect("join HTTP output reader");
        }
        (
            self.stdout.try_iter().collect(),
            self.stderr.try_iter().collect(),
        )
    }
}

impl Drop for HttpChild {
    fn drop(&mut self) {
        if self
            .child
            .try_wait()
            .expect("poll HTTP subprocess in cleanup")
            .is_none()
        {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: &[u8],
) -> HttpResponse {
    request_streaming(address, method, path, token, body, None, body.len().max(1))
}

fn request_with_range(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: &[u8],
    range: &str,
) -> HttpResponse {
    request_streaming(
        address,
        method,
        path,
        token,
        body,
        Some(range),
        body.len().max(1),
    )
}

fn request_streaming(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: &[u8],
    range: Option<&str>,
    write_chunk_bytes: usize,
) -> HttpResponse {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .expect("connect HTTP subprocess");
    stream
        // Generous: this bounds a hung server, not a slow disk.
        .set_read_timeout(Some(Duration::from_secs(15)))
        .expect("set HTTP response timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("set HTTP request timeout");
    let range_header = range
        .map(|value| format!("Range: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\nContent-Length: {}\r\n{range_header}\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("write HTTP request headers");
    for chunk in body.chunks(write_chunk_bytes.max(1)) {
        stream
            .write_all(chunk)
            .expect("write HTTP request body chunk");
        thread::yield_now();
    }

    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).expect("read HTTP response");
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response headers");
    let headers = std::str::from_utf8(&bytes[..header_end]).expect("HTTP response text");
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .expect("HTTP response status");
    let headers = headers
        .lines()
        .skip(1)
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    HttpResponse {
        status,
        headers,
        body: bytes[header_end + 4..].to_vec(),
    }
}

fn temporary_root() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let sequence = TEMP_ROOT_COUNTER.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "mount-rs-cli-http-{}-{nanos}-{sequence}",
        std::process::id()
    ));
    fs::create_dir(&root).expect("create HTTP test directory");
    root
}

fn effective_identity() -> (u32, u32) {
    let uid = std::env::var("SUDO_UID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(current_uid);
    let gid = std::env::var("SUDO_GID")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or_else(current_gid);
    (uid, gid)
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: getuid has no pointer arguments or retained state.
    unsafe { libc::getuid() as u32 }
}

#[cfg(not(unix))]
const fn current_uid() -> u32 {
    0
}

#[cfg(unix)]
fn current_gid() -> u32 {
    // SAFETY: getgid has no pointer arguments or retained state.
    unsafe { libc::getgid() as u32 }
}

#[cfg(not(unix))]
const fn current_gid() -> u32 {
    0
}

#[test]
fn later_invalid_drive_token_fails_before_opening_earlier_database() {
    let root = temporary_root();
    let config = write_http_config(&root);
    for token in [None, Some("invalid token")] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mount-rs"));
        command
            .args(["serve-http", "--config"])
            .arg(&config)
            .env("MOUNT_RS_HTTP_MEMORY_TOKEN", "memory-test-token")
            .env("MOUNT_RS_HTTP_SQLITE_TOKEN", "sqlite-test-token")
            .env_remove("MOUNT_RS_HTTP_SPLIT_TOKEN");
        if let Some(token) = token {
            command.env("MOUNT_RS_HTTP_SPLIT_TOKEN", token);
        }
        let output = command.output().unwrap();
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains(if token.is_some() {
                "invalid HTTP drive 'splitstore'"
            } else {
                "missing environment variable MOUNT_RS_HTTP_SPLIT_TOKEN"
            }),
            "{stderr}"
        );
        assert!(!root.join("sqlite.db").exists());
    }
    fs::remove_dir_all(root).unwrap();
}

fn write_http_config(root: &Path) -> PathBuf {
    let config_path = root.join("http.json");
    let database_path = root.join("sqlite.db");
    let (uid, gid) = effective_identity();
    let config = serde_json::json!({
        "version": 1,
        "http": {
            "host": "127.0.0.1",
            "port": 0,
            "max_request_bytes": 1024,
            "read_chunk_bytes": 128,
            "drain_timeout_ms": 1000,
            "drives": [
                {
                    "id": "memory",
                    "token": {"env": "MOUNT_RS_HTTP_MEMORY_TOKEN"},
                    "driver": {"kind": "memory"}
                },
                {
                    "id": "sqlite",
                    "token": {"env": "MOUNT_RS_HTTP_SQLITE_TOKEN"},
                    "driver": {
                        "kind": "sqlite",
                        "database": database_path,
                        "uid": uid,
                        "gid": gid
                    }
                },
                {
                    "id": "splitstore",
                    "token": {"env": "MOUNT_RS_HTTP_SPLIT_TOKEN"},
                    "driver": {
                        "kind": "splitstore",
                        "storage": {
                            "metadata": {
                                "kind": "memory"
                            },
                            "blocks": {
                                "kind": "memory"
                            },
                            "chunk_size_bytes": 128,
                            "owner": "http-subprocess-test"
                        }
                    }
                }
            ]
        }
    });
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize HTTP config"),
    )
    .expect("write HTTP config");
    config_path
}

fn abort_streaming_write(address: SocketAddr, path: &str, token: &str) {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .expect("connect HTTP subprocess for aborted write");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("set aborted-write timeout");
    let request = format!(
        "PUT {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\nContent-Length: 512\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write aborted HTTP request headers");
    stream
        .write_all(b"incomplete body")
        .expect("write aborted HTTP request body");
    // Dropping before Content-Length bytes arrive forces the server down its
    // request-error path, which must still close the splitstore handle.
}

fn assert_listener_closed(address: SocketAddr) {
    for _ in 0..20 {
        if TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_err() {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("HTTP listener remained reachable after its child exited: {address}");
}

#[test]
fn config_driven_http_subprocess_isolates_drives_and_reopens_sqlite() {
    let root = temporary_root();
    let config_path = write_http_config(&root);
    let mut child = HttpChild::start(&config_path, root.clone());
    let (address, ready_line) = child.ready();
    assert!(ready_line.contains("drives: memory, sqlite, splitstore"));

    let memory_discovery = request(address, "GET", "/v1/drives", "memory-test-token", &[]);
    assert_eq!(memory_discovery.status, 200);
    let memory_discovery = String::from_utf8(memory_discovery.body).expect("memory discovery");
    assert!(memory_discovery.contains("\"id\":\"memory\""));
    assert!(!memory_discovery.contains("sqlite"));

    let sqlite_discovery = request(address, "GET", "/v1/drives", "sqlite-test-token", &[]);
    assert_eq!(sqlite_discovery.status, 200);
    let sqlite_discovery = String::from_utf8(sqlite_discovery.body).expect("SQLite discovery");
    assert!(sqlite_discovery.contains("\"id\":\"sqlite\""));
    assert!(!sqlite_discovery.contains("memory"));

    let memory_payload: Vec<u8> = (0..777)
        .map(|index| ((index * 37 + 11) % 251) as u8)
        .collect();
    let memory_write = request_streaming(
        address,
        "PUT",
        "/v1/drives/memory/fs/memory-only",
        "memory-test-token",
        &memory_payload,
        None,
        17,
    );
    assert!((200..300).contains(&memory_write.status));
    let memory_write_payload: serde_json::Value =
        serde_json::from_slice(&memory_write.body).expect("memory PUT JSON");
    assert_eq!(memory_write_payload["bytes"].as_u64(), Some(777));
    let sqlite_write = request(
        address,
        "PUT",
        "/v1/drives/sqlite/fs/sqlite-only",
        "sqlite-test-token",
        b"sqlite bytes",
    );
    assert!((200..300).contains(&sqlite_write.status));

    let memory_read = request(
        address,
        "GET",
        "/v1/drives/memory/fs/memory-only",
        "memory-test-token",
        &[],
    );
    assert_eq!(memory_read.status, 200);
    assert_eq!(memory_read.body, memory_payload);
    let ranged_memory = request_with_range(
        address,
        "GET",
        "/v1/drives/memory/fs/memory-only",
        "memory-test-token",
        &[],
        "bytes=123-456",
    );
    assert_eq!(ranged_memory.status, 206);
    assert_eq!(
        ranged_memory.header("content-range"),
        Some("bytes 123-456/777")
    );
    assert_eq!(ranged_memory.header("content-length"), Some("334"));
    assert_eq!(ranged_memory.body, memory_payload[123..=456]);

    let truncate_body = serde_json::to_vec(&serde_json::json!({
        "path": "/memory-only",
        "length": 321,
    }))
    .expect("serialize truncate request");
    let truncated = request(
        address,
        "POST",
        "/v1/drives/memory/ops/truncate",
        "memory-test-token",
        &truncate_body,
    );
    assert_eq!(truncated.status, 200);
    let truncated_read = request(
        address,
        "GET",
        "/v1/drives/memory/fs/memory-only",
        "memory-test-token",
        &[],
    );
    assert_eq!(truncated_read.status, 200);
    assert_eq!(truncated_read.body, memory_payload[..321]);

    let concurrent_a = std::thread::spawn(move || {
        request(
            address,
            "PUT",
            "/v1/drives/memory/fs/concurrent-a",
            "memory-test-token",
            b"concurrent A",
        )
    });
    let concurrent_b = std::thread::spawn(move || {
        request(
            address,
            "PUT",
            "/v1/drives/memory/fs/concurrent-b",
            "memory-test-token",
            b"concurrent B",
        )
    });
    assert_eq!(concurrent_a.join().expect("concurrent PUT A").status, 200);
    assert_eq!(concurrent_b.join().expect("concurrent PUT B").status, 200);
    for (path, expected) in [
        (
            "/v1/drives/memory/fs/concurrent-a",
            b"concurrent A".as_slice(),
        ),
        (
            "/v1/drives/memory/fs/concurrent-b",
            b"concurrent B".as_slice(),
        ),
    ] {
        let response = request(address, "GET", path, "memory-test-token", &[]);
        assert_eq!(response.status, 200);
        assert_eq!(response.body, expected);
    }
    let sqlite_read = request(
        address,
        "GET",
        "/v1/drives/sqlite/fs/sqlite-only",
        "sqlite-test-token",
        &[],
    );
    assert_eq!(sqlite_read.status, 200);
    assert_eq!(sqlite_read.body, b"sqlite bytes");

    let split_write = request(
        address,
        "PUT",
        "/v1/drives/splitstore/fs/split-only",
        "splitstore-test-token",
        b"splitstore bytes",
    );
    assert!((200..300).contains(&split_write.status));
    let split_read = request(
        address,
        "GET",
        "/v1/drives/splitstore/fs/split-only",
        "splitstore-test-token",
        &[],
    );
    assert_eq!(split_read.status, 200);
    assert_eq!(split_read.body, b"splitstore bytes");

    let cross_drive = request(
        address,
        "GET",
        "/v1/drives/sqlite/fs/memory-only",
        "sqlite-test-token",
        &[],
    );
    assert_eq!(cross_drive.status, 404);
    let cross_token = request(
        address,
        "GET",
        "/v1/drives/sqlite/fs/sqlite-only",
        "memory-test-token",
        &[],
    );
    assert_eq!(cross_token.status, 401);

    abort_streaming_write(
        address,
        "/v1/drives/splitstore/fs/aborted-write",
        "splitstore-test-token",
    );
    let recovered_write = (0..20).find_map(|_| {
        let response = request(
            address,
            "PUT",
            "/v1/drives/splitstore/fs/aborted-write",
            "splitstore-test-token",
            b"recovered after aborted write",
        );
        if response.status == 200 {
            Some(response)
        } else {
            thread::sleep(Duration::from_millis(10));
            None
        }
    });
    assert!(
        recovered_write.is_some(),
        "splitstore handle was not cleaned up"
    );

    let _forced_status = child.kill_owned();
    assert_listener_closed(address);
    let mut reopened = HttpChild::start(&config_path, root.clone());
    let (reopened_address, reopened_line) = reopened.ready();
    assert!(reopened_line.contains("drives: memory, sqlite, splitstore"));

    let persisted_sqlite = request(
        reopened_address,
        "GET",
        "/v1/drives/sqlite/fs/sqlite-only",
        "sqlite-test-token",
        &[],
    );
    assert_eq!(persisted_sqlite.status, 200);
    assert_eq!(persisted_sqlite.body, b"sqlite bytes");
    let missing_memory = request(
        reopened_address,
        "GET",
        "/v1/drives/memory/fs/memory-only",
        "memory-test-token",
        &[],
    );
    assert_eq!(missing_memory.status, 404);
    let missing_splitstore = request(
        reopened_address,
        "GET",
        "/v1/drives/splitstore/fs/split-only",
        "splitstore-test-token",
        &[],
    );
    assert_eq!(missing_splitstore.status, 404);
    let _forced_reopened_status = reopened.kill_owned();
}

#[cfg(unix)]
#[test]
fn unix_http_subprocess_stops_gracefully_and_boundedly() {
    let root = temporary_root();
    let config_path = write_http_config(&root);
    let mut child = HttpChild::start(&config_path, root);
    let (address, ready_line) = child.ready();
    assert!(ready_line.contains("drives: memory, sqlite, splitstore"));

    let memory_discovery = request(address, "GET", "/v1/drives", "memory-test-token", &[]);
    assert_eq!(memory_discovery.status, 200);
    let memory_discovery = String::from_utf8(memory_discovery.body).expect("memory discovery");
    assert!(memory_discovery.contains("\"id\":\"memory\""));
    assert!(!memory_discovery.contains("sqlite"));

    let shutdown_started = Instant::now();
    child.signal(libc::SIGINT);
    let status = child.wait_for_exit(Duration::from_secs(3));
    assert!(
        status.success(),
        "HTTP subprocess exited unsuccessfully: {status}"
    );
    assert!(shutdown_started.elapsed() < Duration::from_secs(3));
    let (stdout, stderr) = child.output();
    assert!(
        stdout.iter().any(|line| line == "http stopped"),
        "HTTP subprocess did not report bounded shutdown: stdout={stdout:?}, stderr={stderr:?}"
    );
}

/// A config serving one SQLite-backed split-store drive, `durable`.
fn write_sqlite_split_config(
    root: &Path,
    name: &str,
    owner: &str,
    lease_ttl_ms: u64,
    http: serde_json::Value,
) -> PathBuf {
    let config_path = root.join(name);
    let mut http = http;
    http["host"] = "127.0.0.1".into();
    http["port"] = 0.into();
    http["drives"] = serde_json::json!([{
        "id": "durable",
        "token": {"env": "MOUNT_RS_HTTP_SPLIT_TOKEN"},
        "driver": {
            "kind": "splitstore",
            "storage": {
                "metadata": {"kind": "sqlite", "path": root.join("metadata.db")},
                "blocks": {"kind": "sqlite", "path": root.join("blocks.db")},
                "chunk_size_bytes": 128,
                "owner": owner,
                "lease_ttl_ms": lease_ttl_ms
            }
        }
    }]);
    let config = serde_json::json!({"version": 1, "http": http});
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize HTTP config"),
    )
    .expect("write HTTP config");
    config_path
}

/// Send a `PUT` declaring `declared` bytes and deliver only the first
/// `sent`; the caller keeps the connection, and with it the request, open.
fn stalled_put(address: SocketAddr, path: &str, declared: usize, sent: usize) -> TcpStream {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .expect("connect HTTP subprocess for a stalled write");
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .expect("set stalled-write timeout");
    let request = format!(
        "PUT {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer splitstore-test-token\r\nConnection: close\r\nContent-Length: {declared}\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .expect("write stalled HTTP request headers");
    stream
        .write_all(&vec![b'p'; sent])
        .expect("write stalled HTTP request body");
    stream.flush().expect("flush stalled HTTP request");
    stream
}

/// Send a request the way the API contract tells a client to: a 503 that
/// carries `Retry-After` means the drive is briefly busy, so send it again.
/// The two-second lease this test uses makes that likely on a slow machine.
/// Any other answer, including a 503 without `Retry-After`, is returned.
fn request_when_not_busy(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: &[u8],
) -> HttpResponse {
    for _ in 0..20 {
        let response = request(address, method, path, token, body);
        if response.status != 503 || response.header("retry-after").is_none() {
            return response;
        }
        thread::sleep(Duration::from_millis(250));
    }
    request(address, method, path, token, body)
}

#[test]
fn an_acknowledged_upload_survives_the_process_being_killed() {
    let root = temporary_root();
    // A short lease: the killed process cannot release it, and the next
    // one has to wait it out. Not shorter than this: at two seconds a slow
    // machine misses renewals, and requests then stall or are answered 503
    // while the process waits out its own lease.
    let config_path = write_sqlite_split_config(
        &root,
        "http.json",
        "http-kill-test",
        8_000,
        serde_json::json!({"max_request_bytes": 4096}),
    );
    let mut child = HttpChild::start(&config_path, root.clone());
    let (address, _ready) = child.ready();

    let token = "splitstore-test-token";
    // Several chunks each, so a prefix would be recognisable.
    let first: Vec<u8> = (0..1500).map(|index| (index % 251) as u8).collect();
    let second: Vec<u8> = (0..3000).map(|index| (index % 239) as u8).collect();
    for (path, body) in [
        ("/v1/drives/durable/fs/replaced", &first),
        ("/v1/drives/durable/fs/replaced", &second),
        ("/v1/drives/durable/fs/created", &first),
        ("/v1/drives/durable/fs/interrupted", &first),
    ] {
        let response = request_when_not_busy(address, "PUT", path, token, body);
        assert_eq!(
            response.status,
            200,
            "{path}: {}",
            String::from_utf8_lossy(&response.body)
        );
    }
    // One more upload is part-way through when the process dies.
    let interrupted = stalled_put(address, "/v1/drives/durable/fs/interrupted", 4000, 2000);
    let never_created = stalled_put(address, "/v1/drives/durable/fs/never-created", 4000, 2000);
    thread::sleep(Duration::from_millis(200));

    // SIGKILL: nothing is flushed, drained or released on the way out.
    let _status = child.kill_owned();
    drop((interrupted, never_created));
    assert_listener_closed(address);

    // The killed process still holds the writer lease, and until that runs
    // out a replacement is refused at startup (EAGAIN) rather than made to
    // wait: restart it, as a supervisor would.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let (_reopened, address) = loop {
        let mut candidate = HttpChild::start(&config_path, root.clone());
        match candidate.stdout.recv_timeout(Duration::from_secs(30)) {
            Ok(line) => {
                let address = line
                    .split_whitespace()
                    .find_map(|part| part.strip_prefix("http://"))
                    .expect("readiness URL")
                    .parse::<SocketAddr>()
                    .expect("readiness address");
                break (candidate, address);
            }
            Err(_) => {
                let _ = candidate.child.wait();
                let stderr = candidate.stderr.try_iter().collect::<Vec<_>>();
                assert!(
                    stderr.iter().any(|line| line.contains("EAGAIN")),
                    "restart failed for another reason: {stderr:?}"
                );
                assert!(
                    std::time::Instant::now() < deadline,
                    "the killed process's lease never expired: {stderr:?}"
                );
                // Not `drop`: that would remove the volume with the child.
                std::mem::forget(candidate);
                thread::sleep(Duration::from_millis(250));
            }
        }
    };
    for (path, body) in [
        // Every write that was answered 200 is there, whole.
        ("/v1/drives/durable/fs/replaced", &second),
        ("/v1/drives/durable/fs/created", &first),
        // The write that was not answered left the previous content alone.
        ("/v1/drives/durable/fs/interrupted", &first),
    ] {
        let response = request_when_not_busy(address, "GET", path, token, &[]);
        assert_eq!(
            response.status,
            200,
            "{path}: {}",
            String::from_utf8_lossy(&response.body)
        );
        assert!(
            response.body == **body,
            "{path}: {} bytes",
            response.body.len()
        );
    }
    let absent = request(
        address,
        "GET",
        "/v1/drives/durable/fs/never-created",
        token,
        &[],
    );
    assert_eq!(absent.status, 404);
    let listing = request(address, "GET", "/v1/drives/durable/entries/", token, &[]);
    assert_eq!(listing.status, 200);
    let listing: serde_json::Value = serde_json::from_slice(&listing.body).expect("listing");
    let mut names: Vec<_> = listing
        .as_array()
        .expect("entries")
        .iter()
        .map(|entry| entry["name"].as_str().expect("name").to_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["created", "interrupted", "replaced"]);
}

#[cfg(unix)]
#[test]
fn a_stop_that_runs_out_of_drain_time_still_releases_the_writer_lease() {
    let root = temporary_root();
    // A lease long enough that a process which exits without releasing it
    // keeps every other owner out for the rest of this test.
    let lease_ttl_ms = 120_000;
    let http = serde_json::json!({
        "max_request_bytes": 4096,
        "drain_timeout_ms": 300,
        "request_timeout_ms": 60_000
    });
    let first_config = write_sqlite_split_config(
        &root,
        "first.json",
        "http-first-owner",
        lease_ttl_ms,
        http.clone(),
    );
    let second_config = write_sqlite_split_config(
        &root,
        "second.json",
        "http-second-owner",
        lease_ttl_ms,
        http,
    );

    let mut first = HttpChild::start(&first_config, root.clone());
    let (address, _ready) = first.ready();
    let token = "splitstore-test-token";
    let kept = request(
        address,
        "PUT",
        "/v1/drives/durable/fs/kept",
        token,
        b"kept bytes",
    );
    assert_eq!(kept.status, 200);
    // An upload in progress, which a stop answers at once, and a request
    // whose head never completes, which a graceful stop has to wait for:
    // that one outlasts the drain timeout.
    let stalled = stalled_put(address, "/v1/drives/durable/fs/kept", 4000, 100);
    let mut unfinished = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .expect("connect HTTP subprocess for an unfinished request");
    unfinished
        .write_all(b"GET /v1/drives/durable/fs/kept HTTP/1.1\r\nHost: unfinished\r\n")
        .expect("write an unfinished request head");
    // Long enough for the server to have started reading that head; a
    // connection it has read nothing from is simply closed by a stop.
    thread::sleep(Duration::from_millis(500));

    let stopping = Instant::now();
    first.signal(libc::SIGTERM);
    let status = first.wait_for_exit(Duration::from_secs(10));
    assert!(
        stopping.elapsed() < Duration::from_secs(10),
        "the stop was not bounded by the drain timeout"
    );
    // The drain failed, and the exit status says so.
    assert_eq!(status.code(), Some(1), "exit status: {status}");
    let (stdout, stderr) = first.output();
    assert!(
        !stdout.iter().any(|line| line == "http stopped"),
        "stdout={stdout:?}"
    );
    assert!(
        stderr.iter().any(|line| line.contains("timed out")),
        "stderr={stderr:?}"
    );
    drop((stalled, unfinished));

    // Another owner gets the volume at once, not after the lease expires.
    let waiting = Instant::now();
    let second = HttpChild::start(&second_config, root.clone());
    let (address, _ready) = second.ready_with_timeout(Duration::from_secs(30));
    assert!(
        waiting.elapsed() < Duration::from_millis(lease_ttl_ms / 2),
        "the second owner waited {:?} for the volume",
        waiting.elapsed()
    );
    // And finds the interrupted upload's target as it was.
    let read = request(address, "GET", "/v1/drives/durable/fs/kept", token, &[]);
    assert_eq!(read.status, 200);
    assert_eq!(read.body, b"kept bytes");
    let written = request(
        address,
        "PUT",
        "/v1/drives/durable/fs/kept",
        token,
        b"second owner",
    );
    assert_eq!(written.status, 200);
}

#[cfg(unix)]
async fn run_configured_session_case(tidb: bool, rustfs: bool, crash: bool) {
    use mount_rs_core::FsDriver;
    let root = temporary_root();
    let config = root.join("sessions.json");
    let key = format!(
        "cli-sessions-{}",
        root.file_name().unwrap().to_string_lossy()
    );
    let pool = if tidb {
        use mysql_async::prelude::Queryable;
        let url = std::env::var("MOUNT_RS_TIDB_URL").expect("isolated TiDB fixture required");
        let pool = mysql_async::Pool::from_url(&url).expect("TiDB fixture URL");
        let mut connection = pool.get_conn().await.expect("TiDB fixture connection");
        let identity: Option<(String, String)> = connection
            .query_first("SELECT tidb_version(), VERSION()")
            .await
            .expect("real TiDB identity");
        assert!(identity.is_some_and(|(a, b)| format!("{a} {b}").to_lowercase().contains("tidb")));
        drop(connection);
        Some(pool)
    } else {
        None
    };
    let metadata = if tidb {
        serde_json::json!({"kind":"tidb", "connection":{"env":"MOUNT_RS_TIDB_URL"}, "volume_key":key, "durable":false})
    } else {
        serde_json::json!({"kind":"memory"})
    };
    let blocks = if rustfs {
        assert!(tidb, "RustFS restart coverage requires persistent metadata");
        let required = |name: &str| std::env::var(name).expect("isolated RustFS fixture required");
        assert!(std::path::Path::new(&required("RUSTFS_RUN_DIR")).is_dir());
        let endpoint = required("R2_ENDPOINT");
        assert!(endpoint.starts_with("http://127.0.0.1:"));
        serde_json::json!({
            "kind":"r2", "endpoint":endpoint, "bucket":required("R2_BUCKET"),
            "prefix":format!("{}/{}", required("RUSTFS_COMBO_PREFIX"), key),
            "access_key_id":{"env":"R2_ACCESS_KEY_ID"},
            "secret_access_key":{"env":"R2_SECRET_ACCESS_KEY"}, "durable":false
        })
    } else if tidb {
        fs::create_dir(root.join("blocks")).expect("create isolated local block directory");
        serde_json::json!({"kind":"local", "path":root.join("blocks")})
    } else {
        serde_json::json!({"kind":"memory"})
    };
    let timeout = Duration::from_secs(if tidb { 30 } else { 2 });
    fs::write(
        &config,
        serde_json::to_vec(&serde_json::json!({
            "version": 1,
            "http": {
                "remote_sessions": true,
            "max_request_bytes": 512,
                "drives": [{
                    "id": "volume", "token": {"env": "MOUNT_RS_HTTP_SESSION_TOKEN"},
                    "driver": {"kind": "splitstore", "storage": {
                        "metadata": metadata, "blocks": blocks,
                        "verify_backing_blocks": tidb,
                        "chunk_size_bytes": 4096,
                        "lease_ttl_ms": if crash { 10000 } else { 240000 },
                        "read_lease_cache_window_ms": if crash { 0 } else { 60000 }
                    }}
                }]
            }
        }))
        .unwrap(),
    )
    .unwrap();
    if let Some(pool) = pool.as_ref().filter(|_| !crash) {
        use mysql_async::prelude::Queryable;
        let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base: serde_json::Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        let mut occupied_port = base.clone();
        occupied_port["http"]["host"] = "127.0.0.1".into();
        occupied_port["http"]["port"] = occupied.local_addr().unwrap().port().into();
        let mut two_drives = base;
        let mut missing = two_drives["http"]["drives"][0].clone();
        missing["id"] = "missing-block-store".into();
        missing["driver"]["storage"]["metadata"] = serde_json::json!({"kind":"memory"});
        missing["driver"]["storage"]["blocks"] = serde_json::json!({
            "kind":"local", "path":root.join("unprovisioned")
        });
        two_drives["http"]["drives"]
            .as_array_mut()
            .unwrap()
            .push(missing);
        let mut previous_fence = 0;
        for (scenario, unavailable) in [
            ("later-volume", two_drives),
            ("occupied-port", occupied_port),
        ] {
            let path = root.join(format!("{scenario}.json"));
            fs::write(&path, serde_json::to_vec(&unavailable).unwrap()).unwrap();
            let mut failed = HttpChild::start(&path, temporary_root());
            assert!(!failed.wait_for_exit(timeout).success());
            let (stdout, stderr) = failed.output();
            assert!(!stdout.iter().any(|line| line.contains("http listening")));
            if scenario == "later-volume" {
                assert!(stderr.join(" ").contains("open local blocks"), "{stderr:?}");
            }
            let (released, fence): (i64, i64) = pool
                .get_conn()
                .await
                .unwrap()
                .exec_first(
                    "SELECT owner IS NULL, fence FROM mount_rs_tidb_metadata WHERE volume_key=?",
                    (&key,),
                )
                .await
                .unwrap()
                .expect("first drive was initialized");
            assert_eq!(released, 1, "{scenario} retained writer lease");
            assert!(
                fence > previous_fence,
                "{scenario} never acquired a new lease"
            );
            previous_fence = fence;
        }
        // Hold the second volume's row so the signal arrives after the first lease
        // is visible but before startup can finish opening both volumes.
        let mut blocked: serde_json::Value =
            serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        let first_key = format!("{key}-startup");
        let mut first_drive = blocked["http"]["drives"][0].clone();
        first_drive["id"] = "first".into();
        first_drive["driver"]["storage"]["metadata"]["volume_key"] = first_key.clone().into();
        first_drive["driver"]["storage"]["blocks"] = serde_json::json!({"kind":"memory"});
        blocked["http"]["drives"]
            .as_array_mut()
            .unwrap()
            .insert(0, first_drive);
        let path = root.join("interrupted-startup.json");
        fs::write(&path, serde_json::to_vec(&blocked).unwrap()).unwrap();
        let mut lock = pool.get_conn().await.unwrap();
        lock.query_drop("BEGIN PESSIMISTIC").await.unwrap();
        lock.exec_drop(
            "SELECT fence FROM mount_rs_tidb_metadata WHERE volume_key=? FOR UPDATE",
            (&key,),
        )
        .await
        .unwrap();
        let mut starting = HttpChild::start(&path, temporary_root());
        let mut observer = pool.get_conn().await.unwrap();
        let deadline = Instant::now() + timeout;
        loop {
            let owned: Option<i64> = observer
                .exec_first(
                    "SELECT owner IS NOT NULL FROM mount_rs_tidb_metadata WHERE volume_key=?",
                    (&first_key,),
                )
                .await
                .unwrap();
            if owned == Some(1) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "first startup lease was never acquired"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        starting.signal(libc::SIGTERM);
        lock.query_drop("ROLLBACK").await.unwrap();
        assert!(!starting.wait_for_exit(timeout).success());
        let (stdout, stderr) = starting.output();
        assert!(!stdout.iter().any(|line| line.contains("http listening")));
        assert!(
            stderr
                .iter()
                .any(|line| line.contains("shutdown signal during HTTP startup")),
            "{stderr:?}"
        );
        let released: Option<i64> = observer.exec_first("SELECT COUNT(*) FROM mount_rs_tidb_metadata WHERE volume_key IN (?,?) AND owner IS NULL", (&first_key, &key)).await.unwrap();
        assert_eq!(released, Some(2));
        observer
            .exec_drop(
                "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&first_key,),
            )
            .await
            .unwrap();
        // Restrict only the new server process. The parent keeps its normal socket budget.
        let mut limited = Command::new("sh");
        limited.args([
            "-c",
            r#"ulimit -n 64 && exec "$@""#,
            "limited-mount",
            env!("CARGO_BIN_EXE_mount-rs"),
        ]);
        let mut exhausted = HttpChild::start_command(limited, &config, temporary_root());
        let (address, _) = exhausted.ready_with_timeout(timeout);
        let mut sockets = Vec::new();
        for _ in 0..80 {
            match std::net::TcpStream::connect_timeout(&address, Duration::from_millis(100)) {
                Ok(socket) => sockets.push(socket),
                Err(_) => break,
            }
        }
        // Keep accepted sockets occupied until the accept task encounters its limit.
        let deadline = Instant::now() + Duration::from_secs(3);
        while std::net::TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_ok() {
            assert!(Instant::now() < deadline, "limited listener kept accepting");
            thread::sleep(Duration::from_millis(10));
        }
        drop(sockets);
        assert!(!exhausted.wait_for_exit(timeout).success());
        let (_, stderr) = exhausted.output();
        assert!(
            stderr
                .iter()
                .any(|line| line.contains("HTTP listener exited unexpectedly")),
            "{stderr:?}"
        );
        let (released, fence): (i64, i64) = pool
            .get_conn()
            .await
            .unwrap()
            .exec_first(
                "SELECT owner IS NULL, fence FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&key,),
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(released, 1, "failed accept retained writer lease");
        assert!(fence > previous_fence);
    }
    let check_failed_put = tidb && !rustfs && !crash;
    if check_failed_put {
        fs::create_dir(root.join("cache")).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        let storage = &mut value["http"]["drives"][0]["driver"]["storage"];
        storage["block_cache"] = serde_json::json!({"max_bytes": 65536, "max_entries": 128});
        storage["disk_cache"] = serde_json::json!({
            "root": root.join("cache"), "scope": key, "max_bytes": 65536, "max_entries": 128
        });
        fs::write(&config, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    let mut child = HttpChild::start(&config, root);
    let (address, _) = child.ready_with_timeout(Duration::from_secs(if tidb { 30 } else { 5 }));
    let endpoint = format!("http://{address}");
    let token = "session-token-01234567890123456789";
    assert!(
        mount_rs_remote::RemoteFs::connect(&endpoint, "volume", &"x".repeat(32), timeout)
            .await
            .is_err()
    );
    let first = mount_rs_remote::RemoteFs::connect(&endpoint, "volume", token, timeout)
        .await
        .unwrap();
    let second = mount_rs_remote::RemoteFs::connect(&endpoint, "volume", token, timeout)
        .await
        .unwrap();
    let writer = first.open("/shared", "w+", 0o600).await.unwrap();
    writer.write(b"shared bytes", Some(0)).await.unwrap();
    let reader = second.open("/shared", "r", 0).await.unwrap();
    let mut bytes = [0; 12];
    assert_eq!(reader.read(&mut bytes, Some(0)).await.unwrap(), 12);
    assert_eq!(&bytes, b"shared bytes");
    let second_writer = second.open("/shared", "r+", 0).await.unwrap();
    second_writer.write(b"files", Some(7)).await.unwrap();
    assert_eq!(writer.read(&mut bytes, Some(0)).await.unwrap(), 12);
    assert_eq!(&bytes, b"shared files");
    // Both writes modify different bytes of one immutable backing chunk.
    // Losing either read-modify-write must be visible to both open sessions.
    let (left, right) = tokio::join!(
        writer.write(b"A", Some(1)),
        second_writer.write(b"B", Some(2))
    );
    assert_eq!(left.unwrap(), 1);
    assert_eq!(right.unwrap(), 1);
    for handle in [&reader, &writer] {
        assert_eq!(handle.read(&mut bytes, Some(0)).await.unwrap(), 12);
        assert_eq!(&bytes, b"sABred files");
    }
    let bounded = first.open("/bounded", "w+", 0o600).await.unwrap();
    let payload = vec![42; 1024];
    assert_eq!(bounded.write(&payload, Some(0)).await.unwrap(), 512);
    assert_eq!(
        bounded.write(&payload[512..], Some(512)).await.unwrap(),
        512
    );
    let mut received = vec![0; 1024];
    assert_eq!(bounded.read(&mut received, Some(0)).await.unwrap(), 512);
    assert_eq!(&received[..512], &payload[..512]);
    assert_eq!(bounded.stat().await.unwrap().size, 1024);
    if check_failed_put {
        use mysql_async::prelude::Queryable;
        let mut observer = pool.as_ref().unwrap().get_conn().await.unwrap();
        let revision_sql = "SELECT revision FROM mount_rs_tidb_metadata WHERE volume_key=?";
        let before: u64 = observer
            .exec_first(revision_sql, (&key,))
            .await
            .unwrap()
            .unwrap();
        let blocks = child.root.join("blocks");
        let offline = child.root.join("offline-blocks");
        fs::rename(&blocks, &offline).unwrap();
        // Append beyond EOF: the attempted write changes length as well as data.
        let rejected = second_writer.write(b"uncommitted", Some(4096)).await;
        fs::rename(&offline, &blocks).unwrap();
        assert!(rejected.is_err());
        let after: u64 = observer
            .exec_first(revision_sql, (&key,))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(after, before, "failed authoritative put published metadata");
        assert_eq!(writer.stat().await.unwrap().size, 12);
        assert_eq!(writer.read(&mut bytes, Some(0)).await.unwrap(), 12);
        assert_eq!(&bytes, b"sABred files");
        assert_eq!(writer.read(&mut bytes, Some(4096)).await.unwrap(), 0);
    }
    // Leave sessions and handles open in both shutdown modes.
    let mut crashed_fence = None;
    if crash {
        use mysql_async::prelude::Queryable;
        assert!(tidb);
        assert!(!child.kill_owned().success());
        assert_listener_closed(address);
        let mut connection = pool.as_ref().unwrap().get_conn().await.unwrap();
        let sql = "SELECT owner, fence, expires, CAST(UNIX_TIMESTAMP(CURRENT_TIMESTAMP(3)) * 1000 AS SIGNED) FROM mount_rs_tidb_metadata WHERE volume_key=?";
        let (owner, fence, expires, now): (Option<String>, i64, i64, i64) =
            connection.exec_first(sql, (&key,)).await.unwrap().unwrap();
        assert!(
            owner.is_some() && expires > now,
            "crash must retain a live lease"
        );
        crashed_fence = Some(fence);
        // This child's cleanup root is separate: its rejected startup must not
        // remove the crashed owner's configuration or local blocks.
        let mut rejected = HttpChild::start(&config, temporary_root());
        assert!(!rejected.wait_for_exit(Duration::from_secs(5)).success());
        let (_, stderr) = rejected.output();
        assert!(
            stderr
                .iter()
                .any(|line| line.contains("EAGAIN") && line.contains("TiDB acquire writer")),
            "expected owner admission rejection: {stderr:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let (_, current_fence, expires, now): (Option<String>, i64, i64, i64) =
                connection.exec_first(sql, (&key,)).await.unwrap().unwrap();
            assert_eq!(current_fence, fence);
            if expires <= now {
                break;
            }
            assert!(Instant::now() < deadline, "crashed lease did not expire");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    } else {
        child.signal(libc::SIGTERM);
        assert!(child.wait_for_exit(Duration::from_secs(30)).success());
        let (stdout, stderr) = child.output();
        assert!(
            stdout.iter().any(|line| line == "http stopped"),
            "{stdout:?} {stderr:?}"
        );
    }
    if let Some(pool) = pool {
        use mysql_async::prelude::Queryable;
        if !rustfs && !crash {
            // Replace each nonempty block in this isolated store so the test does
            // not depend on extent layout or on which old chunks await GC.
            let mut originals = Vec::new();
            for entry in fs::read_dir(child.root.join("blocks")).unwrap() {
                let path = entry.unwrap().path();
                let bytes = fs::read(&path).unwrap();
                if !bytes.is_empty() {
                    let mut damaged = bytes.clone();
                    damaged[0] ^= 1;
                    fs::write(&path, damaged).unwrap();
                    originals.push((path, bytes));
                }
            }
            assert!(!originals.is_empty());
            let mut rejected = HttpChild::start(&config, temporary_root());
            assert!(!rejected.wait_for_exit(timeout).success());
            let (stdout, stderr) = rejected.output();
            assert!(!stdout.iter().any(|line| line.contains("http listening")));
            assert!(
                stderr
                    .join(" ")
                    .contains("local block content hash mismatch"),
                "{stderr:?}"
            );
            for (path, bytes) in originals {
                fs::write(path, bytes).unwrap();
            }
        }
        let mut reopened = HttpChild::start(&config, child.root.clone());
        let (address, _) = reopened.ready_with_timeout(Duration::from_secs(30));
        if let Some(old_fence) = crashed_fence {
            let mut connection = pool.get_conn().await.unwrap();
            let new_fence: i64 = connection
                .exec_first(
                    "SELECT fence FROM mount_rs_tidb_metadata WHERE volume_key=?",
                    (&key,),
                )
                .await
                .unwrap()
                .unwrap();
            assert!(new_fence > old_fence);
        }
        let remote = mount_rs_remote::RemoteFs::connect(
            &format!("http://{address}"),
            "volume",
            token,
            timeout,
        )
        .await
        .unwrap();
        let file = remote.open("/shared", "r", 0).await.unwrap();
        assert_eq!(file.stat().await.unwrap().size, 12);
        let mut bytes = [0; 12];
        assert_eq!(file.read(&mut bytes, Some(0)).await.unwrap(), 12);
        assert_eq!(&bytes, b"sABred files");
        file.close().await.unwrap();
        remote.shutdown().await.unwrap();
        reopened.signal(libc::SIGINT);
        assert!(reopened.wait_for_exit(Duration::from_secs(30)).success());
        let mut connection = pool.get_conn().await.unwrap();
        connection
            .exec_drop(
                "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&key,),
            )
            .await
            .unwrap();
        drop(connection);
        pool.disconnect().await.unwrap();
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_clients_use_configured_sessions_and_shutdown_drains_open_handles() {
    run_configured_session_case(false, false, false).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated MOUNT_RS_TIDB_URL with actual TiDB"]
async fn tidb_remote_sessions_reopen_local_blocks_after_server_shutdown() {
    run_configured_session_case(true, false, false).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated actual TiDB and RustFS fixtures"]
async fn tidb_remote_sessions_reopen_rustfs_blocks_after_server_shutdown() {
    run_configured_session_case(true, true, false).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated actual TiDB; waits for crashed owner lease expiry"]
async fn tidb_remote_sessions_recover_after_process_kill_and_lease_expiry() {
    run_configured_session_case(true, false, true).await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated actual TiDB and RustFS; waits for crashed owner lease expiry"]
async fn tidb_remote_sessions_recover_rustfs_after_process_kill_and_lease_expiry() {
    run_configured_session_case(true, true, true).await;
}

#[test]
// The disk cache needs directory synchronization, which it only has on Unix;
// elsewhere it refuses to open and the CLI exits before it is ready.
#[cfg_attr(
    not(unix),
    ignore = "the disk cache is supported on Unix only (ENOTSUP elsewhere)"
)]
fn cache_report_exposes_per_drive_read_tiers() {
    let root = temporary_root();
    let path = write_http_config(&root);
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fs::create_dir(root.join("cache")).unwrap();
    let drive = config["http"]["drives"][2].take();
    config["http"]["drives"] = serde_json::json!([drive]);
    config["http"]["cache_report_interval_seconds"] = 1.into();
    let storage = &mut config["http"]["drives"][0]["driver"]["storage"];
    storage["block_cache"] = serde_json::json!({"max_bytes": 4096, "max_entries": 8});
    storage["disk_cache"] = serde_json::json!({
        "root":"cache", "scope":"cache-report-test", "max_bytes":4096, "max_entries":8
    });
    fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    let child = HttpChild::start(&path, root);
    let (address, _) = child.ready();
    let target = "/v1/drives/splitstore/fs/report";
    let token = "splitstore-test-token";
    let put = request(address, "PUT", target, token, b"cache report");
    assert!((200..300).contains(&put.status));
    assert_eq!(
        request(address, "GET", target, token, &[]).body,
        b"cache report"
    );
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let line = child
            .stderr
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        let Ok(report) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if report["event"] != "mount_cache_stats"
            || report["ram"]["hits"].as_u64().unwrap_or(0) == 0
        {
            continue;
        }
        assert_eq!(report["drive"], "splitstore");
        for counter in [
            "backing_gets_completed",
            "backing_gets_failed",
            "backing_read_bytes",
            "backing_get_nanoseconds",
        ] {
            assert_eq!(report["ram"][counter], 0, "{counter}");
        }
        assert_eq!(report["disk"]["misses"], 0);
        assert_eq!(report["disk"]["disabled"], false);
        assert!(report["disk"]["resident_entries"].as_u64().unwrap() > 0);
        break;
    }
}
