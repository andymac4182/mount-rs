//! Keep a prepared response's handle alive until explicit close or final drop.
use mount_rs_core::{ErrorCode, FileHandle, FsError, Result, Stats};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

const OPEN: u8 = 0;
const CLOSING: u8 = 1;
const CLOSED: u8 = 2;

pub(crate) struct ResponseHandle {
    inner: Arc<dyn FileHandle>,
    /// `OPEN`, `CLOSING` while one close is awaiting the driver, or `CLOSED`.
    /// Each response has one owner, which closes it once and may retry, so
    /// nothing ever has to wait here: a close that finds another in flight is
    /// refused instead.
    state: AtomicU8,
    runtime: Option<tokio::runtime::Handle>,
}
impl ResponseHandle {
    pub(crate) fn wrap(inner: Arc<dyn FileHandle>) -> Arc<dyn FileHandle> {
        Arc::new(Self {
            inner,
            state: AtomicU8::new(OPEN),
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
        match self
            .state
            .compare_exchange(OPEN, CLOSING, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => {}
            Err(CLOSED) => return Ok(()),
            // Not told Ok: the close in flight may yet fail.
            Err(_) => return Err(FsError::new(ErrorCode::Ebusy)),
        }
        // A close that fails or is cancelled leaves the handle open, so the
        // owner can retry and drop still closes it.
        let attempt = CloseAttempt(&self.state);
        self.inner.close().await?;
        attempt.0.store(CLOSED, Ordering::Release);
        std::mem::forget(attempt);
        Ok(())
    }
}

struct CloseAttempt<'a>(&'a AtomicU8);
impl Drop for CloseAttempt<'_> {
    fn drop(&mut self) {
        self.0.store(OPEN, Ordering::Release);
    }
}
impl Drop for ResponseHandle {
    fn drop(&mut self) {
        if self.state.load(Ordering::Acquire) != CLOSED
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::task::{Context, Poll, Waker};

    /// A handle whose close can be made to hang or fail.
    #[derive(Default)]
    struct Probe {
        closes: AtomicUsize,
        hang: std::sync::atomic::AtomicBool,
        fail: std::sync::atomic::AtomicBool,
    }
    #[async_trait::async_trait]
    impl FileHandle for Probe {
        async fn read(&self, _: &mut [u8], _: Option<u64>) -> Result<usize> {
            Ok(0)
        }
        async fn write(&self, _: &[u8], _: Option<u64>) -> Result<usize> {
            Ok(0)
        }
        async fn stat(&self) -> Result<Stats> {
            Err(FsError::new(ErrorCode::Enosys))
        }
        async fn truncate(&self, _: u64) -> Result<()> {
            Ok(())
        }
        async fn sync(&self) -> Result<()> {
            Ok(())
        }
        async fn datasync(&self) -> Result<()> {
            Ok(())
        }
        async fn close(&self) -> Result<()> {
            self.closes.fetch_add(1, Ordering::SeqCst);
            if self.hang.load(Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err(FsError::new(ErrorCode::Eio));
            }
            Ok(())
        }
    }

    fn ready<T>(future: impl Future<Output = T>) -> T {
        let mut future = std::pin::pin!(future);
        match future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the close did not finish"),
        }
    }

    #[test]
    fn a_close_that_fails_or_is_cancelled_can_be_retried_and_closes_once() {
        let probe = Arc::new(Probe::default());
        let handle = ResponseHandle::wrap(probe.clone());

        probe.fail.store(true, Ordering::SeqCst);
        assert_eq!(ready(handle.close()).unwrap_err().code, ErrorCode::Eio);
        probe.fail.store(false, Ordering::SeqCst);

        probe.hang.store(true, Ordering::SeqCst);
        {
            let mut cancelled = Box::pin(handle.close());
            let mut context = Context::from_waker(Waker::noop());
            assert!(cancelled.as_mut().poll(&mut context).is_pending());
            // A close that overlaps one in flight is not told Ok.
            assert_eq!(ready(handle.close()).unwrap_err().code, ErrorCode::Ebusy);
        }
        probe.hang.store(false, Ordering::SeqCst);

        ready(handle.close()).unwrap();
        ready(handle.close()).unwrap();
        assert_eq!(probe.closes.load(Ordering::SeqCst), 3);
    }
}
