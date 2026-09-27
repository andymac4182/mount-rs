use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use mount_rs_core::diagnostics::profile;
use serde_json::json;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::time::Duration;

fn target_shape(clients: usize) -> CatalogSnapshot {
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

fn small_authority(marker: &str, revision: u64) -> CatalogSnapshot {
    let mut snapshot = CatalogSnapshot::empty();
    snapshot.revision = revision;
    snapshot.partitions.insert(
        "red".into(),
        PartitionDefinition {
            drives: BTreeMap::from([(
                "data".into(),
                DriveDefinition {
                    driver: json!({"kind":"memory","marker":marker}),
                },
            )]),
        },
    );
    snapshot.issuer_policies.insert(
        "issuer".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["mount-rs"]}),
    );
    snapshot.grants.insert(
        "grant".into(),
        GrantDefinition {
            partition_id: "red".into(),
            policy_id: "issuer".into(),
            drives: BTreeMap::from([("data".into(), Permission::Read)]),
            claim_conditions: BTreeMap::from([("/sub".into(), "workload".into())]),
        },
    );
    snapshot
}

fn marker(snapshot: &CatalogSnapshot) -> &str {
    snapshot.partitions["red"].drives["data"].driver["marker"]
        .as_str()
        .unwrap()
}

fn independent_document(path: &Path, snapshot: &CatalogSnapshot) {
    let connection = Connection::open(path).unwrap();
    connection
        .execute(
            "UPDATE service_catalog SET document=?1 WHERE singleton=1",
            params![serde_json::to_vec(snapshot).unwrap()],
        )
        .unwrap();
}

fn slot_counts(catalog: &SqliteCatalog) -> Vec<usize> {
    catalog
        .shared
        .observed_slots
        .iter()
        .map(|slot| slot.load(Ordering::Relaxed))
        .collect()
}

async fn load_every_slot(catalog: &SqliteCatalog, expected: &str) {
    let before = slot_counts(catalog);
    for _ in 0..CONNECTION_POOL_SIZE {
        let loaded = catalog.load_shared_current().await.unwrap();
        assert_eq!(marker(&loaded), expected);
    }
    for (slot, earlier) in slot_counts(catalog).iter().zip(before) {
        assert!(*slot > earlier, "actual pooled handle was not exercised");
    }
}

fn pause_once(
    catalog: &SqliteCatalog,
    point: CatalogTestPoint,
) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (entered_send, entered_recv) = mpsc::channel();
    let (release_send, release_recv) = mpsc::channel();
    let released = Mutex::new(release_recv);
    let once = AtomicBool::new(false);
    *catalog.shared.test_hook.lock().unwrap() = Some(Arc::new(move |at| {
        if at == point && !once.swap(true, Ordering::SeqCst) {
            entered_send.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .expect("test release must be bounded");
        }
    }));
    (entered_recv, release_send)
}

