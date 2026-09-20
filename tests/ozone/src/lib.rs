#![cfg(test)]
//! Real Apache Ozone S3 Gateway coverage for the immutable block provider.
//!
//! The shell harness owns an isolated, loopback-only all-in-one Ozone service.
//! These tests intentionally retain the same provider invariants as the
//! RustFS gate: every block is create-only, stale conditional writes fail, and
//! a successful block publication can be read by a fresh client.

mod chunked_composition;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use mount_rs_core::ErrorCode;
use mount_rs_core::storage::{BlockId, BlockStore};
use mount_rs_r2::{R2BlockStore, R2Config};
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectStore, PutMode, PutOptions, PutPayload, UpdateVersion};

const TEST_TIMEOUT: Duration = Duration::from_secs(120);
const GATEWAY_FAILURE_TIMEOUT: Duration = Duration::from_secs(5);

fn local_config() -> R2Config {
    let config = R2Config::from_env().expect("Ozone R2-compatible test environment is required");
    assert!(
        config.endpoint.starts_with("http://127.0.0.1:")
            || config.endpoint.starts_with("http://localhost:"),
        "Ozone integration tests only accept a loopback endpoint; refusing {}",
        config.endpoint
    );
    assert!(
        std::env::var("OZONE_TEST_PREFIX").is_ok(),
        "OZONE_TEST_PREFIX must identify this test run"
    );
    config
}

fn test_prefix() -> String {
    std::env::var("OZONE_TEST_PREFIX").expect("OZONE_TEST_PREFIX must be set")
}

fn fixture_path() -> PathBuf {
    std::env::var_os("OZONE_FIXTURE_FILE")
        .map(PathBuf::from)
        .expect("OZONE_FIXTURE_FILE must be set")
}

fn object_path(prefix: &str, name: &str) -> ObjectPath {
    ObjectPath::from(format!("{prefix}/{name}"))
}

fn block_id(value: &str) -> BlockId {
    BlockId(value.to_owned())
}

async fn assert_timeout<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(TEST_TIMEOUT, future)
        .await
        .expect("Ozone request/test exceeded its bounded timeout")
}

