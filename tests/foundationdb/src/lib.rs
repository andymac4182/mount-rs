//! Real FoundationDB metadata + RustFS S3 block composition.
//!
//! The harness supplies a disposable FoundationDB server and a disposable
//! RustFS S3 endpoint. This test deliberately uses the production providers:
//! FoundationDB owns the fenced namespace and `R2BlockStore` owns immutable
//! blocks in RustFS. No in-memory or fake provider is accepted by this gate.

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::driver::FsDriver;
use mount_rs_core::storage::{BlockId, BlockStore, MetadataStore};
use mount_rs_core::{ErrorCode, Loopback, Result};
use mount_rs_foundationdb::{FoundationDbStorage, FoundationDbStorageOptions, LeaseOracle};
use mount_rs_r2::{R2BlockStore, R2Config};
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, PutMode, PutOptions, PutPayload};
use std::collections::BTreeSet;
use std::env;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TEST_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Debug)]
struct DeterministicLeaseOracle {
    now_ms: AtomicU64,
}

impl DeterministicLeaseOracle {
    fn new(now_ms: u64) -> Self {
        Self {
            now_ms: AtomicU64::new(now_ms),
        }
    }

    fn advance_ms(&self, amount: u64) {
        self.now_ms.fetch_add(amount, Ordering::SeqCst);
    }
}

#[async_trait]
impl LeaseOracle for DeterministicLeaseOracle {
    async fn now_ms(&self) -> Result<u64> {
        Ok(self.now_ms.load(Ordering::SeqCst))
    }
}

#[derive(Clone)]
struct TrackedRustFsBlocks {
    inner: R2BlockStore,
    created: Arc<Mutex<BTreeSet<BlockId>>>,
}

impl TrackedRustFsBlocks {
    fn new(
        config: &R2Config,
        prefix: impl Into<String>,
        created: Arc<Mutex<BTreeSet<BlockId>>>,
    ) -> Self {
        Self {
            inner: R2BlockStore::from_config(config, prefix).expect("RustFS block store config"),
            created,
        }
    }

    async fn delete_created(&self) -> Result<()> {
        let ids = self
            .created
            .lock()
            .expect("created block set is not poisoned")
            .iter()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            self.inner.delete(&id).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl BlockStore for TrackedRustFsBlocks {
    fn durable(&self) -> bool {
        self.inner.durable()
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let id = self.inner.put(bytes).await?;
        self.created
            .lock()
            .expect("created block set is not poisoned")
            .insert(id.clone());
        Ok(id)
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.inner.get(id).await
    }

    async fn flush(&self) -> Result<()> {
        self.inner.flush().await
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.inner.delete(id).await
    }
}

fn required_env(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set for the real composition gate"))
}

fn local_rustfs_config() -> R2Config {
    let config = R2Config::from_env().expect("RustFS R2-compatible environment is required");
    assert!(
        config.endpoint.starts_with("http://127.0.0.1:")
            || config.endpoint.starts_with("http://localhost:")
            || config.endpoint.starts_with("http://host.docker.internal:"),
        "composition gate accepts only the harness endpoint, got {}",
        config.endpoint
    );
    config
}

fn patterned_bytes(length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| {
            let value = index as u64;
            ((value.wrapping_mul(0x9e37_79b9) ^ (value >> 3) ^ 0xa5) & 0xff) as u8
        })
        .collect()
}

fn expected_composition_bytes() -> Vec<u8> {
    let mut expected = patterned_bytes(4096 * 3 + 113);
    let patch = patterned_bytes(257);
    expected[4096 + 37..4096 + 37 + patch.len()].copy_from_slice(&patch);
    expected.truncate(4096 + 19);
    expected.resize(4096 * 3 + 29, 0);
    let tail = patterned_bytes(193);
    expected[4096 * 2 + 73..4096 * 2 + 73 + tail.len()].copy_from_slice(&tail);
    expected
}

async fn composition_round_trip(
    cluster_file: &str,
    config: &R2Config,
    volume_prefix: &str,
    block_prefix: &str,
    created: Arc<Mutex<BTreeSet<BlockId>>>,
) -> Result<(Vec<u8>, u64)> {
    let storage = FoundationDbStorage::from_cluster_file(
        cluster_file,
        FoundationDbStorageOptions::new(volume_prefix)
            .with_durable(true)
            .with_persisted_lease_oracle(),
    )?;
    let metadata = storage.metadata();
    let blocks = TrackedRustFsBlocks::new(config, block_prefix, created);
    let filesystem = ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("foundationdb-rustfs-first", 4096)?
            .with_lease_ttl(Duration::from_secs(30)),
    )
    .await?;
    assert!(filesystem.capabilities().durable_writes);

