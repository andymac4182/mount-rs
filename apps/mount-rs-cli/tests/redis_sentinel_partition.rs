//! Network partitions inside a Redis Sentinel deployment under a forwarding
//! `serve-http` cluster, with the real binary and real Redis processes: one
//! primary, two replicas and three sentinels, started by
//! `scripts/redis-sentinel-fixture.sh` with `MOUNT_RS_SENTINEL_FIXTURE_PARTITION`
//! so that links between the processes, and from the host to any of them,
//! can be cut. Packets are dropped, not refused.
//!
//! The cases: the old primary cut off from the sentinels and replicas while
//! every mount-rs server can still reach it and it still accepts writes; the
//! same split with failover candidates on either side of it; the servers cut
//! off from the primary while the sentinels are not; and the servers able to
//! reach only the old primary. See "Redis Sentinel" in
//! `docs/serve-http-failover.md` for the rules these tests hold the servers
//! to. Every wait polls for an outcome with a generous limit; what must not
//! happen is checked over the whole partition and again at the end.
#![cfg(unix)]

mod common;
mod sentinel;

use common::*;
use sentinel::*;
use std::{
    thread,
    time::{Duration, Instant},
};

/// The processes of a deployment, by port.
struct Ports {
    primary: String,
    replicas: Vec<String>,
    sentinels: Vec<String>,
}

impl Ports {
    fn of(deployment: &Deployment) -> Self {
        let primary = deployment.primary();
        assert_eq!(primary, deployment.data_ports[0]);
        Self {
            primary,
            replicas: deployment.data_ports[1..].to_vec(),
            sentinels: deployment.sentinel_ports.clone(),
        }
    }

    /// Every process but the primary.
    fn all_but_primary(&self) -> Vec<&str> {
        self.replicas
            .iter()
            .chain(&self.sentinels)
            .map(String::as_str)
            .collect()
    }
}

fn strs(ports: &[String]) -> Vec<&str> {
    ports.iter().map(String::as_str).collect()
}

/// Wait for every sentinel of `sentinels` to name the same primary, other
/// than `old`, and for that server to report `master`. Returns its port.
fn wait_promoted(deployment: &Deployment, old: &str, sentinels: &[String]) -> String {
    let mut promoted = String::new();
    wait_until(
        "the sentinels to promote a replica",
        REDIS_FAILOVER_LIMIT,
        || {
            let named: Vec<Option<String>> = sentinels
                .iter()
                .map(|port| deployment.named_by(port))
                .collect();
            match named.first() {
                Some(Some(first))
                    if first != old
                        && named.iter().all(|other| other.as_ref() == Some(first))
                        && deployment.role(first) == "master" =>
                {
                    promoted = first.clone();
                    true
                }
                _ => false,
            }
        },
    );
    promoted
}

/// A route record like `record` that names `node` under `fence`: what a
/// later owner would publish.
fn forged_record(record: &str, node: &str, fence: u64) -> String {
    let mut route: serde_json::Value = serde_json::from_str(&record[21..]).unwrap();
    route["node"] = node.into();
    route["fence"] = fence.into();
    format!("{fence:020}|{route}")
}

