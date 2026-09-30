//! Actual cache-lock contention must preserve fresh authorization after waits.
use super::*;
use crate::{
    catalog::{
        CatalogError, CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition,
    },
    request_metadata::{PreparationTestPoint, set_preparation_hook},
};
use async_trait::async_trait;
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use std::{
    sync::{
        Mutex as StdMutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

struct CurrentCatalog {
    snapshot: StdMutex<Arc<CatalogSnapshot>>,
    loads: AtomicUsize,
}
#[async_trait]
impl CatalogStore for CurrentCatalog {
    async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
        self.load_shared_current()
            .await
            .map(|snapshot| (*snapshot).clone())
    }
    async fn load_shared_current(&self) -> Result<Arc<CatalogSnapshot>, CatalogError> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        Ok(Arc::clone(&self.snapshot.lock().unwrap()))
    }
    async fn compare_and_swap(&self, _: u64, _: CatalogSnapshot) -> Result<u64, CatalogError> {
        Err(CatalogError::Conflict)
    }
}

fn authority() -> CatalogSnapshot {
    let mut source = CatalogSnapshot::empty();
    source.revision = 1;
    source.issuer_policies.insert(
        "policy".into(),
        serde_json::json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    source.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "data".into(),
                DriveDefinition {
                    driver: serde_json::json!({"kind":"memory"}),
                },
            )]),
        },
    );
    source.grants.insert(
        "workload".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "policy".into(),
            drives: BTreeMap::from([("data".into(), Permission::Write)]),
            claim_conditions: BTreeMap::from([("/sub".into(), "worker".into())]),
        },
    );
    source
}
fn identity() -> SessionIdentity {
    SessionIdentity {
        partition_id: "red".into(),
        policy_id: "policy".into(),
        issuer: "https://issuer.example.com".into(),
        subject: "worker".into(),
        signing_algorithm: "ES256".into(),
        claims: serde_json::json!({"sub":"worker","aud":"mount-rs"}),
        expires_at: i64::MAX,
    }
}
fn stat() -> Operation {
    Operation {
        name: OperationName::Stat,
        body: serde_json::json!({"path":"/"}),
    }
}

type BuildControl = (
    std::thread::JoinHandle<Result<Value, WireError>>,
    mpsc::Sender<()>,
);
fn held_cold_build(dispatcher: Arc<DriveDispatcher>) -> BuildControl {
    let (building, entered_build) = mpsc::channel();
    let (release_build, released_build) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let mut held = false;
        set_preparation_hook(Box::new(move |point| {
            if !held && point == PreparationTestPoint::BeforeBuild {
                held = true;
                building.send(()).unwrap();
                released_build
                    .recv_timeout(Duration::from_secs(10))
                    .unwrap();
            }
        }));
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(dispatcher.dispatch(&identity(), "data", &stat()))
    });
    entered_build.recv_timeout(Duration::from_secs(10)).unwrap();
    (holder, release_build)
}

#[test]
fn index_lock_wait_rechecks_expiration_revocation_and_policy_before_dispatch() {
    for change in 0..3 {
        let catalog = Arc::new(CurrentCatalog {
            snapshot: StdMutex::new(Arc::new(authority())),
            loads: AtomicUsize::new(0),
        });
        let mut dispatcher = DriveDispatcher::new(catalog.clone());
        dispatcher
            .register(
                "red",
                "data",
                Arc::new(MemoryFs::new(MemoryOptions::default())),
            )
            .unwrap();
        let dispatcher = Arc::new(dispatcher);
        let (holder, release_build) = held_cold_build(Arc::clone(&dispatcher));

        let (waiting, entered_wait) = mpsc::channel();
        let waiter_dispatcher = Arc::clone(&dispatcher);
        let mut waiter_identity = identity();
        if change == 2 {
            waiter_identity.expires_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs() as i64
                + 2;
        }
        let expiration = waiter_identity.expires_at;
        let waiter = std::thread::spawn(move || {
            let mut waiting = Some(waiting);
            set_preparation_hook(Box::new(move |point| {
                if point == PreparationTestPoint::ContendedLock
                    && let Some(waiting) = waiting.take()
                {
                    waiting.send(()).unwrap();
                }
            }));
            tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap()
                .block_on(waiter_dispatcher.dispatch(&waiter_identity, "data", &stat()))
        });
        entered_wait.recv_timeout(Duration::from_secs(10)).unwrap();
        // The receipt proves the waiter tried an actual held cache lock after its
        // initial policy/expiration checks. No timing guess substitutes for it.
        if change == 2 {
            while SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs()
                < expiration as u64
            {
                std::thread::sleep(Duration::from_millis(10));
            }
        } else {
            let mut changed = authority();
            if change == 0 {
                changed.grants.clear();
            } else {
                changed.issuer_policies.insert("policy".into(), serde_json::json!({"issuer":"https://issuer.example.com","audiences":["different"]}));
            }
            *catalog.snapshot.lock().unwrap() = Arc::new(changed);
        }
        release_build.send(()).unwrap();
        assert!(holder.join().unwrap().is_ok());
        assert_eq!(
            waiter.join().unwrap().unwrap_err().code,
            "EACCES",
            "cache waiter used expired or revoked earlier authority"
        );
        if change != 2 {
            assert!(
                catalog.loads.load(Ordering::SeqCst) >= 3,
                "contended authorization must reload current authority"
            );
        }
    }
}

#[test]
fn renewal_index_lock_wait_reloads_new_scoped_claim_conditions() {
    let catalog = Arc::new(CurrentCatalog {
        snapshot: StdMutex::new(Arc::new(authority())),
        loads: AtomicUsize::new(0),
    });
    let mut dispatcher = DriveDispatcher::new(catalog.clone());
    dispatcher
        .register(
            "red",
            "data",
            Arc::new(MemoryFs::new(MemoryOptions::default())),
        )
        .unwrap();
    let dispatcher = Arc::new(dispatcher);
    let (holder, release_build) = held_cold_build(Arc::clone(&dispatcher));
    let mut current = identity();
    current.claims["repository"] = serde_json::json!("currently-unmatched");
    let mut next = current.clone();
    next.claims["repository"] = serde_json::json!("new-match");
    let (waiting, entered_wait) = mpsc::channel();
    let waiter_dispatcher = Arc::clone(&dispatcher);
    let waiter = std::thread::spawn(move || {
        let mut waiting = Some(waiting);
        set_preparation_hook(Box::new(move |point| {
            if point == PreparationTestPoint::ContendedLock
                && let Some(waiting) = waiting.take()
            {
                waiting.send(()).unwrap();
            }
        }));
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(waiter_dispatcher.renewal_matches(&current, &next))
    });
    entered_wait.recv_timeout(Duration::from_secs(10)).unwrap();
    let mut changed = authority();
    changed.grants.insert(
        "new-scoped-condition".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "policy".into(),
            drives: BTreeMap::from([("data".into(), Permission::Read)]),
            claim_conditions: BTreeMap::from([("/repository".into(), "new-match".into())]),
        },
    );
    *catalog.snapshot.lock().unwrap() = Arc::new(changed);
    release_build.send(()).unwrap();
    assert!(holder.join().unwrap().is_ok());
    assert!(
        !waiter.join().unwrap(),
        "renewal used old scoped claim pointers after waiting"
    );
    assert!(catalog.loads.load(Ordering::SeqCst) >= 3);
}
