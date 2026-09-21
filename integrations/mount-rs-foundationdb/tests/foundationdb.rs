#![cfg(feature = "foundationdb")]

use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{
    BlockStore, DirectoryEntry, MetadataStore, Namespace, NodeData, NodeMetadata,
};
use mount_rs_core::types::{S_IFDIR, S_IFLNK, Stats};
use mount_rs_core::{ErrorCode, Result};
use mount_rs_foundationdb::{
    DEFAULT_METADATA_CHUNK_BYTES, FoundationDbLeaseAuthority, FoundationDbLimits,
    FoundationDbSharedLeaseOracle, FoundationDbStorage, FoundationDbStorageOptions,
    LeaseAuthorityKind, LeaseOracle,
};
use std::collections::BTreeMap;
use std::env;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

    assert_eq!(authority.publish_now_ms(2_000_000).await?, 2_000_000);
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
    assert_eq!(authority.publish_now_ms(1).await?, 2_000_000);
    assert_eq!(reader_b.now_ms().await?, 2_000_000);
    assert_eq!(authority.publish_now_ms(2_030_001).await?, 2_030_001);
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
        match authority.publish_system_now_ms().await {
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
