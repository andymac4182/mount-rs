//! Own expiry tasks across a server's volume registries. Listeners must be
//! drained before shutdown; SDK filesystems must outlive successful cleanup.
use crate::VolumeSessions;
use mount_rs_core::{ErrorCode, FsError, Result};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

pub struct SessionSupervisor {
    registries: Arc<Vec<Arc<VolumeSessions>>>,
    cancel: CancellationToken,
    failed: Arc<AtomicBool>,
    task: crate::join_slot::JoinSlot<Result<()>>,
}
fn failure() -> FsError {
    FsError::new(ErrorCode::Eio).with_syscall("session-supervisor")
}
// Construct outside the spawned future so aborting before its first poll also
// stops admission. An unexpected supervisor exit must fail closed.
struct SupervisorGuard {
    registries: Arc<Vec<Arc<VolumeSessions>>>,
    cancel: CancellationToken,
    failed: Arc<AtomicBool>,
    completed: bool,
}
impl Drop for SupervisorGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.failed.store(true, Ordering::Release);
            for registry in self.registries.iter() {
                registry.stop_admission();
            }
            self.cancel.cancel();
        }
    }
}
impl SessionSupervisor {
    /// Start at most 4096 distinct volume expiry workers on the current runtime.
    pub fn start(registries: Vec<Arc<VolumeSessions>>) -> Result<Self> {
        if registries.is_empty() || registries.len() > 4096 {
            return Err(FsError::new(ErrorCode::Einval).with_syscall("session-supervisor"));
        }
        let mut unique = std::collections::BTreeSet::new();
        if registries
            .iter()
            .any(|registry| !unique.insert(Arc::as_ptr(registry) as usize))
        {
            return Err(FsError::new(ErrorCode::Einval).with_syscall("session-supervisor"));
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| failure())?;
        let registries = Arc::new(registries);
        let cancel = CancellationToken::new();
        let failed = Arc::new(AtomicBool::new(false));
        let owned = registries.clone();
        let stop = cancel.clone();
        let failed_task = failed.clone();
        let guard = SupervisorGuard {
            registries: registries.clone(),
            cancel: cancel.clone(),
            failed: failed.clone(),
            completed: false,
        };
        let task = runtime.spawn(async move {
            let mut guard = guard;
            let mut workers = JoinSet::new();
            for registry in owned.iter() {
                let registry = registry.clone();
                let stop = stop.clone();
                workers.spawn(async move {
                    registry.run_expiry(stop, registry.idle_timeout() / 3).await
                });
            }
            let mut first = None;
            while let Some(result) = workers.join_next().await {
                let result = result.map_err(|_| failure()).and_then(|result| result);
                let error = match result {
                    Err(error) => Some(error),
                    Ok(()) if !stop.is_cancelled() => Some(failure()),
                    Ok(()) => None,
                };
                if let Some(error) = error {
                    failed_task.store(true, Ordering::Release);
                    for registry in owned.iter() {
                        registry.stop_admission();
                    }
                    stop.cancel();
                    if first.is_none() {
                        first = Some(error);
                    }
                }
            }
            guard.completed = true;
            first.map_or(Ok(()), Err)
        });
        Ok(Self {
            registries,
            cancel,
            failed,
            task: crate::join_slot::JoinSlot::new(task),
        })
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    /// Stop admission and bound the asynchronous drain wait. On timeout, the
    /// supervisor retains its join/registry ownership for another shutdown call.
    /// Keep the SDK alive until a drain succeeds. Like Tokio timeouts, this
    /// cannot preempt a backend that blocks synchronously without yielding.
    pub async fn shutdown_with_timeout(&self, wait: std::time::Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now()
            .checked_add(wait)
            .filter(|_| !wait.is_zero())
            .ok_or_else(|| FsError::new(ErrorCode::Einval).with_syscall("session-drain-timeout"))?;
        match tokio::time::timeout_at(deadline, self.shutdown()).await {
            Ok(result) => result,
            Err(_) => {
                self.failed.store(true, Ordering::Release);
                Err(FsError::new(ErrorCode::Eagain).with_syscall("session-drain-timeout"))
            }
        }
    }
    /// Retains a cancelled join for a later retry. Always attempts registry
    /// cleanup even when a worker failed. No backend drain deadline is imposed.
    pub async fn shutdown(&self) -> Result<()> {
        for registry in self.registries.iter() {
            registry.stop_admission();
        }
        self.cancel.cancel();
        let mut first = self.task.join().await.and_then(|joined| {
            joined
                .map_err(|_| failure())
                .and_then(|result| result)
                .err()
        });
        for registry in self.registries.iter() {
            if let Err(error) = registry.close().await {
                self.failed.store(true, Ordering::Release);
                if first.is_none() {
                    first = Some(error);
                }
            }
        }
        first.map_or(Ok(()), Err)
    }
}
impl Drop for SessionSupervisor {
    fn drop(&mut self) {
        for registry in self.registries.iter() {
            registry.stop_admission();
        }
        self.cancel.cancel();
        self.task.abort();
        // Registry handles stay owned by the caller for retryable cleanup.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_sdk::{Filesystem, SplitOptions};
    use std::time::Duration;
    #[tokio::test]
    async fn bounded_shutdown_retains_cleanup_for_retry() {
        let fs = Arc::new(
            Filesystem::split(SplitOptions::memory("supervisor-timeout", 4096))
                .await
                .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(fs.clone(), 2, 2, Duration::from_secs(60)).unwrap());
        let supervisor = SessionSupervisor::start(vec![registry.clone()]).unwrap();
        assert_eq!(
            supervisor
                .shutdown_with_timeout(Duration::ZERO)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Einval
        );
        let token = registry.create("alice").await.unwrap();
        let session = registry.lookup("alice", &token).unwrap();
        let handle = session.open("/file", "w+", 0o600).await.unwrap();
        // Hold the join to deterministically suspend shutdown at an await
        // boundary without relying on a slow disk or a sleeping backend.
        supervisor.task.hold.store(true, Ordering::Release);
        let error = supervisor
            .shutdown_with_timeout(Duration::from_millis(10))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Eagain);
        assert_eq!(error.syscall.as_deref(), Some("session-drain-timeout"));
        assert!(supervisor.failed());
        assert!(!supervisor.task.is_joined());
        assert_eq!(
            registry.create("alice").await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        supervisor.task.hold.store(false, Ordering::Release);
        supervisor
            .shutdown_with_timeout(Duration::from_secs(1))
            .await
            .unwrap();
        assert!(supervisor.task.is_joined());
        assert_eq!(
            session.stat(handle).await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        fs.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn aborted_supervisor_fails_closed_even_before_first_poll() {
        for poll_first in [false, true] {
            let fs = Arc::new(
                Filesystem::split(SplitOptions::memory("supervisor-abort", 4096))
                    .await
                    .unwrap(),
            );
            let first =
                Arc::new(VolumeSessions::new(fs.clone(), 2, 2, Duration::from_secs(60)).unwrap());
            let second =
                Arc::new(VolumeSessions::new(fs.clone(), 2, 2, Duration::from_secs(60)).unwrap());
            let token = first.create("alice").await.unwrap();
            let session = first.lookup("alice", &token).unwrap();
            let handle = session.open("/file", "w+", 0o600).await.unwrap();
            let supervisor = SessionSupervisor::start(vec![first.clone(), second.clone()]).unwrap();
            if poll_first {
                tokio::task::yield_now().await;
            }
            supervisor.task.abort();
            tokio::time::timeout(Duration::from_secs(1), async {
                while !supervisor.failed() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            for registry in [&first, &second] {
                assert_eq!(
                    registry.create("alice").await.unwrap_err().code,
                    ErrorCode::Ebadf
                );
            }
            assert_eq!(
                supervisor.shutdown().await.unwrap_err().code,
                ErrorCode::Eio
            );
            assert_eq!(
                session.stat(handle).await.unwrap_err().code,
                ErrorCode::Ebadf
            );
            supervisor.shutdown().await.unwrap();
            fs.shutdown().await.unwrap();
        }
    }
    #[tokio::test]
    async fn supervisor_drains_multiple_registries_before_filesystem_shutdown() {
        let fs = Arc::new(
            Filesystem::split(SplitOptions::memory("supervisor", 4096))
                .await
                .unwrap(),
        );
        let first =
            Arc::new(VolumeSessions::new(fs.clone(), 2, 2, Duration::from_secs(60)).unwrap());
        let second =
            Arc::new(VolumeSessions::new(fs.clone(), 2, 2, Duration::from_secs(60)).unwrap());
        assert!(SessionSupervisor::start(vec![first.clone(), first.clone()]).is_err());
        let supervisor = SessionSupervisor::start(vec![first.clone(), second.clone()]).unwrap();
        let token = first.create("alice").await.unwrap();
        let session = first.lookup("alice", &token).unwrap();
        let handle = session.open("/file", "w+", 0o600).await.unwrap();
        session.write(handle, b"data", Some(0)).await.unwrap();
        second.create("bob").await.unwrap();
        supervisor.shutdown().await.unwrap();
        assert!(!supervisor.failed());
        assert!(supervisor.task.is_joined());
        assert_eq!(
            first.create("alice").await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        assert_eq!(
            second.create("bob").await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        assert_eq!(
            session.stat(handle).await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        assert_eq!(fs.driver().stat("/file").await.unwrap().size, 4);
        supervisor.shutdown().await.unwrap();
        fs.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn dropped_supervisor_stops_admission_and_allows_explicit_cleanup() {
        let fs = Arc::new(
            Filesystem::split(SplitOptions::memory("supervisor-drop", 4096))
                .await
                .unwrap(),
        );
        let registry =
            Arc::new(VolumeSessions::new(fs.clone(), 1, 2, Duration::from_secs(60)).unwrap());
        let supervisor = SessionSupervisor::start(vec![registry.clone()]).unwrap();
        let token = registry.create("alice").await.unwrap();
        let session = registry.lookup("alice", &token).unwrap();
        let handle = session.open("/file", "w", 0o600).await.unwrap();
        drop(supervisor);
        assert_eq!(
            registry.create("alice").await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        registry.close().await.unwrap();
        assert_eq!(
            session.stat(handle).await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        fs.shutdown().await.unwrap();
    }
}
