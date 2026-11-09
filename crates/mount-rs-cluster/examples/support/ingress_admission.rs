//! Benchmark-only admission through response headers, not a production policy.
use std::{sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

// Only this benchmark ingress emits this diagnostic header. Do not infer an
// admission rejection from a generic backend 503 or an unrecognized value.
pub const ERROR_HEADER: &str = "x-mount-audit-admission-error";

pub fn response_error(
    status: http::StatusCode,
    headers: &http::HeaderMap,
    fallback: &'static str,
) -> &'static str {
    if status != http::StatusCode::SERVICE_UNAVAILABLE {
        return fallback;
    }
    match headers
        .get(ERROR_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        Some("ingress queue full") => "ingress queue full",
        Some("ingress admission timeout") => "ingress admission timeout",
        Some("ingress admission closed") => "ingress admission closed",
        _ => fallback,
    }
}

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

struct Waiting<'a>(&'a AtomicUsize);
impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Relaxed);
    }
}

pub struct Admission {
    waiting: AtomicUsize,
    peak: AtomicUsize,
    queued_writes: AtomicU64,
    wait_ns: AtomicU64,
    put_only: bool,
    active: Arc<Semaphore>,
    capacity: Arc<Semaphore>,
}
pub struct Permit {
    _active: OwnedSemaphorePermit,
    _capacity: OwnedSemaphorePermit,
}
impl Admission {
    pub fn new(active: usize, queued: usize) -> Result<Self, &'static str> {
        if !(1..=1024).contains(&active) || queued > 10000 {
            return Err("ingress admission requires active=1..1024 and queued=0..10000");
        }
        Ok(Self {
            waiting: AtomicUsize::new(0),
            peak: AtomicUsize::new(0),
            queued_writes: AtomicU64::new(0),
            wait_ns: AtomicU64::new(0),
            put_only: false,
            active: Arc::new(Semaphore::new(active)),
            capacity: Arc::new(Semaphore::new(active + queued)),
        })
    }
    // The benchmark controller drains these counters between completed batches.
    pub fn take_stats(&self) -> serde_json::Value {
        serde_json::json!({"waiting":self.waiting.load(Relaxed),"peak_waiting":self.peak.swap(0, Relaxed),
            "queued_writes":self.queued_writes.swap(0, Relaxed),"wait_ns":self.wait_ns.swap(0, Relaxed)})
    }
    pub fn put_only(mut self) -> Self {
        self.put_only = true;
        self
    }
    pub async fn acquire(&self, method: &http::Method) -> Result<Option<Permit>, &'static str> {
        if (self.put_only && *method != http::Method::PUT)
            || *method == http::Method::GET
            || *method == http::Method::HEAD
        {
            return Ok(None);
        }
        let capacity = self
            .capacity
            .clone()
            .try_acquire_owned()
            .map_err(|_| "ingress queue full")?;
        let mut acquire = std::pin::pin!(self.active.clone().acquire_owned());
        let mut waiting = None;
        let observed = std::future::poll_fn(|cx| {
            let poll = std::future::Future::poll(acquire.as_mut(), cx);
            if poll.is_pending() && waiting.is_none() {
                let depth = self.waiting.fetch_add(1, Relaxed) + 1;
                self.peak.fetch_max(depth, Relaxed);
                waiting = Some((Waiting(&self.waiting), std::time::Instant::now()));
            }
            poll
        });
        let active = tokio::time::timeout(Duration::from_secs(5), observed)
            .await
            .map_err(|_| "ingress admission timeout")?
            .map_err(|_| "ingress admission closed")?;
        if let Some((_guard, started)) = waiting {
            self.queued_writes.fetch_add(1, Relaxed);
            self.wait_ns
                .fetch_add(started.elapsed().as_nanos() as u64, Relaxed);
        }
        Ok(Some(Permit {
            _active: active,
            _capacity: capacity,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn put_only_leaves_session_posts_outside_full_queue() {
        let gate = Admission::new(1, 0).unwrap().put_only();
        let _permit = gate.acquire(&http::Method::PUT).await.unwrap().unwrap();
        assert!(gate.acquire(&http::Method::POST).await.unwrap().is_none());
        assert!(gate.acquire(&http::Method::PUT).await.is_err());
    }
    #[test]
    fn identifies_only_explicit_admission_rejections() {
        let mut headers = http::HeaderMap::new();
        let unavailable = http::StatusCode::SERVICE_UNAVAILABLE;
        assert_eq!(
            response_error(unavailable, &headers, "write status"),
            "write status"
        );
        for reason in [
            "ingress queue full",
            "ingress admission timeout",
            "ingress admission closed",
        ] {
            headers.insert(ERROR_HEADER, http::HeaderValue::from_static(reason));
            assert_eq!(
                response_error(unavailable, &headers, "write status"),
                reason
            );
            assert_eq!(
                response_error(http::StatusCode::BAD_REQUEST, &headers, "rename status"),
                "rename status"
            );
        }
        headers.insert(ERROR_HEADER, http::HeaderValue::from_static("unknown"));
        assert_eq!(
            response_error(unavailable, &headers, "rename status"),
            "rename status"
        );
    }

    #[tokio::test]
    async fn bounds_queue_bypasses_reads_and_releases_cancelled_waiters() {
        assert!(Admission::new(0, 1).is_err());
        assert!(Admission::new(1, 10001).is_err());
        let gate = Admission::new(1, 1).unwrap();
        let first = gate.acquire(&http::Method::PUT).await.unwrap();
        assert!(gate.acquire(&http::Method::GET).await.unwrap().is_none());
        assert!(gate.acquire(&http::Method::HEAD).await.unwrap().is_none());
        let mut queued = Box::pin(gate.acquire(&http::Method::POST));
        assert!(
            std::future::poll_fn(|cx| std::task::Poll::Ready(
                std::future::Future::poll(queued.as_mut(), cx).is_pending()
            ))
            .await
        );
        assert!(gate.acquire(&http::Method::DELETE).await.is_err());
        assert_eq!(gate.waiting.load(Relaxed), 1);
        drop(queued);
        let stats = gate.take_stats();
        assert_eq!(stats["waiting"], 0);
        assert_eq!(stats["peak_waiting"], 1);
        assert_eq!(stats["queued_writes"], 0);
        assert_eq!(gate.capacity.available_permits(), 1);
        drop(first);
        assert_eq!(gate.capacity.available_permits(), 2);
        assert!(gate.acquire(&http::Method::PUT).await.unwrap().is_some());
    }
}
