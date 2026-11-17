//! The HTTP contract of `serve-http` with cluster features, checked with
//! the real binary.
//!
//! Two documents describe what a client may rely on:
//! `transports/mount-rs-http/openapi.yaml` (the file routes, v1) and
//! `transports/mount-rs-http/openapi-cluster.yaml` (sessions, handles and
//! the responses a forwarding server adds). The tests here show that
//!
//! - the v1 file routes answer one fixed sequence of requests the same way
//!   on a plain server, on a server with `remote_sessions`, on the owner of
//!   a drive in a forwarding cluster, and through an ingress that forwards
//!   to the owner; and every answer is declared by `openapi.yaml`;
//! - the session and handle routes, on a `remote_sessions` server and
//!   through an ingress, serve exactly the routes and methods the cluster
//!   document names, produce every response it declares (or the response
//!   is listed in `NOT_REPRODUCED` with the reason), and produce nothing
//!   it does not declare;
//! - the responses a forwarding server gives in place of the drive's own
//!   match the cluster document.
//!
//! The tests that need a forwarding cluster need Redis: they are ignored
//! unless run with `--include-ignored` and `MOUNT_RS_REDIS_TEST_URL`.
//!
//! Every test compiles and runs on Windows as on Unix, with one difference:
//! a server is stopped there by terminating it, because Windows has no
//! SIGTERM, so the clean exit that `Server::stop` requires is checked on
//! Unix only.

#[path = "../../../transports/mount-rs-http/tests/support/blackbox.rs"]
mod blackbox;
mod common;
#[path = "../../../transports/mount-rs-http/tests/support/openapi.rs"]
mod openapi;

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use blackbox::{Target, UnknownDrive};
use openapi::{METHODS, Operation, Reply, check_response, operations, report, validate};
use rcgen::{
    BasicConstraints, Certificate, CertificateParams, ExtendedKeyUsagePurpose, IsCa, KeyPair,
    KeyUsagePurpose, SanType,
};
use serde_json::{Value, json};

/// A token in the format `remote_sessions` requires.
const TOKEN: &str = "contract-test-token-0123456789abcdef";
const TOKEN_ENV: &str = "CLUSTER_CONTRACT_TOKEN";
const REDIS_ENV: &str = "CLUSTER_CONTRACT_REDIS_URL";
const V1: &str = include_str!("../../../transports/mount-rs-http/openapi.yaml");
const CLUSTER: &str = include_str!("../../../transports/mount-rs-http/openapi-cluster.yaml");

/// Responses the cluster document declares and no request here produces,
/// and why. An entry that is no longer needed fails the test.
const NOT_REPRODUCED: [(&str, u16, &str); 10] = [
    (
        "createSession",
        409,
        "needs a drive that is shutting down or has lost its lease",
    ),
    (
        "createSession",
        500,
        "needs the drive's storage to fail while the session is made",
    ),
    (
        "createSession",
        503,
        "needs a server that is shutting down, or two sessions drawing the same random token",
    ),
    (
        "closeSession",
        409,
        "needs a drive that has lost its lease while a session is open",
    ),
    (
        "closeSession",
        500,
        "needs closing an open handle to fail in storage",
    ),
    ("closeSession", 503, "needs a server that is shutting down"),
    ("readHandle", 500, "needs a read to fail in storage"),
    ("readHandle", 503, "needs a server that is shutting down"),
    (
        "sessionOperation",
        503,
        "needs a server that is shutting down",
    ),
    ("writeHandle", 503, "needs a server that is shutting down"),
];

// --------------------------------------------------------------- processes

struct Server {
    child: Child,
    base: String,
    address: SocketAddr,
    stderr: Arc<Mutex<Vec<String>>>,
}

