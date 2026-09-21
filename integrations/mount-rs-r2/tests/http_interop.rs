//! The R2 adapter's real signed S3 HTTP client against our driver-backed gateway.
//! This is interoperability evidence, deliberately not live Cloudflare proof.
use async_trait::async_trait;
use mount_rs_core::storage::BlockStore;
use mount_rs_core::{Capabilities, ErrorCode, FileHandle, FsDriver, FsError, Loopback, MemoryFs};
use mount_rs_persist::PersistedFs;
use mount_rs_r2::{R2BlockStore, R2Config, R2Store};
use mount_rs_s3::{Credentials, S3Server, S3ServerOptions, S3Session, S3SessionOptions};
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectStore, PutMode, PutOptions, PutPayload, UpdateVersion};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone)]
struct FlakyReadOpenDriver {
    inner: Arc<MemoryFs>,
    remaining_read_failures: Arc<AtomicUsize>,
}

#[async_trait]
impl FsDriver for FlakyReadOpenDriver {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn stat(&self, path: &str) -> mount_rs_core::Result<mount_rs_core::Stats> {
        self.inner.stat(path).await
    }

    async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<mount_rs_core::DirEntry>> {
        self.inner.readdir(path).await
    }

    async fn open(
        &self,
        path: &str,
        flags: &str,
        mode: u32,
    ) -> mount_rs_core::Result<Arc<dyn FileHandle>> {
        if flags == "r"
            && self
                .remaining_read_failures
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |remaining| {
                    if remaining > 0 {
                        Some(remaining - 1)
                    } else {
                        None
                    }
                })
                .is_ok()
        {
            return Err(FsError::backend("transient injected read failure"));
        }
        self.inner.open(path, flags, mode).await
    }

    async fn mkdir(
        &self,
        path: &str,
        options: mount_rs_core::MkdirOptions,
    ) -> mount_rs_core::Result<Option<String>> {
        self.inner.mkdir(path, options).await
    }

    async fn unlink(&self, path: &str) -> mount_rs_core::Result<()> {
        self.inner.unlink(path).await
    }

    async fn rename(&self, old_path: &str, new_path: &str) -> mount_rs_core::Result<()> {
        self.inner.rename(old_path, new_path).await
    }
}

