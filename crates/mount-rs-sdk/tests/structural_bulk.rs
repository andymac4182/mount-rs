//! Actual public SDK oracle. The runner must remove exact owned rows from the
//! cleanup manifest even if this test fails; no credentials enter that manifest.
use mount_rs_core::storage::{InodeMetadataSnapshot, MetadataStore, NodeData};
use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext, StoreConfig};
use std::io::Write;

fn payload(file: usize) -> Vec<u8> {
    (0..4096)
        .map(|i| ((file * 37 + i * 19 + i / 251) % 256) as u8)
        .collect()
}
fn check_authority(snapshot: &InodeMetadataSnapshot) {
    assert_eq!(snapshot.namespace.nodes.len(), 131);
    assert_eq!(snapshot.inode_revisions.len(), 131);
    assert!(snapshot.structural_generation >= 131);
    for (&inode, node) in &snapshot.namespace.nodes {
        assert_eq!(node.stats.ino, inode);
        if matches!(node.data, NodeData::File(_)) {
            assert_eq!(node.stats.size, 4096);
        }
    }
}
async fn oracle(provider: &str) {
    let connection = std::env::var(if provider == "tidb" {
        "MOUNT_RS_TIDB_URL"
    } else {
        "PGLITE_DATABASE_URL"
    })
    .unwrap();
    let key = format!(
        "sdk-bulk-{provider}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let manifest = std::env::var("MOUNT_RS_BULK_CLEANUP_MANIFEST")
        .expect("runner-owned cleanup manifest required");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(manifest)
        .unwrap();
    writeln!(file, "{provider}\t{key}").unwrap();
    file.sync_all().unwrap();
    let options = || {
        let store = if provider == "tidb" {
            StoreConfig::Tidb {
                connection: connection.clone(),
                volume_key: key.clone(),
                durable: true,
            }
        } else {
            StoreConfig::Pglite {
                connection: connection.clone(),
                volume_key: key.clone(),
                durable: true,
            }
        };
        let mut options =
            SplitOptions::memory("structural-bulk-oracle", 4096).with_inode_updates(true);
        options.metadata = store.clone();
        options.blocks = store;
        options
    };
    let context = StorageContext::new(2).unwrap();
    let fs = Filesystem::split_with_context(options(), &context)
        .await
        .unwrap();
    for i in 0..130 {
        fs.driver()
            .write_file(&format!("/f{i}"), &payload(i))
            .await
            .unwrap();
    }
    fs.shutdown().await.unwrap();
    drop(fs);
    context.close().await.unwrap();
    // Independent provider connection validates the committed backing, complete
    // namespace membership and every guard before an independent SDK reopen.
    if provider == "tidb" {
        let store = mount_rs_tidb::TidbMetadataStore::connect_with_key(&connection, &key)
            .await
            .unwrap();
        let state = store.inode_mode_state().await.unwrap().unwrap();
        let snapshot = store.load_inode_snapshot(state.backing).await.unwrap();
        assert_eq!(snapshot.structural_generation, state.structural_generation);
        check_authority(&snapshot);
        store.close().await.unwrap();
    } else {
        let store = mount_rs_pglite::PgliteMetadataStore::connect_with_key(&connection, &key)
            .await
            .unwrap();
        let state = store.inode_mode_state().await.unwrap().unwrap();
        let snapshot = store.load_inode_snapshot(state.backing).await.unwrap();
        assert_eq!(snapshot.structural_generation, state.structural_generation);
        check_authority(&snapshot);
        store.close().await.unwrap();
    }
    let context = StorageContext::new(2).unwrap();
    let fs = Filesystem::split_with_context(options(), &context)
        .await
        .unwrap();
    for i in 0..130 {
        let handle = fs.driver().open(&format!("/f{i}"), "r", 0).await.unwrap();
        let mut actual = vec![0; 4096];
        assert_eq!(handle.read(&mut actual, Some(0)).await.unwrap(), 4096);
        assert_eq!(actual, payload(i));
        assert_eq!(handle.read(&mut [0; 1], Some(4096)).await.unwrap(), 0);
        handle.close().await.unwrap();
    }
    for i in 0..130 {
        fs.driver().unlink(&format!("/f{i}")).await.unwrap();
    }
    fs.shutdown().await.unwrap();
    context.close().await.unwrap();
    println!(
        "SDK_BULK_ORACLE_PASS provider={provider} nodes=131 files=130 verified_bytes=532480 eof_checks=130 unlink=130 contexts_closed=2"
    );
}
#[tokio::test]
#[ignore = "requires owned actual TiDB fixture and cleanup runner"]
async fn actual_tidb_bulk_public_sdk_reopen_bytes_and_eof() {
    oracle("tidb").await;
}
#[tokio::test]
#[ignore = "requires owned actual PGlite fixture and cleanup runner"]
async fn actual_pglite_bulk_public_sdk_reopen_bytes_and_eof() {
    oracle("pglite").await;
}
