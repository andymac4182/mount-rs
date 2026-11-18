//! A forwarding `serve-http` cluster whose route registry is a Redis Sentinel
//! deployment, with the real binary and real Redis processes: one primary,
//! two replicas and three sentinels, started by
//! `scripts/redis-sentinel-fixture.sh` in one Docker container per test.
//!
//! Each test kills the primary with SIGKILL and lets the sentinels promote a
//! replica. See "Redis Sentinel" in `docs/serve-http-failover.md` for the
//! rules these tests hold the servers to. Every wait polls for an outcome
//! with a generous limit; what must not happen is checked over the whole
//! failover and again at the end.
#![cfg(unix)]

mod common;

use common::*;
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

const TOKEN_ENV: &str = "SENTINEL_TEST_TOKEN";
const PASSWORD_ENV: &str = "SENTINEL_TEST_REDIS_PASSWORD";
const SENTINEL_PASSWORD_ENV: &str = "SENTINEL_TEST_SENTINEL_PASSWORD";
const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/redis-sentinel-fixture.sh"
);
/// The longest a registry may take to be usable again after its primary is
/// killed: the sentinels' 2 s detection and their election, the servers'
/// lookups, and a loaded runner.
const REDIS_FAILOVER_LIMIT: Duration = Duration::from_secs(60);
/// The writer lease of the shared drive of the candidates.
const LEASE: Duration = Duration::from_secs(12);

/// How the deployment is secured, and so how the servers must connect.
#[derive(Clone, Default)]
struct Security {
    password: Option<&'static str>,
    sentinel_password: Option<&'static str>,
    tls: bool,
}

/// A running Sentinel deployment, removed on drop.
struct Deployment {
    state: tempfile::TempDir,
    sentinels: Vec<String>,
    service: String,
    security: Security,
    /// With TLS: the CA that signed the servers' certificate.
    ca_pem: Option<String>,
}

impl Deployment {
    fn script(&self, arguments: &[&str]) -> std::process::Output {
        Command::new(FIXTURE)
            .arg(arguments[0])
            .arg(self.state.path())
            .args(&arguments[1..])
            .output()
            .unwrap()
    }

