//! Direct-Rust read baseline. Run in release mode; this is not a mounted-path benchmark.
use async_trait::async_trait;
use mount_rs_chunked::{AtimePolicy, CachedBlockStore, ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, WriterLease,
};
use mount_rs_core::{FsDriver, Result};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use mount_rs_tidb::TidbMetadataStore;
use mysql_async::{Pool, prelude::Queryable};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[derive(Default)]
struct Counts {
    renew: AtomicU64,
    publish: AtomicU64,
    flush: AtomicU64,
    get: AtomicU64,
    put: AtomicU64,
}
impl Counts {
    fn take(&self) -> [u64; 5] {
        [
            &self.renew,
            &self.publish,
            &self.flush,
            &self.get,
            &self.put,
        ]
        .map(|v| v.swap(0, Ordering::Relaxed))
    }
}
struct Meta<M>(M, Arc<Counts>);
#[async_trait]
impl<M: MetadataStore> MetadataStore for Meta<M> {
    fn durable(&self) -> bool {
        self.0.durable()
    }
    async fn load(&self) -> Result<LoadedMetadata> {
        self.0.load().await
    }
    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        self.0.acquire_writer(owner, ttl).await
    }
    async fn renew_writer(&self, lease: &WriterLease, ttl: Duration) -> Result<WriterLease> {
        self.1.renew.fetch_add(1, Ordering::Relaxed);
        self.0.renew_writer(lease, ttl).await
    }
    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        self.0.release_writer(lease).await
    }
    async fn publish(
        &self,
        revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        self.1.publish.fetch_add(1, Ordering::Relaxed);
        self.0.publish(revision, lease, namespace).await
    }
    async fn publish_shared(
        &self,
        revision: u64,
        lease: &WriterLease,
        namespace: Arc<Namespace>,
    ) -> Result<u64> {
        self.1.publish.fetch_add(1, Ordering::Relaxed);
        self.0.publish_shared(revision, lease, namespace).await
    }
    async fn flush(&self) -> Result<()> {
        self.1.flush.fetch_add(1, Ordering::Relaxed);
        self.0.flush().await
    }
}
struct Blocks<B>(B, Arc<Counts>);
#[async_trait]
impl<B: BlockStore> BlockStore for Blocks<B> {
    fn durable(&self) -> bool {
        self.0.durable()
    }
    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        self.1.put.fetch_add(1, Ordering::Relaxed);
        self.0.put(bytes).await
    }
    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.1.get.fetch_add(1, Ordering::Relaxed);
        self.0.get(id).await
    }
    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.0.delete(id).await
    }
    async fn flush(&self) -> Result<()> {
        self.0.flush().await
    }
}
async fn run<M: MetadataStore + Clone + 'static, B: BlockStore + 'static>(
    metadata: M,
    blocks: B,
    block_label: &str,
    label: &str,
    samples: usize,
    files: usize,
) -> Result<()> {
    let counts = Arc::new(Counts::default());
    let cache_mode = std::env::var("AUDIT_CACHE").unwrap_or_else(|_| "off".into());
    if !matches!(cache_mode.as_str(), "off" | "warm" | "cold") {
        return Err(mount_rs_core::FsError::new(
            mount_rs_core::ErrorCode::Einval,
        ));
    }
    let cache = CachedBlockStore::new(
        Blocks(blocks, counts.clone()),
        if cache_mode == "off" {
            0
        } else {
            16 * 1024 * 1024
        },
        4096,
    );
    let atime = match std::env::var("AUDIT_ATIME")
        .unwrap_or_else(|_| "strict".into())
        .as_str()
    {
        "strict" => AtimePolicy::Strict,
        "noatime" => AtimePolicy::NoAtime,
        _ => {
            return Err(mount_rs_core::FsError::new(
                mount_rs_core::ErrorCode::Einval,
            ));
        }
    };
    let lease_window_ms = std::env::var("AUDIT_LEASE_WINDOW_MS")
        .unwrap_or_else(|_| "0".into())
        .parse::<u64>()
        .map_err(|_| mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Einval))?;
    let lease_ttl_ms = std::env::var("AUDIT_LEASE_TTL_MS")
        .unwrap_or_else(|_| "240000".into())
        .parse::<u64>()
        .map_err(|_| mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Einval))?;
    let options = ChunkedOptions::default()
        .with_lease_ttl(Duration::from_millis(lease_ttl_ms))
        .with_atime_policy(atime)
        .with_read_lease_cache_window(Duration::from_millis(lease_window_ms));
    let fs = ChunkedFs::open(
        Meta(metadata.clone(), counts.clone()),
        cache.clone(),
        options.clone(),
    )
    .await?;
    // Different chunks prevent accidental deduplication from disguising fetch cost.
    let payload: Vec<u8> = (0..1024 * 1024)
        .map(|i| ((i * 31 + i / 65536) % 251) as u8)
        .collect();
    let file = fs.open("/payload", "w+", 0o600).await?;
    file.write(&payload, Some(0)).await?;
    fs.symlink("/payload", "/link").await?;
    for i in 0..files {
        fs.open(&format!("/entry-{i}"), "w", 0o600)
            .await?
            .close()
            .await?;
    }
    let mut rows = Vec::new();
    for operation in [
        "stat",
        "lstat",
        "fstat",
        "statfs",
        "readlink",
        "readdir",
        "open-close",
        "read-4k",
        "read-64k",
        "read-1m",
        "read-eof",
        "chmod",
        "rename-roundtrip",
    ] {
        counts.take();
        let cache_before = cache.stats().await;
        let mut latencies = Vec::new();
        let size = match operation {
            "read-4k" => 4096,
            "read-64k" => 65536,
            "read-1m" => payload.len(),
            _ => 0,
        };
        let mut buffer = vec![0; size];
        for sample in 0..samples {
            if cache_mode == "cold" {
                cache.clear().await;
            }
            let start = Instant::now();
            match operation {
                "stat" => {
                    fs.stat("/payload").await?;
                }
                "lstat" => {
                    fs.lstat("/link").await?;
                }
                "fstat" => {
                    file.stat().await?;
                }
                "statfs" => {
                    fs.statfs("/").await?;
                }
                "readlink" => {
                    fs.readlink("/link").await?;
                }
                "readdir" => {
                    fs.readdir("/").await?;
                }
                "open-close" => {
                    fs.open("/payload", "r", 0).await?.close().await?;
                }
                "chmod" => {
                    fs.chmod("/payload", if sample % 2 == 0 { 0o640 } else { 0o600 })
                        .await?;
                }
                "rename-roundtrip" => {
                    fs.rename("/payload", "/renamed").await?;
                    fs.rename("/renamed", "/payload").await?;
                }
                "read-eof" => {
                    assert_eq!(file.read(&mut [0; 1], Some(payload.len() as u64)).await?, 0);
                }
                _ => {
                    assert_eq!(file.read(&mut buffer, Some(0)).await?, size);
                }
            }
            latencies.push(start.elapsed().as_secs_f64() * 1000.0);
            if size > 0 {
                assert_eq!(buffer, payload[..size]);
            }
        }
        let calls = counts.take();
        let cache_after = cache.stats().await;
        if matches!(operation, "chmod" | "rename-roundtrip") {
            assert_eq!(
                (calls[3], calls[4]),
                (0, 0),
                "metadata-only mutations must not fetch or store contents"
            );
            assert_eq!(fs.stat("/payload").await?.ino, file.stat().await?.ino);
            if operation == "chmod" {
                assert_eq!(
                    fs.stat("/payload").await?.mode & 0o7777,
                    if (samples - 1).is_multiple_of(2) {
                        0o640
                    } else {
                        0o600
                    }
                );
            } else {
                assert_eq!(
                    fs.stat("/renamed").await.unwrap_err().code,
                    mount_rs_core::ErrorCode::Enoent
                );
            }
        }
        let operation_family = match operation {
            "chmod" | "rename-roundtrip" => "metadata-mutation",
            op if op.starts_with("read-") => "content-read",
            _ => "metadata-read",
        };
        let raw = latencies.clone();
        latencies.sort_by(f64::total_cmp);
        let percentile = |p: usize| latencies[(samples * p).div_ceil(100).saturating_sub(1)];
        eprintln!("read audit operation={operation} completed samples={samples}");
        rows.push(json!({"operation": operation, "operation_family": operation_family, "cache_hits": cache_after.hits - cache_before.hits, "cache_misses": cache_after.misses - cache_before.misses, "samples_ms": raw, "p50_ms": percentile(50), "p95_ms": percentile(95), "p99_ms": percentile(99), "max_ms": latencies[samples-1], "over_10ms": latencies.iter().filter(|x| **x >= 10.0).count(), "provider_calls": {"lease_renew": calls[0], "namespace_publish": calls[1], "metadata_flush": calls[2], "block_get": calls[3], "block_put": calls[4]}}));
    }
    let readers = std::env::var("AUDIT_READERS")
        .unwrap_or_else(|_| "0".into())
        .parse::<usize>()
        .map_err(|_| mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Einval))?;
    if readers > 32 || (readers > 0 && cache_mode == "cold") {
        return Err(mount_rs_core::FsError::new(
            mount_rs_core::ErrorCode::Einval,
        ));
    }
    let mut concurrent = Vec::new();
    if readers > 0 {
        for mixed in [false, true] {
            let mut handles = Vec::new();
            for _ in 0..readers {
                handles.push(fs.open("/payload", "r", 0).await?);
            }
            let writer = if mixed {
                Some(fs.open("/writer", "w+", 0o600).await?)
            } else {
                None
            };
            let barrier = Arc::new(tokio::sync::Barrier::new(readers + usize::from(mixed)));
            let mut tasks = tokio::task::JoinSet::new();
            counts.take();
            let before = cache.stats().await;
            for handle in &handles {
                let handle = handle.clone();
                let barrier = barrier.clone();
                let expected = payload[..4096].to_vec();
                tasks.spawn(async move {
                    let mut timings = Vec::new();
                    let mut bytes = vec![0; 4096];
                    for _ in 0..samples {
                        barrier.wait().await;
                        let start = Instant::now();
                        let count = handle.read(&mut bytes, Some(0)).await?;
                        timings.push(start.elapsed().as_secs_f64() * 1000.0);
                        if count != bytes.len() || bytes != expected {
                            return Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio));
                        }
                        tokio::task::yield_now().await;
                    }
                    Ok::<_, mount_rs_core::FsError>((false, timings))
                });
            }
            if let Some(handle) = &writer {
                let handle = handle.clone();
                let barrier = barrier.clone();
                tasks.spawn(async move {
                    let mut timings = Vec::new();
                    for iteration in 0..samples {
                        let bytes = vec![(iteration % 251) as u8; 4096];
                        barrier.wait().await;
                        let start = Instant::now();
                        let count = handle.write(&bytes, Some(0)).await?;
                        timings.push(start.elapsed().as_secs_f64() * 1000.0);
                        if count != bytes.len() {
                            return Err(mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio));
                        }
                        tokio::task::yield_now().await;
                    }
                    Ok::<_, mount_rs_core::FsError>((true, timings))
                });
            }
            let mut read_times = Vec::new();
            let mut write_times = Vec::new();
            let joined = tokio::time::timeout(Duration::from_secs(180), async {
                while let Some(result) = tasks.join_next().await {
                    let (is_write, timings) = result.map_err(|_| {
                        mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio)
                    })??;
                    if is_write {
                        write_times.extend(timings);
                    } else {
                        read_times.extend(timings);
                    }
                }
                Ok::<(), mount_rs_core::FsError>(())
            })
            .await
            .map_err(|_| mount_rs_core::FsError::new(mount_rs_core::ErrorCode::Eio))?;
            joined?;
            let calls = counts.take();
            let after = cache.stats().await;
            concurrent.push(json!({"readers": readers, "writers": usize::from(mixed), "pattern": "barrier-aligned closed-loop bursts; one operation per worker per iteration", "iterations": samples, "read_4k": summarize(read_times), "write_4k": if mixed { summarize(write_times) } else { serde_json::Value::Null }, "phase_provider_calls": {"lease_renew": calls[0], "namespace_publish": calls[1], "metadata_flush": calls[2], "block_get": calls[3], "block_put": calls[4]}, "cache_hits": after.hits-before.hits, "cache_misses": after.misses-before.misses}));
            for handle in handles {
                handle.close().await?;
            }
            if let Some(handle) = writer {
                handle.close().await?;
            }
            eprintln!("read audit concurrency readers={readers} mixed={mixed} completed");
        }
    }
    file.close().await?;
    fs.shutdown().await?;
    // Recreate owner state; backing providers and their connection pools remain warm.
    // Validation and lease release are deliberately outside each timed open.
    let mut owner_open_ms = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = Instant::now();
        let reopened = ChunkedFs::open(
            Meta(metadata.clone(), counts.clone()),
            cache.clone(),
            options.clone(),
        )
        .await?;
        owner_open_ms.push(started.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(reopened.stat("/payload").await?.size, payload.len() as u64);
        reopened.shutdown().await?;
    }
    let mut sorted = owner_open_ms.clone();
    sorted.sort_by(f64::total_cmp);
    rows.push(json!({
        "operation": "owner-open", "operation_family": "metadata-owner-load",
        "scope": "fresh owner state; warm provider and pool; lease acquisition and namespace load/validation included; shutdown excluded",
        "samples_ms": owner_open_ms, "p50_ms": sorted[(samples * 50).div_ceil(100)-1],
        "p95_ms": sorted[(samples * 95).div_ceil(100)-1],
        "p99_ms": sorted[(samples * 99).div_ceil(100)-1],
        "max_ms": sorted[samples-1],
        "over_10ms": sorted.iter().filter(|x| **x >= 10.0).count()
    }));
    println!(
        "{}",
        json!({"surface": "direct-rust", "lease_ttl_ms":lease_ttl_ms, "read_lease_cache_window_ms": lease_window_ms, "atime": format!("{atime:?}"), "metadata": label, "blocks": block_label, "durability": "volatile", "namespace_extra_files": files, "chunk_bytes": 65536, "concurrency": 1, "cache": cache_mode, "namespace_cache": "resident", "cache_capacity_bytes": if cache_mode == "off" { 0 } else { 16 * 1024 * 1024 }, "profile": if cfg!(debug_assertions) {"debug"} else {"release"}, "arch": std::env::consts::ARCH, "os": std::env::consts::OS, "results": rows, "concurrent_results": concurrent})
    );
    Ok(())
}

