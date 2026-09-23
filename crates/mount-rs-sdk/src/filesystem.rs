//! Filesystem facade and its shutdown lifecycle.

use std::path::Path;
use std::sync::Arc;

use mount_rs_chunked::{ChunkedFs, ChunkedOptions, migrate_mrc1_backing};
use mount_rs_core::storage::ConcurrentBackingId;
use mount_rs_core::{ErrorCode, FsDriver, FsError, Result};
use mount_rs_host::{HostFs, HostFsOptions};
use mount_rs_memfs::{MemoryFs, MemoryOptions};
use mount_rs_sqlite_fs::{SqliteFs, open_sqlite};

use crate::options::{FoundationDbLeaseAuthority, SplitOptions, StoreConfig};
use crate::providers::{StorageResources, open_storage};
use crate::stores::{ErasedBlockStore, ErasedMetadataStore};
#[cfg(feature = "observability")]
use crate::{Telemetry, global_telemetry};

/// Identifies the top-level filesystem construction used by an SDK consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesystemKind {
    Memory,
    Host,
    Sqlite,
    SplitStore,
}

/// A Rust SDK filesystem and its provider cleanup lifecycle.
pub struct Filesystem {
    inner: FilesystemInner,
}

enum FilesystemInner {
    Memory(MemoryFs),
    Host(HostFs),
    Sqlite(SqliteFs),
    Split(
        ChunkedFs<ErasedMetadataStore, ErasedBlockStore>,
        StorageResources,
    ),
}

impl Filesystem {
    /// Construct a volatile in-memory filesystem.
    pub fn memory(options: MemoryOptions) -> Self {
        Self {
            inner: FilesystemInner::Memory(MemoryFs::new(options)),
        }
    }

    /// Construct a host-backed filesystem rooted at `root`.
    pub fn host(root: impl AsRef<Path>, options: HostFsOptions) -> Self {
        Self {
            inner: FilesystemInner::Host(HostFs::with_options(root, options)),
        }
    }

    /// Open a durable SQLite-backed filesystem.
    pub async fn sqlite(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            inner: FilesystemInner::Sqlite(open_sqlite(path).await?),
        })
    }

    /// Open a filesystem composed from independent metadata and block stores.
    pub async fn split(options: SplitOptions) -> Result<Self> {
        if options.chunk_size_bytes == 0 {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("chunk_size_bytes must be greater than zero"));
        }
        validate_concurrent_split_options(&options)?;
        let chunk_options = ChunkedOptions::fixed(options.owner, options.chunk_size_bytes)?
            .with_lease_ttl(options.lease_ttl)
            .with_concurrent_writes(options.concurrent_writes)
            .with_identity(options.uid, options.gid, options.umask);
        let opened = open_storage(&options.metadata, &options.blocks).await?;
        let resources = opened.resources.clone();
        match ChunkedFs::open(opened.metadata, opened.blocks, chunk_options).await {
            Ok(driver) => Ok(Self {
                inner: FilesystemInner::Split(driver, resources),
            }),
            Err(error) => {
                let _ = resources.close().await;
                Err(error)
            }
        }
    }

    /// Bind an offline MRC1 volume to its verified shared block backing.
    /// Call this only after all older mounts of the volume have stopped.
    pub async fn migrate_concurrent_backing(
        options: SplitOptions,
        expected_revision: u64,
    ) -> Result<ConcurrentBackingId> {
        if !options.concurrent_writes {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("migrate-concurrent-backing requires concurrent_writes"));
        }
        validate_concurrent_split_options(&options)?;
        let opened = open_storage(&options.metadata, &options.blocks).await?;
        let migration =
            migrate_mrc1_backing(&opened.metadata, &opened.blocks, expected_revision).await;
        let cleanup = opened.close().await;
        match migration {
            Ok(backing) => {
                cleanup?;
                Ok(backing)
            }
            Err(error) => Err(error),
        }
    }

    /// Return the shared driver contract used by transports and loopback
    /// clients. The returned handle remains valid until this filesystem is
    /// shut down.
    pub fn driver(&self) -> Arc<dyn FsDriver> {
        match &self.inner {
            FilesystemInner::Memory(driver) => Arc::new(driver.clone()),
            FilesystemInner::Host(driver) => Arc::new(driver.clone()),
            FilesystemInner::Sqlite(driver) => Arc::new(driver.clone()),
            FilesystemInner::Split(driver, _) => Arc::new(driver.clone()),
        }
    }

    /// Return a driver decorated with optional application-owned telemetry.
    ///
    /// The decorator is feature-gated so the default SDK build has no
    /// observability dependency or runtime work. It only instruments this
    /// returned driver view; filesystem construction, shutdown, and the
    /// existing [`Self::driver`] behavior remain unchanged.
    #[cfg(feature = "observability")]
    pub fn driver_with_telemetry(&self, telemetry: Telemetry) -> Arc<dyn FsDriver> {
        mount_rs_observability::InstrumentedDriver::from_arc(self.driver(), telemetry).into_arc()
    }

    /// Return a driver decorated with the process-wide application telemetry.
    ///
    /// Applications can install exporters and then call
    /// [`crate::set_global_telemetry`] during startup. The default global handle is
    /// disabled, so this method is also a no-op at runtime until the
    /// application explicitly enables telemetry.
    #[cfg(feature = "observability")]
    pub fn observed_driver(&self) -> Arc<dyn FsDriver> {
        self.driver_with_telemetry(global_telemetry())
    }

    pub const fn kind(&self) -> FilesystemKind {
        match &self.inner {
            FilesystemInner::Memory(_) => FilesystemKind::Memory,
            FilesystemInner::Host(_) => FilesystemKind::Host,
            FilesystemInner::Sqlite(_) => FilesystemKind::Sqlite,
            FilesystemInner::Split(_, _) => FilesystemKind::SplitStore,
        }
    }

    /// Release writer leases and provider resources in the safe order.
    pub async fn shutdown(&self) -> Result<()> {
        match &self.inner {
            FilesystemInner::Split(driver, resources) => {
                let driver_result = driver.shutdown().await;
                let resources_result = resources.close().await;
                driver_result.and(resources_result)
            }
            FilesystemInner::Memory(_) | FilesystemInner::Host(_) | FilesystemInner::Sqlite(_) => {
                Ok(())
            }
        }
    }
}

