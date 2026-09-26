use serde_json::{Value, json};
use std::time::Duration;
pub struct WorkloadClock {
    start: tokio::time::Instant,
    active_end: Option<tokio::time::Instant>,
}
impl WorkloadClock {
    pub fn start() -> Self {
        Self {
            start: tokio::time::Instant::now(),
            active_end: None,
        }
    }
    pub fn active_finished(&mut self) {
        self.active_end = Some(tokio::time::Instant::now());
    }
    pub fn finish(&self, cycles: usize) -> Value {
        let now = tokio::time::Instant::now();
        let elapsed = now.duration_since(self.start).as_secs_f64();
        let active_end = self.active_end.unwrap_or(now);
        let active = active_end.duration_since(self.start).as_secs_f64();
        let idle = now.duration_since(active_end).as_secs_f64();
        json!({"active_elapsed_seconds":active,"idle_liveness_elapsed_seconds":idle,"phase_elapsed_seconds":elapsed,"cycles_per_second":if active > 0.0 {Some(cycles as f64/active)} else {None}})
    }
}
#[tokio::test(start_paused = true)]
async fn prereview_red_blocked_liveness_does_not_change_active_denominator() {
    let mut clock = WorkloadClock::start();
    tokio::time::advance(Duration::from_secs(30)).await;
    clock.active_finished();
    tokio::time::sleep(Duration::from_secs(10)).await;
    let result = clock.finish(300);
    assert_eq!(result["active_elapsed_seconds"], 30.0);
    assert_eq!(result["cycles_per_second"], 10.0);
    assert_eq!(result["idle_liveness_elapsed_seconds"], 10.0);
    assert_eq!(result["phase_elapsed_seconds"], 40.0);
}