    fn run(&self, arguments: &[&str]) -> String {
        let output = self.script(arguments);
        assert!(
            output.status.success(),
            "fixture {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Start a deployment. Docker being unavailable is a failure.
    fn start(security: Security) -> Self {
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
        Self {
            sentinels: value("MOUNT_RS_SENTINEL_ADDRESSES")
                .split(',')
                .map(str::to_owned)
                .collect(),
            service: value("MOUNT_RS_SENTINEL_SERVICE"),
            state,
            security,
            ca_pem,
        }
    }

    /// `http.forwarding.redis_sentinel` for this deployment.
    fn config(&self) -> serde_json::Value {
        let mut config = serde_json::json!({
            "sentinels": self.sentinels, "service": self.service, "tls": self.security.tls,
        });
        if self.security.password.is_some() {
            config["password"] = serde_json::json!({"env": PASSWORD_ENV});
        }
        if self.security.sentinel_password.is_some() {
            config["sentinel_password"] = serde_json::json!({"env": SENTINEL_PASSWORD_ENV});
        }
        config
    }

    fn primary(&self) -> String {
        self.run(&["primary"])
    }

    /// SIGKILL the primary. Returns its port.
    fn kill_primary(&self) -> String {
        self.run(&["kill-primary"])
    }

    /// Wait for the sentinels to name another primary. Returns its port.
    fn wait_failover(&self, old: &str) -> String {
        self.run(&["wait-failover", old])
    }

    /// `redis-cli` against the server on `port`.
    fn cli(&self, port: &str, arguments: &[&str]) -> String {
        let mut all = vec!["cli", port];
        all.extend_from_slice(arguments);
        let output = self.script(&all);
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// The stored route record of drive `volume` on `port`, as written.
    fn record(&self, port: &str, namespace: &str) -> String {
        self.cli(port, &["GET", &route_key(namespace)])
    }

    /// `(node, fence)` of the route of drive `volume` stored on `port`.
    fn route(&self, port: &str, namespace: &str) -> Option<(String, u64)> {
        let record = self.record(port, namespace);
        let route: serde_json::Value = serde_json::from_str(record.get(21..)?).ok()?;
        Some((route["node"].as_str()?.to_owned(), route["fence"].as_u64()?))
    }
}

fn route_key(namespace: &str) -> String {
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
struct Cluster {
    dir: tempfile::TempDir,
    namespace: String,
}

impl Cluster {
    fn new(deployment: &Deployment, nodes: &[&str], candidates: &[&str]) -> Self {
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

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn command(&self, config: &Path, security: &Security) -> Command {
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

    fn start(&self, node: &str, security: &Security) -> Server {
        Server::spawn(
            &mut self.command(&self.path(&format!("{node}.json")), security),
            node,
        )
    }

    /// Run `node` with `change` applied to its `redis_sentinel`, expecting
    /// it to refuse to start. Returns its exit code and what it printed.
    fn fail_to_start(
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
struct Observed {
    ok: usize,
    unavailable: usize,
    /// Anything that is not the expected body or a 503.
    wrong: Vec<String>,
    /// The longest time from one served request to the next.
    longest_gap: Duration,
}

/// GET `name` from `address` every 100 ms until stopped, checking every
/// answer against `expected`.
fn observe(
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

fn finish(observer: (Arc<AtomicBool>, thread::JoinHandle<Observed>)) -> Observed {
    observer.0.store(true, Ordering::Relaxed);
    observer.1.join().unwrap()
}

/// PUT through `server`, repeated while it is refused or unanswered.
fn write_settled(server: &Server, name: &str, body: &[u8]) -> Duration {
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

/// One owner and one ingress: publish, forward, lose the Redis primary,
/// carry on, and take the old primary back as a replica.
fn a_static_owner_rides_out_a_sentinel_failover(security: Security) {
    let deployment = Deployment::start(security.clone());
    let cluster = Cluster::new(&deployment, &["a", "b"], &[]);
    let mut owner = cluster.start("a", &security);
    let mut ingress = cluster.start("b", &security);

    // The route is published on the primary and requests are forwarded.
    let first = deployment.primary();
    let published = deployment
        .route(&first, &cluster.namespace)
        .expect("the owner's route on the primary");
    assert_eq!(published.0, "a");
    assert_eq!(ingress.put("before", b"written before").unwrap().0, 200);
    assert_eq!(
        ingress.get("before").unwrap(),
        (200, b"written before".to_vec())
    );

    let direct = observe(owner.address, "before", b"written before".to_vec());
    let forwarded = observe(ingress.address, "before", b"written before".to_vec());
    let killed = Instant::now();
    assert_eq!(deployment.kill_primary(), first);
    let second = deployment.wait_failover(&first);
    let promoted = killed.elapsed();
    assert_ne!(second, first);

    // The owner publishes the same route, under the same fence, on the new
    // primary, and a new file can be written through the ingress.
    wait_until("the route on the new primary", REDIS_FAILOVER_LIMIT, || {
        deployment.route(&second, &cluster.namespace) == Some(published.clone())
    });
    let republished = killed.elapsed();
    write_settled(&ingress, "after", b"written after");
    let forwarding = killed.elapsed();
    assert_eq!(
        ingress.get("after").unwrap(),
        (200, b"written after".to_vec())
    );

    // A promoted replica may never have received the route. Take it away,
    // as if this one had not: the owner publishes it again.
    assert_eq!(
        deployment.cli(&second, &["DEL", &route_key(&cluster.namespace)]),
        "1"
    );
    let lost = Instant::now();
    wait_until(
        "the lost route to be published again",
        REDIS_FAILOVER_LIMIT,
        || deployment.route(&second, &cluster.namespace) == Some(published.clone()),
    );
    let restored = lost.elapsed();

    // The old primary comes back empty and believing it is a primary until
    // the sentinels reconfigure it. Nobody goes back to it.
    deployment.run(&["start-node", &first]);
    wait_until(
        "the old primary to become a replica",
        REDIS_FAILOVER_LIMIT,
        || deployment.cli(&first, &["ROLE"]).starts_with("slave"),
    );
    assert_eq!(deployment.primary(), second);
    // Two refresh intervals with the old primary back, and longer after the
    // failover than a route lives without being refreshed.
    thread::sleep(Duration::from_secs(11));
    assert!(killed.elapsed() > ROUTE_TTL + Duration::from_secs(5));
    assert_eq!(
        deployment.route(&second, &cluster.namespace),
        Some(published.clone())
    );
    assert_eq!(
        ingress.get("after").unwrap(),
        (200, b"written after".to_vec())
    );

    let direct = finish(direct);
    let forwarded = finish(forwarded);
    // The owner served every direct request: it never depends on Redis.
    assert!(direct.wrong.is_empty(), "{:?}", direct.wrong);
    assert_eq!(direct.unavailable, 0);
    assert!(direct.ok > 0);
    // A forwarded request was answered by the owner or not served at all.
    assert!(forwarded.wrong.is_empty(), "{:?}", forwarded.wrong);
    assert!(forwarded.ok > 0);
    owner.assert_running("the owner");
    ingress.assert_running("the ingress");
    eprintln!(
        "sentinel failover (tls={}): promoted after {promoted:.1?}, route on the new primary \
         after {republished:.1?}, a forwarded write succeeded after {forwarding:.1?}; a route \
         deleted from the new primary was back after {restored:.1?}; forwarded reads: {} \
         served, {} refused, longest gap {:.1?}; direct reads: {} served, longest gap {:.1?}",
        security.tls,
        forwarded.ok,
        forwarded.unavailable,
        forwarded.longest_gap,
        direct.ok,
        direct.longest_gap
    );
}

#[test]
#[ignore = "requires Docker for scripts/redis-sentinel-fixture.sh"]
fn a_static_owner_and_an_ingress_ride_out_a_sentinel_failover() {
    a_static_owner_rides_out_a_sentinel_failover(Security::default());
}

#[test]
#[ignore = "requires Docker for scripts/redis-sentinel-fixture.sh"]
fn the_same_with_tls_a_private_ca_and_passwords() {
    a_static_owner_rides_out_a_sentinel_failover(Security {
        password: Some("data-password-41c7a9"),
        sentinel_password: Some("sentinel-password-9d02be"),
        tls: true,
    });
}

/// A server must not start on a deployment it cannot use, and must say why
/// without printing a password.
#[test]
#[ignore = "requires Docker for scripts/redis-sentinel-fixture.sh"]
fn a_wrong_service_password_or_ca_is_reported_at_startup() {
    let security = Security {
        password: Some("data-password-77e1c0"),
        sentinel_password: Some("sentinel-password-3b9f15"),
        tls: true,
    };
    let deployment = Deployment::start(security.clone());
    let cluster = Cluster::new(&deployment, &["a", "b"], &[]);
    let secrets = [
        security.password.unwrap(),
        security.sentinel_password.unwrap(),
        "wrong-password-6a20d4",
    ];
    let check = |what: &str, outcome: (Option<i32>, String), expected: &str| {
        let (code, output) = outcome;
        assert_eq!(code, Some(1), "{what}: {output}");
        assert!(
            output.contains("forwarding Redis Sentinel"),
            "{what}: {output}"
        );
        assert!(output.contains(expected), "{what}: {output}");
        for secret in secrets {
            assert!(!output.contains(secret), "{what}: {output}");
        }
    };
    check(
        "an unknown service",
        cluster.fail_to_start("b", &security, |sentinel| {
            sentinel["service"] = "not-monitored".into();
        }),
        "does not monitor a service named \"not-monitored\"",
    );
    let wrong = |data: bool| Security {
        password: Some(if data { secrets[2] } else { secrets[0] }),
        sentinel_password: Some(if data { secrets[1] } else { secrets[2] }),
        tls: true,
    };
    check(
        "a wrong Redis password",
        cluster.fail_to_start("b", &wrong(true), |_| {}),
        "named by a sentinel is unreachable",
    );
    check(
        "a wrong sentinel password",
        cluster.fail_to_start("b", &wrong(false), |_| {}),
        "no sentinel named a primary",
    );
    // A CA that did not sign the servers' certificate.
    std::fs::write(
        cluster.path("redis-ca.pem"),
        authority("mount-rs unrelated CA").cert.pem(),
    )
    .unwrap();
    check(
        "an unrelated CA",
        cluster.fail_to_start("b", &security, |_| {}),
        "no sentinel named a primary",
    );
    // Plaintext to a TLS deployment.
    std::fs::write(
        cluster.path("redis-ca.pem"),
        deployment.ca_pem.clone().unwrap(),
    )
    .unwrap();
    let (code, output) = cluster.fail_to_start("b", &security, |sentinel| {
        sentinel["tls"] = false.into();
    });
    assert_eq!(code, Some(2), "{output}");
    assert!(output.contains("redis_ca"), "{output}");
}

/// Two failover candidates and an ingress. The Redis primary is killed
/// while `a` owns the drive: `a` keeps it, `b` never opens it, and when `a`
/// is then killed too, `b` takes over through the promoted primary.
#[test]
#[ignore = "requires Docker for scripts/redis-sentinel-fixture.sh"]
fn drive_failover_keeps_one_owner_across_a_sentinel_failover() {
    let security = Security::default();
    let deployment = Deployment::start(security.clone());
    let cluster = Cluster::new(&deployment, &["a", "b", "c"], &["a", "b"]);
    let lease = || -> (Option<String>, u64) {
        let db = rusqlite::Connection::open_with_flags(
            cluster.path("shared/meta.db"),
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
    };
    let mut a = cluster.start("a", &security);
    wait_until("a to publish the drive", Duration::from_secs(30), || {
        !a.states("published").is_empty()
    });
    let mut b = cluster.start("b", &security);
    let mut c = cluster.start("c", &security);
    let first = deployment.primary();
    let route = deployment
        .route(&first, &cluster.namespace)
        .expect("the route on the primary");
    assert_eq!(route.0, "a");
    let fence = route.1;
    let record_of_a = deployment.record(&first, &cluster.namespace);
    assert_eq!(lease(), (Some("a".to_owned()), fence));
    assert_eq!(c.put("kept", b"owned by a").unwrap().0, 200);

    // Clients of the standby and of the ingress, across the failover.
    let through_b = observe(b.address, "kept", b"owned by a".to_vec());
    let through_c = observe(c.address, "kept", b"owned by a".to_vec());
    let killed = Instant::now();
    assert_eq!(deployment.kill_primary(), first);
    let second = deployment.wait_failover(&first);
    wait_until("a's route on the new primary", REDIS_FAILOVER_LIMIT, || {
        deployment.route(&second, &cluster.namespace) == Some(("a".to_owned(), fence))
    });
    let republished = killed.elapsed();
    write_settled(&c, "during", b"still owned by a");
    // Longer than a route lifetime on the new primary: time for a standby
    // that saw no route to have tried, and for anything it published to be
    // seen.
    thread::sleep(ROUTE_TTL + Duration::from_secs(2));
    let through_b = finish(through_b);
    let through_c = finish(through_c);
    assert!(through_b.wrong.is_empty(), "{:?}", through_b.wrong);
    assert!(through_c.wrong.is_empty(), "{:?}", through_c.wrong);
    assert!(through_b.ok > 0 && through_c.ok > 0);
    // One owner throughout: the lease never moved, `a` never let go and
    // `b` never opened the drive, so every answer above came from `a`.
    assert_eq!(lease(), (Some("a".to_owned()), fence));
    assert!(b.states("acquired").is_empty(), "{:?}", b.events());
    assert!(a.states("released").is_empty(), "{:?}", a.events());
    assert_eq!(
        deployment.route(&second, &cluster.namespace),
        Some(("a".to_owned(), fence))
    );
    a.assert_running("a");
    b.assert_running("b");
    c.assert_running("c");
    let held = b.states("lease_held").len();

    // The registry still does its job on the promoted primary: with `a`
    // gone, `b` reads that no route is published and takes the drive.
    let crashed = Instant::now();
    a.signal(libc::SIGKILL);
    wait_until(
        "b's route on the new primary",
        ROUTE_TTL + LEASE + Duration::from_secs(40),
        || {
            deployment
                .route(&second, &cluster.namespace)
                .is_some_and(|(node, next)| node == "b" && next > fence)
        },
    );
    let taken = crashed.elapsed();
    wait_until("the ingress to reach b", Duration::from_secs(60), || {
        matches!(
            http_within(c.address, "GET", "during", b"", Duration::from_secs(5)),
            Ok((200, body)) if body == b"still owned by a"
        )
    });
    assert_eq!(c.get("kept").unwrap(), (200, b"owned by a".to_vec()));
    assert_eq!(lease().0, Some("b".to_owned()));

    // A promoted replica can also still hold the record a later one
    // replaced. Put a's back, as if b's publication had been lost: b, which
    // holds the lease under a higher fence, publishes over it and keeps
    // the drive.
    assert_eq!(
        deployment.cli(
            &second,
            &[
                "SET",
                &route_key(&cluster.namespace),
                &record_of_a,
                "PX",
                "15000"
            ]
        ),
        "OK"
    );
    assert_eq!(
        deployment.route(&second, &cluster.namespace),
        Some(("a".to_owned(), fence))
    );
    let stale = Instant::now();
    wait_until("b to publish over a's old route", ROUTE_TTL, || {
        deployment
            .route(&second, &cluster.namespace)
            .is_some_and(|(node, _)| node == "b")
    });
    let replaced = stale.elapsed();
    assert!(b.states("released").is_empty(), "{:?}", b.events());
    assert_eq!(lease().0, Some("b".to_owned()));
    wait_until(
        "the ingress to reach b again",
        Duration::from_secs(60),
        || {
            matches!(
                http_within(c.address, "GET", "kept", b"", Duration::from_secs(5)),
                Ok((200, body)) if body == b"owned by a"
            )
        },
    );
    b.assert_running("b");
    eprintln!(
        "sentinel failover with candidates: a's route republished {republished:.1?} after the \
         primary was killed; reads through the standby: {} served, {} refused; through the \
         ingress: {} served, {} refused, longest gap {:.1?}; standby lease_held reports: \
         {held}; b took the drive {taken:.1?} after a was killed, and replaced a's old route \
         {replaced:.1?} after it reappeared",
        through_b.ok,
        through_b.unavailable,
        through_c.ok,
        through_c.unavailable,
        through_c.longest_gap.max(through_b.longest_gap)
    );
}
