use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const PGLITE_TOKEN_ENV: &str = "MOUNT_RS_HTTP_REMOTE_PGLITE_TOKEN";
const SQLITE_TOKEN_ENV: &str = "MOUNT_RS_HTTP_REMOTE_SQLITE_TOKEN";
const PGLITE_TOKEN: &str = "mount-rs-remote-pglite-test-token";
const SQLITE_TOKEN: &str = "mount-rs-remote-sqlite-test-token";
const REMOTE_WRITE_CHUNK_BYTES: usize = 4 * 1024;
const REMOTE_BODY_BYTES: usize = 131_123;

struct HttpResponse {
    status: u16,
    body: Vec<u8>,
}

struct HttpChild {
    child: Child,
    stdout: Receiver<String>,
    #[cfg_attr(not(unix), allow(dead_code))]
    stderr: Receiver<String>,
    readers: Vec<JoinHandle<()>>,
    root: PathBuf,
}

impl HttpChild {
    fn start(config: &Path, root: PathBuf) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
            .args(["serve-http", "--config"])
            .arg(config)
            .env(PGLITE_TOKEN_ENV, PGLITE_TOKEN)
            .env(SQLITE_TOKEN_ENV, SQLITE_TOKEN)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn remote HTTP CLI subprocess");

        let stdout = child.stdout.take().expect("capture remote HTTP stdout");
        let stderr = child.stderr.take().expect("capture remote HTTP stderr");
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

    fn ready(&self) -> SocketAddr {
        let line = self
            .stdout
            .recv_timeout(Duration::from_secs(60))
            .expect("remote HTTP CLI did not announce readiness");
        let url = line
            .split_whitespace()
            .find(|part| part.starts_with("http://"))
            .expect("remote HTTP readiness line did not contain a URL");
        url.strip_prefix("http://")
            .expect("remote HTTP URL scheme")
            .parse()
            .expect("remote HTTP readiness URL address")
    }

    fn wait_for_exit(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self
                .child
                .try_wait()
                .expect("poll remote HTTP CLI subprocess")
            {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(25));
        }
    }

    fn kill_owned(&mut self) -> ExitStatus {
        self.child.kill().expect("kill remote HTTP CLI subprocess");
        self.wait_for_exit(Duration::from_secs(10))
            .unwrap_or_else(|| {
                let _ = self.child.wait();
                panic!("remote HTTP CLI did not exit after bounded forced cleanup");
            })
    }

    #[cfg(unix)]
    fn graceful_shutdown(&mut self) {
        // SAFETY: this test owns the child process and sends SIGINT only to
        // its still-live PID.
        let result = unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGINT) };
        assert_eq!(result, 0, "send SIGINT to remote HTTP CLI subprocess");
        let status = self
            .wait_for_exit(Duration::from_secs(60))
            .unwrap_or_else(|| {
                let _ = self.kill_owned();
                panic!("remote HTTP CLI graceful shutdown exceeded 60 seconds");
            });
        for reader in self.readers.drain(..) {
            reader.join().expect("join remote HTTP output reader");
        }
        let diagnostics = self
            .stderr
            .try_iter()
            .map(|line| redact_remote_diagnostic(&line))
            .take(32)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            status.success(),
            "remote HTTP CLI graceful shutdown failed: {status}; stderr={diagnostics}"
        );
        assert!(
            self.stdout.try_iter().any(|line| line == "http stopped"),
            "remote HTTP CLI did not report graceful shutdown"
        );
    }
}

#[cfg_attr(not(unix), allow(dead_code))]
fn redact_remote_diagnostic(line: &str) -> String {
    let mut redacted = mount_rs_cli::config::redact_diagnostic(line);
    for name in [
        "R2_ENDPOINT",
        "R2_BUCKET",
        "R2_ACCESS_KEY_ID",
        "R2_SECRET_ACCESS_KEY",
    ] {
        if let Ok(value) = std::env::var(name)
            && !value.is_empty()
        {
            redacted = redacted.replace(&value, "[REDACTED]");
        }
    }
    redacted
}

