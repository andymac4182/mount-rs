use std::collections::BTreeMap;

use mount_rs_service::catalog::{
    CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
    SqliteCatalog,
};
use serde_json::json;

#[tokio::test]
async fn same_drive_id_in_two_partitions_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = SqliteCatalog::open(&path).await.unwrap();
    let mut next = CatalogSnapshot::empty();
    next.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "data".into(),
                DriveDefinition {
                    driver: json!({"kind": "memory"}),
                },
            )]),
        },
    );
    next.partitions.insert(
        "blue".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "data".into(),
                DriveDefinition {
                    driver: json!({"kind": "sqlite", "path": "blue.sqlite"}),
                },
            )]),
        },
    );

    assert_eq!(catalog.compare_and_swap(0, next).await.unwrap(), 1);
    drop(catalog);
    let reopened = SqliteCatalog::open(&path).await.unwrap();
    let loaded = reopened.load_current().await.unwrap();
    assert_eq!(loaded.revision, 1);
    assert_eq!(
        loaded.partitions["red"].drives["data"].driver["kind"],
        "memory"
    );
    assert_eq!(
        loaded.partitions["blue"].drives["data"].driver["kind"],
        "sqlite"
    );
}

#[tokio::test]
async fn compare_and_swap_rejects_stale_revision() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
        .await
        .unwrap();
    let snapshot = CatalogSnapshot::empty();
    assert_eq!(
        catalog.compare_and_swap(0, snapshot.clone()).await.unwrap(),
        1
    );
    assert!(catalog.compare_and_swap(0, snapshot).await.is_err());
    assert_eq!(catalog.load_current().await.unwrap().revision, 1);
}

#[tokio::test]
async fn grant_cannot_target_missing_drive() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
        .await
        .unwrap();
    let mut next = CatalogSnapshot::empty();
    next.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: BTreeMap::new(),
        },
    );
    next.issuer_policies.insert(
        "issuer".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    next.grants.insert(
        "grant".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "issuer".into(),
            drives: BTreeMap::from([("missing".into(), Permission::Read)]),
            claim_conditions: BTreeMap::from([("/sub".into(), "workload".into())]),
        },
    );
    let error = catalog.compare_and_swap(0, next).await.unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid catalog: grant Drive does not exist"
    );
    assert_eq!(catalog.load_current().await.unwrap().revision, 0);
}

#[tokio::test]
async fn catalog_rejects_grant_without_stable_identity_condition() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
        .await
        .unwrap();
    let mut next = CatalogSnapshot::empty();
    next.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "data".into(),
                DriveDefinition {
                    driver: json!({"kind": "memory"}),
                },
            )]),
        },
    );
    next.issuer_policies.insert(
        "issuer".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    next.grants.insert(
        "grant".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "issuer".into(),
            drives: BTreeMap::from([("data".into(), Permission::Read)]),
            claim_conditions: BTreeMap::new(),
        },
    );
    assert!(catalog.compare_and_swap(0, next).await.is_err());
}

#[tokio::test]
async fn catalog_rejects_insecure_issuer_policy() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
        .await
        .unwrap();
    let mut next = CatalogSnapshot::empty();
    next.issuer_policies.insert(
        "unsafe".into(),
        json!({"issuer":"https://127.0.0.1", "audiences":["mount-rs"]}),
    );
    assert!(catalog.compare_and_swap(0, next).await.is_err());
}

#[test]
fn duplicate_catalog_ids_are_rejected() {
    let document = r#"{"revision":0,"partitions":{"red":{"drives":{}},"red":{"drives":{}}},"issuer_policies":{},"grants":{}}"#;
    assert!(serde_json::from_str::<CatalogSnapshot>(document).is_err());
}
#[tokio::test]
async fn partition_must_be_empty_before_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
        .await
        .unwrap();
    let mut next = CatalogSnapshot::empty();
    next.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "data".into(),
                DriveDefinition {
                    driver: json!({"kind":"memory"}),
                },
            )]),
        },
    );
    catalog.compare_and_swap(0, next).await.unwrap();
    assert!(
        catalog
            .compare_and_swap(1, CatalogSnapshot::empty())
            .await
            .is_err()
    );
    let mut emptied = catalog.load_current().await.unwrap();
    emptied.partitions.get_mut("red").unwrap().drives.clear();
    catalog.compare_and_swap(1, emptied).await.unwrap();
    catalog
        .compare_and_swap(2, CatalogSnapshot::empty())
        .await
        .unwrap();
}
