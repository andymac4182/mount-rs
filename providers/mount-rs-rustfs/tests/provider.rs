use std::sync::Arc;

use mount_rs_core::ErrorCode;
use mount_rs_core::storage::BlockStore;
use mount_rs_rustfs::{RustFsBlockStore, RustFsConfig};
use object_store::memory::InMemory;

fn local_config() -> RustFsConfig {
    RustFsConfig {
        endpoint: "http://127.0.0.1:9000".to_owned(),
        bucket: "mount-rs-test".to_owned(),
        access_key_id: "test-access-key".to_owned(),
        secret_access_key: "test-secret-key".to_owned(),
        region: "us-east-1".to_owned(),
    }
}

#[test]
fn local_endpoint_builds_a_signed_path_style_client() {
    let config = local_config();
    assert!(config.build_store().is_ok());
    let mut trailing_slash = config;
    trailing_slash.endpoint.push('/');
    assert!(trailing_slash.build_store().is_ok());
}

#[test]
fn caller_declares_rustfs_block_durability() {
    let config = local_config();
    let temporary = RustFsBlockStore::from_config(&config, "temporary/blocks", false).unwrap();
    let persisted = RustFsBlockStore::from_config(&config, "persisted/blocks", true).unwrap();
    assert!(!temporary.durable());
    assert!(persisted.durable());
}

#[test]
fn rejects_remote_http_and_secret_bearing_endpoints() {
    for endpoint in [
        "http://rustfs.example.com:9000",
        "https://user:password@rustfs.example.com",
        "https://rustfs.example.com?token=secret",
        "ftp://127.0.0.1:9000",
    ] {
        let mut config = local_config();
        config.endpoint = endpoint.to_owned();
        assert!(config.validate().is_err(), "accepted {endpoint}");
    }
}

#[test]
fn rejects_endpoint_paths_that_could_appear_in_request_errors() {
    for endpoint in [
        "https://rustfs.example.com/tenant-secret",
        "http://127.0.0.1:9000/tenant-secret",
        r"https://rustfs.example.com\tenant-secret",
        "https://rustfs.example.com%2Ftenant-secret",
    ] {
        let mut config = local_config();
        config.endpoint = endpoint.to_owned();
        assert!(config.validate().is_err(), "accepted {endpoint}");
    }
}

#[test]
fn rejects_empty_port_before_building_client() {
    let mut config = local_config();
    config.endpoint = "http://localhost:".to_owned();
    assert!(config.validate().is_err());
}

#[test]
fn rejects_malformed_or_out_of_range_authority_ports() {
    for endpoint in [
        "https://rustfs.example.com:tenant-secret",
        "https://rustfs.example.com:99999",
        "http://127.0.0.1:99999",
    ] {
        let mut config = local_config();
        config.endpoint = endpoint.to_owned();
        assert!(config.validate().is_err(), "accepted {endpoint}");
    }
}

#[test]
fn debug_output_redacts_credentials_and_endpoint_path() {
    let mut config = local_config();
    config.endpoint = "https://rustfs.example.com/private-path".to_owned();
    let debug = format!("{config:?}");
    assert!(!debug.contains("test-access-key"));
    assert!(!debug.contains("test-secret-key"));
    assert!(!debug.contains("private-path"));
}

#[test]
fn debug_output_redacts_malformed_authority_bytes() {
    let mut config = local_config();
    config.endpoint = "https://rustfs.example.com%2Ftenant-secret".to_owned();
    let debug = format!("{config:?}");
    assert!(!debug.contains("tenant-secret"));
    assert!(!debug.contains("%2F"));

    config.endpoint = "https://rustfs.example.com:tenant-secret".to_owned();
    let debug = format!("{config:?}");
    assert!(!debug.contains("tenant-secret"));
}

#[tokio::test]
async fn two_clients_share_immutable_blocks_in_one_prefix() {
    let backing = Arc::new(InMemory::new());
    let writer = RustFsBlockStore::new(backing.clone(), "run-owned/blocks", true).unwrap();
    let reader = RustFsBlockStore::new(backing, "run-owned/blocks", true).unwrap();
    assert!(writer.durable());
    assert_eq!(writer.prefix(), "run-owned/blocks");
    let id = writer
        .put(b"the same block is visible across clients")
        .await
        .unwrap();
    writer.flush().await.unwrap();
    assert_eq!(writer.stats().puts, 1);
    assert_eq!(
        reader.get(&id).await.unwrap(),
        b"the same block is visible across clients"
    );
}

#[tokio::test]
async fn direct_object_store_wrapper_cannot_assert_a_shared_rustfs_backing() {
    let backing = Arc::new(InMemory::new());
    let first = RustFsBlockStore::new(backing.clone(), "run-owned/preflight", true).unwrap();
    let second = RustFsBlockStore::new(backing, "run-owned/preflight", true).unwrap();
    assert!(
        first
            .prepare_concurrent_backing()
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup)
    );
    assert!(
        second
            .prepare_concurrent_backing()
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup)
    );
    assert_eq!(first.stats().puts, 0);
    assert_eq!(second.stats().puts, 0);
}

#[tokio::test]
async fn concurrent_preflight_rejects_unavailable_rustfs_before_open() {
    let mut config = local_config();
    config.endpoint = "http://127.0.0.1:1".to_owned();
    let blocks = RustFsBlockStore::from_config(&config, "run-owned/unavailable", true).unwrap();
    let error = blocks.prepare_concurrent_backing().await.unwrap_err();
    assert!(
        !error.is(ErrorCode::Enotsup),
        "signed RustFS preflight must try the remote backing"
    );
}

#[tokio::test]
async fn signed_backing_identity_attempts_the_unavailable_service() {
    let mut config = local_config();
    config.endpoint = "http://127.0.0.1:1".to_owned();
    let blocks = RustFsBlockStore::from_config(&config, "run-owned/unavailable-id", true).unwrap();
    let error = blocks.prepare_concurrent_backing().await.unwrap_err();
    assert!(
        !error.is(ErrorCode::Enotsup),
        "a signed RustFS client must attempt a remote claim before MRC2 metadata opens"
    );
}
