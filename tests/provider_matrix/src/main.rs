//! Bounded P0 provider/consumer checks.
//!
//! This is deliberately a standalone package so it exercises the repository's
//! public Rust APIs without becoming part of the root package's test target.
//! Missing external gates are reported as skips; they are never converted into
//! successful provider runs.

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{BlockId, BlockStore, MetadataStore, Namespace, NodeData};
use mount_rs_core::{FsError, Loopback, MkdirOptions};
use mount_rs_memory::MemoryMetadataStore;
use mount_rs_pglite::{PgliteBlockStore, PgliteMetadataStore, PgliteStorageOptions};
use mount_rs_r2::{R2BlockStore, R2Config};
use mount_rs_sdk::{Filesystem, MemoryOptions, SplitOptions, StoreConfig};
use mount_rs_sqlite::SqliteMetadataStore;
use object_store::ObjectStore;
use object_store::path::Path as ObjectPath;
use std::collections::BTreeSet;
use std::env;
use std::sync::{Arc, Mutex};

const PAYLOAD: &[u8] = b"mount-rs/provider-matrix\0payload\xff";
const CHUNK_SIZE: usize = 7;

type MatrixResult<T> = Result<T, String>;

fn fs_code(error: FsError) -> String {
    error.code.as_str().to_owned()
}

