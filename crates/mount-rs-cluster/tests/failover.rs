//! Failover of one volume between candidate owners that share a real Redis
//! registry and a SQLite-backed split store, whose writer lease is the only
//! authority. Each "node" is a `FailoverSupervisor` with its own peer router.
#![cfg(feature = "http3")]

use mount_rs_cluster::{
    DriveFactory, DriveStatus, FailoverDrive, FailoverEvent, FailoverOptions, FailoverSupervisor,
    FailoverTimings, OwnerHttpRouter, RedisRouteRegistry, ReleaseReason, SessionBudget, placement,
};
use mount_rs_sdk::{Filesystem, SplitOptions, SplitRuntimeOptions, StoreConfig};
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

const VOLUME: &str = "shared";
const ROUTE_TTL: Duration = Duration::from_millis(1500);
const LEASE_TTL: Duration = Duration::from_secs(2);
const TOKEN: &str = "failover-test-token-0123456789abcdef";

async fn registry(namespace: &str) -> RedisRouteRegistry {
    let client = redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
    RedisRouteRegistry::with_manager(
        redis::aio::ConnectionManager::new(client).await.unwrap(),
        namespace.to_owned(),
        Duration::from_secs(2),
    )
    .unwrap()
}

fn namespace(test: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("failover-{test}-{}-{nanos}", std::process::id())
}

/// What a node's volume factory was asked to do, and a switch to break it.
#[derive(Default)]
struct Opens {
    attempts: AtomicUsize,
    broken: AtomicBool,
}

fn factory(dir: PathBuf, owner: String, opens: Arc<Opens>) -> Arc<dyn DriveFactory> {
    Arc::new(move || {
        let (dir, owner, opens) = (dir.clone(), owner.clone(), opens.clone());
        async move {
            opens.attempts.fetch_add(1, Ordering::SeqCst);
            if opens.broken.load(Ordering::SeqCst) {
                return Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio)
                    .with_syscall("test-open"));
            }
            std::fs::create_dir_all(dir.join("blocks")).unwrap();
            let options = SplitOptions {
                metadata: StoreConfig::Sqlite {
                    path: dir.join("meta.db"),
                },
                blocks: StoreConfig::Local {
                    path: dir.join("blocks"),
                },
                ..SplitOptions::memory(owner, 4096)
            };
            let runtime = SplitRuntimeOptions {
                lease_ttl: LEASE_TTL,
                read_lease_cache_window: LEASE_TTL / 4,
                ..Default::default()
            };
            Filesystem::split_with_runtime(options, runtime)
                .await
                .map(Arc::new)
        }
    })
}

struct Node {
    name: String,
    router: OwnerHttpRouter,
    supervisor: FailoverSupervisor,
    events: Arc<Mutex<Vec<FailoverEvent>>>,
    opens: Arc<Opens>,
}
impl Node {
    async fn start(name: &str, namespace: &str, dir: &Path, timings: FailoverTimings) -> Self {
        let router = OwnerHttpRouter::new(Vec::new()).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let opens = Arc::new(Opens::default());
        let recorded = events.clone();
        let supervisor = FailoverSupervisor::start(
            registry(namespace).await,
            router.clone(),
            vec![FailoverDrive {
                volume: VOLUME.into(),
                bearer_token: TOKEN.into(),
                sessions: None,
                factory: factory(dir.to_owned(), name.to_owned(), opens.clone()),
            }],
            FailoverOptions {
                timings,
                route_ttl: ROUTE_TTL,
                node: name.to_owned(),
                boot: format!("{name}-boot"),
                address: "127.0.0.1:4443".parse().unwrap(),
                tls_server_name: "localhost".into(),
                http: mount_rs_http::HttpServerOptions {
                    drain_timeout: Duration::from_secs(5),
                    ..Default::default()
                },
                max_sessions_per_drive: 4,
                max_handles_per_session: 4,
                session_idle_timeout: Duration::from_secs(60),
                session_budget: SessionBudget::new(16).unwrap(),
                trusted: Arc::new(|node| node.starts_with("node-")),
                events: Arc::new(move |event| recorded.lock().unwrap().push(event.clone())),
            },
        )
        .unwrap();
        Self {
            name: name.to_owned(),
            router,
            supervisor,
            events,
            opens,
        }
    }
    fn owns(&self) -> bool {
        !self.router.volumes().is_empty()
    }
    fn events(&self) -> Vec<FailoverEvent> {
        self.events.lock().unwrap().clone()
    }
    fn fence(&self) -> Option<u64> {
        match self.supervisor.statuses()[0].1 {
            DriveStatus::Owner { fence, .. } => Some(fence),
            _ => None,
        }
    }
}

