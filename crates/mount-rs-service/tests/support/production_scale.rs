//! Production target qualification; separate from the historical saturation control.
#[path = "production_fixture.rs"]
mod fixture;
use fixture::{
    FileProfile, GenerationLedger, SignedTokens, expect_authentication_denial, oracle_block,
    target_catalog, verify_block,
};
use mount_rs_service::catalog::{DriveDefinition, PartitionDefinition, Permission};
use serde_json::json;
use std::collections::BTreeMap;

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
    assert!(
        mount_rs_core::diagnostics::profile::enabled(),
        "catalog profile must be enabled"
    );
    let directory = tempfile::tempdir().unwrap();
    let mut rows = Vec::new();
    for clients in [10, 100, 1_000, 10_000] {
        let catalog =
            SqliteCatalog::open(directory.path().join(format!("catalog-{clients}.sqlite")))
                .await
                .unwrap();
        let snapshot = target_catalog(clients);
        let encoded = serde_json::to_vec(&snapshot).unwrap();
        let document_bytes = encoded.len();
        let document_digest = {
            use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
            URL_SAFE_NO_PAD.encode(ring::digest::digest(&ring::digest::SHA256, &encoded).as_ref())
        };
        catalog.compare_and_swap(0, snapshot).await.unwrap();
        // Drain setup/CAS counters on all eight exact round-robin handles.
        for _ in 0..8 {
            catalog.load_shared_current().await.unwrap();
        }
        let warm = catalog.load_shared_current().await.unwrap();
        let claims = json!({"sandbox_id":(clients - 1).to_string()});
        let partition = format!("partition-{}", (clients - 1) / 2);
        let drive = format!("sandbox-{}", clients - 1);
        let before = mount_rs_core::diagnostics::profile::snapshot();
        let diagnostics_before = catalog.read_diagnostics();
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
        let loads = profile
            .entries
            .iter()
            .find(|entry| entry.name == "catalog.load")
            .expect("forty positive authoritative load observations required");
        assert_eq!(loads.calls, 40);
        let query = profile
            .entries
            .iter()
            .find(|entry| entry.name == "catalog.query_document_bytes");
        #[cfg(unix)]
        {
            let diagnostics_after = catalog.read_diagnostics();
            assert!(diagnostics_before.enabled && diagnostics_after.enabled);
            assert!(
                diagnostics_after
                    .slot_observations
                    .iter()
                    .zip(diagnostics_before.slot_observations)
                    .all(|(after, before)| after > &before),
                "all actual eight pool handles must be observed"
            );
            assert_eq!(
                diagnostics_after.certified_cache_hits - diagnostics_before.certified_cache_hits,
                40
            );
            assert_eq!(
                diagnostics_after.full_blob_queries - diagnostics_before.full_blob_queries,
                0
            );
            assert_eq!(
                diagnostics_after.full_blob_returned_bytes
                    - diagnostics_before.full_blob_returned_bytes,
                0
            );
            assert_eq!(
                diagnostics_after.pager_sample_calls - diagnostics_before.pager_sample_calls,
                40
            );
            assert_eq!(
                diagnostics_after.pager_sample_available
                    - diagnostics_before.pager_sample_available,
                40,
                "successful pager sampling required"
            );
            assert_eq!(query.map_or(0, |entry| entry.calls), 0);
            assert_eq!(query.map_or(0, |entry| entry.units), 0);
        }
        #[cfg(not(unix))]
        {
            let _ = diagnostics_before;
            let query =
                query.expect("non-Unix full-row control requires positive query observation");
            assert_eq!(query.calls, 40);
            assert_eq!(query.units, document_bytes as u64 * 40);
        }
        #[cfg(all(feature = "resource-profiling", unix))]
        let resources = super::resource_profile::Snapshot::capture_process()
            .unwrap()
            .delta(&resources_before)
            .unwrap();
        #[cfg(not(all(feature = "resource-profiling", unix)))]
        let resources = serde_json::Value::Null;
        let diagnostics_after = catalog.read_diagnostics();
        rows.push(json!({"clients":clients,"partitions":clients/2,"drives":clients,"grants":clients,"authoritative_loads":40,"full_blob_query_calls":query.map_or(0, |entry| entry.calls),"returned_document_bytes":query.map_or(0, |entry| entry.units),"document_bytes":document_bytes,"document_sha256_base64url":document_digest,"elapsed_us":elapsed.as_micros(),"read_diagnostics_before":diagnostics_before,"read_diagnostics_after":diagnostics_after,"profile":profile,"resources":resources}));
    }
    let artifact = json!({"schema":"mount-rs-catalog-shape-profile-v2","source_revision":std::env::var("MOUNT_RS_PRODUCTION_SOURCE_REVISION").expect("source revision required"),"build_profile":if cfg!(debug_assertions){"debug"}else{"release"},"scope":"one process, minimal fixture descriptors, authoritative catalog loads plus local SQLite API and pager observations; no network/filesystem/connection capacity or physical I/O claim; instrumentation affects throughput; full-row positive control is private same-loader unit test","allocation_profile":cfg!(feature="allocation-profiling"),"rows":rows});
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

