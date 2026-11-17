//! Drive failover between two `serve-http` candidates (`a`, `b`) and an
//! ingress (`c`), with the real binary, a real Redis and SQLite storage that
//! the candidates share. Each server reaches Redis through its own proxy, so
//! one of them can be cut off while the others are not.
//!
//! See `docs/serve-http-failover.md` for the rules these tests hold the
//! servers to. They never assert on a fixed sleep: every wait polls for an
//! outcome with a generous limit, and what must *not* happen is checked over
//! an observation window and again at the end.
#![cfg(unix)]

mod common;

use common::*;
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const REDIS_ENV: &str = "FAILOVER_TEST_REDIS_URL";
const TOKEN_ENV: &str = "FAILOVER_TEST_TOKEN";
/// The writer lease of the shared drive. Short, so that a crashed owner's
/// lease is not what a test waits for, and long enough that its renewals
/// (every eighth of it, each allowed as long again) survive a slow runner.
const LEASE: Duration = Duration::from_secs(12);
/// `poll_interval_ms` and `takeover_stagger_ms`, at their defaults.
const POLL: Duration = Duration::from_secs(1);
const STAGGER: Duration = Duration::from_secs(2);
/// Added to every documented bound: process start-up, the open of the
/// drive, and a loaded runner.
const SLACK: Duration = Duration::from_secs(20);

struct Cluster {
    dir: tempfile::TempDir,
    namespace: String,
    upstream: SocketAddr,
    direct: Mutex<redis::Connection>,
}

