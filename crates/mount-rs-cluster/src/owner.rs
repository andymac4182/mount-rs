//! Ownership-checked registration lifecycle for one SDK split-store volume.
use crate::{RedisRouteRegistry, RegistryError, VolumeRoute};
use mount_rs_sdk::Filesystem;
use std::{collections::hash_map::RandomState, hash::BuildHasher, sync::Arc, time::Duration};

// Equal jitter: retain a nonzero lower bound without synchronizing volumes
// after a shared outage. Configuration validation caps this input at 30s.
fn recovery_delay(ceiling: Duration, entropy: u64) -> Duration {
    let nanos = u64::try_from(ceiling.as_nanos()).expect("validated recovery delay");
    let lower = nanos.div_ceil(2);
    Duration::from_nanos(lower + entropy % (nanos - lower + 1))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationError {
    OwnershipLost,
    Conflict,
    /// An update exceeded its budget; the Redis write outcome may be unknown.
    Deadline,
    Registry(RegistryError),
}

fn terminal_recovery_error(error: RegistrationError, reconciled: bool) -> bool {
    matches!(
        error,
        RegistrationError::OwnershipLost
            | RegistrationError::Registry(
                RegistryError::InvalidInput | RegistryError::InvalidRecord
            )
    ) || (reconciled && error == RegistrationError::Conflict)
}

fn failure_status(error: RegistrationError, recovering: bool) -> RegistrationStatus {
    if recovering && !terminal_recovery_error(error, false) {
        RegistrationStatus::Recovering(error)
    } else {
        RegistrationStatus::Failed(error)
    }
}

/// Observed supervisor state, not authorization for a filesystem operation.
/// Admission must also call `check_destination`; a paused process or disconnected
/// observer must never treat the last `Ready` notification as a lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegistrationStatus {
    Starting,
    Ready,
    /// Routing is unavailable while the recovery supervisor reconciles.
    Recovering(RegistrationError),
    Stopped,
    Failed(RegistrationError),
}

struct StatusGuard(tokio::sync::watch::Sender<RegistrationStatus>);
impl Drop for StatusGuard {
    fn drop(&mut self) {
        // Aborting/dropping the supervisor cannot leave its watch state Ready.
        let running = matches!(
            *self.0.borrow(),
            RegistrationStatus::Ready
                | RegistrationStatus::Starting
                | RegistrationStatus::Recovering(_)
        );
        if running {
            self.0.send_replace(RegistrationStatus::Stopped);
        }
    }
}

/// Bounded exponential backoff for one outage. Each wait is independently
/// jittered between half and all of its exponential ceiling. Successful
/// reconciliation resets the attempt budget. No filesystem operation is retried.
#[derive(Clone, Copy, Debug)]
pub struct RegistrationRecovery {
    pub initial_delay: Duration,
    pub max_delay: Duration,
    pub max_attempts: usize,
}
impl RegistrationRecovery {
    pub(crate) fn validate(self) -> Result<(), RegistrationError> {
        if self.initial_delay.is_zero()
            || self.initial_delay > self.max_delay
            || self.max_delay > Duration::from_secs(30)
            || !(1..=100).contains(&self.max_attempts)
        {
            return Err(RegistrationError::Registry(RegistryError::InvalidInput));
        }
        Ok(())
    }
}

/// One registration is bound to one process identity and one TiDB generation.
/// A reacquired generation requires a new registration, not renewal of this one.
/// The server owns the filesystem lifecycle and must withdraw before shutdown.
/// On crash or cancelled cleanup, the bounded Redis TTL removes the route.
pub struct OwnedVolumeRegistration {
    filesystem: Arc<Filesystem>,
    registry: RedisRouteRegistry,
    route: VolumeRoute,
    ttl: Duration,
}
impl OwnedVolumeRegistration {
    /// Bind a prepared route to an already-open SDK volume. This does not
    /// advertise until `advertise` succeeds, and does not take over ownership.
    pub async fn new(
        filesystem: Arc<Filesystem>,
        registry: RedisRouteRegistry,
        route: VolumeRoute,
        ttl: Duration,
    ) -> Result<Self, RegistrationError> {
        route.encode().map_err(RegistrationError::Registry)?;
        if ttl < Duration::from_millis(20) || ttl > Duration::from_secs(30) {
            return Err(RegistrationError::Registry(RegistryError::InvalidInput));
        }
        let registration = Self {
            filesystem,
            registry,
            route,
            ttl,
        };
        registration.check_owner().await?;
        Ok(registration)
    }

    pub fn route(&self) -> &VolumeRoute {
        &self.route
    }

    /// Check exact destination identity and live owner permission. The caller
    /// must separately authenticate the peer and authorize the user's operation.
    pub async fn check_destination(&self, expected: &VolumeRoute) -> Result<(), RegistrationError> {
        if expected != &self.route {
            return Err(RegistrationError::Conflict);
        }
        self.check_owner().await
    }