/// One owner and one ingress. The primary is cut off from the sentinels
/// and both replicas, but not from the mount-rs servers, and keeps
/// accepting writes. The sentinels promote a replica; every server leaves
/// the old primary; what was written to it alone is discarded when it
/// rejoins as a replica, and is never read.
#[test]
#[ignore = "requires Docker for scripts/redis-sentinel-fixture.sh"]
fn every_server_leaves_an_isolated_primary_that_still_accepts_writes() {
    let security = Security::default();
    let deployment = Deployment::start_partitionable(security.clone());
    let ports = Ports::of(&deployment);
    let cluster = Cluster::new(&deployment, &["a", "b"], &[]);
    let mut owner = cluster.start("a", &security);
    let mut ingress = cluster.start("b", &security);
    let old = ports.primary.clone();
    let key = route_key(&cluster.namespace);
    let probe = format!("mount-rs:{}:partition-probe", cluster.namespace);

    let published = deployment
        .route(&old, &cluster.namespace)
        .expect("the owner's route on the primary");
    assert_eq!(published.0, "a");
    assert_eq!(ingress.put("before", b"written before").unwrap().0, 200);
    assert_eq!(
        ingress.get("before").unwrap(),
        (200, b"written before".to_vec())
    );
    let connected = deployment.host_clients(&old);
    assert!(connected >= 2, "{connected} server connections");

    let direct = observe(owner.address, "before", b"written before".to_vec());
    let forwarded = observe(ingress.address, "before", b"written before".to_vec());
    deployment.partition(&[&old], &ports.all_but_primary());
    let cut = Instant::now();
    // Cut off, the old primary still takes writes.
    assert_eq!(
        deployment.cli(&old, &["SET", &probe, "written to the isolated primary"]),
        "OK"
    );

    let new = wait_promoted(&deployment, &old, &ports.sentinels);
    let promoted = cut.elapsed();
    assert_ne!(new, old);
    // It does not know it was replaced.
    assert_eq!(deployment.role(&old), "master");

    // Every server leaves it: its connections from the host close.
    let mut drops = Vec::new();
    let mut last = connected;
    wait_until(
        "every server to leave the isolated primary",
        REDIS_FAILOVER_LIMIT,
        || {
            let now = deployment.host_clients(&old);
            if now < last {
                drops.push(cut.elapsed());
            }
            last = now;
            now == 0
        },
    );
    let left = cut.elapsed();
    wait_until("the route on the new primary", REDIS_FAILOVER_LIMIT, || {
        deployment.route(&new, &cluster.namespace) == Some(published.clone())
    });
    let republished = cut.elapsed();
    write_settled(&ingress, "during", b"written during the split");
    let forwarding = cut.elapsed();

    // What a server that still used the old primary would act on: a route
    // for the drive under a later fence, naming another node. An owner
    // without failover that read it would stop. Nobody reads it.
    let forged = forged_record(&deployment.record(&old, &cluster.namespace), "b", {
        published.1 + 1
    });
    assert_eq!(
        deployment.cli(&old, &["SET", &key, &forged, "PX", "60000"]),
        "OK"
    );
    // More than a refresh interval of the owner and many lookups of the
    // ingress.
    thread::sleep(Duration::from_secs(6));
    assert_eq!(deployment.host_clients(&old), 0);
    assert_eq!(
        deployment.route(&old, &cluster.namespace),
        Some(("b".to_owned(), published.1 + 1))
    );
    assert_eq!(
        deployment.route(&new, &cluster.namespace),
        Some(published.clone())
    );
    owner.assert_running("the owner");

    // Healed, the old primary is made a replica of the new one and loses
    // everything it accepted alone.
    deployment.heal();
    let healed = Instant::now();
    wait_until(
        "the old primary to become a replica",
        REDIS_FAILOVER_LIMIT,
        || deployment.role(&old) == "slave",
    );
    let demoted = healed.elapsed();
    wait_until(
        "the old primary to drop what it accepted alone",
        REDIS_FAILOVER_LIMIT,
        || {
            deployment.route(&old, &cluster.namespace) == Some(published.clone())
                && deployment.cli(&old, &["EXISTS", &probe]) == "0"
        },
    );
    let resynced = healed.elapsed();
    assert_eq!(deployment.cli(&new, &["EXISTS", &probe]), "0");
    // Two refresh intervals with the old primary back: nobody returns to
    // it, and the route is still the owner's, under its fence.
    thread::sleep(Duration::from_secs(11));
    assert_eq!(deployment.host_clients(&old), 0);
    for sentinel in &ports.sentinels {
        assert_eq!(deployment.named_by(sentinel).as_deref(), Some(new.as_str()));
    }
    assert_eq!(
        deployment.route(&new, &cluster.namespace),
        Some(published.clone())
    );
    assert_eq!(
        ingress.get("during").unwrap(),
        (200, b"written during the split".to_vec())
    );

    let direct = finish(direct);
    let forwarded = finish(forwarded);
    // The owner served every direct request: it never depends on Redis.
    assert!(direct.wrong.is_empty(), "{:?}", direct.wrong);
    assert_eq!(direct.unavailable, 0);
    assert!(direct.ok > 0);
    // A forwarded request was answered by the owner, the only server with
    // the drive, or not served at all.
    assert!(forwarded.wrong.is_empty(), "{:?}", forwarded.wrong);
    assert!(forwarded.ok > 0);
    owner.assert_running("the owner");
    ingress.assert_running("the ingress");
    eprintln!(
        "isolated old primary: promoted {promoted:.1?} after the cut; the servers' {connected} \
         connections to the old primary closed at {drops:.1?}, the last {left:.1?} after the \
         cut ({:.1?} after the promotion); the owner's route on the new primary after \
         {republished:.1?}; a forwarded write succeeded after {forwarding:.1?}; after the \
         heal the old primary was a replica after {demoted:.1?} and had dropped its own writes \
         after {resynced:.1?}; forwarded reads: {} served, {} refused, longest gap {:.1?}; \
         direct reads: {} served, longest gap {:.1?}",
        left.saturating_sub(promoted),
        forwarded.ok,
        forwarded.unavailable,
        forwarded.longest_gap,
        direct.ok,
        direct.longest_gap
    );
}

