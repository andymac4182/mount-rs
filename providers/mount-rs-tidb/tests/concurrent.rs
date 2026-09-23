//! Opt-in MRC2 contracts against an actual TiDB endpoint.
//!
//! The persistent contract uses the harness volume key with a separate suffix.
//! Its first invocation leaves a namespace and immutable block for a fresh
//! invocation with MOUNT_RS_TIDB_EXPECT_PERSISTED=1 after the cluster restart.

use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{
    BlockId, BlockStore, ConcurrentBackingId, ConcurrentModeState, MetadataStore, Namespace,
    NodeData, NodeMetadata, WriterLease,
};
use mount_rs_core::{ErrorCode, FsDriver};
use mount_rs_memfs::MemoryFs;
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PERSISTED_BYTES: &[u8] = b"mount-rs TiDB MRC2 persisted block sentinel";
const PERSISTED_UID: u32 = 12345;
type RawPersistedMetadata = (i64, Option<Vec<u8>>, Option<Vec<u8>>);

fn tidb_url() -> String {
    std::env::var("MOUNT_RS_TIDB_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .expect("MOUNT_RS_TIDB_URL is required for the ignored actual TiDB contract")
}

fn volume_key(suffix: &str) -> String {
    let base = std::env::var("MOUNT_RS_TIDB_TEST_VOLUME_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .unwrap_or_else(|| {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_nanos();
            format!("mount-rs-tidb-{}-{timestamp}", std::process::id())
        });
    format!("{base}-{suffix}")
}

fn persisted_run() -> bool {
    std::env::var("MOUNT_RS_TIDB_EXPECT_PERSISTED").as_deref() == Ok("1")
}

fn transient_key(suffix: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after Unix epoch")
        .as_nanos();
    volume_key(&format!("{suffix}-{timestamp}"))
}

fn block_id(bytes: &[u8]) -> BlockId {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(65);
    encoded.push('b');
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    BlockId(encoded)
}

async fn assert_actual_tidb(url: &str) {
    let pool = Pool::from_url(url).expect("parse TiDB identity URL");
    let mut connection = pool.get_conn().await.expect("connect for TiDB identity");
    let (tidb, version): (String, String) = connection
        .query_first("SELECT tidb_version(), VERSION()")
        .await
        .expect("query actual TiDB identity")
        .expect("TiDB identity row");
    assert!(
        format!("{tidb} {version}")
            .to_ascii_lowercase()
            .contains("tidb")
    );
    println!("TIDB_CONCURRENT_IDENTITY tidb_version={tidb} version={version}");
    drop(connection);
    pool.disconnect().await.expect("close identity pool");
}

async fn restore_global_autocommit(url: &str, original: u8) -> mount_rs_core::Result<()> {
    // Use a fresh connection because the SET acknowledgement may have been
    // lost and the original admin connection may be unusable. Restoring an
    // owned global value is idempotent, unlike replaying a publication.
    tokio::time::timeout(Duration::from_secs(30), async {
        let pool = Pool::from_url(url)
            .map_err(|_| mount_rs_core::FsError::backend("parse global restoration URL"))?;
        let mut admin = pool.get_conn().await.map_err(|error| {
            mount_rs_core::FsError::backend(format!("connect global restoration: {error}"))
        })?;
        admin
            .query_drop("SET SESSION autocommit=1")
            .await
            .map_err(|error| {
                mount_rs_core::FsError::backend(format!(
                    "set restoration session autocommit: {error}"
                ))
            })?;
        admin
            .exec_drop("SET GLOBAL autocommit=?", (original,))
            .await
            .map_err(|error| {
                mount_rs_core::FsError::backend(format!("restore global autocommit: {error}"))
            })?;
        let observed: Option<u8> = admin
            .query_first("SELECT @@GLOBAL.autocommit")
            .await
            .map_err(|error| {
                mount_rs_core::FsError::backend(format!("verify global restoration: {error}"))
            })?;
        if observed != Some(original) {
            return Err(mount_rs_core::FsError::backend(
                "original global autocommit was not restored",
            ));
        }
        drop(admin);
        pool.disconnect().await.map_err(|error| {
            mount_rs_core::FsError::backend(format!("disconnect restoration pool: {error}"))
        })?;
        Ok(())
    })
    .await
    .map_err(|_| {
        mount_rs_core::FsError::backend("global autocommit restoration exceeded 30 seconds")
    })?
}

