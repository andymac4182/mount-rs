//! Token-file rotation and the metrics listener on `serve-http` servers
//! with cluster features, checked with the real binary.
//!
//! - A drive whose token is a file is rotated without a restart on a server
//!   with `remote_sessions`, on the owner of a drive in a forwarding
//!   cluster (directly and through an ingress), and on failover candidates,
//!   including one that was a standby when its file changed. The file
//!   routes and the session routes follow the same token set; a session
//!   belongs to the token that made it, is out of reach once that token is
//!   dropped, and is then closed by idle expiry.
//! - The metrics of each server count what that server did: an ingress
//!   counts a request it forwards once, the owner counts it once more and
//!   says so in `mount_rs_http_forwarded_requests_total`, the session
//!   routes are counted under their own route templates, and an owner
//!   reports the readiness of the drives it owns.
//!
//! The tests that need a forwarding cluster need Redis: they are ignored
//! unless run with `--include-ignored` and `MOUNT_RS_REDIS_TEST_URL`.
#![cfg(unix)]

mod common;

use common::{Server, authority, free_udp, peer, unique, wait_until};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

/// Tokens in the format `remote_sessions` requires.
const OLD: &str = "rotation-old-token-0123456789abcdef";
const NEW: &str = "rotation-new-token-0123456789abcdef";
const NEWER: &str = "rotation-newer-token-0123456789abcd";
const REDIS_ENV: &str = "CLUSTER_OPERATIONS_REDIS_URL";
const LIMIT: Duration = Duration::from_secs(60);

const STAT: &str = "/v1/drives/{id}/stat/{path}";
const FS: &str = "/v1/drives/{id}/fs/{path}";
const SESSIONS: &str = "/v1/drives/{id}/sessions";
const HANDLES: &str = "/v1/drives/{id}/sessions/handles";

/// The config directory of one test: token files, and for a forwarding
/// cluster a CA, a certificate per node and the membership file, under a
/// Redis namespace of its own.
struct Files {
    dir: tempfile::TempDir,
    namespace: String,
    redis: Option<String>,
}

impl Files {
    /// `nodes` are the members of a forwarding cluster; none for a server
    /// that stands alone.
    fn new(nodes: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, content: String| {
            std::fs::write(dir.path().join(name), content).unwrap();
        };
        let mut redis = None;
        if !nodes.is_empty() {
            redis = Some(std::env::var("MOUNT_RS_REDIS_TEST_URL").expect("isolated Redis URL"));
            let ca = authority("mount-rs cluster operations test CA");
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
                    "node": node, "boot": format!("{node}-boot"), "address": free_udp(),
                    "tls_server_name": "localhost", "spki_sha256": pin,
                }));
            }
            write("peers.json", Value::Array(members).to_string());
            std::fs::create_dir(dir.path().join("shared")).unwrap();
        }
        Self {
            dir,
            namespace: unique("cluster-operations"),
            redis,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
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

    /// Replace `node`'s token file as an operator does: a new file renamed
    /// into place, so the server never reads half of one.
    fn write_tokens(&self, node: &str, tokens: &[&str]) {
        let staged = self.path(&format!("{node}.token.staged"));
        std::fs::write(&staged, tokens.join("\n") + "\n").unwrap();
        std::fs::rename(staged, self.path(&format!("{node}.token"))).unwrap();
    }

    /// Start `node` on `http`, with the metrics listener on and the token
    /// files looked at every second.
    fn start(&self, node: &str, mut http: Value) -> Node {
        http["host"] = "127.0.0.1".into();
        http["port"] = 0.into();
        http["metrics"] = json!({"listen": "127.0.0.1:0"});
        if http["drives"]
            .as_array()
            .is_some_and(|drives| !drives.is_empty())
        {
            http["token_reload_seconds"] = 1.into();
        }
        let config = self.path(&format!("{node}.json"));
        std::fs::write(&config, json!({"version": 1, "http": http}).to_string()).unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_mount-rs"));
        command
            .current_dir(self.dir.path())
            .args(["serve-http", "--config"])
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(redis) = &self.redis {
            command.env(REDIS_ENV, redis);
        }
        let server = Server::spawn(&mut command, node);
        // Written before the drives are opened, so it is there by now.
        let metrics = server
            .stderr()
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|record| record["event"] == "http_metrics_listening")
            .and_then(|record| record["address"].as_str()?.parse().ok())
            .expect("the metrics listener's address on stderr");
        Node {
            server,
            metrics,
            client: reqwest::Client::new(),
        }
    }
}

