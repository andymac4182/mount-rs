//! Keep a prepared response's handle alive until explicit close or final drop.
use mount_rs_core::turn_gate::TurnLock;
use mount_rs_core::{FileHandle, Result, Stats};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(crate) struct ResponseHandle {
    inner: Arc<dyn FileHandle>,
    closed: AtomicBool,
    close_gate: TurnLock<()>,
    runtime: Option<tokio::runtime::Handle>,
}
impl ResponseHandle {
    pub(crate) fn wrap(inner: Arc<dyn FileHandle>) -> Arc<dyn FileHandle> {
        Arc::new(Self {
            inner,
            closed: AtomicBool::new(false),
            close_gate: TurnLock::new(()),
            runtime: tokio::runtime::Handle::try_current().ok(),
        })
    }
}
#[async_trait::async_trait]
impl FileHandle for ResponseHandle {
    fn fd(&self) -> Option<u64> {
        self.inner.fd()
    }
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> Result<usize> {
        self.inner.read(buffer, position).await
    }
    async fn write(&self, buffer: &[u8], position: Option<u64>) -> Result<usize> {
        self.inner.write(buffer, position).await
    }
    async fn stat(&self) -> Result<Stats> {
        self.inner.stat().await
    }
    async fn truncate(&self, length: u64) -> Result<()> {
        self.inner.truncate(length).await
    }
    async fn sync(&self) -> Result<()> {
        self.inner.sync().await
    }
    async fn datasync(&self) -> Result<()> {
        self.inner.datasync().await
    }
    async fn close(&self) -> Result<()> {
        let _guard = self.close_gate.lock().await;
        if !self.closed.load(Ordering::Acquire) {
            self.inner.close().await?;
            self.closed.store(true, Ordering::Release);
        }
        Ok(())
    }
}
impl Drop for ResponseHandle {
    fn drop(&mut self) {
        if !self.closed.load(Ordering::Acquire)
            && let Some(runtime) = &self.runtime
        {
            let inner = self.inner.clone();
            // No synchronous I/O in Drop. A stopped runtime cannot promise
            // cleanup; server drain tracking is a separate lifecycle gate.
            runtime.spawn(async move {
                let _ = inner.close().await;
            });
        }
    }
}