#[tokio::test]
async fn every_handle_observes_same_revision_external_edits_and_repair() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = SqliteCatalog::open(&path).await.unwrap();
    catalog
        .compare_and_swap(0, small_authority("A", 0))
        .await
        .unwrap();
    load_every_slot(&catalog, "A").await;
    let separate = SqliteCatalog::open(&path).await.unwrap();
    load_every_slot(&separate, "A").await;
    let initial = catalog.load_shared_current().await.unwrap();

    independent_document(&path, &small_authority("B", 1));
    load_every_slot(&catalog, "B").await;
    load_every_slot(&separate, "B").await;
    assert!(!Arc::ptr_eq(
        &initial,
        &catalog.load_shared_current().await.unwrap()
    ));
    independent_document(&path, &small_authority("A", 1));
    load_every_slot(&catalog, "A").await;
    let mut revoked = small_authority("A", 1);
    revoked.grants.clear();
    independent_document(&path, &revoked);
    for _ in 0..CONNECTION_POOL_SIZE {
        assert!(
            catalog
                .load_shared_current()
                .await
                .unwrap()
                .grants
                .is_empty()
        );
    }
    let mut changed_issuer = small_authority("A", 1);
    changed_issuer.issuer_policies.insert(
        "issuer".into(),
        json!({"issuer":"https://issuer.example.com","audiences":["other"]}),
    );
    independent_document(&path, &changed_issuer);
    for _ in 0..CONNECTION_POOL_SIZE {
        assert_eq!(
            catalog.load_shared_current().await.unwrap().issuer_policies["issuer"]["audiences"][0],
            "other",
        );
    }
    independent_document(&path, &small_authority("A", 1));
    load_every_slot(&catalog, "A").await;
    load_every_slot(&separate, "A").await;
    assert!(!Arc::ptr_eq(
        &initial,
        &catalog.load_shared_current().await.unwrap()
    ));

    let connection = Connection::open(&path).unwrap();
    let valid = serde_json::to_vec(&small_authority("A", 1)).unwrap();
    for invalid in [
        "UPDATE service_catalog SET document=x'010203' WHERE singleton=1",
        "UPDATE service_catalog SET document=zeroblob(0) WHERE singleton=1",
        "UPDATE service_catalog SET document=zeroblob(8388609) WHERE singleton=1",
    ] {
        connection.execute(invalid, []).unwrap();
        for _ in 0..CONNECTION_POOL_SIZE {
            assert!(catalog.load_shared_current().await.is_err());
        }
        independent_document(&path, &small_authority("A", 1));
        load_every_slot(&catalog, "A").await;
    }
    connection
        .execute(
            "UPDATE service_catalog SET document=CAST(?1 AS TEXT) WHERE singleton=1",
            params![&valid],
        )
        .unwrap();
    for _ in 0..CONNECTION_POOL_SIZE {
        assert!(catalog.load_shared_current().await.is_err());
    }
    independent_document(&path, &small_authority("A", 1));
    load_every_slot(&catalog, "A").await;
    connection
        .execute(
            "UPDATE service_catalog SET revision=2 WHERE singleton=1",
            [],
        )
        .unwrap();
    for _ in 0..CONNECTION_POOL_SIZE {
        assert!(catalog.load_shared_current().await.is_err());
    }
    connection
        .execute(
            "UPDATE service_catalog SET revision=1 WHERE singleton=1",
            [],
        )
        .unwrap();
    load_every_slot(&catalog, "A").await;
    connection
        .execute("DELETE FROM service_catalog", [])
        .unwrap();
    for _ in 0..CONNECTION_POOL_SIZE {
        assert!(catalog.load_shared_current().await.is_err());
    }
    connection
        .execute(
            "INSERT INTO service_catalog(singleton,revision,document) VALUES(1,1,?1)",
            params![valid],
        )
        .unwrap();
    load_every_slot(&catalog, "A").await;
    connection
        .execute_batch("ALTER TABLE service_catalog RENAME COLUMN document TO payload")
        .unwrap();
    for _ in 0..CONNECTION_POOL_SIZE {
        assert!(catalog.load_shared_current().await.is_err());
    }
    connection
        .execute_batch("ALTER TABLE service_catalog RENAME COLUMN payload TO document")
        .unwrap();
    load_every_slot(&catalog, "A").await;
    connection
        .execute_batch("DROP TABLE service_catalog")
        .unwrap();
    for _ in 0..CONNECTION_POOL_SIZE {
        assert!(catalog.load_shared_current().await.is_err());
    }
    connection.execute_batch("CREATE TABLE service_catalog (singleton INTEGER PRIMARY KEY CHECK(singleton=1), revision INTEGER NOT NULL CHECK(revision>=0), document BLOB NOT NULL)").unwrap();
    connection
        .execute(
            "INSERT INTO service_catalog(singleton,revision,document) VALUES(1,1,?1)",
            params![serde_json::to_vec(&small_authority("A", 1)).unwrap()],
        )
        .unwrap();
    load_every_slot(&catalog, "A").await;
    connection
        .execute_batch("CREATE TABLE unrelated_catalog_control (id INTEGER)")
        .unwrap();
    load_every_slot(&catalog, "A").await;
}