    let loopback = Loopback::new(filesystem.clone());
    let file = loopback.open("/binary", "w+", 0o640).await?;
    let initial = patterned_bytes(4096 * 3 + 113);
    let mut expected = initial.clone();
    file.write(&initial, Some(0)).await?;

    let patch = patterned_bytes(257);
    file.write(&patch, Some(4096 + 37)).await?;
    expected[4096 + 37..4096 + 37 + patch.len()].copy_from_slice(&patch);

    file.truncate(4096 + 19).await?;
    expected.truncate(4096 + 19);
    file.truncate(4096 * 3 + 29).await?;
    expected.resize(4096 * 3 + 29, 0);

    let tail = patterned_bytes(193);
    file.write(&tail, Some(4096 * 2 + 73)).await?;
    expected[4096 * 2 + 73..4096 * 2 + 73 + tail.len()].copy_from_slice(&tail);
    file.sync().await?;

    let mut actual = vec![0; expected.len() + 41];
    let read = file.read(&mut actual, Some(0)).await?;
    assert_eq!(read, expected.len());
    assert_eq!(&actual[..read], expected);
    assert!(
        filesystem
            .metadata_store()
            .load()
            .await?
            .namespace
            .expect("published namespace")
            .nodes
            .values()
            .any(|node| matches!(
                &node.data,
                mount_rs_core::storage::NodeData::File(layout) if layout.extents.len() > 1
            ))
    );

    file.close().await?;
    drop(loopback);
    let revision = metadata.load().await?.revision;
    filesystem.shutdown().await?;
    drop(filesystem);
    drop(storage);
    Ok((expected, revision))
}

async fn verify_reopen(
    cluster_file: &str,
    config: &R2Config,
    volume_prefix: &str,
    block_prefix: &str,
    expected: &[u8],
    created: Arc<Mutex<BTreeSet<BlockId>>>,
) -> Result<()> {
    let storage = FoundationDbStorage::from_cluster_file(
        cluster_file,
        FoundationDbStorageOptions::new(volume_prefix)
            .with_durable(true)
            .with_persisted_lease_oracle(),
    )?;
    let metadata = storage.metadata();
    let blocks = TrackedRustFsBlocks::new(config, block_prefix, created);
    let filesystem = ChunkedFs::open(
        metadata,
        blocks,
        ChunkedOptions::fixed("foundationdb-rustfs-reopen", 16384)?
            .with_lease_ttl(Duration::from_secs(30)),
    )
    .await?;
    let loopback = Loopback::new(filesystem.clone());
    assert_eq!(loopback.read_file("/binary").await?, expected);
    assert_eq!(loopback.stat("/binary").await?.size, expected.len() as u64);
    drop(loopback);
    filesystem.shutdown().await?;
    drop(filesystem);
    drop(storage);
    Ok(())
}

