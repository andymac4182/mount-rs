//! Bounded P0 provider/consumer checks.
//!
//! This is deliberately a standalone package so it exercises the repository's
//! public Rust APIs without becoming part of the root package's test target.
//! Missing external gates are reported as skips; they are never converted into
//! successful provider runs.

use mount_rs_core::{FsError, Loopback, MkdirOptions};
use mount_rs_r2::R2Config;
use mount_rs_sdk::{Filesystem, MemoryOptions, SplitOptions, StoreConfig};
use object_store::ObjectStore;
use object_store::path::Path as ObjectPath;
use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};
use std::time::Duration;

const PAYLOAD: &[u8] = b"mount-rs\0provider\xff";
const CHUNK_SIZE: usize = 7;
const SEEDED_PAYLOAD: &[u8] = b"seeded\0cross-backend-payload";
const SEEDED_PATCH: &[u8] = b"R2!";
const SEEDED_PATCH_OFFSET: usize = 7;
const SEEDED_FINAL_LENGTH: usize = 20;

type MatrixResult<T> = Result<T, String>;

fn fs_code(error: FsError) -> String {
    error.code.as_str().to_owned()
}

fn missing_env(names: &[&'static str]) -> Vec<&'static str> {
    names
        .iter()
        .filter(|name| env::var_os(name).is_none_or(|value| value.is_empty()))
        .copied()
        .collect()
}

fn safe_run_id() -> String {
    let configured = env::var("MOUNT_RS_PROVIDER_MATRIX_RUN_ID").unwrap_or_default();
    if !configured.is_empty()
        && configured
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        configured
    } else {
        format!("pid-{}", std::process::id())
    }
}

fn sqlite_paths(label: &str) -> (PathBuf, PathBuf) {
    let stem = format!("mount-rs-provider-matrix-{}-{label}", safe_run_id());
    (
        env::temp_dir().join(format!("{stem}-metadata.sqlite")),
        env::temp_dir().join(format!("{stem}-blocks.sqlite")),
    )
}

fn cleanup_sqlite_path(path: &Path) -> MatrixResult<()> {
    for suffix in ["", "-wal", "-shm"] {
        let candidate = if suffix.is_empty() {
            path.to_owned()
        } else {
            PathBuf::from(format!("{}{}", path.display(), suffix))
        };
        match std::fs::remove_file(candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("sqlite-cleanup".to_owned()),
        }
    }
    Ok(())
}

fn cleanup_sqlite_paths(paths: &(PathBuf, PathBuf)) -> MatrixResult<()> {
    let metadata = cleanup_sqlite_path(&paths.0);
    let blocks = cleanup_sqlite_path(&paths.1);
    match (metadata, blocks) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(blocks_error)) => Err(format!("{error};{blocks_error}")),
    }
}

async fn exercise_sdk(filesystem: Filesystem) -> MatrixResult<()> {
    let view = Loopback::from_arc(filesystem.driver());
    let result = async {
        view.mkdir(
            "/provider-matrix",
            MkdirOptions {
                recursive: true,
                mode: Some(0o755),
            },
        )
        .await
        .map_err(fs_code)?;
        view.write_file("/provider-matrix/value", PAYLOAD)
            .await
            .map_err(fs_code)?;
        let actual = view
            .read_file("/provider-matrix/value")
            .await
            .map_err(fs_code)?;
        if actual != PAYLOAD {
            return Err("readback-mismatch".to_owned());
        }
        let stat = view.stat("/provider-matrix/value").await.map_err(fs_code)?;
        if stat.size != PAYLOAD.len() as u64 {
            return Err("size-mismatch".to_owned());
        }
        view.syncfs().await.map_err(fs_code)
    }
    .await;
    let shutdown = filesystem.shutdown().await.map_err(fs_code);
    match (result, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!("shutdown-{error}")),
        (Err(error), Err(shutdown_error)) => Err(format!("{error};shutdown-{shutdown_error}")),
    }
}

async fn exercise_sdk_split(
    metadata: StoreConfig,
    blocks: StoreConfig,
    label: &str,
) -> MatrixResult<()> {
    let filesystem =
        open_sdk_split(metadata, blocks, format!("provider-matrix-sdk-{label}")).await?;
    exercise_sdk(filesystem).await
}

