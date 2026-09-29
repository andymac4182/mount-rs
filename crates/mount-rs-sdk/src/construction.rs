//! Retained resources for one explicit filesystem construction attempt.

use std::sync::{Arc, Mutex, MutexGuard};

use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_core::{ErrorCode, FsError, Result};
use tokio::sync::watch;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Fresh,
    Opening,
    Failed,
    Closing,
    Closed,
    HandedOff,
}

struct State {
    phase: Phase,
    uncertain: bool,
    late_registration: bool,
    resources: Vec<Arc<dyn ConstructionResource>>,
    completion: Option<watch::Sender<Option<Result<()>>>>,
}

/// A cold-path snapshot of retained construction ownership.
///
/// It describes this attempt only, and is not a provider or process drain proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstructionSnapshot {
    pub retained_resources: usize,
    pub opening: bool,
    pub closing: bool,
    pub cleanup_complete: bool,
    pub handed_off: bool,
    pub uncertain: bool,
}

/// Owns resources registered during exactly one filesystem open attempt.
///
/// Retain this object in the application's lifecycle owner before polling the
/// journal-aware constructor. A dropped constructor leaves this journal
/// uncertain; an error does not authorize another attempt with the same journal.
/// Cleanup runs independently of its waiters, stops at the first failed authority
/// or provider, and retains every registered owner if cleanup is unproven.
/// A successful handoff releases only the journal's references after the actual
/// filesystem has assumed ownership. This object adds no filesystem I/O wrapper.
#[derive(Clone)]
pub struct ConstructionJournal {
    inner: Arc<Mutex<State>>,
}

impl Default for ConstructionJournal {
    fn default() -> Self {
        Self::new()
    }
}

fn uncertain() -> FsError {
    FsError::new(ErrorCode::Eio)
}

fn lock(inner: &Mutex<State>) -> MutexGuard<'_, State> {
    match inner.lock() {
        Ok(state) => state,
        Err(poisoned) => {
            let mut state = poisoned.into_inner();
            state.uncertain = true;
            state
        }
    }
}

impl ConstructionJournal {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(State {
                phase: Phase::Fresh,
                uncertain: false,
                late_registration: false,
                resources: Vec::new(),
                completion: None,
            })),
        }
    }

    pub fn snapshot(&self) -> ConstructionSnapshot {
        let state = lock(&self.inner);
        ConstructionSnapshot {
            retained_resources: state.resources.len(),
            opening: state.phase == Phase::Opening,
            closing: state.phase == Phase::Closing,
            cleanup_complete: state.phase == Phase::Closed && !state.uncertain,
            handed_off: state.phase == Phase::HandedOff && !state.uncertain,
            uncertain: state.uncertain,
        }
    }

    /// Begin one attempt before invoking a journal-aware constructor.
    pub fn begin(&self) -> Result<ConstructionAttempt> {
        let mut state = lock(&self.inner);
        if state.phase != Phase::Fresh || state.uncertain {
            return Err(FsError::new(ErrorCode::Ebusy));
        }
        state.phase = Phase::Opening;
        Ok(ConstructionAttempt {
            journal: self.clone(),
            completed: false,
        })
    }

    /// Join this attempt's one owned cleanup, without canceling it with a waiter.
    ///
    /// An active or abandoned constructor cannot be safely closed. Failed
    /// resource cleanup is terminal for this journal; repeated callers receive
    /// the same error and do not start another cleanup. The first poll requires
    /// a Tokio runtime when resources need asynchronous cleanup.
    pub async fn close(&self) -> Result<()> {
        let (mut receiver, start) = {
            let mut state = lock(&self.inner);
            if state.phase == Phase::Closing
                || (state.phase == Phase::Closed
                    && state.completion.is_some()
                    && !state.late_registration)
            {
                (state.completion.as_ref().unwrap().subscribe(), None)
            } else if state.uncertain {
                return Err(uncertain());
            } else {
                match state.phase {
                    Phase::Fresh | Phase::Closed if state.completion.is_none() => {
                        state.phase = Phase::Closed;
                        return Ok(());
                    }
                    Phase::Opening => return Err(FsError::new(ErrorCode::Ebusy)),
                    Phase::HandedOff => return Err(FsError::new(ErrorCode::Estale)),
                    Phase::Closed => (state.completion.as_ref().unwrap().subscribe(), None),
                    Phase::Failed if state.resources.is_empty() => {
                        state.phase = Phase::Closed;
                        return Ok(());
                    }
                    Phase::Failed => {
                        let runtime =
                            tokio::runtime::Handle::try_current().map_err(|_| uncertain())?;
                        let (sender, receiver) = watch::channel(None);
                        state.completion = Some(sender.clone());
                        state.phase = Phase::Closing;
                        let resources = state.resources.clone();
                        (receiver, Some((runtime, sender, resources)))
                    }
                    Phase::Closing | Phase::Fresh => unreachable!("classified construction phase"),
                }
            }
        };
        if let Some((runtime, sender, resources)) = start {
            let completion = CleanupCompletion {
                journal: self.clone(),
                sender,
                completed: false,
            };
            let worker_journal = self.clone();
            let worker = runtime.spawn(async move {
                // Authority is registered after providers. A failed authority
                // must retain the providers required to reconcile it.
                for resource in resources.iter().rev() {
                    if lock(&worker_journal.inner).uncertain {
                        return Err(uncertain());
                    }
                    resource.close().await?;
                    if lock(&worker_journal.inner).uncertain {
                        return Err(uncertain());
                    }
                }
                Ok(())
            });
            runtime.spawn(async move {
                let result = worker.await.unwrap_or_else(|_| Err(uncertain()));
                completion.finish(result);
            });
        }
        loop {
            let result = { receiver.borrow().clone() };
            if let Some(result) = result {
                if result.is_ok() && lock(&self.inner).uncertain {
                    return Err(uncertain());
                }
                return result;
            }
            receiver.changed().await.map_err(|_| uncertain())?;
        }
    }
}

