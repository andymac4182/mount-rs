//! Actual SDK ownership bridge for immutable service construction choices.
//!
//! The factory retains construction journals before polling and delegates runtime
//! health, backing identity, eviction and shutdown to the actual SDK filesystem.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use mount_rs_core::construction::ConstructionObserver;
use mount_rs_core::storage::ConcurrentBackingId;
use mount_rs_core::{ErrorCode, FsDriver, FsError, Result};
use mount_rs_sdk::{ConstructionAttempt, ConstructionJournal, ConstructionSnapshot, Filesystem};

use crate::runtime_pool::{ManagedDrive, RuntimeFactory};

/// A prepared, immutable open choice owned before pool registration.
///
/// The constructor must register actual provider/authority resources with the
/// supplied observer before subsequent awaits and register the returned actual
/// filesystem before postconfiguration/driver wrapping. It must not start an
/// unowned cleanup or resolve a different static provider choice on reopen.
#[async_trait]
pub trait RuntimeConstructor: Send + Sync {
    async fn construct(&self, observer: &dyn ConstructionObserver) -> Result<ConstructedRuntime>;
}

/// The actual lifecycle owner and its selected public driver together.
///
/// `driver` must belong to `filesystem`; it can preserve a telemetry wrapper.
/// The wrapper is never used as the authority, health or close owner.
pub struct ConstructedRuntime {
    filesystem: Arc<Filesystem>,
    driver: Arc<dyn FsDriver>,
}

impl ConstructedRuntime {
    pub fn new(filesystem: Arc<Filesystem>, driver: Arc<dyn FsDriver>) -> Self {
        Self { filesystem, driver }
    }

    /// Move both owners together; callers retain the returned Arc before
    /// handing off the construction journal and before another await.
    pub fn into_managed(self) -> Arc<SdkManagedDrive> {
        Arc::new(SdkManagedDrive {
            filesystem: self.filesystem,
            driver: self.driver,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }
}

pub struct SdkManagedDrive {
    filesystem: Arc<Filesystem>,
    driver: Arc<dyn FsDriver>,
    // Acknowledged close only; absence of an owner never invents a receipt.
    closed: Arc<AtomicBool>,
}

#[async_trait]
impl ManagedDrive for SdkManagedDrive {
    fn driver(&self) -> Arc<dyn FsDriver> {
        Arc::clone(&self.driver)
    }

    fn failed(&self) -> bool {
        self.filesystem.failed()
    }

    fn eviction_allowed(&self) -> bool {
        self.filesystem.persistent_eviction_allowed()
    }

    fn concurrent_backing_id(&self) -> Option<ConcurrentBackingId> {
        self.filesystem.concurrent_backing_id()
    }

    async fn shutdown(&self) -> Result<()> {
        self.filesystem.shutdown().await?;
        self.closed.store(true, Ordering::Release);
        Ok(())
    }
}

struct RetainedAttempt {
    journal: ConstructionJournal,
    actual_owner: Mutex<Option<Arc<SdkManagedDrive>>>,
}

#[derive(Default)]
struct FactoryState {
    stopped: bool,
    opening: bool,
    failure: Option<FsError>,
    retained: Option<Arc<RetainedAttempt>>,
    previous_close: Option<Arc<AtomicBool>>,
}

/// One immutable Drive constructor with bounded partial-construction retention.
///
/// Retain the factory in the application lifecycle owner before registration.
/// A successful owner is handed to the runtime pool; a failed/abandoned attempt
/// remains here. `close` proves only construction cleanup, never pool/context
/// drain. The application must retain this factory if either proof fails.
pub struct SdkRuntimeFactory {
    constructor: Arc<dyn RuntimeConstructor>,
    state: Mutex<FactoryState>,
}

fn lock_state(state: &Mutex<FactoryState>) -> MutexGuard<'_, FactoryState> {
    match state.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.stopped = true;
            state
                .failure
                .get_or_insert_with(|| FsError::new(ErrorCode::Eio));
            state
        }
    }
}

impl SdkRuntimeFactory {
    pub fn new(constructor: Arc<dyn RuntimeConstructor>) -> Arc<Self> {
        Arc::new(Self {
            constructor,
            state: Mutex::new(FactoryState::default()),
        })
    }

