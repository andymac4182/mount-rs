//! Public Rust consumer API for mount-rs.
//!
//! Backend implementations remain separate integration crates. This facade
//! gives applications and the Rust CLI one stable place to construct a
//! filesystem, compose independent metadata/block providers, access the
//! shared [`mount_rs_core::FsDriver`] contract, and finish provider cleanup.

pub mod filesystem;
pub mod options;
mod providers;
mod stores;

pub use filesystem::{BlockStoreDecorator, Filesystem, FilesystemKind};
pub use mount_rs_auto::{
    AutoMount, AutoMountError, AutoMountOptions, AutoProbe, AutoTransport, Transport,
    TransportProbe, probe_transports,
};
pub use mount_rs_chunked::OwnershipMode;
pub use mount_rs_core::{
    Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, Loopback, MkdirOptions, OpenFlags,
    Result, Stats, StatsFs,
};
pub use mount_rs_host::HostFsOptions as HostOptions;
pub use mount_rs_memfs::MemoryOptions;
#[cfg(feature = "observability")]
pub use mount_rs_observability::{
    Telemetry, TelemetryConfig, global as global_telemetry, set_global as set_global_telemetry,
};
pub use options::{FoundationDbLeaseAuthority, SplitOptions, StoreConfig};

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn block_decorator_receives_original_config_and_is_used() {
        struct Reject;
        impl BlockStoreDecorator for Reject {
            fn decorate(
                &self,
                config: &StoreConfig,
                _store: std::sync::Arc<dyn mount_rs_core::storage::BlockStore>,
            ) -> Result<std::sync::Arc<dyn mount_rs_core::storage::BlockStore>> {
                assert!(matches!(config, StoreConfig::Memory));
                Err(mount_rs_core::FsError::new(ErrorCode::Eacces)
                    .with_message("decorator rejection"))
            }
        }
        let error = Filesystem::split_with_block_decorator(
            SplitOptions::memory("decorated", 4096),
            &Reject,
        )
        .await
        .err()
        .unwrap();
        assert_eq!(error.code, ErrorCode::Eacces);
        assert!(error.to_string().contains("decorator rejection"));
    }

    #[tokio::test]
    async fn decorated_filesystem_preserves_io_and_shutdown() {
        struct PassThrough;
        impl BlockStoreDecorator for PassThrough {
            fn decorate(
                &self,
                _config: &StoreConfig,
                store: std::sync::Arc<dyn mount_rs_core::storage::BlockStore>,
            ) -> Result<std::sync::Arc<dyn mount_rs_core::storage::BlockStore>> {
                Ok(store)
            }
        }
        let filesystem = Filesystem::split_with_block_decorator(
            SplitOptions::memory("decorated-io", 4),
            &PassThrough,
        )
        .await
        .unwrap();
        let view = Loopback::from_arc(filesystem.driver());
        view.write_file("/data", b"multiple chunks").await.unwrap();
        assert_eq!(view.read_file("/data").await.unwrap(), b"multiple chunks");
        filesystem.shutdown().await.unwrap();
    }

    #[test]
    fn ownership_builders_preserve_legacy_defaults_and_select_writeback() {
        let legacy = SplitOptions::memory("legacy", 4096);
        assert!(!legacy.writeback);
        assert!(!legacy.concurrent_writes);
        assert!(!legacy.delegated);
        assert!(legacy.checkout_path.is_none());
        let exclusive = legacy.clone().with_ownership_mode(OwnershipMode::Exclusive);
        assert!(exclusive.writeback);
        assert!(!exclusive.concurrent_writes);
        let shared = exclusive.with_ownership_mode(OwnershipMode::Shared);
        assert!(!shared.writeback);
        assert!(shared.concurrent_writes);
        assert!(shared.delegated);
        let scoped = shared.with_checkout_path("/tenant");
        assert_eq!(scoped.checkout_path.as_deref(), Some("/tenant"));
        let legacy_cas = scoped.clone().with_concurrent_writes(true);
        assert!(!legacy_cas.delegated);
        assert!(legacy_cas.checkout_path.is_none());
        let exclusive = scoped.with_ownership_mode(OwnershipMode::Exclusive);
        assert!(!exclusive.delegated);
        assert!(exclusive.checkout_path.is_none());
    }

    #[tokio::test]
    async fn shared_writeback_is_rejected_before_provider_open() {
        let options = SplitOptions::memory("invalid", 4096)
            .with_concurrent_writes(true)
            .with_writeback(true);
        let error = Filesystem::split(options).await.err().unwrap();
        assert_eq!(error.code, ErrorCode::Einval);
        assert!(error.to_string().contains("writeback requires exclusive"));
    }

    #[test]
    fn store_config_debug_redacts_credential_bearing_values() {
        let r2 = StoreConfig::R2 {
            endpoint: "https://example.invalid".to_owned(),
            bucket: "bucket".to_owned(),
            prefix: "volume".to_owned(),
            access_key_id: "access-key".to_owned(),
            secret_access_key: "secret-value".to_owned(),
            durable: true,
        };
        let pglite = StoreConfig::Pglite {
            connection: "postgres://user:password@example.invalid/db".to_owned(),
            volume_key: "volume".to_owned(),
            durable: true,
        };
        let mut rustfs = StoreConfig::RustFs {
            endpoint: "http://127.0.0.1:9878/tenant-secret".to_owned(),
            bucket: "bucket".to_owned(),
            region: "us-east-1".to_owned(),
            prefix: "volume".to_owned(),
            access_key_id: "rustfs-access-key".to_owned(),
            secret_access_key: "rustfs-secret-value".to_owned(),
            durable: true,
        };

        let r2_debug = format!("{r2:?}");
        let pglite_debug = format!("{pglite:?}");
        let rustfs_debug = format!("{rustfs:?}");

        assert!(r2_debug.contains("<redacted>"));
        assert!(!r2_debug.contains("access-key"));
        assert!(!r2_debug.contains("secret-value"));
        assert!(pglite_debug.contains("<redacted>"));
        assert!(!pglite_debug.contains("password"));
        assert!(rustfs_debug.contains("<redacted>"));
        assert!(rustfs_debug.contains("http://127.0.0.1:9878"));
        assert!(!rustfs_debug.contains("tenant-secret"));
        assert!(!rustfs_debug.contains("rustfs-access-key"));
        assert!(!rustfs_debug.contains("rustfs-secret-value"));
        if let StoreConfig::RustFs { endpoint, .. } = &mut rustfs {
            *endpoint = "https://example.invalid:tenant-secret".to_owned();
        }
        assert!(!format!("{rustfs:?}").contains("tenant-secret"));
    }

    #[tokio::test]
    async fn memory_sdk_roundtrip_uses_public_driver() {
        let filesystem = Filesystem::memory(MemoryOptions::default());
        let view = Loopback::from_arc(filesystem.driver());
        view.write_file("/sdk.txt", b"rust sdk").await.unwrap();
        assert_eq!(view.read_file("/sdk.txt").await.unwrap(), b"rust sdk");
        assert_eq!(filesystem.kind(), FilesystemKind::Memory);
        filesystem.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn split_sdk_roundtrip_uses_independent_memory_stores() {
        let filesystem = Filesystem::split(
            SplitOptions::memory("sdk-test-owner", 4).with_identity(1000, 1000, 0),
        )
        .await
        .unwrap();
        let view = Loopback::from_arc(filesystem.driver());
        view.write_file("/split.txt", b"split sdk").await.unwrap();
        assert_eq!(view.read_file("/split.txt").await.unwrap(), b"split sdk");
        assert_eq!(filesystem.kind(), FilesystemKind::SplitStore);
        filesystem.shutdown().await.unwrap();
    }

    #[cfg(not(unix))]
    #[tokio::test]
    async fn sqlite_directory_enrollment_rejects_unsupported_platform_without_publication() {
        use mount_rs_core::storage::MetadataStore;
        let temp = std::env::temp_dir().join(format!(
            "mount-sdk-unsupported-delegation-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let metadata_path = temp.join("metadata.sqlite");
        let mut options = SplitOptions::memory("sdk-owner", 4096);
        options.metadata = StoreConfig::Sqlite {
            path: metadata_path.clone(),
        };
        options.blocks = StoreConfig::Sqlite {
            path: temp.join("blocks.sqlite"),
        };
        let bootstrap = Filesystem::split(options.clone()).await.unwrap();
        bootstrap.shutdown().await.unwrap();
        let metadata = mount_rs_sqlite::SqliteMetadataStore::open(&metadata_path).unwrap();
        let before = metadata.load().await.unwrap();
        let error = Filesystem::enroll_directory_ownership(
            options.with_ownership_mode(OwnershipMode::Shared),
            before.revision,
        )
        .await
        .unwrap_err();
        assert!(error.is(mount_rs_core::ErrorCode::Enotsup));
        let after = metadata.load().await.unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(
            after.namespace.as_ref().map(|namespace| (
                &namespace.nodes,
                namespace.root,
                namespace.next_inode
            )),
            before.namespace.as_ref().map(|namespace| (
                &namespace.nodes,
                namespace.root,
                namespace.next_inode
            ))
        );
        drop(metadata);
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sqlite_sdk_directory_handoff_and_expected_fence_recovery() {
        use mount_rs_core::storage::MetadataStore;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let temp = std::env::temp_dir().join(format!(
            "mount-sdk-delegation-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&temp).unwrap();
        let metadata_path = temp.join("metadata.sqlite");
        let mut options = SplitOptions::memory("sdk-owner", 4096);
        options.metadata = StoreConfig::Sqlite {
            path: metadata_path.clone(),
        };
        options.blocks = StoreConfig::Sqlite {
            path: temp.join("blocks.sqlite"),
        };
        let bootstrap = Filesystem::split(options.clone()).await.unwrap();
        let view = Loopback::from_arc(bootstrap.driver());
        view.mkdir("/left", MkdirOptions::default()).await.unwrap();
        view.mkdir("/right", MkdirOptions::default()).await.unwrap();
        bootstrap.shutdown().await.unwrap();
        let revision = mount_rs_sqlite::SqliteMetadataStore::open(&metadata_path)
            .unwrap()
            .load()
            .await
            .unwrap()
            .revision;
        let options = options.with_ownership_mode(OwnershipMode::Shared);
        Filesystem::enroll_directory_ownership(options.clone(), revision)
            .await
            .unwrap();
        let left = Filesystem::split(options.clone().with_checkout_path("/left"))
            .await
            .unwrap();
        let right = Filesystem::split(options.clone().with_checkout_path("/right"))
            .await
            .unwrap();
        let left_view = Loopback::from_arc(left.driver());
        let right_view = Loopback::from_arc(right.driver());
        left_view
            .write_file("/left/data", b"left durable bytes")
            .await
            .unwrap();
        right_view
            .write_file("/right/data", b"right durable bytes")
            .await
            .unwrap();
        assert!(left_view.read_file("/right/data").await.is_err());
        assert!(
            Filesystem::split(options.clone().with_checkout_path("/left"))
                .await
                .is_err()
        );
        let first = left.delegation_status().await.unwrap().unwrap();
        left.checkin_scope().await.unwrap();
        let handed = Filesystem::split(options.clone().with_checkout_path("/left"))
            .await
            .unwrap();
        let handed_view = Loopback::from_arc(handed.driver());
        assert_eq!(
            handed_view.read_file("/left/data").await.unwrap(),
            b"left durable bytes"
        );
        let second = handed.delegation_status().await.unwrap().unwrap();
        assert!(second.token.fence > first.token.fence);
        assert!(
            Filesystem::recover_directory_ownership(
                options.clone(),
                first.token.root,
                first.token.fence
            )
            .await
            .is_err()
        );
        Filesystem::recover_directory_ownership(
            options.clone(),
            second.token.root,
            second.token.fence,
        )
        .await
        .unwrap();
        assert!(
            handed_view
                .write_file("/left/data", b"stale")
                .await
                .is_err()
        );
        let fresh = Filesystem::split(options.clone().with_checkout_path("/left"))
            .await
            .unwrap();
        assert_eq!(
            Loopback::from_arc(fresh.driver())
                .read_file("/left/data")
                .await
                .unwrap(),
            b"left durable bytes"
        );
        let state = Filesystem::directory_ownership_state(options)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.grants.len(), 2);
        fresh.shutdown().await.unwrap();
        right.shutdown().await.unwrap();
        left.shutdown().await.unwrap();
        let _ = handed.shutdown().await;
        drop((
            left,
            right,
            handed,
            fresh,
            bootstrap,
            left_view,
            right_view,
            handed_view,
        ));
        std::fs::remove_dir_all(temp).unwrap();
    }

    #[tokio::test]
    async fn concurrent_sdk_rejects_process_local_block_backings_before_open() {
        let metadata = StoreConfig::FoundationDb {
            cluster_file: "/nonexistent/fdb.cluster".into(),
            volume_key: "sdk-concurrent-check".to_owned(),
            durable: true,
            lease_authority: FoundationDbLeaseAuthority::RevisionCas,
        };
        for blocks in [
            StoreConfig::Memory,
            StoreConfig::Sqlite {
                path: "/tmp/sdk-local-blocks.sqlite".into(),
            },
        ] {
            let options = SplitOptions {
                metadata: metadata.clone(),
                blocks,
                ..SplitOptions::memory("sdk-concurrent-check", 4096)
            }
            .with_concurrent_writes(true);
            let error = Filesystem::split(options)
                .await
                .err()
                .expect("local blocks must fail before provider open");
            assert_eq!(error.code, ErrorCode::Einval);
            assert!(error.to_string().contains("shared block"));
        }
    }

    #[tokio::test]
    async fn sqlite_concurrent_sdk_rejects_volatile_and_cross_provider_local_blocks() {
        let sqlite = StoreConfig::Sqlite {
            path: "/nonexistent/mount-rs-concurrent-metadata.sqlite".into(),
        };
        for (metadata, blocks) in [
            (sqlite.clone(), StoreConfig::Memory),
            (
                StoreConfig::Sqlite {
                    path: ":memory:".into(),
                },
                StoreConfig::Sqlite {
                    path: "/nonexistent/mount-rs-concurrent-blocks.sqlite".into(),
                },
            ),
            (
                sqlite.clone(),
                StoreConfig::Sqlite {
                    path: ":memory:".into(),
                },
            ),
            (
                StoreConfig::Pglite {
                    connection: "postgres://127.0.0.1:1/missing".to_owned(),
                    volume_key: "metadata".to_owned(),
                    durable: true,
                },
                StoreConfig::Sqlite {
                    path: "/nonexistent/mount-rs-concurrent-blocks.sqlite".into(),
                },
            ),
        ] {
            let error = Filesystem::split(
                SplitOptions {
                    metadata,
                    blocks,
                    ..SplitOptions::memory("sqlite-concurrent-check", 4096)
                }
                .with_concurrent_writes(true),
            )
            .await
            .err()
            .expect("unsupported pairing must fail before provider open");
            assert_eq!(error.code, ErrorCode::Einval);
        }
    }

    #[cfg(feature = "observability")]
    #[tokio::test]
    async fn opt_in_driver_telemetry_preserves_sdk_roundtrip() {
        let filesystem = Filesystem::memory(MemoryOptions::default());
        let telemetry = Telemetry::new(TelemetryConfig::enabled("sdk-test"));
        let view = Loopback::from_arc(filesystem.driver_with_telemetry(telemetry.clone()));
        view.write_file("/observed.txt", b"observed").await.unwrap();
        assert_eq!(view.read_file("/observed.txt").await.unwrap(), b"observed");
        assert!(telemetry.snapshot().operations > 0);
        filesystem.shutdown().await.unwrap();
    }
}