async fn root_namespace(uid: u32) -> Namespace {
    let stats = MemoryFs::empty().stat("/").await.expect("root stat");
    let root = stats.ino;
    Namespace {
        format_version: 1,
        root,
        next_inode: root + 1,
        default_uid: uid,
        default_gid: 0,
        umask: 0o022,
        default_chunker: FixedSizeChunker::new(4096).expect("fixed chunker").config(),
        nodes: BTreeMap::from([(
            root,
            NodeMetadata {
                stats,
                data: NodeData::Directory { entries: vec![] },
            },
        )]),
    }
}

async fn assert_persisted(
    metadata: &TidbMetadataStore,
    blocks: &TidbBlockStore,
) -> ConcurrentBackingId {
    let loaded = metadata
        .load()
        .await
        .expect("read independently persisted namespace");
    assert_eq!(
        loaded.revision, 2,
        "both acknowledged CAS publications must persist"
    );
    assert_eq!(
        loaded.namespace.expect("persisted namespace").default_uid,
        PERSISTED_UID
    );
    let backing = match metadata
        .concurrent_mode_state()
        .await
        .expect("persisted MRC2 mode")
    {
        ConcurrentModeState::Mrc2(backing) => backing,
        state => panic!("persisted metadata was not MRC2: {state:?}"),
    };
    blocks
        .verify_concurrent_backing(backing)
        .await
        .expect("persisted backing marker");
    assert_eq!(
        blocks
            .get(&block_id(PERSISTED_BYTES))
            .await
            .expect("persisted block"),
        PERSISTED_BYTES
    );
    assert_eq!(
        blocks
            .get_for_migration(&block_id(PERSISTED_BYTES))
            .await
            .expect("direct persisted migration read"),
        PERSISTED_BYTES
    );
    backing
}

