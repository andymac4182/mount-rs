//! Diagnostic for the pinned object_store 0.12.5 retry-loop INFO events.
//! Counts across all requests in this worker, including writes. Does not
//! capture fields, URLs or credentials, and does not count HTTP attempts.
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{Event, Subscriber};
use tracing_subscriber::{Layer, layer::Context, prelude::*};

static EVENTS: AtomicU64 = AtomicU64::new(0);

struct RetryEvents;

impl<S: Subscriber> Layer<S> for RetryEvents {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if event.metadata().target() == "mount_audit_h2" {
            super::request_detail::h2_event(event);
        }
        if event.metadata().target() == "mount_audit_pool" {
            super::request_detail::PoolEvent::capture(event);
        }
        #[cfg(feature = "audit-storage-timing")]
        if event.metadata().target() == "mount_rs.event" {
            let mut timing = StorageEvent::default();
            event.record(&mut timing);
            if timing.selected() {
                let end = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("benchmark clock before epoch")
                    .as_micros();
                eprintln!(
                    "storage-finished pid={} end_us={end} operation={} duration_ms={}",
                    std::process::id(),
                    timing.operation,
                    timing.duration_ms
                );
            }
        }
        if is_retry_event(event.metadata()) {
            EVENTS.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn is_retry_event(metadata: &tracing::Metadata<'_>) -> bool {
    metadata.target() == "object_store::client::retry" && *metadata.level() == tracing::Level::INFO
}

#[cfg(feature = "audit-storage-timing")]
#[derive(Default)]
struct StorageEvent {
    boundary: String,
    operation: String,
    duration_ms: u64,
}

#[cfg(feature = "audit-storage-timing")]
impl StorageEvent {
    fn selected(&self) -> bool {
        self.duration_ms >= 10
            && matches!(
                (self.boundary.as_str(), self.operation.as_str()),
                ("provider.metadata", "publish") | ("provider.blocks", "put")
            )
    }
}

#[cfg(feature = "audit-storage-timing")]
impl tracing::field::Visit for StorageEvent {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "boundary" => self.boundary.push_str(value),
            "operation" => self.operation.push_str(value),
            _ => {}
        }
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        if field.name() == "duration_ms" {
            self.duration_ms = value;
        }
    }
    fn record_debug(&mut self, _: &tracing::field::Field, _: &dyn std::fmt::Debug) {}
}

pub fn install() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    #[cfg(feature = "audit-storage-timing")]
    mount_rs_sdk::set_global_telemetry(mount_rs_sdk::Telemetry::new(
        mount_rs_sdk::TelemetryConfig::enabled("mount-load-worker"),
    ));
    tracing::subscriber::set_global_default(tracing_subscriber::registry().with(
        RetryEvents.with_filter(tracing_subscriber::filter::filter_fn(|m| {
            (cfg!(feature = "audit-storage-timing") && m.target() == "mount_rs.event")
                || is_retry_event(m)
                || matches!(m.target(), "mount_audit_pool" | "mount_audit_h2")
        })),
    ))?;
    Ok(())
}

pub fn count() -> u64 {
    EVENTS.load(Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_only_pinned_retry_target_info_without_recording_fields() {
        let subscriber = tracing_subscriber::registry()
            .with(RetryEvents.with_filter(tracing_subscriber::filter::filter_fn(is_retry_event)));
        tracing::subscriber::with_default(subscriber, || {
            let before = count();
            tracing::info!(target: "object_store::client::retry", "retry diagnostic");
            tracing::debug!(target: "object_store::client::retry", "not a retry");
            tracing::info!(target: "another_target", "not a retry");
            assert_eq!(count() - before, 1);
        });
    }
}