    async fn check_owner(&self) -> Result<(), RegistrationError> {
        match self.filesystem.checked_owner_fence().await {
            Ok(fence) if fence == self.route.fence => Ok(()),
            _ => Err(RegistrationError::OwnershipLost),
        }
    }

    async fn update(&self, initial: bool) -> Result<(), RegistrationError> {
        if let Err(error) = self.check_owner().await {
            let _ = self.registry.remove(&self.route).await;
            return Err(error);
        }
        let result = if initial {
            self.registry.advertise(&self.route, self.ttl).await
        } else {
            self.registry.refresh(&self.route, self.ttl).await
        };
        // Do not report readiness if ownership failed during a slow Redis call.
        // A stale hint may briefly exist; destination dispatch always checks owner.
        if let Err(error) = self.check_owner().await {
            let _ = self.registry.remove(&self.route).await;
            return Err(error);
        }
        match result {
            Ok(true) => Ok(()),
            Ok(false) => Err(RegistrationError::Conflict),
            Err(error) => Err(RegistrationError::Registry(error)),
        }
    }

    pub async fn advertise(&self) -> Result<(), RegistrationError> {
        self.update(true).await
    }
    /// Does not silently re-advertise when the route expired or was replaced.
    pub async fn refresh(&self) -> Result<(), RegistrationError> {
        self.update(false).await
    }
    /// Reconcile an advisory route after loss or an uncertain Redis update.
    /// The caller controls backoff/retry and readiness; this never acquires a
    /// filesystem lease or retries a filesystem operation. An observed route
    /// belonging to another registration is left untouched.
    pub async fn reconcile(&self) -> Result<(), RegistrationError> {
        self.check_owner().await?;
        let current = self
            .registry
            .lookup(&self.route.volume)
            .await
            .map_err(RegistrationError::Registry)?;
        match current {
            None => self.advertise().await,
            Some(route) if route == self.route => self.refresh().await,
            Some(_) => Err(RegistrationError::Conflict),
        }
    }
    /// Opt-in routing recovery with bounded backoff and attempt count per outage.
    /// Conflicting registrations and ownership loss are terminal. Redis errors
    /// are reconciled by reading state; filesystem requests are never replayed.
    pub async fn run_with_recovery(
        &self,
        cancel: tokio_util::sync::CancellationToken,
        status: tokio::sync::watch::Sender<RegistrationStatus>,
        recovery: RegistrationRecovery,
    ) -> Result<(), RegistrationError> {
        let status = StatusGuard(status);
        if let Err(error) = recovery.validate() {
            status.0.send_replace(RegistrationStatus::Failed(error));
            return Err(error);
        }
        loop {
            let mut error = match self.run_once(cancel.clone(), &status.0, true).await {
                Ok(()) => return Ok(()),
                Err(error) => error,
            };
            if terminal_recovery_error(error, false) {
                status.0.send_replace(RegistrationStatus::Failed(error));
                return Err(error);
            }
            let mut delay = recovery.initial_delay;
            let mut recovered = false;
            // Fresh process-randomized hashing per outage avoids identical retry
            // schedules across volumes, servers and repeated outages. This is
            // load spreading, not a source of security tokens.
            let jitter = RandomState::new();
            for attempt in 0..recovery.max_attempts {
                let wait = recovery_delay(
                    delay,
                    jitter.hash_one((&self.route.volume, &self.route.boot, attempt)),
                );
                status.0.send_replace(RegistrationStatus::Recovering(error));
                let result = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    result = async {
                        tokio::time::sleep(wait).await;
                        tokio::time::timeout(self.ttl / 3, self.reconcile())
                            .await.unwrap_or(Err(RegistrationError::Deadline))
                    } => Some(result),
                };
                match result {
                    None => {
                        let outcome = self.withdraw().await.map(|_| ());
                        status.0.send_replace(match outcome {
                            Ok(()) => RegistrationStatus::Stopped,
                            Err(error) => RegistrationStatus::Failed(error),
                        });
                        return outcome;
                    }
                    Some(Ok(())) => {
                        recovered = true;
                        break;
                    }
                    Some(Err(next)) => {
                        error = next;
                        if terminal_recovery_error(error, true) {
                            status.0.send_replace(RegistrationStatus::Failed(error));
                            return Err(error);
                        }
                    }
                }
                delay = delay.saturating_mul(2).min(recovery.max_delay);
            }
            if !recovered {
                status.0.send_replace(RegistrationStatus::Failed(error));
                return Err(error);
            }
        }
    }

    /// Advertise, refresh every TTL/3, and withdraw on stop/failure. The caller
    /// owns/spawns this future and joins it before shutting down the filesystem.
    /// Updates have a TTL/3 budget. Unknown outcomes are not automatically replayed;
    /// reconciliation and any restart belong to the server supervisor.
    pub async fn run(
        &self,
        cancel: tokio_util::sync::CancellationToken,
        status: tokio::sync::watch::Sender<RegistrationStatus>,
    ) -> Result<(), RegistrationError> {
        let status = StatusGuard(status);
        self.run_once(cancel, &status.0, false).await
    }

    async fn run_once(
        &self,
        cancel: tokio_util::sync::CancellationToken,
        status: &tokio::sync::watch::Sender<RegistrationStatus>,
        recovering: bool,
    ) -> Result<(), RegistrationError> {
        status.send_replace(RegistrationStatus::Starting);
        let cadence = self.ttl / 3;
        let mut initial = true;
        let outcome = loop {
            let updated = tokio::select! {
                biased;
                _ = cancel.cancelled() => break Ok(()),
                result = tokio::time::timeout(cadence, self.update(initial)) => {
                    result.unwrap_or(Err(RegistrationError::Deadline))
                }
            };
            if let Err(error) = updated {
                break Err(error);
            }
            status.send_replace(RegistrationStatus::Ready);
            initial = false;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => break Ok(()),
                _ = tokio::time::sleep(cadence) => {}
            }
        };
        status.send_replace(match outcome {
            Ok(()) => RegistrationStatus::Stopped,
            Err(error) => failure_status(error, recovering),
        });
        // The registry timeout bounds cleanup. Cancellation or an ambiguous
        // update can leave a TTL-bounded hint, never serving authority.
        let cleanup = self.withdraw().await;
        match outcome {
            Err(error) => Err(error),
            Ok(()) => match cleanup {
                Ok(_) => Ok(()),
                Err(error) => {
                    status.send_replace(failure_status(error, recovering));
                    Err(error)
                }
            },
        }
    }

    /// Exact-record removal is safe after ownership loss and never touches a successor.
    pub async fn withdraw(&self) -> Result<bool, RegistrationError> {
        self.registry
            .remove(&self.route)
            .await
            .map_err(RegistrationError::Registry)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jitter_preserves_backoff_bounds_even_at_nanosecond_limits() {
        for ceiling in [
            Duration::from_nanos(1),
            Duration::from_nanos(3),
            Duration::from_millis(100),
            Duration::from_secs(30),
        ] {
            for entropy in [0, 1, 99, u64::MAX / 2, u64::MAX] {
                let wait = recovery_delay(ceiling, entropy);
                assert!(!wait.is_zero());
                assert!(wait >= ceiling / 2);
                assert!(wait <= ceiling);
            }
        }
        // Both ends of the permitted range remain reachable; the configured
        // maximum is a ceiling, not a minimum added on top of jitter.
        let ceiling = Duration::from_millis(100);
        assert_eq!(recovery_delay(ceiling, 0), Duration::from_millis(50));
        assert_eq!(recovery_delay(ceiling, 50_000_000), ceiling);
    }

    #[test]
    fn recovery_bounds_and_terminal_errors_are_explicit() {
        let valid = RegistrationRecovery {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            max_attempts: 5,
        };
        assert!(valid.validate().is_ok());
        for invalid in [
            RegistrationRecovery {
                initial_delay: Duration::ZERO,
                ..valid
            },
            RegistrationRecovery {
                initial_delay: Duration::from_secs(3),
                ..valid
            },
            RegistrationRecovery {
                max_delay: Duration::from_secs(31),
                ..valid
            },
            RegistrationRecovery {
                max_attempts: 0,
                ..valid
            },
            RegistrationRecovery {
                max_attempts: 101,
                ..valid
            },
        ] {
            assert!(invalid.validate().is_err());
        }
        assert!(!terminal_recovery_error(RegistrationError::Conflict, false));
        assert!(terminal_recovery_error(RegistrationError::Conflict, true));
        for reconciled in [false, true] {
            assert!(terminal_recovery_error(
                RegistrationError::OwnershipLost,
                reconciled
            ));
            assert!(terminal_recovery_error(
                RegistrationError::Registry(RegistryError::InvalidRecord),
                reconciled
            ));
            assert!(!terminal_recovery_error(
                RegistrationError::Deadline,
                reconciled
            ));
            assert!(!terminal_recovery_error(
                RegistrationError::Registry(RegistryError::WriteOutcomeUnknown),
                reconciled
            ));
        }
    }

    #[tokio::test]
    async fn aborting_supervisor_clears_observed_readiness() {
        for active in [
            RegistrationStatus::Ready,
            RegistrationStatus::Recovering(RegistrationError::Deadline),
        ] {
            let (tx, mut rx) = tokio::sync::watch::channel(RegistrationStatus::Starting);
            let task = tokio::spawn(async move {
                let status = StatusGuard(tx);
                status.0.send_replace(active);
                std::future::pending::<()>().await;
            });
            tokio::time::timeout(Duration::from_secs(1), async {
                while *rx.borrow_and_update() != active {
                    rx.changed().await.unwrap();
                }
            })
            .await
            .unwrap();
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            assert_eq!(*rx.borrow(), RegistrationStatus::Stopped);
        }
    }
}
