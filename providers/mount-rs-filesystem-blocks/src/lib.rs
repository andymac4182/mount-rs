//! Immutable, content-addressed blocks in an existing private local directory.
//!
//! The provider is block-only. Linux and macOS use descriptor-relative,
//! no-follow operations and create-only publication. See the crate README for
//! durability, constructor, deployment, and maintenance boundaries.

/// Bound a corrupt object header before allocation; applies to puts and gets.
pub const MAX_BLOCK_BYTES: usize = 128 * 1024 * 1024;

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod unix;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use unix::FilesystemBlockStore;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod unsupported {
    use async_trait::async_trait;
    use mount_rs_core::storage::{BlockId, BlockStore};
    use mount_rs_core::{FsError, Result};
    use std::path::Path;

    /// The secure filesystem implementation requires Linux or macOS.
    #[derive(Clone, Debug)]
    pub struct FilesystemBlockStore;

    impl FilesystemBlockStore {
        pub fn open(_root: impl AsRef<Path>, _persistent: bool) -> Result<Self> {
            Err(FsError::enotsup(
                "filesystem block storage requires Linux or macOS",
            ))
        }
    }

    #[async_trait]
    impl BlockStore for FilesystemBlockStore {
        fn durable(&self) -> bool {
            false
        }
        fn persistent(&self) -> bool {
            false
        }
        async fn put(&self, _bytes: &[u8]) -> Result<BlockId> {
            Err(FsError::enotsup("filesystem immutable put"))
        }
        async fn get(&self, _id: &BlockId) -> Result<Vec<u8>> {
            Err(FsError::enotsup("filesystem immutable get"))
        }
        async fn flush(&self) -> Result<()> {
            Err(FsError::enotsup("filesystem flush"))
        }
        async fn delete(&self, _id: &BlockId) -> Result<()> {
            Err(FsError::enotsup("filesystem delete"))
        }
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub use unsupported::FilesystemBlockStore;
