//! Fresh authorization controls for the private immutable grant candidate index.
use async_trait::async_trait;
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_remote_protocol::{Operation, OperationName};
use mount_rs_service::{
    catalog::{
        CatalogError, CatalogSnapshot, CatalogStore, DriveDefinition, GrantDefinition,
        PartitionDefinition, Permission, SqliteCatalog,
    },
    dispatch::{DriveDispatcher, SessionIdentity},
};
use serde_json::json;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

struct CurrentCatalog {
    snapshot: Mutex<Option<Arc<CatalogSnapshot>>>,
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
        self.snapshot
            .lock()
            .unwrap()
            .clone()
            .ok_or(CatalogError::Invalid("controlled unavailable catalog"))
    }
    async fn compare_and_swap(
        &self,
        expected: u64,
        mut next: CatalogSnapshot,
    ) -> Result<u64, CatalogError> {
        let mut current = self.snapshot.lock().unwrap();
        if current
            .as_ref()
            .is_none_or(|snapshot| snapshot.revision != expected)
        {
            return Err(CatalogError::Conflict);
        }
        next.revision = expected
            .checked_add(1)
            .ok_or(CatalogError::Invalid("controlled revision overflow"))?;
        let revision = next.revision;
        *current = Some(Arc::new(next));
        Ok(revision)
    }
}
impl CurrentCatalog {
    fn replace(&self, next: CatalogSnapshot) {
        *self.snapshot.lock().unwrap() = Some(Arc::new(next));
    }
}

fn authority() -> CatalogSnapshot {
    let mut source = CatalogSnapshot::empty();
    source.revision = 1;
    source.issuer_policies.insert(
        "policy".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    source.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: ["data", "other"]
                .into_iter()
                .map(|id| {
                    (
                        id.into(),
                        DriveDefinition {
                            driver: json!({"kind":"memory"}),
                        },
                    )
                })
                .collect(),
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
        claims: json!({"sub":"worker","aud":"mount-rs"}),
        expires_at: i64::MAX,
    }
}
fn stat() -> Operation {
    Operation {
        name: OperationName::Stat,
        body: json!({"path":"/"}),
    }
}
fn dispatcher(catalog: Arc<dyn CatalogStore>) -> DriveDispatcher {
    let mut dispatcher = DriveDispatcher::new(catalog);
    dispatcher
        .register_definition(
            "red",
            "data",
            json!({"kind":"memory"}),
            Arc::new(MemoryFs::new(MemoryOptions::default())),
        )
        .unwrap();
    dispatcher
}

#[tokio::test]
async fn warmed_index_observes_same_revision_grants_claims_policy_and_definition() {
    let source = authority();
    source.validate().unwrap();
    let catalog = Arc::new(CurrentCatalog {
        snapshot: Mutex::new(Some(Arc::new(source.clone()))),
        loads: AtomicUsize::new(0),
    });
    let dispatcher = dispatcher(catalog.clone());
    for change in 0..5 {
        catalog.replace(source.clone());
        assert!(
            dispatcher
                .dispatch(&identity(), "data", &stat())
                .await
                .is_ok()
        );
        let mut next = source.clone();
        match change {
            0 => next.grants.clear(),
            1 => {
                next.grants
                    .get_mut("workload")
                    .unwrap()
                    .claim_conditions
                    .insert("/sub".into(), "another".into());
            }
            2 => {
                next.issuer_policies.insert(
                    "policy".into(),
                    json!({"issuer":"https://issuer.example.com","audiences":["different"]}),
                );
            }
            3 => {
                next.grants.get_mut("workload").unwrap().drives.clear();
            }
            4 => {
                next.partitions
                    .get_mut("red")
                    .unwrap()
                    .drives
                    .get_mut("data")
                    .unwrap()
                    .driver = json!({"kind":"changed"});
            }
            _ => unreachable!(),
        }
        assert_eq!(next.revision, source.revision);
        catalog.replace(next);
        assert_eq!(
            dispatcher
                .dispatch(&identity(), "data", &stat())
                .await
                .unwrap_err()
                .code,
            if change == 4 { "ESTALE" } else { "EACCES" }
        );
    }
    assert_eq!(
        catalog.loads.load(Ordering::SeqCst),
        10,
        "each request must still obtain current authority"
    );
}

#[tokio::test]
async fn warmed_index_does_not_bypass_permission_reduction_or_expiration_or_load_error() {
    let catalog = Arc::new(CurrentCatalog {
        snapshot: Mutex::new(Some(Arc::new(authority()))),
        loads: AtomicUsize::new(0),
    });
    let dispatcher = dispatcher(catalog.clone());
    assert!(
        dispatcher
            .dispatch(&identity(), "data", &stat())
            .await
            .is_ok()
    );
    let mut read_only = authority();
    read_only
        .grants
        .get_mut("workload")
        .unwrap()
        .drives
        .insert("data".into(), Permission::Read);
    catalog.replace(read_only);
    let mkdir = Operation {
        name: OperationName::Mkdir,
        body: json!({"path":"/new","mode":493}),
    };
    assert_eq!(
        dispatcher
            .dispatch(&identity(), "data", &mkdir)
            .await
            .unwrap_err()
            .code,
        "EACCES"
    );
    assert!(
        dispatcher
            .dispatch(&identity(), "data", &stat())
            .await
            .is_ok()
    );
    *catalog.snapshot.lock().unwrap() = None;
    assert_eq!(
        dispatcher
            .dispatch(&identity(), "data", &stat())
            .await
            .unwrap_err()
            .code,
        "EACCES"
    );
    catalog.replace(authority());
    let before = catalog.loads.load(Ordering::SeqCst);
    let mut expired = identity();
    expired.expires_at = 0;
    assert_eq!(
        dispatcher
            .dispatch(&expired, "data", &stat())
            .await
            .unwrap_err()
            .code,
        "EACCES"
    );
    assert_eq!(catalog.loads.load(Ordering::SeqCst), before);
}

