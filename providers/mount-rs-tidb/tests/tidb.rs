//! Opt-in tests against an actual TiDB endpoint.
//!
//! Run with:
//!
//! ```text
//! MOUNT_RS_TIDB_URL='mysql://user:password@127.0.0.1:4000/test' \
//!   cargo test -p mount-rs-tidb --test tidb -- --ignored --nocapture
//! ```
//!
//! The test writes only to the provider's fixed tables under a unique volume
//! key. It is intentionally ignored by default: a MySQL-compatible substitute
//! is not evidence for the TiDB integration gate.

use mount_rs_core::chunking::{Chunker, FixedSizeChunker};
use mount_rs_core::storage::{
    BlockId, BlockStore, MetadataStore, Namespace, NodeData, NodeMetadata,
};
use mount_rs_core::{ErrorCode, FsDriver, S_IFDIR};
use mount_rs_memfs::MemoryFs;
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PERSISTED_BYTES: &[u8] = b"mount-rs TiDB persisted sentinel";

type SchemaColumn = (String, String, String, Option<u64>, Option<String>);

fn tidb_url() -> String {
    std::env::var("MOUNT_RS_TIDB_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| {
            panic!("MOUNT_RS_TIDB_URL must be set when explicitly running the ignored TiDB test")
        })
}

fn unique_volume_key() -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_nanos();
    format!("mount-rs-tidb-test-{}-{timestamp}", std::process::id())
}

fn volume_key() -> String {
    std::env::var("MOUNT_RS_TIDB_TEST_VOLUME_KEY")
        .ok()
        .filter(|key| !key.trim().is_empty())
        .unwrap_or_else(unique_volume_key)
}

fn persisted_run() -> bool {
    std::env::var("MOUNT_RS_TIDB_EXPECT_PERSISTED").as_deref() == Ok("1")
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
    let pool =
        Pool::from_url(url).unwrap_or_else(|_| panic!("the TiDB identity URL could not be parsed"));
    let mut connection = pool
        .get_conn()
        .await
        .unwrap_or_else(|_| panic!("could not connect for the TiDB identity check"));
    let identity: Option<(String, String)> = connection
        .exec_first("SELECT tidb_version(), VERSION()", ())
        .await
        .unwrap_or_else(|_| panic!("SELECT tidb_version(), VERSION() failed"));
    let (tidb_version, version) =
        identity.unwrap_or_else(|| panic!("TiDB identity query returned no row"));
    let identity_text = format!("{tidb_version} {version}").to_ascii_lowercase();
    assert!(
        identity_text.contains("tidb"),
        "server identity did not identify TiDB: tidb_version()={tidb_version:?}, VERSION()={version:?}"
    );
    println!("TIDB_IDENTITY tidb_version={tidb_version} version={version}");
    drop(connection);
    pool.disconnect()
        .await
        .unwrap_or_else(|_| panic!("could not close the TiDB identity connection"));
}

async fn assert_binary_identity_schema(url: &str) {
    let pool = Pool::from_url(url).expect("the TiDB schema URL could not be parsed");
    let mut connection = pool
        .get_conn()
        .await
        .expect("could not connect for the TiDB schema identity check");
    let columns: Vec<SchemaColumn> = connection
        .exec(
            "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, CHARACTER_MAXIMUM_LENGTH, COLLATION_NAME
             FROM information_schema.columns
             WHERE TABLE_SCHEMA=DATABASE()
               AND ((TABLE_NAME='mount_rs_tidb_metadata'
                     AND COLUMN_NAME IN ('volume_key', 'owner', 'write_mode', 'backing_id'))
                    OR (TABLE_NAME='mount_rs_tidb_blocks'
                        AND COLUMN_NAME IN ('volume_key', 'id'))
                    OR (TABLE_NAME='mount_rs_tidb_block_authority'
                        AND COLUMN_NAME IN ('volume_key', 'backing_id')))
             ORDER BY TABLE_NAME, COLUMN_NAME",
            (),
        )
        .await
        .expect("could not inspect the TiDB provider schema");
    for (table, column, expected_length) in [
        ("mount_rs_tidb_metadata", "volume_key", 1020_u64),
        ("mount_rs_tidb_metadata", "owner", 1020_u64),
        ("mount_rs_tidb_metadata", "write_mode", 4_u64),
        ("mount_rs_tidb_metadata", "backing_id", 32_u64),
        ("mount_rs_tidb_blocks", "volume_key", 1020_u64),
        ("mount_rs_tidb_blocks", "id", 65_u64),
        ("mount_rs_tidb_block_authority", "volume_key", 1020_u64),
        ("mount_rs_tidb_block_authority", "backing_id", 32_u64),
    ] {
        let (_, _, data_type, length, collation) = columns
            .iter()
            .find(|(actual_table, actual_column, _, _, _)| {
                actual_table == table && actual_column == column
            })
            .unwrap_or_else(|| panic!("missing TiDB schema column {table}.{column}"));
        assert_eq!(data_type, "varbinary", "{table}.{column} must be VARBINARY");
        assert_eq!(
            length,
            &Some(expected_length),
            "wrong length for {table}.{column}"
        );
        assert!(
            collation.is_none(),
            "{table}.{column} must not use SQL collation identity"
        );
    }
    drop(connection);
    pool.disconnect()
        .await
        .expect("could not close the TiDB schema identity connection");
}

