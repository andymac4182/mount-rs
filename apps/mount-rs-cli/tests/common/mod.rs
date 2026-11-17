//! Shared by the subprocess tests of a forwarding `serve-http` cluster: a
//! throwaway peer PKI, a running server and its output, one-shot HTTP
//! requests, and a TCP proxy that can cut a server off from Redis.
#![allow(dead_code)]

use rcgen::{
    BasicConstraints, Certificate, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream, UdpSocket},
    process::{Child, Command},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub const TOKEN: &str = "cluster-test-token-0123456789abcdef";
/// The lifetime `serve-http` gives a route in Redis.
pub const ROUTE_TTL: Duration = Duration::from_secs(15);

pub struct Identity {
    pub cert: Certificate,
    pub key: KeyPair,
}

pub fn authority(name: &str) -> Identity {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, name);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    Identity {
        cert: params.self_signed(&key).unwrap(),
        key,
    }
}

/// A peer certificate in the profile the forwarding transport requires.
pub fn peer(ca: &Identity, node: &str) -> Identity {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["localhost".into()]).unwrap();
    params.subject_alt_names.push(SanType::URI(
        format!("urn:agent-tunnel:peer:{node}").try_into().unwrap(),
    ));
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ClientAuth,
        ExtendedKeyUsagePurpose::ServerAuth,
    ];
    Identity {
        cert: params.signed_by(&key, &ca.cert, &ca.key).unwrap(),
        key,
    }
}

pub fn free_udp() -> SocketAddr {
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

pub fn free_tcp_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

pub fn unique(prefix: &str) -> String {
    // The clock alone is not enough: tests start together on their own
    // threads, and where it only counts microseconds two of them can read
    // the same value and then share a Redis namespace or a storage volume.
    static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{}-{nanos}-{sequence}", std::process::id())
}

pub fn collect(stream: impl Read + Send + 'static) -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = lines.clone();
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    lines
}

pub struct Server {
    pub name: String,
    pub child: Child,
    pub address: SocketAddr,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Server {
    /// Run `command` (a `serve-http` with piped output) as server `name`
    /// and wait for its public listener.
    pub fn spawn(command: &mut Command, name: &str) -> Self {
        let mut child = command.spawn().unwrap();
        let stderr = collect(child.stderr.take().unwrap());
        let (sender, receiver) = mpsc::channel();
        let stdout = child.stdout.take().unwrap();
        thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let mut server = Server {
            name: name.to_owned(),
            child,
            address: "127.0.0.1:0".parse().unwrap(),
            stderr,
        };
        let line = receiver
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|error| {
                panic!(
                    "server {name} did not become ready: {error}; stderr={:?}",
                    server.stderr()
                )
            });
        server.address = line
            .split_whitespace()
            .find_map(|part| part.strip_prefix("http://"))
            .unwrap_or_else(|| panic!("no URL in readiness line {line:?}"))
            .parse()
            .unwrap();
        server
    }

    pub fn stderr(&self) -> Vec<String> {
        self.stderr.lock().unwrap().clone()
    }

    /// The `drive_failover` records the server has written, oldest first.
    pub fn events(&self) -> Vec<serde_json::Value> {
        self.stderr()
            .iter()
            .filter(|line| line.contains("\"drive_failover\""))
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    /// The records of `events` whose `state` is `state`.
    pub fn states(&self, state: &str) -> Vec<serde_json::Value> {
        let mut events = self.events();
        events.retain(|event| event["state"] == state);
        events
    }

    pub fn assert_running(&mut self, what: &str) {
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!("{what} exited with {status}; stderr={:?}", self.stderr());
        }
    }

    /// Send `signal` (a `libc::SIG*`) to the server process.
    pub fn signal(&self, signal: i32) {
        // SAFETY: the pid is this test's own live child process.
        let sent = unsafe { libc::kill(self.child.id() as libc::pid_t, signal) };
        assert_eq!(sent, 0, "signal {signal} to server {}", self.name);
    }

    /// Wait up to `limit` for the process to exit.
    pub fn wait_exit(&mut self, limit: Duration) -> std::process::ExitStatus {
        let start = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                start.elapsed() < limit,
                "server {} did not exit within {limit:?}",
                self.name
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn put(&self, name: &str, body: &[u8]) -> std::io::Result<(u16, Vec<u8>)> {
        http(self.address, "PUT", name, body)
    }

    pub fn get(&self, name: &str) -> std::io::Result<(u16, Vec<u8>)> {
        http(self.address, "GET", name, b"")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if thread::panicking() {
            // What the server said, for the failure being reported.
            let lines = self.stderr();
            let tail = &lines[lines.len().saturating_sub(40)..];
            eprintln!("--- server {} stderr (last {}) ---", self.name, tail.len());
            for line in tail {
                eprintln!("{line}");
            }
        }
    }
}

/// One HTTP/1.1 request for a file of drive `volume`, on its own connection.
pub fn http(
    address: SocketAddr,
    method: &str,
    name: &str,
    body: &[u8],
) -> std::io::Result<(u16, Vec<u8>)> {
    http_within(address, method, name, body, Duration::from_secs(20))
}

/// `http`, giving up on a read or a write that makes no progress for
/// `patience`.
pub fn http_within(
    address: SocketAddr,
    method: &str,
    name: &str,
    body: &[u8],
    patience: Duration,
) -> std::io::Result<(u16, Vec<u8>)> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5))?;
    stream.set_read_timeout(Some(patience))?;
    stream.set_write_timeout(Some(patience))?;
    write!(
        stream,
        "{method} /v1/drives/volume/fs/{name} HTTP/1.1\r\nhost: {address}\r\n\
         authorization: Bearer {TOKEN}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response)?;
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed response");
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(invalid)?;
    let head = String::from_utf8_lossy(&response[..split]).to_ascii_lowercase();
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .ok_or_else(invalid)?;
    let mut payload = response[split + 4..].to_vec();
    if head.contains("transfer-encoding: chunked") {
        let mut decoded = Vec::new();
        let mut rest = payload.as_slice();
        loop {
            let line = rest
                .windows(2)
                .position(|window| window == b"\r\n")
                .ok_or_else(invalid)?;
            let size = usize::from_str_radix(
                String::from_utf8_lossy(&rest[..line])
                    .split(';')
                    .next()
                    .unwrap()
                    .trim(),
                16,
            )
            .map_err(|_| invalid())?;
            rest = &rest[line + 2..];
            if size == 0 {
                break;
            }
            decoded.extend_from_slice(rest.get(..size).ok_or_else(invalid)?);
            rest = rest.get(size + 2..).ok_or_else(invalid)?;
        }
        payload = decoded;
    }
    Ok((status, payload))
}

