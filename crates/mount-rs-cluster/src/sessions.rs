//! Bounded owner-local sessions. The transport supplies an authenticated client
//! identity, schedules `expire_idle`, and joins `close` before SDK shutdown.
//! This is not an authentication implementation or a network protocol.
use crate::OwnedHandleSession;
use mount_rs_core::{ErrorCode, FsError, Result};
use mount_rs_sdk::Filesystem;
use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

/// Random session reference. Never log the wire representation. Possession
/// alone is insufficient: lookup also requires the bound authenticated identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SessionToken([u8; 32]);
impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}
impl SessionToken {
    pub fn to_wire(&self) -> String {
        self.0.iter().map(|byte| format!("{byte:02x}")).collect()
    }
    pub fn from_wire(value: &str) -> Result<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(failure(ErrorCode::Einval));
        }
        let mut bytes = [0; 32];
        for (index, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| failure(ErrorCode::Einval))?;
        }
        Ok(Self(bytes))
    }
}
struct Record {
    _budget: Option<tokio::sync::OwnedSemaphorePermit>,
    principal: String,
    session: Arc<OwnedHandleSession>,
    created: Instant,
    touched_ns: AtomicU64,
    closing: bool,
}
impl Record {
    fn expired_at(&self, now_ns: u128, timeout: Duration) -> bool {
        // Fail closed if the nanosecond counter ever overflows (after ~584
        // years). Never wrap or round timestamps at the expiry boundary.
        now_ns > u64::MAX as u128
            || now_ns.saturating_sub(self.touched_ns.load(Ordering::Relaxed) as u128)
                >= timeout.as_nanos()
    }
}
/// Share one instance across a server's volume registries to bound admitted
/// and closing sessions. Does not bound request buffers or provider caches.
#[derive(Clone)]
pub struct SessionBudget(Arc<tokio::sync::Semaphore>);
impl SessionBudget {
    pub fn new(max_sessions: usize) -> Result<Self> {
        if !(1..=65536).contains(&max_sessions) {
            return Err(failure(ErrorCode::Einval));
        }
        Ok(Self(Arc::new(tokio::sync::Semaphore::new(max_sessions))))
    }
}
struct State {
    closing: bool,
    sessions: BTreeMap<SessionToken, Record>,
}
/// Per-volume bound; a multi-volume server must additionally enforce a global
/// session/resource budget. Failed or cancelled cleanup keeps its registry slot
/// and remains denied to clients until a later cleanup attempt succeeds.
pub struct VolumeSessions {
    filesystem: Arc<Filesystem>,
    max_sessions: usize,
    handles_per_session: usize,
    idle_timeout: Duration,
    budget: Option<SessionBudget>,
    state: RwLock<State>,
}
fn failure(code: ErrorCode) -> FsError {
    FsError::new(code).with_syscall("session")
}

