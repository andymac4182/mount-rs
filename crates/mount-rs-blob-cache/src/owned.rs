use crate::{Result, error};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Wake, Waker},
};
use tokio::{sync::Notify, task::JoinHandle};

struct NotifyWake(Arc<Notify>);
impl Wake for NotifyWake {
    fn wake(self: Arc<Self>) {
        self.0.notify_waiters();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.notify_waiters();
    }
}
pub(crate) fn notify_waker(changed: &Arc<Notify>) -> Waker {
    Waker::from(Arc::new(NotifyWake(changed.clone())))
}

struct TaskState {
    task: Option<JoinHandle<Result<()>>>,
    result: Option<Result<()>>,
    uncertain: bool,
}

/// The actual join remains owned when a waiter is canceled. A stable wake
/// target also lets another waiter resume it after the first has disappeared.
pub(crate) struct OwnedTask {
    state: Mutex<TaskState>,
    changed: Arc<Notify>,
    wake: Waker,
}
impl OwnedTask {
    pub(crate) fn new(task: JoinHandle<Result<()>>) -> Arc<Self> {
        let changed = Arc::new(Notify::new());
        let wake = notify_waker(&changed);
        Self::with_signal(task, changed, wake)
    }
    pub(crate) fn with_signal(
        task: JoinHandle<Result<()>>,
        changed: Arc<Notify>,
        wake: Waker,
    ) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(TaskState {
                task: Some(task),
                result: None,
                uncertain: false,
            }),
            changed,
            wake,
        })
    }
    pub(crate) fn poll_result(&self) -> Poll<Result<()>> {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => {
                let mut state = poisoned.into_inner();
                state.uncertain = true;
                state
            }
        };
        if let Some(result) = &state.result {
            return Poll::Ready(result.clone());
        }
        let Some(task) = &mut state.task else {
            return Poll::Ready(Err(error()));
        };
        let joined = match Pin::new(task).poll(&mut Context::from_waker(&self.wake)) {
            Poll::Pending => return Poll::Pending,
            Poll::Ready(result) => result,
        };
        let result = if state.uncertain {
            Err(error())
        } else {
            joined.unwrap_or_else(|_| Err(error()))
        };
        state.task = None;
        state.result = Some(result.clone());
        drop(state);
        self.changed.notify_waiters();
        Poll::Ready(result)
    }
    pub(crate) async fn join(&self) -> Result<()> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Poll::Ready(result) = self.poll_result() {
                return result;
            }
            changed.await;
        }
    }
}
