#![cfg(unix)]

use mount_rs_service::catalog::{
    CatalogSnapshot, DriveDefinition, PartitionDefinition, SqliteCatalog,
};
use rusqlite::{Connection, params};
use serde_json::json;
use std::{collections::BTreeMap, sync::Arc};

fn snapshot(marker: &str, revision: u64) -> CatalogSnapshot {
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.revision = revision;
    snapshot.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "data".into(),
                DriveDefinition {
                    driver: json!({"kind":"memory", "marker":marker}),
                },
            )]),
        },
    );
    snapshot
}

fn external_document(connection: &Connection, snapshot: &CatalogSnapshot) {
    connection
        .execute(
            "UPDATE service_catalog SET document=?1 WHERE singleton=1",
            params![serde_json::to_vec(snapshot).unwrap()],
        )
        .unwrap();
}

#[tokio::test]
async fn public_catalog_handles_independent_same_revision_changes_and_repair() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = SqliteCatalog::open(&path).await.unwrap();
    catalog.compare_and_swap(0, snapshot("A", 0)).await.unwrap();
    let first = catalog.load_shared_current().await.unwrap();
    let outsider = Connection::open(&path).unwrap();
    let before = catalog.read_diagnostics().slot_observations;
    external_document(&outsider, &snapshot("B", 1));
    for _ in 0..8 {
        let loaded = catalog.load_shared_current().await.unwrap();
        assert_eq!(
            loaded.partitions["red"].drives["data"].driver["marker"],
            "B"
        );
        assert!(!Arc::ptr_eq(&loaded, &first));
    }
    if catalog.read_diagnostics().enabled {
        let after = catalog.read_diagnostics().slot_observations;
        assert!(after.iter().zip(before).all(|(a, b)| *a > b));
    }
    outsider
        .execute(
            "UPDATE service_catalog SET document=x'010203' WHERE singleton=1",
            [],
        )
        .unwrap();
    for _ in 0..8 {
        assert!(catalog.load_shared_current().await.is_err());
    }
    external_document(&outsider, &snapshot("A", 1));
    for _ in 0..8 {
        let repaired = catalog.load_shared_current().await.unwrap();
        assert_eq!(
            repaired.partitions["red"].drives["data"].driver["marker"],
            "A"
        );
    }
}
