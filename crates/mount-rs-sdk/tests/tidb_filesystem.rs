//! Actual TiDB metadata with direct filesystem blocks, selected by the owned TiDB harness.
#![cfg(any(target_os = "linux", target_os = "macos"))]

use mount_rs_core::storage::ConcurrentBackingId;
use mount_rs_sdk::{Filesystem, Loopback, SplitOptions, StorageContext, StoreConfig};
use mysql_async::{Pool, prelude::Queryable};
use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write as _};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, symlink};
use std::path::{Path, PathBuf};

const FILE_A: &str = "/filesystem-roundtrip";
const FILE_B: &str = "/filesystem-peer";
const MAX_FIXTURE_BYTES: u64 = 8192;
const AUTHORITY_QUERY: &str = "SELECT CONCAT(revision,'|',IFNULL(HEX(write_mode),'~'),'|',IFNULL(HEX(backing_id),'~'),'|',IFNULL(HEX(owner),'~'),'|',fence,'|',expires,'|',IFNULL(HEX(namespace),'~'),'|',IFNULL(HEX(delegation),'~')) FROM mount_rs_tidb_metadata WHERE volume_key=?";

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("explicit owned fixture setting {name} required"))
}

fn component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

fn private_read(path: &Path) -> String {
    let metadata = fs::symlink_metadata(path).unwrap();
    assert!(metadata.is_file());
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert!(metadata.len() <= MAX_FIXTURE_BYTES);
    let mut bytes = Vec::new();
    fs::File::open(path)
        .unwrap()
        .take(MAX_FIXTURE_BYTES + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert!(bytes.len() as u64 <= MAX_FIXTURE_BYTES);
    String::from_utf8(bytes).unwrap()
}

fn private_write(path: &Path, bytes: &[u8]) {
    assert!(bytes.len() as u64 <= MAX_FIXTURE_BYTES);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
}

fn json_string(value: &str) -> String {
    let mut json = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => json.push_str("\\\""),
            '\\' => json.push_str("\\\\"),
            ch if ch <= '\u{1f}' => write!(json, "\\u{:04x}", u32::from(ch)).unwrap(),
            ch => json.push(ch),
        }
    }
    json.push('"');
    json
}

fn cli_config(key: &str, root: &Path) -> String {
    format!(
        "{{\"version\":1,\"driver\":{{\"kind\":\"splitstore\",\"storage\":{{\"metadata\":{{\"kind\":\"tidb\",\"connection\":{{\"env\":\"MOUNT_RS_TIDB_URL\"}},\"volume_key\":{},\"durable\":true}},\"blocks\":{{\"kind\":\"filesystem\",\"root\":{},\"persistent\":true}},\"chunk_size_bytes\":4096,\"concurrent_writes\":true,\"inode_updates\":true,\"compact_inode_updates\":true}}}}}}\n",
        json_string(key),
        json_string(root.to_str().unwrap()),
    )
}

fn options(connection: &str, key: &str, root: &Path, owner: &str) -> SplitOptions {
    let metadata = fs::symlink_metadata(root).unwrap();
    let mut options = SplitOptions::memory(owner, 4096)
        .with_compact_inode_updates(true)
        .with_identity(metadata.uid(), metadata.gid(), 0);
    options.metadata = StoreConfig::Tidb {
        connection: connection.into(),
        volume_key: key.into(),
        durable: true,
    };
    options.blocks = StoreConfig::Filesystem {
        root: root.into(),
        persistent: true,
    };
    options
}

async fn authority(connection: &str, key: &str) -> Option<String> {
    let pool = Pool::from_url(connection).unwrap();
    let mut sql = pool.get_conn().await.unwrap();
    let row = sql.exec_first(AUTHORITY_QUERY, (key,)).await.unwrap();
    drop(sql);
    pool.disconnect().await.unwrap();
    row
}

async fn verify_sql_backing_and_filesystem_blocks(
    connection: &str,
    key: &str,
    backing: ConcurrentBackingId,
    root: &Path,
) {
    let pool = Pool::from_url(connection).unwrap();
    let mut sql = pool.get_conn().await.unwrap();
    let row: Option<(Option<String>, Option<String>)> = sql
        .exec_first(
            "SELECT backing_id,write_mode FROM mount_rs_tidb_metadata WHERE volume_key=?",
            (key,),
        )
        .await
        .unwrap();
    assert_eq!(row, Some((Some(backing.to_hex()), Some("MRC5".into()))));
    let sql_blocks: Option<u64> = sql
        .exec_first(
            "SELECT COUNT(*) FROM mount_rs_tidb_blocks WHERE volume_key=?",
            (key,),
        )
        .await
        .unwrap();
    assert_eq!(
        sql_blocks,
        Some(0),
        "blocks must use the filesystem provider"
    );
    drop(sql);
    pool.disconnect().await.unwrap();
    assert!(fs::read_dir(root).unwrap().any(|entry| {
        let shard = entry.unwrap().path();
        shard.is_dir()
            && fs::read_dir(shard)
                .unwrap()
                .any(|file| file.unwrap().path().is_file())
    }));
}