#[tokio::test]
async fn external_commit_boundaries_have_documented_observation_points() {
    for point in [
        CatalogTestPoint::BeforeProbe,
        CatalogTestPoint::AfterProbe,
        CatalogTestPoint::AfterSelect,
        CatalogTestPoint::AfterPostProbe,
    ] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("catalog.sqlite");
        let catalog = SqliteCatalog::open(&path).await.unwrap();
        catalog
            .compare_and_swap(0, small_authority("A", 0))
            .await
            .unwrap();
        load_every_slot(&catalog, "A").await;
        catalog.shared.current.lock().unwrap().invalidate();
        let (entered, release) = pause_once(&catalog, point);
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            independent_document(&writer_path, &small_authority("B", 1));
            release.send(()).unwrap();
        });
        let observed = catalog.load_shared_current().await.unwrap();
        writer.join().unwrap();
        *catalog.shared.test_hook.lock().unwrap() = None;
        let expected = match point {
            CatalogTestPoint::BeforeProbe | CatalogTestPoint::AfterProbe => "B",
            CatalogTestPoint::AfterSelect | CatalogTestPoint::AfterPostProbe => "A",
            _ => unreachable!(),
        };
        assert_eq!(marker(&observed), expected, "boundary {point:?}");
        load_every_slot(&catalog, "B").await;
    }
}

#[tokio::test]
async fn own_cas_success_conflict_and_failures_invalidate_all_handles() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = SqliteCatalog::open(&path).await.unwrap();
    catalog
        .compare_and_swap(0, small_authority("A", 0))
        .await
        .unwrap();
    load_every_slot(&catalog, "A").await;

    catalog
        .compare_and_swap(1, small_authority("B", 1))
        .await
        .unwrap();
    load_every_slot(&catalog, "B").await;
    let generation_before_conflict = catalog.shared.current.lock().unwrap().generation;
    assert!(matches!(
        catalog.compare_and_swap(1, small_authority("C", 1)).await,
        Err(CatalogError::Conflict)
    ));
    assert!(catalog.shared.current.lock().unwrap().generation > generation_before_conflict);
    load_every_slot(&catalog, "B").await;

    catalog
        .shared
        .test_cas_fault
        .store(CAS_FAIL_BEFORE_COMMIT, Ordering::Relaxed);
    assert!(matches!(
        catalog.compare_and_swap(2, small_authority("C", 2)).await,
        Err(CatalogError::Invalid("injected precommit failure"))
    ));
    let independent = Connection::open(&path).unwrap();
    let durable_revision: i64 = independent
        .query_row("SELECT revision FROM service_catalog", [], |row| row.get(0))
        .unwrap();
    assert_eq!(durable_revision, 2, "precommit failure must roll back");
    load_every_slot(&catalog, "B").await;

    catalog
        .shared
        .test_cas_fault
        .store(CAS_LOST_RESULT_AFTER_COMMIT, Ordering::Relaxed);
    assert!(matches!(
        catalog.compare_and_swap(2, small_authority("C", 2)).await,
        Err(CatalogError::Invalid("injected lost commit result"))
    ));
    let durable_revision: i64 = independent
        .query_row("SELECT revision FROM service_catalog", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        durable_revision, 3,
        "lost result must not undo durable commit"
    );
    load_every_slot(&catalog, "C").await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancelled_waiter_cannot_rec_certify_during_retained_cas_closure() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("catalog.sqlite");
    let catalog = SqliteCatalog::open(&path).await.unwrap();
    catalog
        .compare_and_swap(0, small_authority("A", 0))
        .await
        .unwrap();
    load_every_slot(&catalog, "A").await;
    let (entered_send, entered_recv) = mpsc::channel();
    let (release_send, release_recv) = mpsc::channel();
    let (completed_send, completed_recv) = mpsc::channel();
    let (reader_entered_send, reader_entered_recv) = mpsc::channel();
    let release_recv = Mutex::new(release_recv);
    *catalog.shared.test_hook.lock().unwrap() = Some(Arc::new(move |point| match point {
        CatalogTestPoint::ReaderAfterConnection => reader_entered_send.send(()).unwrap(),
        CatalogTestPoint::CasAfterInvalidate => {
            entered_send.send(()).unwrap();
            release_recv
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap();
        }
        CatalogTestPoint::CasComplete => completed_send.send(()).unwrap(),
        _ => {}
    }));
    let writer_catalog = catalog.clone();
    let writer = tokio::spawn(async move {
        writer_catalog
            .compare_and_swap(1, small_authority("B", 1))
            .await
    });
    entered_recv.recv_timeout(Duration::from_secs(5)).unwrap();
    let started = std::time::Instant::now();
    let before_slots = slot_counts(&catalog);
    let readers: Vec<_> = (0..CONNECTION_POOL_SIZE)
        .map(|_| {
            let reader = catalog.clone();
            tokio::spawn(async move { reader.load_shared_current().await })
        })
        .collect();
    writer.abort();
    assert!(writer.await.unwrap_err().is_cancelled());
    // The writer holds one connection and the shared cache lock. Seven actual
    // other pooled handles must enter the native read closure and block on the
    // cache lock before the retained writer is released. The eighth reader
    // waits on the writer's connection. Spawn alone is not this observation.
    for _ in 0..CONNECTION_POOL_SIZE - 1 {
        reader_entered_recv
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    }
    assert_eq!(
        slot_counts(&catalog)
            .iter()
            .zip(&before_slots)
            .filter(|(after, before)| after > before)
            .count(),
        CONNECTION_POOL_SIZE - 1,
    );
    assert!(
        readers.iter().all(|reader| !reader.is_finished()),
        "no read can complete while retained CAS holds the cache lock"
    );
    release_send.send(()).unwrap();
    completed_recv.recv_timeout(Duration::from_secs(5)).unwrap();
    for reader in readers {
        let current = tokio::time::timeout(Duration::from_secs(5), reader)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(marker(&current), "B");
        assert_eq!(current.revision, 2);
    }
    eprintln!(
        "barrier_contended_catalog_reader_window_us={}",
        started.elapsed().as_micros()
    );
    for (slot, earlier) in slot_counts(&catalog).iter().zip(before_slots) {
        assert!(
            *slot > earlier,
            "same writer handle and all seven peers must revalidate"
        );
    }
    *catalog.shared.test_hook.lock().unwrap() = None;
    let independent = Connection::open(&path).unwrap();
    let durable_revision: i64 = independent
        .query_row("SELECT revision FROM service_catalog", [], |row| row.get(0))
        .unwrap();
    assert_eq!(durable_revision, 2);
    load_every_slot(&catalog, "B").await;
}

