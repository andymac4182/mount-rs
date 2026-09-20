//! The R2 adapter's real signed S3 HTTP client against our driver-backed gateway.
//! This is interoperability evidence, deliberately not live Cloudflare proof.
use mount_rs_core::storage::BlockStore;
use mount_rs_core::{ErrorCode, Loopback, MemoryFs};
use mount_rs_persist::PersistedFs;
use mount_rs_r2::{R2BlockStore, R2Config, R2Store};
use mount_rs_s3::{Credentials, S3Server, S3ServerOptions, S3Session, S3SessionOptions};
use std::sync::Arc;

#[tokio::test]
async fn signed_r2_client_reopens_and_rejects_stale_writes_over_http() {
    let session = S3Session::new_with_options(
        MemoryFs::empty(),
        S3SessionOptions {
            credentials: Some(Credentials::new("fixture-key", "fixture-secret")),
            region: Some("auto".to_owned()),
            ..Default::default()
        },
    );
    let server = S3Server::start(Arc::new(session), S3ServerOptions::default())
        .await
        .unwrap();
    let config = R2Config {
        endpoint: format!("http://{}", server.address()),
        bucket: "mountx".to_owned(),
        access_key_id: "fixture-key".to_owned(),
        secret_access_key: "fixture-secret".to_owned(),
        state_key: "http-test/state.json".to_owned(),
    };
    let store = R2Store::from_config(&config).unwrap();
    let first = Loopback::new(PersistedFs::open(store.clone()).await.unwrap());
    first.write_file("/file", &[0, 255, 3]).await.unwrap();
    let second = Loopback::new(PersistedFs::open(store.clone()).await.unwrap());
    assert_eq!(second.read_file("/file").await.unwrap(), [0, 255, 3]);
    // Closing the reader persists its access-time metadata, so reopen the
    // intended winning writer at that revision before testing a stale writer.
    let first = Loopback::new(PersistedFs::open(store.clone()).await.unwrap());
    first.write_file("/file", b"new").await.unwrap();
    let error = second.write_file("/file", b"stale").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Eagain);
    let reopened = Loopback::new(PersistedFs::open(store.clone()).await.unwrap());
    assert_eq!(reopened.read_file("/file").await.unwrap(), b"new");
    store.delete_snapshot().await.unwrap();
    let blocks = R2BlockStore::from_config(&config, "http-test/blocks").unwrap();
    let payload = (0..1_048_593).map(|i| (i * 37) as u8).collect::<Vec<_>>();
    let id = blocks.put(&payload).await.unwrap();
    blocks.flush().await.unwrap();
    // Rebuild the signed HTTP client, not just the filesystem wrapper.
    let fresh = R2BlockStore::from_config(&config, "http-test/blocks").unwrap();
    assert_eq!(fresh.get(&id).await.unwrap(), payload);
    let replacement = fresh.put(b"independent immutable object").await.unwrap();
    assert_ne!(replacement, id);
    assert_eq!(blocks.get(&id).await.unwrap(), payload);
    let other_prefix = R2BlockStore::from_config(&config, "http-test/other").unwrap();
    assert_eq!(
        other_prefix.get(&id).await.unwrap_err().code,
        ErrorCode::Enoent
    );
    fresh.delete(&id).await.unwrap();
    fresh.delete(&replacement).await.unwrap();
    assert_eq!(blocks.get(&id).await.unwrap_err().code, ErrorCode::Enoent);
    server.close().await.unwrap();
}