async fn run_sqlite_split(label: &str) -> MatrixResult<()> {
    let paths = sqlite_paths(label);
    let result = exercise_sdk_split_reopen(
        StoreConfig::Sqlite {
            path: paths.0.clone(),
        },
        StoreConfig::Sqlite {
            path: paths.1.clone(),
        },
        label,
    )
    .await;
    let cleanup = cleanup_sqlite_paths(&paths);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!("{error};{cleanup_error}")),
    }
}

async fn run_sqlite_direct() -> MatrixResult<()> {
    let path = env::temp_dir().join(format!(
        "mount-rs-provider-matrix-{}-direct.sqlite",
        safe_run_id()
    ));
    let result = async {
        let first = Filesystem::sqlite(path.clone()).await.map_err(fs_code)?;
        exercise_sdk(first).await?;

        let reopened = Filesystem::sqlite(path.clone()).await.map_err(fs_code)?;
        let view = Loopback::from_arc(reopened.driver());
        let result = async {
            let actual = view
                .read_file("/provider-matrix/value")
                .await
                .map_err(fs_code)?;
            if actual != PAYLOAD {
                return Err("reopen-readback-mismatch".to_owned());
            }
            let stat = view.stat("/provider-matrix/value").await.map_err(fs_code)?;
            if stat.size != PAYLOAD.len() as u64 {
                return Err("reopen-size-mismatch".to_owned());
            }
            view.syncfs().await.map_err(fs_code)
        }
        .await;
        let shutdown = reopened.shutdown().await.map_err(fs_code);
        match (result, shutdown) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(format!("reopen-shutdown-{error}")),
            (Err(error), Err(shutdown_error)) => {
                Err(format!("{error};reopen-shutdown-{shutdown_error}"))
            }
        }
    }
    .await;
    let cleanup = cleanup_sqlite_path(&path);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!("{error};{cleanup_error}")),
    }
}

async fn open_sdk_split(
    metadata: StoreConfig,
    blocks: StoreConfig,
    owner: String,
) -> MatrixResult<Filesystem> {
    Filesystem::split(SplitOptions {
        metadata,
        blocks,
        chunk_size_bytes: CHUNK_SIZE,
        owner,
        lease_ttl: Duration::from_secs(30),
        uid: 0,
        gid: 0,
        umask: 0,
    })
    .await
    .map_err(fs_code)
}

async fn exercise_sdk_split_reopen(
    metadata: StoreConfig,
    blocks: StoreConfig,
    label: &str,
) -> MatrixResult<()> {
    let first = open_sdk_split(
        metadata.clone(),
        blocks.clone(),
        format!("provider-matrix-sdk-{label}-first"),
    )
    .await?;
    exercise_sdk(first).await?;

    let reopened = open_sdk_split(
        metadata,
        blocks,
        format!("provider-matrix-sdk-{label}-reopened"),
    )
    .await?;
    let view = Loopback::from_arc(reopened.driver());
    let result = async {
        let actual = view
            .read_file("/provider-matrix/value")
            .await
            .map_err(fs_code)?;
        if actual != PAYLOAD {
            return Err("reopen-readback-mismatch".to_owned());
        }
        let stat = view.stat("/provider-matrix/value").await.map_err(fs_code)?;
        if stat.size != PAYLOAD.len() as u64 {
            return Err("reopen-size-mismatch".to_owned());
        }
        view.syncfs().await.map_err(fs_code)
    }
    .await;
    let shutdown = reopened.shutdown().await.map_err(fs_code);
    match (result, shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!("reopen-shutdown-{error}")),
        (Err(error), Err(shutdown_error)) => {
            Err(format!("{error};reopen-shutdown-{shutdown_error}"))
        }
    }
}

fn seeded_expected() -> Vec<u8> {
    let mut expected = SEEDED_PAYLOAD.to_vec();
    expected[SEEDED_PATCH_OFFSET..SEEDED_PATCH_OFFSET + SEEDED_PATCH.len()]
        .copy_from_slice(SEEDED_PATCH);
    expected.truncate(SEEDED_FINAL_LENGTH);
    expected
}

