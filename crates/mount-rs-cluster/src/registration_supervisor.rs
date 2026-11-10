//! Caller-owned advisory registration workers. Readiness never grants I/O authority.
use crate::{OwnedVolumeRegistration, RegistrationRecovery, RegistrationStatus};
use mount_rs_core::{ErrorCode, FsError, Result};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::{sync::watch, task::JoinSet};
use tokio_util::sync::CancellationToken;

fn failure() -> FsError {
    FsError::new(ErrorCode::Eio).with_syscall("registration-supervisor")
}
struct ExitGuard {
    cancel: CancellationToken,
    failed: Arc<AtomicBool>,
    completed: bool,
}
impl Drop for ExitGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.failed.store(true, Ordering::Release);
            self.cancel.cancel();
        }
    }
}
/// Supervises one server's registrations. A terminal worker failure cancels
/// siblings and withdraws their advisory routes; it does not release TiDB leases.
/// Drain listeners and sessions before releasing the caller-owned filesystems.
pub struct RegistrationSupervisor {
    registrations: Arc<Vec<Arc<OwnedVolumeRegistration>>>,
    statuses: Vec<watch::Receiver<RegistrationStatus>>,
    cancel: CancellationToken,
    failed: Arc<AtomicBool>,
    task: crate::join_slot::JoinSlot<Result<()>>,
}
impl RegistrationSupervisor {
    pub fn start(
        registrations: Vec<Arc<OwnedVolumeRegistration>>,
        recovery: RegistrationRecovery,
    ) -> Result<Self> {
        let invalid = || FsError::new(ErrorCode::Einval).with_syscall("registration-supervisor");
        recovery.validate().map_err(|_| invalid())?;
        if registrations.is_empty() || registrations.len() > 4096 {
            return Err(invalid());
        }
        let mut volumes = std::collections::BTreeSet::new();
        if registrations
            .iter()
            .any(|r| !volumes.insert(r.route().volume.clone()))
        {
            return Err(invalid());
        }
        let runtime = tokio::runtime::Handle::try_current().map_err(|_| failure())?;
        let registrations = Arc::new(registrations);
        let cancel = CancellationToken::new();
        let failed = Arc::new(AtomicBool::new(false));
        let mut statuses = Vec::new();
        let mut pending = Vec::new();
        for registration in registrations.iter() {
            let (sender, receiver) = watch::channel(RegistrationStatus::Starting);
            statuses.push(receiver);
            pending.push((registration.clone(), sender));
        }
        let guard = ExitGuard {
            cancel: cancel.clone(),
            failed: failed.clone(),
            completed: false,
        };
        let stop = cancel.clone();
        let task = runtime.spawn(async move {
            let mut guard = guard;
            let mut workers = JoinSet::new();
            for (registration, sender) in pending {
                let stop = stop.clone();
                workers.spawn(async move {
                    registration.run_with_recovery(stop, sender, recovery).await
                });
            }
            let mut first = None;
            while let Some(outcome) = workers.join_next().await {
                if !matches!(outcome, Ok(Ok(()))) || !stop.is_cancelled() {
                    guard.failed.store(true, Ordering::Release);
                    stop.cancel();
                    first.get_or_insert_with(failure);
                }
            }
            guard.completed = true;
            first.map_or(Ok(()), Err)
        });
        Ok(Self {
            registrations,
            statuses,
            cancel,
            failed,
            task: crate::join_slot::JoinSlot::new(task),
        })
    }
    /// Wait for every exact registration before reporting server readiness.
    /// A startup failure withdraws any partial registrations before returning.
    pub async fn start_ready(
        registrations: Vec<Arc<OwnedVolumeRegistration>>,
        recovery: RegistrationRecovery,
        wait: std::time::Duration,
    ) -> Result<Self> {
        let deadline = tokio::time::Instant::now()
            .checked_add(wait)
            .filter(|_| !wait.is_zero())
            .ok_or_else(|| FsError::new(ErrorCode::Einval).with_syscall("registration-startup"))?;
        let supervisor = Self::start(registrations, recovery)?;
        let startup = async {
            loop {
                if supervisor.cancel.is_cancelled() || supervisor.failed() {
                    return Err(failure());
                }
                if supervisor.ready() {
                    return Ok(());
                }
                for status in &supervisor.statuses {
                    let mut rx = status.clone();
                    if *rx.borrow_and_update() != RegistrationStatus::Ready {
                        tokio::select! {
                            result = rx.changed() => result.map_err(|_| failure())?,
                            _ = supervisor.cancel.cancelled() => return Err(failure()),
                        }
                        break;
                    }
                }
            }
        };
        let result = tokio::time::timeout_at(deadline, startup)
            .await
            .unwrap_or_else(|_| {
                Err(FsError::new(ErrorCode::Eagain).with_syscall("registration-startup"))
            });
        if let Err(mut error) = result {
            if let Err(cleanup) = supervisor.shutdown().await {
                error = error.with_message(format!(
                    "registration startup failed; cleanup returned {}",
                    cleanup.code.as_str()
                ));
            }
            return Err(error);
        }
        Ok(supervisor)
    }