impl ConstructionObserver for ConstructionJournal {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        let mut state = lock(&self.inner);
        // Even a misbehaving constructor's late owner must remain retained.
        // It can never make an old cleanup receipt cover that new resource.
        if state.phase != Phase::Opening {
            state.uncertain = true;
            state.late_registration = true;
        }
        state.resources.push(resource);
    }
}

/// An active open attempt. Dropping it without an acknowledged result leaves
/// the journal uncertain and prevents dependent provider cleanup.
#[must_use = "retain the attempt until construction has failed or ownership has transferred"]
pub struct ConstructionAttempt {
    journal: ConstructionJournal,
    completed: bool,
}

impl ConstructionAttempt {
    /// Seal registration after the constructor has returned an acknowledged error.
    /// An uncertain resource must reject its own cleanup and retain its authority.
    pub fn fail(mut self) {
        let mut state = lock(&self.journal.inner);
        if state.phase != Phase::Opening {
            state.uncertain = true;
        } else {
            state.phase = Phase::Failed;
        }
        self.completed = true;
    }

    /// Release journal references after the returned owner has assumed every
    /// resource. The caller must retain that actual owner across this transition.
    pub fn handoff(mut self) -> Result<()> {
        let resources = {
            let mut state = lock(&self.journal.inner);
            if state.phase != Phase::Opening || state.uncertain {
                return Err(uncertain());
            }
            state.phase = Phase::HandedOff;
            std::mem::take(&mut state.resources)
        };
        // Provider destructors must not run under the journal mutex. The
        // acknowledged filesystem owns these resources before this transition.
        drop(resources);
        if lock(&self.journal.inner).uncertain {
            Err(uncertain())
        } else {
            self.completed = true;
            Ok(())
        }
    }
}

impl Drop for ConstructionAttempt {
    fn drop(&mut self) {
        if !self.completed {
            let mut state = lock(&self.journal.inner);
            state.uncertain = true;
            if state.phase == Phase::Opening {
                state.phase = Phase::Failed;
            }
        }
    }
}

struct CleanupCompletion {
    journal: ConstructionJournal,
    sender: watch::Sender<Option<Result<()>>>,
    completed: bool,
}

impl CleanupCompletion {
    fn finish(mut self, mut result: Result<()>) {
        let released = {
            let mut state = lock(&self.journal.inner);
            if state.phase != Phase::Closing || state.uncertain {
                result = Err(uncertain());
            }
            if result.is_ok() {
                std::mem::take(&mut state.resources)
            } else {
                state.uncertain = true;
                Vec::new()
            }
        };
        // All acknowledged resource owners drop before publishing completion.
        drop(released);
        {
            let mut state = lock(&self.journal.inner);
            if state.phase != Phase::Closing || state.late_registration {
                result = Err(uncertain());
            }
            state.phase = Phase::Closed;
            if result.is_err() {
                state.uncertain = true;
            }
        }
        self.sender.send_replace(Some(result));
        self.completed = true;
    }
}

impl Drop for CleanupCompletion {
    fn drop(&mut self) {
        if !self.completed {
            {
                let mut state = lock(&self.journal.inner);
                state.phase = Phase::Closed;
                state.uncertain = true;
            }
            self.sender.send_replace(Some(Err(uncertain())));
        }
    }
}