#[tokio::test]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_bound_concurrent_contract_persists() {
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let key = volume_key("concurrent-mrc2");
    let options = TidbStorageOptions::new(&key).with_durable(true);
    let metadata_a = TidbMetadataStore::connect_with_options(&url, options.clone())
        .await
        .expect("connect independent metadata A");
    let metadata_b = TidbMetadataStore::connect_with_options(&url, options.clone())
        .await
        .expect("connect independent metadata B");
    let blocks_a = TidbBlockStore::connect_with_options(&url, options.clone())
        .await
        .expect("connect independent blocks A");
    let blocks_b = TidbBlockStore::connect_with_options(&url, options.clone())
        .await
        .expect("connect independent blocks B");
    if persisted_run() {
        let left = assert_persisted(&metadata_a, &blocks_a).await;
        let right = assert_persisted(&metadata_b, &blocks_b).await;
        assert_eq!(left, right);
        println!(
            "TIDB_CONCURRENT_PERSISTED volume_key={key} revision=2 backing={}",
            left.to_hex()
        );
    } else {
        assert_eq!(
            metadata_a.load().await.expect("initial metadata").revision,
            0
        );
        // This is the first contract check: the existing provider must fail
        // here with ENOTSUP before any marker or publication is written.
        metadata_a
            .preflight_new_bound_mode()
            .await
            .expect("read-only new MRC2 enrollment preflight");
        assert_eq!(
            metadata_a
                .concurrent_mode_state()
                .await
                .expect("mode after preflight"),
            ConcurrentModeState::Legacy
        );
        assert!(
            metadata_a
                .load()
                .await
                .expect("read-only preflight left namespace empty")
                .namespace
                .is_none()
        );

        let (left, right) = tokio::join!(
            blocks_a.prepare_concurrent_backing(),
            blocks_b.prepare_concurrent_backing()
        );
        let backing = left.expect("prepare backing A");
        assert_eq!(
            backing,
            right.expect("prepare backing B"),
            "racing independent connections must retain one stable backing"
        );
        let wrong = ConcurrentBackingId::from_bytes(if backing.as_bytes() == [1; 16] {
            [2; 16]
        } else {
            [1; 16]
        })
        .expect("nonzero wrong backing");
        assert!(
            blocks_a
                .verify_concurrent_backing(wrong)
                .await
                .expect_err("wrong blocks authority")
                .is(ErrorCode::Estale)
        );
        let (left, right) = tokio::join!(
            metadata_a.prepare_bound_concurrent_mode(backing),
            metadata_b.prepare_bound_concurrent_mode(backing)
        );
        left.expect("claim bound mode A");
        right.expect("idempotent racing bound mode B");
        assert_eq!(
            metadata_b
                .concurrent_mode_state()
                .await
                .expect("claimed mode"),
            ConcurrentModeState::Mrc2(backing)
        );
        assert!(
            metadata_a
                .prepare_bound_concurrent_mode(wrong)
                .await
                .expect_err("mismatched metadata authority")
                .is(ErrorCode::Estale)
        );
        assert!(metadata_a.preflight_new_bound_mode().await.is_err());
        assert!(
            metadata_a
                .acquire_writer("legacy-client", Duration::from_secs(30))
                .await
                .is_err()
        );
        let obsolete = WriterLease {
            owner: "legacy-client".into(),
            fence: 1,
            expires_at_ms: 1,
        };
        assert!(
            metadata_a
                .renew_writer(&obsolete, Duration::from_secs(30))
                .await
                .expect_err("legacy renewal fenced")
                .is(ErrorCode::Estale)
        );
        assert!(
            metadata_a
                .release_writer(&obsolete)
                .await
                .expect_err("legacy release fenced")
                .is(ErrorCode::Estale)
        );

        let namespace_a = root_namespace(10).await;
        let namespace_b = root_namespace(11).await;
        assert!(
            metadata_a
                .publish(0, &obsolete, namespace_a.clone())
                .await
                .expect_err("legacy publication fenced")
                .is(ErrorCode::Estale)
        );
        assert!(
            metadata_a
                .publish_bound_if_revision(wrong, 0, namespace_a.clone())
                .await
                .expect_err("wrong backing cannot publish")
                .is(ErrorCode::Estale)
        );
        let (left, right) = tokio::join!(
            metadata_a.publish_bound_if_revision(backing, 0, namespace_a),
            metadata_b.publish_bound_if_revision(backing, 0, namespace_b)
        );
        match (left, right) {
            (Ok(1), Err(error)) | (Err(error), Ok(1)) => assert!(error.is(ErrorCode::Eagain)),
            outcome => panic!("exactly one independent revision-zero CAS must commit: {outcome:?}"),
        }
        assert_eq!(
            metadata_a.load().await.expect("read race winner").revision,
            1
        );
        let id = blocks_a
            .put(PERSISTED_BYTES)
            .await
            .expect("put persistent immutable block");
        assert_eq!(id, block_id(PERSISTED_BYTES));
        blocks_a
            .flush()
            .await
            .expect("block acknowledgement barrier");
        let sentinel = root_namespace(PERSISTED_UID).await;
        assert_eq!(
            metadata_b
                .publish_bound_if_revision(backing, 1, sentinel)
                .await
                .expect("publish persistent namespace sentinel"),
            2
        );
        metadata_b
            .flush()
            .await
            .expect("metadata acknowledgement barrier");

        // Reconnect through new pools before restart; neither loaded state nor
        // authority may depend on connection-local caches.
        let fresh_metadata = TidbMetadataStore::connect_with_options(&url, options.clone())
            .await
            .expect("fresh metadata reconnect");
        let fresh_blocks = TidbBlockStore::connect_with_options(&url, options)
            .await
            .expect("fresh blocks reconnect");
        assert_eq!(
            assert_persisted(&fresh_metadata, &fresh_blocks).await,
            backing
        );
        fresh_metadata.close().await.expect("close fresh metadata");
        fresh_blocks.close().await.expect("close fresh blocks");
        println!(
            "TIDB_CONCURRENT_INITIAL volume_key={key} revision=2 backing={}",
            backing.to_hex()
        );
    }
    metadata_a.close().await.expect("close metadata A");
    metadata_b.close().await.expect("close metadata B");
    blocks_a.close().await.expect("close blocks A");
    blocks_b.close().await.expect("close blocks B");
}

