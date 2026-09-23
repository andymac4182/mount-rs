#![cfg(feature = "foundationdb")]

use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{
    BlockStore, ConcurrentModeState, DirectoryEntry, MetadataStore, Namespace, NodeData,
    NodeMetadata,
};
use mount_rs_core::types::{S_IFDIR, S_IFLNK, Stats};
use mount_rs_core::{ErrorCode, Result};
use mount_rs_foundationdb::{
    DEFAULT_METADATA_CHUNK_BYTES, FoundationDbLeaseAuthority, FoundationDbLimits,
    FoundationDbSharedLeaseOracle, FoundationDbStorage, FoundationDbStorageOptions,
    LeaseAuthorityKind, LeaseOracle, LeasePublicationPolicy,
};
use std::collections::BTreeMap;
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const AUTHORITY_MAX_FORWARD_JUMP: Duration = Duration::from_secs(300);

fn authority_publication_policy() -> LeasePublicationPolicy {
    LeasePublicationPolicy::new(
        Duration::from_secs(120),
        Duration::from_secs(30),
        Duration::from_secs(120),
    )
    .expect("the hosted production authority policy must be valid")
}

fn configured_cluster_file() -> Option<String> {
    match env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE") {
        Ok(path) if !path.trim().is_empty() => Some(path),
        _ if env::var("MOUNT_RS_FOUNDATIONDB_USE_DEFAULT").as_deref() == Ok("1") => {
            Some(foundationdb::default_config_path().to_owned())
        }
        _ => None,
    }
}

fn empty_namespace() -> Namespace {
    let root = 1;
    let chunker = FixedSizeChunker::new(4096).unwrap();
    let stats = Stats {
        dev: 1,
        ino: root,
        mode: S_IFDIR | 0o755,
        nlink: 2,
        uid: 0,
        gid: 0,
        rdev: 0,
        size: 4096,
        blksize: 4096,
        blocks: 8,
        atime_ms: 1,
        mtime_ms: 1,
        ctime_ms: 1,
        birthtime_ms: 1,
    };
    Namespace {
        format_version: 1,
        root,
        next_inode: root + 1,
        default_uid: 0,
        default_gid: 0,
        umask: 0,
        default_chunker: chunker.config(),
        nodes: BTreeMap::from([(
            root,
            NodeMetadata {
                stats,
                data: NodeData::Directory { entries: vec![] },
            },
        )]),
    }
}

fn multi_chunk_namespace() -> Namespace {
    let mut namespace = empty_namespace();
    let root = namespace.root;
    let mut entries = Vec::new();

    for index in 0..160_u64 {
        let inode = namespace.next_inode;
        namespace.next_inode += 1;
        let name = format!("link-{index:03}-{}", "n".repeat(24));
        let target = format!("/target/{index}/{}", "t".repeat(96));
        entries.push(DirectoryEntry { name, inode });
        namespace.nodes.insert(
            inode,
            NodeMetadata {
                stats: Stats {
                    dev: 1,
                    ino: inode,
                    mode: S_IFLNK | 0o777,
                    nlink: 1,
                    uid: 0,
                    gid: 0,
                    rdev: 0,
                    size: target.len() as u64,
                    blksize: 4096,
                    blocks: 1,
                    atime_ms: 1,
                    mtime_ms: 1,
                    ctime_ms: 1,
                    birthtime_ms: 1,
                },
                data: NodeData::Symlink { target },
            },
        );
    }

    let root_node = namespace.nodes.get_mut(&root).unwrap();
    let NodeData::Directory {
        entries: root_entries,
    } = &mut root_node.data
    else {
        unreachable!("empty namespace root is a directory");
    };
    root_entries.extend(entries);
    namespace
}

#[derive(Debug)]
struct DeterministicLeaseOracle {
    now_ms: AtomicU64,
}

impl DeterministicLeaseOracle {
    fn new(now_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(now_ms),
        }
    }

    fn advance_ms(&self, amount: u64) {
        self.now_ms.fetch_add(amount, Ordering::SeqCst);
    }
}