/// Two failover candidates and an ingress, split by the partition. The
/// owner `a` can reach only the sentinel left with the old primary, so it
/// keeps publishing there; the standby `b` and the ingress `c` reach only
/// the two sentinels that promote a replica, which has no route once the
/// replicated one expires. `b` tries the drive and finds the lease held;
/// nothing changes owner, and the routes converge after the heal.
#[test]
#[ignore = "requires Docker for scripts/redis-sentinel-fixture.sh"]
fn the_lease_holds_when_an_owner_and_a_standby_see_different_primaries() {
    let security = Security::default();
    let deployment = Deployment::start_partitionable(security.clone());
    let ports = Ports::of(&deployment);
    let old = ports.primary.clone();
    let minority = ports.sentinels[0].clone();
    let majority = ports.sentinels[1..].to_vec();
    let cluster = Cluster::new(&deployment, &["a", "b", "c"], &["a", "b"]);
    cluster.only_sentinels("a", deployment.sentinel_addresses(&[&minority]));
    for node in ["b", "c"] {
        cluster.only_sentinels(node, deployment.sentinel_addresses(&strs(&majority)));
    }
    let key = route_key(&cluster.namespace);
    let mut a = cluster.start("a", &security);
    wait_until("a to publish the drive", Duration::from_secs(30), || {
        !a.states("published").is_empty()
    });
    let mut b = cluster.start("b", &security);
    let mut c = cluster.start("c", &security);
    let route = deployment
        .route(&old, &cluster.namespace)
        .expect("the route on the primary");
    assert_eq!(route.0, "a");
    let fence = route.1;
    let owned = (Some("a".to_owned()), fence);
    assert_eq!(cluster.lease(), owned);
    assert_eq!(c.put("kept", b"owned by a").unwrap().0, 200);
    // The standby saw a's route; it has had no reason to try the lease.
    thread::sleep(Duration::from_secs(3));
    assert!(b.states("lease_held").is_empty(), "{:?}", b.events());

    let direct = observe(a.address, "kept", b"owned by a".to_vec());
    let through_b = observe(b.address, "kept", b"owned by a".to_vec());
    let through_c = observe(c.address, "kept", b"owned by a".to_vec());
    // The old primary and one sentinel on one side; both replicas and two
    // sentinels, a quorum and a majority, on the other.
    let mut other_side = strs(&ports.replicas);
    other_side.extend(strs(&majority));
    deployment.partition(&[&old, &minority], &other_side);
    let cut = Instant::now();
    let new = wait_promoted(&deployment, &old, &majority);
    let promoted = cut.elapsed();
    // The minority sentinel still names the old primary, which still
    // believes it is the primary: two primaries.
    assert_eq!(deployment.named_by(&minority).as_deref(), Some(old.as_str()));
    assert_eq!(deployment.role(&old), "master");

    // a's route on the new primary is the replicated copy, which nothing
    // refreshes: it lapses. The standby then reads that no route exists.
    wait_until(
        "a's route to lapse on the new primary",
        ROUTE_TTL + Duration::from_secs(30),
        || deployment.route(&new, &cluster.namespace).is_none(),
    );
    let lapsed = cut.elapsed();
    wait_until(
        "b to try the drive and find its lease held",
        LEASE + Duration::from_secs(30),
        || !b.states("lease_held").is_empty(),
    );
    let tried = cut.elapsed();
    // Keep the split for another lease period, while b keeps retrying.
    thread::sleep(LEASE + Duration::from_secs(3));
    // a still publishes on the old primary: the record is fresh there.
    assert_eq!(
        deployment.route(&old, &cluster.namespace),
        Some(("a".to_owned(), fence))
    );
    let remaining = deployment.pttl(&old, &key);
    assert!(
        remaining > (ROUTE_TTL - Duration::from_secs(7)).as_millis() as i64,
        "{remaining} ms left on the old primary"
    );
    // b holds no route, so the new primary has none.
    assert_eq!(deployment.route(&new, &cluster.namespace), None);
    assert_eq!(cluster.lease(), owned);
    assert!(b.states("acquired").is_empty(), "{:?}", b.events());
    assert!(a.states("released").is_empty(), "{:?}", a.events());
    a.assert_running("a");
    b.assert_running("b");
    c.assert_running("c");
    let split_for = cut.elapsed();

    // Healed: the minority sentinel learns of the promotion, a leaves the
    // old primary and publishes its route, under the same fence, on the
    // new one.
    deployment.heal();
    let healed = Instant::now();
    wait_until("a's route on the new primary", REDIS_FAILOVER_LIMIT, || {
        deployment.route(&new, &cluster.namespace) == Some(("a".to_owned(), fence))
    });
    let converged = healed.elapsed();
    wait_until("c to forward to a again", Duration::from_secs(60), || {
        matches!(
            http_within(c.address, "GET", "kept", b"", Duration::from_secs(5)),
            Ok((200, body)) if body == b"owned by a"
        )
    });
    let forwarding = healed.elapsed();
    wait_until(
        "the old primary to become a replica",
        REDIS_FAILOVER_LIMIT,
        || deployment.role(&old) == "slave",
    );
    write_settled(&c, "after", b"written after the heal");
    // Two refresh intervals after the heal: still one owner, one fence.
    thread::sleep(Duration::from_secs(10));
    assert_eq!(
        deployment.route(&new, &cluster.namespace),
        Some(("a".to_owned(), fence))
    );
    assert_eq!(cluster.lease(), owned);
    assert!(b.states("acquired").is_empty(), "{:?}", b.events());
    assert!(a.states("released").is_empty(), "{:?}", a.events());
    assert_eq!(
        b.get("after").unwrap(),
        (200, b"written after the heal".to_vec())
    );

    let direct = finish(direct);
    let through_b = finish(through_b);
    let through_c = finish(through_c);
    // a served every request sent to it; every other answer came from a,
    // the only server that ever opened the drive, or was a 503.
    assert!(direct.wrong.is_empty(), "{:?}", direct.wrong);
    assert_eq!(direct.unavailable, 0);
    assert!(through_b.wrong.is_empty(), "{:?}", through_b.wrong);
    assert!(through_c.wrong.is_empty(), "{:?}", through_c.wrong);
    // Without a route on the new primary, forwarded requests were refused.
    assert!(through_c.unavailable > 0);
    a.assert_running("a");
    b.assert_running("b");
    c.assert_running("c");
    eprintln!(
        "split brain with candidates: promoted {promoted:.1?} after the cut; a's route lapsed \
         on the new primary {lapsed:.1?} after the cut; b found the lease held {tried:.1?} \
         after the cut; split held {split_for:.1?}; lease and fence unchanged throughout; after \
         the heal a's route was on the new primary in {converged:.1?} and c forwarded to a in \
         {forwarding:.1?}; direct reads: {} served; through b: {} served, {} refused; through \
         c: {} served, {} refused, longest gap {:.1?}",
        direct.ok,
        through_b.ok,
        through_b.unavailable,
        through_c.ok,
        through_c.unavailable,
        through_c.longest_gap
    );
}