#[tokio::test]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_bound_claim_rechecks_legacy_writer_and_history() {
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let key = transient_key("concurrent-legacy-race");
    let options = TidbStorageOptions::new(&key);
    let metadata_a = TidbMetadataStore::connect_with_options(&url, options.clone())
        .await
        .expect("metadata A");
    let metadata_b = TidbMetadataStore::connect_with_options(&url, options)
        .await
        .expect("metadata B");
    metadata_a
        .preflight_new_bound_mode()
        .await
        .expect("initial read-only preflight");
    let lease = metadata_b
        .acquire_writer("racing-legacy-writer", Duration::from_secs(30))
        .await
        .expect("legacy writer enters after preflight");
    let backing = ConcurrentBackingId::from_bytes([7; 16]).expect("test backing");
    assert!(
        metadata_a
            .prepare_bound_concurrent_mode(backing)
            .await
            .expect_err("live writer must defeat stale preflight")
            .is(ErrorCode::Ebusy)
    );
    assert_eq!(
        metadata_a
            .concurrent_mode_state()
            .await
            .expect("unchanged mode"),
        ConcurrentModeState::Legacy
    );
    metadata_b
        .release_writer(&lease)
        .await
        .expect("release raced legacy writer");
    assert!(
        metadata_a
            .preflight_new_bound_mode()
            .await
            .expect_err("released lease history is not a never-used store")
            .is(ErrorCode::Ebusy)
    );
    assert!(
        metadata_a
            .prepare_bound_concurrent_mode(backing)
            .await
            .expect_err("released legacy fence remains a claim blocker")
            .is(ErrorCode::Ebusy)
    );
    let lease = metadata_b
        .acquire_writer("published-legacy-writer", Duration::from_secs(30))
        .await
        .expect("legacy store remains writable");
    metadata_b
        .publish(0, &lease, root_namespace(17).await)
        .await
        .expect("legacy publication after failed claim");
    metadata_b
        .release_writer(&lease)
        .await
        .expect("release published writer");
    assert!(
        metadata_a
            .prepare_bound_concurrent_mode(backing)
            .await
            .expect_err("published legacy namespace cannot enroll")
            .is(ErrorCode::Ebusy)
    );
    assert_eq!(
        metadata_a
            .load()
            .await
            .expect("legacy namespace remains intact")
            .namespace
            .expect("published namespace")
            .default_uid,
        17
    );
    metadata_a.close().await.expect("close metadata A");
    metadata_b.close().await.expect("close metadata B");
}

#[tokio::test]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_mrc1_migration_is_revision_guarded_and_read_only_preflight() {
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let key = transient_key("concurrent-migration");
    let metadata = TidbMetadataStore::connect_with_key(&url, &key)
        .await
        .expect("metadata");
    let blocks = TidbBlockStore::connect_with_key(&url, &key)
        .await
        .expect("blocks");
    let backing = blocks
        .prepare_concurrent_backing()
        .await
        .expect("blocks authority");
    let lease = metadata
        .acquire_writer("migration-seed", Duration::from_secs(30))
        .await
        .expect("seed lease");
    metadata
        .publish(0, &lease, root_namespace(29).await)
        .await
        .expect("seed historical namespace");
    metadata
        .release_writer(&lease)
        .await
        .expect("release seed lease");
    let pool = Pool::from_url(&url).expect("raw test URL");
    let mut connection = pool
        .get_conn()
        .await
        .expect("raw migration fixture connection");
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_metadata SET write_mode='MRC1', backing_id=NULL,
             owner=NULL, fence=?, expires=0 WHERE volume_key=?",
            (i64::MAX, &key),
        )
        .await
        .expect("seed documented unbound MRC1 state");
    assert_eq!(
        metadata.concurrent_mode_state().await.expect("MRC1 state"),
        ConcurrentModeState::Mrc1
    );
    assert!(
        metadata
            .prepare_bound_concurrent_mode(backing)
            .await
            .expect_err("normal claim cannot migrate MRC1")
            .is(ErrorCode::Ebusy)
    );
    assert!(
        metadata
            .preflight_mrc1_to_bound_mode(0)
            .await
            .expect_err("wrong revision preflight")
            .is(ErrorCode::Eagain)
    );
    metadata
        .preflight_mrc1_to_bound_mode(1)
        .await
        .expect("read-only MRC1 preflight");
    assert_eq!(
        metadata
            .concurrent_mode_state()
            .await
            .expect("preflight left MRC1 marker unchanged"),
        ConcurrentModeState::Mrc1
    );
    assert!(
        metadata
            .migrate_mrc1_to_bound_mode(backing, 0)
            .await
            .expect_err("wrong revision migration")
            .is(ErrorCode::Eagain)
    );
    metadata
        .migrate_mrc1_to_bound_mode(backing, 1)
        .await
        .expect("atomic revision-guarded migration");
    assert_eq!(
        metadata
            .concurrent_mode_state()
            .await
            .expect("migrated MRC2"),
        ConcurrentModeState::Mrc2(backing)
    );
    let loaded = metadata.load().await.expect("namespace after migration");
    assert_eq!(loaded.revision, 1);
    assert_eq!(
        loaded
            .namespace
            .expect("historical namespace retained")
            .default_uid,
        29
    );
    assert!(
        metadata
            .acquire_writer("obsolete-legacy", Duration::from_secs(30))
            .await
            .is_err()
    );
    metadata.close().await.expect("close metadata");
    blocks.close().await.expect("close blocks");
    drop(connection);
    pool.disconnect().await.expect("close raw fixture pool");
}