async fn root_namespace() -> Namespace {
    let stats = MemoryFs::empty().stat("/").await.expect("root stat");
    let root = stats.ino;
    assert_eq!(stats.mode & mount_rs_core::S_IFMT, S_IFDIR);
    Namespace {
        format_version: 1,
        root,
        next_inode: root + 1,
        default_uid: 0,
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

#[tokio::test]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_split_store_contract() {
    let url = tidb_url();
    // This runs before either provider constructor creates its tables. A
    // MySQL-compatible server without TiDB's identity function must fail here.
    assert_actual_tidb(&url).await;
    let volume_key = volume_key();
    let options = TidbStorageOptions::new(&volume_key)
        .with_durable(true)
        .with_max_block_bytes(4 * 1024 * 1024)
        .with_max_namespace_bytes(4 * 1024 * 1024);
    let metadata = TidbMetadataStore::connect_with_options(&url, options.clone())
        .await
        .expect("connect TiDB metadata");
    let blocks = TidbBlockStore::connect_with_options(&url, options)
        .await
        .expect("connect TiDB blocks");
    assert_binary_identity_schema(&url).await;

    assert!(metadata.durable());
    assert!(blocks.durable());
    let initial_revision = metadata
        .load()
        .await
        .expect("load initial metadata")
        .revision;
    if persisted_run() {
        assert_eq!(
            initial_revision, 1,
            "unexpected persisted metadata revision"
        );
        assert_eq!(
            blocks
                .get(&block_id(PERSISTED_BYTES))
                .await
                .expect("load persisted TiDB block"),
            PERSISTED_BYTES
        );
        metadata.close().await.expect("close persisted metadata");
        blocks.close().await.expect("close persisted blocks");
        return;
    }
    assert_eq!(initial_revision, 0);

    let first = metadata
        .acquire_writer("tidb-test-first", Duration::from_secs(30))
        .await
        .expect("acquire first writer");

    metadata
        .release_writer(&first)
        .await
        .expect("release writer before concurrent fencing race");

    let concurrent_a = metadata.clone();
    let concurrent_b = metadata.clone();
    let (left, right) = tokio::join!(
        concurrent_a.acquire_writer("tidb-test-concurrent-a", Duration::from_secs(5)),
        concurrent_b.acquire_writer("tidb-test-concurrent-b", Duration::from_secs(5)),
    );
    match (left, right) {
        (Ok(lease), Err(error)) => {
            assert!(error.is(ErrorCode::Eagain));
            concurrent_a
                .release_writer(&lease)
                .await
                .expect("release concurrently acquired writer");
        }
        (Err(error), Ok(lease)) => {
            assert!(error.is(ErrorCode::Eagain));
            concurrent_b
                .release_writer(&lease)
                .await
                .expect("release concurrently acquired writer");
        }
        (Ok(_), Ok(_)) => panic!("concurrent TiDB writers must not both acquire the lease"),
        (Err(left), Err(right)) => {
            panic!("concurrent TiDB acquisition unexpectedly failed twice: {left}; {right}")
        }
    }

    let first = metadata
        .acquire_writer("tidb-test-first", Duration::from_secs(30))
        .await
        .expect("reacquire first writer");
    let busy = metadata
        .acquire_writer("tidb-test-busy", Duration::from_secs(2))
        .await
        .expect_err("active writer must fence competing acquisition");
    assert!(busy.is(ErrorCode::Eagain));

    metadata
        .release_writer(&first)
        .await
        .expect("initial writer releases");

    let expiring = metadata
        .acquire_writer("tidb-test-expiring", Duration::from_millis(250))
        .await
        .expect("acquire expiring writer");
    tokio::time::sleep(Duration::from_millis(400)).await;
    let second = metadata
        .acquire_writer("tidb-test-second", Duration::from_secs(5))
        .await
        .expect("expired writer can be replaced");
    assert!(second.fence > expiring.fence);
    let stale = metadata
        .publish(0, &expiring, root_namespace().await)
        .await
        .expect_err("expired writer must not publish");
    assert!(stale.is(ErrorCode::Estale));

    let revision = metadata
        .publish(0, &second, root_namespace().await)
        .await
        .expect("current fenced writer publishes");
    assert_eq!(revision, 1);
    let conflict = metadata
        .publish(0, &second, root_namespace().await)
        .await
        .expect_err("stale revision must not publish");
    assert!(conflict.is(ErrorCode::Eagain));
    let second = metadata
        .renew_writer(&second, Duration::from_secs(5))
        .await
        .expect("current writer renews");
    metadata
        .release_writer(&second)
        .await
        .expect("current writer releases");
    metadata.flush().await.expect("metadata durability barrier");

    let first_id = blocks
        .put(b"immutable TiDB bytes")
        .await
        .expect("put block");
    let same_id = blocks
        .put(b"immutable TiDB bytes")
        .await
        .expect("deduplicating immutable put");
    assert_eq!(first_id, same_id);
    assert_eq!(
        blocks.get(&first_id).await.expect("get block"),
        b"immutable TiDB bytes"
    );
    blocks.flush().await.expect("block durability barrier");
    let persisted_id = blocks
        .put(PERSISTED_BYTES)
        .await
        .expect("put persisted TiDB sentinel");
    assert_eq!(persisted_id, block_id(PERSISTED_BYTES));
    blocks
        .flush()
        .await
        .expect("acknowledge persisted TiDB sentinel");
    blocks.delete(&first_id).await.expect("delete block");
    let missing = blocks
        .get(&first_id)
        .await
        .expect_err("deleted block must be absent");
    assert!(missing.is(ErrorCode::Enoent));

    let reopened = TidbMetadataStore::connect_with_key(&url, &volume_key)
        .await
        .expect("reopen TiDB metadata");
    assert_eq!(
        reopened
            .load()
            .await
            .expect("load reopened metadata")
            .revision,
        1
    );
    assert_eq!(
        blocks
            .get(&persisted_id)
            .await
            .expect("reopen persisted TiDB block"),
        PERSISTED_BYTES
    );
    reopened.close().await.expect("close reopened metadata");
    metadata.close().await.expect("close metadata");
    blocks.close().await.expect("close blocks");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_provider_clock_fencing_and_concurrent_publication() {
    let url = tidb_url();
    assert_actual_tidb(&url).await;

    let volume_key = unique_volume_key();
    let metadata = TidbMetadataStore::connect_with_options(
        &url,
        TidbStorageOptions::new(&volume_key)
            .with_durable(true)
            .with_max_namespace_bytes(4 * 1024 * 1024),
    )
    .await
    .expect("connect TiDB metadata for provider-clock fencing");
    let initial = metadata
        .load()
        .await
        .expect("load initial provider-clock metadata");
    assert_eq!(initial.revision, 0);
    assert!(initial.namespace.is_none());

    let stale_store = metadata.clone();
    let replacement_store = metadata.clone();
    let stale = stale_store
        .acquire_writer("tidb-provider-clock-stale", Duration::from_millis(250))
        .await
        .expect("acquire short-lived TiDB lease");
    // Expiry is evaluated with TiDB's CURRENT_TIMESTAMP(3), not this
    // process's wall clock. The delay only gives the provider clock time to
    // advance before the replacement acquisition.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let replacement = replacement_store
        .acquire_writer("tidb-provider-clock-replacement", Duration::from_secs(30))
        .await
        .expect("replace expired TiDB lease");
    assert!(replacement.fence > stale.fence);

    let stale_renew = stale_store
        .renew_writer(&stale, Duration::from_secs(30))
        .await
        .expect_err("stale TiDB lease renewal must be fenced");
    assert!(stale_renew.is(ErrorCode::Estale));
    let stale_release = stale_store
        .release_writer(&stale)
        .await
        .expect_err("stale TiDB lease release must be fenced");
    assert!(stale_release.is(ErrorCode::Estale));

    let current = replacement_store
        .renew_writer(&replacement, Duration::from_secs(30))
        .await
        .expect("replacement TiDB lease remains valid after stale operations");

    let publish_a_store = replacement_store.clone();
    let publish_b_store = replacement_store.clone();
    let publish_a_lease = current.clone();
    let publish_b_lease = current.clone();
    let namespace_a = root_namespace().await;
    let namespace_b = root_namespace().await;
    let (left, right) = tokio::join!(
        publish_a_store.publish(0, &publish_a_lease, namespace_a),
        publish_b_store.publish(0, &publish_b_lease, namespace_b),
    );
    match (left, right) {
        (Ok(revision), Err(error)) | (Err(error), Ok(revision)) => {
            assert_eq!(revision, 1);
            assert!(
                error.is(ErrorCode::Eagain),
                "same-revision TiDB publication conflict must be EAGAIN: {error}"
            );
        }
        (Ok(left_revision), Ok(right_revision)) => {
            panic!(
                "concurrent same-revision TiDB publications both succeeded: {left_revision}, {right_revision}"
            );
        }
        (Err(left_error), Err(right_error)) => {
            panic!(
                "concurrent same-revision TiDB publications both failed: {left_error}; {right_error}"
            );
        }
    }
    assert_eq!(
        replacement_store
            .load()
            .await
            .expect("load concurrently published TiDB metadata")
            .revision,
        1
    );

    replacement_store
        .release_writer(&current)
        .await
        .expect("release current TiDB lease");
    metadata
        .close()
        .await
        .expect("close provider-clock TiDB metadata");
}

#[tokio::test]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn volume_keys_preserve_exact_utf8_and_trailing_spaces() {
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let base = format!("{}-🦀", unique_volume_key());
    let left = TidbStorageOptions::new(&base);
    let right = TidbStorageOptions::new(format!("{base} "));
    let first = TidbMetadataStore::connect_with_options(&url, left.clone())
        .await
        .unwrap();
    let second = TidbMetadataStore::connect_with_options(&url, right.clone())
        .await
        .unwrap();
    let lease = first
        .acquire_writer("writer🦀 ", Duration::from_secs(30))
        .await
        .unwrap();
    first
        .publish(0, &lease, root_namespace().await)
        .await
        .unwrap();
    let pool = Pool::from_url(&url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    let stored_owner: Option<Vec<u8>> = connection
        .exec_first(
            "SELECT owner FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (&base,),
        )
        .await
        .unwrap();
    assert_eq!(stored_owner, Some("writer🦀 ".as_bytes().to_vec()));
    drop(connection);
    pool.disconnect().await.unwrap();
    assert_eq!(second.load().await.unwrap().revision, 0);
    assert!(second.load().await.unwrap().namespace.is_none());
    first.release_writer(&lease).await.unwrap();
    let first_blocks = TidbBlockStore::connect_with_options(&url, left)
        .await
        .unwrap();
    let second_blocks = TidbBlockStore::connect_with_options(&url, right)
        .await
        .unwrap();
    let id = first_blocks.put(b"isolated bytes").await.unwrap();
    assert!(
        second_blocks
            .get(&id)
            .await
            .unwrap_err()
            .is(ErrorCode::Enoent)
    );
    first_blocks.delete(&id).await.unwrap();
    first_blocks.close().await.unwrap();
    second_blocks.close().await.unwrap();
    first.close().await.unwrap();
    second.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual tidb service"]
async fn delegated_directory_authority_fences_old_and_stale_clients() {
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let key = unique_volume_key();
    let metadata = TidbMetadataStore::connect_with_key(&url, &key)
        .await
        .unwrap();
    let other = TidbMetadataStore::connect_with_key(&url, &key)
        .await
        .unwrap();
    let blocks = TidbBlockStore::connect_with_key(&url, &key).await.unwrap();
    let backing = blocks.prepare_concurrent_backing().await.unwrap();
    let mut namespace = root_namespace().await;

    use mount_rs_core::delegation::{
        CheckoutRequest, DelegatedCheckin, DelegatedPublish, DelegatedRecovery,
    };
    use mount_rs_core::storage::DirectoryEntry;
    for ino in [2, 3] {
        let mut node = namespace.nodes[&namespace.root].clone();
        node.stats.ino = ino;
        node.stats.nlink = 2;
        namespace.nodes.insert(ino, node);
    }
    namespace
        .nodes
        .get_mut(&namespace.root)
        .unwrap()
        .stats
        .nlink = 4;
    namespace.nodes.get_mut(&namespace.root).unwrap().data = NodeData::Directory {
        entries: vec![
            DirectoryEntry {
                name: "a".into(),
                inode: 2,
            },
            DirectoryEntry {
                name: "b".into(),
                inode: 3,
            },
        ],
    };
    namespace.next_inode = 4;
    metadata
        .prepare_bound_concurrent_mode(backing)
        .await
        .unwrap();
    let revision = metadata
        .publish_bound_if_revision(backing, 0, namespace.clone())
        .await
        .unwrap();
    metadata
        .prepare_delegated_mode(backing, revision)
        .await
        .unwrap();
    metadata
        .prepare_delegated_mode(backing, revision)
        .await
        .unwrap();
    assert!(
        metadata
            .publish_bound_if_revision(backing, revision, namespace.clone())
            .await
            .is_err()
    );
    assert!(
        metadata
            .acquire_writer("old-writer", Duration::from_secs(10))
            .await
            .is_err()
    );
    let request_a = CheckoutRequest {
        backing,
        root: 2,
        owner: "a".into(),
    };
    let request_b = CheckoutRequest {
        backing,
        root: 3,
        owner: "b".into(),
    };
    let (a, b) = tokio::join!(metadata.checkout(&request_a), other.checkout(&request_b));
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.token.fence, b.token.fence);
    assert_eq!(
        metadata
            .checkout(&CheckoutRequest {
                backing,
                root: 2,
                owner: "a".into()
            })
            .await
            .unwrap(),
        a
    );
    assert!(
        other
            .checkout(&CheckoutRequest {
                backing,
                root: namespace.root,
                owner: "overlap".into()
            })
            .await
            .is_err()
    );
    let request = DelegatedPublish {
        backing,
        token: a.token.clone(),
        expected_revision: revision,
    };
    let mut hostile = namespace.clone();
    hostile.nodes.get_mut(&3).unwrap().stats.ctime_ms += 1;
    assert!(metadata.publish_delegated(&request, hostile).await.is_err());
    let mut header = namespace.clone();
    header.umask = 0o777;
    assert!(metadata.publish_delegated(&request, header).await.is_err());
    assert_eq!(other.load().await.unwrap().revision, revision);
    namespace.nodes.get_mut(&2).unwrap().stats.ctime_ms += 1;
    let updated = metadata
        .publish_delegated(&request, namespace.clone())
        .await
        .unwrap();
    assert!(
        metadata
            .publish_delegated(&request, namespace.clone())
            .await
            .unwrap_err()
            .is(ErrorCode::Eagain)
    );
    let release = DelegatedCheckin {
        backing,
        token: a.token.clone(),
        expected_revision: updated,
    };
    metadata.checkin(&release).await.unwrap();
    other.checkin(&release).await.unwrap();
    let new = other
        .checkout(&CheckoutRequest {
            backing,
            root: 2,
            owner: "new".into(),
        })
        .await
        .unwrap();
    assert!(new.token.fence > a.token.fence);
    assert!(
        metadata
            .publish_delegated(
                &DelegatedPublish {
                    expected_revision: updated,
                    ..request
                },
                namespace
            )
            .await
            .is_err()
    );
    assert!(
        metadata
            .recover(&DelegatedRecovery {
                backing,
                root: 2,
                expected_fence: a.token.fence
            })
            .await
            .is_err()
    );
    other
        .recover(&DelegatedRecovery {
            backing,
            root: 2,
            expected_fence: new.token.fence,
        })
        .await
        .unwrap();
    other
        .recover(&DelegatedRecovery {
            backing,
            root: 2,
            expected_fence: new.token.fence,
        })
        .await
        .unwrap();
    assert!(
        !metadata
            .delegation_state()
            .await
            .unwrap()
            .unwrap()
            .grants
            .contains_key(&2)
    );
}

#[tokio::test]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_conditional_metadata_load() {
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let key = unique_volume_key();
    let metadata = TidbMetadataStore::connect_with_options(&url, TidbStorageOptions::new(&key))
        .await
        .expect("connect metadata");
    assert_eq!(
        metadata.load_if_changed(0).await.unwrap().unwrap().revision,
        0
    );
    let pool = Pool::from_url(&url).unwrap();
    let mut connection = pool.get_conn().await.unwrap();
    let namespace = root_namespace().await;
    let json = serde_json::to_string(&namespace).unwrap();
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_metadata SET revision=1, namespace=? WHERE volume_key=?",
            (&json, &key),
        )
        .await
        .unwrap();
    let changed = metadata.load_if_changed(0).await.unwrap().unwrap();
    assert_eq!(changed.revision, 1);
    assert_eq!(
        serde_json::to_string(&changed.namespace.unwrap()).unwrap(),
        json
    );
    assert!(metadata.load_if_changed(1).await.unwrap().is_none());
    // An unchanged revision must not transfer or decode the namespace payload.
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_metadata SET namespace='invalid JSON' WHERE volume_key=?",
            (&key,),
        )
        .await
        .unwrap();
    assert!(metadata.load_if_changed(1).await.unwrap().is_none());
    assert!(metadata.load_if_changed(0).await.is_err());
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_metadata SET revision=2, namespace=? WHERE volume_key=?",
            (&json, &key),
        )
        .await
        .unwrap();
    assert_eq!(
        metadata.load_if_changed(1).await.unwrap().unwrap().revision,
        2
    );
    assert_eq!(
        metadata
            .load_if_changed(u64::MAX)
            .await
            .unwrap()
            .unwrap()
            .revision,
        2
    );
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_metadata SET revision=0, namespace=NULL WHERE volume_key=?",
            (&key,),
        )
        .await
        .unwrap();
    let uninitialized = metadata.load_if_changed(2).await.unwrap().unwrap();
    assert_eq!(uninitialized.revision, 0);
    assert!(uninitialized.namespace.is_none());
    connection
        .exec_drop(
            "UPDATE mount_rs_tidb_metadata SET revision=-1 WHERE volume_key=?",
            (&key,),
        )
        .await
        .unwrap();
    assert!(metadata.load_if_changed(2).await.is_err());
    connection
        .exec_drop(
            "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (&key,),
        )
        .await
        .unwrap();
    assert!(metadata.load_if_changed(2).await.is_err());
    metadata.close().await.unwrap();
    drop(connection);
    pool.disconnect().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_ten_way_virgin_inode_open() {
    use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let volume = unique_volume_key();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(10));
    let mut tasks = Vec::new();
    for index in 0..10 {
        let url = url.clone();
        let volume = volume.clone();
        let barrier = barrier.clone();
        tasks.push(tokio::spawn(async move {
            let options = TidbStorageOptions::new(volume).with_durable(true);
            let metadata = TidbMetadataStore::connect_with_options(&url, options.clone())
                .await
                .unwrap();
            let blocks = TidbBlockStore::connect_with_options(&url, options)
                .await
                .unwrap();
            barrier.wait().await;
            let opened = ChunkedFs::open(
                metadata.clone(),
                blocks.clone(),
                ChunkedOptions::fixed(format!("virgin-{index}"), 4096)
                    .unwrap()
                    .with_inode_updates(true),
            )
            .await;
            (opened, metadata, blocks)
        }));
    }
    let mut opened = Vec::new();
    let mut errors = Vec::new();
    for task in tasks {
        let (result, metadata, blocks) = task.await.unwrap();
        match result {
            Ok(fs) => opened.push((fs, metadata, blocks)),
            Err(error) => {
                errors.push(error);
                metadata.close().await.unwrap();
                blocks.close().await.unwrap();
            }
        }
    }
    let successes = opened.len();
    if let Some((first, _, _)) = opened.first() {
        first
            .write_file("/oracle", b"ten-way virgin inode authority")
            .await
            .unwrap();
        for (fs, _, _) in &opened {
            let handle = fs.open("/oracle", "r", 0).await.unwrap();
            let mut bytes = [0; 30];
            let count = handle.read(&mut bytes, Some(0)).await.unwrap();
            assert_eq!(&bytes[..count], b"ten-way virgin inode authority");
            handle.close().await.unwrap();
        }
        first.unlink("/oracle").await.unwrap();
    }
    for (fs, metadata, blocks) in opened {
        fs.shutdown().await.unwrap();
        metadata.close().await.unwrap();
        blocks.close().await.unwrap();
    }
    let pool = Pool::from_url(&url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    for table in [
        "mount_rs_tidb_inodes",
        "mount_rs_tidb_metadata",
        "mount_rs_tidb_blocks",
        "mount_rs_tidb_block_authority",
    ] {
        conn.exec_drop(
            format!("DELETE FROM {table} WHERE volume_key=?"),
            (&volume,),
        )
        .await
        .unwrap();
    }
    drop(conn);
    pool.disconnect().await.unwrap();
    assert_eq!(successes, 10, "virgin opens failed: {errors:?}");
}