impl Server {
    /// Start `serve-http` on the config `http` in `dir` and wait for its
    /// listener.
    fn start(dir: &std::path::Path, name: &str, http: Value, redis: Option<&str>) -> Self {
        let path = dir.join(format!("{name}.json"));
        std::fs::write(&path, json!({"version": 1, "http": http}).to_string()).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_mount-rs"));
        command
            .args(["serve-http", "--config"])
            .arg(&path)
            .env(TOKEN_ENV, TOKEN)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(redis) = redis {
            command.env(REDIS_ENV, redis);
        }
        let mut child = command.spawn().unwrap();
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let sink = stderr.clone();
        let errors = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(errors).lines().map_while(Result::ok) {
                sink.lock().unwrap().push(line);
            }
        });
        let (sender, receiver) = mpsc::channel();
        let output = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(output).lines().map_while(Result::ok) {
                let _ = sender.send(line);
            }
        });
        let line = receiver
            .recv_timeout(Duration::from_secs(60))
            .unwrap_or_else(|error| {
                let _ = child.kill();
                panic!(
                    "server {name} did not become ready: {error}; stderr={:?}",
                    stderr.lock().unwrap()
                )
            });
        let authority = line
            .split_whitespace()
            .find_map(|part| part.strip_prefix("http://"))
            .unwrap_or_else(|| panic!("no URL in readiness line {line:?}"))
            .to_owned();
        Self {
            child,
            base: format!("http://{authority}"),
            address: authority.parse().unwrap(),
            stderr,
        }
    }

    fn target(&self, holds_drive: bool, unknown_drive: UnknownDrive) -> Target {
        Target {
            base: self.base.clone(),
            drive: "main".to_owned(),
            token: TOKEN.to_owned(),
            holds_drive,
            unknown_drive,
        }
    }

    /// Stop with SIGTERM, as a deploy does, and require a clean exit.
    #[cfg(unix)]
    fn stop(mut self, name: &str) {
        // SAFETY: the pid is this test's own child, which has not been waited for.
        unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
        let status = self.child.wait().unwrap();
        assert!(
            status.success(),
            "{name} exited with {status}; stderr={:?}",
            self.stderr.lock().unwrap()
        );
    }

    /// Stop the server. Unix only for the clean exit: Windows has no
    /// SIGTERM, and the standard library can only terminate a child there,
    /// so the server is required to be running until then and no more.
    /// Every answer the tests check has been received by the time they stop
    /// a server.
    #[cfg(not(unix))]
    fn stop(mut self, name: &str) {
        if let Some(status) = self.child.try_wait().unwrap() {
            panic!(
                "{name} exited early with {status}; stderr={:?}",
                self.stderr.lock().unwrap()
            );
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Kill without warning, as a crash does.
    fn crash(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn drive() -> Value {
    json!({
        "id": "main",
        "token": {"env": TOKEN_ENV},
        "driver": {"kind": "splitstore", "storage": {
            "metadata": {"kind": "memory"}, "blocks": {"kind": "memory"}}}
    })
}

/// The listener settings the client sequence is written for.
fn sequence_http(drives: Value) -> Value {
    json!({
        "host": "127.0.0.1",
        "port": 0,
        "max_request_bytes": blackbox::MAX_REQUEST_BYTES,
        "max_upload_bytes": blackbox::MAX_UPLOAD_BYTES,
        "max_directory_entries": blackbox::MAX_DIRECTORY_ENTRIES,
        "drives": drives,
    })
}

fn with(mut http: Value, key: &str, value: Value) -> Value {
    http[key] = value;
    http
}

// ------------------------------------------------------------------ cluster

struct Identity {
    cert: Certificate,
    key: KeyPair,
}

fn authority() -> Identity {
    let key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "mount-rs contract test CA");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
    Identity {
        cert: params.self_signed(&key).unwrap(),
        key,
    }
}

/// A peer certificate in the profile the forwarding transport requires.
fn peer(ca: &Identity, node: &str) -> Identity {
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

/// The files of a forwarding cluster: a CA, a certificate per node and the
/// membership file, under a Redis namespace of its own.
struct Cluster {
    dir: tempfile::TempDir,
    namespace: String,
    redis: String,
}

impl Cluster {
    fn new(nodes: &[&str]) -> Self {
        let redis = std::env::var("MOUNT_RS_REDIS_TEST_URL").expect("isolated Redis URL");
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, content: String| {
            std::fs::write(dir.path().join(name), content).unwrap();
        };
        let ca = authority();
        write("ca.pem", ca.cert.pem());
        let mut members = Vec::new();
        for node in nodes {
            let identity = peer(&ca, node);
            write(&format!("{node}.pem"), identity.cert.pem());
            write(&format!("{node}.key"), identity.key.serialize_pem());
            let pin = mount_rs_cluster::transport::spki_sha256_from_der(identity.cert.der())
                .unwrap()
                .to_hex();
            members.push(json!({
                "node": node, "boot": format!("{node}-boot"), "address": common::free_udp(),
                "tls_server_name": "localhost", "spki_sha256": pin,
            }));
        }
        write("peers.json", Value::Array(members).to_string());
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        Self {
            dir,
            namespace: format!("cluster-contract-{}-{nanos}", std::process::id()),
            redis,
        }
    }

    fn forwarding(&self, node: &str, owner: bool) -> Value {
        json!({
            "peers": "peers.json",
            "certificate": format!("{node}.pem"),
            "private_key": format!("{node}.key"),
            "ca": "ca.pem",
            "redis_url": {"env": REDIS_ENV},
            "namespace": self.namespace,
            "bind": "127.0.0.1:0",
            "owner": owner,
        })
    }

    fn start(&self, node: &str, http: Value, owner: bool) -> Server {
        let http = with(http, "forwarding", self.forwarding(node, owner));
        Server::start(self.dir.path(), node, http, Some(&self.redis))
    }
}

// ------------------------------------------------------------ raw requests

/// Send the head of a request that declares `declared` body bytes and
/// never sends them, and return what the server answers.
fn stalled(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(String, String)],
    declared: usize,
) -> Reply {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(5)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut head = format!("{method} {path} HTTP/1.1\r\nhost: {address}\r\n");
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str(&format!("content-length: {declared}\r\n\r\n"));
    stream.write_all(head.as_bytes()).unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("no status in {line:?}"));
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').unwrap();
        headers.push((name.to_ascii_lowercase(), value.trim().to_owned()));
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    Reply {
        status,
        headers,
        body,
    }
}

// ------------------------------------------------------- the session suite

#[derive(Clone, Default)]
struct Request {
    query: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    /// `None`: the drive's token. `Some(None)`: no `Authorization`.
    token: Option<Option<String>>,
}

impl Request {
    fn new() -> Self {
        Self::default()
    }
    fn header(mut self, name: &str, value: impl ToString) -> Self {
        self.headers.push((name.to_owned(), value.to_string()));
        self
    }
    fn session(self, token: &str) -> Self {
        self.header("x-mount-session", token)
    }
    fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = Some(body.into());
        self
    }
    fn query(mut self, query: &str) -> Self {
        self.query = query.to_owned();
        self
    }
    fn anonymous(mut self) -> Self {
        self.token = Some(None);
        self
    }
    fn token(mut self, token: &str) -> Self {
        self.token = Some(Some(token.to_owned()));
        self
    }
}

struct Sessions {
    base: String,
    address: SocketAddr,
    doc: Value,
    operations: Vec<Operation>,
    client: reqwest::Client,
    produced: BTreeSet<(String, u16)>,
    failures: Vec<String>,
}

impl Sessions {
    fn new(server: &Server) -> Self {
        let doc = openapi::parse(CLUSTER);
        Self {
            base: server.base.clone(),
            address: server.address,
            operations: operations(&doc),
            doc,
            client: reqwest::Client::new(),
            produced: BTreeSet::new(),
            failures: Vec::new(),
        }
    }

    fn operation(&self, id: &str) -> Operation {
        self.operations
            .iter()
            .find(|operation| operation.id == id)
            .unwrap_or_else(|| panic!("the cluster document has no operation `{id}`"))
            .clone()
    }

    fn path(operation: &Operation) -> String {
        operation.template.replace("{id}", "main")
    }

    async fn send(&self, method: &str, path: &str, request: &Request) -> Reply {
        let method = reqwest::Method::from_bytes(method.as_bytes()).unwrap();
        let url = format!("{}{path}{}", self.base, request.query);
        let mut builder = self.client.request(method, url);
        match &request.token {
            None => builder = builder.header("authorization", format!("Bearer {TOKEN}")),
            Some(Some(token)) => {
                builder = builder.header("authorization", format!("Bearer {token}"));
            }
            Some(None) => {}
        }
        for (name, value) in &request.headers {
            builder = builder.header(name, value);
        }
        if let Some(body) = &request.body {
            builder = builder.body(body.clone());
        }
        let response = builder.send().await.expect("response");
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = response.bytes().await.expect("body");
        Reply::collect(status, &headers, &body)
    }

