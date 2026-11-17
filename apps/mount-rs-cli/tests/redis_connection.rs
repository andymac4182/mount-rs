//! The Redis connection of a forwarding `serve-http` cluster, exercised with
//! the real binary: the plaintext policy, TLS with a private CA and a
//! password, and a Redis outage longer than the route lifetime.
#![cfg(unix)]

mod common;

use common::*;
use rcgen::{CertificateParams, ExtendedKeyUsagePurpose, KeyPair, KeyUsagePurpose};
use std::{
    path::PathBuf,
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

const REDIS_ENV: &str = "REDIS_CONNECTION_TEST_URL";

/// The files and configs of one owner (`a`, which owns drive `volume`) and
/// one ingress-only server (`b`).
struct Fixture {
    dir: tempfile::TempDir,
    namespace: String,
}

impl Fixture {
    /// `forwarding` is merged into both servers' `http.forwarding`.
    fn new(forwarding: serde_json::Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, content: String| {
            std::fs::write(dir.path().join(name), content).unwrap();
        };
        let ca = authority("mount-rs peer test CA");
        write("ca.pem", ca.cert.pem());
        let mut peers = Vec::new();
        for node in ["a", "b"] {
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
        let namespace = unique("redis-connection");
        for node in ["a", "b"] {
            let mut config = serde_json::json!({"version":1,"http":{
                "host":"127.0.0.1","port":0,"drives":[],"forwarding":{
                    "peers":"peers.json","certificate":format!("{node}.pem"),
                    "private_key":format!("{node}.key"),"ca":"ca.pem",
                    "redis_url":{"env":REDIS_ENV},"namespace":namespace,
                    "bind":"127.0.0.1:0","owner":node == "a"
                }
            }});
            if node == "a" {
                config["http"]["drives"] = serde_json::json!([{
                    "id":"volume","token":{"env":"REDIS_CONNECTION_TEST_TOKEN"},
                    "driver":{"kind":"splitstore","storage":{
                        "metadata":{"kind":"memory"},"blocks":{"kind":"memory"}}}
                }]);
            }
            for (key, value) in forwarding.as_object().unwrap() {
                config["http"]["forwarding"][key] = value.clone();
            }
            write(&format!("{node}.json"), config.to_string());
        }
        Self { dir, namespace }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn route_key(&self) -> String {
        format!("mount-rs:{}:route:volume", self.namespace)
    }

    fn command(&self, node: &str, redis_url: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mount-rs"));
        command
            .args(["serve-http", "--config"])
            .arg(self.path(&format!("{node}.json")))
            .env(REDIS_ENV, redis_url)
            .env("REDIS_CONNECTION_TEST_TOKEN", TOKEN)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Start `node` and wait for its public listener.
    fn start(&self, node: &str, redis_url: &str) -> Server {
        Server::spawn(&mut self.command(node, redis_url), node)
    }

    /// Run `node` expecting it to refuse to start. Returns its exit status
    /// and everything it printed.
    fn fail_to_start(&self, node: &str, redis_url: &str) -> (ExitStatus, String) {
        let mut child = self.command(node, redis_url).spawn().unwrap();
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
                panic!(
                    "server {node} kept running with {redis_url}; stdout={:?} stderr={:?}",
                    stdout.lock().unwrap(),
                    stderr.lock().unwrap()
                );
            }
            thread::sleep(Duration::from_millis(50));
        };
        // The readers end when the pipes close, just after the exit.
        thread::sleep(Duration::from_millis(200));
        let output = format!(
            "{}\n{}",
            stdout.lock().unwrap().join("\n"),
            stderr.lock().unwrap().join("\n")
        );
        (status, output)
    }
}

#[test]
fn plaintext_redis_on_another_host_is_refused_unless_explicitly_allowed() {
    let password = "plaintext-policy-password";
    // TEST-NET-1: never routed, and never contacted by the refused case.
    let url = format!("redis://registry:{password}@192.0.2.1:6379/");
    let fixture = Fixture::new(serde_json::json!({}));
    for node in ["a", "b"] {
        let (status, output) = fixture.fail_to_start(node, &url);
        assert_eq!(status.code(), Some(2), "{output}");
        assert!(
            output.contains("refusing an unencrypted Redis connection to redis://192.0.2.1:6379"),
            "{output}"
        );
        assert!(output.contains("allow_plaintext_redis"), "{output}");
        assert!(!output.contains(password), "{output}");
        assert!(!output.contains("registry"), "{output}");
    }

    // With the opt-in the policy passes and the connection itself is tried.
    // The name cannot resolve, so that fails without any network traffic.
    let allowed = Fixture::new(serde_json::json!({"allow_plaintext_redis": true}));
    let url = format!("redis://registry:{password}@redis.invalid:6379/");
    let (status, output) = allowed.fail_to_start("b", &url);
    assert_eq!(status.code(), Some(1), "{output}");
    assert!(
        output.contains("forwarding Redis connection to redis://redis.invalid:6379"),
        "{output}"
    );
    assert!(!output.contains("refusing"), "{output}");
    assert!(!output.contains(password), "{output}");

    // Disabling verification from the URL is refused, as is a CA without TLS.
    let (status, output) = fixture.fail_to_start(
        "b",
        &format!("rediss://registry:{password}@192.0.2.1/#insecure"),
    );
    assert_eq!(status.code(), Some(2), "{output}");
    assert!(output.contains("#insecure"), "{output}");
    assert!(!output.contains(password), "{output}");
    let with_ca = Fixture::new(serde_json::json!({"redis_ca": "ca.pem"}));
    let (status, output) = with_ca.fail_to_start("b", "redis://127.0.0.1:1/");
    assert_eq!(status.code(), Some(2), "{output}");
    assert!(output.contains("does not use TLS"), "{output}");
}

// ---------------------------------------------------------------------------
// TLS and authentication against a disposable Redis container.

struct Container(String);

impl Container {
    fn docker(arguments: &[&str]) -> std::process::Output {
        Command::new("docker").args(arguments).output().unwrap()
    }

    fn run(arguments: &[&str]) -> String {
        let output = Self::docker(arguments);
        assert!(
            output.status.success(),
            "docker {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// `redis-cli` inside the container, over TLS as the default user.
    fn cli(&self, password: &str, arguments: &[&str]) -> std::process::Output {
        let mut all = vec![
            "exec",
            &self.0,
            "redis-cli",
            "--tls",
            "--cacert",
            "/tls/ca.pem",
            "--no-auth-warning",
            "-a",
            password,
        ];
        all.extend_from_slice(arguments);
        Self::docker(&all)
    }
}

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Self::docker(&["rm", "-f", "-v", &self.0]);
    }
}

#[test]
#[ignore = "requires Docker and MOUNT_RS_REDIS_TLS_TEST_IMAGE"]
fn tls_redis_with_a_private_ca_and_password_serves_the_cluster() {
    let image = std::env::var("MOUNT_RS_REDIS_TLS_TEST_IMAGE")
        .expect("a Redis image to run with TLS, such as redis:8.4.0-alpine");
    let admin_password = "admin-password-5f1c9e";
    let user_password = "registry-password-83ab2d";
    let wrong_password = "wrong-password-0c77e1";

    // A throwaway CA, a server certificate for the loopback address, and an
    // unrelated CA that did not sign it.
    let redis_ca = authority("mount-rs Redis test CA");
    let other_ca = authority("mount-rs unrelated CA");
    let server_key = KeyPair::generate().unwrap();
    let mut params = CertificateParams::new(vec!["127.0.0.1".into()]).unwrap();
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let server = params
        .signed_by(&server_key, &redis_ca.cert, &redis_ca.key)
        .unwrap();
    let tls = tempfile::tempdir().unwrap();
    for (name, content) in [
        ("ca.pem", redis_ca.cert.pem()),
        ("server.pem", server.pem()),
        ("server.key", server_key.serialize_pem()),
    ] {
        std::fs::write(tls.path().join(name), content).unwrap();
    }
    // The server reads these as an unprivileged user inside the container.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tls.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        for entry in std::fs::read_dir(tls.path()).unwrap() {
            let path = entry.unwrap().path();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644)).unwrap();
        }
    }

    // The files are copied in rather than mounted, so that this works with
    // any Docker host. A fixed host port keeps the endpoint stable.
    let port = free_tcp_port();
    let container = Container(unique("mount-rs-redis-tls"));
    let publish = format!("127.0.0.1:{port}:6379");
    Container::run(&[
        "create",
        "--name",
        &container.0,
        "-p",
        &publish,
        &image,
        "redis-server",
        "--port",
        "0",
        "--tls-port",
        "6379",
        "--tls-cert-file",
        "/tls/server.pem",
        "--tls-key-file",
        "/tls/server.key",
        "--tls-ca-cert-file",
        "/tls/ca.pem",
        "--tls-auth-clients",
        "no",
        "--requirepass",
        admin_password,
        "--save",
        "",
        "--appendonly",
        "no",
    ]);
    let source = format!("{}/.", tls.path().display());
    Container::run(&["cp", &source, &format!("{}:/tls", container.0)]);
    Container::run(&["start", &container.0]);
    wait_until("Redis to accept TLS", Duration::from_secs(60), || {
        let ping = container.cli(admin_password, &["ping"]);
        String::from_utf8_lossy(&ping.stdout).trim() == "PONG"
    });
    // An ACL user limited to this cluster's keys and the commands the
    // registry uses is enough; see the README for the rule.
    let acl = container.cli(
        admin_password,
        &[
            "ACL",
            "SETUSER",
            "registry",
            "on",
            &format!(">{user_password}"),
            "~mount-rs:*",
            "+eval",
            "+getrange",
            "+get",
            "+set",
            "+del",
            "+pexpire",
            "+strlen",
            "+ping",
            "+client|setinfo",
        ],
    );
    assert_eq!(String::from_utf8_lossy(&acl.stdout).trim(), "OK");

    let with_ca = |ca: &Identity| {
        let fixture = Fixture::new(serde_json::json!({"redis_ca": "redis-ca.pem"}));
        std::fs::write(fixture.path("redis-ca.pem"), ca.cert.pem()).unwrap();
        fixture
    };
    let target = format!("rediss://127.0.0.1:{port}");

    // An ACL user and the private CA: the owner registers, the ingress
    // looks the route up, and both use the encrypted, authenticated link.
    let fixture = with_ca(&redis_ca);
    let url = format!("rediss://registry:{user_password}@127.0.0.1:{port}/");
    let mut owner = fixture.start("a", &url);
    let mut ingress = fixture.start("b", &url);
    let body = b"written through TLS Redis discovery";
    assert_eq!(owner.put("tls.txt", body).unwrap().0, 200);
    assert_eq!(ingress.get("tls.txt").unwrap(), (200, body.to_vec()));
    let exists = container.cli(admin_password, &["EXISTS", &fixture.route_key()]);
    assert_eq!(String::from_utf8_lossy(&exists.stdout).trim(), "1");
    let clients = container.cli(admin_password, &["CLIENT", "LIST"]);
    let clients = String::from_utf8_lossy(&clients.stdout);
    assert!(clients.contains("user=registry"), "{clients}");

    // Dropping every connection: both servers reconnect, which repeats the
    // handshake and the login, and forwarding resumes without a restart.
    let killed = container.cli(admin_password, &["CLIENT", "KILL", "USER", "registry"]);
    assert_ne!(String::from_utf8_lossy(&killed.stdout).trim(), "0");
    thread::sleep(Duration::from_secs(2));
    wait_until(
        "forwarding after reconnect",
        Duration::from_secs(30),
        || {
            ingress
                .get("tls.txt")
                .is_ok_and(|(status, data)| status == 200 && data == body)
        },
    );
    wait_until(
        "the route to be present after reconnect",
        Duration::from_secs(30),
        || {
            let exists = container.cli(admin_password, &["EXISTS", &fixture.route_key()]);
            String::from_utf8_lossy(&exists.stdout).trim() == "1"
        },
    );
    owner.assert_running("owner");
    ingress.assert_running("ingress");
    for server in [&owner, &ingress] {
        let stderr = server.stderr().join("\n");
        assert!(!stderr.contains(user_password), "{stderr}");
    }
    drop((owner, ingress));

    // The default user with only a password.
    let default_user = with_ca(&redis_ca);
    let url = format!("rediss://:{admin_password}@127.0.0.1:{port}/");
    let mut ingress = default_user.start("b", &url);
    ingress.assert_running("ingress with the default user");
    drop(ingress);

    let refused = |fixture: &Fixture, url: &str| {
        let (status, output) = fixture.fail_to_start("b", url);
        assert_eq!(status.code(), Some(1), "{output}");
        for secret in [admin_password, user_password, wrong_password] {
            assert!(!output.contains(secret), "{output}");
        }
        output
    };

    // A CA that did not sign the server certificate: the handshake fails.
    let url = format!("rediss://registry:{user_password}@127.0.0.1:{port}/");
    let output = refused(&with_ca(&other_ca), &url);
    assert!(
        output.contains(&format!("forwarding Redis connection to {target} failed")),
        "{output}"
    );
    assert!(
        output.to_ascii_lowercase().contains("certificate"),
        "{output}"
    );

    // No CA configured: the system roots do not include the private CA.
    let output = refused(&Fixture::new(serde_json::json!({})), &url);
    assert!(
        output.to_ascii_lowercase().contains("certificate"),
        "{output}"
    );

    // The right CA and the wrong password: the handshake succeeds and the
    // login is refused.
    for url in [
        format!("rediss://registry:{wrong_password}@127.0.0.1:{port}/"),
        format!("rediss://:{wrong_password}@127.0.0.1:{port}/"),
        // No credentials at all against a server that requires them.
        format!("rediss://127.0.0.1:{port}/"),
    ] {
        let output = refused(&with_ca(&redis_ca), &url);
        assert!(
            output.contains(&format!("forwarding Redis connection to {target} failed")),
            "{output}"
        );
        let lower = output.to_ascii_lowercase();
        assert!(
            lower.contains("wrongpass") || lower.contains("auth"),
            "{output}"
        );
        assert!(!lower.contains("certificate"), "{output}");
    }

    // Plaintext to the TLS port fails rather than hanging or succeeding.
    let url = format!("redis://:{admin_password}@127.0.0.1:{port}/");
    let output = refused(&Fixture::new(serde_json::json!({})), &url);
    assert!(
        output.contains(&format!("redis://127.0.0.1:{port}")),
        "{output}"
    );
}