#[tokio::test]
async fn renewal_checks_unmatched_grants_for_other_drives_in_same_scope() {
    let catalog = Arc::new(CurrentCatalog {
        snapshot: Mutex::new(Some(Arc::new(authority()))),
        loads: AtomicUsize::new(0),
    });
    let dispatcher = dispatcher(catalog.clone());
    let mut current = identity();
    current.claims["repository"] = json!("currently-unmatched");
    let mut next = current.clone();
    next.claims["repository"] = json!("new-match");
    assert!(dispatcher.dispatch(&current, "data", &stat()).await.is_ok());
    assert!(dispatcher.renewal_matches(&current, &next).await);
    let mut replacement = authority();
    replacement.grants.insert(
        "currently-unmatched-other-drive".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "policy".into(),
            drives: BTreeMap::from([("other".into(), Permission::Read)]),
            claim_conditions: BTreeMap::from([("/repository".into(), "new-match".into())]),
        },
    );
    catalog.replace(replacement);
    assert!(!dispatcher.renewal_matches(&current, &next).await);
    assert!(dispatcher.renewal_matches(&current, &current).await);
}

#[tokio::test]
async fn sqlite_same_revision_external_revocation_invalidates_warmed_dispatch_index() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = Arc::new(SqliteCatalog::open(&path).await.unwrap());
    assert_eq!(catalog.compare_and_swap(0, authority()).await.unwrap(), 1);
    let dispatcher = dispatcher(catalog);
    assert!(
        dispatcher
            .dispatch(&identity(), "data", &stat())
            .await
            .is_ok()
    );
    let mut revoked = authority();
    revoked.grants.clear();
    revoked.validate().unwrap();
    let connection = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE service_catalog SET document=?1 WHERE singleton=1 AND revision=1",
                rusqlite::params![serde_json::to_vec(&revoked).unwrap()]
            )
            .unwrap(),
        1
    );
    assert_eq!(
        dispatcher
            .dispatch(&identity(), "data", &stat())
            .await
            .unwrap_err()
            .code,
        "EACCES"
    );
}

struct OwnedCatalog(CatalogSnapshot);
#[async_trait]
impl CatalogStore for OwnedCatalog {
    async fn load_current(&self) -> Result<CatalogSnapshot, CatalogError> {
        Ok(self.0.clone())
    }
    // Deliberately use the real trait default, which returns a fresh Arc per load.
    async fn compare_and_swap(&self, _: u64, _: CatalogSnapshot) -> Result<u64, CatalogError> {
        Err(CatalogError::Conflict)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn default_fresh_arc_store_remains_usable_under_concurrent_dispatch() {
    let dispatcher = Arc::new(dispatcher(Arc::new(OwnedCatalog(authority()))));
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..32 {
        let dispatcher = Arc::clone(&dispatcher);
        tasks.spawn(async move {
            for _ in 0..16 {
                assert!(
                    dispatcher
                        .dispatch(&identity(), "data", &stat())
                        .await
                        .is_ok()
                );
            }
        });
    }
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while let Some(result) = tasks.join_next().await {
            result.unwrap();
        }
    })
    .await
    .expect("fresh-Arc catalog queued repeated index contention retries");
}

#[tokio::test]
async fn default_fresh_arc_renewal_preserves_partition_policy_scope() {
    let mut current = identity();
    current.claims["repository"] = json!("currently-unmatched");
    let mut next = current.clone();
    next.claims["repository"] = json!("new-match");
    for (partition, policy, renewal_allowed) in [
        ("blue", "policy", true),
        ("red", "different", true),
        ("red", "policy", false),
    ] {
        let mut source = authority();
        source
            .partitions
            .insert("blue".into(), source.partitions["red"].clone());
        source.issuer_policies.insert(
            "different".into(),
            json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
        );
        source.grants.insert(
            "repository-condition".into(),
            GrantDefinition {
                partition_id: partition.into(),
                policy_id: policy.into(),
                drives: BTreeMap::from([("other".into(), Permission::Read)]),
                claim_conditions: BTreeMap::from([("/repository".into(), "new-match".into())]),
            },
        );
        source.validate().unwrap();
        let catalog = Arc::new(OwnedCatalog(source));
        let first = catalog.load_shared_current().await.unwrap();
        let second = catalog.load_shared_current().await.unwrap();
        assert!(
            !Arc::ptr_eq(&first, &second),
            "real trait default must return fresh Arcs"
        );
        assert_eq!(first.revision, second.revision);
        assert_eq!(Arc::strong_count(&first), 1);
        assert_eq!(Arc::strong_count(&second), 1);
        let dispatcher = dispatcher(catalog);
        assert!(dispatcher.dispatch(&current, "data", &stat()).await.is_ok());
        assert_eq!(
            dispatcher.renewal_matches(&current, &next).await,
            renewal_allowed,
            "incorrect renewal scope for {partition}/{policy}",
        );
        assert!(dispatcher.renewal_matches(&current, &current).await);
    }
}
