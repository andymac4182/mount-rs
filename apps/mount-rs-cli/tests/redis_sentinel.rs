//! A forwarding `serve-http` cluster whose route registry is a Redis Sentinel
//! deployment, with the real binary and real Redis processes: one primary,
//! two replicas and three sentinels, started by
//! `scripts/redis-sentinel-fixture.sh` in one Docker container per test. The
//! deployment and cluster helpers are in `sentinel/mod.rs`.
//!
//! Each test kills the primary with SIGKILL and lets the sentinels promote a
//! replica. See "Redis Sentinel" in `docs/serve-http-failover.md` for the
//! rules these tests hold the servers to. Every wait polls for an outcome
//! with a generous limit; what must not happen is checked over the whole
//! failover and again at the end.
#![cfg(unix)]

mod common;
mod sentinel;

use common::*;
use sentinel::*;
use std::{
    thread,
    time::{Duration, Instant},
};

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
        ..Security::default()
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
        ..Security::default()
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
        ..Security::default()
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
    let lease = || cluster.lease();
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

/// The ACL rules `docs/serve-http-runbook.md` gives for the Redis servers
/// (the registry's commands, failover's, and `ROLE`) and for the sentinels.
const DATA_RULES: &[&str] = &[
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
    "+time",
    "+zadd",
    "+zcard",
    "+zscore",
    "+zrange",
    "+zrem",
    "+zremrangebyscore",
    "+role",
];
const SENTINEL_RULES: &[&str] = &["+sentinel|get-master-addr-by-name", "+client|setinfo"];

/// Servers that log in as ACL users holding only the documented rules, on
/// the Redis servers and on the sentinels, through everything the other
/// tests do: startup, publication, forwarding, candidates, a failover of
/// the Redis primary and then of the drive. No command is refused anywhere
/// (`ACL LOG` stays empty), and a user without `ROLE` cannot start.
#[test]
#[ignore = "requires Docker for scripts/redis-sentinel-fixture.sh"]
fn acl_users_with_only_the_documented_rules_are_enough() {
    let mut deployment = Deployment::start(Security {
        password: Some("admin-password-5e81d2"),
        sentinel_password: Some("admin-sentinel-password-c40a17"),
        ..Security::default()
    });
    let user = |port: &str, name: &str, password: &str, rules: &[&str]| {
        let secret = format!(">{password}");
        let mut arguments = vec!["ACL", "SETUSER", name, "reset", "on", secret.as_str()];
        arguments.extend_from_slice(rules);
        assert_eq!(deployment.cli(port, &arguments), "OK", "{name} on {port}");
    };
    let data_password = "registry-password-7b3f90";
    let sentinel_password = "registry-sentinel-password-21d6ae";
    for port in &deployment.data_ports {
        user(port, "mount-rs", data_password, DATA_RULES);
        let without_role = &DATA_RULES[..DATA_RULES.len() - 1];
        user(port, "mount-rs-without-role", data_password, without_role);
    }
    for port in &deployment.sentinel_ports {
        user(port, "mount-rs-sentinel", sentinel_password, SENTINEL_RULES);
    }
    deployment.set_client(Security {
        username: Some("mount-rs"),
        password: Some(data_password),
        sentinel_username: Some("mount-rs-sentinel"),
        sentinel_password: Some(sentinel_password),
        tls: false,
    });
    let security = deployment.client();
    let refused = |port: &str| deployment.cli(port, &["ACL", "LOG"]);
    let cluster = Cluster::new(&deployment, &["a", "b", "c"], &["a", "b"]);

    // The rules are enforced: without ROLE the primary cannot be checked.
    let (code, output) = cluster.fail_to_start("c", &security, |sentinel| {
        sentinel["username"] = "mount-rs-without-role".into();
    });
    assert_eq!(code, Some(1), "{output}");
    assert!(output.contains("forwarding Redis Sentinel"), "{output}");
    let first = deployment.primary();
    assert!(refused(&first).contains("role"), "{}", refused(&first));
    assert_eq!(deployment.cli(&first, &["ACL", "LOG", "RESET"]), "OK");

    let a = cluster.start("a", &security);
    wait_until("a to publish the drive", Duration::from_secs(30), || {
        !a.states("published").is_empty()
    });
    let mut b = cluster.start("b", &security);
    let mut c = cluster.start("c", &security);
    let route = deployment
        .route(&first, &cluster.namespace)
        .expect("the route on the primary");
    assert_eq!(route.0, "a");
    let fence = route.1;
    assert_eq!(c.put("kept", b"owned by a").unwrap().0, 200);
    assert_eq!(b.get("kept").unwrap(), (200, b"owned by a".to_vec()));
    // Both candidates announce themselves, in the sorted set of the drive.
    let pattern = format!("mount-rs:{}:*", cluster.namespace);
    wait_until(
        "both candidates to be announced",
        Duration::from_secs(30),
        || {
            deployment
                .cli(&first, &["KEYS", &pattern])
                .lines()
                .any(|key| {
                    deployment.cli(&first, &["TYPE", key]) == "zset"
                        && deployment.cli(&first, &["ZCARD", key]) == "2"
                })
        },
    );
    thread::sleep(Duration::from_secs(6));
    assert_eq!(refused(&first), "");

    // A failover of the Redis primary.
    assert_eq!(deployment.kill_primary(), first);
    let second = deployment.wait_failover(&first);
    wait_until("a's route on the new primary", REDIS_FAILOVER_LIMIT, || {
        deployment.route(&second, &cluster.namespace) == Some(("a".to_owned(), fence))
    });
    write_settled(&c, "during", b"written after the Redis failover");

    // A failover of the drive, through the promoted primary.
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
    wait_until("the ingress to reach b", Duration::from_secs(60), || {
        matches!(
            http_within(c.address, "GET", "during", b"", Duration::from_secs(5)),
            Ok((200, body)) if body == b"written after the Redis failover"
        )
    });
    assert_eq!(cluster.lease().0, Some("b".to_owned()));
    b.assert_running("b");
    c.assert_running("c");

    // Nothing the servers sent was refused, on any Redis server or sentinel
    // still running.
    for port in deployment.data_ports.iter().chain(&deployment.sentinel_ports) {
        if *port != first {
            assert_eq!(refused(port), "", "ACL LOG on {port}");
        }
    }
}