impl Cluster {
    /// `failover` is `http.forwarding.failover` of both candidates, and
    /// `http` is merged into every server's `http`.
    fn new(failover: serde_json::Value, http: serde_json::Value) -> Self {
        let (direct_url, upstream) = redis_test_address();
        let direct = redis::Client::open(direct_url)
            .unwrap()
            .get_connection()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, content: String| {
            std::fs::write(dir.path().join(name), content).unwrap();
        };
        std::fs::create_dir(dir.path().join("shared")).unwrap();
        let ca = authority("mount-rs failover test CA");
        write("ca.pem", ca.cert.pem());
        let mut peers = Vec::new();
        for node in ["a", "b", "c"] {
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
        let namespace = unique("failover");
        for node in ["a", "b", "c"] {
            let candidate = node != "c";
            let mut config = serde_json::json!({"version":1,"http":{
                "host":"127.0.0.1","port":0,"drives":[],"forwarding":{
                    "peers":"peers.json","certificate":format!("{node}.pem"),
                    "private_key":format!("{node}.key"),"ca":"ca.pem",
                    "redis_url":{"env":REDIS_ENV},"namespace":namespace,
                    "bind":"127.0.0.1:0","owner":candidate
                }
            }});
            if candidate {
                config["http"]["forwarding"]["failover"] = failover.clone();
                config["http"]["drives"] = serde_json::json!([{
                    "id":"volume","token":{"env":TOKEN_ENV},
                    "driver":{"kind":"splitstore","storage":{
                        "metadata":{"kind":"sqlite","path":"./shared/meta.db"},
                        "blocks":{"kind":"sqlite","path":"./shared/blocks.db"},
                        "chunk_size_bytes":65536,"owner":node,
                        "lease_ttl_ms":LEASE.as_millis() as u64}}
                }]);
            }
            for (key, value) in http.as_object().unwrap() {
                config["http"][key] = value.clone();
            }
            write(&format!("{node}.json"), config.to_string());
        }
        Self {
            dir,
            namespace,
            upstream,
            direct: Mutex::new(direct),
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    /// A proxy to the test Redis, for the servers that should share a link.
    fn proxy(&self) -> Proxy {
        Proxy::start(self.upstream)
    }

    /// Start `node`, reaching Redis through `proxy`, and wait for its public
    /// listener. A candidate has made its first decision by then.
    fn start(&self, node: &str, proxy: &Proxy) -> Server {
        let mut command = Command::new(env!("CARGO_BIN_EXE_mount-rs"));
        command
            .current_dir(self.dir.path())
            .args(["serve-http", "--config"])
            .arg(self.path(&format!("{node}.json")))
            .env(REDIS_ENV, format!("redis://127.0.0.1:{}/", proxy.port))
            .env(TOKEN_ENV, TOKEN)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        Server::spawn(&mut command, node)
    }

    /// `(node, fence)` of the route published in Redis, read directly.
    fn route(&self) -> Option<(String, u64)> {
        let record: Option<String> = redis::cmd("GET")
            .arg(format!("mount-rs:{}:route:volume", self.namespace))
            .query(&mut *self.direct.lock().unwrap())
            .unwrap();
        let record = record?;
        let route: serde_json::Value = serde_json::from_str(&record[21..]).unwrap();
        Some((
            route["node"].as_str().unwrap().to_owned(),
            route["fence"].as_u64().unwrap(),
        ))
    }

    /// `(owner label, fence)` of the drive's writer lease row in storage.
    fn lease(&self) -> (Option<String>, u64) {
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
}

/// The fence under which `server` last reported acquiring the drive. Its
/// report reaches this process a moment after the route reaches Redis.
fn acquired_fence(server: &Server) -> u64 {
    wait_states(server, "acquired", 1, SLACK);
    server
        .states("acquired")
        .last()
        .unwrap_or_else(|| panic!("{} never acquired: {:?}", server.name, server.events()))["fence"]
        .as_u64()
        .unwrap()
}

/// Wait for `server` to have reported `count` records of `state`.
fn wait_states(server: &Server, state: &str, count: usize, limit: Duration) -> Duration {
    wait_until(
        &format!("{} to report {state} x{count}", server.name),
        limit,
        || server.states(state).len() >= count,
    )
}

/// `count` bytes that depend on `seed`, so that a file holding another
/// file's bytes, a truncated file or a mix of two versions is recognised.
fn content(seed: &str, count: usize) -> Vec<u8> {
    let mut state = seed.bytes().fold(0x9e37_79b9_7f4a_7c15_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    (0..count)
        .map(|_| {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (state >> 56) as u8
        })
        .collect()
}

/// GET `name` from `server`, repeated while the answer is a 5xx or none:
/// right after a takeover an ingress may still hold a stale route or a
/// pooled connection to the old owner process.
fn read_settled(server: &Server, name: &str) -> (u16, Vec<u8>) {
    let start = Instant::now();
    loop {
        match http_within(server.address, "GET", name, b"", Duration::from_secs(5)) {
            Ok((status, body)) if status < 500 => return (status, body),
            outcome => assert!(
                start.elapsed() < Duration::from_secs(90),
                "GET {name} from {} kept failing: {outcome:?}",
                server.name
            ),
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// PUT `name` through `server`, repeated while it is refused or unanswered.
fn write_settled(server: &Server, name: &str, body: &[u8]) {
    let start = Instant::now();
    loop {
        match http_within(server.address, "PUT", name, body, Duration::from_secs(5)) {
            Ok((200, _)) => return,
            outcome => assert!(
                start.elapsed() < Duration::from_secs(90),
                "PUT {name} through {} kept failing: {outcome:?}",
                server.name
            ),
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// How many files `server` holds open, where the platform can say.
fn open_files(server: &Server) -> Option<usize> {
    let pid = server.child.id();
    if let Ok(entries) = std::fs::read_dir(format!("/proc/{pid}/fd")) {
        return Some(entries.count());
    }
    let listed = Command::new("lsof")
        .args(["-p", &pid.to_string()])
        .output()
        .ok()?;
    listed
        .status
        .success()
        .then(|| String::from_utf8_lossy(&listed.stdout).lines().count())
}

/// An owner that cannot reach Redis, while its standby and the ingress can,
/// keeps its drive: it serves requests that reach it, the standby finds the
/// lease held and takes nothing, and the route returns when the link does.
fn owner_cut_off_from_redis(link: Link) {
    let cluster = Cluster::new(serde_json::json!({}), serde_json::json!({}));
    let (owner_link, others) = (cluster.proxy(), cluster.proxy());
    let mut a = cluster.start("a", &owner_link);
    wait_states(&a, "published", 1, SLACK);
    let fence = acquired_fence(&a);
    let mut b = cluster.start("b", &others);
    let mut c = cluster.start("c", &others);
    assert_eq!(cluster.route(), Some(("a".into(), fence)));
    assert_eq!(cluster.lease(), (Some("a".into()), fence));

    let before = content("before", 200_000);
    assert_eq!(c.put("before.bin", &before).unwrap().0, 200);
    assert_eq!(b.get("before.bin").unwrap(), (200, before.clone()));

    owner_link.set(link);
    let cut = Instant::now();
    // Long enough for the route to lapse, for the standby to try the lease
    // several times, and for a lease that was not being renewed to expire.
    let partition = ROUTE_TTL * 2 + LEASE;
    let mut acknowledged = BTreeMap::new();
    let mut slowest = Duration::ZERO;
    let mut unrouted_at = None;
    // The standby's open files once it has started retrying the lease.
    let mut retrying = None;
    let mut round = 0;
    while cut.elapsed() < partition {
        round += 1;
        // Requests that reach the owner are served, reads and writes.
        let name = format!("during-{round}.bin");
        let body = content(&name, 70_000);
        let started = Instant::now();
        let wrote = a.put(&name, &body);
        let read = a.get("before.bin");
        slowest = slowest.max(started.elapsed() / 2);
        assert_eq!(
            wrote.as_ref().map(|(status, _)| *status).ok(),
            Some(200),
            "the owner refused a direct write {:?} into the partition: {wrote:?}",
            cut.elapsed()
        );
        assert_eq!(read.ok(), Some((200, before.clone())));
        acknowledged.insert(name, body);
        // The standby never serves the drive itself: it has not opened it.
        // Once the owner's route has lapsed it has nowhere to forward to.
        if unrouted_at.is_none() && cluster.route().is_none() {
            unrouted_at = Some(cut.elapsed());
        }
        if let Some(lapsed) = unrouted_at {
            let (status, _) =
                http_within(b.address, "GET", "before.bin", b"", Duration::from_secs(10)).unwrap();
            // The route cache of the standby may outlive the record by 1 s.
            assert!(
                status == 503 || lapsed + Duration::from_secs(2) > cut.elapsed(),
                "the standby answered {status} with no route published"
            );
        }
        assert!(b.states("acquired").is_empty(), "{:?}", b.events());
        assert!(b.states("published").is_empty(), "{:?}", b.events());
        assert_eq!(cluster.lease(), (Some("a".into()), fence));
        if retrying.is_none() && !b.states("lease_held").is_empty() {
            retrying = Some((cut.elapsed(), open_files(&b)));
        }
        for server in [&mut a, &mut b, &mut c] {
            server.assert_running("a server during the partition");
        }
        thread::sleep(Duration::from_millis(250));
    }
    let unrouted_at = unrouted_at.expect("the owner's route never lapsed");
    assert!(
        unrouted_at <= ROUTE_TTL + Duration::from_secs(5),
        "{unrouted_at:?}"
    );
    // The standby saw the absence, tried, found the lease held, said so
    // once, and kept trying without ever opening the drive.
    assert_eq!(b.states("lease_held").len(), 1, "{:?}", b.events());
    assert!(b.states("open_failed").is_empty(), "{:?}", b.events());
    // Each retry opens the storage and gives it up again. It keeps nothing:
    // a standby may retry every few seconds for as long as an outage lasts.
    let (since, early) = retrying.expect("the standby never tried the lease");
    let late = open_files(&b);
    let retried_for = cut.elapsed() - since;
    assert!(retried_for > POLL * 15, "{retried_for:?}");
    if let (Some(early), Some(late)) = (early, late) {
        assert!(
            late <= early + 4,
            "the standby went from {early} to {late} open files in {retried_for:?} of retries"
        );
    }
    assert_eq!(a.states("route_unpublished").len(), 1, "{:?}", a.events());
    assert!(a.states("released").is_empty(), "{:?}", a.events());

    owner_link.set(Link::Up);
    let healed = Instant::now();
    let republished = wait_until(
        "the owner's route to return",
        Duration::from_secs(60),
        || cluster.route() == Some(("a".into(), fence)),
    );
    let last = format!("during-{round}.bin");
    wait_until("forwarding to resume", Duration::from_secs(60), || {
        c.get(&last)
            .is_ok_and(|(status, body)| status == 200 && body == acknowledged[&last])
    });
    let forwarding = healed.elapsed();
    eprintln!(
        "{link:?} partition of the owner for {:?}: {round} direct writes served (slowest \
         request {slowest:?}); route lapsed after {unrouted_at:?}; route back {republished:?} \
         and forwarding back {forwarding:?} after the link returned; the standby retried \
         the lease for {retried_for:?}, its open files going from {early:?} to {late:?}",
        partition
    );
    // Nothing that was acknowledged is missing, through either other server.
    for (name, body) in &acknowledged {
        assert_eq!(read_settled(&c, name), (200, body.clone()), "{name}");
    }
    assert_eq!(read_settled(&b, &last).0, 200);
    assert_eq!(c.put("after.bin", b"after").unwrap().0, 200);
    assert_eq!(a.get("after.bin").unwrap(), (200, b"after".to_vec()));
    // One owner throughout, under one fence, and still exactly one episode.
    assert_eq!(cluster.lease(), (Some("a".into()), fence));
    // The owner reports the route as published again once Redis has
    // answered it, which can be a poll after the record itself appeared.
    wait_states(&a, "published", 2, Duration::from_secs(60));
    assert_eq!(a.states("acquired").len(), 1, "{:?}", a.events());
    assert!(a.states("released").is_empty(), "{:?}", a.events());
    assert!(b.states("acquired").is_empty(), "{:?}", b.events());
    assert_eq!(b.states("lease_held").len(), 1, "{:?}", b.events());
}

#[test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
fn an_owner_whose_redis_is_refused_keeps_its_drive_and_the_standby_takes_nothing() {
    owner_cut_off_from_redis(Link::Refused);
}

#[test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
fn an_owner_whose_redis_is_silent_keeps_its_drive_and_the_standby_takes_nothing() {
    owner_cut_off_from_redis(Link::Silent);
}

/// Redis is down for every server and the owner is killed meanwhile: the
/// standby opens nothing on a registry it cannot read, for longer than the
/// route and the lease last, and takes over once Redis answers again.
#[test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
fn a_standby_waits_out_a_redis_outage_before_replacing_an_owner_killed_during_it() {
    let cluster = Cluster::new(serde_json::json!({}), serde_json::json!({}));
    let link = cluster.proxy();
    let mut a = cluster.start("a", &link);
    wait_states(&a, "published", 1, SLACK);
    let fence = acquired_fence(&a);
    let mut b = cluster.start("b", &link);
    let mut c = cluster.start("c", &link);
    let before = content("before", 200_000);
    assert_eq!(c.put("before.bin", &before).unwrap().0, 200);

    link.set(Link::Refused);
    a.child.kill().unwrap();
    a.child.wait().unwrap();
    let killed = Instant::now();
    // Past the route's and the lease's expiry: with a readable registry the
    // standby would have taken over by now.
    let outage = ROUTE_TTL.max(LEASE) + POLL * 5 + Duration::from_secs(5);
    let mut answers = BTreeMap::<String, usize>::new();
    while killed.elapsed() < outage {
        assert!(b.states("acquired").is_empty(), "{:?}", b.events());
        assert!(b.states("open_failed").is_empty(), "{:?}", b.events());
        assert!(b.states("lease_held").is_empty(), "{:?}", b.events());
        // The dead owner's lease is untouched, and expires by itself.
        assert_eq!(cluster.lease(), (Some("a".into()), fence));
        for server in [&b, &c] {
            let outcome = match http_within(server.address, "GET", "before.bin", b"", POLL * 10) {
                Ok((status, _)) => status.to_string(),
                Err(error) => format!("{:?}", error.kind()),
            };
            assert_ne!(outcome, "200", "{} served a drive nobody owns", server.name);
            *answers.entry(outcome).or_default() += 1;
        }
        b.assert_running("the standby during the outage");
        c.assert_running("the ingress during the outage");
        thread::sleep(Duration::from_millis(250));
    }

    link.set(Link::Up);
    // Documented: once the route is read as absent, `P + k x S + 5P + open`.
    // The dead owner's announcement has expired, so k is 0 here.
    let bound = POLL * 6 + STAGGER;
    let took = wait_until("the standby to take over", bound + SLACK, || {
        b.assert_running("the standby after the outage");
        cluster.route() == Some(("b".into(), fence + 1))
    });
    eprintln!(
        "Redis down for {outage:?} with the owner killed: answers meanwhile {answers:?}; the \
         standby published {took:?} after Redis returned (documented bound {bound:?} plus \
         reconnecting and the open)"
    );
    assert_eq!(cluster.lease(), (Some("b".into()), fence + 1));
    assert_eq!(acquired_fence(&b), fence + 1);
    assert_eq!(b.states("acquired").len(), 1, "{:?}", b.events());
    assert_eq!(read_settled(&c, "before.bin"), (200, before.clone()));
    assert_eq!(read_settled(&b, "before.bin"), (200, before));
    write_settled(&c, "after.bin", b"after the outage");
    assert_eq!(
        read_settled(&b, "after.bin"),
        (200, b"after the outage".to_vec())
    );
}

/// With `fail_back`, a drive taken by the less preferred candidate returns
/// to the preferred one once it has been back for `fail_back_after_seconds`,
/// exactly once, and then stays there.
#[test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
fn fail_back_returns_the_drive_to_the_preferred_node_once_and_it_stays() {
    let after = Duration::from_secs(10);
    let cluster = Cluster::new(
        serde_json::json!({"fail_back": true, "fail_back_after_seconds": after.as_secs()}),
        serde_json::json!({}),
    );
    let ranked = mount_rs_cluster::placement::ranking("volume", ["a", "b"]);
    let (preferred, other) = (ranked[0], ranked[1]);
    let link = cluster.proxy();
    // The less preferred candidate is alone at first, so it takes the drive.
    let mut second = cluster.start(other, &link);
    wait_states(&second, "published", 1, SLACK);
    let fence = acquired_fence(&second);
    let mut c = cluster.start("c", &link);
    let mut written = BTreeMap::new();
    for index in 0..5 {
        let name = format!("before-{index}.bin");
        let body = content(&name, 100_000);
        assert_eq!(c.put(&name, &body).unwrap().0, 200);
        written.insert(name, body);
    }

    let mut first = cluster.start(preferred, &link);
    // The owner reads the candidates every third of the route TTL, so it
    // sees the preferred node within 5 s and hands back `after` later. The
    // preferred node then takes the released drive like any standby.
    let bound = ROUTE_TTL / 3 + after + POLL * 6;
    let took = wait_until(
        "the drive to return to the preferred node",
        bound + SLACK,
        || cluster.route() == Some((preferred.into(), fence + 1)),
    );
    assert!(
        took >= after,
        "handed back after {took:?}, before the preference lasted {after:?}"
    );
    wait_states(&second, "released", 1, SLACK);
    assert_eq!(
        second.states("released")[0]["reason"],
        "fail_back",
        "{:?}",
        second.events()
    );
    let unavailable = {
        // From the old owner's release to the new owner's publication,
        // measured by a client that retries.
        let start = Instant::now();
        read_settled(&c, "before-0.bin");
        start.elapsed()
    };
    eprintln!(
        "fail-back: the drive returned to {preferred} {took:?} after it started (bound \
         {bound:?}); a retrying client was served again {unavailable:?} after the new route"
    );

    // Nothing moves again: longer than the hold-off, the hand-back delay
    // and its doubling put together.
    let window = ROUTE_TTL + after * 3 + Duration::from_secs(5);
    let observed = Instant::now();
    let mut round = 0;
    while observed.elapsed() < window {
        round += 1;
        let name = format!("after-{round}.bin");
        let body = content(&name, 30_000);
        // Through the ingress and through the node that handed back.
        let via = if round % 2 == 0 { &c } else { &second };
        write_settled(via, &name, &body);
        written.insert(name, body);
        assert_eq!(cluster.route(), Some((preferred.into(), fence + 1)));
        assert_eq!(cluster.lease(), (Some(preferred.into()), fence + 1));
        for server in [&mut first, &mut second, &mut c] {
            server.assert_running("a server after the fail-back");
        }
        thread::sleep(Duration::from_millis(500));
    }
    assert_eq!(first.states("acquired").len(), 1, "{:?}", first.events());
    assert!(first.states("released").is_empty(), "{:?}", first.events());
    assert_eq!(second.states("acquired").len(), 1, "{:?}", second.events());
    assert_eq!(second.states("released").len(), 1, "{:?}", second.events());
    for (name, body) in &written {
        assert_eq!(read_settled(&c, name), (200, body.clone()), "{name}");
    }
}

/// What a client knows about one write it attempted.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Outcome {
    Acknowledged,
    /// A 5xx, a timeout or a broken connection: the write may have happened.
    Unknown(String),
}

/// Clients write through the ingress and the standby while the owner is
/// killed. Every acknowledged write is there afterwards, and a write that
/// was not acknowledged is either absent or complete: never a part of it.
#[test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
fn writes_across_a_killed_owner_are_complete_or_absent_and_never_lost_once_acknowledged() {
    let cluster = Cluster::new(serde_json::json!({}), serde_json::json!({}));
    let link = cluster.proxy();
    let mut a = cluster.start("a", &link);
    wait_states(&a, "published", 1, SLACK);
    let fence = acquired_fence(&a);
    let b = cluster.start("b", &link);
    let c = cluster.start("c", &link);

    // Several chunks per file, so that a partial write would show.
    let size = 300_000;
    let stop = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(Mutex::new(Vec::<(String, Outcome)>::new()));
    // One file overwritten again and again: (version, outcome), in order.
    let versions = Arc::new(Mutex::new(Vec::<(u64, Outcome)>::new()));
    let put = |address: SocketAddr, name: &str, body: &[u8]| {
        // A client's own timeout, shorter than the forwarder's 30 s.
        match http_within(address, "PUT", name, body, Duration::from_secs(5)) {
            Ok((200, _)) => Outcome::Acknowledged,
            Ok((status, _)) => {
                assert!(
                    matches!(status, 502 | 503),
                    "PUT {name} was answered {status}"
                );
                Outcome::Unknown(status.to_string())
            }
            Err(error) => Outcome::Unknown(format!("{:?}", error.kind())),
        }
    };
    let mut writers = Vec::new();
    for (writer, address) in [("c1", c.address), ("c2", c.address), ("b", b.address)] {
        let (stop, attempts) = (stop.clone(), attempts.clone());
        writers.push(thread::spawn(move || {
            let mut index = 0;
            while !stop.load(Ordering::Acquire) {
                let name = format!("w-{writer}-{index}.bin");
                let outcome = put(address, &name, &content(&name, size));
                attempts.lock().unwrap().push((name, outcome));
                index += 1;
            }
        }));
    }
    {
        let (stop, versions, address) = (stop.clone(), versions.clone(), c.address);
        writers.push(thread::spawn(move || {
            let mut version = 0;
            while !stop.load(Ordering::Acquire) {
                version += 1;
                let body = content(&format!("version-{version}"), size);
                let outcome = put(address, "overwritten.bin", &body);
                versions.lock().unwrap().push((version, outcome));
            }
        }));
    }
    let acknowledged = |attempts: &Mutex<Vec<(String, Outcome)>>| {
        attempts
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, outcome)| *outcome == Outcome::Acknowledged)
            .count()
    };
    wait_until("writes to be flowing", SLACK, || {
        acknowledged(&attempts) >= 20 && versions.lock().unwrap().len() >= 5
    });

    a.child.kill().unwrap();
    a.child.wait().unwrap();
    let served_before = acknowledged(&attempts);
    // Documented: max(R + k x S, L) + 5P + open after the crash.
    let bound = (ROUTE_TTL + STAGGER).max(LEASE) + POLL * 5;
    let took = wait_until("the standby to take over", bound + SLACK, || {
        cluster.route() == Some(("b".into(), fence + 1))
    });
    // Keep writing into the new owner for a while, then stop.
    wait_until(
        "writes to be served by the new owner",
        Duration::from_secs(90),
        || acknowledged(&attempts) >= served_before + 20,
    );
    stop.store(true, Ordering::Release);
    for writer in writers {
        writer.join().unwrap();
    }
    assert_eq!(cluster.lease(), (Some("b".into()), fence + 1));

    let attempts = attempts.lock().unwrap().clone();
    let mut summary = BTreeMap::<String, usize>::new();
    let (mut absent, mut complete) = (0, 0);
    for (name, outcome) in &attempts {
        let expected = content(name, size);
        let (status, body) = read_settled(&c, name);
        match outcome {
            Outcome::Acknowledged => {
                *summary.entry("200".into()).or_default() += 1;
                assert_eq!(status, 200, "acknowledged write {name} is missing");
                assert!(
                    body == expected,
                    "acknowledged write {name} has other content"
                );
            }
            Outcome::Unknown(how) => {
                *summary.entry(how.clone()).or_default() += 1;
                match status {
                    404 => absent += 1,
                    200 => {
                        assert!(
                            body == expected,
                            "unacknowledged write {name} ({how}) left {} of {size} bytes",
                            body.len()
                        );
                        complete += 1;
                    }
                    other => panic!("GET {name} after an unacknowledged write: {other}"),
                }
            }
        }
        // The new owner itself agrees with what the ingress forwards.
        assert_eq!(read_settled(&b, name), (status, body), "{name}");
    }
    // The overwritten file holds exactly one whole version: the last one
    // acknowledged, or one attempted after it whose outcome was unknown.
    let versions = versions.lock().unwrap().clone();
    let last_acknowledged = versions
        .iter()
        .rev()
        .find(|(_, outcome)| *outcome == Outcome::Acknowledged)
        .map(|(version, _)| *version)
        .expect("no overwrite was acknowledged");
    let (status, body) = read_settled(&c, "overwritten.bin");
    assert_eq!(status, 200);
    let holds = versions
        .iter()
        .map(|(version, _)| *version)
        .find(|version| body == content(&format!("version-{version}"), size));
    assert!(
        holds.is_some_and(|version| version >= last_acknowledged),
        "the overwritten file holds {holds:?} ({} bytes); the last acknowledged version is \
         {last_acknowledged} of {}",
        body.len(),
        versions.len()
    );
    eprintln!(
        "kill -9 of the owner under {} writes: new route after {took:?} (documented bound \
         {bound:?} plus the open); outcomes {summary:?}; of the unacknowledged writes \
         {absent} are absent and {complete} complete; the overwritten file holds version \
         {holds:?}, last acknowledged {last_acknowledged}",
        attempts.len(),
    );
    assert!(
        summary.keys().any(|outcome| outcome != "200"),
        "no write overlapped the failure: {summary:?}"
    );
}

/// An owner that stops running for longer than its lease (a paused VM, a
/// long stall) and then resumes: the standby has taken the drive by then,
/// and the old owner must not serve what it remembers or write anything.
#[test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
fn an_owner_paused_past_its_lease_gives_the_drive_up_when_it_resumes() {
    let cluster = Cluster::new(serde_json::json!({}), serde_json::json!({}));
    let link = cluster.proxy();
    let mut a = cluster.start("a", &link);
    wait_states(&a, "published", 1, SLACK);
    let fence = acquired_fence(&a);
    let mut b = cluster.start("b", &link);
    let c = cluster.start("c", &link);
    let old = content("old", 150_000);
    assert_eq!(c.put("file.bin", &old).unwrap().0, 200);
    assert_eq!(a.get("file.bin").unwrap(), (200, old.clone()));

    a.signal(libc::SIGSTOP);
    let paused = Instant::now();
    let bound = (ROUTE_TTL + STAGGER).max(LEASE) + POLL * 5;
    let took = wait_until("the standby to take over", bound + SLACK, || {
        cluster.route() == Some(("b".into(), fence + 1))
    });
    let new = content("new", 150_000);
    write_settled(&c, "file.bin", &new);
    write_settled(&c, "only-new.bin", b"written under the new owner");
    assert_eq!(read_settled(&b, "file.bin"), (200, new.clone()));

    a.signal(libc::SIGCONT);
    let resumed = Instant::now();
    // The old owner still has the old file in memory. It may refuse, or
    // forward to the new owner once it has let go; it must never answer
    // with what it remembers, and its writes must not land.
    let mut answers = BTreeMap::<u16, usize>::new();
    let released = wait_until("the old owner to give the drive up", SLACK, || {
        if let Ok((status, body)) =
            http_within(a.address, "GET", "file.bin", b"", Duration::from_secs(5))
        {
            assert!(
                status != 200 || body == new,
                "the resumed owner served its stale copy {:?} after resuming",
                resumed.elapsed()
            );
            *answers.entry(status).or_default() += 1;
        }
        let _ = http_within(
            a.address,
            "PUT",
            "stale-write.bin",
            b"from the old owner",
            Duration::from_secs(5),
        );
        !a.states("released").is_empty()
    });
    assert_eq!(
        a.states("released")[0]["reason"],
        "lease_lost",
        "{:?}",
        a.events()
    );
    // From here it is a standby: its listener forwards to the new owner.
    wait_until("the old owner to forward to the new one", SLACK, || {
        a.get("file.bin")
            .is_ok_and(|answer| answer == (200, new.clone()))
    });
    eprintln!(
        "owner paused for {:?}: the standby published {took:?} after the pause began (bound \
         {bound:?} plus the open); the resumed owner released {released:?} after resuming, \
         answering {answers:?} meanwhile",
        resumed.duration_since(paused)
    );
    // A write sent to the old owner either failed or was forwarded to the
    // new one; it never bypassed it.
    let (status, body) = read_settled(&c, "stale-write.bin");
    assert!(
        status == 404 || (status == 200 && body == b"from the old owner"),
        "{status}"
    );
    assert_eq!(read_settled(&c, "file.bin"), (200, new));
    // The new owner was never displaced and the old one did not retake.
    a.assert_running("the resumed owner");
    b.assert_running("the new owner");
    assert_eq!(cluster.lease(), (Some("b".into()), fence + 1));
    assert_eq!(cluster.route(), Some(("b".into(), fence + 1)));
    assert_eq!(a.states("acquired").len(), 1, "{:?}", a.events());
    assert!(b.states("released").is_empty(), "{:?}", b.events());
}

/// Storage that stops answering while the owner is told to stop: the
/// release is abandoned after `drain_timeout_ms + drive_shutdown_timeout_ms`,
/// the process says so and exits non-zero, and the standby takes the drive
/// when the abandoned lease has expired.
#[test]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
fn a_release_that_storage_does_not_answer_is_abandoned_and_the_lease_expires() {
    let (drain, shutdown) = (Duration::from_millis(500), Duration::from_millis(1500));
    let cluster = Cluster::new(
        serde_json::json!({}),
        serde_json::json!({
            "drain_timeout_ms": drain.as_millis() as u64,
            "drive_shutdown_timeout_ms": shutdown.as_millis() as u64,
        }),
    );
    let link = cluster.proxy();
    let mut a = cluster.start("a", &link);
    wait_states(&a, "published", 1, SLACK);
    let fence = acquired_fence(&a);
    let mut b = cluster.start("b", &link);
    let c = cluster.start("c", &link);
    let before = content("before", 100_000);
    assert_eq!(c.put("before.bin", &before).unwrap().0, 200);

    // Hold SQLite's write lock: the owner's release (a write) waits for it,
    // for up to its 5 s busy timeout, which is longer than the limit above.
    let mut blocker = rusqlite::Connection::open(cluster.path("shared/meta.db")).unwrap();
    blocker.busy_timeout(Duration::from_secs(30)).unwrap();
    let lock = blocker
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .unwrap();
    a.signal(libc::SIGTERM);
    let asked = Instant::now();
    let status = a.wait_exit(Duration::from_secs(60));
    let exited = asked.elapsed();
    drop(lock);
    drop(blocker);
    let stderr = a.stderr().join("\n");
    assert!(!status.success(), "exit {status}; {stderr}");
    assert!(
        stderr.contains("were not released within 2000 ms"),
        "{stderr}"
    );
    assert!(
        exited < drain + shutdown + SLACK,
        "the owner took {exited:?} to give up on its release"
    );
    // The lease was not released: it is still the old owner's.
    assert_eq!(cluster.lease(), (Some("a".into()), fence));

    let bound = LEASE + POLL * 5 + Duration::from_secs(60);
    let took = wait_until("the standby to take the expired lease", bound, || {
        b.assert_running("the standby");
        cluster.route() == Some(("b".into(), fence + 1))
    });
    eprintln!(
        "release blocked by storage: the owner exited {status} {exited:?} after SIGTERM \
         (limit {:?}); the standby published {:?} after that exit ({took:?} after the lock \
         was dropped)",
        drain + shutdown,
        asked.elapsed() - exited,
    );
    assert_eq!(cluster.lease(), (Some("b".into()), fence + 1));
    assert_eq!(read_settled(&c, "before.bin"), (200, before));
    write_settled(&c, "after.bin", b"after");
}
