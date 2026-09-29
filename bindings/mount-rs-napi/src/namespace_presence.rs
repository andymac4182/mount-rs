use std::future::Future;
use std::time::{Duration, Instant};

use mount_rs_core::{ErrorCode, FsError};
use mount_rs_rustfs::RustFsConfig;
use mount_rs_tidb::{TidbNamespacePresence, TidbPoolContext};

use crate::{CoreResult, JsChunkedStoreOptions, to_js_error};

const PROBE_DEADLINE: Duration = Duration::from_secs(30);
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(15);

struct PresenceInput {
    metadata_uri: String,
    metadata_key: String,
    blob_prefix: String,
    rustfs: RustFsConfig,
}

#[derive(Clone, Copy)]
struct Limits {
    probe: Duration,
    shutdown: Duration,
}

fn parse_input(
    metadata: JsChunkedStoreOptions,
    blocks: JsChunkedStoreOptions,
) -> napi::Result<PresenceInput> {
    if metadata.journal_mode.is_some()
        || blocks.journal_mode.is_some()
        || metadata.kind != "tidb"
        || blocks.kind != "rustfs"
        || metadata.lease_authority.is_some()
        || metadata.authority_prefix.is_some()
        || metadata.endpoint.is_some()
        || metadata.bucket.is_some()
        || metadata.region.is_some()
        || metadata.access_key_id.is_some()
        || metadata.secret_access_key.is_some()
        || blocks.uri.is_some()
        || blocks.lease_authority.is_some()
        || blocks.authority_prefix.is_some()
    {
        return Err(configuration_error());
    }
    let metadata_uri = required(metadata.uri)?;
    let metadata_key = required(metadata.key)?;
    let blob_prefix = required(blocks.key)?;
    validate_scope(&metadata_key, 255)?;
    validate_scope(&blob_prefix, 512)?;
    let rustfs = RustFsConfig {
        endpoint: required(blocks.endpoint)?,
        bucket: required(blocks.bucket)?,
        region: required(blocks.region)?,
        access_key_id: required(blocks.access_key_id)?,
        secret_access_key: required(blocks.secret_access_key)?,
    };
    rustfs.validate().map_err(|_| configuration_error())?;
    Ok(PresenceInput {
        metadata_uri,
        metadata_key,
        blob_prefix,
        rustfs,
    })
}

fn configuration_error() -> napi::Error {
    to_js_error(
        FsError::new(ErrorCode::Einval)
            .with_syscall("inspectSplitNamespacePresence")
            .with_message("namespace presence configuration invalid"),
    )
}

fn required(value: Option<String>) -> napi::Result<String> {
    value
        .filter(|value| !value.is_empty())
        .ok_or_else(configuration_error)
}

fn validate_scope(scope: &str, max_bytes: usize) -> napi::Result<()> {
    if scope.len() > max_bytes
        || !scope
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/'))
        || scope.split('/').any(|part| matches!(part, "" | "." | ".."))
    {
        return Err(configuration_error());
    }
    Ok(())
}

fn observation_error(stage: &str, shutdown_confirmed: bool) -> napi::Error {
    let shutdown = if shutdown_confirmed {
        "shutdown_confirmed"
    } else {
        "shutdown_unconfirmed"
    };
    to_js_error(
        FsError::new(ErrorCode::Eio)
            .with_syscall("inspectSplitNamespacePresence")
            .with_message(format!("namespace presence {stage} failed;{shutdown}")),
    )
}