async fn start_balanced_fixture(
    catalog: std::sync::Arc<mount_rs_service::catalog::SqliteCatalog>,
    drivers: Vec<std::sync::Arc<dyn mount_rs_core::FsDriver>>,
    tokens: &SignedTokens,
) -> Result<
    (
        mount_rs_service::server::RemoteServer,
        quinn::Endpoint,
        tempfile::TempDir,
    ),
    String,
> {
    struct Keys(mount_rs_service::auth::Jwk);
    #[async_trait::async_trait]
    impl mount_rs_service::auth::OidcKeySource for Keys {
        async fn fetch(
            &self,
            issuer: &str,
            audiences: &[String],
        ) -> Result<mount_rs_service::auth::OidcVerifier, mount_rs_service::auth::AuthError>
        {
            mount_rs_service::auth::OidcVerifier::new(issuer, audiences, vec![self.0.clone()])
        }
    }
    let snapshot = catalog
        .load_shared_current()
        .await
        .map_err(|_| "catalog load failed")?;
    let mut dispatcher = mount_rs_service::dispatch::DriveDispatcher::new(catalog.clone());
    for (client, driver) in drivers.into_iter().enumerate() {
        let partition = format!("partition-{}", client / 2);
        let drive = format!("sandbox-{client}");
        let definition = snapshot.partitions[&partition].drives[&drive]
            .driver
            .clone();
        dispatcher
            .register_definition(&partition, &drive, definition, driver)
            .map_err(|_| "Drive registration failed")?;
    }
    let authenticator = std::sync::Arc::new(
        mount_rs_service::auth::CatalogAuthenticator::with_key_source(
            catalog,
            std::sync::Arc::new(Keys(tokens.jwk.clone())),
        ),
    );
    super::wire::bind_authenticated_dispatcher(
        dispatcher,
        authenticator,
        tempfile::tempdir().map_err(|_| "fixture directory failed")?,
        mount_rs_service::server::RemoteTransferLimits::default(),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_server_signed_oidc_balanced_scope_smoke() {
    use mount_rs_remote_protocol::OperationName;
    use std::sync::Arc;
    let directory = tempfile::tempdir().unwrap();
    let catalog = Arc::new(
        mount_rs_service::catalog::SqliteCatalog::open(directory.path().join("catalog.sqlite"))
            .await
            .unwrap(),
    );
    catalog
        .compare_and_swap(0, target_catalog(10))
        .await
        .unwrap();
    let tokens = SignedTokens::new();
    let drivers: Vec<Arc<dyn mount_rs_core::FsDriver>> = (0..10)
        .map(|_| Arc::new(mount_rs_memfs::MemoryFs::empty()) as Arc<dyn mount_rs_core::FsDriver>)
        .collect();
    let mut servers = Vec::new();
    for _ in 0..10 {
        servers.push(
            start_balanced_fixture(catalog.clone(), drivers.clone(), &tokens)
                .await
                .unwrap(),
        );
    }
    let mut connections = Vec::new();
    let mut attempted = 0;
    let mut sibling_denials = 0;
    let mut partition_denials = 0;
    let mut verified = 0;
    let work: Result<(), String> = async {
        for (client, (server, endpoint, _)) in servers.iter().enumerate() {
            attempted += 1;
            let token = tokens.token(client, 300);
            let partition = format!("partition-{}", client / 2);
            let drive = format!("sandbox-{client}");
            let connection =
                super::wire::connect_token(endpoint, server.local_addr(), &partition, &token)
                    .await?;
            let sibling = super::wire::request(
                &connection,
                1,
                &format!("sandbox-{}", client ^ 1),
                OperationName::Stat,
                json!({"path":"/"}),
            )
            .await?;
            if sibling != Err("EACCES".into()) {
                return Err("ungranted sibling Drive accepted".into());
            }
            sibling_denials += 1;
            let other_partition = format!("partition-{}", ((client + 2) % 10) / 2);
            expect_authentication_denial(endpoint, server.local_addr(), &other_partition, &token)
                .await?;
            partition_denials += 1;
            let handle = super::wire::success(
                &connection,
                2,
                &drive,
                OperationName::Open,
                json!({"path":"/oracle","flags":"w+","mode":420}),
            )
            .await?
            .as_u64()
            .ok_or("invalid handle")?;
            let request = mount_rs_remote_protocol::binary::IoRequest {
                drive_id: drive.clone(),
                handle,
                position: Some(0),
            };
            let payload = oracle_block((client / 2) as u64, client as u64, 0, 0, 0);
            if super::wire::handle_write(&connection, 3, &request, &payload).await? != 4096 {
                return Err("short smoke write".into());
            }
            let mut actual = [0; 4096];
            if super::wire::handle_read(&connection, 4, &request, &mut actual).await? != 4096
                || !verify_block(&actual, (client / 2) as u64, client as u64, 0, 0, 0)
            {
                return Err("smoke byte oracle mismatch".into());
            }
            super::wire::success(
                &connection,
                5,
                &drive,
                OperationName::HandleClose,
                json!({"handle":handle}),
            )
            .await?;
            verified += 1;
            connections.push(connection);
        }
        Ok(())
    }
    .await;
    let concurrent_connections = connections.len();
    for connection in connections {
        connection.close(0u32.into(), b"smoke complete");
    }
    for (server, endpoint, _directory) in servers {
        endpoint.close(0u32.into(), b"fixture complete");
        server.close().await;
        endpoint.wait_idle().await;
    }
    if let Ok(output) = std::env::var("MOUNT_RS_PRODUCTION_AUTH_SMOKE_OUTPUT") {
        let artifact = json!({"schema":"mount-rs-production-auth-smoke-v1","servers":10,"configured_clients":10,"attempted_clients":attempted,"concurrent_connections":concurrent_connections,"sibling_denials":sibling_denials,"partition_denials":partition_denials,"verified_files":verified,"verified_bytes":verified*4096,"files_per_drive":1,"auth":"real ES256 JWT signature validation; owned static JWK source; discovery/JWKS network fetching excluded","token_lifetime_seconds":300,"scope":"one process/runtime on loopback; ten listeners; ten shared volatile MemoryFs Drives; auth/transport correctness only","cleanup":"all owned endpoints and listeners closed/idle","work_error":work.as_ref().err()});
        std::fs::write(output, serde_json::to_vec_pretty(&artifact).unwrap()).unwrap();
    }
    work.unwrap();
    assert_eq!(
        (
            attempted,
            concurrent_connections,
            sibling_denials,
            partition_denials,
            verified
        ),
        (10, 10, 10, 10, 10)
    );
}

#[path = "production_checkpoint.rs"]
mod production_checkpoint;
