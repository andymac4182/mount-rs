//! Measure a traced future's pending intervals without changing its poll order.
use std::{
    future::{Future, poll_fn},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Wake, Waker},
    time::Instant,
};

struct Notice {
    origin: Instant,
    first: AtomicU64,
    parent: Waker,
}
impl Wake for Notice {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let ns = self.origin.elapsed().as_nanos() as u64;
        let _ = self
            .first
            .compare_exchange(u64::MAX, ns, Ordering::Relaxed, Ordering::Relaxed);
        self.parent.wake_by_ref();
    }
}
#[derive(Default)]
pub struct WaitTiming {
    pub pending_ns: u64,
    pub notified_ns: u64,
}
pub async fn observe<F: Future>(future: F) -> (F::Output, WaitTiming) {
    let origin = Instant::now();
    let mut future = std::pin::pin!(future);
    let mut notice: Option<Arc<Notice>> = None;
    let mut previous_end = None;
    let mut timing = WaitTiming::default();
    let output = poll_fn(|cx| {
        let begin = origin.elapsed().as_nanos() as u64;
        if let (Some(end), Some(previous)) = (previous_end, &notice) {
            let wake = previous.first.swap(u64::MAX, Ordering::Relaxed);
            timing.pending_ns += begin - end;
            timing.notified_ns += begin.saturating_sub(wake.max(end));
        }
        if !notice
            .as_ref()
            .is_some_and(|n| n.parent.will_wake(cx.waker()))
        {
            notice = Some(Arc::new(Notice {
                origin,
                first: AtomicU64::new(u64::MAX),
                parent: cx.waker().clone(),
            }));
        }
        let waker = Waker::from(notice.as_ref().unwrap().clone());
        let result = future.as_mut().poll(&mut Context::from_waker(&waker));
        previous_end = Some(origin.elapsed().as_nanos() as u64);
        result
    })
    .await;
    (output, timing)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_pending_without_a_wake_from_a_notified_wait() {
        for notify in [false, true] {
            let mut first = true;
            let lock = poll_fn(|cx| {
                if std::mem::take(&mut first) {
                    if notify {
                        cx.waker().wake_by_ref();
                    }
                    std::task::Poll::Pending
                } else {
                    std::task::Poll::Ready(())
                }
            });
            let mut measured = std::pin::pin!(observe(lock));
            let mut cx = Context::from_waker(Waker::noop());
            assert!(measured.as_mut().poll(&mut cx).is_pending());
            std::thread::sleep(std::time::Duration::from_millis(1));
            let std::task::Poll::Ready(((), t)) = measured.as_mut().poll(&mut cx) else {
                panic!("second poll should acquire the simulated lock")
            };
            assert!(t.pending_ns > 0);
            assert!(t.notified_ns <= t.pending_ns);
            assert_eq!(t.notified_ns > 0, notify);
        }
    }
}