impl Drop for HttpChild {
    fn drop(&mut self) {
        if self
            .child
            .try_wait()
            .expect("poll remote HTTP CLI subprocess in cleanup")
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

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set by the remote harness"))
}

fn temporary_root() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "mount-rs-cli-http-remote-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir(&root).expect("create remote HTTP test directory");
    root
}

fn remote_prefix() -> String {
    let prefix = required_env("MOUNT_RS_CLI_REMOTE_PREFIX");
    assert!(!prefix.is_empty(), "remote prefix must not be empty");
    assert!(
        !prefix
            .split('/')
            .any(|component| component.is_empty() || component == "." || component == ".."),
        "remote prefix contains an unsafe path component"
    );
    prefix
}

fn allow_external_endpoint() -> bool {
    matches!(
        std::env::var("MOUNT_RS_CLI_REMOTE_ALLOW_EXTERNAL_ENDPOINT").as_deref(),
        Ok("1") | Ok("true")
    )
}

fn write_config(root: &Path) -> PathBuf {
    let endpoint = required_env("R2_ENDPOINT");
    let bucket = required_env("R2_BUCKET");
    let _access_key = required_env("R2_ACCESS_KEY_ID");
    let _secret_key = required_env("R2_SECRET_ACCESS_KEY");
    let pglite_url = required_env("PGLITE_DATABASE_URL");
    let loopback_endpoint =
        endpoint.starts_with("http://127.0.0.1:") || endpoint.starts_with("http://localhost:");
    assert!(
        loopback_endpoint || allow_external_endpoint(),
        "remote CLI tests require a loopback RustFS R2_ENDPOINT unless MOUNT_RS_CLI_REMOTE_ALLOW_EXTERNAL_ENDPOINT is enabled"
    );
    if !loopback_endpoint {
        assert!(
            endpoint.starts_with("https://"),
            "external remote CLI tests require an HTTPS object-store endpoint"
        );
    }
    assert!(
        pglite_url.starts_with("postgresql://"),
        "remote CLI tests require a PostgreSQL-compatible PGLITE_DATABASE_URL"
    );

    let prefix = remote_prefix();
    let pglite_blocks_prefix = format!("{prefix}/pglite-r2-blocks");
    let sqlite_blocks_prefix = format!("{prefix}/sqlite-r2-blocks");
    let config_path = root.join("http-remote.json");
    let config = serde_json::json!({
        "version": 1,
        "http": {
            "host": "127.0.0.1",
            "port": 0,
            "max_request_bytes": 524288,
            "read_chunk_bytes": 4096,
            "drain_timeout_ms": 5000,
            "drives": [
                {
                    "id": "pglite-r2",
                    "token": {"env": PGLITE_TOKEN_ENV},
                    "driver": {
                        "kind": "splitstore",
                        "storage": {
                            "metadata": {
                                "kind": "pglite",
                                "connection": {"env": "PGLITE_DATABASE_URL"},
                                "volume_key": format!("{prefix}/pglite-metadata"),
                                "durable": true
                            },
                            "blocks": {
                                "kind": "r2",
                                "endpoint": endpoint,
                                "bucket": bucket,
                                "prefix": pglite_blocks_prefix,
                                "access_key_id": {"env": "R2_ACCESS_KEY_ID"},
                                "secret_access_key": {"env": "R2_SECRET_ACCESS_KEY"},
                                "durable": true
                            },
                            "chunk_size_bytes": 4096
                        }
                    }
                },
                {
                    "id": "sqlite-r2",
                    "token": {"env": SQLITE_TOKEN_ENV},
                    "driver": {
                        "kind": "splitstore",
                        "storage": {
                            "metadata": {
                                "kind": "sqlite",
                                "path": root.join("sqlite-metadata.db")
                            },
                            "blocks": {
                                "kind": "r2",
                                "endpoint": endpoint,
                                "bucket": bucket,
                                "prefix": sqlite_blocks_prefix,
                                "access_key_id": {"env": "R2_ACCESS_KEY_ID"},
                                "secret_access_key": {"env": "R2_SECRET_ACCESS_KEY"},
                                "durable": true
                            },
                            "chunk_size_bytes": 4096
                        }
                    }
                }
            ]
        }
    });
    fs::write(
        &config_path,
        serde_json::to_vec_pretty(&config).expect("serialize remote HTTP config"),
    )
    .expect("write remote HTTP config");
    config_path
}

