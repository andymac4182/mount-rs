//! Differential controls retain the original full-map authorization/audit oracles.
use super::*;
use crate::{
    auth::{authorize_drive, authorize_prepared_drive},
    catalog::{DriveDefinition, PartitionDefinition, Permission},
    dispatch::SessionIdentity,
    request_metadata::{AuditRecord, MatchingGrants, claim_pointer, write_audit},
};
use mount_rs_remote_protocol::OperationName;
use serde::{Serialize, Serializer, ser::SerializeSeq};
use serde_json::{Value, json};

fn identity() -> SessionIdentity {
    SessionIdentity {
        partition_id: "red".into(),
        policy_id: "policy".into(),
        issuer: "https://issuer.example.com".into(),
        subject: "worker".into(),
        signing_algorithm: "ES256".into(),
        claims: json!({"aud":"mount-rs","sub":"worker","a/b":{"~key":"escaped"},"items":["zero"],"number":7}),
        expires_at: i64::MAX,
    }
}

fn grant(
    partition: &str,
    policy: &str,
    drive: &str,
    permission: Permission,
    conditions: &[(&str, &str)],
) -> GrantDefinition {
    GrantDefinition {
        partition_id: partition.into(),
        policy_id: policy.into(),
        drives: BTreeMap::from([(drive.into(), permission)]),
        claim_conditions: conditions
            .iter()
            .map(|(pointer, expected)| ((*pointer).into(), (*expected).into()))
            .collect(),
    }
}

fn authority() -> CatalogSnapshot {
    let mut catalog = CatalogSnapshot::empty();
    catalog.revision = 7;
    for partition in ["red", "blue"] {
        catalog.partitions.insert(
            partition.into(),
            PartitionDefinition {
                drives: ["data", "other"]
                    .into_iter()
                    .map(|drive| {
                        (
                            drive.into(),
                            DriveDefinition {
                                driver: json!({"kind":"memory"}),
                            },
                        )
                    })
                    .collect(),
            },
        );
    }
    for policy in ["policy", "different"] {
        catalog.issuer_policies.insert(
            policy.into(),
            json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
        );
    }
    // Deliberately insert Write before Read and include unvalidated empty conditions.
    for (id, partition, policy, drive, permission, conditions) in [
        (
            "z-write",
            "red",
            "policy",
            "data",
            Permission::Write,
            &[("/sub", "worker")][..],
        ),
        (
            "a-read",
            "red",
            "policy",
            "data",
            Permission::Read,
            &[("/sub", "worker")][..],
        ),
        (
            "other-partition",
            "blue",
            "policy",
            "data",
            Permission::Write,
            &[("/sub", "worker")][..],
        ),
        (
            "other-policy",
            "red",
            "different",
            "data",
            Permission::Write,
            &[("/sub", "worker")][..],
        ),
        (
            "other-drive",
            "red",
            "policy",
            "other",
            Permission::Write,
            &[("/sub", "worker")][..],
        ),
        (
            "wrong-claim",
            "red",
            "policy",
            "data",
            Permission::Write,
            &[("/sub", "another")][..],
        ),
        (
            "escaped-claim",
            "red",
            "policy",
            "data",
            Permission::Read,
            &[("/a~1b/~0key", "escaped"), ("/items/0", "zero")][..],
        ),
        (
            "non-string",
            "red",
            "policy",
            "data",
            Permission::Write,
            &[("/number", "7")][..],
        ),
        ("empty", "red", "policy", "data", Permission::Write, &[][..]),
        (
            "line\nwith\"escapes",
            "red",
            "policy",
            "data",
            Permission::Read,
            &[("/sub", "worker")][..],
        ),
    ] {
        catalog.grants.insert(
            id.into(),
            grant(partition, policy, drive, permission, conditions),
        );
    }
    catalog
}