async fn assert_seeded_state(view: &Loopback) -> MatrixResult<()> {
    let expected = seeded_expected();
    let actual = view
        .read_file("/provider-matrix/seeded")
        .await
        .map_err(fs_code)?;
    if actual != expected {
        return Err("seeded-readback-mismatch".to_owned());
    }
    let stat = view
        .stat("/provider-matrix/seeded")
        .await
        .map_err(fs_code)?;
    if stat.size != expected.len() as u64 {
        return Err("seeded-size-mismatch".to_owned());
    }
    Ok(())
}

async fn exercise_sdk_split_seeded_reopen(
    metadata: StoreConfig,
    blocks: StoreConfig,
    label: &str,
) -> MatrixResult<()> {
    let first = open_sdk_split(
        metadata.clone(),
        blocks.clone(),
        format!("provider-matrix-sdk-{label}-first"),
    )
    .await?;
    let first_view = Loopback::from_arc(first.driver());
    let first_result = async {
        first_view
            .mkdir(
                "/provider-matrix",
                MkdirOptions {
                    recursive: true,
                    mode: Some(0o755),
                },
            )
            .await
            .map_err(fs_code)?;
        first_view
            .write_file("/provider-matrix/seeded", SEEDED_PAYLOAD)
            .await
            .map_err(fs_code)?;

        let handle = first_view
            .open("/provider-matrix/seeded", "r+", 0)
            .await
            .map_err(fs_code)?;
        let written = handle
            .write(SEEDED_PATCH, Some(SEEDED_PATCH_OFFSET as u64))
            .await
            .map_err(fs_code)?;
        if written != SEEDED_PATCH.len() {
            return Err("seeded-partial-write-count".to_owned());
        }
        handle.sync().await.map_err(fs_code)?;
        handle.close().await.map_err(fs_code)?;

        first_view
            .truncate("/provider-matrix/seeded", SEEDED_FINAL_LENGTH as u64)
            .await
            .map_err(fs_code)?;
        assert_seeded_state(&first_view).await?;
        first_view.syncfs().await.map_err(fs_code)
    }
    .await;
    let first_shutdown = first.shutdown().await.map_err(fs_code);
    match (first_result, first_shutdown) {
        (Ok(()), Ok(())) => {}
        (Err(error), Ok(())) => return Err(error),
        (Ok(()), Err(error)) => return Err(format!("shutdown-{error}")),
        (Err(error), Err(shutdown_error)) => {
            return Err(format!("{error};shutdown-{shutdown_error}"));
        }
    }

    let reopened = open_sdk_split(
        metadata,
        blocks,
        format!("provider-matrix-sdk-{label}-reopened"),
    )
    .await?;
    let reopened_view = Loopback::from_arc(reopened.driver());
    let reopened_result = async {
        assert_seeded_state(&reopened_view).await?;
        reopened_view.syncfs().await.map_err(fs_code)
    }
    .await;
    let reopened_shutdown = reopened.shutdown().await.map_err(fs_code);
    match (reopened_result, reopened_shutdown) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(format!("reopen-shutdown-{error}")),
        (Err(error), Err(shutdown_error)) => {
            Err(format!("{error};reopen-shutdown-{shutdown_error}"))
        }
    }
}

async fn run_sqlite_seeded_split(label: &str) -> MatrixResult<()> {
    let paths = sqlite_paths(label);
    let result = exercise_sdk_split_seeded_reopen(
        StoreConfig::Sqlite {
            path: paths.0.clone(),
        },
        StoreConfig::Sqlite {
            path: paths.1.clone(),
        },
        label,
    )
    .await;
    let cleanup = cleanup_sqlite_paths(&paths);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!("{error};{cleanup_error}")),
    }
}

fn r2_blocks_config(config: &R2Config, prefix: String) -> StoreConfig {
    StoreConfig::R2 {
        endpoint: config.endpoint.clone(),
        bucket: config.bucket.clone(),
        prefix,
        access_key_id: config.access_key_id.clone(),
        secret_access_key: config.secret_access_key.clone(),
        durable: true,
    }
}