async fn verify_concurrent_acquire(cluster_file: &str, volume_prefix: &str) -> Result<()> {
    let first = FoundationDbStorage::from_cluster_file(
        cluster_file,
        FoundationDbStorageOptions::new(format!("{volume_prefix}/concurrent-acquire"))
            .with_durable(true)
            .with_persisted_lease_oracle(),
    )?;
    let second = FoundationDbStorage::from_cluster_file(
        cluster_file,
        FoundationDbStorageOptions::new(format!("{volume_prefix}/concurrent-acquire"))
            .with_durable(true)
            .with_persisted_lease_oracle(),
    )?;
    let first_metadata = first.metadata();
    let second_metadata = second.metadata();
    let (left, right) = tokio::join!(
        first_metadata.acquire_writer("concurrent-left", Duration::from_secs(30)),
        second_metadata.acquire_writer("concurrent-right", Duration::from_secs(30)),
    );

    match (left, right) {
        (Ok(winner), Err(loser)) => {
            assert_eq!(
                loser.code,
                ErrorCode::Eagain,
                "the losing concurrent acquire must observe the committed lease"
            );
            first_metadata.release_writer(&winner).await?;
        }
        (Err(loser), Ok(winner)) => {
            assert_eq!(
                loser.code,
                ErrorCode::Eagain,
                "the losing concurrent acquire must observe the committed lease"
            );
            second_metadata.release_writer(&winner).await?;
        }
        (Ok(_), Ok(_)) => panic!("concurrent FoundationDB lease acquires both succeeded"),
        (Err(left), Err(right)) => {
            panic!("concurrent FoundationDB lease acquires both failed: {left}; {right}")
        }
    }
    Ok(())
}

async fn verify_cas_and_fencing(cluster_file: &str, volume_prefix: &str) -> Result<()> {
    let first = FoundationDbStorage::from_cluster_file(
        cluster_file,
        FoundationDbStorageOptions::new(volume_prefix)
            .with_durable(true)
            .with_persisted_lease_oracle(),
    )?;
    let second = FoundationDbStorage::from_cluster_file(
        cluster_file,
        FoundationDbStorageOptions::new(volume_prefix)
            .with_durable(true)
            .with_persisted_lease_oracle(),
    )?;
    let first_metadata = first.metadata();
    let second_metadata = second.metadata();
    let first_lease = first_metadata
        .acquire_writer("cas-first", Duration::from_secs(30))
        .await?;
    let busy = second_metadata
        .acquire_writer("cas-busy", Duration::from_secs(30))
        .await
        .expect_err("FDB lease acquisition must be a compare-and-swap");
    assert_eq!(busy.code, ErrorCode::Eagain);

    first_metadata.release_writer(&first_lease).await?;
    let second_lease = second_metadata
        .acquire_writer("cas-second", Duration::from_secs(30))
        .await?;
    assert!(second_lease.fence > first_lease.fence);

    let loaded = second_metadata.load().await?;
    let namespace = loaded.namespace.expect("composed namespace");
    let next_revision = second_metadata
        .publish(loaded.revision, &second_lease, namespace.clone())
        .await?;
    let stale_revision = second_metadata
        .publish(loaded.revision, &second_lease, namespace.clone())
        .await
        .expect_err("the metadata revision CAS must reject a stale expected revision");
    assert_eq!(stale_revision.code, ErrorCode::Eagain);

    second_metadata.release_writer(&second_lease).await?;
    let third_lease = first_metadata
        .acquire_writer("cas-third", Duration::from_secs(30))
        .await?;
    assert!(third_lease.fence > second_lease.fence);
    let stale_fence = first_metadata
        .publish(next_revision, &second_lease, namespace)
        .await
        .expect_err("a released fence must not publish after a replacement lease");
    assert_eq!(stale_fence.code, ErrorCode::Estale);
    first_metadata.release_writer(&third_lease).await
}