// This is the exact pre-index audit predicate and iteration order, not the new iterator.
struct FullScanGrants<'a> {
    catalog: &'a CatalogSnapshot,
    identity: &'a SessionIdentity,
    drive_id: &'a str,
}
impl Serialize for FullScanGrants<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut sequence = serializer.serialize_seq(None)?;
        for (id, grant) in &self.catalog.grants {
            if grant.partition_id == self.identity.partition_id
                && grant.policy_id == self.identity.policy_id
                && grant.drives.contains_key(self.drive_id)
                && grant.claim_conditions.iter().all(|(pointer, expected)| {
                    claim_pointer(&self.identity.claims, pointer).and_then(Value::as_str)
                        == Some(expected.as_str())
                })
            {
                sequence.serialize_element(id)?;
            }
        }
        sequence.end()
    }
}
#[derive(Serialize)]
struct FullScanAuditRecord<'a> {
    event: &'static str,
    partition_id: &'a str,
    drive_id: &'a str,
    grant_ids: FullScanGrants<'a>,
    operation: OperationName,
    request_id: u64,
    outcome: &'a str,
}

fn reference_audit(
    catalog: &CatalogSnapshot,
    identity: &SessionIdentity,
    drive: &str,
    outcome: &str,
) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&FullScanAuditRecord {
        event: "remote_access",
        partition_id: &identity.partition_id,
        drive_id: drive,
        grant_ids: FullScanGrants {
            catalog,
            identity,
            drive_id: drive,
        },
        operation: OperationName::HandleWrite,
        request_id: 42,
        outcome,
    })
    .unwrap();
    bytes.push(b'\n');
    bytes
}

fn indexed_audit(
    catalog: &PreparedCatalog,
    identity: &SessionIdentity,
    drive: &str,
    outcome: &str,
) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_audit(
        &mut bytes,
        &AuditRecord {
            event: "remote_access",
            partition_id: &identity.partition_id,
            drive_id: drive,
            grant_ids: MatchingGrants {
                catalog,
                identity,
                drive_id: drive,
            },
            operation: OperationName::HandleWrite,
            request_id: 42,
            outcome,
        },
    )
    .unwrap();
    bytes
}

fn indexed_permission(
    catalog: &PreparedCatalog,
    identity: &SessionIdentity,
    drive: &str,
) -> Option<Permission> {
    authorize_prepared_drive(
        catalog,
        &identity.policy_id,
        &identity.claims,
        &identity.partition_id,
        drive,
    )
}

