#![cfg(test)]
//! Opt-in acceptance against the actual AWS S3 service.
//!
//! The shell harness owns the bucket/prefix boundary and cleanup. These tests
//! deliberately build a fresh AWS client from short-lived credentials supplied
//! by the environment; they do not create IAM users, access keys, buckets, or
//! policies.

use std::env;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use mount_rs_aws_s3::{AwsS3BlockStore, AwsS3Config};
use mount_rs_core::storage::{BlockId, BlockStore};
use mount_rs_core::{Loopback, MkdirOptions};
use mount_rs_sdk::{Filesystem, SplitOptions, StoreConfig};
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectStore, PutMode, PutOptions, PutPayload, UpdateVersion};

const TEST_TIMEOUT: Duration = Duration::from_secs(180);

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set by scripts/test-aws-s3.sh"))
}

fn test_prefix() -> String {
    required_env("AWS_S3_TEST_PREFIX")
}

fn restart_fixture() -> PathBuf {
    PathBuf::from(required_env("AWS_S3_RESTART_FIXTURE"))
}

fn metadata_fixture() -> PathBuf {
    PathBuf::from(required_env("AWS_S3_METADATA_FIXTURE"))
}

fn aws_s3_store_config(prefix: impl Into<String>) -> StoreConfig {
    StoreConfig::AwsS3 {
        bucket: required_env("AWS_S3_TEST_BUCKET"),
        region: required_env("AWS_S3_TEST_REGION"),
        prefix: prefix.into(),
        durable: true,
    }
}

fn aws_s3_split_options(
    metadata_path: PathBuf,
    prefix: impl Into<String>,
    owner: &str,
    chunk_size_bytes: usize,
) -> SplitOptions {
    SplitOptions {
        metadata: StoreConfig::Sqlite {
            path: metadata_path,
        },
        blocks: aws_s3_store_config(prefix),
        chunk_size_bytes,
        owner: owner.to_owned(),
        lease_ttl: Duration::from_secs(30),
        concurrent_writes: false,
        inode_updates: false,
        compact_inode_updates: false,
        writeback: false,
        delegated: false,
        checkout_path: None,
        uid: 0,
        gid: 0,
        umask: 0,
    }
}

fn object_path(prefix: &str, name: &str) -> ObjectPath {
    ObjectPath::from(format!("{prefix}/{name}"))
}

fn aws_store() -> Arc<dyn ObjectStore> {
    AwsS3Config {
        bucket: required_env("AWS_S3_TEST_BUCKET"),
        region: required_env("AWS_S3_TEST_REGION"),
    }
    .build_store()
    .unwrap_or_else(|error| panic!("build actual AWS S3 client: {error}"))
}

fn patterned_bytes(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| {
            let value = index as u64;
            ((value.wrapping_mul(0x9e37_79b9) ^ (value >> 3) ^ 0xa5) & 0xff) as u8
        })
        .collect()
}

fn restart_payload() -> Vec<u8> {
    patterned_bytes(65_537)
}

fn composed_expected() -> Vec<u8> {
    let mut expected = patterned_bytes(4096 * 3 + 113);

    let patch = patterned_bytes(257);
    expected[4096 + 37..4096 + 37 + patch.len()].copy_from_slice(&patch);

    expected.truncate(4096 + 19);
    expected.resize(4096 * 3 + 29, 0);

    let tail = patterned_bytes(193);
    expected[4096 * 2 + 73..4096 * 2 + 73 + tail.len()].copy_from_slice(&tail);
    expected
}

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, future)
        .await
        .expect("AWS S3 request/test exceeded the bounded timeout")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires scripts/test-aws-s3.sh and an actual AWS S3 test bucket"]