    fn begin(&self) -> Result<OpenTicket<'_>> {
        let mut state = lock_state(&self.state);
        if let Some(error) = &state.failure {
            return Err(error.clone());
        }
        if state.stopped {
            return Err(FsError::new(ErrorCode::Ebadf));
        }
        if state.opening
            || state
                .previous_close
                .as_ref()
                .is_some_and(|closed| !closed.load(Ordering::Acquire))
        {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        let journal = ConstructionJournal::new();
        let attempt = journal.begin()?;
        let retained = Arc::new(RetainedAttempt {
            journal,
            actual_owner: Mutex::new(None),
        });
        // Journal and active attempt exist in the retained owner before the
        // first poll of the arbitrary constructor, including panic/cancel.
        state.retained = Some(retained.clone());
        state.opening = true;
        Ok(OpenTicket {
            factory: self,
            retained,
            attempt: Some(attempt),
            completed: false,
        })
    }

    /// Join one independently owned journal cleanup after pool shutdown.
    ///
    /// An active open returns EBUSY and remains retained; an unknown attempt
    /// refuses cleanup. Canceling this waiter cannot cancel journal cleanup.
    /// Repeated calls never retry failed authority/provider cleanup.
    pub async fn close(&self) -> Result<()> {
        let retained = {
            let mut state = lock_state(&self.state);
            state.stopped = true;
            if state.opening
                || state
                    .previous_close
                    .as_ref()
                    .is_some_and(|closed| !closed.load(Ordering::Acquire))
            {
                return Err(FsError::new(ErrorCode::Ebusy));
            }
            state.retained.clone()
        };
        match retained {
            None => Ok(()),
            Some(retained) => retained.journal.close().await,
        }
    }

    /// Cold-path construction evidence, not a filesystem or process receipt.
    pub fn construction_snapshot(&self) -> Option<ConstructionSnapshot> {
        let retained = { lock_state(&self.state).retained.clone() };
        retained.map(|retained| retained.journal.snapshot())
    }
}

struct OpenTicket<'a> {
    factory: &'a SdkRuntimeFactory,
    retained: Arc<RetainedAttempt>,
    attempt: Option<ConstructionAttempt>,
    completed: bool,
}

impl OpenTicket<'_> {
    fn failed(mut self, error: FsError) -> FsError {
        self.attempt.take().unwrap().fail();
        {
            let mut state = lock_state(&self.factory.state);
            state.failure.get_or_insert_with(|| error.clone());
            state.opening = false;
        }
        self.completed = true;
        error
    }

    fn handoff(mut self, actual: Arc<SdkManagedDrive>) -> Result<Arc<dyn ManagedDrive>> {
        // Retain the real lifecycle owner synchronously before handoff. A
        // failed transition must not drop a constructor's successful owner.
        *self.retained.actual_owner.lock().unwrap() = Some(actual.clone());
        if let Err(error) = self.attempt.take().unwrap().handoff() {
            return Err(self.failed_without_attempt(error));
        }
        let retired = {
            let mut state = lock_state(&self.factory.state);
            state.previous_close = Some(actual.closed.clone());
            state.opening = false;
            state.retained.take()
        };
        self.completed = true;
        // Any destructor runs outside the factory mutex. `actual` remains the
        // returned owner; the journal has acknowledged its resource transfer.
        drop(retired);
        Ok(actual)
    }

    fn failed_without_attempt(mut self, error: FsError) -> FsError {
        {
            let mut state = lock_state(&self.factory.state);
            state.failure.get_or_insert_with(|| error.clone());
            state.opening = false;
        }
        self.completed = true;
        error
    }
}

impl Drop for OpenTicket<'_> {
    fn drop(&mut self) {
        if !self.completed {
            // Seal uncertainty before exposing the now-inactive admission.
            drop(self.attempt.take());
            let mut state = lock_state(&self.factory.state);
            state
                .failure
                .get_or_insert_with(|| FsError::new(ErrorCode::Eio));
            state.opening = false;
        }
    }
}

#[async_trait]
impl RuntimeFactory for SdkRuntimeFactory {
    async fn open(&self) -> Result<Arc<dyn ManagedDrive>> {
        let ticket = self.begin()?;
        match self.constructor.construct(&ticket.retained.journal).await {
            Ok(constructed) => ticket.handoff(constructed.into_managed()),
            Err(error) => {
                let journal = ticket.retained.journal.clone();
                let primary = ticket.failed(error);
                // Keep the constructor's original typed result. The journal
                // records cleanup independently; close() re-joins its cached
                // result, including an authority/provider cleanup failure.
                let _ = journal.close().await;
                Err(primary)
            }
        }
    }
}
