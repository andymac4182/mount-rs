use std::{collections::BTreeMap, sync::Arc};

use mount_rs_core::FsDriver;
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_remote_protocol::{Operation, OperationName};
use mount_rs_service::{
    catalog::{
        CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
        SqliteCatalog,
    },
    dispatch::{DriveDispatcher, SessionHandles, SessionIdentity},
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
        signing_algorithm: "ES256".into(),
        claims: json!({"repository_id":"repo-1","aud":"mount-rs"}),
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
    let mut writable = catalog.load_current().await.unwrap();
    writable
        .grants
        .get_mut("reader")
        .unwrap()
        .drives
        .insert("data".into(), Permission::Write);
    catalog.compare_and_swap(1, writable).await.unwrap();
    let handles = SessionHandles::default();
    let open = Operation {
        name: OperationName::Open,
        body: json!({"path":"/file","flags":"w+","mode":420}),
    };
    let id = dispatcher
        .dispatch_with_handles(&identity, "data", &open, &handles)
        .await
        .unwrap()
        .as_u64()
        .unwrap();
    let write = Operation {
        name: OperationName::HandleWrite,
        body: json!({"handle":id,"data":[104,105],"position":0}),
    };
    assert_eq!(
        dispatcher
            .dispatch_with_handles(&identity, "data", &write, &handles)
            .await
            .unwrap(),
        json!(2)
    );
    let read = Operation {
        name: OperationName::HandleRead,
        body: json!({"handle":id,"length":2,"position":0}),
    };
    assert_eq!(
        dispatcher
            .dispatch_with_handles(&identity, "data", &read, &handles)
            .await
            .unwrap(),
        json!([104, 105])
    );
    let other_connection = SessionHandles::default();
    assert_eq!(
        dispatcher
            .dispatch_with_handles(&identity, "data", &read, &other_connection)
            .await
            .unwrap_err()
            .code,
        "EBADF"
    );
    handles.close_all().await;
    assert_eq!(
        dispatcher
            .dispatch_with_handles(&identity, "data", &read, &handles)
            .await
            .unwrap_err()
            .code,
        "EBADF"
    );
    let mut revoked = catalog.load_current().await.unwrap();
    revoked.grants.clear();
    catalog.compare_and_swap(2, revoked).await.unwrap();
    assert_eq!(
        dispatcher
            .dispatch(&identity, "data", &stat)
            .await
            .unwrap_err()
            .code,
        "EACCES"
    );
}
