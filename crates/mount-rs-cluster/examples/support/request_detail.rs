//! Diagnostic intervals local to the benchmark caller and ingress.
use std::{
    future::{Future, poll_fn},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    task::{Context, Wake, Waker},
};
use tokio::time::Instant;

pub const POOL_STAGES: [&str; 20] = [
    "dial_map_lock",
    "dial_entry_setup",
    "destination_lock",
    "pool_map_lock",
    "pool_validation",
    "reuse",
    "replacement_capacity",
    "quic_handshake",
    "h3_setup",
    "checkout_finish",
    "lease_release",
    "dial_map_pending",
    "dial_map_notified",
    "destination_pending",
    "destination_notified",
    "pool_map_pending",
    "pool_map_notified",
    "stream_permit",
    "sender_clone",
    "h3_dispatch",
];
#[derive(Default)]
pub struct Pool([AtomicU64; 20]);
tokio::task_local! { pub static POOL: Arc<Pool>; }
impl Pool {
    pub fn snapshot(&self) -> serde_json::Value {
        POOL_STAGES
            .iter()
            .zip(&self.0)
            .filter_map(|(name, value)| {
                let ns = value.load(Ordering::Relaxed);
                ns.checked_sub(1).map(|ns| (name.to_string(), ns.into()))
            })
            .collect::<serde_json::Map<_, _>>()
            .into()
    }
}