/// A drive of `node` whose token is the file `<node>.token`.
fn drive(node: &str, id: &str, storage: Value) -> Value {
    json!({
        "id": id,
        "token": {"file": format!("{node}.token")},
        "driver": {"kind": "splitstore", "storage": storage},
    })
}

fn memory() -> Value {
    json!({"metadata": {"kind": "memory"}, "blocks": {"kind": "memory"}})
}

/// Storage two failover candidates share, with a short writer lease.
fn shared(node: &str) -> Value {
    json!({
        "metadata": {"kind": "sqlite", "path": "./shared/meta.db"},
        "blocks": {"kind": "sqlite", "path": "./shared/blocks.db"},
        "chunk_size_bytes": 65536, "owner": node, "lease_ttl_ms": 12000,
    })
}

struct Node {
    server: Server,
    metrics: SocketAddr,
    client: reqwest::Client,
}

#[derive(Debug, PartialEq, Eq)]
struct Reply {
    status: u16,
    body: Vec<u8>,
}

impl Node {
    async fn send(
        &self,
        method: &str,
        path: &str,
        token: &str,
        session: Option<&str>,
        body: Option<Vec<u8>>,
    ) -> Reply {
        let mut request = self
            .client
            .request(
                method.parse().unwrap(),
                format!("http://{}{path}", self.server.address),
            )
            .header("authorization", format!("Bearer {token}"));
        if let Some(session) = session {
            request = request.header("x-mount-session", session);
        }
        if let Some(body) = body {
            request = request.body(body);
        }
        let response = request.send().await.expect("response");
        Reply {
            status: response.status().as_u16(),
            body: response.bytes().await.expect("body").to_vec(),
        }
    }

    /// The status of `GET .../stat` on `drive` with `token`.
    async fn stat(&self, drive: &str, token: &str) -> u16 {
        let path = format!("/v1/drives/{drive}/stat");
        self.send("GET", &path, token, None, None).await.status
    }

    /// Open a session on `drive` with `token`.
    async fn open_session(&self, drive: &str, token: &str) -> Result<String, Reply> {
        let path = format!("/v1/drives/{drive}/sessions");
        let reply = self.send("POST", &path, token, None, None).await;
        if reply.status != 201 {
            return Err(reply);
        }
        let created: Value = serde_json::from_slice(&reply.body).unwrap();
        Ok(created["session"].as_str().unwrap().to_owned())
    }

    /// One session operation that needs no handle, in `session`.
    async fn operate(&self, drive: &str, token: &str, session: &str) -> Reply {
        let path = format!("/v1/drives/{drive}/sessions/handles");
        let body = json!({"operation": "capabilities"}).to_string();
        self.send("POST", &path, token, Some(session), Some(body.into()))
            .await
    }

    async fn close_session(&self, drive: &str, token: &str, session: &str) -> u16 {
        let path = format!("/v1/drives/{drive}/sessions");
        self.send("DELETE", &path, token, Some(session), None)
            .await
            .status
    }