fn request(
    address: SocketAddr,
    method: &str,
    path: &str,
    token: &str,
    body: &[u8],
    range: Option<&str>,
) -> HttpResponse {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(10))
        .expect("connect remote HTTP CLI");
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .expect("set remote HTTP response timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(60)))
        .expect("set remote HTTP request timeout");
    let range_header = range
        .map(|range| format!("Range: {range}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\nConnection: close\r\nContent-Length: {}\r\n{range_header}\r\n",
        body.len()
    );
    stream
        .write_all(request.as_bytes())
        .expect("write remote HTTP request headers");
    // Exercise the server's streaming body path with multiple transport
    // writes. HTTP PUT is whole-file replacement; ranged partial reads are
    // asserted separately below.
    for chunk in body.chunks(REMOTE_WRITE_CHUNK_BYTES) {
        stream
            .write_all(chunk)
            .expect("write remote HTTP request body chunk");
        thread::yield_now();
    }

    let mut bytes = Vec::new();
    stream
        .read_to_end(&mut bytes)
        .expect("read remote HTTP response");
    let header_end = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("remote HTTP response headers");
    let headers = std::str::from_utf8(&bytes[..header_end]).expect("remote HTTP response text");
    let status = headers
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .expect("remote HTTP response status");
    HttpResponse {
        status,
        body: bytes[header_end + 4..].to_vec(),
    }
}

fn payload(seed: u8) -> Vec<u8> {
    (0..REMOTE_BODY_BYTES)
        .map(|index| {
            let value = (index as u64)
                .wrapping_mul(0x9e37_79b9)
                .wrapping_add(u64::from(seed));
            (value ^ (value >> 13) ^ (value >> 29)) as u8
        })
        .collect()
}

fn round_trip(address: SocketAddr, drive: &str, token: &str, expected: &[u8]) {
    let path = format!("/v1/drives/{drive}/fs/remote-binary");
    let written = request(address, "PUT", &path, token, expected, None);
    assert!((200..300).contains(&written.status));

    let full = request(address, "GET", &path, token, &[], None);
    assert_eq!(full.status, 200);
    assert_eq!(full.body, expected);

    let start = 1234;
    let end = expected.len() - 567;
    let range = format!("bytes={start}-{end}");
    let partial = request(address, "GET", &path, token, &[], Some(&range));
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, expected[start..=end]);
}

#[cfg(unix)]
fn persisted_read(address: SocketAddr, drive: &str, token: &str, expected: &[u8]) {
    let path = format!("/v1/drives/{drive}/fs/remote-binary");
    let full = request(address, "GET", &path, token, &[], None);
    assert_eq!(full.status, 200);
    assert_eq!(full.body, expected);
    let range = "bytes=4096-8191";
    let partial = request(address, "GET", &path, token, &[], Some(range));
    assert_eq!(partial.status, 206);
    assert_eq!(partial.body, expected[4096..=8191]);
}

fn start_and_write(config_path: &Path, root: PathBuf) -> (HttpChild, Vec<u8>, Vec<u8>) {
    let child = HttpChild::start(config_path, root);
    let address = child.ready();
    let pglite_payload = payload(0x31);
    let sqlite_payload = payload(0xa7);
    round_trip(address, "pglite-r2", PGLITE_TOKEN, &pglite_payload);
    round_trip(address, "sqlite-r2", SQLITE_TOKEN, &sqlite_payload);

    let unauthorized = request(
        address,
        "GET",
        "/v1/drives/pglite-r2/fs/remote-binary",
        SQLITE_TOKEN,
        &[],
        None,
    );
    assert_eq!(unauthorized.status, 401);
    (child, pglite_payload, sqlite_payload)
}