async fn verify_chunked_lease_fencing(
    cluster_file: &str,
    config: &R2Config,
    volume_prefix: &str,
    block_prefix: &str,
    created: Arc<Mutex<BTreeSet<BlockId>>>,
) -> Result<()> {
    // Keep ordinary container/CI setup out of the expiry window. The lease
    // remains long-lived while the old filesystem is being opened, then the
    // provider is driven past its persisted expiry through the injected
    // oracle. The lease/fence mutations still execute against the real
    // FoundationDB cluster.
    let clock = Arc::new(DeterministicLeaseOracle::new(1_000_000));
    let oracle: Arc<dyn LeaseOracle> = clock.clone();
    let old_storage = FoundationDbStorage::from_cluster_file(
        cluster_file,
        FoundationDbStorageOptions::new(volume_prefix)
            .with_durable(true)
            .with_oracle_arc(Arc::clone(&oracle)),
    )?;
    let old_blocks = TrackedRustFsBlocks::new(config, block_prefix, Arc::clone(&created));
    let old = ChunkedFs::open(
        old_storage.metadata(),
        old_blocks,
        ChunkedOptions::fixed("foundationdb-rustfs-expiring", 4096)?
            .with_lease_ttl(Duration::from_secs(30)),
    )
    .await?;

    clock.advance_ms(30_000);

    let new_storage = FoundationDbStorage::from_cluster_file(
        cluster_file,
        FoundationDbStorageOptions::new(volume_prefix)
            .with_durable(true)
            .with_oracle_arc(Arc::clone(&oracle)),
    )?;
    let new_blocks = TrackedRustFsBlocks::new(config, block_prefix, Arc::clone(&created));
    let new = ChunkedFs::open(
        new_storage.metadata(),
        new_blocks,
        ChunkedOptions::fixed("foundationdb-rustfs-replacement", 4096)?
            .with_lease_ttl(Duration::from_secs(30)),
    )
    .await?;

    let old_loopback = Loopback::new(old.clone());
    let stale = old_loopback
        .stat("/")
        .await
        .expect_err("the expired ChunkedFs writer must fail closed after replacement");
    assert_eq!(stale.code, ErrorCode::Estale);
    assert!(old.failed());

    let new_loopback = Loopback::new(new.clone());
    assert_eq!(new_loopback.stat("/").await?.ino, 1);
    new.shutdown().await?;

    let old_shutdown = old
        .shutdown()
        .await
        .expect_err("an old fenced writer must not release the replacement lease");
    assert_eq!(old_shutdown.code, ErrorCode::Estale);
    drop(old_loopback);
    drop(new_loopback);
    drop(new_storage);
    drop(old_storage);
    Ok(())
}

async fn verify_exact_rustfs_cleanup(
    config: &R2Config,
    combo_prefix: &str,
    block_prefix: &str,
    blocks: &TrackedRustFsBlocks,
) -> Result<()> {
    let object_store = config.build_store()?;
    let sibling_path = ObjectPath::from(format!("{block_prefix}-sibling/keep"));
    let parent_path = ObjectPath::from(format!("{combo_prefix}/keep"));

    for path in [&sibling_path, &parent_path] {
        object_store
            .put_opts(
                path,
                PutPayload::from(b"must-survive-scoped-cleanup".to_vec()),
                PutOptions {
                    mode: PutMode::Create,
                    ..Default::default()
                },
            )
            .await
            .map_err(|error| {
                mount_rs_core::backend_error(format!("seed cleanup sentinel: {error}"))
            })?;
    }

    let created_is_empty = blocks
        .created
        .lock()
        .expect("created block set is not poisoned")
        .is_empty();
    if created_is_empty {
        // A fresh post-restart client does not inherit the first process's
        // in-memory block-ID ledger. Listing only the exact owned prefix keeps
        // cleanup scoped while still proving the restart process can reap the
        // blocks it reopened.
        let owned = object_store
            .list_with_delimiter(Some(&ObjectPath::from(block_prefix)))
            .await
            .map_err(|error| {
                mount_rs_core::backend_error(format!("list restart block prefix: {error}"))
            })?;
        assert!(
            owned.common_prefixes.is_empty(),
            "restart cleanup found unexpected nested prefixes: {:?}",
            owned.common_prefixes
        );
        for object in owned.objects {
            object_store
                .delete(&object.location)
                .await
                .map_err(|error| {
                    mount_rs_core::backend_error(format!("delete restart block: {error}"))
                })?;
        }
    } else {
        blocks.delete_created().await?;
    }

    let owned = object_store
        .list_with_delimiter(Some(&ObjectPath::from(block_prefix)))
        .await
        .map_err(|error| {
            mount_rs_core::backend_error(format!("list cleaned block prefix: {error}"))
        })?;
    assert!(
        owned.objects.is_empty() && owned.common_prefixes.is_empty(),
        "owned RustFS block prefix was not cleaned exactly: objects={:?} prefixes={:?}",
        owned.objects,
        owned.common_prefixes
    );

    for path in [&sibling_path, &parent_path] {
        let value = object_store
            .get(path)
            .await
            .map_err(|error| {
                mount_rs_core::backend_error(format!("read cleanup sentinel: {error}"))
            })?
            .bytes()
            .await
            .map_err(|error| {
                mount_rs_core::backend_error(format!("read cleanup sentinel body: {error}"))
            })?;
        assert_eq!(value.as_ref(), b"must-survive-scoped-cleanup");
        object_store.delete(path).await.map_err(|error| {
            mount_rs_core::backend_error(format!("delete cleanup sentinel: {error}"))
        })?;
    }

    Ok(())
}