fn sqlite_durable_path(path: &Path) -> bool {
    let value = path.to_string_lossy();
    !value.is_empty() && value != ":memory:" && !value.starts_with("file:")
}

fn validate_concurrent_split_options(options: &SplitOptions) -> Result<()> {
    if options.concurrent_writes {
        match &options.metadata {
            StoreConfig::FoundationDb {
                lease_authority: FoundationDbLeaseAuthority::RevisionCas,
                ..
            }
            | StoreConfig::Pglite { .. } => {}
            StoreConfig::Sqlite { path } if sqlite_durable_path(path) => {}
            StoreConfig::Sqlite { .. } => {
                return Err(FsError::new(ErrorCode::Einval).with_message(
                    "concurrent_writes SQLite metadata requires a durable local database path",
                ));
            }
            _ => {
                return Err(FsError::new(ErrorCode::Einval).with_message(
                    "concurrent_writes requires SQLite, PGlite, or FoundationDB revision-CAS metadata",
                ));
            }
        }
        match &options.blocks {
            StoreConfig::Memory => {
                return Err(FsError::new(ErrorCode::Einval).with_message(
                    "concurrent_writes requires a shared block provider; memory blocks are unavailable to independent mounts",
                ));
            }
            StoreConfig::Sqlite { path }
                if !matches!(&options.metadata, StoreConfig::Sqlite { .. })
                    || !sqlite_durable_path(path) =>
            {
                return Err(FsError::new(ErrorCode::Einval).with_message(
                    "concurrent_writes requires a shared block provider; local SQLite blocks require local SQLite metadata on the same host and a durable block database path",
                ));
            }
            _ => {}
        }
    } else if matches!(
        &options.metadata,
        StoreConfig::FoundationDb {
            lease_authority: FoundationDbLeaseAuthority::RevisionCas,
            ..
        }
    ) {
        return Err(FsError::new(ErrorCode::Einval)
            .with_message("FoundationDB revision-CAS metadata requires concurrent_writes"));
    }
    Ok(())
}

impl Clone for Filesystem {
    fn clone(&self) -> Self {
        let inner = match &self.inner {
            FilesystemInner::Memory(driver) => FilesystemInner::Memory(driver.clone()),
            FilesystemInner::Host(driver) => FilesystemInner::Host(driver.clone()),
            FilesystemInner::Sqlite(driver) => FilesystemInner::Sqlite(driver.clone()),
            FilesystemInner::Split(driver, resources) => {
                FilesystemInner::Split(driver.clone(), resources.clone())
            }
        };
        Self { inner }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrate_requires_concurrent_writes_before_opening_backing() {
        let options = SplitOptions::memory("sdk-migration-disabled", 4096);
        let error = Filesystem::migrate_concurrent_backing(options, 0)
            .await
            .expect_err("migration requires the concurrent writer mode");
        assert_eq!(error.code, ErrorCode::Einval);
        assert!(error.to_string().contains("concurrent_writes"));
    }

    #[tokio::test]
    async fn migrate_reuses_split_pairing_rules_before_opening_provider() {
        let options = SplitOptions {
            metadata: StoreConfig::FoundationDb {
                cluster_file: "/nonexistent/sdk-migration-fdb.cluster".into(),
                volume_key: "sdk-migration-pairing".to_owned(),
                durable: true,
                lease_authority: FoundationDbLeaseAuthority::RevisionCas,
            },
            blocks: StoreConfig::Memory,
            ..SplitOptions::memory("sdk-migration-pairing", 4096)
        }
        .with_concurrent_writes(true);
        let error = Filesystem::migrate_concurrent_backing(options, 0)
            .await
            .expect_err("local blocks must fail before FoundationDB opens");
        assert_eq!(error.code, ErrorCode::Einval);
        assert!(error.to_string().contains("shared block"));
    }
}