async fn list_r2_prefix(
    config: &R2Config,
    prefix: &str,
) -> MatrixResult<(std::sync::Arc<dyn ObjectStore>, BTreeSet<String>)> {
    let object_store = config.build_store().map_err(fs_code)?;
    let listing = object_store
        .list_with_delimiter(Some(&ObjectPath::from(prefix.to_owned())))
        .await
        .map_err(|_| "r2-cleanup-list".to_owned())?;
    if !listing.common_prefixes.is_empty() {
        return Err("r2-cleanup-unexpected-nested-prefix".to_owned());
    }
    let owned_prefix = format!("{prefix}/");
    let mut keys = BTreeSet::new();
    for object in listing.objects {
        let key = object.location.to_string();
        if !key.starts_with(&owned_prefix) {
            return Err("r2-cleanup-scope".to_owned());
        }
        keys.insert(key);
    }
    Ok((object_store, keys))
}

async fn cleanup_r2_prefix(
    config: &R2Config,
    prefix: &str,
    protected: &BTreeSet<String>,
) -> MatrixResult<()> {
    let (object_store, keys) = list_r2_prefix(config, prefix).await?;
    for key in keys {
        if protected.contains(&key) {
            continue;
        }
        match object_store.delete(&ObjectPath::from(key)).await {
            Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
            Err(_) => return Err("r2-cleanup-delete".to_owned()),
        }
    }
    let (_, remaining) = list_r2_prefix(config, prefix).await?;
    if remaining.into_iter().any(|key| !protected.contains(&key)) {
        return Err("r2-cleanup-incomplete".to_owned());
    }
    Ok(())
}

async fn run_r2_with_metadata(
    metadata: StoreConfig,
    config: &R2Config,
    label: &str,
    reopen: bool,
) -> MatrixResult<()> {
    let prefix = format!("mount-rs-provider-matrix/{}/{label}", safe_run_id());
    let (_, protected) = list_r2_prefix(config, &prefix).await?;
    let result = if reopen {
        exercise_sdk_split_reopen(
            metadata,
            r2_blocks_config(config, prefix.clone()),
            &format!("{label}-reopen"),
        )
        .await
    } else {
        exercise_sdk_split(metadata, r2_blocks_config(config, prefix.clone()), label).await
    };
    let cleanup = cleanup_r2_prefix(config, &prefix, &protected).await;
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!("{error};{cleanup_error}")),
    }
}

async fn run_r2_with_sqlite_metadata(config: &R2Config, label: &str) -> MatrixResult<()> {
    let metadata_path = env::temp_dir().join(format!(
        "mount-rs-provider-matrix-{}-{label}-metadata.sqlite",
        safe_run_id()
    ));
    let result = run_r2_with_metadata(
        StoreConfig::Sqlite {
            path: metadata_path.clone(),
        },
        config,
        label,
        true,
    )
    .await;
    let cleanup = cleanup_sqlite_path(&metadata_path);
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!("{error};{cleanup_error}")),
    }
}

async fn run_pglite(url: &str, with_r2: bool, config: Option<&R2Config>) -> MatrixResult<()> {
    let volume_suffix = if with_r2 { "-r2" } else { "" };
    let key = format!(
        "mount-rs-provider-matrix/{}/pglite{volume_suffix}",
        safe_run_id()
    );
    let metadata = StoreConfig::Pglite {
        connection: url.to_owned(),
        volume_key: key.clone(),
        durable: false,
    };
    if with_r2 {
        let config = config.ok_or_else(|| "r2-config-not-provided".to_owned())?;
        let prefix = format!("mount-rs-provider-matrix/{}/pglite-r2", safe_run_id());
        let (_, protected) = list_r2_prefix(config, &prefix).await?;
        let result = exercise_sdk_split_reopen(
            metadata,
            r2_blocks_config(config, prefix.clone()),
            "provider-matrix-pglite-r2-reopen",
        )
        .await;
        let cleanup = cleanup_r2_prefix(config, &prefix, &protected).await;
        return match (result, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error),
            (Err(error), Err(cleanup_error)) => Err(format!("{error};{cleanup_error}")),
        };
    }
    exercise_sdk_split_reopen(
        metadata,
        StoreConfig::Pglite {
            connection: url.to_owned(),
            volume_key: key,
            durable: false,
        },
        "provider-matrix-pglite-pglite-reopen",
    )
    .await
}