#[tokio::main]
async fn main() {
    // Do not print provider errors: database errors may contain deployment details.
    if execute().await.is_err() {
        eprintln!("read audit failed; no successful benchmark claim");
        std::process::exit(1);
    }
}
async fn execute() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let samples = std::env::var("AUDIT_SAMPLES")
        .unwrap_or("100".into())
        .parse::<usize>()?;
    let files = std::env::var("AUDIT_FILES")
        .unwrap_or("0".into())
        .parse::<usize>()?;
    if samples == 0 || samples > 100_000 || files > 100_000 {
        return Err("invalid workload bounds".into());
    }
    let metadata_kind = std::env::var("AUDIT_METADATA").unwrap_or_else(|_| "memory".into());
    if !matches!(metadata_kind.as_str(), "memory" | "tidb") {
        return Err("invalid metadata backend".into());
    }
    if metadata_kind == "tidb" {
        let url = std::env::var("MOUNT_RS_TIDB_URL")?;
        let pool = Pool::from_url(&url)?;
        let mut connection = pool.get_conn().await?;
        let identity: Option<(String, String)> = connection
            .query_first("SELECT tidb_version(), VERSION()")
            .await?;
        if !identity.is_some_and(|(a, b)| format!("{a} {b}").to_lowercase().contains("tidb")) {
            return Err("not TiDB".into());
        }
        let key = format!(
            "read-audit-{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        );
        let metadata = TidbMetadataStore::connect_with_key(&url, &key).await?;
        let result = run_backend(metadata.clone(), "tidb", samples, files).await;
        // Cleanup only this invocation's unique namespace, including on a workload error.
        metadata.close().await?;
        connection
            .exec_drop(
                "DELETE FROM mount_rs_tidb_metadata WHERE volume_key=?",
                (&key,),
            )
            .await?;
        drop(connection);
        pool.disconnect().await?;
        result?;
    } else {
        run_backend(MemoryMetadataStore::new(), "memory", samples, files).await?;
    }
    Ok(())
}