    /// Observation only. Closed status channels cannot retain a stale Ready.
    pub fn ready(&self) -> bool {
        !self.cancel.is_cancelled()
            && !self.failed()
            && self
                .statuses
                .iter()
                .all(|s| s.has_changed().is_ok() && *s.borrow() == RegistrationStatus::Ready)
    }
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    pub fn statuses(&self) -> Vec<RegistrationStatus> {
        self.statuses
            .iter()
            .map(|s| {
                if s.has_changed().is_err() && *s.borrow() == RegistrationStatus::Ready {
                    RegistrationStatus::Stopped
                } else {
                    *s.borrow()
                }
            })
            .collect()
    }
    /// Cancellation retains the task handle for retry. Attempt exact-record
    /// withdrawal on every registry even if joining a worker failed.
    pub async fn shutdown(&self) -> Result<()> {
        self.cancel.cancel();
        let mut first = self
            .task
            .join()
            .await
            .and_then(|joined| joined.map_err(|_| failure()).and_then(|r| r).err());
        for registration in self.registrations.iter() {
            if registration.withdraw().await.is_err() {
                self.failed.store(true, Ordering::Release);
                first.get_or_insert_with(failure);
            }
        }
        first.map_or(Ok(()), Err)
    }
}
impl Drop for RegistrationSupervisor {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
        // Interrupted withdrawal leaves a TTL-bounded hint, never authority.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedisRouteRegistry, VolumeRoute};
    use mount_rs_sdk::{Filesystem, SplitOptions};
    use std::time::Duration;

    async fn wait_ready(supervisor: &RegistrationSupervisor) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !supervisor.ready() {
                assert!(!supervisor.failed());
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    #[ignore = "requires isolated MOUNT_RS_REDIS_TEST_URL"]
    async fn registration_supervisor_recovers_drains_and_handles_abort() {
        let client =
            redis::Client::open(std::env::var("MOUNT_RS_REDIS_TEST_URL").unwrap()).unwrap();
        let registry = RedisRouteRegistry::with_manager(
            redis::aio::ConnectionManager::new(client).await.unwrap(),
            format!("supervisor-{}", std::process::id()),
            Duration::from_secs(1),
        )
        .unwrap();
        let recovery = RegistrationRecovery {
            initial_delay: Duration::from_millis(20),
            max_delay: Duration::from_millis(100),
            max_attempts: 10,
        };
        assert!(RegistrationSupervisor::start(Vec::new(), recovery).is_err());
        let mut filesystems = Vec::new();
        let mut registrations = Vec::new();
        for i in 0..2 {
            let fs = Arc::new(
                Filesystem::split(SplitOptions::memory(format!("supervisor-{i}"), 4096))
                    .await
                    .unwrap(),
            );
            let route = VolumeRoute {
                volume: format!("volume-{i}"),
                node: "node".into(),
                boot: "boot".into(),
                registration: format!("registration-{i}"),
                fence: fs.checked_owner_fence().await.unwrap(),
                address: "127.0.0.1:4443".parse().unwrap(),
                tls_server_name: "node.example".into(),
            };
            registrations.push(Arc::new(
                OwnedVolumeRegistration::new(
                    fs.clone(),
                    registry.clone(),
                    route,
                    Duration::from_secs(3),
                )
                .await
                .unwrap(),
            ));
            filesystems.push(fs);
        }
        assert!(
            RegistrationSupervisor::start(
                vec![registrations[0].clone(), registrations[0].clone()],
                recovery
            )
            .is_err()
        );
        let supervisor = RegistrationSupervisor::start_ready(
            registrations.clone(),
            recovery,
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        registry.remove(registrations[0].route()).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while registry.lookup("volume-0").await.unwrap().is_none() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        wait_ready(&supervisor).await;
        supervisor.shutdown().await.unwrap();
        assert!(!supervisor.ready());
        for registration in &registrations {
            assert!(
                registry
                    .lookup(&registration.route().volume)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        supervisor.shutdown().await.unwrap();
        let conflicted = RegistrationSupervisor::start(registrations.clone(), recovery).unwrap();
        wait_ready(&conflicted).await;
        // Inject a different advisory registration. This is not a TiDB takeover:
        // the test proves old-worker cleanup cannot delete a successor hint.
        let mut successor = registrations[0].route().clone();
        successor.fence += 1;
        successor.boot = "successor-boot".into();
        successor.registration = "successor-registration".into();
        assert!(
            registry
                .advertise(&successor, Duration::from_secs(30))
                .await
                .unwrap()
        );
        tokio::time::timeout(Duration::from_secs(5), async {
            while !conflicted.failed() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(!conflicted.ready());
        assert!(conflicted.shutdown().await.is_err());
        assert!(
            conflicted
                .statuses()
                .iter()
                .all(|status| *status != RegistrationStatus::Ready)
        );
        assert_eq!(
            registry.lookup("volume-0").await.unwrap(),
            Some(successor.clone())
        );
        assert!(registry.lookup("volume-1").await.unwrap().is_none());
        conflicted.shutdown().await.unwrap();
        assert_eq!(
            registry.lookup("volume-0").await.unwrap(),
            Some(successor.clone())
        );
        assert!(
            RegistrationSupervisor::start_ready(
                registrations.clone(),
                recovery,
                Duration::from_secs(3),
            )
            .await
            .is_err()
        );
        assert_eq!(
            registry.lookup("volume-0").await.unwrap(),
            Some(successor.clone())
        );
        assert!(registry.lookup("volume-1").await.unwrap().is_none());
        registry.remove(&successor).await.unwrap();
        let aborted = RegistrationSupervisor::start(registrations.clone(), recovery).unwrap();
        wait_ready(&aborted).await;
        aborted.task.abort();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !aborted.failed() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!aborted.ready());
        assert!(aborted.shutdown().await.is_err());
        aborted.shutdown().await.unwrap();
        for (fs, registration) in filesystems.iter().zip(&registrations) {
            assert!(
                registry
                    .lookup(&registration.route().volume)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                fs.checked_owner_fence().await.unwrap(),
                registration.route().fence
            );
            fs.shutdown().await.unwrap();
        }
    }
}
