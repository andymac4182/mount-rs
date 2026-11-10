//! A spawned task's handle that any number of callers may try to join, one
//! at a time, with no lock: a turn gate orders the callers and the handle
//! sits in an atomic slot between attempts.
use arc_swap::ArcSwapOption;
use mount_rs_core::turn_gate::TurnGate;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::task::{AbortHandle, JoinError, JoinHandle};

pub(crate) struct JoinSlot<T> {
    pub(crate) turn: TurnGate,
    /// Touched only by the caller holding `turn`, so the slot's reference is
    /// always the only one.
    task: ArcSwapOption<JoinHandle<T>>,
    abort: AbortHandle,
    joined: AtomicBool,
}

/// Puts an unfinished join back, so a cancelled caller leaves it for a retry.
struct Restore<'a, T> {
    slot: &'a ArcSwapOption<JoinHandle<T>>,
    task: Option<JoinHandle<T>>,
}

impl<T> Drop for Restore<'_, T> {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            self.slot.store(Some(Arc::new(task)));
        }
    }
}

impl<T> JoinSlot<T> {
    pub(crate) fn new(task: JoinHandle<T>) -> Self {
        Self {
            turn: TurnGate::new(),
            abort: task.abort_handle(),
            task: ArcSwapOption::new(Some(Arc::new(task))),
            joined: AtomicBool::new(false),
        }
    }

    /// Waits for the task. `None` once an earlier call has joined it.
    pub(crate) async fn join(&self) -> Option<Result<T, JoinError>> {
        let _turn = self.turn.lock().await;
        let task = self.task.swap(None).and_then(Arc::into_inner)?;
        let mut restore = Restore {
            slot: &self.task,
            task: Some(task),
        };
        let result = restore.task.as_mut()?.await;
        restore.task = None;
        self.joined.store(true, Ordering::Release);
        Some(result)
    }

    pub(crate) fn abort(&self) {
        self.abort.abort();
    }

    #[cfg(test)]
    pub(crate) fn is_joined(&self) -> bool {
        self.joined.load(Ordering::Acquire)
    }
}
