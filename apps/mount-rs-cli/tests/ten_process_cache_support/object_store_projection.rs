//! Fixed observations for the owned ten-CLI qualification. These are process
//! cumulative HTTP-service counters and leaf connector build wall time, never
//! CPU time, device IOPS or close acknowledgments.
//! Raw complete frames remain in the existing bounded stderr artifacts.
use super::config::sha256;
use super::contracts::{FILE_CAP, LINE_CAP};
use mount_rs_core::diagnostics::object_store::Snapshot;
use mount_rs_service::object_store_diagnostics::{self as codec, Capture, CaptureContext, Sample};
use serde_json::{Map, Value, json};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

pub struct CliBinding<'a> {
    pub node: &'a str,
    pub generation: u64,
    pub pid: u32,
    pub path: &'a str,
    pub owner_complete: bool,
}
pub struct WorkerBinding {
    pub pid: u32,
    pub job: u64,
    pub phase: Value,
}
pub struct Captured {
    pub sample: Option<Sample>,
    pub evidence: Value,
}
fn unavailable(reason: &'static str) -> Value {
    json!({"status":"unavailable","reason":reason})
}
fn absent(reason: &'static str) -> Captured {
    Captured {
        sample: None,
        evidence: unavailable(reason),
    }
}
fn evidence(sample: &Sample, bytes: &[u8]) -> Value {
    json!({"status":"observed","capture":sample.capture(),"raw_export_sha256":sha256(bytes),
        "quality":{"saturated":sample.snapshot().saturated,
            "concurrent_activity":sample.snapshot().concurrent_activity,
            "cache_unknown_live":sample.snapshot().cache.unknown_live}})
}

const ROLES: [&str; 6] = [
    "primary_data_mixed",
    "primary_probe_mixed",
    "qualification_data",
    "qualification_probe",
    "standalone_data",
    "standalone_probe",
];
const METHODS: [&str; 6] = ["get", "head", "put", "delete", "post", "other"];
const SCOPE: &str = "same owned process and sample context; inclusive cumulative HTTP-service observations; connector build durations cover inner HTTP connector connect wall time, not whole-client/store construction, HTTP request duration, CPU time or physical I/O; body bytes precede adapter validation and retries; gauges/maxima are endpoints; not storage API calls, transactional cuts, close proof, allocator calls, or physical IOPS";