async fn inspect_operations<M, B, C, MF, BF, CF>(
    metadata: M,
    blobs: B,
    close: C,
    limits: Limits,
) -> napi::Result<String>
where
    M: FnOnce() -> MF,
    B: FnOnce() -> BF,
    C: FnOnce() -> CF,
    MF: Future<Output = CoreResult<TidbNamespacePresence>>,
    BF: Future<Output = CoreResult<bool>>,
    CF: Future<Output = CoreResult<()>>,
{
    let started = Instant::now();
    // Await only one observation at a time. A timeout drops the probe future
    // before pool shutdown starts. Elapsed checks also reject a late result
    // from synchronous parsing or work that cannot yield to Tokio's timer.
    let probe = tokio::time::timeout(limits.probe, async {
        let rows = metadata().await.map_err(|_| "metadata")?;
        let metadata_elapsed = started.elapsed();
        if metadata_elapsed > limits.probe {
            return Err("probe_deadline");
        }
        let prefix_absent = blobs().await.map_err(|_| "blobs")?;
        let blobs_elapsed = started.elapsed();
        if blobs_elapsed > limits.probe {
            return Err("probe_deadline");
        }
        Ok((rows, prefix_absent, metadata_elapsed, blobs_elapsed))
    })
    .await
    .unwrap_or(Err("probe_deadline"));

    let shutdown_started = Instant::now();
    let shutdown = tokio::time::timeout(limits.shutdown, async { close().await }).await;
    let shutdown_elapsed = shutdown_started.elapsed();
    let shutdown_confirmed = matches!(shutdown, Ok(Ok(()))) && shutdown_elapsed <= limits.shutdown;
    let (rows, prefix_absent, metadata_elapsed, blobs_elapsed) =
        probe.map_err(|stage| observation_error(stage, shutdown_confirmed))?;
    if !shutdown_confirmed {
        return Err(observation_error("pool_shutdown", false));
    }
    // Fixed projection only: no URLs, keys, bucket names, credentials, or
    // arbitrary provider errors cross the diagnostic boundary.
    Ok(serde_json::json!({
        "schema": "mount-rs.split-namespace-presence.v1",
        "namespace_absent": rows.is_absent() && prefix_absent,
        "metadata": {
            "provider": "tidb",
            "key_scope": "exact_input_utf8_bytes",
            "schema_setup": "shared_ddl_and_session_configuration",
            "row_presence": {
                "metadata": rows.metadata, "inodes": rows.inodes,
                "compact_guards": rows.compact_guards,
                "block_authority": rows.block_authority, "blocks": rows.blocks,
            },
            "observed_at_ns": metadata_elapsed.as_nanos().to_string(),
        },
        "blobs": {
            "provider": "rustfs",
            "scope": "canonical_ascii_prefix_descendants",
            "observation": "signed_list_page_api",
            "requested_max_keys": 1,
            "prefix_absent": prefix_absent,
            "observed_at_ns": blobs_elapsed.as_nanos().to_string(),
        },
        "pool_shutdown": { "confirmed": true, "elapsed_ns": shutdown_elapsed.as_nanos().to_string() },
        "clock": "elapsed_monotonic_since_native_preflight_start",
        "consistency": "separate_observations_no_reservation",
        "limits": {
            "probe_deadline_ms": 30000, "pool_shutdown_deadline_ms": 15000,
            "list_request_keys": 1, "response_byte_cap": "unavailable",
            "server_truncation_flag": "unavailable",
            "deadline_semantics": "cooperative_await_with_elapsed_recheck",
        },
    }).to_string())
}