#[cfg(unix)]
#[test]
#[ignore = "requires the real PGlite and RustFS harness environment"]
fn remote_cli_http_durable_reopen_after_graceful_shutdown() {
    let root = temporary_root();
    let config_path = write_config(&root);
    let (mut first, pglite_payload, sqlite_payload) = start_and_write(&config_path, root.clone());
    first.graceful_shutdown();

    let mut reopened = HttpChild::start(&config_path, root);
    let address = reopened.ready();
    persisted_read(address, "pglite-r2", PGLITE_TOKEN, &pglite_payload);
    persisted_read(address, "sqlite-r2", SQLITE_TOKEN, &sqlite_payload);
    reopened.graceful_shutdown();
}

#[cfg(not(unix))]
#[test]
#[ignore = "requires the real PGlite and RustFS harness environment"]
fn remote_cli_http_roundtrip_forced_cleanup() {
    let root = temporary_root();
    let config_path = write_config(&root);
    let (mut child, _pglite_payload, _sqlite_payload) = start_and_write(&config_path, root);
    // Windows has no portable equivalent of the Unix SIGINT path used to
    // release the chunked writer lease. This test still exercises real remote
    // HTTP writes, full reads, ranged reads, and auth before owned cleanup;
    // the Unix-only test is the durable reopen evidence.
    let _ = child.kill_owned();
}

#[cfg(unix)]
#[test]
#[ignore = "requires isolated real TiDB and RustFS harnesses"]
fn tidb_cli_http_cached_reopen() {
    check_tidb_cli_http_cached_reopen(false);
}