struct Report {
    pass: usize,
    skip: usize,
    fail: usize,
}

impl Report {
    async fn case<F>(&mut self, label: &str, operation: F)
    where
        F: std::future::Future<Output = MatrixResult<()>>,
    {
        match operation.await {
            Ok(()) => {
                self.pass += 1;
                println!("PASS rust-sdk case={label}");
            }
            Err(reason) => {
                self.fail += 1;
                println!("FAIL rust-sdk case={label} reason={reason}");
            }
        }
    }

    fn skip(&mut self, label: &str, gate: &str) {
        self.skip += 1;
        println!("SKIP rust-sdk case={label} gate={gate}");
    }
}

#[tokio::main]
async fn main() {
    let mut report = Report {
        pass: 0,
        skip: 0,
        fail: 0,
    };

    report
        .case(
            "memfs",
            exercise_sdk(Filesystem::memory(MemoryOptions::default())),
        )
        .await;
    report
        .case("memory/memory", async {
            exercise_sdk_split(StoreConfig::Memory, StoreConfig::Memory, "memory-memory").await
        })
        .await;
    report.case("sqlite", run_sqlite_direct()).await;
    report
        .case("sqlite/sqlite", run_sqlite_split("sqlite-sqlite"))
        .await;
    report
        .case(
            "sqlite/sqlite-seeded-reopen",
            run_sqlite_seeded_split("sqlite-sqlite-seeded-reopen"),
        )
        .await;

    let pglite_url = ["PGLITE_DATABASE_URL", "MOUNT_RS_PGLITE_URL"]
        .iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()));
    let r2_missing = missing_env(&[
        "R2_ENDPOINT",
        "R2_BUCKET",
        "R2_ACCESS_KEY_ID",
        "R2_SECRET_ACCESS_KEY",
    ]);

    if let Some(url) = pglite_url.as_deref() {
        report
            .case("pglite/pglite", run_pglite(url, false, None))
            .await;
    } else {
        report.skip("pglite/pglite", "PGLITE_DATABASE_URL|MOUNT_RS_PGLITE_URL");
    }

    if r2_missing.is_empty() {
        let config = R2Config::from_env().ok();
        if let Some(config) = config.as_ref() {
            report
                .case(
                    "memory/r2",
                    run_r2_with_metadata(StoreConfig::Memory, config, "memory-r2", false),
                )
                .await;
            report
                .case(
                    "sqlite/r2",
                    run_r2_with_sqlite_metadata(config, "sqlite-r2"),
                )
                .await;
        } else {
            report.fail += 2;
            println!("FAIL rust-sdk case=memory/r2 reason=R2_CONFIG");
            println!("FAIL rust-sdk case=sqlite/r2 reason=R2_CONFIG");
        }
    } else {
        let gate = r2_missing.join("|");
        report.skip("memory/r2", &gate);
        report.skip("sqlite/r2", &gate);
    }

    if let Some(url) = pglite_url.as_deref()
        && r2_missing.is_empty()
    {
        let config = R2Config::from_env().ok();
        if let Some(config) = config.as_ref() {
            report
                .case("pglite/r2", run_pglite(url, true, Some(config)))
                .await;
        } else {
            report.fail += 1;
            println!("FAIL rust-sdk case=pglite/r2 reason=R2_CONFIG");
        }
    } else {
        let mut gates = Vec::new();
        if pglite_url.is_none() {
            gates.push("PGLITE_DATABASE_URL|MOUNT_RS_PGLITE_URL");
        }
        if !r2_missing.is_empty() {
            gates.push("R2_ENDPOINT|R2_BUCKET|R2_ACCESS_KEY_ID|R2_SECRET_ACCESS_KEY");
        }
        report.skip("pglite/r2", &gates.join("+"));
    }

    println!(
        "SUMMARY rust-sdk pass={} skip={} fail={}",
        report.pass, report.skip, report.fail
    );
    if report.fail != 0 {
        std::process::exit(1);
    }
}
