//! Shared by the tests of a forwarding `serve-http` cluster on a Redis
//! Sentinel deployment: the deployment itself, started by
//! `scripts/redis-sentinel-fixture.sh` in Docker and removed on drop, and the
//! configuration and processes of the cluster that uses it.
#![allow(dead_code)]

use crate::common::*;
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose};
use std::{
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const TOKEN_ENV: &str = "SENTINEL_TEST_TOKEN";
pub const PASSWORD_ENV: &str = "SENTINEL_TEST_REDIS_PASSWORD";
pub const SENTINEL_PASSWORD_ENV: &str = "SENTINEL_TEST_SENTINEL_PASSWORD";
pub const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/redis-sentinel-fixture.sh"
);
/// The longest a registry may take to be usable again after its primary is
/// killed: the sentinels' 2 s detection and their election, the servers'
/// lookups, and a loaded runner.
pub const REDIS_FAILOVER_LIMIT: Duration = Duration::from_secs(60);
/// The writer lease of the shared drive of the candidates.
pub const LEASE: Duration = Duration::from_secs(12);

/// How the deployment is secured, and so how the servers must connect.
#[derive(Clone, Default)]
pub struct Security {
    /// The Redis servers' password, of `username` if set, else of the
    /// default user.
    pub password: Option<&'static str>,
    pub username: Option<&'static str>,
    /// The sentinels' password, of `sentinel_username` if set.
    pub sentinel_password: Option<&'static str>,
    pub sentinel_username: Option<&'static str>,
    pub tls: bool,
}

/// A running Sentinel deployment, removed on drop.
pub struct Deployment {
    state: tempfile::TempDir,
    pub sentinels: Vec<String>,
    /// The sentinels' ports, in the order of `sentinels`.
    pub sentinel_ports: Vec<String>,
    /// The Redis servers' ports, the first primary first.
    pub data_ports: Vec<String>,
    pub service: String,
    security: Security,
    /// How the servers log in: `security`, unless the test made ACL users.
    client: Security,
    /// With TLS: the CA that signed the servers' certificate.
    pub ca_pem: Option<String>,
}

impl Deployment {
    pub fn script(&self, arguments: &[&str]) -> std::process::Output {
        Command::new(FIXTURE)
            .arg(arguments[0])
            .arg(self.state.path())
            .args(&arguments[1..])
            .output()
            .unwrap()
    }