pub(super) async fn inspect(
    metadata: JsChunkedStoreOptions,
    blocks: JsChunkedStoreOptions,
) -> napi::Result<String> {
    let input = parse_input(metadata, blocks)?;
    // Pool construction is lazy and validates its URL before any checkout.
    // Do not open a metadata store here: that would insert a namespace row.
    let pool = TidbPoolContext::new(&input.metadata_uri, 1).map_err(|_| configuration_error())?;
    let metadata_pool = pool.clone();
    inspect_operations(
        move || async move {
            metadata_pool
                .inspect_namespace_presence(&input.metadata_key)
                .await
        },
        move || async move {
            input
                .rustfs
                .observe_owned_prefix_absence(&input.blob_prefix)
                .await
        },
        move || async move { pool.close().await },
        Limits {
            probe: PROBE_DEADLINE,
            shutdown: SHUTDOWN_DEADLINE,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_core::FsError;
    use serde_json::Value;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    fn stores() -> (JsChunkedStoreOptions, JsChunkedStoreOptions) {
        let metadata = JsChunkedStoreOptions {
            journal_mode: None,
            kind: "tidb".into(),
            uri: Some("mysql://private-user:private-password@127.0.0.1:14000/storage".into()),
            key: Some("storage-benchmark/owned-a1/tidb-rustfs/metadata".into()),
            durable: Some(true),
            lease_authority: None,
            authority_prefix: None,
            endpoint: None,
            bucket: None,
            region: None,
            access_key_id: None,
            secret_access_key: None,
        };
        let blocks = JsChunkedStoreOptions {
            journal_mode: None,
            kind: "rustfs".into(),
            uri: None,
            key: Some("storage-benchmark/owned-a1/tidb-rustfs/blocks".into()),
            durable: Some(true),
            lease_authority: None,
            authority_prefix: None,
            endpoint: Some("http://127.0.0.1:19000".into()),
            bucket: Some("private-bucket".into()),
            region: Some("us-east-1".into()),
            access_key_id: Some("private-access".into()),
            secret_access_key: Some("private-secret".into()),
        };
        (metadata, blocks)
    }

    fn absent() -> TidbNamespacePresence {
        TidbNamespacePresence {
            metadata: false,
            inodes: false,
            compact_guards: false,
            block_authority: false,
            blocks: false,
        }
    }

    fn limits() -> Limits {
        Limits {
            probe: Duration::from_secs(30),
            shutdown: Duration::from_secs(15),
        }
    }

    fn message(error: &napi::Error) -> String {
        let Some(encoded) = error.reason.strip_prefix("__mount_rs_error_v1__|") else {
            return error.reason.clone();
        };
        let text = encoded.split('|').nth(5).expect("structured error message");
        let bytes: Vec<u8> = text
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect();
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn permitted_configuration_parses_without_opening_either_backend() {
        let (metadata, blocks) = stores();
        let input = parse_input(metadata, blocks)
            .expect("configured pre-open inspection input must be accepted");
        assert_eq!(
            input.metadata_key,
            "storage-benchmark/owned-a1/tidb-rustfs/metadata"
        );
        assert_eq!(
            input.blob_prefix,
            "storage-benchmark/owned-a1/tidb-rustfs/blocks"
        );
        assert!(input.metadata_uri.starts_with("mysql://"));
        assert_eq!(input.rustfs.bucket, "private-bucket");
    }

    #[test]
    fn invalid_scope_or_backend_fields_are_refused_before_opening() {
        for scope in [
            "", "/a", "a/", "a//b", "a/../b", ".", "..", "a%2Fb", "a:b", "a\\b", "a\0b", "目录",
        ] {
            for metadata_scope in [true, false] {
                let (mut metadata, mut blocks) = stores();
                if metadata_scope {
                    metadata.key = Some(scope.into());
                } else {
                    blocks.key = Some(scope.into());
                }
                assert!(
                    parse_input(metadata, blocks).is_err(),
                    "unsafe scope was accepted"
                );
            }
        }
        let (mut metadata, blocks) = stores();
        metadata.kind = "pglite".into();
        assert!(parse_input(metadata, blocks).is_err());
        let (metadata, mut blocks) = stores();
        blocks.kind = "r2".into();
        assert!(parse_input(metadata, blocks).is_err());
        let (mut metadata, blocks) = stores();
        metadata.endpoint = Some("private-detail".into());
        assert!(parse_input(metadata, blocks).is_err());
        let (metadata, mut blocks) = stores();
        blocks.uri = Some("private-detail".into());
        assert!(parse_input(metadata, blocks).is_err());
        let (mut metadata, blocks) = stores();
        metadata.key = Some("a".repeat(256));
        assert!(parse_input(metadata, blocks).is_err());
        let (metadata, mut blocks) = stores();
        blocks.key = Some("a".repeat(513));
        assert!(parse_input(metadata, blocks).is_err());
    }

    #[tokio::test]
    async fn absence_receipt_closes_pool_after_separate_observations_and_has_only_fixed_fields() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (m, b, c) = (events.clone(), events.clone(), events.clone());
        let raw = inspect_operations(
            move || async move {
                m.lock().unwrap().push("metadata");
                Ok(absent())
            },
            move || async move {
                b.lock().unwrap().push("blobs");
                Ok(true)
            },
            move || async move {
                c.lock().unwrap().push("close");
                Ok(())
            },
            limits(),
        )
        .await
        .expect("API-observed absence must produce a receipt after confirmed pool shutdown");
        assert_eq!(*events.lock().unwrap(), ["metadata", "blobs", "close"]);
        let receipt: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(receipt["schema"], "mount-rs.split-namespace-presence.v1");
        assert_eq!(receipt["namespace_absent"], true);
        assert_eq!(receipt["pool_shutdown"]["confirmed"], true);
        assert_eq!(
            receipt["consistency"],
            "separate_observations_no_reservation"
        );
        assert_eq!(receipt["limits"]["response_byte_cap"], "unavailable");
        assert_eq!(receipt["limits"]["server_truncation_flag"], "unavailable");
        assert_eq!(
            receipt["limits"]["deadline_semantics"],
            "cooperative_await_with_elapsed_recheck"
        );
        for field in [
            "metadata",
            "inodes",
            "compact_guards",
            "block_authority",
            "blocks",
        ] {
            assert_eq!(receipt["metadata"]["row_presence"][field], false);
        }
        let mn = receipt["metadata"]["observed_at_ns"]
            .as_str()
            .unwrap()
            .parse::<u128>()
            .unwrap();
        let bn = receipt["blobs"]["observed_at_ns"]
            .as_str()
            .unwrap()
            .parse::<u128>()
            .unwrap();
        assert!(mn <= bn && bn <= 30_000_000_000);
        for private in [
            "private-user",
            "private-password",
            "private-bucket",
            "private-access",
            "private-secret",
            "owned-a1",
            "127.0.0.1",
        ] {
            assert!(!raw.contains(private));
        }
        assert!(raw.len() < 4096);
        println!("NATIVE_SPLIT_PRESENCE_CONTROL {raw}");
    }

    #[tokio::test]
    async fn each_contaminated_table_or_blob_stays_present_without_namespace_creation() {
        for index in 0..6 {
            let mut presence = absent();
            match index {
                0 => presence.metadata = true,
                1 => presence.inodes = true,
                2 => presence.compact_guards = true,
                3 => presence.block_authority = true,
                4 => presence.blocks = true,
                _ => {}
            }
            let raw = inspect_operations(
                || async { Ok(presence) },
                || async { Ok(index != 5) },
                || async { Ok(()) },
                limits(),
            )
            .await
            .unwrap();
            let receipt: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(receipt["namespace_absent"], false);
        }
    }

    #[tokio::test]
    async fn provider_failures_preserve_first_stage_and_still_await_pool_shutdown() {
        for stage in ["metadata", "blobs"] {
            let closed = Arc::new(AtomicBool::new(false));
            let c = closed.clone();
            let error = inspect_operations(
                || async {
                    if stage == "metadata" {
                        Err(FsError::backend("private SQL credential"))
                    } else {
                        Ok(absent())
                    }
                },
                || async { Err(FsError::backend("private blob credential")) },
                move || async move {
                    c.store(true, Ordering::SeqCst);
                    Ok(())
                },
                limits(),
            )
            .await
            .unwrap_err();
            assert!(closed.load(Ordering::SeqCst));
            assert!(message(&error).contains(stage));
            assert!(!message(&error).contains("private"));
        }
        let error = inspect_operations(
            || async { Err(FsError::backend("private SQL")) },
            || async { Ok(true) },
            || async { Err(FsError::backend("private close")) },
            limits(),
        )
        .await
        .unwrap_err();
        assert!(
            message(&error).contains("metadata")
                && message(&error).contains("shutdown_unconfirmed")
        );
        assert!(!message(&error).contains("private"));
    }

    struct PendingDrop(Arc<AtomicBool>);
    impl Drop for PendingDrop {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn timed_out_probe_is_dropped_before_shutdown_and_cannot_produce_absence() {
        let dropped = Arc::new(AtomicBool::new(false));
        let d = dropped.clone();
        let check = dropped.clone();
        let error = inspect_operations(
            move || async move {
                let _guard = PendingDrop(d);
                std::future::pending::<CoreResult<TidbNamespacePresence>>().await
            },
            || -> std::future::Ready<CoreResult<bool>> {
                panic!("blob query cannot follow a timed out metadata query")
            },
            move || async move {
                assert!(check.load(Ordering::SeqCst));
                Ok(())
            },
            Limits {
                probe: Duration::from_millis(5),
                shutdown: Duration::from_secs(1),
            },
        )
        .await
        .unwrap_err();
        assert!(message(&error).contains("probe_deadline"));
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn failed_or_pending_pool_shutdown_refuses_an_otherwise_empty_receipt() {
        let error = inspect_operations(
            || async { Ok(absent()) },
            || async { Ok(true) },
            || async { Err(FsError::backend("private shutdown")) },
            limits(),
        )
        .await
        .unwrap_err();
        assert!(
            message(&error).contains("shutdown_unconfirmed")
                && !message(&error).contains("private")
        );
        let error = inspect_operations(
            || async { Ok(absent()) },
            || async { Ok(true) },
            std::future::pending::<CoreResult<()>>,
            Limits {
                probe: Duration::from_secs(1),
                shutdown: Duration::from_millis(5),
            },
        )
        .await
        .unwrap_err();
        assert!(message(&error).contains("shutdown_unconfirmed"));
    }

    #[tokio::test]
    async fn elapsed_recheck_refuses_synchronously_late_probe_and_shutdown() {
        let error = inspect_operations(
            || async {
                std::thread::sleep(Duration::from_millis(10));
                Ok(absent())
            },
            || -> std::future::Ready<CoreResult<bool>> {
                panic!("late metadata must not dispatch blobs")
            },
            || async { Ok(()) },
            Limits {
                probe: Duration::from_millis(1),
                shutdown: Duration::from_secs(1),
            },
        )
        .await
        .unwrap_err();
        assert!(message(&error).contains("probe_deadline"));
        let error = inspect_operations(
            || async { Ok(absent()) },
            || async { Ok(true) },
            || async {
                std::thread::sleep(Duration::from_millis(10));
                Ok(())
            },
            Limits {
                probe: Duration::from_secs(1),
                shutdown: Duration::from_millis(1),
            },
        )
        .await
        .unwrap_err();
        assert!(message(&error).contains("shutdown_unconfirmed"));
    }
}