async fn actual_aws_s3_block_and_composed_filesystem() {
    bounded(async {
        let prefix = test_prefix();
        let object_store = aws_store();
        let blocks = AwsS3BlockStore::new(Arc::clone(&object_store), prefix.clone(), true).unwrap();
        assert!(blocks.durable(), "actual AWS S3 blocks must be durable");

        let payload = patterned_bytes(32_768);
        let first_id = blocks.put(&payload).await.unwrap();
        let second_id = blocks.put(&payload).await.unwrap();
        assert_ne!(
            first_id, second_id,
            "immutable block publication reused an ID"
        );
        assert_eq!(blocks.get(&first_id).await.unwrap(), payload);
        blocks.flush().await.unwrap();

        let first_path = object_path(&prefix, &first_id.0);
        assert_eq!(
            object_store
                .get_range(&first_path, 7..23)
                .await
                .unwrap()
                .as_ref(),
            &payload[7..23]
        );

        // Exercise the same create-only operation used by AwsS3BlockStore at the
        // exact block key, so an immutable block cannot be overwritten.
        let duplicate_block = object_store
            .put_opts(
                &first_path,
                PutPayload::from(b"must-not-overwrite".to_vec()),
                PutOptions {
                    mode: PutMode::Create,
                    ..Default::default()
                },
            )
            .await
            .expect_err("AWS S3 must reject an immutable block overwrite");
        assert!(
            matches!(
                duplicate_block,
                object_store::Error::AlreadyExists { .. }
                    | object_store::Error::Precondition { .. }
            ),
            "unexpected AWS S3 immutable-block error: {duplicate_block}"
        );
        assert_eq!(blocks.get(&first_id).await.unwrap(), payload);

        // Exercise create-only publication and both stale/current ETag
        // conditionals against a stable key. This is a separate mutable
        // probe; the block objects above remain immutable.
        let conditional_path = object_path(&prefix, "conditional-immutable-probe");
        let conditional_body = b"first conditional value";
        let created = object_store
            .put_opts(
                &conditional_path,
                PutPayload::from(conditional_body.to_vec()),
                PutOptions {
                    mode: PutMode::Create,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            created.e_tag.is_some(),
            "AWS S3 conditional-write coverage requires an object ETag"
        );
        let duplicate = object_store
            .put_opts(
                &conditional_path,
                PutPayload::from(b"must-not-overwrite".to_vec()),
                PutOptions {
                    mode: PutMode::Create,
                    ..Default::default()
                },
            )
            .await
            .expect_err("AWS S3 must reject a duplicate immutable create");
        assert!(
            matches!(
                duplicate,
                object_store::Error::AlreadyExists { .. }
                    | object_store::Error::Precondition { .. }
            ),
            "unexpected AWS S3 conditional-create error: {duplicate}"
        );
        let metadata = object_store.head(&conditional_path).await.unwrap();
        assert!(
            metadata.e_tag.is_some(),
            "AWS S3 conditional-read/write coverage requires an object ETag"
        );
        let stale_read = object_store
            .get_opts(
                &conditional_path,
                GetOptions {
                    if_match: Some("\"not-the-current-etag\"".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("AWS S3 must reject a stale conditional read");
        assert!(
            matches!(stale_read, object_store::Error::Precondition { .. }),
            "unexpected AWS S3 stale-read error: {stale_read}"
        );
        let stale_update = object_store
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
            .expect_err("AWS S3 must reject a stale conditional write");
        assert!(
            matches!(stale_update, object_store::Error::Precondition { .. }),
            "unexpected AWS S3 stale-write error: {stale_update}"
        );
        assert_eq!(
            object_store
                .get(&conditional_path)
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            conditional_body
        );
        let updated = object_store
            .put_opts(
                &conditional_path,
                PutPayload::from(b"second conditional value".to_vec()),
                PutOptions {
                    mode: PutMode::Update(UpdateVersion {
                        e_tag: metadata.e_tag.clone(),
                        version: metadata.version.clone(),
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            updated.e_tag.is_some(),
            "AWS S3 successful conditional writes must return an ETag"
        );
        assert_ne!(created.e_tag, updated.e_tag);
        assert_eq!(
            object_store
                .get(&conditional_path)
                .await
                .unwrap()
                .bytes()
                .await
                .unwrap()
                .as_ref(),
            b"second conditional value"
        );

        // Publish through fresh concurrent handles as well. The provider's
        // conditional create, rather than the process-local ID sequence, is
        // the authority if writers ever choose the same candidate ID.
        let shared = Arc::new(blocks.clone());
        let mut workers = Vec::new();
        for worker in 0..4_u8 {
            let blocks = Arc::clone(&shared);
            workers.push(tokio::spawn(async move {
                let body = vec![worker; 4096];
                let id = blocks.put(&body).await.unwrap();
                (id, body)
            }));
        }
        for worker in workers {
            let (id, body) = worker.await.unwrap();
            assert_eq!(shared.get(&id).await.unwrap(), body);
        }

        // Keep a small bounded workload sample in the live packet so the
        // provider is exercised across repeated fresh immutable publications,
        // not only one large write and four concurrent calls. The full
        // production load/soak target remains a deployment-specific gate.
        for round in 0..24_usize {
            let mut body = patterned_bytes(4096 + round * 257);
            body[0] ^= round as u8;
            let id = blocks.put(&body).await.unwrap();
            assert_eq!(blocks.get(&id).await.unwrap(), body);
        }
        blocks.flush().await.unwrap();

        let restart_id = blocks.put(&restart_payload()).await.unwrap();
        std::fs::write(restart_fixture(), &restart_id.0).unwrap();

        let metadata_path = metadata_fixture();
        let composed_prefix = format!("{prefix}/composed/blocks");
        let filesystem = Filesystem::split(aws_s3_split_options(
            metadata_path.clone(),
            composed_prefix.clone(),
            "aws-s3-compose",
            4096,
        ))
        .await
        .unwrap();
        let loopback = Loopback::from_arc(filesystem.driver());
        assert!(loopback.capabilities.durable_writes);
        loopback
            .mkdir("/composed", MkdirOptions::default())
            .await
            .unwrap();
        let file = loopback.open("/composed/file", "w+", 0o640).await.unwrap();
        let mut expected = patterned_bytes(4096 * 3 + 113);
        file.write(&expected, Some(0)).await.unwrap();

        let patch = patterned_bytes(257);
        file.write(&patch, Some(4096 + 37)).await.unwrap();
        expected[4096 + 37..4096 + 37 + patch.len()].copy_from_slice(&patch);

        file.truncate(4096 + 19).await.unwrap();
        expected.truncate(4096 + 19);
        file.truncate(4096 * 3 + 29).await.unwrap();
        expected.resize(4096 * 3 + 29, 0);

        let tail = patterned_bytes(193);
        file.write(&tail, Some(4096 * 2 + 73)).await.unwrap();
        expected[4096 * 2 + 73..4096 * 2 + 73 + tail.len()].copy_from_slice(&tail);
        file.sync().await.unwrap();

        let range_start = 4096_usize + 37;
        let mut range = vec![0; 211];
        assert_eq!(
            file.read(&mut range, Some(range_start as u64))
                .await
                .unwrap(),
            range.len()
        );
        assert_eq!(&range, &expected[range_start..range_start + range.len()]);
        file.close().await.unwrap();
        drop(loopback);
        filesystem.shutdown().await.unwrap();

        // Reopen both providers through fresh handles. The metadata database
        // is persisted locally; the immutable bytes are fetched from a new
        // signed AWS S3 client, so this cannot be satisfied by an in-process
        // object-store cache.
        let reopened = Filesystem::split(aws_s3_split_options(
            metadata_path,
            composed_prefix,
            "aws-s3-compose-reopen",
            65_536,
        ))
        .await
        .unwrap();
        let reopened_loopback = Loopback::from_arc(reopened.driver());
        assert_eq!(
            reopened_loopback.read_file("/composed/file").await.unwrap(),
            expected
        );
        let reopened_file = reopened_loopback
            .open("/composed/file", "r", 0)
            .await
            .unwrap();
        let mut reopened_range = vec![0; 211];
        assert_eq!(
            reopened_file
                .read(&mut reopened_range, Some(range_start as u64))
                .await
                .unwrap(),
            reopened_range.len()
        );
        assert_eq!(
            &reopened_range,
            &expected[range_start..range_start + reopened_range.len()]
        );
        reopened_file.close().await.unwrap();
        drop(reopened_loopback);
        reopened.shutdown().await.unwrap();

        println!("AWS_S3_BLOCK_COMPOSED_PASS prefix={prefix}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires the first process from scripts/test-aws-s3.sh"]
async fn actual_aws_s3_reopen_after_process_restart() {
    bounded(async {
        let block_id = BlockId(
            std::fs::read_to_string(restart_fixture())
                .unwrap()
                .trim()
                .to_owned(),
        );
        let blocks = AwsS3BlockStore::new(aws_store(), test_prefix(), true).unwrap();
        assert_eq!(blocks.get(&block_id).await.unwrap(), restart_payload());

        let object_store = aws_store();
        let prefix = test_prefix();
        let path = object_path(&prefix, &block_id.0);
        assert_eq!(
            object_store
                .get_range(&path, 11..29)
                .await
                .unwrap()
                .as_ref(),
            &restart_payload()[11..29]
        );

        // Reopen the composed filesystem through a new process, metadata
        // connection, and signed S3 client. This is the cross-process
        // boundary for the independent metadata and block providers.
        let composed_prefix = format!("{prefix}/composed/blocks");
        let reopened = Filesystem::split(aws_s3_split_options(
            metadata_fixture(),
            composed_prefix,
            "aws-s3-process-reopen",
            65_536,
        ))
        .await
        .unwrap();
        let reopened_loopback = Loopback::from_arc(reopened.driver());
        let expected = composed_expected();
        assert_eq!(
            reopened_loopback.read_file("/composed/file").await.unwrap(),
            expected
        );
        let reopened_file = reopened_loopback
            .open("/composed/file", "r", 0)
            .await
            .unwrap();
        let range_start = 4096_usize + 37;
        let mut range = vec![0; 211];
        assert_eq!(
            reopened_file
                .read(&mut range, Some(range_start as u64))
                .await
                .unwrap(),
            range.len()
        );
        assert_eq!(&range, &expected[range_start..range_start + range.len()]);
        reopened_file.close().await.unwrap();
        drop(reopened_loopback);
        reopened.shutdown().await.unwrap();

        println!("AWS_S3_PROCESS_REOPEN_PASS prefix={prefix}");
    })
    .await;
}