#[tokio::test]
async fn generation_exhaustion_and_active_transaction_fail_closed() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
        .await
        .unwrap();
    catalog
        .compare_and_swap(0, small_authority("A", 0))
        .await
        .unwrap();
    load_every_slot(&catalog, "A").await;
    let slot = catalog.shared.next.load(Ordering::Relaxed) % CONNECTION_POOL_SIZE;
    {
        let guard = catalog.shared.connections[slot].lock().unwrap();
        guard.connection.execute_batch("BEGIN").unwrap();
    }
    assert!(matches!(
        catalog.load_shared_current().await,
        Err(CatalogError::Invalid(
            "catalog connection has active transaction"
        ))
    ));
    {
        let guard = catalog.shared.connections[slot].lock().unwrap();
        guard.connection.execute_batch("ROLLBACK").unwrap();
    }
    load_every_slot(&catalog, "A").await;
    {
        let mut cache = catalog.shared.current.lock().unwrap();
        cache.generation = u64::MAX;
        cache.invalidate();
        assert!(cache.exhausted);
        assert!(cache.current.is_none());
    }
    for _ in 0..CONNECTION_POOL_SIZE {
        assert!(matches!(
            catalog.load_shared_current().await,
            Err(CatalogError::Invalid("catalog cache generation exhausted"))
        ));
    }
}