async fn run_real_composition() -> Result<()> {
    let cluster_file = required_env("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE");
    let config = local_rustfs_config();
    let combo_prefix = required_env("RUSTFS_COMBO_PREFIX");
    let volume_prefix = format!("{combo_prefix}/foundationdb-metadata");
    let block_prefix = format!("{combo_prefix}/rustfs-blocks");
    let created = Arc::new(Mutex::new(BTreeSet::new()));

    let network = unsafe { foundationdb::boot() };
    let (expected, revision) = composition_round_trip(
        &cluster_file,
        &config,
        &volume_prefix,
        &block_prefix,
        Arc::clone(&created),
    )
    .await?;
    verify_reopen(
        &cluster_file,
        &config,
        &volume_prefix,
        &block_prefix,
        &expected,
        Arc::clone(&created),
    )
    .await?;
    verify_chunked_lease_fencing(
        &cluster_file,
        &config,
        &volume_prefix,
        &block_prefix,
        Arc::clone(&created),
    )
    .await?;
    verify_concurrent_acquire(&cluster_file, &volume_prefix).await?;
    verify_cas_and_fencing(&cluster_file, &volume_prefix).await?;

    let blocks = TrackedRustFsBlocks::new(&config, block_prefix, created);
    let defer_cleanup = env::var_os("MOUNT_RS_FOUNDATIONDB_DEFER_CLEANUP").is_some();
    if !defer_cleanup {
        verify_exact_rustfs_cleanup(&config, &combo_prefix, blocks.inner.prefix(), &blocks).await?;
    }
    drop(network);
    println!(
        "FOUNDATIONDB_RUSTFS_CHUNKED_PASS revision={revision} volume_prefix={volume_prefix} cleanup_deferred={defer_cleanup}"
    );
    Ok(())
}

async fn run_real_restart_reopen() -> Result<()> {
    let cluster_file = required_env("MOUNT_RS_FOUNDATIONDB_CLUSTER_FILE");
    let config = local_rustfs_config();
    let combo_prefix = required_env("RUSTFS_COMBO_PREFIX");
    let volume_prefix = format!("{combo_prefix}/foundationdb-metadata");
    let block_prefix = format!("{combo_prefix}/rustfs-blocks");
    let created = Arc::new(Mutex::new(BTreeSet::new()));

    let network = unsafe { foundationdb::boot() };
    verify_reopen(
        &cluster_file,
        &config,
        &volume_prefix,
        &block_prefix,
        &expected_composition_bytes(),
        Arc::clone(&created),
    )
    .await?;
    // Re-run the metadata CAS and lease-fence checks after the service restart;
    // this is distinct from the first process's pre-restart evidence.
    verify_cas_and_fencing(&cluster_file, &volume_prefix).await?;

    let blocks = TrackedRustFsBlocks::new(&config, block_prefix, created);
    verify_exact_rustfs_cleanup(&config, &combo_prefix, blocks.inner.prefix(), &blocks).await?;
    drop(network);
    println!("FOUNDATIONDB_RUSTFS_SERVICE_RESTART_PASS volume_prefix={volume_prefix}");
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foundationdb_rustfs_chunked_composition() {
    tokio::time::timeout(TEST_TIMEOUT, run_real_composition())
        .await
        .expect("FoundationDB + RustFS composition exceeded its bounded timeout")
        .expect("FoundationDB + RustFS composition failed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn foundationdb_rustfs_chunked_restart_reopen() {
    tokio::time::timeout(TEST_TIMEOUT, run_real_restart_reopen())
        .await
        .expect("FoundationDB + RustFS restart reopen exceeded its bounded timeout")
        .expect("FoundationDB + RustFS restart reopen failed");
}