struct ExpiryGuard<'a>(&'a VolumeSessions);
impl Drop for ExpiryGuard<'_> {
    fn drop(&mut self) {
        // An aborted/failed worker must not leave a registry admitting new
        // sessions indefinitely without an expiry owner. Handles stay retained
        // for the server's explicit drain retry.
        self.0.stop_admission();
    }
}
impl VolumeSessions {
    /// Inactivity limit advertised to authenticated clients for keepalive scheduling.
    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    pub fn new(
        filesystem: Arc<Filesystem>,
        max_sessions: usize,
        handles_per_session: usize,
        idle_timeout: Duration,
    ) -> Result<Self> {
        if !(1..=4096).contains(&max_sessions)
            || !(1..=4096).contains(&handles_per_session)
            || !(Duration::from_secs(1)..=Duration::from_secs(3600)).contains(&idle_timeout)
        {
            return Err(failure(ErrorCode::Einval));
        }
        Ok(Self {
            filesystem,
            max_sessions,
            handles_per_session,
            idle_timeout,
            budget: None,
            state: RwLock::new(State {
                closing: false,
                sessions: BTreeMap::new(),
            }),
        })
    }
    pub fn new_with_budget(
        filesystem: Arc<Filesystem>,
        max_sessions: usize,
        handles_per_session: usize,
        idle_timeout: Duration,
        budget: SessionBudget,
    ) -> Result<Self> {
        let mut registry = Self::new(filesystem, max_sessions, handles_per_session, idle_timeout)?;
        registry.budget = Some(budget);
        Ok(registry)
    }
    pub async fn create(&self, authenticated_principal: &str) -> Result<SessionToken> {
        if authenticated_principal.is_empty() || authenticated_principal.len() > 256 {
            return Err(failure(ErrorCode::Einval));
        }
        // Reserve before any asynchronous owner check; cancellation/error drops
        // the reservation. Once inserted, cleanup retains it through close.
        let permit = self
            .budget
            .as_ref()
            .map(|budget| {
                budget
                    .0
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| failure(ErrorCode::Enfile))
            })
            .transpose()?;
        let session = Arc::new(
            OwnedHandleSession::new(self.filesystem.clone(), self.handles_per_session).await?,
        );
        let mut bytes = [0; 32];
        getrandom::fill(&mut bytes).map_err(|_| failure(ErrorCode::Eio))?;
        let token = SessionToken(bytes);
        let principal = authenticated_principal.to_owned();
        let mut state = self.state.write().unwrap();
        if state.closing {
            return Err(failure(ErrorCode::Ebadf));
        }
        if state.sessions.len() >= self.max_sessions {
            return Err(failure(ErrorCode::Enfile));
        }
        if state.sessions.contains_key(&token) {
            return Err(failure(ErrorCode::Eagain));
        }
        state.sessions.insert(
            token.clone(),
            Record {
                _budget: permit,
                principal,
                session,
                created: Instant::now(),
                touched_ns: AtomicU64::new(0),
                closing: false,
            },
        );
        Ok(token)
    }
    /// Trusted transport use only: do not retain the returned Arc across client
    /// requests. Every new request must pass through this authorization/expiry
    /// check. Idle expiry measures request admission, not request completion.
    pub fn lookup(
        &self,
        authenticated_principal: &str,
        token: &SessionToken,
    ) -> Result<Arc<OwnedHandleSession>> {
        let state = self.state.read().unwrap();
        if state.closing {
            return Err(failure(ErrorCode::Ebadf));
        }
        let record = state
            .sessions
            .get(token)
            .filter(|r| r.principal == authenticated_principal)
            .ok_or_else(|| failure(ErrorCode::Eacces))?;
        let now = record.created.elapsed().as_nanos();
        if record.closing || record.expired_at(now, self.idle_timeout) {
            return Err(failure(ErrorCode::Estale));
        }
        // Shared registry admission excludes expiry/close until publication.
        // Concurrent admissions can finish in reverse timestamp order; max
        // prevents an older admission from shortening the idle lifetime.
        record.touched_ns.fetch_max(now as u64, Ordering::Relaxed);
        Ok(record.session.clone())
    }
    async fn cleanup(&self, token: &SessionToken, session: Arc<OwnedHandleSession>) -> Result<()> {
        session.close().await?;
        let retired = {
            let mut state = self.state.write().unwrap();
            if state
                .sessions
                .get(token)
                .is_some_and(|record| Arc::ptr_eq(&record.session, &session))
            {
                state.sessions.remove(token)
            } else {
                None
            }
        };
        // Reclaim the principal/session reference and release shared capacity
        // outside registry admission. Permit release can wake another task.
        drop(retired);
        Ok(())
    }
    pub async fn close_session(
        &self,
        authenticated_principal: &str,
        token: &SessionToken,
    ) -> Result<()> {
        let session = {
            let mut state = self.state.write().unwrap();
            let record = state
                .sessions
                .get_mut(token)
                .filter(|r| r.principal == authenticated_principal)
                .ok_or_else(|| failure(ErrorCode::Eacces))?;
            record.closing = true;
            record.session.clone()
        };
        self.cleanup(token, session).await
    }
    /// One bounded sweep; caller controls scheduling and timeout. Cancellation
    /// cannot discard a session that still owns handles.
    pub async fn expire_idle(&self) -> Result<usize> {
        let expired: Vec<_> = {
            let mut state = self.state.write().unwrap();
            state
                .sessions
                .iter_mut()
                .filter_map(|(token, record)| {
                    if record.closing
                        || record.expired_at(record.created.elapsed().as_nanos(), self.idle_timeout)
                    {
                        record.closing = true;
                        Some((token.clone(), record.session.clone()))
                    } else {
                        None
                    }
                })
                .collect()
        };
        let mut completed = 0;
        let mut error = None;
        for (token, session) in expired {
            match self.cleanup(&token, session).await {
                Ok(()) => completed += 1,
                Err(failure) => {
                    error.get_or_insert(failure);
                }
            }
        }
        error.map_or(Ok(completed), Err)
    }
    pub(crate) fn stop_admission(&self) {
        let mut state = self.state.write().unwrap();
        state.closing = true;
        for record in state.sessions.values_mut() {
            record.closing = true;
        }
    }

    /// Caller-owned expiry task. Cancellation drains all sessions before
    /// returning; failures/abort stop admission and retain cleanup state.
    /// The caller must retain the registry and join this task before shutting
    /// down the filesystem, retrying `close` if the task fails or is aborted.
    /// A stuck backend close can delay completion; server shutdown policy owns
    /// its timeout and must not mistake an aborted drain for successful cleanup.
    pub async fn run_expiry(
        &self,
        cancel: tokio_util::sync::CancellationToken,
        cadence: Duration,
    ) -> Result<()> {
        if cadence.is_zero() || cadence > self.idle_timeout / 2 {
            return Err(failure(ErrorCode::Einval));
        }
        let _guard = ExpiryGuard(self);
        let mut interval = tokio::time::interval(cadence);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return self.close().await,
                result = async {
                    interval.tick().await;
                    self.expire_idle().await
                } => { result?; }
            }
        }
    }

    pub async fn close(&self) -> Result<()> {
        self.stop_admission();
        self.expire_idle().await.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn fixture(limit: usize) -> (Arc<Filesystem>, VolumeSessions) {
        let fs = Arc::new(
            Filesystem::split(mount_rs_sdk::SplitOptions::memory("sessions", 4096))
                .await
                .unwrap(),
        );
        let sessions = VolumeSessions::new(fs.clone(), limit, 2, Duration::from_secs(60)).unwrap();
        (fs, sessions)
    }
    #[tokio::test]
    async fn lookup_overlaps_shared_admission_and_timestamp_never_regresses() {
        let (fs, registry) = fixture(2).await;
        let token = registry.create("alice").await.unwrap();
        {
            let guard = registry.state.read().unwrap();
            let record = guard.sessions.get(&token).unwrap();
            // Explicit boundary and overflow checks do not depend on wall-clock sleeps.
            record.touched_ns.store(100, Ordering::Relaxed);
            record.touched_ns.fetch_max(50, Ordering::Relaxed);
            assert_eq!(record.touched_ns.load(Ordering::Relaxed), 100);
            assert!(!record.expired_at(109, Duration::from_nanos(10)));
            assert!(record.expired_at(110, Duration::from_nanos(10)));
            assert!(record.expired_at(u64::MAX as u128 + 1, Duration::from_secs(60)));
            let (send, receive) = std::sync::mpsc::channel();
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    send.send(registry.lookup("alice", &token).is_ok()).unwrap();
                });
                let admitted = receive.recv_timeout(Duration::from_secs(2));
                // Release before joining even on failure, so regression cannot hang.
                drop(guard);
                assert!(admitted.unwrap());
            });
        }
        registry.stop_admission();
        assert!(matches!(registry.lookup("alice", &token), Err(e) if e.code == ErrorCode::Ebadf));
        registry.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn expiry_worker_reclaims_idle_and_cancellation_drains_active() {
        let (fs, registry) = fixture(2).await;
        let registry = Arc::new(registry);
        let expired = registry.create("idle").await.unwrap();
        let active = registry.create("active").await.unwrap();
        let session = registry.lookup("active", &active).unwrap();
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        registry
            .state
            .write()
            .unwrap()
            .sessions
            .get_mut(&expired)
            .unwrap()
            .created = Instant::now() - Duration::from_secs(61);
        let stop = tokio_util::sync::CancellationToken::new();
        let worker_stop = stop.clone();
        let worker = registry.clone();
        let task = tokio::spawn(async move {
            worker
                .run_expiry(worker_stop, Duration::from_millis(10))
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry
                .state
                .write()
                .unwrap()
                .sessions
                .contains_key(&expired)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(registry.lookup("active", &active).is_ok());
        stop.cancel();
        task.await.unwrap().unwrap();
        assert_eq!(session.stat(id).await.unwrap_err().code, ErrorCode::Ebadf);
        assert!(registry.state.write().unwrap().sessions.is_empty());
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn aborting_expiry_stops_admission_and_retains_cleanup_state() {
        let (fs, registry) = fixture(2).await;
        let registry = Arc::new(registry);
        let expired = registry.create("idle").await.unwrap();
        let active = registry.create("active").await.unwrap();
        registry
            .state
            .write()
            .unwrap()
            .sessions
            .get_mut(&expired)
            .unwrap()
            .created = Instant::now() - Duration::from_secs(61);
        let worker = registry.clone();
        let task = tokio::spawn(async move {
            worker
                .run_expiry(
                    tokio_util::sync::CancellationToken::new(),
                    Duration::from_millis(10),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(1), async {
            while registry
                .state
                .write()
                .unwrap()
                .sessions
                .contains_key(&expired)
            {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(registry.lookup("active", &active).is_err());
        assert_eq!(
            registry.create("new").await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        assert!(
            registry
                .state
                .write()
                .unwrap()
                .sessions
                .contains_key(&active)
        );
        registry.close().await.unwrap();
        assert!(registry.state.write().unwrap().sessions.is_empty());
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn cleanup_releases_budget_after_unlocking_registry() {
        use std::{
            future::Future,
            sync::atomic::{AtomicBool, Ordering},
            task::{Context, Wake, Waker},
        };
        struct Probe {
            registry: Arc<VolumeSessions>,
            unlocked: AtomicBool,
            woke: AtomicBool,
        }
        impl Wake for Probe {
            fn wake(self: Arc<Self>) {
                self.unlocked
                    .store(self.registry.state.try_write().is_ok(), Ordering::SeqCst);
                self.woke.store(true, Ordering::SeqCst);
            }
        }
        let (fs, _) = fixture(2).await;
        let budget = SessionBudget::new(1).unwrap();
        let registry = Arc::new(
            VolumeSessions::new_with_budget(
                fs.clone(),
                2,
                2,
                Duration::from_secs(60),
                budget.clone(),
            )
            .unwrap(),
        );
        let token = registry.create("client").await.unwrap();
        let probe = Arc::new(Probe {
            registry: registry.clone(),
            unlocked: AtomicBool::new(false),
            woke: AtomicBool::new(false),
        });
        let waker = Waker::from(probe.clone());
        let mut waiting = Box::pin(budget.0.clone().acquire_owned());
        assert!(
            waiting
                .as_mut()
                .poll(&mut Context::from_waker(&waker))
                .is_pending()
        );
        registry.close_session("client", &token).await.unwrap();
        assert!(probe.woke.load(Ordering::SeqCst));
        assert!(
            probe.unlocked.load(Ordering::SeqCst),
            "permit released while registry locked"
        );
        drop(waiting.await.unwrap());
        assert_eq!(budget.0.available_permits(), 1);
        registry.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shared_budget_bounds_distinct_volumes_and_releases_after_cleanup() {
        let (first_fs, _) = fixture(2).await;
        let (second_fs, _) = fixture(2).await;
        let budget = SessionBudget::new(1).unwrap();
        let first = VolumeSessions::new_with_budget(
            first_fs.clone(),
            2,
            2,
            Duration::from_secs(60),
            budget.clone(),
        )
        .unwrap();
        let second = VolumeSessions::new_with_budget(
            second_fs.clone(),
            2,
            2,
            Duration::from_secs(60),
            budget.clone(),
        )
        .unwrap();
        let token = first.create("first").await.unwrap();
        assert_eq!(
            second.create("second").await.unwrap_err().code,
            ErrorCode::Enfile
        );
        // Marked-for-close sessions retain their reservation until drain.
        first.stop_admission();
        assert_eq!(
            second.create("second").await.unwrap_err().code,
            ErrorCode::Enfile
        );
        first.close_session("first", &token).await.unwrap();
        let token = second.create("second").await.unwrap();
        second
            .state
            .write()
            .unwrap()
            .sessions
            .get_mut(&token)
            .unwrap()
            .created = Instant::now() - Duration::from_secs(61);
        assert_eq!(second.expire_idle().await.unwrap(), 1);
        assert_eq!(budget.0.available_permits(), 1);
        // Rejection after reserving capacity must not leak that permit.
        first.close().await.unwrap();
        assert_eq!(
            first.create("closed").await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        assert_eq!(budget.0.available_permits(), 1);
        second.close().await.unwrap();
        first_fs.shutdown().await.unwrap();
        second_fs.shutdown().await.unwrap();
    }

    #[test]
    fn token_wire_is_canonical_and_debug_redacted() {
        let token = SessionToken([0xab; 32]);
        assert_eq!(SessionToken::from_wire(&token.to_wire()).unwrap(), token);
        assert!(!format!("{token:?}").contains("ab"));
        for invalid in ["", "ab", &"AB".repeat(32), &"é".repeat(32)] {
            assert!(SessionToken::from_wire(invalid).is_err());
        }
    }
    #[tokio::test]
    async fn identity_binding_capacity_and_registry_isolation() {
        let (fs, registry) = fixture(1).await;
        let token = registry.create("alice").await.unwrap();
        assert!(registry.lookup("bob", &token).is_err());
        assert_eq!(
            registry
                .close_session("bob", &token)
                .await
                .unwrap_err()
                .code,
            ErrorCode::Eacces
        );
        assert_eq!(
            registry.create("bob").await.unwrap_err().code,
            ErrorCode::Enfile
        );
        let other = VolumeSessions::new(fs.clone(), 1, 2, Duration::from_secs(60)).unwrap();
        assert!(other.lookup("alice", &token).is_err());
        let session = registry.lookup("alice", &token).unwrap();
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        registry.close_session("alice", &token).await.unwrap();
        assert_eq!(session.stat(id).await.unwrap_err().code, ErrorCode::Ebadf);
        assert!(registry.lookup("alice", &token).is_err());
        let next = registry.create("bob").await.unwrap();
        assert_ne!(next, token);
        registry.close().await.unwrap();
        assert_eq!(
            registry.create("alice").await.unwrap_err().code,
            ErrorCode::Ebadf
        );
        other.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }
    #[tokio::test]
    async fn idle_expiry_denies_access_and_reclaims_handles() {
        let (fs, registry) = fixture(2).await;
        let token = registry.create("alice").await.unwrap();
        let active = registry.create("bob").await.unwrap();
        let session = registry.lookup("alice", &token).unwrap();
        let id = session.open("/file", "w+", 0o600).await.unwrap();
        registry
            .state
            .write()
            .unwrap()
            .sessions
            .get_mut(&token)
            .unwrap()
            .created = Instant::now() - Duration::from_secs(61);
        assert!(registry.lookup("alice", &token).is_err());
        assert!(registry.lookup("bob", &active).is_ok());
        assert_eq!(registry.expire_idle().await.unwrap(), 1);
        assert_eq!(session.stat(id).await.unwrap_err().code, ErrorCode::Ebadf);
        assert_eq!(registry.expire_idle().await.unwrap(), 0);
        registry.close().await.unwrap();
        fs.shutdown().await.unwrap();
    }
}
