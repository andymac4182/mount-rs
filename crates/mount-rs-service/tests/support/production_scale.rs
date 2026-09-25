//! Production target qualification; separate from the historical saturation control.
use mount_rs_service::catalog::{
    CatalogSnapshot, DriveDefinition, GrantDefinition, PartitionDefinition, Permission,
};
use serde_json::json;
use std::collections::BTreeMap;

fn target_catalog(clients: usize) -> CatalogSnapshot {
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.issuer_policies.insert(
        "load-policy".into(),
        json!({"issuer":"https://load.example.com","audiences":["mount-rs"]}),
    );
    for client in 0..clients {
        let partition = format!("partition-{}", client / 2);
        let drive = format!("sandbox-{client}");
        snapshot
            .partitions
            .entry(partition.clone())
            .or_insert_with(|| PartitionDefinition {
                drives: BTreeMap::new(),
            })
            .drives
            .insert(
                drive.clone(),
                DriveDefinition {
                    driver: json!({"kind":"tidb-test","sandbox":client}),
                },
            );
        snapshot.grants.insert(
            drive.clone(),
            GrantDefinition {
                partition_id: partition,
                policy_id: "load-policy".into(),
                drives: BTreeMap::from([(drive, Permission::Write)]),
                claim_conditions: BTreeMap::from([("/sandbox_id".into(), client.to_string())]),
            },
        );
    }
    snapshot
}

#[test]
fn production_target_catalog_is_bounded_and_exactly_scoped() {
    let snapshot = target_catalog(10_000);
    assert_eq!(snapshot.partitions.len(), 5_000);
    assert_eq!(snapshot.grants.len(), 10_000);
    assert!(
        snapshot
            .partitions
            .values()
            .all(|partition| partition.drives.len() == 2)
    );
    let encoded = serde_json::to_vec(&snapshot).unwrap();
    assert!(encoded.len() < 8 * 1024 * 1024);
    assert!(
        snapshot.validate().is_ok(),
        "production target rejected: {:?}",
        snapshot.validate()
    );
}

#[test]
fn target_grants_deny_sibling_and_other_partition() {
    use mount_rs_service::auth::authorize_drive;
    let snapshot = target_catalog(10);
    for client in 0..10 {
        let partition = format!("partition-{}", client / 2);
        let own = format!("sandbox-{client}");
        let sibling = format!("sandbox-{}", client ^ 1);
        let other = format!("sandbox-{}", (client + 2) % 10);
        let other_partition = format!("partition-{}", ((client + 2) % 10) / 2);
        let claims = json!({"sandbox_id":client.to_string()});
        assert_eq!(
            authorize_drive(&snapshot, "load-policy", &claims, &partition, &own),
            Some(Permission::Write)
        );
        assert_eq!(
            authorize_drive(&snapshot, "load-policy", &claims, &partition, &sibling),
            None
        );
        assert_eq!(
            authorize_drive(&snapshot, "load-policy", &claims, &other_partition, &other),
            None
        );
    }
}

#[test]
fn production_target_count_caps_still_reject_overflow() {
    let mut partitions = target_catalog(10_000);
    partitions.partitions.insert(
        "overflow".into(),
        PartitionDefinition {
            drives: BTreeMap::new(),
        },
    );
    assert!(partitions.validate().is_err());
    let mut drives = target_catalog(10_000);
    drives
        .partitions
        .get_mut("partition-0")
        .unwrap()
        .drives
        .insert(
            "overflow".into(),
            DriveDefinition {
                driver: json!({"kind":"tidb-test"}),
            },
        );
    assert!(drives.validate().is_err());
    let mut grants = target_catalog(10_000);
    grants
        .grants
        .insert("overflow".into(), grants.grants["sandbox-0"].clone());
    assert!(grants.validate().is_err());
}