#[tokio::test]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_bound_validation_and_authority_corruption_fail_closed() {
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let key = transient_key("concurrent-invalid");
    let metadata = TidbMetadataStore::connect_with_key(&url, &key)
        .await
        .expect("metadata");
    let blocks = TidbBlockStore::connect_with_key(&url, &key)
        .await
        .expect("blocks");
    let backing = blocks
        .prepare_concurrent_backing()
        .await
        .expect("blocks authority");
    metadata
        .prepare_bound_concurrent_mode(backing)
        .await
        .expect("bind metadata");
    let tiny = TidbMetadataStore::connect_with_options(
        &url,
        TidbStorageOptions::new(&key).with_max_namespace_bytes(1),
    )
    .await
    .expect("bounded namespace provider");
    assert!(
        tiny.publish_bound_if_revision(backing, 0, root_namespace(0).await)
            .await
            .expect_err("oversized namespace must not commit")
            .is(ErrorCode::Efbig)
    );
    let mut invalid = root_namespace(0).await;
    invalid.next_inode = 0;
    assert!(
        metadata
            .publish_bound_if_revision(backing, 0, invalid)
            .await
            .is_err()
    );
    assert_eq!(
        metadata.load().await.expect("unchanged revision").revision,
        0
    );
    let pool = Pool::from_url(&url).expect("raw test URL");
    let mut connection = pool.get_conn().await.expect("raw fixture connection");
    let id = blocks
        .put(b"integrity sentinel")
        .await
        .expect("put integrity sentinel");
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_blocks SET bytes=? WHERE volume_key=? AND id=?",
            (b"corrupt bytes".as_slice(), &key, &id.0),
        )
        .await
        .expect("inject block corruption");
    assert!(
        blocks
            .get_for_migration(&id)
            .await
            .expect_err("uncached content-verified read rejects corruption")
            .is(ErrorCode::Eio)
    );
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_block_authority SET backing_id=? WHERE volume_key=?",
            (vec![0xff_u8], &key),
        )
        .await
        .expect("inject invalid blocks marker");
    assert!(
        blocks
            .verify_concurrent_backing(backing)
            .await
            .expect_err("invalid blocks authority cannot verify")
            .is(ErrorCode::Estale)
    );
    assert!(
        blocks
            .prepare_concurrent_backing()
            .await
            .expect_err("invalid marker must not be repaired by preparation")
            .is(ErrorCode::Eio)
    );
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_metadata SET backing_id=? WHERE volume_key=?",
            (vec![0xff_u8], &key),
        )
        .await
        .expect("inject invalid metadata marker");
    assert!(
        metadata
            .concurrent_mode_state()
            .await
            .expect_err("invalid metadata identity")
            .is(ErrorCode::Estale)
    );
    assert!(
        metadata
            .publish_bound_if_revision(backing, 0, root_namespace(0).await)
            .await
            .expect_err("corrupt marker cannot publish")
            .is(ErrorCode::Estale)
    );
    assert_eq!(
        metadata
            .load()
            .await
            .expect("corruption did not publish")
            .revision,
        0
    );
    connection
        .exec_drop(
            "DELETE FROM mount_rs_tidb_block_authority WHERE volume_key=?",
            (&key,),
        )
        .await
        .expect("remove owned blocks marker");
    assert!(
        blocks
            .verify_concurrent_backing(backing)
            .await
            .expect_err("missing marker must not enroll during verification")
            .is(ErrorCode::Estale)
    );
    let marker: Option<String> = connection
        .exec_first(
            "SELECT backing_id FROM mount_rs_tidb_block_authority WHERE volume_key=?",
            (&key,),
        )
        .await
        .expect("inspect absent marker");
    assert!(marker.is_none());
    metadata.close().await.expect("close metadata");
    tiny.close().await.expect("close tiny metadata");
    blocks.close().await.expect("close blocks");
    drop(connection);
    pool.disconnect().await.expect("close raw fixture pool");
}