fn timings() -> FailoverTimings {
    FailoverTimings {
        poll: Duration::from_millis(100),
        stagger: Duration::from_millis(300),
        fail_back_after: None,
        fail_back_hold_off: ROUTE_TTL,
    }
}

async fn eventually(what: &str, within: Duration, mut condition: impl AsyncFnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !condition().await {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The node named by the published route, with its fence.
async fn published(registry: &RedisRouteRegistry) -> Option<(String, u64)> {
    registry
        .lookup(VOLUME)
        .await
        .unwrap()
        .map(|route| (route.node, route.fence))
}

/// The two node names, the one preferred for the volume first.
fn ranked() -> (String, String) {
    let ranked = placement::ranking(VOLUME, ["node-a", "node-b"]);
    (ranked[0].to_owned(), ranked[1].to_owned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn redis_candidates_expire_on_the_server_clock_and_can_be_withdrawn() {
    let registry = registry(&namespace("candidates")).await;
    assert!(registry.candidates(VOLUME).await.unwrap().is_empty());
    let ttl = Duration::from_millis(400);
    assert_eq!(
        registry
            .announce_candidate(VOLUME, "node-a", ttl)
            .await
            .unwrap(),
        ["node-a"]
    );
    let mut both = registry
        .announce_candidate(VOLUME, "node-b", Duration::from_secs(5))
        .await
        .unwrap();
    both.sort();
    assert_eq!(both, ["node-a", "node-b"]);
    // Another volume has its own set.
    assert!(registry.candidates("other").await.unwrap().is_empty());
    // node-a is not renewed and expires; node-b is still announced.
    tokio::time::sleep(ttl + Duration::from_millis(200)).await;
    assert_eq!(registry.candidates(VOLUME).await.unwrap(), ["node-b"]);
    registry.withdraw_candidate(VOLUME, "node-b").await.unwrap();
    assert!(registry.candidates(VOLUME).await.unwrap().is_empty());
    // Withdrawing an absent candidate is not an error.
    registry.withdraw_candidate(VOLUME, "node-b").await.unwrap();
    for invalid in ["", "bad:node", "bad node"] {
        assert!(
            registry
                .announce_candidate(VOLUME, invalid, ttl)
                .await
                .is_err()
        );
    }
    assert!(
        registry
            .announce_candidate("bad:volume", "node-a", ttl)
            .await
            .is_err()
    );
    assert!(
        registry
            .announce_candidate(VOLUME, "node-a", Duration::from_secs(31))
            .await
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn redis_clean_stop_hands_the_volume_to_the_standby_with_a_higher_fence() {
    let dir = tempfile::tempdir().unwrap();
    let namespace = namespace("clean");
    let observer = registry(&namespace).await;
    let a = Node::start("node-a", &namespace, dir.path(), timings()).await;
    assert!(a.supervisor.settled(Duration::from_secs(10)).await);
    assert!(
        a.owns(),
        "the only candidate takes the volume at startup: {:?}",
        a.events()
    );
    let first = a.fence().unwrap();
    assert_eq!(
        published(&observer).await,
        Some(("node-a".into(), first)),
        "the route is published before the node reports itself settled"
    );

    let b = Node::start("node-b", &namespace, dir.path(), timings()).await;
    assert!(b.supervisor.settled(Duration::from_secs(10)).await);
    // A standby does not open the volume while a route is published.
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(!b.owns());
    assert_eq!(b.opens.attempts.load(Ordering::SeqCst), 0);
    assert_eq!(b.supervisor.statuses()[0].1, DriveStatus::Standby);
    let mut both = observer.candidates(VOLUME).await.unwrap();
    both.sort();
    assert_eq!(both, ["node-a", "node-b"]);

    let stopped = Instant::now();
    a.supervisor.shutdown().await;
    assert!(!a.owns());
    eventually("node-b to publish", Duration::from_secs(10), async || {
        published(&observer)
            .await
            .is_some_and(|(node, _)| node == "node-b")
    })
    .await;
    let took = stopped.elapsed();
    let (_, second) = published(&observer).await.unwrap();
    assert!(second > first, "{second} > {first}");
    assert!(b.owns());
    // Far sooner than the lease (2 s) plus the route (1.5 s) would allow
    // after a crash: both were given up, not left to expire.
    assert!(took < Duration::from_millis(1500), "{took:?}");
    assert_eq!(observer.candidates(VOLUME).await.unwrap(), ["node-b"]);
    assert!(a.events().contains(&FailoverEvent::Released {
        volume: VOLUME.into(),
        fence: first,
        reason: ReleaseReason::Shutdown,
    }));
    assert_eq!(a.supervisor.statuses()[0].1, DriveStatus::Stopped);
    assert!(!a.supervisor.failed());

    b.supervisor.shutdown().await;
    assert_eq!(published(&observer).await, None);
    assert!(observer.candidates(VOLUME).await.unwrap().is_empty());
    assert!(!b.supervisor.failed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn redis_crashed_owner_is_replaced_only_after_its_lease_expires() {
    let dir = tempfile::tempdir().unwrap();
    let namespace = namespace("crash");
    let observer = registry(&namespace).await;
    let a = Node::start("node-a", &namespace, dir.path(), timings()).await;
    assert!(a.supervisor.settled(Duration::from_secs(10)).await);
    let first = a.fence().expect("node-a owns the volume");
    let b = Node::start("node-b", &namespace, dir.path(), timings()).await;
    assert!(b.supervisor.settled(Duration::from_secs(10)).await);

    // Write through the owner's filesystem so there is something to keep.
    let (_, filesystem) = a.supervisor.filesystems().remove(0);
    filesystem
        .driver()
        .write_file("/kept", b"before the crash")
        .await
        .unwrap();
    drop(filesystem);

    // A crash: the tasks stop, nothing is withdrawn and nothing is released.
    let crashed = Instant::now();
    let Node {
        supervisor, router, ..
    } = a;
    drop(supervisor);
    drop(router);
    eventually("node-b to take over", Duration::from_secs(20), async || {
        b.owns()
    })
    .await;
    let took = crashed.elapsed();
    // The route lapsed first (1.5 s after its last refresh, at most 0.5 s
    // before the crash). The open could not succeed before the lease
    // expired (2 s after its last renewal, at most 0.25 s before the
    // crash), however often it was tried: the first try found it held.
    assert!(took >= Duration::from_millis(1700), "{took:?}");
    assert!(b.opens.attempts.load(Ordering::SeqCst) >= 2);
    assert!(b.events().contains(&FailoverEvent::LeaseHeld {
        volume: VOLUME.into()
    }));
    eventually("node-b to publish", Duration::from_secs(5), async || {
        published(&observer)
            .await
            .is_some_and(|(node, _)| node == "node-b")
    })
    .await;
    let (_, second) = published(&observer).await.unwrap();
    assert!(second > first);
    // max(R + k x S, L) + 5P, plus a generous allowance for a busy machine.
    assert!(took < Duration::from_secs(8), "{took:?}");

    let (_, filesystem) = b.supervisor.filesystems().remove(0);
    let handle = filesystem.driver().open("/kept", "r", 0).await.unwrap();
    let mut buffer = vec![0u8; 64];
    let read = handle.read(&mut buffer, Some(0)).await.unwrap();
    assert_eq!(&buffer[..read], b"before the crash");
    handle.close().await.unwrap();
    drop(handle);
    drop(filesystem);
    b.supervisor.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn redis_owner_that_loses_its_lease_withdraws_and_returns_to_standby() {
    let dir = tempfile::tempdir().unwrap();
    let namespace = namespace("lost");
    let observer = registry(&namespace).await;
    let a = Node::start("node-a", &namespace, dir.path(), timings()).await;
    assert!(a.supervisor.settled(Duration::from_secs(10)).await);
    let first = a.fence().expect("node-a owns the volume");

    // The volume is shut down underneath the supervisor: its lease is gone,
    // exactly as when a renewal fails and the volume fails closed.
    let (_, filesystem) = a.supervisor.filesystems().remove(0);
    filesystem.shutdown().await.unwrap();
    drop(filesystem);
    eventually(
        "the lost lease to be noticed",
        Duration::from_secs(5),
        async || {
            a.events().contains(&FailoverEvent::Released {
                volume: VOLUME.into(),
                fence: first,
                reason: ReleaseReason::LeaseLost,
            })
        },
    )
    .await;
    // Nothing is served or published without a lease. (The node may by now
    // have reopened the volume; that is a new, higher fence.)
    let after = published(&observer).await;
    assert!(
        after.as_ref().is_none_or(|(_, fence)| *fence > first),
        "{after:?}"
    );

    // With nobody else to take it, the node reopens the volume itself after
    // its back-off, under a new fence.
    eventually("node-a to reopen", Duration::from_secs(10), async || {
        a.fence().is_some_and(|fence| fence > first)
            && published(&observer)
                .await
                .is_some_and(|(node, fence)| node == "node-a" && fence > first)
    })
    .await;
    assert_eq!(a.opens.attempts.load(Ordering::SeqCst), 2);
    a.supervisor.shutdown().await;
    assert!(!a.supervisor.failed());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn redis_node_that_cannot_open_the_volume_stops_being_a_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let namespace = namespace("broken");
    let observer = registry(&namespace).await;
    let (best, second) = ranked();
    // The preferred node cannot open the volume; the other one can.
    let broken = Node::start(&best, &namespace, dir.path(), timings()).await;
    broken.opens.broken.store(true, Ordering::SeqCst);
    let healthy = Node::start(&second, &namespace, dir.path(), timings()).await;
    eventually(
        "the healthy node to own",
        Duration::from_secs(10),
        async || {
            published(&observer)
                .await
                .is_some_and(|(node, _)| node == second)
        },
    )
    .await;
    assert!(healthy.owns() && !broken.owns());
    if broken.opens.attempts.load(Ordering::SeqCst) > 0 {
        assert!(
            broken
                .events()
                .iter()
                .any(|event| matches!(event, FailoverEvent::OpenFailed { .. }))
        );
    }
    broken.supervisor.shutdown().await;
    healthy.supervisor.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn redis_fail_back_returns_the_volume_to_the_preferred_node_when_enabled() {
    let (best, second) = ranked();
    for fail_back in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let namespace = namespace(if fail_back { "failback" } else { "stay" });
        let observer = registry(&namespace).await;
        let timings = FailoverTimings {
            fail_back_after: fail_back.then_some(Duration::from_secs(2)),
            ..timings()
        };
        // The less preferred node starts alone and takes the volume.
        let other = Node::start(&second, &namespace, dir.path(), timings).await;
        assert!(other.supervisor.settled(Duration::from_secs(10)).await);
        let first = other.fence().expect("the only node owns the volume");
        let preferred = Node::start(&best, &namespace, dir.path(), timings).await;
        assert!(preferred.supervisor.settled(Duration::from_secs(10)).await);
        if fail_back {
            eventually("the hand-back", Duration::from_secs(15), async || {
                published(&observer)
                    .await
                    .is_some_and(|(node, fence)| node == best && fence > first)
            })
            .await;
            assert!(preferred.owns() && !other.owns());
            assert!(other.events().contains(&FailoverEvent::Released {
                volume: VOLUME.into(),
                fence: first,
                reason: ReleaseReason::FailBack,
            }));
            // The node that handed it back stays a standby.
            tokio::time::sleep(Duration::from_secs(3)).await;
            assert!(preferred.owns() && !other.owns());
            assert_eq!(other.name, second);
        } else {
            // Well past the two seconds a hand-back would have needed.
            tokio::time::sleep(Duration::from_secs(5)).await;
            assert_eq!(published(&observer).await, Some((second.clone(), first)));
            assert!(other.owns() && !preferred.owns());
            assert_eq!(preferred.opens.attempts.load(Ordering::SeqCst), 0);
        }
        other.supervisor.shutdown().await;
        preferred.supervisor.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
async fn redis_three_candidates_never_serve_one_volume_twice_across_repeated_stops() {
    let dir = tempfile::tempdir().unwrap();
    let namespace = namespace("three");
    let observer = registry(&namespace).await;
    let names = ["node-a", "node-b", "node-c"];
    let mut nodes = Vec::new();
    for name in names {
        nodes.push(Some(
            Node::start(name, &namespace, dir.path(), timings()).await,
        ));
    }
    let mut last_fence = 0;
    for round in 0..3 {
        eventually("an owner", Duration::from_secs(15), async || {
            published(&observer).await.is_some()
        })
        .await;
        let (owner, fence) = published(&observer).await.unwrap();
        assert!(fence > last_fence, "round {round}: {fence} > {last_fence}");
        last_fence = fence;
        // The lease admits one holder: at most one node serves the volume.
        for _ in 0..20 {
            let serving: Vec<&str> = nodes
                .iter()
                .flatten()
                .filter(|node| node.owns())
                .map(|node| node.name.as_str())
                .collect();
            assert!(serving.len() <= 1, "{serving:?}");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let index = names.iter().position(|name| *name == owner).unwrap();
        let node = nodes[index].take().expect("the owner is still running");
        node.supervisor.shutdown().await;
    }
    assert_eq!(published(&observer).await, None, "no candidate is left");
}

/// A standby whose owner has no published route keeps trying the lease
/// while the owner keeps writing. Each try must find the lease held and
/// nothing else: any other error makes the standby back off and stop
/// announcing itself, as if it could not reach the storage at all.
///
/// Unix only: the blocks are a local directory, which other platforms
/// refuse with ENOTSUP because its durability is unverified there.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opening_a_volume_its_owner_is_writing_to_only_finds_the_lease_held() {
    let dir = tempfile::tempdir().unwrap();
    let open = |node: &str| factory(dir.path().to_owned(), node.to_owned(), Arc::default());
    let (attempts, written) = open_against_a_writing_owner(open, Duration::from_secs(5)).await;
    assert!(written > 50, "the owner only wrote {written} files");
    assert!(attempts > 20, "the standby only tried {attempts} times");
}

/// The same with the lease in TiDB, where a standby's attempt is a new
/// connection, the schema statements, and a locking read of the row the
/// owner publishes through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires a disposable TiDB at MOUNT_RS_TIDB_URL"]
async fn opening_a_tidb_volume_its_owner_is_writing_to_only_finds_the_lease_held() {
    let connection = std::env::var("MOUNT_RS_TIDB_URL").expect("TiDB fixture URL");
    let dir = tempfile::tempdir().unwrap();
    let volume_key = namespace("contention");
    let open = |node: &str| -> Arc<dyn DriveFactory> {
        let options = SplitOptions {
            metadata: StoreConfig::Tidb {
                connection: connection.clone(),
                volume_key: volume_key.clone(),
                durable: true,
            },
            blocks: StoreConfig::Local {
                path: dir.path().to_owned(),
            },
            ..SplitOptions::memory(node.to_owned(), 4096)
        };
        // Long enough that a renewal on a loaded TiDB is never what fails.
        let runtime = SplitRuntimeOptions {
            lease_ttl: Duration::from_secs(60),
            read_lease_cache_window: Duration::from_secs(15),
            ..Default::default()
        };
        Arc::new(move || {
            let options = options.clone();
            async move {
                Filesystem::split_with_runtime(options, runtime)
                    .await
                    .map(Arc::new)
            }
        })
    };
    let window = Duration::from_secs(30);
    let (attempts, written) = open_against_a_writing_owner(open, window).await;
    println!(
        "TIDB_STANDBY_OPEN_CONTENTION attempts={attempts} owner_writes={written} \
         window_seconds={}",
        window.as_secs()
    );
    assert!(written > 50, "the owner only wrote {written} files");
    assert!(attempts > 20, "the standby only tried {attempts} times");
}

/// Open a volume as `node-a` and keep writing to it while `node-b` tries
/// to open it for `window`; then check the files as a successor. Returns
/// how often the standby tried and how many files the owner wrote.
async fn open_against_a_writing_owner(
    open: impl Fn(&str) -> Arc<dyn DriveFactory>,
    window: Duration,
) -> (usize, u32) {
    let owner = open("node-a").open().await.unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (owner, stop) = (owner.clone(), stop.clone());
        tokio::spawn(async move {
            let mut written = 0u32;
            while !stop.load(Ordering::SeqCst) {
                let body = vec![written as u8; 20_000];
                owner
                    .driver()
                    .write_file(&format!("/file-{}", written % 8), &body)
                    .await
                    .unwrap();
                written += 1;
            }
            written
        })
    };
    let standby = open("node-b");
    let mut outcomes = std::collections::BTreeMap::<String, usize>::new();
    let started = Instant::now();
    while started.elapsed() < window {
        let outcome = match standby.open().await {
            Ok(_) => "opened".to_owned(),
            Err(error) if error.code == mount_rs_core::ErrorCode::Eagain => "EAGAIN".to_owned(),
            Err(error) => format!("{error:?}"),
        };
        *outcomes.entry(outcome).or_default() += 1;
    }
    stop.store(true, Ordering::SeqCst);
    let written = writer.await.unwrap();
    assert_eq!(
        outcomes.keys().collect::<Vec<_>>(),
        ["EAGAIN"],
        "{outcomes:#?} while the owner wrote {written} files"
    );
    owner.shutdown().await.unwrap();
    // The standby's attempts left the owner's files as the owner wrote
    // them: the next holder of the lease reads the last write of each.
    let successor = standby.open().await.unwrap();
    for last in written.saturating_sub(8)..written {
        let path = format!("/file-{}", last % 8);
        let handle = successor.driver().open(&path, "r", 0).await.unwrap();
        let mut read = vec![0u8; 20_001];
        let count = handle.read(&mut read, Some(0)).await.unwrap();
        handle.close().await.unwrap();
        read.truncate(count);
        assert!(read == vec![last as u8; 20_000], "file {}", last % 8);
    }
    successor.shutdown().await.unwrap();
    (outcomes["EAGAIN"], written)
}