async fn run_backend<M: MetadataStore + Clone + 'static>(
    metadata: M,
    label: &str,
    samples: usize,
    files: usize,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    use mount_rs_r2::{R2BlockStore, R2Config};
    match std::env::var("AUDIT_BLOCKS")
        .unwrap_or_else(|_| "memory".into())
        .as_str()
    {
        "memory" => {
            run(
                metadata,
                MemoryBlockStore::new(),
                "memory",
                label,
                samples,
                files,
            )
            .await?
        }
        "local-fsync" => {
            let directory = tempfile::tempdir()?;
            let blocks = mount_rs_host::LocalBlockStore::open(directory.path())?;
            run(
                metadata,
                blocks,
                "local-fsync-sha256",
                label,
                samples,
                files,
            )
            .await?;
            directory.close()?;
        }
        "local" => {
            let directory = tempfile::tempdir()?;
            let store = object_store::local::LocalFileSystem::new_with_prefix(directory.path())?;
            // Local object-store puts are not claimed as power-loss durable.
            let blocks = R2BlockStore::new(Arc::new(store), "read-audit", false)?;
            run(metadata, blocks, "local-filesystem", label, samples, files).await?;
            directory.close()?;
        }
        "rustfs" => {
            // Require the isolated repository harness, which owns service identity
            // and scoped cleanup, rather than trusting an arbitrary S3 endpoint.
            let directory = std::env::var("RUSTFS_RUN_DIR")?;
            if !std::path::Path::new(&directory).is_dir() {
                return Err("missing RustFS harness".into());
            }
            let prefix = std::env::var("RUSTFS_COMBO_PREFIX")?;
            let config = R2Config::from_env()?;
            let endpoint = url::Url::parse(&config.endpoint)?;
            if endpoint.scheme() != "http"
                || !matches!(endpoint.host_str(), Some("127.0.0.1" | "localhost"))
            {
                return Err("RustFS audit requires isolated loopback harness".into());
            }
            let blocks = R2BlockStore::from_config(&config, format!("{prefix}/read-audit"))?;
            run(metadata, blocks, "rustfs-harness", label, samples, files).await?;
        }
        _ => return Err("invalid block backend".into()),
    }
    Ok(())
}

fn summarize(mut samples: Vec<f64>) -> serde_json::Value {
    let raw = samples.clone();
    samples.sort_by(f64::total_cmp);
    let count = samples.len();
    let percentile = |p: usize| samples[(count * p).div_ceil(100).saturating_sub(1)];
    json!({"samples_ms": raw, "p50_ms": percentile(50), "p95_ms": percentile(95), "p99_ms": percentile(99), "max_ms": samples[count-1], "over_10ms": samples.iter().filter(|x| **x >= 10.0).count()})
}