#[test]
fn indexed_authorization_and_audit_match_full_map_across_scopes_and_claims() {
    let source = Arc::new(authority());
    let catalog = PreparedCatalog::new(Arc::clone(&source));
    let mut current = identity();
    for partition in ["red", "blue", "missing"] {
        current.partition_id = partition.into();
        for policy in ["policy", "different", "missing"] {
            current.policy_id = policy.into();
            for claims in [
                identity().claims,
                json!({"sub":"another"}),
                json!({"sub":7,"number":"7"}),
                json!({"a/b":{"~key":"escaped"},"items":["zero"]}),
                json!({}),
                Value::Null,
            ] {
                current.claims = claims;
                for drive in ["data", "other", "missing"] {
                    assert_eq!(
                        indexed_permission(&catalog, &current, drive),
                        authorize_drive(&source, policy, &current.claims, partition, drive)
                    );
                    for outcome in ["ok", "EIO", "EACCES"] {
                        assert_eq!(
                            indexed_audit(&catalog, &current, drive, outcome),
                            reference_audit(&source, &current, drive, outcome)
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn indexed_grants_preserve_write_union_and_empty_condition_distinction() {
    let source = Arc::new(authority());
    let catalog = PreparedCatalog::new(Arc::clone(&source));
    let identity = identity();
    assert_eq!(
        indexed_permission(&catalog, &identity, "data"),
        Some(Permission::Write)
    );
    let mut empty_only = (*source).clone();
    empty_only.grants.retain(|id, _| id == "empty");
    let catalog = PreparedCatalog::new(Arc::new(empty_only));
    assert_eq!(indexed_permission(&catalog, &identity, "data"), None);
    let event: Value =
        serde_json::from_slice(&indexed_audit(&catalog, &identity, "data", "ok")).unwrap();
    assert_eq!(event["grant_ids"], json!(["empty"]));
}

#[test]
fn indexed_write_union_is_independent_of_grant_id_order() {
    for (read_id, write_id, expected_order) in [
        ("a-read", "z-write", [Permission::Read, Permission::Write]),
        ("z-read", "a-write", [Permission::Write, Permission::Read]),
    ] {
        let mut source = authority();
        source.grants.clear();
        source.grants.insert(
            read_id.into(),
            grant(
                "red",
                "policy",
                "data",
                Permission::Read,
                &[("/sub", "worker")],
            ),
        );
        source.grants.insert(
            write_id.into(),
            grant(
                "red",
                "policy",
                "data",
                Permission::Write,
                &[("/sub", "worker")],
            ),
        );
        source.validate().unwrap();
        assert_eq!(source.grants.len(), 2);
        let sorted_permissions: Vec<_> = source
            .grants
            .values()
            .map(|matching| matching.drives.get("data").unwrap())
            .collect();
        assert_eq!(
            sorted_permissions,
            expected_order.iter().collect::<Vec<_>>()
        );
        let source = Arc::new(source);
        let prepared = PreparedCatalog::new(Arc::clone(&source));
        let current = identity();
        assert_eq!(
            indexed_permission(&prepared, &current, "data"),
            Some(Permission::Write)
        );
        assert_eq!(
            indexed_permission(&prepared, &current, "data"),
            authorize_drive(&source, "policy", &current.claims, "red", "data"),
        );
        for outcome in ["ok", "EIO", "EACCES"] {
            assert_eq!(
                indexed_audit(&prepared, &current, "data", outcome),
                reference_audit(&source, &current, "data", outcome),
            );
        }
    }
}

#[test]
fn reused_candidate_index_evaluates_current_identity_claims() {
    let cache = PreparedCatalogCache::default();
    let source = Arc::new(authority());
    let first = cache.prepare(Arc::clone(&source)).unwrap().catalog;
    let second = cache.prepare(source).unwrap().catalog;
    assert!(Arc::ptr_eq(&first, &second));
    let mut current = identity();
    let before = indexed_audit(&first, &current, "data", "ok");
    current.claims = json!({"sub":"another"});
    let after = indexed_audit(&second, &current, "data", "ok");
    assert_ne!(before, after);
    assert_eq!(
        after,
        reference_audit(second.snapshot(), &current, "data", "ok")
    );
}

#[test]
fn new_arc_at_same_revision_replaces_candidates_and_retains_old_authority() {
    let cache = PreparedCatalogCache::default();
    let original = Arc::new(authority());
    let prepared = cache.prepare(Arc::clone(&original)).unwrap().catalog;
    let mut replacement = Arc::clone(&original);
    Arc::make_mut(&mut replacement).grants.clear();
    assert_eq!(original.revision, replacement.revision);
    let revoked = cache.prepare(Arc::clone(&replacement)).unwrap().catalog;
    assert!(!Arc::ptr_eq(&prepared, &revoked));
    assert!(Arc::ptr_eq(&prepared.source, &original));
    assert!(Arc::ptr_eq(&revoked.source, &replacement));
    assert_eq!(indexed_permission(&revoked, &identity(), "data"), None);
    assert_eq!(
        indexed_permission(&prepared, &identity(), "data"),
        Some(Permission::Write)
    );
    assert_eq!(
        indexed_audit(&prepared, &identity(), "data", "ok"),
        reference_audit(&original, &identity(), "data", "ok")
    );
}

#[test]
fn simultaneous_cold_readers_share_one_prepared_authority() {
    let cache = Arc::new(PreparedCatalogCache::default());
    let source = Arc::new(authority());
    let barrier = Arc::new(std::sync::Barrier::new(16));
    let readers: Vec<_> = (0..16)
        .map(|_| {
            let cache = Arc::clone(&cache);
            let source = Arc::clone(&source);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                cache.prepare(source).unwrap().catalog
            })
        })
        .collect();
    let results: Vec<_> = readers
        .into_iter()
        .map(|reader| reader.join().unwrap())
        .collect();
    for result in &results {
        assert!(Arc::ptr_eq(result, &results[0]));
    }
}

#[test]
fn concurrent_same_revision_sources_never_mix_candidates_and_authority() {
    let cache = Arc::new(PreparedCatalogCache::default());
    let first = Arc::new(authority());
    let mut next = (*first).clone();
    next.grants.clear();
    let second = Arc::new(next);
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let readers: Vec<_> = (0..8)
        .map(|reader| {
            let cache = Arc::clone(&cache);
            let source = Arc::clone(if reader % 2 == 0 { &first } else { &second });
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..32 {
                    let prepared = cache.prepare(Arc::clone(&source)).unwrap().catalog;
                    assert!(Arc::ptr_eq(&prepared.source, &source));
                    assert_eq!(
                        indexed_permission(&prepared, &identity(), "data"),
                        authorize_drive(&source, "policy", &identity().claims, "red", "data")
                    );
                }
            })
        })
        .collect();
    for reader in readers {
        reader.join().unwrap();
    }
}

#[test]
fn poisoned_cache_locks_fail_closed_without_reusing_stale_candidates() {
    for poison_build_lock in [false, true] {
        let cache = Arc::new(PreparedCatalogCache::default());
        let source = Arc::new(authority());
        cache.prepare(Arc::clone(&source)).unwrap();
        let poison = Arc::clone(&cache);
        assert!(
            std::thread::spawn(move || {
                if poison_build_lock {
                    let _guard = poison.build.lock().unwrap();
                    panic!("controlled build lock poison");
                } else {
                    let _guard = poison.current.write().unwrap();
                    panic!("controlled index lock poison");
                }
            })
            .join()
            .is_err()
        );
        // A poisoned slow-path lock must fail on a new source rather than affect warm reads.
        let next = if poison_build_lock {
            Arc::new((*source).clone())
        } else {
            source
        };
        assert!(cache.prepare(Arc::clone(&next)).is_err());
    }
}

pub(crate) fn target_authority(drives: usize) -> CatalogSnapshot {
    assert!(drives > 0 && drives.is_multiple_of(2));
    let mut catalog = CatalogSnapshot::empty();
    catalog.issuer_policies.insert(
        "policy".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    for index in (0..drives).rev() {
        let partition = format!("p{:05}", index / 2);
        let drive = format!("d{index:05}");
        catalog
            .partitions
            .entry(partition.clone())
            .or_insert_with(|| PartitionDefinition {
                drives: BTreeMap::new(),
            })
            .drives
            .insert(
                drive.clone(),
                DriveDefinition {
                    driver: json!({"kind":"memory"}),
                },
            );
        catalog.grants.insert(
            format!("g{index:05}"),
            grant(
                &partition,
                "policy",
                &drive,
                Permission::Write,
                &[("/sub", &format!("c{index:05}"))],
            ),
        );
    }
    catalog
}

#[test]
fn target_catalog_scans_only_partition_policy_candidates_for_audit() {
    let source = Arc::new(target_authority(10_000));
    source.validate().unwrap();
    let prepared = PreparedCatalogCache::default()
        .prepare(Arc::clone(&source))
        .unwrap()
        .catalog;
    assert_eq!(source.partitions.len(), 5_000);
    for index in [0, 5_000, 9_999] {
        let mut current = identity();
        current.partition_id = format!("p{:05}", index / 2);
        current.claims = json!({"sub":format!("c{index:05}")});
        let drive = format!("d{index:05}");
        assert_eq!(
            prepared.candidates(&current.partition_id, "policy").len(),
            2
        );
        crate::request_metadata::AUDIT_CANDIDATE_VISITS.with(|visits| visits.set(0));
        let bytes = indexed_audit(&prepared, &current, &drive, "ok");
        let visits = crate::request_metadata::AUDIT_CANDIDATE_VISITS.with(|visits| visits.get());
        assert_eq!(
            visits, 2,
            "audit scanned unrelated grants under its writer lock"
        );
        assert_eq!(bytes, reference_audit(&source, &current, &drive, "ok"));
        assert_eq!(
            indexed_permission(&prepared, &current, &drive),
            Some(Permission::Write)
        );
    }
}

#[test]
fn warmed_target_catalog_selection_and_audit_allocate_nothing() {
    let source = Arc::new(target_authority(10_000));
    let cache = PreparedCatalogCache::default();
    let prepared = cache.prepare(Arc::clone(&source)).unwrap().catalog;
    let mut current = identity();
    current.partition_id = "p04999".into();
    current.claims = json!({"sub":"c09999"});
    let expected = reference_audit(&source, &current, "d09999", "ok");
    let mut output = Vec::with_capacity(expected.len());
    // Initialize test-only thread observation before the allocator window.
    crate::request_metadata::AUDIT_CANDIDATE_VISITS.with(|visits| visits.set(0));
    let allocated = crate::dispatch::allocation_tests::count(|| {
        let reused = cache.prepare(Arc::clone(&source)).unwrap().catalog;
        assert!(Arc::ptr_eq(&prepared, &reused));
        assert_eq!(
            indexed_permission(&reused, &current, "d09999"),
            Some(Permission::Write)
        );
        write_audit(
            &mut output,
            &AuditRecord {
                event: "remote_access",
                partition_id: &current.partition_id,
                drive_id: "d09999",
                grant_ids: MatchingGrants {
                    catalog: &reused,
                    identity: &current,
                    drive_id: "d09999",
                },
                operation: OperationName::HandleWrite,
                request_id: 42,
                outcome: "ok",
            },
        )
        .unwrap();
    });
    assert_eq!(allocated, (0, 0), "warmed grant selection/audit allocates");
    assert_eq!(output, expected);
}

#[test]
fn uniquely_owned_authority_keeps_full_scan_without_using_shared_index_gate() {
    let cache = Arc::new(PreparedCatalogCache::default());
    let source = Arc::new(authority());
    let expected = reference_audit(&source, &identity(), "data", "ok");
    let grant_count = source.grants.len();
    // A poisoned gate would reject a fresh default-store snapshot if the local
    // fallback used it. This avoids an indefinitely held-lock failure control.
    let poison = Arc::clone(&cache);
    assert!(
        std::thread::spawn(move || {
            let _gate = poison.build.lock().unwrap();
            panic!("controlled unused shared gate poison");
        })
        .join()
        .is_err()
    );
    let loaded = cache.prepare(source).unwrap();
    assert!(!loaded.waited);
    assert!(loaded.catalog.scopes.is_none());
    assert_eq!(
        loaded.catalog.candidates("red", "policy").len(),
        grant_count
    );
    assert_eq!(
        indexed_audit(&loaded.catalog, &identity(), "data", "ok"),
        expected
    );
    assert_eq!(
        indexed_permission(&loaded.catalog, &identity(), "data"),
        Some(Permission::Write)
    );
}

#[test]
fn indexed_audit_preserves_large_records_and_io_error_prefixes() {
    let mut source = authority();
    source.grants.clear();
    for index in (0..400).rev() {
        source.grants.insert(
            format!("grant-{index:04}-{}", "x".repeat(48)),
            grant(
                "red",
                "policy",
                "data",
                Permission::Read,
                &[("/sub", "worker")],
            ),
        );
    }
    let prepared = PreparedCatalog::new(Arc::new(source));
    let current = identity();
    let expected = reference_audit(prepared.snapshot(), &current, "data", "EIO");
    assert!(expected.len() > 4096);
    assert_eq!(indexed_audit(&prepared, &current, "data", "EIO"), expected);
    struct FailingWriter {
        bytes: Vec<u8>,
        remaining: usize,
    }
    impl std::io::Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("controlled audit failure"));
            }
            let count = self.remaining.min(bytes.len());
            self.bytes.extend_from_slice(&bytes[..count]);
            self.remaining -= count;
            Ok(count)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for limit in [1, 4095, 4096, 5000] {
        let mut writer = FailingWriter {
            bytes: Vec::new(),
            remaining: limit,
        };
        let error = write_audit(
            &mut writer,
            &AuditRecord {
                event: "remote_access",
                partition_id: &current.partition_id,
                drive_id: "data",
                grant_ids: MatchingGrants {
                    catalog: &prepared,
                    identity: &current,
                    drive_id: "data",
                },
                operation: OperationName::HandleWrite,
                request_id: 42,
                outcome: "EIO",
            },
        )
        .unwrap_err();
        assert!(error.is_io());
        assert_eq!(writer.bytes, expected[..limit]);
    }
}
