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

pub use filesystem::{Filesystem, FilesystemKind};
pub use mount_rs_auto::{
    AutoMount, AutoMountError, AutoMountOptions, AutoProbe, AutoTransport, Transport,
    TransportProbe, probe_transports,
};
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

        let r2_debug = format!("{r2:?}");
        let pglite_debug = format!("{pglite:?}");

        assert!(r2_debug.contains("<redacted>"));
        assert!(!r2_debug.contains("access-key"));
        assert!(!r2_debug.contains("secret-value"));
        assert!(pglite_debug.contains("<redacted>"));
        assert!(!pglite_debug.contains("password"));
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