/// The servers cannot reach the primary; the sentinels and replicas can,
/// so there is no failover. This is a Redis outage for the servers: the
/// owner keeps serving its drive, forwarded requests get 503, nobody exits,
/// and everything resumes on the same primary when the link returns.
#[test]
#[ignore = "requires Docker for scripts/redis-sentinel-fixture.sh"]
fn servers_cut_off_from_the_primary_but_not_from_the_sentinels() {
    let security = Security::default();
    let deployment = Deployment::start_partitionable(security.clone());
    let ports = Ports::of(&deployment);
    let cluster = Cluster::new(&deployment, &["a", "b"], &[]);
    let mut owner = cluster.start("a", &security);
    let mut ingress = cluster.start("b", &security);
    let primary = ports.primary.clone();
    let published = deployment
        .route(&primary, &cluster.namespace)
        .expect("the owner's route on the primary");
    assert_eq!(ingress.put("before", b"written before").unwrap().0, 200);

    let direct = observe(owner.address, "before", b"written before".to_vec());
    deployment.cut_host(&[&primary]);
    let cut = Instant::now();
    let refused = wait_until(
        "forwarded requests to be refused",
        Duration::from_secs(60),
        || {
            matches!(
                http_within(ingress.address, "GET", "before", b"", Duration::from_secs(15)),
                Ok((503, _))
            )
        },
    );
    // Longer than a route lives without a refresh: the owner cannot reach
    // the primary to refresh it, and the record expires there.
    let forwarded = observe(ingress.address, "before", b"written before".to_vec());
    wait_until(
        "the unrefreshed route to expire",
        ROUTE_TTL + Duration::from_secs(10),
        || deployment.route(&primary, &cluster.namespace).is_none(),
    );
    thread::sleep(Duration::from_secs(3));
    let during = finish(forwarded);
    // No failover: every sentinel still names the same primary.
    for sentinel in &ports.sentinels {
        assert_eq!(
            deployment.named_by(sentinel).as_deref(),
            Some(primary.as_str())
        );
    }
    assert_eq!(deployment.role(&primary), "master");
    owner.assert_running("the owner");
    ingress.assert_running("the ingress");
    let cut_for = cut.elapsed();

    deployment.heal();
    let healed = Instant::now();
    wait_until("the route to be published again", REDIS_FAILOVER_LIMIT, || {
        deployment.route(&primary, &cluster.namespace) == Some(published.clone())
    });
    let republished = healed.elapsed();
    write_settled(&ingress, "after", b"written after");
    let forwarding = healed.elapsed();

    let direct = finish(direct);
    assert!(direct.wrong.is_empty(), "{:?}", direct.wrong);
    assert_eq!(direct.unavailable, 0);
    assert!(direct.ok > 0);
    // While cut off, a forwarded request was refused, never misrouted.
    assert!(during.wrong.is_empty(), "{:?}", during.wrong);
    assert_eq!(during.ok, 0, "served {} while cut off", during.ok);
    assert!(during.unavailable > 0);
    owner.assert_running("the owner");
    ingress.assert_running("the ingress");
    eprintln!(
        "servers cut off from the primary: forwarded requests refused {refused:.1?} after the \
         cut; cut for {cut_for:.1?}, {} forwarded reads refused and none served meanwhile; \
         direct reads: {} served, longest gap {:.1?}; after the heal the route was back in \
         {republished:.1?} and a forwarded write succeeded after {forwarding:.1?}",
        during.unavailable,
        direct.ok,
        direct.longest_gap
    );
}

