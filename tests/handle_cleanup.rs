use async_trait::async_trait;
use mount_rs_core::*;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

struct FailingHandle(Arc<AtomicUsize>);
#[async_trait]
impl FileHandle for FailingHandle {
    async fn read(&self, _: &mut [u8], _: Option<u64>) -> Result<usize> {
        Err(FsError::new(ErrorCode::Eio))
    }
    async fn write(&self, _: &[u8], _: Option<u64>) -> Result<usize> {
        Ok(usize::MAX)
    }
    async fn stat(&self) -> Result<Stats> {
        Err(FsError::enosys("fstat"))
    }
    async fn truncate(&self, _: u64) -> Result<()> {
        Err(FsError::enosys("ftruncate"))
    }
    async fn close(&self) -> Result<()> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
struct Driver(Arc<AtomicUsize>);
#[async_trait]
impl FsDriver for Driver {
    fn capabilities(&self) -> Capabilities {
        Capabilities::default()
    }
    async fn stat(&self, _: &str) -> Result<Stats> {
        Err(FsError::enosys("stat"))
    }
    async fn readdir(&self, _: &str) -> Result<Vec<DirEntry>> {
        Err(FsError::enosys("readdir"))
    }
    async fn open(&self, _: &str, _: &str, _: u32) -> Result<Arc<dyn FileHandle>> {
        Ok(Arc::new(FailingHandle(self.0.clone())))
    }
}
#[tokio::test]
async fn helpers_close_handles_after_io_failure_or_invalid_byte_count() {
    let closed = Arc::new(AtomicUsize::new(0));
    let fs = Loopback::new(Driver(closed.clone()));
    assert_eq!(fs.lstat("/f").await.unwrap_err().code, ErrorCode::Enosys);
    assert_eq!(fs.read_file("/f").await.unwrap_err().code, ErrorCode::Eio);
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    assert_eq!(
        fs.write_file("/f", b"hello").await.unwrap_err().code,
        ErrorCode::Eio
    );
    assert_eq!(closed.load(Ordering::SeqCst), 2);
}
