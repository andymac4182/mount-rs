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
use mount_rs_core::{ErrorCode, FsDriver, MemoryFs, S_IFDIR};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore, TidbStorageOptions};
use mysql_async::Pool;
use mysql_async::prelude::Queryable;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const PERSISTED_BYTES: &[u8] = b"mount-rs TiDB persisted sentinel";

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
    let columns: Vec<(String, String, String, Option<u64>, Option<String>)> = connection
        .exec(
            "SELECT TABLE_NAME, COLUMN_NAME, DATA_TYPE, CHARACTER_MAXIMUM_LENGTH, COLLATION_NAME
             FROM information_schema.columns
             WHERE TABLE_SCHEMA=DATABASE()
               AND ((TABLE_NAME='mount_rs_tidb_metadata'
                     AND COLUMN_NAME IN ('volume_key', 'owner'))
                    OR (TABLE_NAME='mount_rs_tidb_blocks'
                        AND COLUMN_NAME IN ('volume_key', 'id')))
             ORDER BY TABLE_NAME, COLUMN_NAME",
            (),
        )
        .await
        .expect("could not inspect the TiDB provider schema");
    for (table, column, expected_length) in [
        ("mount_rs_tidb_metadata", "volume_key", 1020_u64),
        ("mount_rs_tidb_metadata", "owner", 1020_u64),
        ("mount_rs_tidb_blocks", "volume_key", 1020_u64),
        ("mount_rs_tidb_blocks", "id", 65_u64),
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
