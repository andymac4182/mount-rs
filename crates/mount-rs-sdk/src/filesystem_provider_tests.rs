//! Filesystem block-provider SDK construction and reopen controls.
use super::*;
use crate::{Filesystem, Loopback, SplitOptions};
use mount_rs_core::ErrorCode;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

struct OwnedDirectory(PathBuf);
impl OwnedDirectory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "mount-rs-sdk-filesystem-blocks-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        Self::create(&path);
        Self(path)
    }
    fn create(path: &std::path::Path) {
        std::fs::DirBuilder::new().mode(0o700).create(path).unwrap();
    }
}
impl Drop for OwnedDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn filesystem_blocks_preserve_roots_persistence_and_independent_context_reopen() {
    let directory = OwnedDirectory::new();
    let context = StorageContext::new(1).unwrap();
    for persistent in [false, true] {
        let root = directory.0.join(format!("blocks-{persistent}"));
        OwnedDirectory::create(&root);
        let config = StoreConfig::Filesystem {
            root: root.clone(),
            persistent,
        };
        let (first, resources) = open_blocks(&config, Some(&context), None).await.unwrap();
        assert!(resources.is_empty());
        assert!(!first.durable());
        assert_eq!(first.persistent(), persistent);
        let id = first.put(b"filesystem SDK bytes").await.unwrap();
        first.flush().await.unwrap();
        let backing = first.prepare_concurrent_backing().await.unwrap();
        drop(first);
        let (reopened, resources) = open_blocks(&config, None, None).await.unwrap();
        assert!(resources.is_empty());
        assert!(!reopened.durable());
        assert_eq!(reopened.persistent(), persistent);
        reopened.verify_concurrent_backing(backing).await.unwrap();
        assert_eq!(
            reopened.prepare_concurrent_backing().await.unwrap(),
            backing
        );
        assert_eq!(reopened.get(&id).await.unwrap(), b"filesystem SDK bytes");
        let other_root = directory.0.join(format!("other-{persistent}"));
        OwnedDirectory::create(&other_root);
        let other = StoreConfig::Filesystem {
            root: other_root,
            persistent,
        };
        let (different, _) = open_blocks(&other, Some(&context), None).await.unwrap();
        assert_ne!(
            different.prepare_concurrent_backing().await.unwrap(),
            backing
        );
        assert!(different.verify_concurrent_backing(backing).await.is_err());
        assert!(different.get(&id).await.is_err());
    }
    context.close().await.unwrap();
}

#[tokio::test]
async fn filesystem_metadata_rejects_before_creating_the_directory() {
    let directory = OwnedDirectory::new();
    let root = directory.0.join("metadata-is-not-filesystem");
    let config = StoreConfig::Filesystem {
        root: root.clone(),
        persistent: true,
    };
    let error = open_metadata(&config, &StoreConfig::Memory, None, None)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, ErrorCode::Enotsup);
    assert!(!root.exists());
}

#[tokio::test]
async fn filesystem_blocks_do_not_create_a_directory_after_context_close() {
    let directory = OwnedDirectory::new();
    let root = directory.0.join("closed-context-blocks");
    let context = StorageContext::new(1).unwrap();
    context.close().await.unwrap();
    let error = open_storage_in_context(
        &StoreConfig::Memory,
        &StoreConfig::Filesystem {
            root: root.clone(),
            persistent: true,
        },
        None,
        Some(&context),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::Estale);
    assert!(!root.exists());
}

#[tokio::test]
async fn filesystem_blocks_reopen_through_the_public_split_driver() {
    let directory = OwnedDirectory::new();
    let context = StorageContext::new(1).unwrap();
    let mut options = SplitOptions::memory("filesystem-sdk-owner", 4);
    options.metadata = StoreConfig::Sqlite {
        path: directory.0.join("metadata.sqlite"),
    };
    let blocks_root = directory.0.join("blocks");
    OwnedDirectory::create(&blocks_root);
    options.blocks = StoreConfig::Filesystem {
        root: blocks_root,
        persistent: true,
    };
    let first = Filesystem::split_with_context(options.clone(), &context)
        .await
        .unwrap();
    assert!(!first.driver().capabilities().durable_writes);
    let first_view = Loopback::from_arc(first.driver());
    first_view
        .write_file("/roundtrip", b"multiple immutable chunks")
        .await
        .unwrap();
    first.shutdown().await.unwrap();
    drop(first_view);
    drop(first);
    options.owner = "filesystem-sdk-reopened".into();
    let second = Filesystem::split_with_context(options, &context)
        .await
        .unwrap();
    assert!(!second.driver().capabilities().durable_writes);
    let second_view = Loopback::from_arc(second.driver());
    assert_eq!(
        second_view.read_file("/roundtrip").await.unwrap(),
        b"multiple immutable chunks"
    );
    second.shutdown().await.unwrap();
    drop(second_view);
    drop(second);
    context.close().await.unwrap();
}