/// Poll until `check` holds, or panic with `what` after `limit`.
pub fn wait_until(what: &str, limit: Duration, mut check: impl FnMut() -> bool) -> Duration {
    let start = Instant::now();
    while !check() {
        assert!(start.elapsed() < limit, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(100));
    }
    start.elapsed()
}

// ---------------------------------------------------------------------------
// A Redis outage, made by a TCP proxy between the servers and Redis.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Link {
    Up,
    /// Nothing listens: connections are refused, as when Redis is stopped.
    Refused,
    /// Connections are accepted and never answered, as when packets are
    /// dropped. Connections that existed stay open and silent, for good.
    Silent,
}

pub struct Proxy {
    pub port: u16,
    link: Arc<Mutex<Link>>,
    clients: Arc<Mutex<Vec<TcpStream>>>,
}

impl Proxy {
    pub fn start(upstream: SocketAddr) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let link = Arc::new(Mutex::new(Link::Up));
        let clients = Arc::new(Mutex::new(Vec::<TcpStream>::new()));
        let (state, held) = (link.clone(), clients.clone());
        thread::spawn(move || {
            let mut listener = Some(listener);
            loop {
                let mode = *state.lock().unwrap();
                if mode == Link::Refused {
                    listener = None;
                    thread::sleep(Duration::from_millis(20));
                    continue;
                }
                let active = listener.get_or_insert_with(|| {
                    loop {
                        match TcpListener::bind(("127.0.0.1", port)) {
                            Ok(listener) => break listener,
                            Err(_) => thread::sleep(Duration::from_millis(20)),
                        }
                    }
                });
                active.set_nonblocking(true).unwrap();
                let Ok((client, _)) = active.accept() else {
                    thread::sleep(Duration::from_millis(5));
                    continue;
                };
                client.set_nonblocking(false).unwrap();
                held.lock().unwrap().push(client.try_clone().unwrap());
                if mode == Link::Silent {
                    continue;
                }
                let Ok(server) = TcpStream::connect(upstream) else {
                    let _ = client.shutdown(Shutdown::Both);
                    continue;
                };
                let lost = Arc::new(AtomicBool::new(false));
                for (mut from, mut to) in [
                    (client.try_clone().unwrap(), server.try_clone().unwrap()),
                    (server, client),
                ] {
                    let state = state.clone();
                    let lost = lost.clone();
                    thread::spawn(move || {
                        let mut buffer = [0u8; 16384];
                        while let Ok(count) = from.read(&mut buffer) {
                            // A silent link swallows what it is given, and a
                            // connection that lost bytes never carries any
                            // again: it stays open and dead, as it would
                            // behind a firewall that dropped its state.
                            if *state.lock().unwrap() == Link::Silent {
                                lost.store(true, Ordering::Release);
                            }
                            if lost.load(Ordering::Acquire) {
                                if count == 0 {
                                    return;
                                }
                                continue;
                            }
                            if count == 0 || to.write_all(&buffer[..count]).is_err() {
                                break;
                            }
                        }
                        if !lost.load(Ordering::Acquire) {
                            let _ = from.shutdown(Shutdown::Both);
                            let _ = to.shutdown(Shutdown::Both);
                        }
                    });
                }
            }
        });
        Self {
            port,
            link,
            clients,
        }
    }

    pub fn set(&self, link: Link) {
        *self.link.lock().unwrap() = link;
        if link == Link::Refused {
            for client in self.clients.lock().unwrap().drain(..) {
                let _ = client.shutdown(Shutdown::Both);
            }
            // Let the listener close before the caller measures anything.
            thread::sleep(Duration::from_millis(100));
        }
    }
}

pub fn redis_test_address() -> (String, SocketAddr) {
    let url = std::env::var("MOUNT_RS_REDIS_TEST_URL").expect("isolated Redis URL");
    let authority = url
        .strip_prefix("redis://")
        .expect("a plaintext loopback Redis URL")
        .split('/')
        .next()
        .unwrap();
    assert!(
        !authority.contains('@'),
        "the outage proxy does not carry credentials"
    );
    let address = if authority.contains(':') {
        authority.to_owned()
    } else {
        format!("{authority}:6379")
    };
    (url, address.parse().expect("a literal address and port"))
}