fn missing_env(names: &[&'static str]) -> Vec<&'static str> {
    names
        .iter()
        .filter(|name| env::var_os(name).is_none_or(|value| value.is_empty()))
        .map(|name| *name)
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
    let filesystem = Filesystem::split(SplitOptions {
        metadata,
        blocks,
        chunk_size_bytes: CHUNK_SIZE,
        owner: format!("provider-matrix-sdk-{label}"),
        uid: 0,
        gid: 0,
        umask: 0,
    })
    .await
    .map_err(fs_code)?;
    exercise_sdk(filesystem).await
}

fn block_ids(namespace: &Namespace) -> Vec<BlockId> {
    namespace
        .nodes
        .values()
        .filter_map(|node| match &node.data {
            NodeData::File(layout) => {
                Some(layout.extents.iter().map(|extent| extent.block.clone()))
            }
            _ => None,
        })
        .flatten()
        .collect()
}

async fn exercise_chunked<M, B>(metadata: M, blocks: B, owner: &str) -> MatrixResult<Vec<BlockId>>
where
    M: MetadataStore + Clone + 'static,
    B: BlockStore + Clone + 'static,
{
    let filesystem = ChunkedFs::open(
        metadata,
        blocks,
        ChunkedOptions::fixed(owner, CHUNK_SIZE).map_err(fs_code)?,
    )
    .await
    .map_err(fs_code)?;

    let result = async {
        let loopback = Loopback::new(filesystem.clone());
        loopback
            .mkdir(
                "/provider-matrix",
                MkdirOptions {
                    recursive: true,
                    mode: Some(0o755),
                },
            )
            .await
            .map_err(fs_code)?;
        loopback
            .write_file("/provider-matrix/value", PAYLOAD)
            .await
            .map_err(fs_code)?;
        let actual = loopback
            .read_file("/provider-matrix/value")
            .await
            .map_err(fs_code)?;
        if actual != PAYLOAD {
            return Err("readback-mismatch".to_owned());
        }
        let stat = loopback
            .stat("/provider-matrix/value")
            .await
            .map_err(fs_code)?;
        if stat.size != PAYLOAD.len() as u64 {
            return Err("size-mismatch".to_owned());
        }
        loopback.syncfs().await.map_err(fs_code)?;

        let loaded = filesystem.metadata_store().load().await.map_err(fs_code)?;
        loaded.validate().map_err(fs_code)?;
        let namespace = loaded
            .namespace
            .as_ref()
            .ok_or_else(|| "metadata-not-published".to_owned())?;
        Ok(block_ids(namespace))
    }
    .await;

    let shutdown = filesystem.shutdown().await.map_err(fs_code);
    match (result, shutdown) {
        (Ok(ids), Ok(())) => Ok(ids),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(format!("shutdown-{error}")),
        (Err(error), Err(shutdown_error)) => Err(format!("{error};shutdown-{shutdown_error}")),
    }
}

/// Track exactly the blocks created by this run so a live R2 row can clean up
/// without listing or deleting anything outside its owned prefix.
#[derive(Clone)]
struct TrackedR2Blocks {
    inner: R2BlockStore,
    object_store: Arc<dyn ObjectStore>,
    prefix: String,
    created: Arc<Mutex<BTreeSet<String>>>,
}

impl TrackedR2Blocks {
    fn new(config: &R2Config, prefix: String) -> MatrixResult<Self> {
        let object_store = config.build_store().map_err(fs_code)?;
        let inner =
            R2BlockStore::new(object_store.clone(), prefix.clone(), true).map_err(fs_code)?;
        Ok(Self {
            inner,
            object_store,
            prefix,
            created: Arc::new(Mutex::new(BTreeSet::new())),
        })
    }

    async fn cleanup(&self) -> MatrixResult<()> {
        let keys = self
            .created
            .lock()
            .map_err(|_| "r2-tracker-lock".to_owned())?
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for key in keys {
            match self.object_store.delete(&ObjectPath::from(key)).await {
                Ok(()) | Err(object_store::Error::NotFound { .. }) => {}
                Err(_) => return Err("r2-cleanup-delete".to_owned()),
            }
        }
        let remaining = self
            .object_store
            .list_with_delimiter(Some(&ObjectPath::from(self.prefix.clone())))
            .await
            .map_err(|_| "r2-cleanup-list".to_owned())?;
        if !remaining.objects.is_empty() || !remaining.common_prefixes.is_empty() {
            return Err("r2-cleanup-incomplete".to_owned());
        }
        Ok(())
    }

    fn track(&self, id: &BlockId) -> MatrixResult<()> {
        self.created
            .lock()
            .map_err(|_| "r2-tracker-lock".to_owned())?
            .insert(format!("{}/{}", self.prefix, id.0));
        Ok(())
    }
}

#[async_trait]
impl BlockStore for TrackedR2Blocks {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> mount_rs_core::Result<BlockId> {
        let id = self.inner.put(bytes).await?;
        self.track(&id).map_err(FsError::backend)?;
        Ok(id)
    }

    async fn get(&self, id: &BlockId) -> mount_rs_core::Result<Vec<u8>> {
        self.inner.get(id).await
    }

    async fn flush(&self) -> mount_rs_core::Result<()> {
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> mount_rs_core::Result<()> {
        let result = self.inner.delete(id).await;
        if result.is_ok()
            && let Ok(mut created) = self.created.lock()
        {
            created.remove(&format!("{}/{}", self.prefix, id.0));
        }
        result
    }
}

async fn run_r2_with_metadata<M>(metadata: M, config: &R2Config, label: &str) -> MatrixResult<()>
where
    M: MetadataStore + Clone + 'static,
{
    let prefix = format!("mount-rs-provider-matrix/{}/{label}", safe_run_id());
    let blocks = TrackedR2Blocks::new(config, prefix)?;
    let result = exercise_chunked(
        metadata,
        blocks.clone(),
        &format!("provider-matrix-{label}"),
    )
    .await
    .map(|_| ());
    let cleanup = blocks.cleanup().await;
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Err(error), Err(cleanup_error)) => Err(format!("{error};{cleanup_error}")),
    }
}

async fn run_pglite(url: &str, with_r2: bool, config: Option<&R2Config>) -> MatrixResult<()> {
    let key = format!("mount-rs-provider-matrix/{}/pglite", safe_run_id());
    let options = PgliteStorageOptions::new(key).with_durable(false);
    let metadata = PgliteMetadataStore::connect_with_options(url, options.clone())
        .await
        .map_err(fs_code)?;

    if with_r2 {
        let config = config.ok_or_else(|| "r2-config-not-provided".to_owned())?;
        let prefix = format!("mount-rs-provider-matrix/{}/pglite-r2", safe_run_id());
        let blocks = TrackedR2Blocks::new(config, prefix)?;
        let result = exercise_chunked(
            metadata.clone(),
            blocks.clone(),
            "provider-matrix-pglite-r2",
        )
        .await
        .map(|_| ());
        let close = metadata.close().await.map_err(fs_code);
        let cleanup = blocks.cleanup().await;
        return match (result, close, cleanup) {
            (Ok(()), Ok(()), Ok(())) => Ok(()),
            (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
        };
    }

    let blocks = PgliteBlockStore::connect_with_options(url, options)
        .await
        .map_err(fs_code)?;
    let result = exercise_chunked(
        metadata.clone(),
        blocks.clone(),
        "provider-matrix-pglite-pglite",
    )
    .await
    .map(|_| ());
    let close_metadata = metadata.close().await.map_err(fs_code);
    let close_blocks = blocks.close().await.map_err(fs_code);
    match (result, close_metadata, close_blocks) {
        (Ok(()), Ok(()), Ok(())) => Ok(()),
        (Err(error), _, _) | (_, Err(error), _) | (_, _, Err(error)) => Err(error),
    }
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
    report
        .case("sqlite/sqlite", async {
            exercise_sdk_split(
                StoreConfig::Sqlite {
                    path: ":memory:".into(),
                },
                StoreConfig::Sqlite {
                    path: ":memory:".into(),
                },
                "sqlite-sqlite",
            )
            .await
        })
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
                    run_r2_with_metadata(MemoryMetadataStore::new(), config, "memory-r2"),
                )
                .await;
            report
                .case("sqlite/r2", async {
                    let metadata = SqliteMetadataStore::in_memory().map_err(fs_code)?;
                    run_r2_with_metadata(metadata, config, "sqlite-r2").await
                })
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