fn quality(snapshot: &Snapshot) -> Result<(), &'static str> {
    if snapshot.saturated {
        Err("incomplete_quality")
    } else {
        Ok(())
    }
}
macro_rules! monotonic {
    ($a:expr, $b:expr; $($field:ident),* $(,)?) => {{
        let a = $a; let b = $b;
        $(if b.$field < a.$field { return Err("counter_reset"); })*
    }};
}
fn comparable(before: &Snapshot, after: &Snapshot) -> Result<(), &'static str> {
    quality(before)?;
    quality(after)?;
    for (a, b) in before.clients.iter().zip(&after.clients) {
        monotonic!(a,b; constructed,released);
        monotonic!(&a.build,&b.build; started,succeeded,failed,abandoned,elapsed_ns,max_ns);
        for (a, b) in a.http.iter().zip(&b.http) {
            monotonic!(a,b; attempts_started,header_responses,transport_errors,cancelled_before_headers,
                offered_bytes,offered_known,offered_unknown,known_extra_future_boxes,known_extra_response_body_boxes,
                bodies_started,body_eof,body_errors,body_dropped,body_bytes,body_chunks,dispatch_elapsed_ns,
                dispatch_max_ns,body_elapsed_ns,body_max_ns);
            for (a, b) in a.status.iter().zip(&b.status) {
                if b < a {
                    return Err("counter_reset");
                }
            }
        }
    }
    monotonic!(&before.bundles,&after.bundles; builds_started,committed,failed,abandoned,released,elapsed_ns,max_ns);
    monotonic!(&before.cache,&after.cache; created,released);
    Ok(())
}
macro_rules! row {
    ($a:expr, $b:expr; counters[$($counter:ident),*]; gauges[$($gauge:ident),*]; maxima[$($maximum:ident),*]) => {{
        let a = $a; let b = $b;
        let counters: Map<String,Value> = [$( (stringify!($counter).into(),json!(b.$counter.checked_sub(a.$counter).ok_or("counter_reset")?)), )*].into_iter().collect();
        let gauges: Map<String,Value> = [$( (stringify!($gauge).into(),json!({"before":a.$gauge,"after":b.$gauge})), )*].into_iter().collect();
        let maxima: Map<String,Value> = [$( (stringify!($maximum).into(),json!({"before":a.$maximum,"after":b.$maximum})), )*].into_iter().collect();
        json!({"counters":counters,"gauges":gauges,"maxima":maxima})
    }};
}
fn window(before: &Snapshot, after: &Snapshot) -> Result<Value, &'static str> {
    comparable(before, after)?;
    let clients = before.clients.iter().zip(&after.clients).enumerate().map(|(role,(a,b))| {
        let mut value = row!(a,b; counters[constructed,released]; gauges[live]; maxima[]);
        value["role"] = json!(ROLES[role]);
        value["build"] = row!(&a.build,&b.build; counters[started,succeeded,failed,abandoned,elapsed_ns]; gauges[inflight]; maxima[max_ns]);
        let http = a.http.iter().zip(&b.http).enumerate().map(|(method,(a,b))| {
            let mut value = row!(a,b; counters[attempts_started,header_responses,transport_errors,cancelled_before_headers,
                offered_bytes,offered_known,offered_unknown,known_extra_future_boxes,known_extra_response_body_boxes,
                bodies_started,body_eof,body_errors,body_dropped,body_bytes,body_chunks,dispatch_elapsed_ns,body_elapsed_ns];
                gauges[attempts_inflight,bodies_inflight]; maxima[dispatch_max_ns,body_max_ns]);
            value["method"] = json!(METHODS[method]);
            value["counters"]["status"] = json!(a.status.iter().zip(&b.status).map(|(a,b)| b.checked_sub(*a).ok_or("counter_reset")).collect::<Result<Vec<_>,_>>()?);
            Ok(value)
        }).collect::<Result<Vec<Value>, &'static str>>()?;
        value["http"] = json!(http);
        Ok(value)
    }).collect::<Result<Vec<Value>, &'static str>>()?;
    Ok(json!({"status":"observed","clients":clients,
        "bundles":row!(&before.bundles,&after.bundles; counters[builds_started,committed,failed,abandoned,released,elapsed_ns]; gauges[builds_inflight,live]; maxima[max_ns]),
        "cache":row!(&before.cache,&after.cache; counters[created,released]; gauges[live,resident_entries,payload_bytes,unknown_live]; maxima[]),
        "quality":{"before_concurrent_activity":before.concurrent_activity,
            "after_concurrent_activity":after.concurrent_activity,
            "cache_totals_complete":before.cache.unknown_live == 0 && after.cache.unknown_live == 0,
            "capture_atomic":false,"application_drain_proven":false},
        "scope":SCOPE}))
}

#[derive(Default)]
struct History {
    first: Option<Captured>,
    last: Option<Captured>,
    count: u64,
    any_concurrent_activity: bool,
    any_unknown_cache: bool,
}
impl History {
    fn accept(&mut self, sample: Sample, bytes: &[u8]) -> Result<(), &'static str> {
        quality(sample.snapshot())?;
        self.any_concurrent_activity |= sample.snapshot().concurrent_activity;
        self.any_unknown_cache |= sample.snapshot().cache.unknown_live != 0;
        if let Some(previous) = &self.last {
            let old = previous.sample.as_ref().ok_or("invalid_frames")?;
            if sample.capture().sequence <= old.capture().sequence
                || sample.capture().observed_unix_ms < old.capture().observed_unix_ms
            {
                return Err("duplicate_or_regressed_capture");
            }
            comparable(old.snapshot(), sample.snapshot())?;
        }
        if self.first.is_none() {
            self.first = Some(Captured {
                sample: Some(sample),
                evidence: evidence(&sample, bytes),
            });
        }
        self.last = Some(Captured {
            sample: Some(sample),
            evidence: evidence(&sample, bytes),
        });
        self.count = self.count.checked_add(1).ok_or("invalid_frames")?;
        Ok(())
    }
    fn project(&self) -> Result<Value, &'static str> {
        let (Some(first), Some(last)) = (&self.first, &self.last) else {
            return Ok(Value::Null);
        };
        let change = if self.count > 1 {
            window(
                first.sample.as_ref().ok_or("invalid_frames")?.snapshot(),
                last.sample.as_ref().ok_or("invalid_frames")?.snapshot(),
            )?
        } else {
            unavailable("single_sample")
        };
        Ok(
            json!({"samples":self.count,"first":first.evidence,"last":last.evidence,"window":change,
            "quality":{"any_concurrent_activity":self.any_concurrent_activity,
                "cache_totals_complete":!self.any_unknown_cache,
                "capture_atomic":false,"application_drain_proven":false}}),
        )
    }
}
pub fn project_cli(binding: &CliBinding<'_>, bytes: &[u8]) -> Value {
    let owner = json!({"node":binding.node,"pid":binding.pid,"generation":binding.generation,
        "stderr":{"path":binding.path,"bytes":bytes.len(),"sha256":sha256(bytes)}});
    let result = || -> Result<Value, &'static str> {
        if !binding.owner_complete
            || binding.pid == 0
            || binding.node.is_empty()
            || binding.path.is_empty()
        {
            return Err("invalid_owner");
        }
        if bytes.len() as u64 > FILE_CAP
            || (!bytes.is_empty() && bytes.last() != Some(&b'\n'))
            || std::str::from_utf8(bytes).is_err()
        {
            return Err("invalid_output");
        }
        let mut histories: [History; 2] = std::array::from_fn(|_| History::default());
        let mut group = Vec::new();
        let mut count = 0;
        for line in bytes.split_inclusive(|byte| *byte == b'\n') {
            if line.len().saturating_sub(1) > LINE_CAP {
                return Err("invalid_output");
            }
            if line.starts_with(b"object_store_diagnostics") && !line.starts_with(codec::PREFIX) {
                return Err("invalid_frames");
            }
            if !line.starts_with(codec::PREFIX) {
                continue;
            }
            if line.len() > codec::RECORD_LIMIT {
                return Err("invalid_frames");
            }
            group.extend_from_slice(line);
            count += 1;
            if count == codec::FRAME_COUNT {
                let sample = codec::decode(&group).map_err(|_| "invalid_frames")?;
                let identity = sample.capture();
                if identity.pid != binding.pid || identity.generation.is_some() {
                    return Err("foreign_capture");
                }
                let index = match identity.context {
                    CaptureContext::Periodic => 0,
                    CaptureContext::Shutdown => 1,
                    CaptureContext::WorkerBoundary => return Err("foreign_capture"),
                };
                histories[index].accept(sample, &group)?;
                group.clear();
                count = 0;
            }
        }
        if count != 0 {
            return Err("invalid_frames");
        }
        if histories.iter().all(|history| history.count == 0) {
            return Err("disabled_or_missing");
        }
        Ok(
            json!({"status":"observed","contexts":{"periodic":histories[0].project()?,"shutdown":histories[1].project()?},"scope":SCOPE}),
        )
    };
    let mut value = match result() {
        Ok(value) => value,
        Err(reason) => unavailable(reason),
    };
    value["owner"] = owner;
    value
}