fn outage(link: Link) {
    let (direct_url, upstream) = redis_test_address();
    let direct = redis::Client::open(direct_url).unwrap();
    let mut direct = direct.get_connection().unwrap();
    let proxy = Proxy::start(upstream);
    let fixture = Fixture::new(serde_json::json!({}));
    let url = format!("redis://127.0.0.1:{}/", proxy.port);
    let mut owner = fixture.start("a", &url);
    let mut ingress = fixture.start("b", &url);
    let mut registered = || -> bool {
        redis::cmd("EXISTS")
            .arg(fixture.route_key())
            .query(&mut direct)
            .unwrap()
    };

    let body = b"written before the outage".to_vec();
    assert_eq!(owner.put("before.txt", &body).unwrap().0, 200);
    assert_eq!(ingress.get("before.txt").unwrap(), (200, body.clone()));
    assert!(registered());

    proxy.set(link);
    let down = Instant::now();
    // Three route lifetimes, unless a longer soak is asked for.
    let outage = std::env::var("MOUNT_RS_REDIS_OUTAGE_SECONDS").map_or(ROUTE_TTL * 3, |seconds| {
        Duration::from_secs(seconds.parse().expect("a number of seconds"))
    });
    assert!(outage > ROUTE_TTL);
    let mut forwarded = std::collections::BTreeMap::<String, usize>::new();
    let mut slowest_local = Duration::ZERO;
    let mut slowest_forwarded = Duration::ZERO;
    let mut round = 0;
    while down.elapsed() < outage {
        round += 1;
        // The owner keeps serving its own drive, reads and writes.
        let started = Instant::now();
        let during = format!("during-{round}.txt");
        let read = owner.get("before.txt");
        let write = owner.put(&during, during.as_bytes());
        let read_back = owner.get(&during);
        if let Some(error) = [&read, &write, &read_back]
            .iter()
            .find_map(|result| result.as_ref().err())
        {
            // A server that is shutting down closes its listener first.
            let exit = (0..100).find_map(|_| {
                thread::sleep(Duration::from_millis(100));
                owner.child.try_wait().unwrap()
            });
            panic!(
                "the owner stopped serving its own drive {:?} into the outage: {error}; \
                 exit={exit:?} stderr={:?}",
                down.elapsed(),
                owner.stderr()
            );
        }
        assert_eq!(read.unwrap(), (200, body.clone()));
        assert_eq!(write.unwrap().0, 200);
        assert_eq!(read_back.unwrap(), (200, during.as_bytes().to_vec()));
        slowest_local = slowest_local.max(started.elapsed() / 3);
        // The ingress answers, with an error once its cached route lapses.
        let started = Instant::now();
        let outcome = match ingress.get("before.txt") {
            Ok((status, _)) => status.to_string(),
            Err(error) => format!("error: {error}"),
        };
        slowest_forwarded = slowest_forwarded.max(started.elapsed());
        *forwarded.entry(outcome).or_default() += 1;
        owner.assert_running("owner during the outage");
        ingress.assert_running("ingress during the outage");
        thread::sleep(Duration::from_millis(250));
    }
    // The route was not refreshed, so Redis has expired it.
    assert!(!registered(), "the route outlived an outage of {outage:?}");
    eprintln!(
        "{link:?} outage of {:?}: {round} rounds; owner served every local request \
         (slowest {slowest_local:?}); forwarded outcomes {forwarded:?} \
         (slowest {slowest_forwarded:?})",
        down.elapsed()
    );
    assert!(
        forwarded.keys().all(|outcome| outcome != "200") || forwarded.len() > 1,
        "the ingress never noticed the outage: {forwarded:?}"
    );
    assert!(
        slowest_forwarded < Duration::from_secs(15),
        "a forwarded request waited {slowest_forwarded:?}"
    );

    proxy.set(Link::Up);
    let registered_after = wait_until("the owner to re-register", Duration::from_secs(60), || {
        owner.assert_running("owner after the outage");
        registered()
    });
    let last = format!("during-{round}.txt");
    let forwarding_after = wait_until(
        "the ingress to resume forwarding",
        Duration::from_secs(60),
        || {
            ingress.assert_running("ingress after the outage");
            ingress
                .get(&last)
                .is_ok_and(|(status, data)| status == 200 && data == last.as_bytes())
        },
    );
    eprintln!(
        "{link:?} outage ended: route re-registered after {registered_after:?}, \
         forwarding resumed {forwarding_after:?} later"
    );
    // Both directions work again through the ingress, in the same processes.
    assert_eq!(ingress.put("after.txt", b"after").unwrap().0, 200);
    assert_eq!(owner.get("after.txt").unwrap(), (200, b"after".to_vec()));
    owner.assert_running("owner");
    ingress.assert_running("ingress");
}

#[test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
fn a_stopped_redis_longer_than_the_route_lifetime_is_survived_without_a_restart() {
    outage(Link::Refused);
}

#[test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
fn a_silent_redis_longer_than_the_route_lifetime_is_survived_without_a_restart() {
    outage(Link::Silent);
}