/// The servers can reach only the old primary: not the sentinels, and the
/// old primary is cut off from everything else. They keep using it (a
/// check that cannot reach a sentinel is no evidence), consistently, so
/// forwarding still works. When the old primary rejoins as a replica they
/// stop using it, and with no sentinel to name another primary, forwarded
/// requests get 503 until the sentinels are reachable again.
#[test]
#[ignore = "requires Docker for scripts/redis-sentinel-fixture.sh"]
fn servers_that_reach_only_the_old_primary() {
    let security = Security::default();
    let deployment = Deployment::start_partitionable(security.clone());
    let ports = Ports::of(&deployment);
    let cluster = Cluster::new(&deployment, &["a", "b"], &[]);
    let mut owner = cluster.start("a", &security);
    let mut ingress = cluster.start("b", &security);
    let old = ports.primary.clone();
    let key = route_key(&cluster.namespace);
    let published = deployment
        .route(&old, &cluster.namespace)
        .expect("the owner's route on the primary");
    assert_eq!(ingress.put("before", b"written before").unwrap().0, 200);
    let connected = deployment.host_clients(&old);

    let direct = observe(owner.address, "before", b"written before".to_vec());
    let forwarded = observe(ingress.address, "before", b"written before".to_vec());
    deployment.cut_host(&strs(&ports.sentinels));
    deployment.partition(&[&old], &ports.all_but_primary());
    let cut = Instant::now();
    let new = wait_promoted(&deployment, &old, &ports.sentinels);
    let promoted = cut.elapsed();
    // Two refresh intervals after the promotion: the servers are still on
    // the old primary, the owner still refreshes its route there, and
    // forwarding works through it.
    thread::sleep(Duration::from_secs(10));
    assert_eq!(deployment.host_clients(&old), connected);
    let remaining = deployment.pttl(&old, &key);
    assert!(
        remaining > (ROUTE_TTL - Duration::from_secs(7)).as_millis() as i64,
        "{remaining} ms left on the old primary"
    );
    write_settled(&ingress, "split", b"written through the old primary");
    let stayed = finish(forwarded);
    assert!(stayed.wrong.is_empty(), "{:?}", stayed.wrong);
    assert!(stayed.ok > 0);

    // The old primary rejoins and becomes a replica; the servers still
    // cannot reach a sentinel. They stop using it, and have no primary.
    let forwarded = observe(ingress.address, "before", b"written before".to_vec());
    deployment.heal_links();
    let healed = Instant::now();
    wait_until(
        "the old primary to become a replica",
        REDIS_FAILOVER_LIMIT,
        || deployment.role(&old) == "slave",
    );
    let demoted = healed.elapsed();
    let mut closed = None;
    let refused = wait_until(
        "forwarded requests to be refused",
        Duration::from_secs(60),
        || {
            if closed.is_none() && deployment.host_clients(&old) == 0 {
                closed = Some(healed.elapsed());
            }
            matches!(
                http_within(ingress.address, "GET", "before", b"", Duration::from_secs(15)),
                Ok((503, _))
            )
        },
    );
    wait_until(
        "the servers to stop using the replica",
        Duration::from_secs(30),
        || deployment.host_clients(&old) == 0,
    );
    let left = healed.elapsed();
    thread::sleep(Duration::from_secs(5));
    let without = finish(forwarded);

    // With the sentinels reachable again they find the new primary.
    let forwarded = observe(ingress.address, "before", b"written before".to_vec());
    deployment.heal_host();
    let reachable = Instant::now();
    wait_until("the route on the new primary", REDIS_FAILOVER_LIMIT, || {
        deployment.route(&new, &cluster.namespace) == Some(published.clone())
    });
    let republished = reachable.elapsed();
    write_settled(&ingress, "after", b"written after");
    let after = finish(forwarded);

    let direct = finish(direct);
    assert!(direct.wrong.is_empty(), "{:?}", direct.wrong);
    assert_eq!(direct.unavailable, 0);
    for observed in [&without, &after] {
        assert!(observed.wrong.is_empty(), "{:?}", observed.wrong);
    }
    assert!(without.unavailable > 0);
    owner.assert_running("the owner");
    ingress.assert_running("the ingress");
    eprintln!(
        "servers reaching only the old primary: promoted {promoted:.1?} after the cut; the \
         servers stayed on the old primary and forwarding worked through it ({} reads served, \
         {} refused); after the old primary rejoined it was a replica in {demoted:.1?}, the \
         servers' connections to it closed {closed:.1?} / all by {left:.1?} after the heal \
         and forwarded requests were refused {refused:.1?} after it was demoted ({} refused, \
         {} served meanwhile); with the sentinels back the route was on the new primary in \
         {republished:.1?}; direct reads: {} served, longest gap {:.1?}",
        stayed.ok,
        stayed.unavailable,
        without.unavailable,
        without.ok,
        direct.ok,
        direct.longest_gap
    );
}