#[async_trait::async_trait]
impl LeaseOracle for DeterministicLeaseOracle {
    async fn now_ms(&self) -> Result<u64> {
        Ok(self.now_ms.load(Ordering::SeqCst))
    }
}

async fn verify_shared_authority(cluster_file: &str, prefix: &str) -> Result<()> {
    let authority_prefix = format!("{prefix}/shared-authority");
    let volume_prefix = format!("{prefix}/shared-authority-volume");
    let authority = FoundationDbLeaseAuthority::connect(
        cluster_file,
        &authority_prefix,
        FoundationDbLimits::default(),
    )?;
    let reader_a = authority.shared_oracle();
    let reader_b = FoundationDbSharedLeaseOracle::connect(
        cluster_file,
        &authority_prefix,
        FoundationDbLimits::default(),
    )?;
    assert_eq!(
        reader_a.authority_kind(),
        LeaseAuthorityKind::SharedProvider
    );

    let unavailable = reader_a
        .now_ms()
        .await
        .expect_err("a reader must fail closed before authority recovery");
    assert_eq!(unavailable.code, ErrorCode::Enotsup);

    assert_eq!(
        authority
            .publish_now_ms_with_max_forward_jump(2_000_000, AUTHORITY_MAX_FORWARD_JUMP)
            .await?,
        2_000_000
    );
    assert_eq!(reader_a.now_ms().await?, 2_000_000);
    assert_eq!(reader_b.now_ms().await?, 2_000_000);

    // Both independent storage handles consume the same published time. A
    // local clock skew on either host cannot move the shared oracle because the
    // reader path never proposes or writes a time value.
    let first = FoundationDbStorage::connect(
        cluster_file,
        FoundationDbStorageOptions::new(&volume_prefix)
            .with_production_lease_oracle(reader_a.clone()),
    )?;
    let second = FoundationDbStorage::connect(
        cluster_file,
        FoundationDbStorageOptions::new(&volume_prefix)
            .with_production_lease_oracle(reader_b.clone()),
    )?;
    let first_lease = first
        .metadata()
        .acquire_writer("shared-authority-first", Duration::from_secs(30))
        .await?;
    let busy = second
        .metadata()
        .acquire_writer("shared-authority-second", Duration::from_secs(30))
        .await
        .expect_err("the shared authority must protect the live writer lease");
    assert_eq!(busy.code, ErrorCode::Eagain);

    // A backward authority sample is clamped by the durable record. Advancing
    // the authority then expires the old lease for both readers, and the old
    // writer's renewal remains fenced after replacement.
    assert_eq!(
        authority
            .publish_now_ms_with_max_forward_jump(1, AUTHORITY_MAX_FORWARD_JUMP)
            .await?,
        2_000_000
    );
    assert_eq!(reader_b.now_ms().await?, 2_000_000);
    assert_eq!(
        authority
            .publish_now_ms_with_max_forward_jump(2_030_001, AUTHORITY_MAX_FORWARD_JUMP)
            .await?,
        2_030_001
    );
    assert_eq!(reader_a.now_ms().await?, 2_030_001);
    let replacement = second
        .metadata()
        .acquire_writer("shared-authority-second", Duration::from_secs(5))
        .await?;
    assert!(replacement.fence > first_lease.fence);
    let stale = first
        .metadata()
        .renew_writer(&first_lease, Duration::from_secs(5))
        .await
        .expect_err("the old shared-authority writer must be fenced");
    assert_eq!(stale.code, ErrorCode::Estale);
    second.metadata().release_writer(&replacement).await?;

    let authority_stats = authority.stats();
    assert_eq!(authority_stats.publication_attempts, 3);
    assert_eq!(authority_stats.publication_successes, 3);
    assert_eq!(authority_stats.publication_failures, 0);
    assert_eq!(authority_stats.last_published_time_ms, Some(2_030_001));
    assert!(authority_stats.last_success_at_ms.is_some());

    let reader_stats = reader_a.stats();
    assert_eq!(
        reader_stats.read_attempts,
        reader_stats.read_successes + reader_stats.read_failures
    );
    assert!(reader_stats.read_successes > 0);
    assert!(reader_stats.read_failures > 0);
    assert_eq!(reader_stats.last_observed_time_ms, Some(2_030_001));
    assert!(reader_stats.last_success_at_ms.is_some());
    assert!(reader_stats.last_failure_at_ms.is_some());
    println!(
        "FOUNDATIONDB_AUTHORITY_STATS_PASS publication_attempts={} publication_successes={} publication_failures={} reader_attempts={} reader_successes={} reader_failures={} last_published_time_ms={} last_observed_time_ms={}",
        authority_stats.publication_attempts,
        authority_stats.publication_successes,
        authority_stats.publication_failures,
        reader_stats.read_attempts,
        reader_stats.read_successes,
        reader_stats.read_failures,
        authority_stats
            .last_published_time_ms
            .expect("authority stats include a publication"),
        reader_stats
            .last_observed_time_ms
            .expect("reader stats include an observation"),
    );
    drop(first);
    drop(second);
    Ok(())
}