#[tokio::test]
async fn signed_r2_client_retries_transient_read_over_http() {
    let remaining_read_failures = Arc::new(AtomicUsize::new(1));
    let session = S3Session::new_with_options(
        FlakyReadOpenDriver {
            inner: Arc::new(MemoryFs::empty()),
            remaining_read_failures: Arc::clone(&remaining_read_failures),
        },
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
        state_key: "http-retry/state.json".to_owned(),
    };
    let blocks = R2BlockStore::from_config(&config, "http-retry/blocks").unwrap();
    let object_store = config.build_store().unwrap();
    let payload = b"retryable R2 block";
    let id = blocks.put(payload).await.unwrap();

    // Reopen the block client so this assertion exercises the signed HTTP
    // retry path rather than the process-local immutable-block cache.
    let fresh = R2BlockStore::from_config(&config, "http-retry/blocks").unwrap();
    assert_eq!(fresh.get(&id).await.unwrap(), payload);
    assert_eq!(remaining_read_failures.load(Ordering::Acquire), 0);

    fresh.delete(&id).await.unwrap();
    let remaining = object_store
        .list_with_delimiter(Some(&ObjectPath::from("http-retry/blocks")))
        .await
        .unwrap();
    let unexpected_objects = remaining
        .objects
        .iter()
        .filter(|object| object.location.as_ref() != "http-retry/blocks")
        .collect::<Vec<_>>();
    assert!(
        unexpected_objects.is_empty() && remaining.common_prefixes.is_empty(),
        "HTTP retry objects remained after exact cleanup: {unexpected_objects:?}"
    );
    server.close().await.unwrap();
}

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
    let object_store = config.build_store().unwrap();
    let payload = (0..1_048_593).map(|i| (i * 37) as u8).collect::<Vec<_>>();
    let id = blocks.put(&payload).await.unwrap();
    blocks.flush().await.unwrap();
    // Rebuild the signed HTTP client, not just the filesystem wrapper.
    let fresh = R2BlockStore::from_config(&config, "http-test/blocks").unwrap();
    assert_eq!(fresh.get(&id).await.unwrap(), payload);
    let path = ObjectPath::from(format!("http-test/blocks/{}", id.0));
    assert_eq!(
        object_store
            .get(&path)
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        payload
    );
    assert_eq!(
        object_store
            .get_range(&path, 17..63)
            .await
            .unwrap()
            .as_ref(),
        &payload[17..63]
    );
    let ranges = object_store
        .get_ranges(&path, &[0..9, 500_000..500_017])
        .await
        .unwrap();
    assert_eq!(ranges[0].as_ref(), &payload[0..9]);
    assert_eq!(ranges[1].as_ref(), &payload[500_000..500_017]);

    let immutable_overwrite = object_store
        .put_opts(
            &path,
            PutPayload::from(b"must-not-overwrite".to_vec()),
            PutOptions {
                mode: PutMode::Create,
                ..Default::default()
            },
        )
        .await
        .expect_err("the HTTP S3 gateway must preserve immutable block publication");
    assert!(matches!(
        immutable_overwrite,
        object_store::Error::AlreadyExists { .. } | object_store::Error::Precondition { .. }
    ));

    let conditional_path = ObjectPath::from("http-test/blocks/conditional-object");
    object_store
        .put_opts(
            &conditional_path,
            PutPayload::from(b"first conditional value".to_vec()),
            PutOptions {
                mode: PutMode::Create,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let conditional_meta = object_store.head(&conditional_path).await.unwrap();
    let stale_read = object_store
        .get_opts(
            &conditional_path,
            GetOptions {
                if_match: Some("\"not-the-current-etag\"".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect_err("the HTTP S3 gateway must reject a stale conditional read");
    assert!(matches!(
        stale_read,
        object_store::Error::Precondition { .. }
    ));
    let stale_write = object_store
        .put_opts(
            &conditional_path,
            PutPayload::from(b"must-not-win-the-CAS".to_vec()),
            PutOptions {
                mode: PutMode::Update(UpdateVersion {
                    e_tag: Some("\"not-the-current-etag\"".to_owned()),
                    version: None,
                }),
                ..Default::default()
            },
        )
        .await
        .expect_err("the HTTP S3 gateway must reject a stale conditional write");
    assert!(matches!(
        stale_write,
        object_store::Error::Precondition { .. }
    ));
    object_store
        .put_opts(
            &conditional_path,
            PutPayload::from(b"second conditional value".to_vec()),
            PutOptions {
                mode: PutMode::Update(UpdateVersion {
                    e_tag: conditional_meta.e_tag,
                    version: conditional_meta.version,
                }),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let mut workers = Vec::new();
    for worker in 0..8_u8 {
        let config = config.clone();
        workers.push(tokio::spawn(async move {
            let blocks = R2BlockStore::from_config(&config, "http-test/blocks").unwrap();
            let body = vec![worker; 4096];
            let id = blocks.put(&body).await.unwrap();
            (id, body)
        }));
    }
    let mut concurrent = Vec::new();
    for worker in workers {
        let (worker_id, body) = worker.await.unwrap();
        assert_eq!(fresh.get(&worker_id).await.unwrap(), body);
        concurrent.push(worker_id);
    }

    let replacement = fresh.put(b"independent immutable object").await.unwrap();
    assert_ne!(replacement, id);
    assert_eq!(blocks.get(&id).await.unwrap(), payload);
    let other_prefix = R2BlockStore::from_config(&config, "http-test/other").unwrap();
    assert_eq!(
        other_prefix.get(&id).await.unwrap_err().code,
        ErrorCode::Enoent
    );
    for worker_id in concurrent {
        fresh.delete(&worker_id).await.unwrap();
    }
    fresh.delete(&id).await.unwrap();
    fresh.delete(&replacement).await.unwrap();
    object_store.delete(&conditional_path).await.unwrap();
    let remaining = object_store
        .list_with_delimiter(Some(&ObjectPath::from("http-test/blocks")))
        .await
        .unwrap();
    let unexpected_objects = remaining
        .objects
        .iter()
        .filter(|object| object.location.as_ref() != "http-test/blocks")
        .map(|object| object.location.to_string())
        .collect::<Vec<_>>();
    assert!(
        unexpected_objects.is_empty() && remaining.common_prefixes.is_empty(),
        "HTTP test objects remained after exact cleanup: {unexpected_objects:?}"
    );
    assert_eq!(fresh.get(&id).await.unwrap_err().code, ErrorCode::Enoent);
    server.close().await.unwrap();
}