#[tokio::test]
#[ignore = "requires an actual TiDB service and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_context_keeps_siblings_and_bounds_sessions() {
    use mount_rs_tidb::TidbPoolContext;
    let url = tidb_url();
    assert_actual_tidb(&url).await;
    let key = unique_volume_key();
    // Existing private connect/clone is deliberately still a single owned pool.
    let private = TidbMetadataStore::connect_with_key(&url, format!("{key}-private"))
        .await
        .unwrap();
    let clone = private.clone();
    private.close().await.unwrap();
    assert!(
        clone.flush().await.is_err(),
        "naively sharing a private pool would disconnect siblings"
    );

    let context = TidbPoolContext::new(&url, 2).unwrap();
    let first = context
        .metadata(TidbStorageOptions::new(&key).with_durable(true))
        .await
        .unwrap();
    let sibling = context
        .metadata(TidbStorageOptions::new(format!("{key} ")).with_durable(true))
        .await
        .unwrap();
    let blocks = context
        .blocks(TidbStorageOptions::new(&key).with_durable(true))
        .await
        .unwrap();
    let lease = first
        .acquire_writer("first", Duration::from_secs(30))
        .await
        .unwrap();
    assert_eq!(sibling.load().await.unwrap().revision, 0);
    first.release_writer(&lease).await.unwrap();
    first.close().await.unwrap();
    assert!(context.metadata(TidbStorageOptions::new("")).await.is_err());
    sibling.flush().await.unwrap();
    let id = blocks.put(b"context sibling bytes").await.unwrap();
    assert_eq!(blocks.get(&id).await.unwrap(), b"context sibling bytes");
    blocks.delete(&id).await.unwrap();
    sibling.close().await.unwrap();
    blocks.close().await.unwrap();
    context.close().await.unwrap();
    assert!(sibling.flush().await.is_err());
    assert!(
        context
            .metadata(TidbStorageOptions::new(&key))
            .await
            .is_err()
    );
    context.close().await.unwrap();
    let pool = Pool::from_url(&url).unwrap();
    let mut conn = pool.get_conn().await.unwrap();
    for volume in [&key, &format!("{key} "), &format!("{key}-private")] {
        for table in [
            "mount_rs_tidb_inodes",
            "mount_rs_tidb_metadata",
            "mount_rs_tidb_blocks",
            "mount_rs_tidb_block_authority",
        ] {
            conn.exec_drop(format!("DELETE FROM {table} WHERE volume_key=?"), (volume,))
                .await
                .unwrap();
        }
    }
    drop(conn);
    pool.disconnect().await.unwrap();
}