    pub fn run(&self, arguments: &[&str]) -> String {
        let output = self.script(arguments);
        assert!(
            output.status.success(),
            "fixture {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Start a deployment. Docker being unavailable is a failure.
    pub fn start(security: Security) -> Self {
        Self::start_with(security, false)
    }

    /// Start a deployment whose links can be cut (`partition`, `cut_host`).
    pub fn start_partitionable(security: Security) -> Self {
        Self::start_with(security, true)
    }

    fn start_with(security: Security, partitionable: bool) -> Self {
        let state = tempfile::tempdir().unwrap();
        let mut command = Command::new(FIXTURE);
        command.arg("start").arg(state.path());
        if let Ok(image) = std::env::var("MOUNT_RS_REDIS_SENTINEL_TEST_IMAGE") {
            command.env("MOUNT_RS_REDIS_SENTINEL_IMAGE", image);
        }
        if let Some(password) = security.password {
            command.env("MOUNT_RS_SENTINEL_FIXTURE_PASSWORD", password);
        }
        if let Some(password) = security.sentinel_password {
            command.env("MOUNT_RS_SENTINEL_FIXTURE_SENTINEL_PASSWORD", password);
        }
        if partitionable {
            command.env("MOUNT_RS_SENTINEL_FIXTURE_PARTITION", "1");
            if let Ok(image) = std::env::var("MOUNT_RS_SENTINEL_TEST_NET_IMAGE") {
                command.env("MOUNT_RS_SENTINEL_NET_IMAGE", image);
            }
        }
        let mut ca_pem = None;
        let tls = tempfile::tempdir().unwrap();
        if security.tls {
            // A throwaway CA and a certificate for the loopback address,
            // which is what the sentinels report for every server.
            let ca = authority("mount-rs Sentinel test CA");
            let key = KeyPair::generate().unwrap();
            let mut params = CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
            params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
            params.extended_key_usages = vec![
                ExtendedKeyUsagePurpose::ServerAuth,
                // Replicas and sentinels are clients of the other servers.
                ExtendedKeyUsagePurpose::ClientAuth,
            ];
            let server = params.signed_by(&key, &ca.cert, &ca.key).unwrap();
            for (name, content) in [
                ("ca.pem", ca.cert.pem()),
                ("server.pem", server.pem()),
                ("server.key", key.serialize_pem()),
            ] {
                std::fs::write(tls.path().join(name), content).unwrap();
            }
            command.env("MOUNT_RS_SENTINEL_FIXTURE_TLS_DIR", tls.path());
            ca_pem = Some(ca.cert.pem());
        }
        let output = command.output().expect("run the Sentinel fixture script");
        assert!(
            output.status.success(),
            "the Sentinel fixture did not start: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let env = std::fs::read_to_string(state.path().join("env")).unwrap();
        let value = |name: &str| {
            env.lines()
                .find_map(|line| line.strip_prefix(&format!("{name}=")))
                .unwrap_or_else(|| panic!("{name} in {env}"))
                .trim_matches('"')
                .to_owned()
        };
        let sentinels: Vec<String> = value("MOUNT_RS_SENTINEL_ADDRESSES")
            .split(',')
            .map(str::to_owned)
            .collect();
        Self {
            sentinel_ports: sentinels
                .iter()
                .map(|address| address.rsplit(':').next().unwrap().to_owned())
                .collect(),
            data_ports: value("MOUNT_RS_SENTINEL_DATA_PORTS")
                .split_whitespace()
                .map(str::to_owned)
                .collect(),
            sentinels,
            service: value("MOUNT_RS_SENTINEL_SERVICE"),
            state,
            client: security.clone(),
            security,
            ca_pem,
        }
    }

    /// `http.forwarding.redis_sentinel` for this deployment.
    pub fn config(&self) -> serde_json::Value {
        let client = &self.client;
        let mut config = serde_json::json!({
            "sentinels": self.sentinels, "service": self.service, "tls": client.tls,
        });
        if client.password.is_some() {
            config["password"] = serde_json::json!({"env": PASSWORD_ENV});
        }
        if let Some(username) = client.username {
            config["username"] = username.into();
        }
        if client.sentinel_password.is_some() {
            config["sentinel_password"] = serde_json::json!({"env": SENTINEL_PASSWORD_ENV});
        }
        if let Some(username) = client.sentinel_username {
            config["sentinel_username"] = username.into();
        }
        config
    }

    /// Have the servers of clusters made from now on log in as `client`.
    pub fn set_client(&mut self, client: Security) {
        self.client = client;
    }

    /// How the servers log in.
    pub fn client(&self) -> Security {
        self.client.clone()
    }

    /// Cut every link between a process of `one` and a process of `other`
    /// (ports of this deployment). Packets are dropped, not refused.
    pub fn partition(&self, one: &[&str], other: &[&str]) {
        self.run(&["partition", &one.join(","), &other.join(",")]);
    }

    /// Drop what the host, and so every mount-rs server, sends to `ports`.
    pub fn cut_host(&self, ports: &[&str]) {
        let mut arguments = vec!["cut-host"];
        arguments.extend_from_slice(ports);
        self.run(&arguments);
    }

    /// Restore every link `partition` and `cut_host` cut.
    pub fn heal(&self) {
        self.run(&["heal"]);
    }

    /// Restore only the links between processes that `partition` cut.
    pub fn heal_links(&self) {
        self.run(&["heal", "links"]);
    }

    /// Restore only what `cut_host` cut.
    pub fn heal_host(&self) {
        self.run(&["heal", "host"]);
    }

    /// The remaining lifetime of `key` on `port`, in milliseconds: -2 for a
    /// missing key.
    pub fn pttl(&self, port: &str, key: &str) -> i64 {
        self.cli(port, &["PTTL", key]).parse().unwrap_or(-2)
    }

    /// The port of the primary the sentinel on `port` names, if it names one.
    pub fn named_by(&self, port: &str) -> Option<String> {
        let reply = self.cli(port, &["SENTINEL", "GET-MASTER-ADDR-BY-NAME", &self.service]);
        reply.lines().nth(1).map(|line| line.trim().to_owned())
    }

    /// The first word of `ROLE` on the server on `port`: `master` or `slave`.
    pub fn role(&self, port: &str) -> String {
        self.cli(port, &["ROLE"])
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned()
    }

    /// The connections the server on `port` has from outside its container,
    /// that is from mount-rs servers: everything inside connects from a
    /// loopback address.
    pub fn host_clients(&self, port: &str) -> usize {
        self.cli(port, &["CLIENT", "LIST", "TYPE", "normal"])
            .lines()
            .filter_map(|line| {
                line.split_whitespace()
                    .find_map(|field| field.strip_prefix("addr="))
            })
            .filter(|address| !address.starts_with("127."))
            .count()
    }

    /// `host:port` of the sentinels on `ports`, for a server's configuration.
    pub fn sentinel_addresses(&self, ports: &[&str]) -> Vec<String> {
        ports.iter().map(|port| format!("127.0.0.1:{port}")).collect()
    }

    pub fn primary(&self) -> String {
        self.run(&["primary"])
    }

    /// SIGKILL the primary. Returns its port.
    pub fn kill_primary(&self) -> String {
        self.run(&["kill-primary"])
    }

    /// Wait for the sentinels to name another primary. Returns its port.
    pub fn wait_failover(&self, old: &str) -> String {
        self.run(&["wait-failover", old])
    }

    /// `redis-cli` against the server on `port`.
    pub fn cli(&self, port: &str, arguments: &[&str]) -> String {
        let mut all = vec!["cli", port];
        all.extend_from_slice(arguments);
        let output = self.script(&all);
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// The stored route record of drive `volume` on `port`, as written.
    pub fn record(&self, port: &str, namespace: &str) -> String {
        self.cli(port, &["GET", &route_key(namespace)])
    }

    /// `(node, fence)` of the route of drive `volume` stored on `port`.
    pub fn route(&self, port: &str, namespace: &str) -> Option<(String, u64)> {
        let record = self.record(port, namespace);
        let route: serde_json::Value = serde_json::from_str(record.get(21..)?).ok()?;
        Some((route["node"].as_str()?.to_owned(), route["fence"].as_u64()?))
    }
}

pub fn route_key(namespace: &str) -> String {
    format!("mount-rs:{namespace}:route:volume")
}

impl Drop for Deployment {
    fn drop(&mut self) {
        let _ = self.script(&["stop"]);
    }
}

/// The files and configs of a cluster on `deployment`. `candidates` share
/// drive `volume` on SQLite and fail over; without them `a` alone owns it
/// in memory. The last node is always an ingress.
pub struct Cluster {
    dir: tempfile::TempDir,
    pub namespace: String,
}

impl Cluster {
    pub fn new(deployment: &Deployment, nodes: &[&str], candidates: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, content: String| {
            std::fs::write(dir.path().join(name), content).unwrap();
        };
        std::fs::create_dir(dir.path().join("shared")).unwrap();
        let ca = authority("mount-rs Sentinel peer CA");
        write("ca.pem", ca.cert.pem());
        if let Some(pem) = &deployment.ca_pem {
            write("redis-ca.pem", pem.clone());
        }
        let mut peers = Vec::new();
        for node in nodes {
            let identity = peer(&ca, node);
            write(&format!("{node}.pem"), identity.cert.pem());
            write(&format!("{node}.key"), identity.key.serialize_pem());
            let pin = mount_rs_cluster::transport::spki_sha256_from_der(identity.cert.der())
                .unwrap()
                .to_hex();
            peers.push(serde_json::json!({
                "node": node, "boot": format!("{node}-boot"), "address": free_udp(),
                "tls_server_name": "localhost", "spki_sha256": pin,
            }));
        }
        write("peers.json", serde_json::Value::Array(peers).to_string());
        let namespace = unique("sentinel");
        for node in nodes {
            let candidate = candidates.contains(node);
            let owner = candidate || (candidates.is_empty() && *node == "a");
            let mut config = serde_json::json!({"version":1,"http":{
                "host":"127.0.0.1","port":0,"drives":[],"forwarding":{
                    "peers":"peers.json","certificate":format!("{node}.pem"),
                    "private_key":format!("{node}.key"),"ca":"ca.pem",
                    "redis_sentinel":deployment.config(),"namespace":namespace,
                    "bind":"127.0.0.1:0","owner":owner
                }
            }});
            if deployment.ca_pem.is_some() {
                config["http"]["forwarding"]["redis_ca"] = "redis-ca.pem".into();
            }
            let storage = if candidate {
                config["http"]["forwarding"]["failover"] = serde_json::json!({});
                serde_json::json!({
                    "metadata":{"kind":"sqlite","path":"./shared/meta.db"},
                    "blocks":{"kind":"sqlite","path":"./shared/blocks.db"},
                    "chunk_size_bytes":65536,"owner":node,
                    "lease_ttl_ms":LEASE.as_millis() as u64,
                })
            } else {
                serde_json::json!({"metadata":{"kind":"memory"},"blocks":{"kind":"memory"}})
            };
            if owner {
                config["http"]["drives"] = serde_json::json!([{
                    "id":"volume","token":{"env":TOKEN_ENV},
                    "driver":{"kind":"splitstore","storage":storage}
                }]);
            }
            write(&format!("{node}.json"), config.to_string());
        }
        Self { dir, namespace }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// Change the configuration of `node` before it is started.
    pub fn edit(&self, node: &str, change: impl FnOnce(&mut serde_json::Value)) {
        let path = self.path(&format!("{node}.json"));
        let mut config: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        change(&mut config);
        std::fs::write(path, config.to_string()).unwrap();
    }

    /// Give `node` only these sentinels: it can reach no others.
    pub fn only_sentinels(&self, node: &str, addresses: Vec<String>) {
        self.edit(node, |config| {
            config["http"]["forwarding"]["redis_sentinel"]["sentinels"] = addresses.into();
        });
    }

    /// `(owner, fence)` of the writer lease of the candidates' shared drive.
    pub fn lease(&self) -> (Option<String>, u64) {
        let db = rusqlite::Connection::open_with_flags(
            self.path("shared/meta.db"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        db.busy_timeout(Duration::from_secs(30)).unwrap();
        db.query_row(
            "SELECT owner, fence FROM mount_rs_metadata WHERE id=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    }

    pub fn command(&self, config: &Path, security: &Security) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mount-rs"));
        command
            .current_dir(self.dir.path())
            .args(["serve-http", "--config"])
            .arg(config)
            .env(TOKEN_ENV, TOKEN)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(password) = security.password {
            command.env(PASSWORD_ENV, password);
        }
        if let Some(password) = security.sentinel_password {
            command.env(SENTINEL_PASSWORD_ENV, password);
        }
        command
    }

    pub fn start(&self, node: &str, security: &Security) -> Server {
        Server::spawn(
            &mut self.command(&self.path(&format!("{node}.json")), security),
            node,
        )
    }

    /// Run `node` with `change` applied to its `redis_sentinel`, expecting
    /// it to refuse to start. Returns its exit code and what it printed.
    pub fn fail_to_start(
        &self,
        node: &str,
        security: &Security,
        change: impl FnOnce(&mut serde_json::Value),
    ) -> (Option<i32>, String) {
        let source = std::fs::read_to_string(self.path(&format!("{node}.json"))).unwrap();
        let mut config: serde_json::Value = serde_json::from_str(&source).unwrap();
        change(&mut config["http"]["forwarding"]["redis_sentinel"]);
        let changed = self.path(&format!("{node}-changed.json"));
        std::fs::write(&changed, config.to_string()).unwrap();
        let mut child = self.command(&changed, security).spawn().unwrap();
        let stderr = collect(child.stderr.take().unwrap());
        let stdout = collect(child.stdout.take().unwrap());
        let deadline = Instant::now() + Duration::from_secs(60);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() {
                break status;
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("server {node} kept running: {:?}", stderr.lock().unwrap());
            }
            thread::sleep(Duration::from_millis(50));
        };
        thread::sleep(Duration::from_millis(200));
        let output = format!(
            "{}\n{}",
            stdout.lock().unwrap().join("\n"),
            stderr.lock().unwrap().join("\n")
        );
        (status.code(), output)
    }
}

/// What a client saw while it repeated one GET.
#[derive(Default)]
pub struct Observed {
    pub ok: usize,
    pub unavailable: usize,
    /// Anything that is not the expected body or a 503.
    pub wrong: Vec<String>,
    /// The longest time from one served request to the next.
    pub longest_gap: Duration,
}

/// GET `name` from `address` every 100 ms until stopped, checking every
/// answer against `expected`.
pub fn observe(
    address: std::net::SocketAddr,
    name: &'static str,
    expected: Vec<u8>,
) -> (Arc<AtomicBool>, thread::JoinHandle<Observed>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let handle = thread::spawn(move || {
        let mut observed = Observed::default();
        let mut served = Instant::now();
        while !stopped.load(Ordering::Relaxed) {
            match http_within(address, "GET", name, b"", Duration::from_secs(10)) {
                Ok((200, body)) if body == expected => {
                    observed.ok += 1;
                    observed.longest_gap = observed.longest_gap.max(served.elapsed());
                    served = Instant::now();
                }
                // Not dispatched: never another node's answer.
                Ok((503, _)) => observed.unavailable += 1,
                other => observed.wrong.push(format!("{other:?}")),
            }
            thread::sleep(Duration::from_millis(100));
        }
        observed
    });
    (stop, handle)
}

pub fn finish(observer: (Arc<AtomicBool>, thread::JoinHandle<Observed>)) -> Observed {
    observer.0.store(true, Ordering::Relaxed);
    observer.1.join().unwrap()
}

/// PUT through `server`, repeated while it is refused or unanswered.
pub fn write_settled(server: &Server, name: &str, body: &[u8]) -> Duration {
    wait_until(
        &format!("PUT {name} through {}", server.name),
        Duration::from_secs(90),
        || {
            matches!(
                http_within(server.address, "PUT", name, body, Duration::from_secs(5)),
                Ok((200, _))
            )
        },
    )
}

