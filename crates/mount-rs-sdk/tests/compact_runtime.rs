//! Bounded public SDK compact runtime oracle on owned native fixtures.
//! Every run uses a unique key recorded for scoped cleanup by its runner.

use mount_rs_core::ErrorCode;
use mount_rs_core::storage::{BlockStore, ConcurrentBackingId, MetadataStore};
use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore};
use mount_rs_sdk::{Filesystem, SplitOptions, StorageContext, StoreConfig};
use mount_rs_tidb::{TidbBlockStore, TidbMetadataStore};
use std::io::Write as _;

fn owned_key(provider: &str) -> String {
    let key = format!(
        "compact-runtime-{provider}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    record_owned_key(provider, &key);
    key
}

fn record_owned_key(provider: &str, key: &str) {
    let manifest = std::env::var("MOUNT_RS_COMPACT_CLEANUP_MANIFEST")
        .expect("runner-owned compact cleanup manifest required");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(manifest)
        .expect("open owned compact cleanup manifest");
    writeln!(file, "{provider}\t{key}").unwrap();
    file.sync_all().unwrap();
}

fn payload(seed: usize) -> Vec<u8> {
    (0..4096)
        .map(|index| ((index * 19 + seed * 37 + index / 101) % 256) as u8)
        .collect()
}

async fn byte_oracle(fs: &Filesystem, path: &str, expected: &[u8]) {
    let file = fs.driver().open(path, "r", 0).await.unwrap();
    let mut actual = vec![0; expected.len()];
    let mut offset = 0;
    while offset < actual.len() {
        let count = file
            .read(&mut actual[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0, "file ended before expected full payload");
        offset += count;
    }
    assert_eq!(actual, expected);
    assert_eq!(
        file.read(&mut [0], Some(expected.len() as u64))
            .await
            .unwrap(),
        0
    );
    file.close().await.unwrap();
}

async fn run_oracle(provider: &str, store: StoreConfig) {
    let options = || {
        let mut options = SplitOptions::memory(format!("compact-runtime-{provider}"), 4096)
            .with_compact_inode_updates(true);
        options.metadata = store.clone();
        options.blocks = store.clone();
        options
    };
    let first_context = StorageContext::new(2).unwrap();
    let first = Filesystem::split_with_context(options(), &first_context)
        .await
        .unwrap();
    for index in 0..4 {
        first
            .driver()
            .write_file(&format!("/file-{index}"), &payload(index))
            .await
            .unwrap();
    }
    let selected = first.driver().open("/file-0", "r+", 0).await.unwrap();
    let replacement = payload(9);
    assert_eq!(
        selected.write(&replacement, Some(0)).await.unwrap(),
        replacement.len()
    );
    selected.close().await.unwrap();
    first.driver().rename("/file-1", "/renamed").await.unwrap();
    first.shutdown().await.unwrap();
    drop(first);
    first_context.close().await.unwrap();

    let second_context = StorageContext::new(2).unwrap();
    let second = Filesystem::split_with_context(options(), &second_context)
        .await
        .unwrap();
    byte_oracle(&second, "/file-0", &replacement).await;
    byte_oracle(&second, "/renamed", &payload(1)).await;
    byte_oracle(&second, "/file-2", &payload(2)).await;
    byte_oracle(&second, "/file-3", &payload(3)).await;
    second.shutdown().await.unwrap();
    second_context.close().await.unwrap();
    println!(
        "COMPACT_SDK_NATIVE_PASS provider={provider} files=4 full_bytes=16384 eof_checks=4 selected_writes=1 full_renames=1 reopen=1"
    );
}

fn with_key(store: &StoreConfig, volume_key: String) -> StoreConfig {
    match store {
        StoreConfig::Tidb {
            connection,
            durable,
            ..
        } => StoreConfig::Tidb {
            connection: connection.clone(),
            volume_key,
            durable: *durable,
        },
        StoreConfig::Pglite {
            connection,
            durable,
            ..
        } => StoreConfig::Pglite {
            connection: connection.clone(),
            volume_key,
            durable: *durable,
        },
        _ => panic!("native marker oracle requires TiDB or PGlite"),
    }
}

async fn mode_backing(store: &StoreConfig) -> ConcurrentBackingId {
    match store {
        StoreConfig::Tidb {
            connection,
            volume_key,
            ..
        } => {
            let metadata = TidbMetadataStore::connect_with_key(connection, volume_key)
                .await
                .unwrap();
            let backing = metadata
                .compact_inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .backing;
            metadata.close().await.unwrap();
            backing
        }
        StoreConfig::Pglite {
            connection,
            volume_key,
            ..
        } => {
            let metadata = PgliteMetadataStore::connect_with_key(connection, volume_key)
                .await
                .unwrap();
            let backing = metadata
                .compact_inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .backing;
            metadata.close().await.unwrap();
            backing
        }
        _ => unreachable!(),
    }
}

async fn marker_matches(store: &StoreConfig, backing: ConcurrentBackingId) -> bool {
    match store {
        StoreConfig::Tidb {
            connection,
            volume_key,
            ..
        } => {
            let blocks = TidbBlockStore::connect_with_key(connection, volume_key)
                .await
                .unwrap();
            let matches = blocks.verify_concurrent_backing(backing).await.is_ok();
            blocks.close().await.unwrap();
            matches
        }
        StoreConfig::Pglite {
            connection,
            volume_key,
            ..
        } => {
            let blocks = PgliteBlockStore::connect_with_key(connection, volume_key)
                .await
                .unwrap();
            let matches = blocks.verify_concurrent_backing(backing).await.is_ok();
            blocks.close().await.unwrap();
            matches
        }
        _ => unreachable!(),
    }
}

async fn native_marker_negatives(provider: &str, established: StoreConfig) {
    let backing = mode_backing(&established).await;
    let missing = with_key(
        &established,
        owned_key(&format!("{provider}-missing-blocks")),
    );
    assert!(!marker_matches(&missing, backing).await);
    let mut options = SplitOptions::memory(format!("{provider}-missing-marker"), 4096)
        .with_compact_inode_updates(true);
    options.metadata = established.clone();
    options.blocks = missing.clone();
    let missing_error = match Filesystem::split(options).await {
        Ok(_) => panic!("missing marker was repaired"),
        Err(error) => error,
    };
    assert_eq!(missing_error.code, ErrorCode::Estale);
    assert!(!marker_matches(&missing, backing).await);
    assert_eq!(mode_backing(&established).await, backing);

    let wrong = with_key(&established, owned_key(&format!("{provider}-wrong-blocks")));
    let mut initialize = SplitOptions::memory(format!("{provider}-wrong-fixture"), 4096)
        .with_compact_inode_updates(true);
    initialize.metadata = wrong.clone();
    initialize.blocks = wrong.clone();
    let wrong_fs = Filesystem::split(initialize).await.unwrap();
    wrong_fs.shutdown().await.unwrap();
    let wrong_backing = mode_backing(&wrong).await;
    assert_ne!(wrong_backing, backing);
    assert!(marker_matches(&wrong, wrong_backing).await);
    let mut options = SplitOptions::memory(format!("{provider}-wrong-marker"), 4096)
        .with_compact_inode_updates(true);
    options.metadata = established.clone();
    options.blocks = wrong.clone();
    let wrong_error = match Filesystem::split(options).await {
        Ok(_) => panic!("wrong marker was accepted"),
        Err(error) => error,
    };
    assert_eq!(wrong_error.code, ErrorCode::Estale);
    assert!(!marker_matches(&wrong, backing).await);
    assert!(marker_matches(&wrong, wrong_backing).await);
    assert_eq!(mode_backing(&wrong).await, wrong_backing);
    assert_eq!(mode_backing(&established).await, backing);
    let oracle = Filesystem::split({
        let mut options = SplitOptions::memory(format!("{provider}-marker-oracle"), 4096)
            .with_compact_inode_updates(true);
        options.metadata = established.clone();
        options.blocks = established;
        options
    })
    .await
    .unwrap();
    byte_oracle(&oracle, "/file-0", &payload(9)).await;
    oracle.shutdown().await.unwrap();
    println!(
        "COMPACT_SDK_MARKER_NEGATIVES_PASS provider={provider} missing=1 wrong=1 foreign_marker_preserved=1 foreign_metadata_preserved=1 fresh_reopen=1"
    );
}

#[tokio::test]
#[ignore = "requires owned actual TiDB and cleanup manifest"]
async fn actual_tidb_compact_selected_full_reopen_bytes_eof() {
    let connection = std::env::var("MOUNT_RS_TIDB_URL").unwrap();
    let key = owned_key("tidb");
    let store = StoreConfig::Tidb {
        connection,
        volume_key: key,
        durable: true,
    };
    run_oracle("tidb", store.clone()).await;
    native_marker_negatives("tidb", store).await;
}

#[tokio::test]
#[ignore = "requires owned actual PGlite and cleanup manifest"]
async fn actual_pglite_compact_selected_full_reopen_bytes_eof() {
    let connection = std::env::var("PGLITE_DATABASE_URL").unwrap();
    let key = owned_key("pglite");
    let store = StoreConfig::Pglite {
        connection,
        volume_key: key,
        durable: true,
    };
    run_oracle("pglite", store.clone()).await;
    native_marker_negatives("pglite", store).await;
}

#[cfg(feature = "foundationdb")]
#[tokio::test]
#[ignore = "requires owned actual FoundationDB and cleanup manifest"]
async fn actual_foundationdb_virgin_compact_selected_full_reopen_bytes_eof() {
    use mount_rs_core::storage::{BlockStore, MetadataStore};
    use mount_rs_foundationdb::{FoundationDbStorage, FoundationDbStorageOptions};
    use mount_rs_sdk::FoundationDbLeaseAuthority;
    let cluster = std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").unwrap();
    let key = owned_key("foundationdb");
    let virgin =
        FoundationDbStorage::connect(&cluster, FoundationDbStorageOptions::new(&key)).unwrap();
    assert_eq!(
        virgin.metadata().compact_inode_mode_state().await.unwrap(),
        None
    );
    drop(virgin);
    run_oracle(
        "foundationdb",
        StoreConfig::FoundationDb {
            cluster_file: cluster.into(),
            volume_key: key.clone(),
            durable: true,
            lease_authority: FoundationDbLeaseAuthority::RevisionCas,
        },
    )
    .await;

    let cluster = std::env::var("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE").unwrap();
    let established =
        FoundationDbStorage::connect(&cluster, FoundationDbStorageOptions::new(&key)).unwrap();
    let mode = established
        .metadata()
        .compact_inode_mode_state()
        .await
        .unwrap()
        .unwrap();
    let wrong_key = format!("{key}/wrong-blocks");
    record_owned_key("foundationdb", &wrong_key);
    let wrong = FoundationDbStorage::connect(&cluster, FoundationDbStorageOptions::new(&wrong_key))
        .unwrap();
    assert!(
        wrong
            .blocks()
            .verify_concurrent_backing(mode.backing)
            .await
            .is_err()
    );
    let mut options =
        SplitOptions::memory("fdb-missing-marker", 4096).with_compact_inode_updates(true);
    options.metadata = StoreConfig::FoundationDb {
        cluster_file: cluster.clone().into(),
        volume_key: key.clone(),
        durable: true,
        lease_authority: FoundationDbLeaseAuthority::RevisionCas,
    };
    options.blocks = StoreConfig::FoundationDb {
        cluster_file: cluster.clone().into(),
        volume_key: wrong_key.clone(),
        durable: true,
        lease_authority: FoundationDbLeaseAuthority::RevisionCas,
    };
    let missing_error = match Filesystem::split(options.clone()).await {
        Ok(_) => panic!("missing FoundationDB marker was repaired"),
        Err(error) => error,
    };
    assert_eq!(missing_error.code, ErrorCode::Estale);
    assert_eq!(
        established
            .metadata()
            .compact_inode_mode_state()
            .await
            .unwrap(),
        Some(mode)
    );
    assert!(
        wrong
            .blocks()
            .verify_concurrent_backing(mode.backing)
            .await
            .is_err()
    );
    let foreign_backing = wrong.blocks().prepare_concurrent_backing().await.unwrap();
    assert_ne!(foreign_backing, mode.backing);
    wrong
        .blocks()
        .verify_concurrent_backing(foreign_backing)
        .await
        .unwrap();
    options.owner = "fdb-wrong-marker".into();
    let wrong_error = match Filesystem::split(options).await {
        Ok(_) => panic!("wrong FoundationDB marker was accepted"),
        Err(error) => error,
    };
    assert_eq!(wrong_error.code, ErrorCode::Estale);
    wrong
        .blocks()
        .verify_concurrent_backing(foreign_backing)
        .await
        .unwrap();
    assert!(
        wrong
            .blocks()
            .verify_concurrent_backing(mode.backing)
            .await
            .is_err()
    );
    assert_eq!(
        established
            .metadata()
            .compact_inode_mode_state()
            .await
            .unwrap(),
        Some(mode)
    );
    drop(wrong);
    drop(established);
    let mut oracle =
        SplitOptions::memory("fdb-marker-oracle", 4096).with_compact_inode_updates(true);
    let original = StoreConfig::FoundationDb {
        cluster_file: cluster.into(),
        volume_key: key,
        durable: true,
        lease_authority: FoundationDbLeaseAuthority::RevisionCas,
    };
    oracle.metadata = original.clone();
    oracle.blocks = original;
    let reopened = Filesystem::split(oracle).await.unwrap();
    byte_oracle(&reopened, "/file-0", &payload(9)).await;
    reopened.shutdown().await.unwrap();
    println!(
        "COMPACT_SDK_MARKER_NEGATIVES_PASS provider=foundationdb missing=1 wrong=1 fresh_reopen=1"
    );
}