#[cfg(unix)]
fn check_tidb_cli_http_cached_reopen(normalized: bool) {
    use mysql_async::{Pool, prelude::Queryable};
    assert!(Path::new(&required_env("RUSTFS_RUN_DIR")).is_dir());
    let endpoint = required_env("R2_ENDPOINT");
    assert!(endpoint.starts_with("http://127.0.0.1:") || endpoint.starts_with("http://localhost:"));
    let url = required_env("MOUNT_RS_TIDB_URL");
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let (pool, mut connection) = rt.block_on(async {
        let pool = Pool::from_url(&url).expect("valid TiDB URL");
        let mut conn = pool.get_conn().await.expect("connect real TiDB");
        let identity: Option<(String, String)> = conn
            .query_first("SELECT tidb_version(), VERSION()")
            .await
            .expect("real TiDB identity");
        assert!(identity.is_some_and(|(a, b)| format!("{a} {b}").to_lowercase().contains("tidb")));
        (pool, conn)
    });
    let root = temporary_root();
    let key = format!(
        "cli-tidb-audit-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let lease_window_ms: u64 = std::env::var("AUDIT_LEASE_WINDOW_MS")
        .unwrap_or_else(|_| "0".into())
        .parse()
        .expect("integer owner window");
    let config_path = root.join("tidb-http.json");
    let config = serde_json::json!({"version":1,"http":{
        "host":"127.0.0.1","port":0,"max_request_bytes":524288,"read_chunk_bytes":65536,"drain_timeout_ms":5000,
        "drives":[{"id":"tidb","token":{"env":PGLITE_TOKEN_ENV},"driver":{"kind":"splitstore","storage":{
            "metadata":{"kind":"tidb","connection":{"env":"MOUNT_RS_TIDB_URL"},"volume_key":key,"durable":false},
            "blocks":{"kind":"r2","endpoint":endpoint,"bucket":required_env("R2_BUCKET"),"prefix":format!("{}/cli-tidb/{key}",required_env("RUSTFS_COMBO_PREFIX")),"access_key_id":{"env":"R2_ACCESS_KEY_ID"},"secret_access_key":{"env":"R2_SECRET_ACCESS_KEY"},"durable":false},
            "atime":"noatime","read_lease_cache_window_ms":lease_window_ms,"block_cache":{"max_bytes":16777216,"max_entries":4096},"chunk_size_bytes":65536
        }}}]
    }});
    fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    let mut first = HttpChild::start(&config_path, root.clone());
    let address = first.ready();
    let data = payload(0x57);
    round_trip(address, "tidb", PGLITE_TOKEN, &data);
    let path = "/v1/drives/tidb/fs/remote-binary";
    assert_eq!(
        request(address, "GET", path, "wrong-token", &[], None).status,
        401
    );
    let mut timings = Vec::new();
    for _ in 0..20 {
        let start = Instant::now();
        let response = request(address, "GET", path, PGLITE_TOKEN, &[], None);
        timings.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(response.status, 200);
        assert_eq!(response.body, data);
    }
    println!("TIDB_CLI_HTTP_LEASE_WINDOW_MS {lease_window_ms}");
    println!(
        "TIDB_CLI_HTTP_READ_SAMPLES_MS {}",
        serde_json::to_string(&timings).unwrap()
    );
    first.graceful_shutdown();
    let mut second = HttpChild::start(&config_path, root.clone());
    let address = second.ready();
    persisted_read(address, "tidb", PGLITE_TOKEN, &data);
    second.graceful_shutdown();

    // Seed a disposable disk cache from the persisted RustFS objects, then
    // restart with empty RAM so no memory hit can hide corrupted disk bytes.
    let cache_root = root.join("disk-cache");
    fs::create_dir(&cache_root).unwrap();
    let mut disk_config = config.clone();
    disk_config["http"]["drives"][0]["driver"]["storage"]["disk_cache"] = serde_json::json!({
        "root": cache_root, "scope": key, "max_bytes": 16777216, "max_entries": 4096
    });
    fs::write(&config_path, serde_json::to_vec(&disk_config).unwrap()).unwrap();
    let mut seed = HttpChild::start(&config_path, root.clone());
    persisted_read(seed.ready(), "tidb", PGLITE_TOKEN, &data);
    seed.graceful_shutdown();
    let mut entries = Vec::new();
    for scope in fs::read_dir(&cache_root).unwrap() {
        for entry in fs::read_dir(scope.unwrap().path()).unwrap() {
            let entry = entry.unwrap();
            if entry.file_name() == "lock" {
                continue;
            }
            let path = entry.path();
            let original = fs::read(&path).unwrap();
            let mut corrupt = original.clone();
            *corrupt.last_mut().expect("nonempty cache entry") ^= 1;
            fs::write(&path, corrupt).unwrap();
            entries.push((path, original));
        }
    }
    assert!(!entries.is_empty());
    let mut repaired = HttpChild::start(&config_path, root.clone());
    persisted_read(repaired.ready(), "tidb", PGLITE_TOKEN, &data);
    repaired.graceful_shutdown();
    for (path, original) in &entries {
        assert_eq!(fs::read(path).unwrap(), *original);
    }
    println!(
        "TIDB_RUSTFS_DISK_CACHE_REPAIRED normalized={normalized} entries={}",
        entries.len()
    );
    rt.block_on(async {
        let stored: String = connection
            .exec_first(
                "SELECT namespace FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&key,),
            )
            .await
            .unwrap()
            .expect("committed TiDB namespace");
        assert_eq!(stored == "{\"mount_rs_normalized_records\":1}", normalized);
        if normalized {
            connection
                .exec_drop(
                    "DELETE FROM mount_rs_tidb_metadata_records WHERE volume_key=?",
                    (&key,),
                )
                .await
                .expect("scoped normalized-record cleanup");
        }
        connection
            .exec_drop(
                "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&key,),
            )
            .await
            .expect("scoped TiDB cleanup");
        drop(connection);
        pool.disconnect().await.unwrap();
    });
    println!("TIDB_CLI_HTTP_CACHE_REOPEN_PASS normalized={normalized}");
}