pub fn capture_worker(
    enabled: bool,
    sequence: &AtomicU64,
    identity: impl FnOnce(u64) -> Option<Capture>,
    snapshot: impl FnOnce() -> Option<Snapshot>,
    output: &mut impl Write,
) -> Captured {
    if !enabled {
        return absent("disabled_or_missing");
    }
    let Ok(number) =
        sequence.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |old| old.checked_add(1))
    else {
        return absent("capture_exhausted");
    };
    let Some(identity) = identity(number) else {
        return absent("capture_identity");
    };
    if identity.pid == 0
        || identity.sequence != number
        || identity.context != CaptureContext::WorkerBoundary
        || identity.generation.is_some()
    {
        return absent("capture_identity");
    }
    let Ok(Some(sample)) = codec::capture(true, identity, snapshot) else {
        return absent("capture_unavailable");
    };
    let mut bytes = Vec::new();
    if sample.write(&mut bytes).is_err() || output.write_all(&bytes).is_err() {
        return absent("capture_export_failed");
    }
    Captured {
        sample: Some(sample),
        evidence: evidence(&sample, &bytes),
    }
}
pub fn project_worker(binding: &WorkerBinding, before: &Captured, after: &Captured) -> Value {
    let result = || -> Result<Value, &'static str> {
        let (Some(first), Some(last)) = (&before.sample, &after.sample) else {
            return Err("capture_unavailable");
        };
        let a = first.capture();
        let b = last.capture();
        if binding.pid == 0
            || a.pid != binding.pid
            || b.pid != binding.pid
            || a.context != CaptureContext::WorkerBoundary
            || b.context != CaptureContext::WorkerBoundary
            || a.generation.is_some()
            || b.generation.is_some()
            || a.sequence >= b.sequence
            || a.observed_unix_ms > b.observed_unix_ms
        {
            return Err("mixed_window");
        }
        Ok(
            json!({"status":"observed","before":before.evidence,"after":after.evidence,"window":window(first.snapshot(),last.snapshot())?}),
        )
    };
    let mut value = match result() {
        Ok(value) => value,
        Err(reason) => unavailable(reason),
    };
    value["before"] = before.evidence.clone();
    value["after"] = after.evidence.clone();
    value["owner"] = json!({"pid":binding.pid,"job":binding.job,"phase":binding.phase,"stderr":"worker.stderr","server_generation":Value::Null});
    value
}

#[cfg(test)]
#[path = "object_store_projection_tests.rs"]
mod tests;
