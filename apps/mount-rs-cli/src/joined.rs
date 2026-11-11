//! A background task whose result is published, not held behind a lock.
//!
//! The task writes its result to a watch channel as its last act. A closer
//! waits on the channel and then takes the result with one atomic exchange,
//! so:
//!
//! * the result is reported to exactly one closer, and a later or concurrent
//!   closer waits for the task to end and is answered `Ok`;
//! * a closer that is cancelled while waiting has taken nothing, so the next
//!   closer still waits for the task and still receives its result;
//! * the join handle is never moved out, so dropping the owner can always
//!   abort a task that is still running.

use std::future::Future;
use tokio::sync::watch;
use tokio::task::JoinHandle;

enum Outcome<E> {
    Running,
    Finished(Result<(), E>),
    Reported,
}

pub(crate) struct JoinedTask<E> {
    done: watch::Sender<Outcome<E>>,
    worker: JoinHandle<()>,
}

/// Publishes the task's end even when it ends without a result (a panic or
/// an abort). The owner holds a sender too, so the channel never closes by
/// itself and a waiter would otherwise wait for ever.
struct Publish<E> {
    done: watch::Sender<Outcome<E>>,
    lost: fn() -> E,
}

impl<E> Drop for Publish<E> {
    fn drop(&mut self) {
        let lost = self.lost;
        self.done.send_if_modified(|outcome| {
            if matches!(outcome, Outcome::Running) {
                *outcome = Outcome::Finished(Err(lost()));
                true
            } else {
                false
            }
        });
    }
}

impl<E: Send + Sync + 'static> JoinedTask<E> {
    /// Spawn `task`. `lost` makes the error reported when the task ends
    /// without returning.
    pub(crate) fn spawn(
        task: impl Future<Output = Result<(), E>> + Send + 'static,
        lost: fn() -> E,
    ) -> Self {
        let (done, _) = watch::channel(Outcome::Running);
        let publish = Publish {
            done: done.clone(),
            lost,
        };
        let worker = tokio::spawn(async move {
            // Declared before the task is polled, so on a panic the task's
            // own state is dropped before its end is published.
            let publish = publish;
            let result = task.await;
            publish.done.send_replace(Outcome::Finished(result));
        });
        Self { done, worker }
    }

    /// Wait for the task to end and report its result once.
    pub(crate) async fn join(&self) -> Result<(), E> {
        let mut done = self.done.subscribe();
        // `self` holds a sender, so this cannot fail.
        let _ = done
            .wait_for(|outcome| !matches!(outcome, Outcome::Running))
            .await;
        match self.done.send_replace(Outcome::Reported) {
            Outcome::Finished(result) => result,
            Outcome::Running | Outcome::Reported => Ok(()),
        }
    }

    /// Whether the task has ended, reported or not.
    pub(crate) fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }

    /// Stop the task without waiting for it.
    pub(crate) fn abort(&self) {
        self.worker.abort();
    }

    #[cfg(test)]
    pub(crate) fn reported(&self) -> bool {
        matches!(*self.done.borrow(), Outcome::Reported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Notify;

    fn lost() -> &'static str {
        "lost"
    }

    /// Many closers at once: the error is reported to exactly one, and
    /// every other closer waits for the task and is answered `Ok`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_result_is_reported_to_exactly_one_of_many_closers() {
        for _ in 0..200 {
            let release = Arc::new(Notify::new());
            let gate = release.clone();
            let task = Arc::new(JoinedTask::spawn(
                async move {
                    gate.notified().await;
                    Err("failed")
                },
                lost,
            ));
            let closers: Vec<_> = (0..8)
                .map(|_| {
                    let task = task.clone();
                    tokio::spawn(async move { task.join().await })
                })
                .collect();
            tokio::task::yield_now().await;
            assert!(!task.is_finished());
            release.notify_one();
            let mut errors = 0;
            for closer in closers {
                if let Err(error) = closer.await.unwrap() {
                    assert_eq!(error, "failed");
                    errors += 1;
                }
            }
            assert_eq!(errors, 1, "the error is reported once");
            assert!(task.reported());
            assert_eq!(task.join().await, Ok(()), "a later closer gets Ok");
        }
    }

    /// A closer cancelled while it waits has taken nothing: the task keeps
    /// running, and the next closer waits for it and receives its error.
    #[tokio::test]
    async fn a_cancelled_closer_loses_nothing() {
        let release = Arc::new(Notify::new());
        let gate = release.clone();
        let task = JoinedTask::spawn(
            async move {
                gate.notified().await;
                Err("failed")
            },
            lost,
        );
        for _ in 0..3 {
            assert!(
                tokio::time::timeout(Duration::from_millis(5), task.join())
                    .await
                    .is_err(),
                "the task has not ended"
            );
        }
        assert!(!task.is_finished() && !task.reported());
        release.notify_one();
        assert_eq!(task.join().await, Err("failed"));
        assert_eq!(task.join().await, Ok(()));
    }

    /// A task that panics or is aborted still wakes its closers, with the
    /// `lost` error, once.
    #[tokio::test]
    async fn a_task_that_ends_without_a_result_reports_the_lost_error() {
        let panicked = JoinedTask::<&'static str>::spawn(async { panic!("worker panic") }, lost);
        assert_eq!(panicked.join().await, Err("lost"));
        assert_eq!(panicked.join().await, Ok(()));

        let aborted = JoinedTask::<&'static str>::spawn(std::future::pending(), lost);
        aborted.abort();
        assert_eq!(aborted.join().await, Err("lost"));
        assert_eq!(aborted.join().await, Ok(()));
    }
}