    /// Check `reply` against what the document says the operation answers.
    fn check(&mut self, operation: &Operation, reply: &Reply, what: &str) {
        let label = format!(
            "{} ({} {}) {what}: {}",
            operation.id, operation.method, operation.template, reply.status
        );
        let Some(declared) = operation.spec["responses"].get(reply.status.to_string()) else {
            self.failures.push(format!(
                "{label}: the document does not declare this status: {}",
                String::from_utf8_lossy(&reply.body)
            ));
            return;
        };
        self.produced.insert((operation.id.clone(), reply.status));
        let failures = check_response(&self.doc, declared, &operation.method, reply);
        self.failures.extend(
            failures
                .into_iter()
                .map(|failure| format!("{label}: {failure}")),
        );
    }

    async fn expect(&mut self, id: &str, request: Request, status: u16, what: &str) -> Reply {
        let operation = self.operation(id);
        let reply = self
            .send(&operation.method, &Self::path(&operation), &request)
            .await;
        if reply.status != status {
            self.failures.push(format!(
                "{id} {what}: expected {status}, the server answered {} {}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            ));
        }
        self.check(&operation, &reply, what);
        reply
    }

    /// The same for a request whose declared body never arrives.
    async fn expect_stalled(&mut self, id: &str, request: Request, status: u16, what: &str) {
        let operation = self.operation(id);
        let mut headers = request.headers.clone();
        headers.push(("authorization".to_owned(), format!("Bearer {TOKEN}")));
        let (address, method, path) = (
            self.address,
            operation.method.clone(),
            Self::path(&operation),
        );
        let reply =
            tokio::task::spawn_blocking(move || stalled(address, &method, &path, &headers, 16))
                .await
                .unwrap();
        if reply.status != status {
            self.failures.push(format!(
                "{id} {what}: expected {status}, the server answered {} {}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            ));
        }
        self.check(&operation, &reply, what);
    }

    /// Run one JSON operation that the document's request schema allows.
    async fn operate(&mut self, session: &str, body: Value, status: u16, what: &str) -> Reply {
        let schema = self.doc["components"]["schemas"]["Operation"].clone();
        if let Err(error) = validate(&self.doc, &schema, &body, what) {
            self.failures
                .push(format!("the request for {what} is not documented: {error}"));
        }
        let request = Request::new().session(session).body(body.to_string());
        self.expect("sessionOperation", request, status, what).await
    }

    fn error_code(reply: &Reply) -> String {
        serde_json::from_slice::<Value>(&reply.body).unwrap_or_default()["error"]["code"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }

    fn require_code(&mut self, reply: &Reply, code: &str, what: &str) {
        let actual = Self::error_code(reply);
        if actual != code {
            self.failures
                .push(format!("{what}: the code is {actual:?}, not {code:?}"));
        }
    }
}

fn path_action(action: Value) -> Value {
    json!({"operation": "path", "request": action})
}

fn open(path: &str, write: bool, create: bool) -> Value {
    json!({"operation": "open", "path": path, "mode": 0o644, "flags": {
        "read": true, "write": write, "create": create,
        "truncate": false, "append": false, "exclusive": false}})
}

/// Everything the cluster document says about the session routes, against
/// the server at `server`. Its drive `main` must be empty, and its limits
/// `SESSION_LIMITS`. `slow` is a server whose request timeout is shorter
/// than the session routes' own wait for a body.
async fn session_suite(server: &Server, slow: Option<&Server>) -> Sessions {
    let mut suite = Sessions::new(server);
    let created = suite
        .expect("createSession", Request::new(), 201, "a session")
        .await;
    let created: Value = serde_json::from_slice(&created.body).unwrap_or_default();
    let session = created["session"].as_str().unwrap_or_default().to_owned();
    let s = session.as_str();

    // -- routes and methods: exactly the documented ones
    for template in suite.doc["paths"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>()
    {
        let documented: BTreeSet<&str> = METHODS
            .into_iter()
            .filter(|method| {
                suite.doc["paths"][&template]
                    .get(method.to_ascii_lowercase())
                    .is_some()
            })
            .collect();
        let path = template.replace("{id}", "main");
        for method in METHODS {
            // A method the path does not have is refused before the
            // session is looked up, so a session nobody holds will do; a
            // real one would be closed by the DELETE.
            let request = Request::new().session(&"f".repeat(64));
            let reply = suite.send(method, &path, &request).await;
            let what = format!("{method} {template}");
            if reply.status != 405 {
                if !documented.contains(method) {
                    suite.failures.push(format!(
                        "{what}: the server answers {}, but the document has no such method",
                        reply.status
                    ));
                }
                continue;
            }
            if documented.contains(method) {
                suite
                    .failures
                    .push(format!("{what}: documented, but the server answers 405"));
            }
            let allowed: BTreeSet<&str> = reply
                .header("allow")
                .unwrap_or_default()
                .split(',')
                .map(str::trim)
                .collect();
            if allowed != documented {
                suite.failures.push(format!(
                    "{what}: Allow is {allowed:?}, the document has {documented:?}"
                ));
            }
            let declaring = suite
                .operations
                .iter()
                .find(|operation| {
                    operation.template == template
                        && operation.spec["responses"].get("405").is_some()
                })
                .cloned();
            match declaring {
                None => suite
                    .failures
                    .push(format!("{what}: no operation of the path declares 405")),
                Some(operation) => {
                    let declared = operation.spec["responses"]["405"].clone();
                    let failures = check_response(&suite.doc, &declared, method, &reply);
                    suite
                        .failures
                        .extend(failures.into_iter().map(|f| format!("{what}: 405: {f}")));
                    suite.produced.insert((operation.id.clone(), 405));
                }
            }
        }
    }
    // A path under the session routes that the document does not name is
    // no route at all: it gets the 404 of the file routes.
    for path in ["/v1/drives/main/sessions/x", "/v1/drives/main/session"] {
        let reply = suite.send("POST", path, &Request::new().session(s)).await;
        if reply.status != 404 || reply.message().as_deref() != Some("drive or route not found") {
            suite.failures.push(format!(
                "POST {path}: expected the 404 of an unknown route, got {} {}",
                reply.status,
                String::from_utf8_lossy(&reply.body)
            ));
        }
    }

    // -- createSession
    for (request, what) in [
        (Request::new().body("x"), "a body"),
        (Request::new().session(s), "a session header"),
        (Request::new().query("?x=1"), "a query"),
    ] {
        suite.expect("createSession", request, 400, what).await;
    }
    for (request, what) in [
        (Request::new().anonymous(), "no token"),
        (Request::new().token("short"), "a short token"),
        (
            Request::new().token("another-token-of-the-right-shape-0123456789"),
            "a wrong token",
        ),
    ] {
        let reply = suite.expect("createSession", request, 401, what).await;
        if reply.header("www-authenticate") != Some("Bearer") {
            suite
                .failures
                .push(format!("createSession {what}: no challenge"));
        }
    }
    suite
        .expect_stalled(
            "createSession",
            Request::new(),
            408,
            "a body that never arrives",
        )
        .await;
    let declared = Request::new().body(vec![b' '; SESSION_MAX_REQUEST_BYTES + 1]);
    let reply = suite
        .expect(
            "createSession",
            declared,
            413,
            "a declared body over the limit",
        )
        .await;
    if reply.header("connection") != Some("close") {
        suite
            .failures
            .push("createSession 413: the connection is not closed".to_owned());
    }

    // -- operations on paths
    let ok = [
        (
            json!({"action": "mkdir", "path": "/d", "recursive": false}),
            200,
        ),
        (
            json!({"action": "mkdir", "path": "/e/f", "recursive": true, "mode": 0o755}),
            200,
        ),
        (json!({"action": "stat", "path": "/d"}), 200),
        (json!({"action": "lstat", "path": "/d"}), 200),
        (json!({"action": "statfs", "path": "/"}), 200),
        (json!({"action": "readdir", "path": "/"}), 200),
        (
            json!({"action": "symlink", "target": "d", "path": "/l"}),
            204,
        ),
        (json!({"action": "readlink", "path": "/l"}), 200),
        (json!({"action": "chmod", "path": "/d", "mode": 0o700}), 204),
        (json!({"action": "syncfs"}), 204),
        (json!({"action": "rmdir", "path": "/e/f"}), 204),
    ];
    for (action, status) in ok {
        let what = format!("path {}", action["action"].as_str().unwrap());
        suite.operate(s, path_action(action), status, &what).await;
    }
    let reply = suite
        .operate(
            s,
            path_action(json!({"action": "stat", "path": "/none"})),
            404,
            "a missing path",
        )
        .await;
    suite.require_code(&reply, "ENOENT", "a missing path");
    let again = path_action(json!({"action": "mkdir", "path": "/d", "recursive": false}));
    let reply = suite
        .operate(s, again, 500, "a directory that exists")
        .await;
    suite.require_code(&reply, "EEXIST", "a directory that exists");

    // -- handles
    let reply = suite
        .operate(s, open("/d/file", true, true), 200, "open")
        .await;
    let handle = serde_json::from_slice::<Value>(&reply.body).unwrap_or_default()["handle"]
        .as_u64()
        .unwrap_or(u64::MAX);
    let write = |position: u64| {
        Request::new()
            .session(s)
            .header("x-mount-handle", handle)
            .header("x-mount-position", position)
    };
    let reply = suite
        .expect("writeHandle", write(0).body("hello world"), 200, "a write")
        .await;
    if serde_json::from_slice::<Value>(&reply.body).unwrap_or_default()["written"] != 11 {
        suite
            .failures
            .push("a write did not report 11 bytes".to_owned());
    }
    let read = |position: u64, length: u64| {
        Request::new()
            .session(s)
            .header("x-mount-handle", handle)
            .header("x-mount-position", position)
            .header("x-mount-length", length)
    };
    let reply = suite
        .expect("readHandle", read(6, 100), 200, "a read")
        .await;
    if reply.body != b"world" {
        suite
            .failures
            .push(format!("a positional read returned {:?}", reply.body));
    }
    let reply = suite
        .expect("readHandle", read(600, 10), 200, "a read past the end")
        .await;
    if !reply.body.is_empty() {
        suite
            .failures
            .push("a read past the end returned bytes".to_owned());
    }
    let reply = suite
        .operate(
            s,
            json!({"operation": "read", "handle": handle, "length": 5, "position": 0}),
            200,
            "a read operation",
        )
        .await;
    if reply.body != b"hello" || reply.header("content-type") != Some("application/octet-stream") {
        suite
            .failures
            .push("a read operation did not return bytes".to_owned());
    }
    for (operation, status, what) in [
        (json!({"operation": "capabilities"}), 200, "capabilities"),
        (json!({"operation": "stat", "handle": handle}), 200, "stat"),
        (
            json!({"operation": "truncate", "handle": handle, "length": 5}),
            204,
            "truncate",
        ),
        (
            json!({"operation": "sync", "handle": handle, "data_only": false}),
            204,
            "sync",
        ),
    ] {
        suite.operate(s, operation, status, what).await;
    }
    // The rest of the path actions, on the file just written.
    for action in [
        json!({"action": "truncate", "path": "/d/file", "length": 4}),
        json!({"action": "utimes", "path": "/d/file", "atime_ms": 1000, "mtime_ms": 2000}),
        json!({"action": "link", "source": "/d/file", "destination": "/d/hard"}),
        json!({"action": "rename", "source": "/d/hard", "destination": "/d/moved"}),
        json!({"action": "unlink", "path": "/d/moved"}),
    ] {
        let what = format!("path {}", action["action"].as_str().unwrap());
        suite.operate(s, path_action(action), 204, &what).await;
    }
    // Actions a drive may or may not support: whatever the answer, it
    // must be one the document declares.
    for action in [
        json!({"action": "chown", "path": "/d/file", "uid": 0, "gid": 0}),
        json!({"action": "lchown", "path": "/l", "uid": 0, "gid": 0}),
        json!({"action": "lutimes", "path": "/l", "atime_ms": 1000, "mtime_ms": 2000}),
        json!({"action": "utimens", "path": "/d/file", "atime_ns": "1000000",
            "mtime_ns": "2000000", "follow_symlinks": true}),
        json!({"action": "mknod", "path": "/d/fifo", "mode": 0o010644, "dev": 0}),
    ] {
        let what = format!("path {}", action["action"].as_str().unwrap());
        let body = path_action(action);
        let schema = suite.doc["components"]["schemas"]["Operation"].clone();
        if let Err(error) = validate(&suite.doc, &schema, &body, &what) {
            suite
                .failures
                .push(format!("{what} is not documented: {error}"));
        }
        let operation = suite.operation("sessionOperation");
        let request = Request::new().session(s).body(body.to_string());
        let reply = suite
            .send("POST", &Sessions::path(&operation), &request)
            .await;
        suite.check(&operation, &reply, &what);
    }

    // -- a whole-file write, and that the file routes see what sessions wrote
    let whole = |path: &str| Request::new().session(s).header("x-mount-write-file", path);
    suite
        .expect(
            "writeHandle",
            whole("/d/whole%20file").body("whole"),
            204,
            "a whole file",
        )
        .await;
    let seen = reqwest::Client::new()
        .get(format!("{}/v1/drives/main/fs/d/whole%20file", suite.base))
        .header("authorization", format!("Bearer {TOKEN}"))
        .send()
        .await
        .unwrap();
    if seen.status() != 200 || seen.bytes().await.unwrap().as_ref() != b"whole" {
        suite
            .failures
            .push("the file routes do not see a file written through a session".to_owned());
    }
    let reply = suite
        .expect(
            "writeHandle",
            whole("/none/x").body("x"),
            404,
            "a missing parent",
        )
        .await;
    suite.require_code(&reply, "ENOENT", "a missing parent");
    let reply = suite
        .expect(
            "writeHandle",
            whole("/d").body("x"),
            500,
            "a whole file onto a directory",
        )
        .await;
    suite.require_code(&reply, "EISDIR", "a whole file onto a directory");

    // -- malformed requests
    for (request, what) in [
        (Request::new().session(s).body("x"), "no handle"),
        (
            whole("/d/x").header("x-mount-handle", handle).body("x"),
            "a path and a handle",
        ),
        (whole("/bad%zz").body("x"), "a bad escape"),
        (whole("relative").body("x"), "a relative path"),
        (
            Request::new()
                .session(s)
                .header("x-mount-handle", "-1")
                .body("x"),
            "a handle that is not a number",
        ),
        (
            Request::new().header("x-mount-handle", handle).body("x"),
            "no session header",
        ),
        (write(0).query("?x=1").body("x"), "a query"),
    ] {
        let reply = suite.expect("writeHandle", request, 400, what).await;
        suite.require_code(&reply, "EINVAL", what);
    }
    for (request, what) in [
        (
            Request::new()
                .session(s)
                .header("x-mount-handle", handle)
                .header("x-mount-length", 1),
            "no position",
        ),
        (
            Request::new()
                .session(s)
                .header("x-mount-handle", handle)
                .header("x-mount-position", 0),
            "no length",
        ),
        (read(0, 1).session(s), "two session headers"),
        (
            Request::new()
                .session("not-a-session-token")
                .header("x-mount-handle", handle)
                .header("x-mount-position", 0)
                .header("x-mount-length", 1),
            "a malformed session token",
        ),
    ] {
        suite.expect("readHandle", request, 400, what).await;
    }
    for (body, what) in [
        ("{".to_owned(), "not JSON"),
        (
            json!({"operation": "stat", "handle": handle, "extra": 1}).to_string(),
            "an unknown property",
        ),
        (
            json!({"operation": "fly"}).to_string(),
            "an unknown operation",
        ),
        (
            path_action(json!({"action": "stat", "path": "relative"})).to_string(),
            "a relative path",
        ),
    ] {
        let request = Request::new().session(s).body(body);
        suite.expect("sessionOperation", request, 400, what).await;
    }
    let with_header = Request::new()
        .session(s)
        .header("x-mount-handle", handle)
        .body(json!({"operation": "capabilities"}).to_string());
    suite
        .expect("sessionOperation", with_header, 400, "a handle header")
        .await;

    // -- authentication and the session
    let unknown = "0".repeat(64);
    for id in ["readHandle", "sessionOperation", "writeHandle"] {
        let request = |session: &str| match id {
            "readHandle" => Request::new()
                .session(session)
                .header("x-mount-handle", handle)
                .header("x-mount-position", 0)
                .header("x-mount-length", 1),
            "writeHandle" => Request::new()
                .session(session)
                .header("x-mount-handle", handle)
                .body("x"),
            _ => Request::new()
                .session(session)
                .body(json!({"operation": "capabilities"}).to_string()),
        };
        let reply = suite
            .expect(id, request(s).anonymous(), 401, "no token")
            .await;
        if reply.header("www-authenticate") != Some("Bearer") {
            suite.failures.push(format!("{id}: no challenge"));
        }
        let reply = suite
            .expect(id, request(&unknown), 403, "an unknown session")
            .await;
        suite.require_code(&reply, "EACCES", "an unknown session");
        let over = request(s).body(vec![b' '; SESSION_MAX_REQUEST_BYTES + 1]);
        let reply = suite
            .expect(id, over, 413, "a declared body over the limit")
            .await;
        if reply.header("connection") != Some("close") {
            suite
                .failures
                .push(format!("{id} 413: the connection is not closed"));
        }
    }

    // -- limits
    let reply = suite
        .expect("readHandle", read(0, 65537), 413, "a read over the limit")
        .await;
    suite.require_code(&reply, "EFBIG", "a read over the limit");
    let reply = suite
        .expect(
            "writeHandle",
            write(0).body(vec![b'x'; 65537]),
            413,
            "a write over the limit",
        )
        .await;
    suite.require_code(&reply, "EFBIG", "a write over the limit");
    let request = Request::new().session(s).body(vec![b' '; 8193]);
    let reply = suite
        .expect(
            "sessionOperation",
            request,
            413,
            "an operation over the limit",
        )
        .await;
    suite.require_code(&reply, "EFBIG", "an operation over the limit");
    suite
        .operate(s, open("/d/file", false, false), 200, "a second handle")
        .await;
    let reply = suite
        .operate(
            s,
            open("/d/file", false, false),
            429,
            "a handle over the limit",
        )
        .await;
    suite.require_code(&reply, "EMFILE", "a handle over the limit");

    // -- bodies that never arrive
    suite
        .expect_stalled(
            "sessionOperation",
            Request::new().session(s),
            408,
            "a body that never arrives",
        )
        .await;
    suite
        .expect_stalled("writeHandle", write(0), 408, "a body that never arrives")
        .await;
    suite
        .expect_stalled("readHandle", read(0, 1), 408, "a body that never arrives")
        .await;
    suite
        .expect_stalled(
            "closeSession",
            Request::new().session(s),
            408,
            "a body that never arrives",
        )
        .await;

    // -- closed handles and sessions
    let reply = suite
        .operate(
            s,
            json!({"operation": "stat", "handle": 999}),
            409,
            "an unknown handle",
        )
        .await;
    suite.require_code(&reply, "EBADF", "an unknown handle");
    suite
        .operate(
            s,
            json!({"operation": "close", "handle": handle}),
            204,
            "close",
        )
        .await;
    let reply = suite
        .expect("readHandle", read(0, 1), 409, "a closed handle")
        .await;
    suite.require_code(&reply, "EBADF", "a closed handle");
    let reply = suite
        .expect("writeHandle", write(0).body("x"), 409, "a closed handle")
        .await;
    suite.require_code(&reply, "EBADF", "a closed handle");

    // -- the session limit: this is the first of `max_sessions_per_drive`
    let mut others = Vec::new();
    for _ in 1..SESSIONS_PER_DRIVE {
        let reply = suite
            .expect("createSession", Request::new(), 201, "another session")
            .await;
        let body: Value = serde_json::from_slice(&reply.body).unwrap_or_default();
        others.push(body["session"].as_str().unwrap_or_default().to_owned());
    }
    let reply = suite
        .expect(
            "createSession",
            Request::new(),
            429,
            "a session over the limit",
        )
        .await;
    suite.require_code(&reply, "ENFILE", "a session over the limit");

    // -- closeSession
    for (request, what) in [
        (Request::new(), "no session header"),
        (Request::new().session("zz"), "a malformed session token"),
        (Request::new().session(s).body("x"), "a body"),
        (Request::new().session(s).query("?x=1"), "a query"),
    ] {
        suite.expect("closeSession", request, 400, what).await;
    }
    suite
        .expect(
            "closeSession",
            Request::new().session(s).anonymous(),
            401,
            "no token",
        )
        .await;
    suite
        .expect(
            "closeSession",
            Request::new().session(&unknown),
            403,
            "an unknown session",
        )
        .await;
    let over = Request::new()
        .session(s)
        .body(vec![b' '; SESSION_MAX_REQUEST_BYTES + 1]);
    suite
        .expect("closeSession", over, 413, "a declared body over the limit")
        .await;
    suite
        .expect("closeSession", Request::new().session(s), 204, "close")
        .await;
    let reply = suite
        .expect(
            "closeSession",
            Request::new().session(s),
            403,
            "close again",
        )
        .await;
    suite.require_code(&reply, "EACCES", "close again");
    let after = Request::new()
        .session(s)
        .body(json!({"operation": "capabilities"}).to_string());
    suite
        .expect("sessionOperation", after, 403, "a closed session")
        .await;
    for other in &others {
        suite
            .expect("closeSession", Request::new().session(other), 204, "close")
            .await;
    }

    // -- the listener's own request timeout, shorter than the 5 s above
    if let Some(slow) = slow {
        let mut timed = Sessions::new(slow);
        let created = timed
            .expect("createSession", Request::new(), 201, "a session")
            .await;
        let created: Value = serde_json::from_slice(&created.body).unwrap_or_default();
        let session = created["session"].as_str().unwrap_or_default().to_owned();
        let named = || Request::new().session(&session);
        timed
            .expect_stalled("createSession", Request::new(), 504, "the request timeout")
            .await;
        timed
            .expect_stalled("sessionOperation", named(), 504, "the request timeout")
            .await;
        timed
            .expect_stalled(
                "writeHandle",
                named().header("x-mount-handle", 1),
                504,
                "the request timeout",
            )
            .await;
        timed
            .expect_stalled("closeSession", named(), 504, "the request timeout")
            .await;
        let read = named()
            .header("x-mount-handle", 1)
            .header("x-mount-position", 0)
            .header("x-mount-length", 1);
        timed
            .expect_stalled("readHandle", read, 504, "the request timeout")
            .await;
        suite.produced.extend(timed.produced);
        suite.failures.extend(timed.failures);
    }
    suite
}

/// Fail for every response the document declares that the suite did not
/// produce and `NOT_REPRODUCED` does not excuse. `also` excuses more, for
/// a run that cannot reach them.
fn require_every_declared_response(suite: &mut Sessions, also: &[(&str, u16)]) {
    let mut exempt: BTreeSet<(String, u16)> = NOT_REPRODUCED
        .into_iter()
        .map(|(id, status, _)| (id.to_owned(), status))
        .collect();
    for (id, status) in also {
        suite.produced.insert(((*id).to_owned(), *status));
    }
    for operation in &suite.operations {
        for status in operation.declared() {
            let pair = (operation.id.clone(), status);
            if !suite.produced.contains(&pair) && !exempt.remove(&pair) {
                suite.failures.push(format!(
                    "{} declares {status}, which no request here produces",
                    operation.id
                ));
            }
        }
    }
    for (id, status) in exempt {
        suite.failures.push(format!(
            "NOT_REPRODUCED lists {id} {status}, which is produced or no longer declared"
        ));
    }
}

const SESSION_MAX_REQUEST_BYTES: usize = 131072;
const SESSIONS_PER_DRIVE: usize = 3;

/// A server for the session suite: a body limit above the session routes'
/// own, and session limits small enough to reach.
fn session_http() -> Value {
    json!({
        "host": "127.0.0.1",
        "port": 0,
        "max_request_bytes": SESSION_MAX_REQUEST_BYTES,
        "remote_sessions": true,
        "sessions": {"max_sessions_per_drive": SESSIONS_PER_DRIVE, "max_handles_per_session": 2},
        "drives": [drive()],
    })
}

// ------------------------------------------------------------------- tests

#[test]
fn the_examples_in_the_cluster_document_match_its_schemas() {
    let doc = openapi::parse(CLUSTER);
    let mut found = Vec::new();
    openapi::examples(&doc, String::new(), &mut found);
    assert!(found.len() > 10, "the document's examples were not found");
    let failures: Vec<String> = found
        .into_iter()
        .filter_map(|(at, schema, example)| validate(&doc, schema, example, &at).err())
        .collect();
    report(&failures);
    // Each forwarding response names a response the document defines.
    let forwarding = doc["x-forwarding-responses"]["responses"]
        .as_object()
        .expect("the forwarding responses");
    assert_eq!(forwarding.len(), 3);
    for (status, response) in forwarding {
        let resolved = openapi::resolve(&doc, response);
        assert!(resolved["content"].is_object(), "{status} resolves");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_file_routes_answer_the_same_with_remote_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let doc = openapi::parse(V1);
    let plain = Server::start(dir.path(), "plain", sequence_http(json!([drive()])), None);
    let sessions = Server::start(
        dir.path(),
        "sessions",
        with(
            sequence_http(json!([drive()])),
            "remote_sessions",
            json!(true),
        ),
        None,
    );
    let reference =
        blackbox::run(&plain.target(true, UnknownDrive::Unauthorized), &doc, None).await;
    report(&reference.failures);
    let with_sessions = blackbox::run(
        &sessions.target(true, UnknownDrive::Unauthorized),
        &doc,
        None,
    )
    .await;
    report(&with_sessions.failures);
    let differences = blackbox::differences(&reference, &with_sessions, false);
    assert!(
        differences.is_empty(),
        "remote_sessions changed {} answers of the file routes:\n{}",
        differences.len(),
        differences.join("\n")
    );
    plain.stop("the plain server");
    sessions.stop("the server with remote_sessions");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_session_routes_match_the_cluster_document() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::start(dir.path(), "sessions", session_http(), None);
    let slow = Server::start(
        dir.path(),
        "slow",
        with(session_http(), "request_timeout_ms", json!(1000)),
        None,
    );
    let mut suite = session_suite(&server, Some(&slow)).await;
    require_every_declared_response(&mut suite, &[]);
    report(&suite.failures);
    server.stop("the server with remote_sessions");
    slow.stop("the server with a short request timeout");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_server_without_remote_sessions_has_no_session_routes() {
    let dir = tempfile::tempdir().unwrap();
    let plain = Server::start(dir.path(), "plain", sequence_http(json!([drive()])), None);
    let client = reqwest::Client::new();
    for path in [
        "/v1/drives/main/sessions",
        "/v1/drives/main/sessions/handles",
    ] {
        for method in ["GET", "POST", "PUT", "DELETE"] {
            let response = client
                .request(method.parse().unwrap(), format!("{}{path}", plain.base))
                .header("authorization", format!("Bearer {TOKEN}"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 404, "{method} {path}");
            let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            assert_eq!(body["error"]["message"], "drive or route not found");
        }
    }
    plain.stop("the plain server");
}

/// One forwarding response, checked against the cluster document.
fn check_forwarding(doc: &Value, name: &str, method: &str, reply: &Reply, what: &str) {
    let declared = &doc["components"]["responses"][name];
    assert!(declared.is_object(), "the cluster document has no {name}");
    let status = doc["x-forwarding-responses"]["responses"]
        .as_object()
        .unwrap()
        .iter()
        .find(|(_, response)| {
            response["$ref"].as_str() == Some(&format!("#/components/responses/{name}"))
        })
        .map(|(status, _)| status.parse::<u16>().unwrap())
        .unwrap_or_else(|| panic!("x-forwarding-responses does not list {name}"));
    assert_eq!(
        reply.status,
        status,
        "{what}: {}",
        String::from_utf8_lossy(&reply.body)
    );
    let failures = check_response(doc, declared, method, reply);
    assert!(failures.is_empty(), "{what}: {failures:?}");
}

async fn fetch(base: &str, method: &str, path: &str, headers: &[(&str, &str)]) -> Reply {
    let mut request = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("{base}{path}"))
        .header("authorization", format!("Bearer {TOKEN}"));
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await.unwrap();
    let status = response.status().as_u16();
    let headers = response.headers().clone();
    let body = response.bytes().await.unwrap();
    Reply::collect(status, &headers, &body)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn the_file_routes_answer_the_same_on_an_owner_and_through_an_ingress() {
    let dir = tempfile::tempdir().unwrap();
    let doc = openapi::parse(V1);
    let cluster_doc = openapi::parse(CLUSTER);
    let plain = Server::start(dir.path(), "plain", sequence_http(json!([drive()])), None);
    let reference =
        blackbox::run(&plain.target(true, UnknownDrive::Unauthorized), &doc, None).await;
    report(&reference.failures);
    plain.stop("the plain server");

    // An owner answering for its own drive on its public listener.
    let alone = Cluster::new(&["a"]);
    let owner = alone.start("a", sequence_http(json!([drive()])), true);
    let on_owner = blackbox::run(
        &owner.target(true, UnknownDrive::Unrouted),
        &doc,
        Some(&cluster_doc),
    )
    .await;
    report(&on_owner.failures);
    let differences = blackbox::differences(&reference, &on_owner, false);
    assert!(
        differences.is_empty(),
        "a forwarding owner changed {} answers of the file routes:\n{}",
        differences.len(),
        differences.join("\n")
    );
    owner.stop("the owner");

    // An ingress that holds no drive, forwarding every request to the owner.
    let pair = Cluster::new(&["a", "b"]);
    let owner = pair.start("a", sequence_http(json!([drive()])), true);
    let ingress = pair.start("b", sequence_http(json!([])), false);
    let through_ingress = blackbox::run(
        &ingress.target(false, UnknownDrive::Unrouted),
        &doc,
        Some(&cluster_doc),
    )
    .await;
    report(&through_ingress.failures);
    let differences = blackbox::differences(&reference, &through_ingress, true);
    assert!(
        differences.is_empty(),
        "forwarding changed {} answers of the file routes:\n{}",
        differences.len(),
        differences.join("\n")
    );

    // The owner sees what was written through the ingress.
    let direct = fetch(&owner.base, "GET", "/v1/drives/main/fs/dir/new.txt", &[]).await;
    assert_eq!(
        (direct.status, direct.body.as_slice()),
        (200, &b"newer"[..])
    );

    // A reserved header is refused by every forwarding server, for a drive
    // of its own and for one it would forward.
    for (server, what) in [(&owner, "the owner"), (&ingress, "the ingress")] {
        for header in ["x-mount-hop", "x-mount-owner-route"] {
            let reply = fetch(
                &server.base,
                "GET",
                "/v1/drives/main/fs/dir/a.txt",
                &[(header, "1")],
            )
            .await;
            check_forwarding(
                &cluster_doc,
                "ForwardingRejected",
                "GET",
                &reply,
                &format!("{header} to {what}"),
            );
        }
    }
    ingress.stop("the ingress");
    owner.stop("the owner");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn the_session_routes_match_the_cluster_document_through_an_ingress() {
    let pair = Cluster::new(&["a", "b"]);
    let owner = pair.start("a", session_http(), true);
    let ingress = pair.start(
        "b",
        json!({"host": "127.0.0.1", "port": 0,
            "max_request_bytes": SESSION_MAX_REQUEST_BYTES, "drives": []}),
        false,
    );
    let mut suite = session_suite(&ingress, None).await;
    // Not reached here: the ingress and the owner have the default 30 s
    // request timeout. The run against one server covers them.
    require_every_declared_response(
        &mut suite,
        &[
            ("createSession", 504),
            ("closeSession", 504),
            ("readHandle", 504),
            ("sessionOperation", 504),
            ("writeHandle", 504),
        ],
    );
    report(&suite.failures);
    ingress.stop("the ingress");
    owner.stop("the owner");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn a_forwarding_server_answers_for_itself_and_says_when_it_could_not_forward() {
    let doc = openapi::parse(V1);
    let cluster_doc = openapi::parse(CLUSTER);
    let pair = Cluster::new(&["a", "b"]);
    let owner = pair.start("a", sequence_http(json!([drive()])), true);
    let ingress = pair.start("b", sequence_http(json!([])), false);

    // Probes, discovery and unknown paths are each server's own to answer.
    for (server, drives, what) in [(&owner, 1, "the owner"), (&ingress, 0, "the ingress")] {
        let health = fetch(&server.base, "GET", "/healthz", &[]).await;
        assert_eq!(health.status, 200, "{what}");
        assert_eq!(health.body, br#"{"status":"ok"}"#, "{what}");
        let ready = fetch(&server.base, "GET", "/readyz", &[]).await;
        assert_eq!(ready.status, 200, "{what}");
        let body: Value = serde_json::from_slice(&ready.body).unwrap();
        assert_eq!(body, json!({"status": "ready", "drives": drives}), "{what}");
        // The v1 document for a server that holds a drive; the cluster
        // document's for one that holds none.
        let (against, declared) = if drives > 0 {
            (&doc, &doc["paths"]["/readyz"]["get"]["responses"]["200"])
        } else {
            let declared = &cluster_doc["components"]["responses"]["ForwardingReady"];
            (&cluster_doc, declared)
        };
        assert!(declared.is_object(), "{what}: no declared readiness");
        let failures = check_response(against, declared, "GET", &ready);
        assert!(failures.is_empty(), "{what}: {failures:?}");
        let missing = fetch(&server.base, "GET", "/v1/drives/main", &[]).await;
        assert_eq!(missing.status, 404, "{what}");
        assert_eq!(
            missing.message().as_deref(),
            Some("drive or route not found"),
            "{what}"
        );
    }
    // Discovery lists the drives a server holds itself, and no others.
    let listed = fetch(&owner.base, "GET", "/v1/drives", &[]).await;
    assert_eq!(listed.body, br#"[{"id":"main"}]"#);
    let unlisted = fetch(&ingress.base, "GET", "/v1/drives", &[]).await;
    assert_eq!(unlisted.status, 401);

    // No owner: nothing ran, and the client is told to ask again.
    for server in [&owner, &ingress] {
        let reply = fetch(&server.base, "PUT", "/v1/drives/elsewhere/fs/x", &[]).await;
        check_forwarding(
            &cluster_doc,
            "ForwardingUnavailable",
            "PUT",
            &reply,
            "a drive without an owner",
        );
    }

    // An owner that died with its route still published: the ingress gave
    // it the request and cannot know what became of it.
    let written = fetch(&ingress.base, "POST", "/v1/drives/main/ops/sync", &[]).await;
    assert_eq!(written.status, 204, "the ingress reaches the owner");
    owner.crash();
    let started = std::time::Instant::now();
    let reply = fetch(&ingress.base, "GET", "/v1/drives/main/stat", &[]).await;
    check_forwarding(
        &cluster_doc,
        "ForwardingOutcomeUnknown",
        "GET",
        &reply,
        "an owner that crashed",
    );
    // Once the dead owner's route has expired the answer is the 503 again.
    let deadline = started + Duration::from_secs(90);
    loop {
        let reply = fetch(&ingress.base, "GET", "/v1/drives/main/stat", &[]).await;
        if reply.status == 503 {
            check_forwarding(
                &cluster_doc,
                "ForwardingUnavailable",
                "GET",
                &reply,
                "an owner whose route expired",
            );
            break;
        }
        check_forwarding(
            &cluster_doc,
            "ForwardingOutcomeUnknown",
            "GET",
            &reply,
            "an owner that crashed",
        );
        assert!(
            std::time::Instant::now() < deadline,
            "the crashed owner's route did not expire"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let ready = fetch(&ingress.base, "GET", "/readyz", &[]).await;
    assert_eq!(ready.status, 200, "the ingress is ready without the owner");
    ingress.stop("the ingress");
}

#[test]
fn the_cluster_example_configs_are_valid() {
    let examples = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/cluster");
    let mut checked = 0;
    for entry in std::fs::read_dir(&examples).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_str().unwrap().to_owned();
        if !name.starts_with("node-") || !name.ends_with(".json") {
            continue;
        }
        let output = Command::new(env!("CARGO_BIN_EXE_mount-rs"))
            .arg("validate-config")
            .arg("--config")
            .arg(&path)
            .env_remove("MOUNT_RS_CLUSTER_REDIS_URL")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{name}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        checked += 1;
    }
    assert_eq!(checked, 3, "a config for each of the three nodes");
    // The membership file the configs name is one the cluster accepts.
    let members = std::fs::read(examples.join("peers.json")).unwrap();
    let directory = mount_rs_cluster::PeerDirectory::parse(members.as_slice())
        .expect("the example membership file is valid");
    assert_eq!(directory.len(), 3);
}