#[tokio::test]
#[ignore = "requires actual TiDB with create-database privilege and MOUNT_RS_TIDB_URL"]
async fn actual_tidb_context_schema_initialization_retries_after_failed_connect() {
    let url = tidb_url();
    let database = unique_volume_key().replace('-', "_");
    let mut scoped = url::Url::parse(&url).unwrap();
    scoped.set_path(&format!("/{database}"));
    let context = mount_rs_tidb::TidbPoolContext::new(scoped.as_str(), 1).unwrap();
    assert!(
        context
            .metadata(TidbStorageOptions::new("first"))
            .await
            .is_err()
    );
    let admin = Pool::from_url(&url).unwrap();
    let mut conn = admin.get_conn().await.unwrap();
    conn.query_drop(format!("CREATE DATABASE `{database}`"))
        .await
        .unwrap();
    let retried = context.metadata(TidbStorageOptions::new("first")).await;
    let sibling = context.metadata(TidbStorageOptions::new("second")).await;
    context.close().await.unwrap();
    conn.query_drop(format!("DROP DATABASE `{database}`"))
        .await
        .unwrap();
    drop(conn);
    admin.disconnect().await.unwrap();
    assert!(retried.is_ok(), "schema initializer must remain retryable");
    assert!(
        sibling.is_ok(),
        "each volume row must initialize independently"
    );
}