#[tokio::test]
async fn certificate_rejects_replaced_arc_even_with_unchanged_handle_token() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
        .await
        .unwrap();
    catalog
        .compare_and_swap(0, small_authority("A", 0))
        .await
        .unwrap();
    load_every_slot(&catalog, "A").await;
    let slot = catalog.shared.next.load(Ordering::Relaxed) % CONNECTION_POOL_SIZE;
    let (old_version, old_generation, old_arc) = {
        let pooled = catalog.shared.connections[slot].lock().unwrap();
        let old = pooled.certificate.as_ref().unwrap();
        (old.data_version, old.generation, Arc::clone(&old.snapshot))
    };
    let document = serde_json::to_vec(&small_authority("A", 1)).unwrap();
    let replacement = {
        let mut cache = catalog.shared.current.lock().unwrap();
        cache.invalidate();
        cache.select(1, &document).unwrap()
    };
    assert!(!Arc::ptr_eq(&old_arc, &replacement));
    let loaded = catalog.load_shared_current().await.unwrap();
    assert!(Arc::ptr_eq(&loaded, &replacement));
    let pooled = catalog.shared.connections[slot].lock().unwrap();
    let certificate = pooled.certificate.as_ref().unwrap();
    assert_eq!(certificate.data_version, old_version);
    assert!(certificate.generation > old_generation);
    assert!(Arc::ptr_eq(&certificate.snapshot, &replacement));
}

#[tokio::test]
#[ignore = "owned 10000-Drive SQLite paired control; requires MOUNT_RS_PROFILE_IO=1"]
async fn target_shape_unchanged_reads_avoid_full_blob() {
    assert!(
        profile::enabled(),
        "enabled positive CatalogQuery control required"
    );
    let directory = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
        .await
        .unwrap();
    let shape = target_shape(10_000);
    assert_eq!(
        (shape.partitions.len(), shape.grants.len()),
        (5_000, 10_000)
    );
    let document = serde_json::to_vec(&shape).unwrap();
    assert_eq!(
        document.len(),
        2_271_257,
        "retained descriptor shape changed"
    );
    let digest = ring::digest::digest(&ring::digest::SHA256, &document);
    eprintln!(
        "target_shape_document_sha256_base64url={}",
        URL_SAFE_NO_PAD.encode(digest.as_ref())
    );
    catalog.compare_and_swap(0, shape).await.unwrap();

    let before_slots: Vec<usize> = catalog
        .shared
        .observed_slots
        .iter()
        .map(|slot| slot.load(Ordering::Relaxed))
        .collect();
    for _ in 0..CONNECTION_POOL_SIZE {
        catalog.load_shared_current().await.unwrap();
    }
    for (slot, before) in catalog.shared.observed_slots.iter().zip(&before_slots) {
        assert!(
            slot.load(Ordering::Relaxed) > *before,
            "every actual pool handle must warm"
        );
    }
    let warm = catalog.load_shared_current().await.unwrap();
    let full_before = profile::snapshot();
    let full_diagnostics_before = catalog.read_diagnostics();
    for _ in 0..40 {
        let current = catalog.load_shared_full_control().await.unwrap();
        assert!(
            Arc::ptr_eq(&warm, &current),
            "full-row control must reuse Arc"
        );
        assert_eq!(current.revision, 1);
    }
    let full_delta = profile::snapshot().delta(&full_before).unwrap();
    let full_query = full_delta
        .entries
        .iter()
        .find(|entry| entry.name == "catalog.query_document_bytes")
        .expect("positive full-row control required");
    assert_eq!(full_query.calls, 40);
    assert_eq!(full_query.units, document.len() as u64 * 40);
    let full_diagnostics = catalog.read_diagnostics();
    assert_eq!(
        full_diagnostics.full_blob_queries - full_diagnostics_before.full_blob_queries,
        40,
    );
    assert_eq!(
        full_diagnostics.full_blob_returned_bytes
            - full_diagnostics_before.full_blob_returned_bytes,
        document.len() as u64 * 40,
    );

    // Full-row control leaves certificates empty. Warm each actual handle
    // again, then drain setup work before measuring the candidate.
    let before_candidate_slots: Vec<usize> = catalog
        .shared
        .observed_slots
        .iter()
        .map(|slot| slot.load(Ordering::Relaxed))
        .collect();
    for _ in 0..CONNECTION_POOL_SIZE {
        let current = catalog.load_shared_current().await.unwrap();
        assert!(Arc::ptr_eq(&warm, &current));
    }
    for (slot, before) in catalog
        .shared
        .observed_slots
        .iter()
        .zip(&before_candidate_slots)
    {
        assert!(
            slot.load(Ordering::Relaxed) > *before,
            "every candidate handle must warm"
        );
    }
    let before = profile::snapshot();
    let diagnostics_before = catalog.read_diagnostics();
    for _ in 0..40 {
        let current = catalog.load_shared_current().await.unwrap();
        assert!(
            Arc::ptr_eq(&warm, &current),
            "unchanged typed authority must reuse Arc"
        );
        assert_eq!(current.revision, 1);
    }
    let delta = profile::snapshot().delta(&before).unwrap();
    let loads = delta
        .entries
        .iter()
        .find(|entry| entry.name == "catalog.load")
        .expect("positive authoritative-load control required");
    assert_eq!(loads.calls, 40);
    let query = delta
        .entries
        .iter()
        .find(|entry| entry.name == "catalog.query_document_bytes");
    assert_eq!(query.map_or(0, |entry| entry.calls), 0);
    assert_eq!(query.map_or(0, |entry| entry.units), 0);
    let diagnostics = catalog.read_diagnostics();
    assert_eq!(
        diagnostics.full_blob_queries - diagnostics_before.full_blob_queries,
        0
    );
    assert_eq!(
        diagnostics.full_blob_returned_bytes - diagnostics_before.full_blob_returned_bytes,
        0,
    );
    assert_eq!(
        diagnostics.certified_cache_hits - diagnostics_before.certified_cache_hits,
        40,
    );
    assert_eq!(
        diagnostics.token_probe_calls - diagnostics_before.token_probe_calls,
        40
    );
    #[cfg(feature = "io-profiling")]
    {
        assert_eq!(
            diagnostics.pager_sample_calls - diagnostics_before.pager_sample_calls,
            40
        );
        assert_eq!(
            diagnostics.pager_sample_available - diagnostics_before.pager_sample_available,
            40,
        );
    }
}

