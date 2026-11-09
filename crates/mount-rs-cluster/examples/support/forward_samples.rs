//! Bounded, benchmark-only correlation; no global per-request mutex.
use mount_rs_cluster::ForwardRequestTrace;
use std::sync::{
    OnceLock,
    atomic::{AtomicU64, Ordering},
};
use tokio::time::Instant;

/// Client elapsed intervals; ingress is a nested server duration, not additive.
#[derive(Default, serde::Serialize)]
pub struct ClientTiming {
    #[serde(skip)]
    pub started: Option<Instant>,
    #[serde(skip)]
    pub polls: super::request_detail::ClientPolls,
    pub response_headers_ns: u64,
    pub response_body_ns: u64,
    pub ingress_ns: Option<u64>,
}

#[derive(Clone, Copy)]
pub struct ServiceStart(pub Instant);
struct Slot {
    handler: Instant,
    service: Option<Instant>,
    pool: std::sync::Arc<super::request_detail::Pool>,
    trace: ForwardRequestTrace,
}
pub const HEADER: &str = "x-mount-audit-sample";
pub struct Capture {
    h2: std::sync::Arc<super::request_detail::H2Clock>,
    slots: Vec<OnceLock<Slot>>,
    invalid: AtomicU64,
}
impl Capture {
    pub fn new(count: usize) -> Result<Self, &'static str> {
        if count == 0 || count > 20_000 {
            return Err("trace batch must contain 1..20000 requests including warmup");
        }
        Ok(Self {
            h2: super::request_detail::H2Clock::start(count * 2),
            slots: (0..count).map(|_| OnceLock::new()).collect(),
            invalid: AtomicU64::new(0),
        })
    }
    pub fn attach(
        &self,
        request: &mut http::Request<mount_rs_http::HttpBody>,
        started: Instant,
    ) -> Option<usize> {
        let header = request.headers_mut().remove(HEADER)?;
        let index = header
            .to_str()
            .ok()
            .and_then(|value| value.parse::<usize>().ok());
        let Some(slot) = index.and_then(|index| self.slots.get(index)) else {
            self.invalid.fetch_add(1, Ordering::Relaxed);
            return None;
        };
        let trace = ForwardRequestTrace::default();
        let pool = std::sync::Arc::new(super::request_detail::Pool::default());
        let service = request.extensions().get::<ServiceStart>().map(|s| s.0);
        if slot
            .set(Slot {
                handler: started,
                trace: trace.clone(),
                service,
                pool: pool.clone(),
            })
            .is_err()
        {
            self.invalid.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        request.extensions_mut().insert(pool);
        request.extensions_mut().insert(trace);
        index.map(|i| self.slots.len() + i)
    }
    pub fn report(&self, mut observed: Vec<(usize, f64, ClientTiming)>) -> serde_json::Value {
        let missing_ingress_timing = observed
            .iter()
            .filter(|(_, _, client)| client.ingress_ns.is_none())
            .count();
        let mut missing = 0;
        let mut forwarded = 0;
        let mut complete = 0;
        for (index, _, _) in &observed {
            match self.slots.get(*index).and_then(OnceLock::get) {
                None => missing += 1,
                Some(slot) => {
                    let sample = slot.trace.snapshot();
                    forwarded += usize::from(sample.route_ns.is_some());
                    complete += usize::from(sample.complete);
                }
            }
        }
        observed.sort_by(|a, b| b.1.total_cmp(&a.1));
        let count = observed.len();
        let slowest: Vec<_> = observed
            .into_iter()
            .take(512)
            .map(|(index, elapsed, client)| {
                let slot = self.slots.get(index).and_then(OnceLock::get);
                let before_handler_ns = slot.and_then(|slot| {
                    let duration = slot.handler.checked_duration_since(client.started?)?;
                    u64::try_from(duration.as_nanos()).ok()
                });
                let after_handler_to_headers_ns = before_handler_ns.and_then(|before| {
                    client
                        .response_headers_ns
                        .checked_sub(before)?
                        .checked_sub(client.ingress_ns?)
                });
                let pre_service_ns = slot.and_then(|s| Some(s.service?.checked_duration_since(client.started?)?.as_nanos() as u64));
                let offsets = client.started.map(|start| self.h2.offsets(index,start));
                serde_json::json!({
                    "before_http_service_ns": pre_service_ns,
                    "service_to_handler_ns": before_handler_ns.zip(pre_service_ns).and_then(|(end,start)| end.checked_sub(start)),
                    "pre_service_detail": pre_service_ns.map(|ns|client.polls.before(ns)),
                    "h2_driver_ready_and_poll_age_ns":self.h2.driver_intervals(index),
                    "response_frame_offsets_ns": client.started.map(|start| {
                        let s = self.h2.offsets(self.slots.len() + index, start);
                        [s[0], s[1], s[2], s[3], s[4], s[5], s[6]]
                    }),
                    "socket_read_ready_monotonic_ns": [self.h2.read_ready_monotonic_ns(index), self.h2.read_ready_monotonic_ns(self.slots.len() + index)],
                    "socket_read_offsets_ns": client.started.map(|start| {
                        let request = self.h2.offsets(index, start);
                        let response = self.h2.offsets(self.slots.len() + index, start);
                        [[request[7], request[8]], [response[7], response[8]]]
                    }),
                    "h2_offsets_ns": offsets.map(|s| [s[0], s[1], s[2], s[3]]),
                    "h2_io_offsets_ns": offsets.map(|s| serde_json::json!({
                        "final_write_poll_start":s[4], "final_write_poll_end":s[5],
                        "server_frame_decode_start":s[6]
                    })),
                    "pool_ns": slot.map(|s|s.pool.snapshot()),
                    "sample": index, "client_ms": elapsed, "client": client,
                    "before_handler_ns": before_handler_ns,
                    "after_handler_to_headers_ns": after_handler_to_headers_ns,
                    "response_header_detail": after_handler_to_headers_ns.map(|ns| client.polls.response_headers(
                        client.response_headers_ns - ns, client.response_headers_ns)),
                    "forward": slot.map(|s| s.trace.snapshot()),
                })
            })
            .collect();
        serde_json::json!({"measured_reads":count,"missing_ingress_timing":missing_ingress_timing,"missing":missing,"forwarded":forwarded,"complete":complete,"invalid_or_duplicate":self.invalid.load(Ordering::Relaxed),"slowest":slowest})
    }
}

