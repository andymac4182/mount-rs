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

/// Catalog shape/query-amplification stage; no filesystem or connection-capacity claim.
#[tokio::test]
#[ignore = "serialized controlled catalog profile; retain configured output"]
async fn catalog_shape_and_authorization_profile() {
    use mount_rs_service::catalog::SqliteCatalog;
    use std::time::Instant;
    let directory = tempfile::tempdir().unwrap();
    let mut rows = Vec::new();
    for clients in [10, 100, 1_000, 10_000] {
        let catalog =
            SqliteCatalog::open(directory.path().join(format!("catalog-{clients}.sqlite")))
                .await
                .unwrap();
        let snapshot = target_catalog(clients);
        let document_bytes = serde_json::to_vec(&snapshot).unwrap().len();
        catalog.compare_and_swap(0, snapshot).await.unwrap();
        let warm = catalog.load_shared_current().await.unwrap();
        let claims = json!({"sandbox_id":(clients - 1).to_string()});
        let partition = format!("partition-{}", (clients - 1) / 2);
        let drive = format!("sandbox-{}", clients - 1);
        let before = mount_rs_core::diagnostics::profile::snapshot();
        #[cfg(all(feature = "resource-profiling", unix))]
        let resources_before = super::resource_profile::Snapshot::capture_process().unwrap();
        let started = Instant::now();
        for _ in 0..40 {
            let current = catalog.load_shared_current().await.unwrap();
            assert!(std::sync::Arc::ptr_eq(&warm, &current));
            assert_eq!(
                mount_rs_service::auth::authorize_drive(
                    &current,
                    "load-policy",
                    &claims,
                    &partition,
                    &drive
                ),
                Some(Permission::Write)
            );
        }
        let elapsed = started.elapsed();
        let profile = mount_rs_core::diagnostics::profile::snapshot()
            .delta(&before)
            .expect("profile counter invalid");
        let query = profile
            .entries
            .iter()
            .find(|entry| entry.name == "catalog.query_document_bytes")
            .expect("enable MOUNT_RS_PROFILE_IO=1 for actual catalog counters");
        assert_eq!(query.calls, 40);
        assert_eq!(query.units, document_bytes as u64 * 40);
        #[cfg(all(feature = "resource-profiling", unix))]
        let resources = super::resource_profile::Snapshot::capture_process()
            .unwrap()
            .delta(&resources_before)
            .unwrap();
        #[cfg(not(all(feature = "resource-profiling", unix)))]
        let resources = serde_json::Value::Null;
        rows.push(json!({"clients":clients,"partitions":clients/2,"drives":clients,"grants":clients,"queries":40,"document_bytes":document_bytes,"elapsed_us":elapsed.as_micros(),"profile":profile,"resources":resources}));
    }
    let artifact = json!({"schema":"mount-rs-catalog-shape-profile-v1","source_revision":std::env::var("MOUNT_RS_PRODUCTION_SOURCE_REVISION").expect("source revision required"),"scope":"one process, minimal fixture descriptors, authoritative SQLite catalog calls plus exact authorization; no network/filesystem/connection capacity claim; instrumentation affects throughput","allocation_profile":cfg!(feature="allocation-profiling"),"rows":rows});
    let output = std::env::var("MOUNT_RS_PRODUCTION_CATALOG_PROFILE_OUTPUT")
        .expect("retained output required");
    std::fs::write(output, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
}

#[test]
fn requested_mixed_profile_and_compact_ledger_account_for_every_file() {
    let profile = FileProfile::Mixed;
    assert_eq!(
        (0..1_000).map(|file| profile.size(file)).sum::<usize>(),
        6_283_264
    );
    assert_eq!(
        (0..1_000)
            .filter(|file| profile.size(*file) == 4_096)
            .count(),
        990
    );
    assert_eq!(
        (0..1_000)
            .filter(|file| profile.size(*file) == 131_072)
            .count(),
        9
    );
    assert_eq!(profile.size(999), 1_048_576);
    let mut ledger = GenerationLedger::new(profile, 1_000);
    assert_eq!(ledger.generations.len(), 1_534);
    assert_eq!(ledger.bytes(), 1_534 * 8);
    ledger.commit(999, 255, 42);
    assert_eq!(ledger.generation(999, 255), 42);
    assert_eq!(ledger.generation(999, 254), 0);
    assert_eq!(ledger.generation(0, 0), 0);
}

#[test]
fn unique_file_oracle_rejects_every_changed_byte_and_wrong_identity() {
    let expected = oracle_block(2, 4, 999, 255, 9);
    for index in 0..expected.len() {
        let mut corrupt = expected;
        corrupt[index] ^= 1;
        assert!(!verify_block(&corrupt, 2, 4, 999, 255, 9));
    }
    for actual in [
        oracle_block(3, 4, 999, 255, 9),
        oracle_block(2, 5, 999, 255, 9),
        oracle_block(2, 4, 998, 255, 9),
        oracle_block(2, 4, 999, 254, 9),
        oracle_block(2, 4, 999, 255, 10),
    ] {
        assert!(!verify_block(&actual, 2, 4, 999, 255, 9));
    }
    assert!(verify_block(&expected, 2, 4, 999, 255, 9));
}

#[derive(Clone, Copy)]
enum FileProfile {
    Mixed,
}
impl FileProfile {
    fn size(self, file: usize) -> usize {
        assert!(file < 1_000, "file outside bounded profile");
        match file {
            0..990 => 4_096,
            990..999 => 131_072,
            _ => 1_048_576,
        }
    }
}
struct GenerationLedger {
    profile: FileProfile,
    generations: Vec<u64>,
}
impl GenerationLedger {
    fn new(profile: FileProfile, files: usize) -> Self {
        Self {
            profile,
            generations: vec![0; (0..files).map(|file| profile.size(file) / 4096).sum()],
        }
    }
    fn index(&self, file: usize, block: usize) -> usize {
        assert!(
            block < self.profile.size(file) / 4_096,
            "block outside file"
        );
        let start = match file {
            0..990 => file,
            990..999 => 990 + (file - 990) * 32,
            _ => 1_278,
        };
        start + block
    }
    fn generation(&self, file: usize, block: usize) -> u64 {
        self.generations[self.index(file, block)]
    }
    fn commit(&mut self, file: usize, block: usize, generation: u64) {
        let index = self.index(file, block);
        self.generations[index] = generation;
    }
    fn bytes(&self) -> usize {
        self.generations.len() * std::mem::size_of::<u64>()
    }
}
fn oracle_block(partition: u64, drive: u64, file: u64, block: u64, generation: u64) -> [u8; 4096] {
    let mut data = [0; 4096];
    for (field, value) in [partition, drive, file, block, generation]
        .iter()
        .enumerate()
    {
        data[field * 8..(field + 1) * 8].copy_from_slice(&value.to_le_bytes());
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, &data[..40]);
    let mut state = u64::from_le_bytes(digest.as_ref()[..8].try_into().unwrap());
    for chunk in data[40..].chunks_mut(8) {
        // SplitMix64 expands a tuple-specific seed; the distinct 40-byte tuple
        // header additionally guarantees block identity without relying on hash collisions.
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^= value >> 31;
        chunk.copy_from_slice(&value.to_le_bytes()[..chunk.len()]);
    }
    data
}
fn verify_block(
    data: &[u8],
    partition: u64,
    drive: u64,
    file: u64,
    block: u64,
    generation: u64,
) -> bool {
    data == oracle_block(partition, drive, file, block, generation)
}