#[tokio::test]
async fn real_ozone_block_contract() {
    assert_timeout(async {
        let config = local_config();
        let prefix = test_prefix();
        let blocks = R2BlockStore::from_config(&config, prefix.clone()).unwrap();
        assert!(blocks.durable());
        let object_store = config.build_store().unwrap();

        let payload = (0_u32..32_768)
            .map(|index| (index.wrapping_mul(37) & 0xff) as u8)
            .collect::<Vec<_>>();
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

        let missing = block_id("b00000000000000000000000000000000");
        assert!(
            blocks
                .get(&missing)
                .await
                .unwrap_err()
                .is(ErrorCode::Enoent),
            "missing Ozone blocks must map to ENOENT"
        );

        let conditional_name = "conditional-object";
        let conditional_path = object_path(&prefix, conditional_name);
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
            .expect_err("Ozone must enforce create-only publication");
        assert!(
            matches!(
                duplicate,
                object_store::Error::AlreadyExists { .. }
                    | object_store::Error::Precondition { .. }
            ),
            "unexpected Ozone conditional-create error: {duplicate}"
        );

        let metadata = object_store.head(&conditional_path).await.unwrap();
        assert!(
            metadata.e_tag.is_some(),
            "Ozone conditional-write coverage requires an object ETag"
        );
        let wrong_read = object_store
            .get_opts(
                &conditional_path,
                GetOptions {
                    if_match: Some("\"not-the-current-etag\"".to_owned()),
                    ..Default::default()
                },
            )
            .await
            .expect_err("Ozone must reject a stale conditional read");
        assert!(
            matches!(wrong_read, object_store::Error::Precondition { .. }),
            "unexpected Ozone stale-read error: {wrong_read}"
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
            .expect_err("Ozone must reject a stale conditional write");
        assert!(
            matches!(stale_update, object_store::Error::Precondition { .. }),
            "unexpected Ozone stale-write error: {stale_update}"
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

        let current_version = UpdateVersion {
            e_tag: metadata.e_tag.clone(),
            version: metadata.version.clone(),
        };
        let updated = object_store
            .put_opts(
                &conditional_path,
                PutPayload::from(b"second conditional value".to_vec()),
                PutOptions {
                    mode: PutMode::Update(current_version),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert!(
            created.e_tag.is_some() && updated.e_tag.is_some(),
            "Ozone successful conditional writes must return ETags"
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

        let shared = Arc::new(blocks.clone());
        let mut workers = Vec::new();
        for worker in 0..8_u8 {
            let blocks = Arc::clone(&shared);
            workers.push(tokio::spawn(async move {
                let body = vec![worker; 4096];
                let id = blocks.put(&body).await.unwrap();
                (id, body)
            }));
        }
        let mut concurrent_ids = Vec::new();
        for worker in workers {
            let (id, body) = worker.await.unwrap();
            assert_eq!(shared.get(&id).await.unwrap(), body);
            concurrent_ids.push(id);
        }

        let restart_body = b"survives a fresh Ozone client after service restart".to_vec();
        let restart_id = blocks.put(&restart_body).await.unwrap();
        let fixture = fixture_path();
        std::fs::write(
            &fixture,
            format!("{}\n{}\n", restart_id.0, hex(&restart_body)),
        )
        .unwrap();

        for id in concurrent_ids.into_iter().chain([first_id, second_id]) {
            blocks.delete(&id).await.unwrap();
        }
        object_store.delete(&conditional_path).await.unwrap();
        println!(
            "OZONE_BLOCK_CONTRACT_PASS prefix={} committed_object={} restart_fixture={}",
            prefix,
            restart_id.0,
            fixture.display()
        );
    })
    .await;
}

#[tokio::test]
async fn real_ozone_reopen_after_service_restart() {
    assert_timeout(async {
        let config = local_config();
        let prefix = test_prefix();
        let fixture = fixture_path();
        let lines = std::fs::read_to_string(&fixture).expect("restart fixture must exist");
        let mut lines = lines.lines();
        let id = block_id(lines.next().expect("restart fixture block ID"));
        let expected = decode_hex(lines.next().expect("restart fixture payload"));
        let blocks = R2BlockStore::from_config(&config, prefix.clone()).unwrap();
        assert_eq!(blocks.get(&id).await.unwrap(), expected);
        blocks.delete(&id).await.unwrap();
        std::fs::remove_file(&fixture).unwrap();
        println!("OZONE_RESTART_REOPEN_PASS prefix={} block={}", prefix, id.0);
    })
    .await;
}

#[tokio::test]
async fn real_ozone_gateway_failure_is_bounded() {
    assert_timeout(async {
        let config = local_config();
        let prefix = test_prefix();
        let object_store = config.build_store().unwrap();
        let fixture = std::fs::read_to_string(fixture_path())
            .expect("restart fixture must exist during the gateway fault window");
        let restart_id = fixture.lines().next().expect("restart fixture block ID");
        let path = object_path(&prefix, restart_id);

        let result = tokio::time::timeout(GATEWAY_FAILURE_TIMEOUT, object_store.head(&path)).await;
        match result {
            Ok(Ok(_)) => panic!("Ozone served a read while its gateway was stopped"),
            Ok(Err(object_store::Error::NotFound { .. })) => {
                panic!("Ozone gateway answered NotFound while its service was stopped")
            }
            Ok(Err(error)) => {
                println!("OZONE_GATEWAY_FAILURE_PASS prefix={} error={error}", prefix)
            }
            Err(_) => {
                panic!("Ozone gateway request did not fail within {GATEWAY_FAILURE_TIMEOUT:?}")
            }
        }
    })
    .await;
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_hex(value: &str) -> Vec<u8> {
    assert!(value.len().is_multiple_of(2), "fixture payload is not hex");
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = char::from(pair[0]).to_digit(16).expect("fixture hex digit");
            let low = char::from(pair[1]).to_digit(16).expect("fixture hex digit");
            ((high << 4) | low) as u8
        })
        .collect()
}