pub fn enabled() -> Result<bool, &'static str> {
    match std::env::var("AUDIT_FORWARD_SAMPLES").as_deref() {
        Ok("1") => Ok(true),
        Ok("0") | Err(std::env::VarError::NotPresent) => Ok(false),
        _ => Err("AUDIT_FORWARD_SAMPLES must be 0 or 1"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_clock_header_intervals_are_paired_per_sample() {
        use std::time::Duration;
        let capture = Capture::new(1).unwrap();
        let start = Instant::now();
        capture.slots[0]
            .set(Slot {
                handler: start + Duration::from_millis(2),
                service: None,
                trace: ForwardRequestTrace::default(),
                pool: Default::default(),
            })
            .ok()
            .unwrap();
        let report = capture.report(vec![(
            0,
            8.0,
            ClientTiming {
                polls: Default::default(),
                started: Some(start),
                response_headers_ns: 7_000_000,
                response_body_ns: 1_000_000,
                ingress_ns: Some(3_000_000),
            },
        )]);
        let sample = &report["slowest"][0];
        assert_eq!(
            sample["h2_offsets_ns"],
            serde_json::json!([null, null, null, null])
        );
        assert_eq!(
            sample["h2_io_offsets_ns"],
            serde_json::json!({
                "final_write_poll_start": null, "final_write_poll_end": null,
                "server_frame_decode_start": null
            })
        );
        assert_eq!(sample["before_handler_ns"], 2_000_000);
        assert_eq!(sample["after_handler_to_headers_ns"], 2_000_000);
    }

    #[test]
    fn bounds_and_missing_samples_are_explicit() {
        assert!(Capture::new(0).is_err());
        assert!(Capture::new(20_001).is_err());
        let capture = Capture::new(100).unwrap();
        let report = capture.report(
            (0..100)
                .map(|i| {
                    (
                        i,
                        i as f64,
                        ClientTiming {
                            polls: Default::default(),
                            started: None,
                            response_headers_ns: i as u64,
                            response_body_ns: 17,
                            ingress_ns: Some(5),
                        },
                    )
                })
                .collect(),
        );
        assert_eq!(report["missing"], 100);
        assert_eq!(report["complete"], 0);
        assert_eq!(report["slowest"].as_array().unwrap().len(), 100);
        assert_eq!(report["slowest"][0]["sample"], 99);
        assert_eq!(report["slowest"][0]["client"]["response_headers_ns"], 99);
        assert_eq!(report["slowest"][0]["client"]["response_body_ns"], 17);
        assert_eq!(report["slowest"][0]["client"]["ingress_ns"], 5);
    }
}