    fn reloads(&self) -> Vec<Value> {
        self.server
            .stderr()
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|record| record["event"] == "http_token_reload")
            .collect()
    }

    /// Wait for the next `http_token_reload` line after the `seen` already
    /// read, and return it.
    fn next_reload(&self, seen: &mut usize) -> Value {
        wait_until("a token reload line", LIMIT, || {
            self.reloads().len() > *seen
        });
        *seen += 1;
        self.reloads()[*seen - 1].clone()
    }

    /// Wait for a reload that leaves `tokens` tokens accepted.
    fn reloaded(&self, seen: &mut usize, tokens: u64) {
        let record = self.next_reload(seen);
        assert_eq!(
            (&record["outcome"], &record["tokens"], &record["error"]),
            (&json!("reloaded"), &json!(tokens), &Value::Null),
            "{record}"
        );
    }

    async fn scrape(&self) -> String {
        let response = self
            .client
            .get(format!("http://{}/metrics", self.metrics))
            .send()
            .await
            .expect("scrape");
        assert_eq!(response.status(), 200);
        response.text().await.expect("metrics text")
    }

    /// Wait until every series of `expected` has its value: a request is
    /// counted when its response is finished with, which may be a moment
    /// after its client has read it. `None` is a series that is absent.
    async fn expect_metrics(&self, expected: &[(String, Option<f64>)]) -> String {
        let deadline = std::time::Instant::now() + LIMIT;
        loop {
            let text = self.scrape().await;
            let wrong: Vec<_> = expected
                .iter()
                .filter(|(series, value)| sample(&text, series) != *value)
                .map(|(series, value)| {
                    format!("{series}: {:?}, expected {value:?}", sample(&text, series))
                })
                .collect();
            if wrong.is_empty() {
                return text;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "metrics of server {}: {wrong:#?}\n{text}",
                self.server.name
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Stop with SIGTERM, as a deploy does, and require a clean exit that
    /// wrote no token.
    fn stop(mut self) {
        self.server.signal(libc::SIGTERM);
        let status = self.server.wait_exit(LIMIT);
        let stderr = self.server.stderr();
        assert!(status.success(), "{status}; stderr={stderr:?}");
        assert!(
            !stderr.iter().any(|line| line.contains("rotation-")),
            "no token is written to stderr: {stderr:?}"
        );
    }
}

fn sample(text: &str, series: &str) -> Option<f64> {
    text.lines()
        .find_map(|line| line.strip_prefix(series)?.strip_prefix(' '))
        .map(|value| value.parse().expect("a number"))
}

fn requests(route: &str, method: &str, status: &str, count: u32) -> (String, Option<f64>) {
    (
        format!(
            "mount_rs_http_requests_total{{route=\"{route}\",method=\"{method}\",status=\"{status}\"}}"
        ),
        Some(f64::from(count)),
    )
}

fn series(name: &str, value: Option<u32>) -> (String, Option<f64>) {
    (name.to_owned(), value.map(f64::from))
}

/// On a server with `remote_sessions` a token file is rotated without a
/// restart, for the file routes and the session routes together. A session
/// belongs to the token that made it: the other token of the overlap does
/// not reach it, a dropped token is refused at its next request, and the
/// session it left behind is closed by idle expiry. The session routes are
/// counted under their own templates.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_token_file_is_rotated_with_remote_sessions_and_the_session_routes_are_counted() {
    let files = Files::new(&[]);
    files.write_tokens("solo", &[OLD]);
    let node = files.start(
        "solo",
        json!({"remote_sessions": true,
            "sessions": {"max_sessions_per_drive": 2, "idle_timeout_seconds": 10},
            "drives": [drive("solo", "main", memory())]}),
    );
    let mut seen = 0;

    assert_eq!(node.stat("main", OLD).await, 200);
    assert_eq!(node.stat("main", NEW).await, 401);
    let never_valid = node
        .open_session("main", NEW)
        .await
        .expect_err("no session");
    assert_eq!(never_valid.status, 401);

    // The overlap: both tokens, on both kinds of route. Each token has its
    // own sessions, and both count towards the drive's limit.
    files.write_tokens("solo", &[NEW, OLD]);
    node.reloaded(&mut seen, 2);
    assert_eq!(node.stat("main", OLD).await, 200);
    assert_eq!(node.stat("main", NEW).await, 200);
    let of_old = node.open_session("main", OLD).await.expect("a session");
    let of_new = node.open_session("main", NEW).await.expect("a session");
    let full = node.open_session("main", NEW).await.expect_err("the limit");
    assert_eq!(full.status, 429);
    assert_eq!(node.operate("main", OLD, &of_old).await.status, 200);
    assert_eq!(node.operate("main", NEW, &of_old).await.status, 403);
    assert_eq!(node.operate("main", NEW, &of_new).await.status, 200);

    // The old token is dropped: it gets what a token that was never valid
    // gets, everywhere, and nothing reaches the session it made.
    files.write_tokens("solo", &[NEW]);
    node.reloaded(&mut seen, 1);
    assert_eq!(node.stat("main", OLD).await, 401);
    assert_eq!(node.stat("main", NEW).await, 200);
    assert_eq!(node.operate("main", OLD, &of_old).await.status, 401);
    assert_eq!(
        node.open_session("main", OLD)
            .await
            .expect_err("no session"),
        never_valid
    );
    assert_eq!(node.operate("main", NEW, &of_old).await.status, 403);
    assert_eq!(node.close_session("main", OLD, &of_old).await, 401);
    // The new token's session was not touched by the rotation.
    assert_eq!(node.operate("main", NEW, &of_new).await.status, 200);

    // A file with a token the session routes do not accept is refused
    // whole, so the two kinds of route never disagree.
    files.write_tokens("solo", &["too-short-for-sessions", NEW]);
    let refused = node.next_reload(&mut seen);
    assert_eq!(
        (&refused["outcome"], &refused["tokens"]),
        (&json!("refused"), &json!(1)),
        "{refused}"
    );
    let reason = refused["error"].as_str().expect("a reason");
    assert!(
        reason.contains("line 1 is not a token remote_sessions accepts"),
        "{reason}"
    );
    assert_eq!(node.stat("main", "too-short-for-sessions").await, 401);
    assert_eq!(node.stat("main", NEW).await, 200);

    let text = node
        .expect_metrics(&[
            requests(STAT, "GET", "2xx", 5),
            requests(STAT, "GET", "4xx", 3),
            requests(SESSIONS, "POST", "2xx", 2),
            requests(SESSIONS, "POST", "4xx", 3),
            requests(SESSIONS, "DELETE", "4xx", 1),
            requests(HANDLES, "POST", "2xx", 3),
            requests(HANDLES, "POST", "4xx", 3),
            series(
                &format!("mount_rs_http_request_duration_seconds_count{{route=\"{HANDLES}\"}}"),
                Some(6),
            ),
            series("mount_rs_http_forwarded_requests_total", Some(0)),
            series("mount_rs_http_requests_in_flight", Some(0)),
            series("mount_rs_http_drive_ready{drive=\"main\"}", Some(1)),
        ])
        .await;
    assert!(!text.contains("route=\"unmatched\""), "{text}");
    for private in ["rotation-", "too-short", &of_old, &of_new] {
        assert!(!text.contains(private), "{private} in {text}");
    }

    // The dropped token's session still holds its place under the limit
    // until it has been idle for `idle_timeout_seconds`; then the server
    // closes it and the place is free. The live session is kept in use
    // meanwhile and outlives it.
    let deadline = std::time::Instant::now() + LIMIT;
    let freed = loop {
        assert_eq!(node.operate("main", NEW, &of_new).await.status, 200);
        match node.open_session("main", NEW).await {
            Ok(session) => break session,
            Err(reply) => assert_eq!(reply.status, 429, "{reply:?}"),
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the session of the dropped token was never closed"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    assert_eq!(node.close_session("main", NEW, &freed).await, 204);
    assert_eq!(node.close_session("main", NEW, &of_new).await, 204);
    node.stop();
}

/// In a forwarding cluster the owner of a drive checks its token, so a
/// rotation of the owner's token file reaches requests sent to the owner
/// and requests an ingress forwards alike. Each server counts what it did:
/// the ingress every request once, the owner every request once and the
/// forwarded ones in `mount_rs_http_forwarded_requests_total` as well.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn a_token_file_is_rotated_on_an_owner_and_through_an_ingress_and_both_count_once() {
    let files = Files::new(&["a", "b"]);
    files.write_tokens("a", &[OLD]);
    let owner = files.start(
        "a",
        json!({"remote_sessions": true, "drives": [drive("a", "main", memory())],
            "forwarding": files.forwarding("a", true)}),
    );
    let ingress = files.start(
        "b",
        json!({"drives": [], "forwarding": files.forwarding("b", false)}),
    );
    let mut seen = 0;
    let file = "/v1/drives/main/fs/a-private-name.bin";
    let content = vec![7_u8; 1000];

    let put = ingress
        .send("PUT", file, OLD, None, Some(content.clone()))
        .await;
    assert!((200..300).contains(&put.status), "{put:?}");
    assert_eq!(ingress.stat("main", NEW).await, 401);
    let session = ingress.open_session("main", OLD).await.expect("a session");
    assert_eq!(owner.stat("main", OLD).await, 200);

    files.write_tokens("a", &[NEW, OLD]);
    owner.reloaded(&mut seen, 2);
    assert_eq!(ingress.stat("main", NEW).await, 200);
    assert_eq!(ingress.stat("main", OLD).await, 200);
    // The session belongs to the token that made it.
    assert_eq!(ingress.operate("main", NEW, &session).await.status, 403);
    assert_eq!(ingress.operate("main", OLD, &session).await.status, 200);

    files.write_tokens("a", &[NEW]);
    owner.reloaded(&mut seen, 1);
    assert_eq!(ingress.stat("main", OLD).await, 401);
    assert_eq!(ingress.operate("main", OLD, &session).await.status, 401);
    let session = ingress.open_session("main", NEW).await.expect("a session");
    assert_eq!(ingress.operate("main", NEW, &session).await.status, 200);
    assert_eq!(owner.stat("main", OLD).await, 401);
    // The session made through the ingress is the same one on the owner.
    assert_eq!(owner.operate("main", NEW, &session).await.status, 200);
    let got = ingress.send("GET", file, NEW, None, None).await;
    assert_eq!((got.status, &got.body), (200, &content));
    assert_eq!(ingress.close_session("main", NEW, &session).await, 204);

    // The ingress: the thirteen requests it was sent, each once, none of
    // them received from another server, and no drive of its own.
    let text = ingress
        .expect_metrics(&[
            requests(FS, "PUT", "2xx", 1),
            requests(FS, "GET", "2xx", 1),
            requests(STAT, "GET", "2xx", 2),
            requests(STAT, "GET", "4xx", 2),
            requests(SESSIONS, "POST", "2xx", 2),
            requests(SESSIONS, "DELETE", "2xx", 1),
            requests(HANDLES, "POST", "2xx", 2),
            requests(HANDLES, "POST", "4xx", 2),
            series("mount_rs_http_forwarded_requests_total", Some(0)),
            series("mount_rs_http_requests_in_flight", Some(0)),
            series("mount_rs_http_drive_ready{drive=\"main\"}", None),
        ])
        .await;
    assert!(!text.contains("mount_rs_http_drive_ready{"), "{text}");
    assert!(!text.contains("route=\"unmatched\""), "{text}");
    assert!(sample(&text, "mount_rs_http_request_body_bytes_total").unwrap() >= 1000.0);
    assert!(sample(&text, "mount_rs_http_response_body_bytes_total").unwrap() >= 1000.0);

    // The owner: those thirteen, forwarded to it, and the three it was
    // sent itself, each once.
    let text = owner
        .expect_metrics(&[
            requests(FS, "PUT", "2xx", 1),
            requests(FS, "GET", "2xx", 1),
            requests(STAT, "GET", "2xx", 3),
            requests(STAT, "GET", "4xx", 3),
            requests(SESSIONS, "POST", "2xx", 2),
            requests(SESSIONS, "DELETE", "2xx", 1),
            requests(HANDLES, "POST", "2xx", 3),
            requests(HANDLES, "POST", "4xx", 2),
            series("mount_rs_http_forwarded_requests_total", Some(13)),
            series("mount_rs_http_requests_in_flight", Some(0)),
            series("mount_rs_http_drive_ready{drive=\"main\"}", Some(1)),
        ])
        .await;
    assert!(!text.contains("route=\"unmatched\""), "{text}");
    assert!(sample(&text, "mount_rs_http_request_body_bytes_total").unwrap() >= 1000.0);
    for private in ["rotation-", "a-private-name", &session] {
        assert!(!text.contains(private), "{private} in {text}");
    }
    ingress.stop();
    owner.stop();
}

/// Failover candidates each have their own token file. A candidate reloads
/// its file whether or not it owns the drive, so a standby that takes the
/// drive over serves it with the tokens its file lists then, and reports
/// the drive's readiness only while it owns it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn a_standby_serves_a_drive_it_takes_over_with_its_rotated_tokens() {
    let files = Files::new(&["a", "b"]);
    let candidate = |node: &str| {
        let mut forwarding = files.forwarding(node, true);
        forwarding["failover"] = json!({});
        json!({"remote_sessions": true, "drives": [drive(node, "volume", shared(node))],
            "forwarding": forwarding})
    };
    files.write_tokens("a", &[OLD]);
    files.write_tokens("b", &[OLD]);
    let first = files.start("a", candidate("a"));
    wait_until("the first candidate to publish", LIMIT, || {
        !first.server.states("published").is_empty()
    });
    let standby = files.start("b", candidate("b"));
    let (mut seen_first, mut seen_standby) = (0, 0);
    assert_eq!(standby.stat("volume", OLD).await, 200);

    // The rotation, on every candidate: the new token beside the old one,
    // then the old one dropped.
    for (tokens, count) in [(vec![NEW, OLD], 2), (vec![NEW], 1)] {
        files.write_tokens("a", &tokens);
        files.write_tokens("b", &tokens);
        first.reloaded(&mut seen_first, count);
        standby.reloaded(&mut seen_standby, count);
        assert_eq!(standby.stat("volume", NEW).await, 200);
        assert_eq!(first.stat("volume", NEW).await, 200);
    }
    assert_eq!(standby.stat("volume", OLD).await, 401);
    assert_eq!(first.stat("volume", OLD).await, 401);

    // The owner counted what the standby forwarded to it; the standby owns
    // nothing, so it has no drive to report.
    first
        .expect_metrics(&[
            requests(STAT, "GET", "2xx", 5),
            requests(STAT, "GET", "4xx", 2),
            series("mount_rs_http_forwarded_requests_total", Some(4)),
            series("mount_rs_http_drive_ready{drive=\"volume\"}", Some(1)),
        ])
        .await;
    standby
        .expect_metrics(&[
            requests(STAT, "GET", "2xx", 3),
            requests(STAT, "GET", "4xx", 1),
            series("mount_rs_http_forwarded_requests_total", Some(0)),
            series("mount_rs_http_drive_ready{drive=\"volume\"}", None),
        ])
        .await;

    first.stop();
    wait_until("the standby to take the drive over", LIMIT, || {
        !standby.server.states("published").is_empty()
    });
    // It serves the drive with the tokens rotated while it was a standby,
    // on the file routes and the session routes.
    assert_eq!(standby.stat("volume", NEW).await, 200);
    assert_eq!(standby.stat("volume", OLD).await, 401);
    let session = standby
        .open_session("volume", NEW)
        .await
        .expect("a session");
    assert_eq!(
        standby
            .open_session("volume", OLD)
            .await
            .expect_err("no session")
            .status,
        401
    );
    // And a rotation while it owns the drive reaches it as well.
    files.write_tokens("b", &[NEWER, NEW]);
    standby.reloaded(&mut seen_standby, 2);
    assert_eq!(standby.stat("volume", NEWER).await, 200);
    assert_eq!(standby.operate("volume", NEW, &session).await.status, 200);
    assert_eq!(standby.operate("volume", NEWER, &session).await.status, 403);
    standby
        .expect_metrics(&[
            requests(SESSIONS, "POST", "2xx", 1),
            requests(SESSIONS, "POST", "4xx", 1),
            requests(HANDLES, "POST", "2xx", 1),
            requests(HANDLES, "POST", "4xx", 1),
            series("mount_rs_http_forwarded_requests_total", Some(0)),
            series("mount_rs_http_drive_ready{drive=\"volume\"}", Some(1)),
        ])
        .await;
    standby.stop();
}