fn process_cpu_us() -> (u64, u64) {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    let usage = unsafe { usage.assume_init() };
    let micros = |time: libc::timeval| {
        u64::try_from(time.tv_sec).unwrap() * 1_000_000 + u64::try_from(time.tv_usec).unwrap()
    };
    (micros(usage.ru_utime), micros(usage.ru_stime))
}

fn profile_units(delta: &profile::Snapshot, name: &str) -> u64 {
    delta
        .entries
        .iter()
        .find(|entry| entry.name == name)
        .map_or(0, |entry| entry.units)
}

#[tokio::test]
#[ignore = "owned paired native SQLite resource windows; run with and without MOUNT_RS_PROFILE_IO=1"]
async fn target_shape_paired_full_row_and_conditional_resource_windows() {
    let directory = tempfile::tempdir().unwrap();
    let catalog = SqliteCatalog::open(directory.path().join("catalog.sqlite"))
        .await
        .unwrap();
    let shape = target_shape(10_000);
    let document = serde_json::to_vec(&shape).unwrap();
    assert_eq!(document.len(), 2_271_257);
    catalog.compare_and_swap(0, shape).await.unwrap();
    let expected = catalog.load_shared_current().await.unwrap();
    for round in 0..3 {
        let full_slots_before = slot_counts(&catalog);
        for _ in 0..CONNECTION_POOL_SIZE {
            assert!(Arc::ptr_eq(
                &expected,
                &catalog.load_shared_full_control().await.unwrap()
            ));
        }
        assert!(
            slot_counts(&catalog)
                .iter()
                .zip(full_slots_before)
                .all(|(after, before)| after > &before)
        );
        let full_before = catalog.read_diagnostics();
        let full_profile_before = profile::snapshot();
        let full_cpu = process_cpu_us();
        let full_started = std::time::Instant::now();
        let full_allocations = crate::dispatch::allocation_tests::count(|| {
            for _ in 0..40 {
                let loaded =
                    load_shared_blocking(&catalog.shared, CatalogReadMode::FullRow).unwrap();
                assert!(Arc::ptr_eq(&expected, &loaded));
                std::hint::black_box(loaded);
            }
        });
        let full_wall_us = full_started.elapsed().as_micros();
        let full_cpu_after = process_cpu_us();
        let full_after = catalog.read_diagnostics();
        let full_profile = profile::snapshot().delta(&full_profile_before).unwrap();
        assert_eq!(
            full_after.full_blob_queries - full_before.full_blob_queries,
            if profile::enabled() { 40 } else { 0 }
        );
        let candidate_slots_before = slot_counts(&catalog);
        for _ in 0..CONNECTION_POOL_SIZE {
            assert!(Arc::ptr_eq(
                &expected,
                &catalog.load_shared_current().await.unwrap()
            ));
        }
        assert!(
            slot_counts(&catalog)
                .iter()
                .zip(candidate_slots_before)
                .all(|(after, before)| after > &before)
        );
        let candidate_before = catalog.read_diagnostics();
        let candidate_profile_before = profile::snapshot();
        let candidate_cpu = process_cpu_us();
        let candidate_started = std::time::Instant::now();
        let candidate_allocations = crate::dispatch::allocation_tests::count(|| {
            for _ in 0..40 {
                let loaded =
                    load_shared_blocking(&catalog.shared, CatalogReadMode::Conditional).unwrap();
                assert!(Arc::ptr_eq(&expected, &loaded));
                std::hint::black_box(loaded);
            }
        });
        let candidate_wall_us = candidate_started.elapsed().as_micros();
        let candidate_cpu_after = process_cpu_us();
        let candidate_after = catalog.read_diagnostics();
        let candidate_profile = profile::snapshot()
            .delta(&candidate_profile_before)
            .unwrap();
        assert_eq!(
            candidate_after.full_blob_queries - candidate_before.full_blob_queries,
            0
        );
        if profile::enabled() {
            assert_eq!(
                candidate_after.certified_cache_hits - candidate_before.certified_cache_hits,
                40
            );
            assert_eq!(
                candidate_after.token_probe_calls - candidate_before.token_probe_calls,
                40
            );
        }
        eprintln!(
            "{}",
            json!({
                "round":round, "document_bytes":document.len(), "profile_enabled":profile::enabled(),
                "scope":"40 synchronous calls inside actual SQLite blocking loader per mode; Rust allocator is current thread only; CPU is whole process; SQLite C allocations and physical I/O excluded",
                "full_row":{
                    "wall_us":full_wall_us,
                    "process_user_us":full_cpu_after.0-full_cpu.0,
                    "process_system_us":full_cpu_after.1-full_cpu.1,
                    "rust_alloc_calls":full_allocations.0,
                    "rust_alloc_requested_bytes":full_allocations.1,
                    "full_blob_queries":full_after.full_blob_queries-full_before.full_blob_queries,
                    "returned_document_bytes":full_after.full_blob_returned_bytes-full_before.full_blob_returned_bytes,
                    "pager_sample_calls":full_after.pager_sample_calls-full_before.pager_sample_calls,
                "pager_sample_available":full_after.pager_sample_available-full_before.pager_sample_available,
                "sqlite_pager_hits":profile_units(&full_profile, "catalog.pager_hits"),
                "sqlite_pager_misses":profile_units(&full_profile, "catalog.pager_misses"),
                "sqlite_pager_writes":profile_units(&full_profile, "catalog.pager_writes"),
                },
                "conditional":{
                    "wall_us":candidate_wall_us,
                    "process_user_us":candidate_cpu_after.0-candidate_cpu.0,
                    "process_system_us":candidate_cpu_after.1-candidate_cpu.1,
                    "rust_alloc_calls":candidate_allocations.0,
                    "rust_alloc_requested_bytes":candidate_allocations.1,
                    "token_probe_calls":candidate_after.token_probe_calls-candidate_before.token_probe_calls,
                    "certified_cache_hits":candidate_after.certified_cache_hits-candidate_before.certified_cache_hits,
                    "full_blob_queries":candidate_after.full_blob_queries-candidate_before.full_blob_queries,
                    "returned_document_bytes":candidate_after.full_blob_returned_bytes-candidate_before.full_blob_returned_bytes,
                    "pager_sample_calls":candidate_after.pager_sample_calls-candidate_before.pager_sample_calls,
                "pager_sample_available":candidate_after.pager_sample_available-candidate_before.pager_sample_available,
                "sqlite_pager_hits":profile_units(&candidate_profile, "catalog.pager_hits"),
                "sqlite_pager_misses":profile_units(&candidate_profile, "catalog.pager_misses"),
                "sqlite_pager_writes":profile_units(&candidate_profile, "catalog.pager_writes"),
                }
            })
        );
    }
}