async fn publish_authority_with_bounded_retry(
    authority: &FoundationDbLeaseAuthority,
) -> Result<u64> {
    const MAX_ATTEMPTS: usize = 5;
    const RETRY_DELAY: Duration = Duration::from_secs(2);

    let mut last_error = None;
    for attempt in 0..MAX_ATTEMPTS {
        match authority
            .publish_system_now_ms_with_policy(authority_publication_policy())
            .await
        {
            Ok(published) => return Ok(published),
            Err(error) => {
                last_error = Some(error);
                if attempt + 1 < MAX_ATTEMPTS {
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    }

    Err(last_error.expect("bounded authority publication attempts must record an error"))
}

async fn exercise_real_cluster(cluster_file: &str) -> Result<()> {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_nanos();
    let prefix = env::var("MOUNT_RS_FOUNDATIONDB_TEST_PREFIX")
        .ok()
        .filter(|prefix| !prefix.trim().is_empty())
        .map(|prefix| format!("{prefix}/{}/{}", std::process::id(), unique))
        .unwrap_or_else(|| format!("mount-rs/real-test/{}/{}", std::process::id(), unique));

    // The default is intentionally checked against a live storage handle: a
    // distributed deployment must not accidentally acquire a lease with a
    // process-local wall clock or the persisted single-authority oracle.
    let default_without_clock = FoundationDbStorage::connect(
        cluster_file,
        FoundationDbStorageOptions::new(format!("{prefix}/no-clock-default")),
    )?;
    let error = default_without_clock
        .metadata()
        .acquire_writer("no-clock", Duration::from_secs(5))
        .await
        .expect_err("lease acquisition without an authority must fail closed");
    assert_eq!(error.code, ErrorCode::Enotsup);
    drop(default_without_clock);

    let explicit_without_clock = FoundationDbStorage::connect(
        cluster_file,
        FoundationDbStorageOptions::new(format!("{prefix}/no-clock-explicit"))
            .without_lease_oracle(),
    )?;
    let error = explicit_without_clock
        .metadata()
        .acquire_writer("no-clock-explicit", Duration::from_secs(5))
        .await
        .expect_err("explicitly disabled lease authority must fail closed");
    assert_eq!(error.code, ErrorCode::Enotsup);
    drop(explicit_without_clock);

    verify_shared_authority(cluster_file, &prefix).await?;

    // The persisted FoundationDB oracle is an explicit single-authority/test
    // choice. It is not a production cross-host clock authority, so the
    // ordinary constructor above remains fail closed.
    let storage = FoundationDbStorage::connect(
        cluster_file,
        FoundationDbStorageOptions::new(&prefix).with_persisted_lease_oracle(),
    )?;
    assert!(!storage.metadata().durable());
    assert!(!storage.blocks().durable());

    let blocks = storage.blocks();
    let bytes = b"real FoundationDB block";
    let block_id = blocks.put(bytes).await?;
    assert_eq!(blocks.put(bytes).await?, block_id);
    assert_eq!(blocks.get(&block_id).await?, bytes);

    // Concurrent content-addressed puts conflict on the same key. The
    // production idempotent transaction policy must retry the losing
    // transaction, observe the committed value, and still return one ID.
    let retry_bytes = b"real FoundationDB retry block";
    let (retry_a, retry_b, retry_c, retry_d) = tokio::join!(
        blocks.put(retry_bytes),
        blocks.put(retry_bytes),
        blocks.put(retry_bytes),
        blocks.put(retry_bytes),
    );
    let retry_id = retry_a?;
    for result in [retry_b, retry_c, retry_d] {
        assert_eq!(result?, retry_id);
    }
    assert_eq!(blocks.get(&retry_id).await?, retry_bytes);
    blocks.flush().await?;

    let metadata = storage.metadata();
    assert_eq!(metadata.load().await?.revision, 0);
    let lease = metadata
        .acquire_writer("real-writer", Duration::from_secs(30))
        .await?;
    let namespace = empty_namespace();
    assert_eq!(metadata.publish(0, &lease, namespace.clone()).await?, 1);
    let loaded = metadata.load().await?;
    assert_eq!(loaded.revision, 1);
    assert_eq!(
        serde_json::to_vec(loaded.namespace.as_ref().expect("published namespace")).unwrap(),
        serde_json::to_vec(&namespace).unwrap()
    );

    let large_namespace = multi_chunk_namespace();
    let large_payload = serde_json::to_vec(&large_namespace).unwrap();
    assert!(large_payload.len() > DEFAULT_METADATA_CHUNK_BYTES);
    let large_revision = metadata.publish(1, &lease, large_namespace.clone()).await?;
    assert_eq!(large_revision, 2);
    let loaded_large = metadata.load().await?;
    assert_eq!(loaded_large.revision, large_revision);
    assert_eq!(
        serde_json::to_vec(&loaded_large.namespace.expect("multi-shard namespace")).unwrap(),
        large_payload
    );

    // Publishing a smaller namespace after a multi-shard value also checks
    // that the old tail shards are removed before the new manifest commits.
    let compact_revision = metadata
        .publish(large_revision, &lease, namespace.clone())
        .await?;
    assert_eq!(compact_revision, 3);
    assert_eq!(metadata.load().await?.revision, compact_revision);

    let conflict = metadata
        .publish(0, &lease, empty_namespace())
        .await
        .expect_err("a stale metadata revision must conflict");
    assert_eq!(conflict.code, ErrorCode::Eagain);

    metadata.release_writer(&lease).await?;
    let second = metadata
        .acquire_writer("second-writer", Duration::from_secs(30))
        .await?;
    assert!(second.fence > lease.fence);
    let stale = metadata
        .publish(1, &lease, empty_namespace())
        .await
        .expect_err("a released fence must not publish");
    assert_eq!(stale.code, ErrorCode::Estale);
    metadata.release_writer(&second).await?;

    // Use the public oracle seam to make the expiry boundary deterministic:
    // slow service/client setup must not accidentally consume a short lease.
    // The lease and fence records are still read and mutated by the real
    // FoundationDB provider.
    let lease_clock = Arc::new(DeterministicLeaseOracle::new(1_000_000));
    let oracle: Arc<dyn LeaseOracle> = lease_clock.clone();
    let lease_storage = FoundationDbStorage::connect(
        cluster_file,
        FoundationDbStorageOptions::new(format!("{prefix}/lease-expiry"))
            .with_oracle_arc(Arc::clone(&oracle)),
    )?;
    let lease_metadata = lease_storage.metadata();
    let initial = lease_metadata
        .acquire_writer("expiring-writer", Duration::from_secs(30))
        .await?;
    let renewed = lease_metadata
        .renew_writer(&initial, Duration::from_secs(60))
        .await?;
    assert_eq!(renewed.fence, initial.fence);
    assert!(renewed.expires_at_ms > initial.expires_at_ms);
    lease_clock.advance_ms(60_000);
    let replacement = lease_metadata
        .acquire_writer("replacement-writer", Duration::from_secs(5))
        .await?;
    assert!(replacement.fence > renewed.fence);
    let stale_renew = lease_metadata
        .renew_writer(&renewed, Duration::from_secs(5))
        .await
        .expect_err("an expired and replaced lease must be fenced");
    assert_eq!(stale_renew.code, ErrorCode::Estale);
    lease_metadata.release_writer(&replacement).await?;
    drop(lease_storage);

    blocks.delete(&block_id).await?;
    blocks.delete(&retry_id).await?;
    let missing = blocks
        .get(&block_id)
        .await
        .expect_err("deleted block must not be readable");
    assert_eq!(missing.code, ErrorCode::Enoent);
    Ok(())
}

#[test]
fn authority_publication_policy_matches_the_production_contract() {
    let policy = authority_publication_policy();
    assert!(policy.publication_interval < policy.lease_ttl);
    assert!(policy.max_forward_jump <= policy.lease_ttl);
    assert!(
        LeasePublicationPolicy::new(
            Duration::from_secs(30),
            Duration::from_secs(30),
            Duration::from_secs(5),
        )
        .is_err()
    );
    println!(
        "FOUNDATIONDB_LEASE_PUBLICATION_POLICY_PASS lease_ttl_ms={} publication_interval_ms={} max_forward_jump_ms={}",
        policy.lease_ttl.as_millis(),
        policy.publication_interval.as_millis(),
        policy.max_forward_jump.as_millis(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn publish_foundationdb_authority_for_consumers() {
    let Some(cluster_file) = configured_cluster_file() else {
        eprintln!(
            "SKIP publish_foundationdb_authority_for_consumers: set MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE or MOUNT_RS_FOUNDATIONDB_USE_DEFAULT=1"
        );
        return;
    };
    let Some(prefix) = env::var("MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX")
        .ok()
        .filter(|prefix| !prefix.trim().is_empty())
    else {
        eprintln!(
            "SKIP publish_foundationdb_authority_for_consumers: set MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX"
        );
        return;
    };
    let authority =
        FoundationDbLeaseAuthority::connect(&cluster_file, &prefix, FoundationDbLimits::default())
            .expect("connect FoundationDB consumer-gate authority");
    // A restarted FoundationDB process can report status before its client
    // transaction path is ready. Publication is an idempotent monotonic
    // operation, so retry it in a bounded window instead of turning that
    // readiness race into a false restart failure.
    let published = tokio::time::timeout(
        Duration::from_secs(60),
        publish_authority_with_bounded_retry(&authority),
    )
    .await
    .expect("FoundationDB authority publication exceeded its bounded timeout")
    .expect("publish FoundationDB consumer-gate authority time");
    assert!(published > 0, "published authority time must be non-zero");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs as the bounded authority service in the composed qualification"]
async fn foundationdb_authority_heartbeat() {
    let cluster_file = configured_cluster_file()
        .expect("set MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE for the authority heartbeat");
    let prefix = env::var("MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX")
        .expect("set MOUNT_RS_FOUNDATIONDB_AUTHORITY_PREFIX for the authority heartbeat");
    let duration_seconds = env::var("MOUNT_RS_FOUNDATIONDB_AUTHORITY_HEARTBEAT_SECONDS")
        .expect("set MOUNT_RS_FOUNDATIONDB_AUTHORITY_HEARTBEAT_SECONDS")
        .parse::<u64>()
        .expect("authority heartbeat duration must be an integer");
    assert!(
        duration_seconds > 0,
        "authority heartbeat duration must be positive"
    );

    let authority =
        FoundationDbLeaseAuthority::connect(&cluster_file, &prefix, FoundationDbLimits::default())
            .expect("connect FoundationDB authority heartbeat");
    let policy = authority_publication_policy();
    let first = publish_authority_with_bounded_retry(&authority)
        .await
        .expect("publish initial FoundationDB authority heartbeat sample");
    println!(
        "FOUNDATIONDB_AUTHORITY_HEARTBEAT_READY published_ms={first} interval_ms={} max_forward_jump_ms={} duration_seconds={duration_seconds}",
        policy.publication_interval.as_millis(),
        policy.max_forward_jump.as_millis(),
    );

    let deadline = Instant::now() + Duration::from_secs(duration_seconds);
    let mut publications = 1_u64;
    while Instant::now() < deadline {
        tokio::time::sleep(policy.publication_interval).await;
        if Instant::now() >= deadline {
            break;
        }
        publish_authority_with_bounded_retry(&authority)
            .await
            .expect("publish FoundationDB authority heartbeat sample");
        publications += 1;
    }
    println!(
        "FOUNDATIONDB_AUTHORITY_HEARTBEAT_PASS publications={publications} interval_ms={} max_forward_jump_ms={}",
        policy.publication_interval.as_millis(),
        policy.max_forward_jump.as_millis(),
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn foundationdb_real_cluster_contract() {
    let Some(cluster_file) = configured_cluster_file() else {
        eprintln!(
            "SKIP foundationdb_real_cluster_contract: set MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE \
             or MOUNT_RS_FOUNDATIONDB_USE_DEFAULT=1"
        );
        return;
    };

    let result = tokio::time::timeout(
        Duration::from_secs(60),
        exercise_real_cluster(&cluster_file),
    )
    .await
    .expect("FoundationDB integration exceeded its bounded test timeout");
    result.expect("FoundationDB real-cluster contract failed");
}

#[tokio::test]
async fn concurrent_backing_keyspace_identity_and_wrong_backing_fence() {
    let Some(cluster_file) = configured_cluster_file() else {
        eprintln!(
            "SKIP concurrent_backing_keyspace_identity_and_wrong_backing_fence: no real cluster configured"
        );
        return;
    };
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_nanos();
    let prefix = format!(
        "mount-rs/concurrent-backing/{}/{unique}",
        std::process::id()
    );
    let prefix_a = format!("{prefix}/a");
    let a = FoundationDbStorage::connect(&cluster_file, FoundationDbStorageOptions::new(&prefix_a))
        .expect("open first handle on A");
    let a_second =
        FoundationDbStorage::connect(&cluster_file, FoundationDbStorageOptions::new(&prefix_a))
            .expect("open independent second handle on A");
    let b = FoundationDbStorage::connect(
        &cluster_file,
        FoundationDbStorageOptions::new(format!("{prefix}/b")),
    )
    .expect("open handle on B");
    let blocks_a = a.blocks();
    let blocks_a_second = a_second.blocks();
    let blocks_b = b.blocks();
    let id_a = blocks_a
        .prepare_concurrent_backing()
        .await
        .expect("create block authority in A's actual keyspace");
    assert_eq!(
        blocks_a_second.prepare_concurrent_backing().await.unwrap(),
        id_a,
        "independently opened clients on one prefix share one authority"
    );
    let id_b = blocks_b.prepare_concurrent_backing().await.unwrap();
    assert_ne!(id_a, id_b, "a different prefix needs its own authority");
    let migration_bytes = b"FoundationDB migration must read this exact content block";
    let digest = blocks_a.put(migration_bytes).await.unwrap();
    assert_eq!(
        blocks_a.get_for_migration(&digest).await.unwrap(),
        migration_bytes,
        "migration reads the persisted content block"
    );
    let mut corrupt_key = prefix_a.as_bytes().to_vec();
    corrupt_key.extend_from_slice(b"\0block/");
    corrupt_key.extend_from_slice(digest.0.as_bytes());
    let mut corrupt_bytes = migration_bytes.to_vec();
    corrupt_bytes[0] ^= 1;
    assert_eq!(corrupt_bytes.len(), migration_bytes.len());
    let db = foundationdb::Database::from_path(&cluster_file).unwrap();
    let trx = db.create_trx().unwrap();
    trx.set(&corrupt_key, &corrupt_bytes);
    trx.commit().await.unwrap();
    assert_eq!(
        blocks_a.get(&digest).await.unwrap_err().code,
        ErrorCode::Eio,
        "ordinary reads must reject same-length content-addressed block corruption"
    );
    assert_eq!(
        blocks_a.get_for_migration(&digest).await.unwrap_err().code,
        ErrorCode::Eio,
        "migration must reject a corrupted content-addressed block"
    );
    blocks_a_second
        .verify_concurrent_backing(id_a)
        .await
        .unwrap();
    assert_eq!(
        blocks_a
            .verify_concurrent_backing(id_b)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Estale
    );

    let metadata = a.metadata();
    metadata.prepare_bound_concurrent_mode(id_a).await.unwrap();
    assert_eq!(
        metadata.concurrent_mode_state().await.unwrap(),
        ConcurrentModeState::Mrc2(id_a)
    );
    assert_eq!(
        a_second
            .metadata()
            .prepare_bound_concurrent_mode(id_b)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Estale,
        "wrong blocks must fail before opening the metadata volume"
    );
    assert_eq!(
        metadata
            .publish_bound_if_revision(id_b, 0, empty_namespace())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Estale,
        "wrong blocks must fail before a revision publication"
    );
    assert_eq!(metadata.load().await.unwrap().revision, 0);
    assert_eq!(
        metadata
            .publish_bound_if_revision(id_a, 0, empty_namespace())
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        metadata
            .publish_bound_if_revision(id_b, 0, empty_namespace())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Estale,
        "backing mismatch takes precedence over stale revision"
    );
    assert_eq!(metadata.load().await.unwrap().revision, 1);

    // Two independently opened bound writers race on the same manifest.
    // Exactly one can commit revision 2; the loser sees a known CAS conflict.
    let second_metadata = a_second.metadata();
    let (first, second) = tokio::join!(
        metadata.publish_bound_if_revision(id_a, 1, empty_namespace()),
        second_metadata.publish_bound_if_revision(id_a, 1, empty_namespace()),
    );
    let outcomes = [first, second];
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Ok(2)))
            .count(),
        1,
        "only one bound writer may commit the next revision"
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|result| matches!(result, Err(error) if error.code == ErrorCode::Eagain))
            .count(),
        1,
        "the losing bound writer must get a known revision conflict"
    );
    assert_eq!(second_metadata.load().await.unwrap().revision, 2);
}

fn mrc1_fixture_key(prefix: &str, suffix: &[u8]) -> Vec<u8> {
    let mut key = prefix.as_bytes().to_vec();
    key.push(0);
    key.extend_from_slice(suffix);
    key
}

async fn seed_mrc1_fixture(
    cluster_file: &str,
    prefix: &str,
    namespace: &Namespace,
) -> (Vec<u8>, Vec<u8>) {
    // Recreate an old client's committed MRC1 records without exposing an
    // unbound writer on the current provider trait.
    let payload = serde_json::to_vec(namespace).unwrap();
    assert!(payload.len() <= DEFAULT_METADATA_CHUNK_BYTES);
    let mut manifest = b"MRM1".to_vec();
    manifest.extend_from_slice(&1_u64.to_be_bytes());
    manifest.extend_from_slice(&1_u32.to_be_bytes());
    manifest.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    let mut chunk_key = mrc1_fixture_key(prefix, b"meta/chunk/");
    chunk_key.extend_from_slice(&0_u32.to_be_bytes());
    let db = foundationdb::Database::from_path(cluster_file).unwrap();
    let trx = db.create_trx().unwrap();
    trx.set(&mrc1_fixture_key(prefix, b"meta/write-mode"), b"MRC1");
    trx.set(
        &mrc1_fixture_key(prefix, b"meta/fence"),
        b"MRCF\0\0\0\0\0\0\0\0",
    );
    trx.set(&mrc1_fixture_key(prefix, b"meta/manifest"), &manifest);
    trx.set(&chunk_key, &payload);
    trx.commit().await.unwrap();
    (manifest, payload)
}

#[tokio::test]
async fn concurrent_backing_mrc1_migration_keeps_manifest_and_fences_old_publisher() {
    let Some(cluster_file) = configured_cluster_file() else {
        eprintln!(
            "SKIP concurrent_backing_mrc1_migration_keeps_manifest_and_fences_old_publisher: no real cluster configured"
        );
        return;
    };
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_nanos();
    let prefix = format!(
        "mount-rs/concurrent-migration/{}/{unique}",
        std::process::id()
    );
    let storage =
        FoundationDbStorage::connect(&cluster_file, FoundationDbStorageOptions::new(&prefix))
            .expect("open isolated MRC1 fixture");
    let metadata = storage.metadata();
    let namespace = empty_namespace();
    let (manifest, payload) = seed_mrc1_fixture(&cluster_file, &prefix, &namespace).await;
    assert_eq!(
        metadata.concurrent_mode_state().await.unwrap(),
        ConcurrentModeState::Mrc1
    );
    assert_eq!(metadata.load().await.unwrap().revision, 1);
    let backing = storage.blocks().prepare_concurrent_backing().await.unwrap();
    assert_eq!(
        metadata
            .prepare_bound_concurrent_mode(backing)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Ebusy,
        "normal prepare cannot silently convert MRC1"
    );
    assert_eq!(
        metadata
            .migrate_mrc1_to_bound_mode(backing, 0)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Eagain,
        "a revision mismatch must leave MRC1 unchanged"
    );
    assert_eq!(
        metadata.concurrent_mode_state().await.unwrap(),
        ConcurrentModeState::Mrc1
    );
    metadata
        .migrate_mrc1_to_bound_mode(backing, 1)
        .await
        .unwrap();
    assert_eq!(
        metadata.concurrent_mode_state().await.unwrap(),
        ConcurrentModeState::Mrc2(backing)
    );
    let loaded = metadata.load().await.unwrap();
    assert_eq!(loaded.revision, 1);
    assert_eq!(
        serde_json::to_vec(&loaded.namespace.unwrap()).unwrap(),
        payload,
        "migration must retain the exact published namespace"
    );
    let db = foundationdb::Database::from_path(&cluster_file).unwrap();
    let trx = db.create_trx().unwrap();
    let mut chunk_key = mrc1_fixture_key(&prefix, b"meta/chunk/");
    chunk_key.extend_from_slice(&0_u32.to_be_bytes());
    assert_eq!(
        trx.get(&mrc1_fixture_key(&prefix, b"meta/manifest"), false)
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        manifest.as_slice(),
        "migration must leave the manifest bytes unchanged"
    );
    assert_eq!(
        trx.get(&chunk_key, false).await.unwrap().unwrap().as_ref(),
        payload.as_slice(),
        "migration must leave the namespace chunk bytes unchanged"
    );
    assert_eq!(
        trx.get(&mrc1_fixture_key(&prefix, b"meta/fence"), false)
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        b"MRCF\0\0\0\0\0\0\0\0",
        "migration must keep the old-client fence sentinel"
    );
    assert_eq!(
        trx.get(&mrc1_fixture_key(&prefix, b"meta/write-mode"), false)
            .await
            .unwrap()
            .unwrap()
            .as_ref(),
        b"MRC2",
        "the persisted mode must reject an old client's exact MRC1 decoder"
    );
    assert_eq!(metadata.load().await.unwrap().revision, 1);
    assert_eq!(
        metadata
            .publish_bound_if_revision(backing, 1, namespace)
            .await
            .unwrap(),
        2
    );
}
