use std::{collections::BTreeMap, sync::Arc};

use mount_rs_core::FsDriver;
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_remote_protocol::{Operation, OperationName};
use mount_rs_service::{
    catalog::{
        CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        SqliteCatalog,
    },
    dispatch::{DriveDispatcher, SessionIdentity},
};
use serde_json::json;

#[tokio::test]
async fn dispatcher_enforces_current_drive_grant_and_partition() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
    );
    let mut snapshot = CatalogSnapshot::empty();
    for partition in ["red", "blue"] {
        snapshot.partitions.insert(
            partition.into(),
            PartitionDefinition {
                drives: BTreeMap::from([(
                    "data".into(),
                    DriveDefinition {
                        driver: json!({"kind":"memory"}),
                    },
                )]),
            },
        );
    }
    snapshot.issuer_policies.insert(
        "oidc".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    snapshot.grants.insert(
        "reader".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "oidc".into(),
            drives: BTreeMap::from([("data".into(), Permission::Read)]),
            claim_conditions: BTreeMap::from([("/repository_id".into(), "repo-1".into())]),
        },
    );
    catalog.compare_and_swap(0, snapshot).await.unwrap();
    let mut dispatcher = DriveDispatcher::new(catalog.clone());
    dispatcher
        .register(
            "red",
            "data",
            Arc::new(MemoryFs::new(MemoryOptions::default())) as Arc<dyn FsDriver>,
        )
        .unwrap();
    dispatcher
        .register(
            "blue",
            "data",
            Arc::new(MemoryFs::new(MemoryOptions::default())) as Arc<dyn FsDriver>,
        )
        .unwrap();
    let identity = SessionIdentity {
        partition_id: "red".into(),
        policy_id: "oidc".into(),
        issuer: "https://issuer.example.com".into(),
        subject: "workload-1".into(),
        claims: json!({"repository_id":"repo-1"}),
        expires_at: i64::MAX,
    };
    let stat = Operation {
        name: OperationName::Stat,
        body: json!({"path":"/"}),
    };
    assert!(dispatcher.dispatch(&identity, "data", &stat).await.is_ok());
    let noncanonical = Operation {
        name: OperationName::Stat,
        body: json!({"path":"/../"}),
    };
    assert_eq!(
        dispatcher
            .dispatch(&identity, "data", &noncanonical)
            .await
            .unwrap_err()
            .code,
        "EINVAL"
    );
    let mkdir = Operation {
        name: OperationName::Mkdir,
        body: json!({"path":"/new","mode":493}),
    };
    assert_eq!(
        dispatcher
            .dispatch(&identity, "data", &mkdir)
            .await
            .unwrap_err()
            .code,
        "EACCES"
    );
    assert_eq!(
        dispatcher
            .dispatch(&identity, "blue/data", &stat)
            .await
            .unwrap_err()
            .code,
        "EACCES"
    );
    let mut revoked = catalog.load_current().await.unwrap();
    revoked.grants.clear();
    catalog.compare_and_swap(1, revoked).await.unwrap();
    assert_eq!(
        dispatcher
            .dispatch(&identity, "data", &stat)
            .await
            .unwrap_err()
            .code,
        "EACCES"
    );
}