async fn assert_file(filesystem: &Filesystem, path: &str, expected: &[u8]) {
    let file = filesystem.driver().open(path, "r", 0).await.unwrap();
    let mut bytes = vec![0; expected.len()];
    let mut offset = 0;
    while offset < bytes.len() {
        let count = file
            .read(&mut bytes[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0, "premature EOF");
        offset += count;
    }
    assert_eq!(bytes, expected);
    assert_eq!(file.read(&mut [0], Some(offset as u64)).await.unwrap(), 0);
    file.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires the owned durable TiDB harness and private filesystem root"]
async fn actual_tidb_filesystem_writeback_peer_writes_reopen_and_root_authority() {
    assert_eq!(required("MOUNT_RS_TIDB_FILESYSTEM"), "1");
    assert_eq!(required("MOUNT_RS_TIDB_TOPOLOGY"), "durable");
    let run = required("MOUNT_RS_TIDB_RUN_ID");
    assert!(component(&run));
    let original_key = required("MOUNT_RS_TIDB_TEST_VOLUME_KEY");
    assert_eq!(original_key, format!("mount-rs-tidb-{run}"));
    let key = format!("{original_key}-filesystem");
    let connection = required("MOUNT_RS_TIDB_URL");
    let port = connection
        .strip_prefix("mysql://root@127.0.0.1:")
        .and_then(|url| url.strip_suffix("/test"))
        .expect("owned loopback TiDB connection required");
    assert!(port.parse::<u16>().is_ok_and(|port| port > 0));
    let phase = required("MOUNT_RS_TIDB_EXPECT_PERSISTED");
    assert!(matches!(phase.as_str(), "0" | "1"));
    let root = PathBuf::from(required("MOUNT_RS_TIDB_FILESYSTEM_ROOT"));
    assert!(root.is_absolute());
    let directory = root.parent().unwrap();
    assert_eq!(directory.canonicalize().unwrap(), directory);
    assert_eq!(root, directory.join("filesystem-blocks"));
    assert_eq!(
        directory
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .replace('.', "-"),
        run
    );
    let directory_metadata = fs::symlink_metadata(directory).unwrap();
    assert!(directory_metadata.is_dir());
    assert_eq!(directory_metadata.mode() & 0o777, 0o700);
    let root_metadata = fs::symlink_metadata(&root).unwrap();
    assert!(root_metadata.is_dir());
    assert_eq!(root_metadata.mode() & 0o777, 0o700);
    let witness = PathBuf::from(required("MOUNT_RS_TIDB_FILESYSTEM_WITNESS"));
    let config = PathBuf::from(required("MOUNT_RS_TIDB_FILESYSTEM_CLI_CONFIG"));
    assert_eq!(witness, directory.join("filesystem.witness"));
    assert_eq!(config, directory.join("filesystem-cli.json"));
    let binding = format!(
        "mount-rs-tidb-filesystem-v1\n{run}\n{key}\n{}:{}\n",
        root_metadata.dev(),
        root_metadata.ino(),
    );
    let config_bytes = cli_config(&key, &root);
    let bytes_a: Vec<u8> = (0..12_345).map(|index| (index % 251) as u8).collect();
    let bytes_b: Vec<u8> = (0..7_333)
        .map(|index| ((index * 17 + 3) % 251) as u8)
        .collect();
    let pool = Pool::from_url(&connection).unwrap();
    let mut sql = pool.get_conn().await.unwrap();
    let version: String = sql.query_first("SELECT VERSION()").await.unwrap().unwrap();
    assert!(version.to_ascii_lowercase().contains("tidb"));
    drop(sql);
    pool.disconnect().await.unwrap();

    let retained = if phase == "1" {
        let witness_bytes = private_read(&witness);
        let backing = witness_bytes
            .strip_prefix(&binding)
            .unwrap()
            .strip_suffix('\n')
            .unwrap();
        let backing = ConcurrentBackingId::from_hex(backing).unwrap();
        assert_eq!(private_read(&config), config_bytes);
        // Observe retained SQL authority before a constructor could initialize missing metadata.
        verify_sql_backing_and_filesystem_blocks(&connection, &key, backing, &root).await;
        Some(backing)
    } else {
        assert!(!witness.exists());
        assert!(!config.exists());
        assert!(authority(&connection, &key).await.is_none());
        assert!(fs::read_dir(&root).unwrap().next().is_none());
        None
    };

    let first_context = StorageContext::new(2).unwrap();
    let first_options = options(&connection, &key, &root, "filesystem-first");
    let first = Filesystem::split_with_context(first_options.clone(), &first_context)
        .await
        .unwrap();
    assert!(!first.driver().capabilities().durable_writes);
    let backing = first.concurrent_backing_id().unwrap();
    if let Some(retained) = retained {
        assert_eq!(backing, retained);
        assert_file(&first, FILE_A, &bytes_a).await;
        assert_file(&first, FILE_B, &bytes_b).await;
    } else {
        let peer_context = StorageContext::new(2).unwrap();
        let peer = Filesystem::split_with_context(
            options(&connection, &key, &root, "filesystem-peer"),
            &peer_context,
        )
        .await
        .unwrap();
        assert!(!peer.driver().capabilities().durable_writes);
        assert_eq!(peer.concurrent_backing_id(), Some(backing));
        let first_view = Loopback::from_arc(first.driver());
        let peer_view = Loopback::from_arc(peer.driver());
        let (first_write, peer_write) = tokio::join!(
            first_view.write_file(FILE_A, &bytes_a),
            peer_view.write_file(FILE_B, &bytes_b),
        );
        first_write.unwrap();
        peer_write.unwrap();
        assert_file(&peer, FILE_A, &bytes_a).await;
        assert_file(&first, FILE_B, &bytes_b).await;
        first.driver().syncfs().await.unwrap();
        peer.driver().syncfs().await.unwrap();
        peer.shutdown().await.unwrap();
        drop(peer_view);
        drop(peer);
        peer_context.close().await.unwrap();
    }
    first.shutdown().await.unwrap();
    drop(first);
    first_context.close().await.unwrap();

    // Foreign and symlink roots must not rebind the retained TiDB authority.
    if phase == "0" {
        let before = authority(&connection, &key).await.unwrap();
        let foreign = directory.join("filesystem-foreign");
        fs::DirBuilder::new().mode(0o700).create(&foreign).unwrap();
        let alias = directory.join("filesystem-alias");
        symlink(&root, &alias).unwrap();
        for bad_root in [&foreign, &alias] {
            let bad_context = StorageContext::new(2).unwrap();
            assert!(
                Filesystem::split_with_context(
                    options(&connection, &key, bad_root, "filesystem-wrong-root"),
                    &bad_context,
                )
                .await
                .is_err()
            );
            bad_context.close().await.unwrap();
            assert_eq!(
                authority(&connection, &key).await.as_deref(),
                Some(before.as_str())
            );
        }
    }
    let reopened_context = StorageContext::new(2).unwrap();
    let mut reopened_options = first_options;
    reopened_options.owner = "filesystem-reopened".into();
    let reopened = Filesystem::split_with_context(reopened_options, &reopened_context)
        .await
        .unwrap();
    assert!(!reopened.driver().capabilities().durable_writes);
    assert_eq!(reopened.concurrent_backing_id(), Some(backing));
    assert_file(&reopened, FILE_A, &bytes_a).await;
    assert_file(&reopened, FILE_B, &bytes_b).await;
    reopened.shutdown().await.unwrap();
    drop(reopened);
    reopened_context.close().await.unwrap();
    verify_sql_backing_and_filesystem_blocks(&connection, &key, backing, &root).await;
    if phase == "0" {
        private_write(
            &witness,
            format!("{binding}{}\n", backing.to_hex()).as_bytes(),
        );
        private_write(&config, config_bytes.as_bytes());
        println!(
            "TIDB_FILESYSTEM_SEED_PASS full_bytes=verified peer_contexts=verified fresh_reopen=verified root_authority=verified sql_blocks=0"
        );
    } else {
        println!(
            "TIDB_FILESYSTEM_REOPEN_PASS retained_backing=verified pre_constructor_sql=verified full_bytes=verified fresh_reopen=verified sql_blocks=0"
        );
    }
    // The owned harness retains this exact root and volume through restart,
    // then removes its own containers, volumes and run directory on exit.
}