#[derive(Default)]
pub struct PoolEvent {
    stage: Option<usize>,
    ns: Option<u64>,
}
impl tracing::field::Visit for PoolEvent {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "stage" {
            self.stage = POOL_STAGES.iter().position(|s| *s == value);
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "ns" {
            self.ns = Some(value);
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}
impl PoolEvent {
    pub fn capture(event: &tracing::Event<'_>) {
        let _ = POOL.try_with(|pool| {
            let mut visitor = PoolEvent::default();
            event.record(&mut visitor);
            if let (Some(stage), Some(ns)) = (visitor.stage, visitor.ns) {
                pool.0[stage].store(ns.saturating_add(1), Ordering::Relaxed);
            }
        });
    }
}

struct Notice {
    start: Instant,
    first: AtomicU64,
    parent: Waker,
}
impl Wake for Notice {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let ns = self.start.elapsed().as_nanos() as u64;
        let _ = self
            .first
            .compare_exchange(u64::MAX, ns, Ordering::Relaxed, Ordering::Relaxed);
        self.parent.wake_by_ref();
    }
}

#[derive(Default)]
pub struct ClientPolls {
    polls: Vec<(u64, u64, u64)>,
}
impl ClientPolls {
    pub async fn observe<F: Future>(&mut self, start: Instant, future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        let mut notice: Option<Arc<Notice>> = None;
        poll_fn(|cx| {
            let begin = start.elapsed().as_nanos() as u64;
            let previous_wake = notice
                .as_ref()
                .map_or(u64::MAX, |n| n.first.swap(u64::MAX, Ordering::Relaxed));
            let n = notice.get_or_insert_with(|| {
                Arc::new(Notice {
                    start,
                    first: AtomicU64::new(u64::MAX),
                    parent: cx.waker().clone(),
                })
            });
            // dispatch is pinned inside a single caller task.
            debug_assert!(n.parent.will_wake(cx.waker()));
            let waker = Waker::from(n.clone());
            let result = future.as_mut().poll(&mut Context::from_waker(&waker));
            self.polls
                .push((begin, start.elapsed().as_nanos() as u64, previous_wake));
            result
        })
        .await
    }
    fn window(&self, start: u64, end: u64) -> (u64, u64, u64) {
        let overlap = |a: u64, b: u64| end.min(b).saturating_sub(start.max(a));
        let (mut active, mut ready, mut prev_end) = (0, 0, 0);
        for &(begin, finish, wake) in &self.polls {
            active += overlap(begin, finish);
            ready += overlap(wake.max(prev_end), begin);
            prev_end = finish;
        }
        (active, ready, end - start - active - ready)
    }
    pub fn before(&self, cutoff: u64) -> serde_json::Value {
        let (active, ready, other) = self.window(0, cutoff);
        serde_json::json!({"client_send_poll_ns":active,"client_notified_wait_ns":ready,"other_pre_service_ns":other})
    }
    pub fn response_headers(&self, start: u64, end: u64) -> serde_json::Value {
        let (active, ready, other) = self.window(start, end);
        serde_json::json!({"client_send_poll_ns":active,"client_notified_wait_ns":ready,"other_response_ns":other})
    }
}

#[derive(Default)]
struct H2Stage {
    reactor: AtomicU64,
    at: AtomicU64,
    ready: AtomicU64,
    age: AtomicU64,
}
pub struct H2Clock {
    origin: Instant,
    stamps: Vec<[H2Stage; 9]>,
}
static H2_CLOCK: std::sync::OnceLock<arc_swap::ArcSwapOption<H2Clock>> = std::sync::OnceLock::new();
impl H2Clock {
    pub fn start(count: usize) -> Arc<Self> {
        let clock = Arc::new(Self {
            origin: Instant::now(),
            stamps: (0..count).map(|_| Default::default()).collect(),
        });
        H2_CLOCK
            .get_or_init(Default::default)
            .store(Some(clock.clone()));
        clock
    }
    pub fn stop() {
        if let Some(clock) = H2_CLOCK.get() {
            clock.store(None);
        }
    }
    pub fn read_ready_monotonic_ns(&self, index: usize) -> Option<u64> {
        let value = self.stamps[index][7].reactor.load(Ordering::Relaxed);
        (value != 0).then_some(value)
    }
    pub fn driver_intervals(&self, index: usize) -> [(Option<u64>, Option<u64>); 4] {
        std::array::from_fn(|i| {
            let s = &self.stamps[index][i];
            (
                s.ready.load(Ordering::Relaxed).checked_sub(1),
                s.age.load(Ordering::Relaxed).checked_sub(1),
            )
        })
    }
    pub fn offsets(&self, index: usize, start: Instant) -> [Option<u64>; 9] {
        std::array::from_fn(|i| {
            let ns = self.stamps.get(index)?[i]
                .at
                .load(Ordering::Relaxed)
                .checked_sub(1)?;
            (self.origin + std::time::Duration::from_nanos(ns))
                .checked_duration_since(start)
                .map(|d| d.as_nanos() as u64)
        })
    }
}

#[derive(Default)]
struct H2Event {
    reactor: Option<u64>,
    elapsed_ns: Option<u64>,
    ready: Option<u64>,
    age: Option<u64>,
    sample: Option<usize>,
    stage: Option<usize>,
}
impl tracing::field::Visit for H2Event {
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "reactor_ns" => self.reactor = Some(value),
            "elapsed_ns" => self.elapsed_ns = Some(value),
            "ready_ns" => self.ready = Some(value),
            "poll_age_ns" => self.age = Some(value),
            "sample" => self.sample = usize::try_from(value).ok(),
            "stage" => self.stage = usize::try_from(value).ok(),
            _ => {}
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}
pub fn h2_event(event: &tracing::Event<'_>) {
    if let Some(clock) = H2_CLOCK.get().and_then(|s| s.load_full()) {
        let now = clock.origin.elapsed().as_nanos() as u64;
        let mut v = H2Event::default();
        event.record(&mut v);
        if let Some(slot) = v
            .sample
            .and_then(|s| clock.stamps.get(s))
            .and_then(|a| v.stage.and_then(|i| a.get(i)))
        {
            if let Some(ns) = v.reactor {
                slot.reactor.store(ns, Ordering::Relaxed);
            }
            slot.at.store(
                now.saturating_sub(v.elapsed_ns.unwrap_or(0)) + 1,
                Ordering::Relaxed,
            );
            if let (Some(ready), Some(age)) = (v.ready, v.age) {
                slot.ready.store(ready + 1, Ordering::Relaxed);
                slot.age.store(age + 1, Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod poll_tests {
    use super::ClientPolls;

    #[test]
    fn clips_polls_and_first_wakes_to_response_window() {
        let trace = ClientPolls {
            polls: vec![(1, 4, u64::MAX), (12, 15, 7), (20, 24, 13)],
        };
        assert_eq!(trace.window(5, 22), (5, 10, 2));
        assert_eq!(trace.window(8, 11), (0, 3, 0));
        assert_eq!(trace.window(25, 30), (0, 0, 5));
    }
}
