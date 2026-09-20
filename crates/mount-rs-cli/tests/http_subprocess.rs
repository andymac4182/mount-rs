use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::time::Instant;

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
            .args(["serve-http", "--config"])
            .arg(config)
            .env("MOUNT_RS_HTTP_MEMORY_TOKEN", "memory-test-token")
            .env("MOUNT_RS_HTTP_SQLITE_TOKEN", "sqlite-test-token")
            .env("MOUNT_RS_HTTP_SPLIT_TOKEN", "splitstore-test-token")
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
        let line = self
            .stdout
            .recv_timeout(Duration::from_secs(5))
            .unwrap_or_else(|error| {
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
    fn interrupt(&mut self) {
        // SAFETY: the child process was spawned by this test and its PID is
        // not reused while it is still running.
        let result = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(result, 0, "send SIGINT to HTTP subprocess");
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
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2))
        .expect("connect HTTP subprocess");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set HTTP response timeout");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("write HTTP request headers");
    stream.write_all(body).expect("write HTTP request body");

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
    HttpResponse {
        status,
        body: bytes[header_end + 4..].to_vec(),
    }
}

fn temporary_root() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("mount-rs-cli-http-{}-{nanos}", std::process::id()));
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

    let memory_write = request(
        address,
        "PUT",
        "/v1/drives/memory/fs/memory-only",
        "memory-test-token",
        b"memory bytes",
    );
    assert!((200..300).contains(&memory_write.status));
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
    assert_eq!(memory_read.body, b"memory bytes");
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

    let _forced_status = child.kill_owned();
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
    child.interrupt();
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