#[tokio::test]
#[ignore = "requires an owned actual TiDB service and explicit global-setting opt-in"]
async fn actual_tidb_sessions_commit_when_global_autocommit_is_disabled() {
    if std::env::var("MOUNT_RS_TIDB_GLOBAL_AUTOCOMMIT_TEST").as_deref() != Ok("1") {
        println!("TIDB_GLOBAL_AUTOCOMMIT_TEST_SKIPPED explicit_owned_cluster_opt_in_required");
        return;
    }
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let key = transient_key("global-autocommit");
    // Create tables and the empty metadata row before changing the global
    // setting. The regression then exercises only freshly opened sessions.
    let seeded_metadata = TidbMetadataStore::connect_with_key(&url, &key)
        .await
        .expect("seed metadata");
    let seeded_blocks = TidbBlockStore::connect_with_key(&url, &key)
        .await
        .expect("seed blocks");
    seeded_metadata.close().await.expect("close seed metadata");
    seeded_blocks.close().await.expect("close seed blocks");
    let pool = Pool::from_url(&url).expect("owned admin URL");
    let mut admin = pool.get_conn().await.expect("owned admin connection");
    admin
        .query_drop("SET SESSION autocommit=1")
        .await
        .expect("independent reader autocommit");
    let original: u8 = admin
        .query_first("SELECT @@GLOBAL.autocommit")
        .await
        .expect("read original global setting")
        .expect("global autocommit row");
    let disabled = tokio::time::timeout(
        Duration::from_secs(30),
        admin.query_drop("SET GLOBAL autocommit=0"),
    )
    .await;
    let task_url = url.clone();
    let task_key = key.clone();
    // JoinHandle catches assertions/panics too. Restore the global setting
    // before interpreting any operation outcome, including timeout or panic.
    let execution = if matches!(&disabled, Ok(Ok(()))) {
        let mut task = tokio::spawn(async move {
            let metadata = TidbMetadataStore::connect_with_key(&task_url, &task_key).await?;
            let blocks = TidbBlockStore::connect_with_key(&task_url, &task_key).await?;
            let backing = blocks.prepare_concurrent_backing().await?;
            metadata.prepare_bound_concurrent_mode(backing).await?;
            let id = blocks.put(PERSISTED_BYTES).await?;
            metadata
                .publish_bound_if_revision(backing, 0, root_namespace(61).await)
                .await?;
            mount_rs_core::Result::Ok((metadata, blocks, backing, id))
        });
        let result = tokio::time::timeout(Duration::from_secs(30), &mut task).await;
        if result.is_err() {
            task.abort();
            let _ = task.await;
        }
        Some(result)
    } else {
        None
    };
    let restored = restore_global_autocommit(&url, original).await;
    restored.expect("restore original global setting before operation assertions");
    println!("TIDB_GLOBAL_AUTOCOMMIT_RESTORED original={original}");
    disabled
        .expect("disable global autocommit exceeded 30 seconds")
        .expect("disable global autocommit in owned service");
    let (metadata, blocks, backing, id) = execution
        .expect("operations start only after a confirmed setting change")
        .expect("fresh-session operation timeout")
        .expect("fresh-session operation panic")
        .expect("fresh sessions must acknowledge committed operations");
    let persisted: Option<RawPersistedMetadata> = admin.exec_first(
        "SELECT revision, write_mode, backing_id FROM mount_rs_tidb_metadata WHERE volume_key=?", (&key,),
    ).await.expect("read acknowledged publication independently");
    assert_eq!(
        persisted,
        Some((
            1,
            Some(b"MRC2".to_vec()),
            Some(backing.to_hex().into_bytes())
        ))
    );
    let persisted_bytes: Option<Vec<u8>> = admin
        .exec_first(
            "SELECT bytes FROM mount_rs_tidb_blocks WHERE volume_key=? AND id=?",
            (&key, &id.0),
        )
        .await
        .expect("read acknowledged block independently");
    assert_eq!(persisted_bytes.as_deref(), Some(PERSISTED_BYTES));
    metadata.close().await.expect("close tested metadata");
    blocks.close().await.expect("close tested blocks");
    drop(admin);
    pool.disconnect().await.expect("close admin pool");
    println!("TIDB_GLOBAL_AUTOCOMMIT_COMMITTED independent_revision=1");
}
