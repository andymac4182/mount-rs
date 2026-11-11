//! A spawned task's result that any number of callers may wait for, with no
//! lock and no queue: a forwarder task awaits the task once, parks its result
//! in an atomic slot and flips a watch flag. The first caller to swap the
//! slot empty gets the result. A caller cancelled while waiting holds
//! nothing, so nothing is lost and nothing needs putting back.
use arc_swap::ArcSwapOption;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::watch;
use tokio::task::{AbortHandle, JoinError, JoinHandle};

pub(crate) struct JoinSlot<T> {
    /// True once `result` has been filled.
    done: watch::Receiver<bool>,
    result: Arc<ArcSwapOption<Result<T, JoinError>>>,
    abort: AbortHandle,
    joined: AtomicBool,
    /// While set, `join` does not take the result: lets a test suspend a
    /// joiner at an await point without a slow backend.
    #[cfg(test)]
    pub(crate) hold: AtomicBool,
}

impl<T: Send + Sync + 'static> JoinSlot<T> {
    /// Must be called inside a Tokio runtime, as the task itself was.
    pub(crate) fn new(task: JoinHandle<T>) -> Self {
        let abort = task.abort_handle();
        let result = Arc::new(ArcSwapOption::empty());
        let (finished, done) = watch::channel(false);
        let slot = Arc::clone(&result);
        tokio::spawn(async move {
            let outcome = task.await;
            slot.store(Some(Arc::new(outcome)));
            let _ = finished.send(true);
        });
        Self {
            done,
            result,
            abort,
            joined: AtomicBool::new(false),
            #[cfg(test)]
            hold: AtomicBool::new(false),
        }
    }
}

impl<T> JoinSlot<T> {
    /// Waits for the task. `None` once an earlier call has joined it.
    pub(crate) async fn join(&self) -> Option<Result<T, JoinError>> {
        let mut done = self.done.clone();
        // An error means the forwarder was dropped with its runtime; the
        // slot is then empty unless it finished first.
        let _ = done.wait_for(|done| *done).await;
        #[cfg(test)]
        while self.hold.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
        let result = self.result.swap(None).and_then(Arc::into_inner)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_joiners_share_one_result() {
        for round in 0..200_u32 {
            let (release, wait) = tokio::sync::oneshot::channel::<()>();
            let slot = Arc::new(JoinSlot::new(tokio::spawn(async move {
                let _ = wait.await;
                round
            })));
            let joiners: Vec<_> = (0..8)
                .map(|_| {
                    let slot = slot.clone();
                    tokio::spawn(async move { slot.join().await })
                })
                .collect();
            if round % 2 == 0 {
                tokio::task::yield_now().await;
            }
            release.send(()).unwrap();
            let mut results = Vec::new();
            for joiner in joiners {
                if let Some(result) = joiner.await.unwrap() {
                    results.push(result.unwrap());
                }
            }
            assert_eq!(results, [round]);
            assert!(slot.is_joined());
            assert!(slot.join().await.is_none());
        }
    }

    #[tokio::test]
    async fn a_cancelled_joiner_leaves_the_result_for_the_next() {
        let (release, wait) = tokio::sync::oneshot::channel::<()>();
        let slot = JoinSlot::new(tokio::spawn(async move {
            let _ = wait.await;
            7
        }));
        {
            // Polled while the task is still running, then dropped.
            let mut cancelled = pin!(slot.join());
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                cancelled.as_mut().poll(&mut context),
                Poll::Pending
            ));
        }
        release.send(()).unwrap();
        {
            // Cancelled again after the result is ready but before taking it.
            slot.hold.store(true, Ordering::Release);
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(20), slot.join())
                    .await
                    .is_err()
            );
            slot.hold.store(false, Ordering::Release);
        }
        assert!(!slot.is_joined());
        assert_eq!(slot.join().await.unwrap().unwrap(), 7);
        assert!(slot.is_joined());
        assert!(slot.join().await.is_none());
    }

    #[tokio::test]
    async fn an_aborted_task_reports_cancellation_once() {
        let slot = JoinSlot::new(tokio::spawn(std::future::pending::<()>()));
        slot.abort();
        assert!(slot.join().await.unwrap().unwrap_err().is_cancelled());
        assert!(slot.join().await.is_none());
    }
}
