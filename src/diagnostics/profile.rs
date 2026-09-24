//! Opt-in, bounded process-local counters for integration I/O investigations.
//! Set MOUNT_RS_PROFILE_IO=1 before starting the process. No paths or payloads
//! are recorded. Durations are inclusive wall time, not exclusive CPU time.

use serde::Serialize;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

macro_rules! events {
    ($($event:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy)]
        #[repr(usize)]
        pub enum Event { $($event),+ }
        const NAMES: &[&str] = &[$($name),+];
    };
}
events! {
    WireEncode => "wire.json_encode_bytes",
    WireDecode => "wire.json_decode_bytes",
    CatalogLoad => "catalog.load",
    CatalogQueue => "catalog.queue_wait",
    CatalogPoolWait => "catalog.pool_wait",
    CatalogBackingVerify => "catalog.backing_verify",
    CatalogConnect => "catalog.connect_configure",
    CatalogQuery => "catalog.query_document_bytes",
    CatalogDecode => "catalog.decode_validate_bytes",
    CatalogClose => "catalog.close",
    CatalogPagerHits => "catalog.pager_hits",
    CatalogPagerMisses => "catalog.pager_misses",
    CatalogPagerWrites => "catalog.pager_writes",
    CatalogPagerUnavailable => "catalog.pager_unavailable",
    Dispatch => "service.dispatch",
    Authorization => "service.authorization",
    HandleWait => "service.handle_lock_wait",
    Audit => "service.audit",
    GateWait => "filesystem.gate_wait",
    Snapshot => "filesystem.snapshot_nodes",
    Refresh => "filesystem.metadata_refresh",
    Changed => "filesystem.changed_namespace_nodes",
    Fallback => "filesystem.write_fallback",
    RewriteRead => "filesystem.old_chunk_read_bytes",
    MetadataLoad => "provider.metadata.load",
    MetadataConditional => "provider.metadata.load_if_changed",
    BlockGet => "provider.blocks.get_bytes",
    BlockPut => "provider.blocks.put_bytes",
    BlockFlush => "provider.blocks.flush",
    BackingVerify => "provider.blocks.verify_authority",
    Publication => "provider.metadata.publish_cas_nodes",
    PublishConflict => "provider.metadata.cas_conflict",
    NamespaceReturned => "provider.namespace_returned_bytes",
    NamespaceSerialized => "provider.namespace_serialized_bytes",
}

#[derive(Default)]
struct Metric {
    calls: AtomicU64,
    elapsed_ns: AtomicU64,
    units: AtomicU64,
}
struct Recorder {
    metrics: Vec<Metric>,
}
impl Recorder {
    fn new() -> Self {
        Self {
            metrics: NAMES.iter().map(|_| Metric::default()).collect(),
        }
    }
    fn record(&self, event: Event, elapsed_ns: u64, units: u64) {
        let metric = &self.metrics[event as usize];
        metric.calls.fetch_add(1, Ordering::Relaxed);
        metric.elapsed_ns.fetch_add(elapsed_ns, Ordering::Relaxed);
        metric.units.fetch_add(units, Ordering::Relaxed);
    }
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            entries: self
                .metrics
                .iter()
                .enumerate()
                .map(|(index, m)| Entry {
                    name: NAMES[index],
                    calls: m.calls.load(Ordering::Relaxed),
                    elapsed_ns: m.elapsed_ns.load(Ordering::Relaxed),
                    units: m.units.load(Ordering::Relaxed),
                })
                .collect(),
        }
    }
}
static ENABLED: OnceLock<bool> = OnceLock::new();
static RECORDER: OnceLock<Recorder> = OnceLock::new();
pub fn enabled() -> bool {
    *ENABLED.get_or_init(|| std::env::var_os("MOUNT_RS_PROFILE_IO").is_some_and(|v| v == "1"))
}
pub fn add(event: Event, units: u64) {
    if enabled() {
        RECORDER.get_or_init(Recorder::new).record(event, 0, units);
    }
}
pub fn snapshot() -> Snapshot {
    RECORDER.get_or_init(Recorder::new).snapshot()
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub name: &'static str,
    pub calls: u64,
    pub elapsed_ns: u64,
    pub units: u64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub entries: Vec<Entry>,
}
impl Snapshot {
    /// Call at quiescent stage boundaries; concurrent snapshots are not atomic.
    pub fn delta(&self, before: &Self) -> Result<Self, &'static str> {
        if self.entries.len() != before.entries.len() {
            return Err("profile shape changed");
        }
        let mut entries = Vec::new();
        for (now, old) in self.entries.iter().zip(&before.entries) {
            if now.name != old.name {
                return Err("profile shape changed");
            }
            let entry = Entry {
                name: now.name,
                calls: now
                    .calls
                    .checked_sub(old.calls)
                    .ok_or("profile counter reset")?,
                elapsed_ns: now
                    .elapsed_ns
                    .checked_sub(old.elapsed_ns)
                    .ok_or("profile counter reset")?,
                units: now
                    .units
                    .checked_sub(old.units)
                    .ok_or("profile counter reset")?,
            };
            if entry.calls != 0 || entry.units != 0 {
                entries.push(entry);
            }
        }
        Ok(Self { entries })
    }
}
pub struct Span {
    event: Event,
    started: Option<Instant>,
    units: u64,
}
impl Span {
    pub fn new(event: Event) -> Self {
        Self {
            event,
            started: enabled().then(Instant::now),
            units: 0,
        }
    }
    pub fn units(mut self, units: u64) -> Self {
        self.units = units;
        self
    }
    pub fn set_units(&mut self, units: u64) {
        self.units = units;
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            RECORDER.get_or_init(Recorder::new).record(
                self.event,
                started.elapsed().as_nanos().min(u64::MAX as u128) as u64,
                self.units,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concurrent_counters_and_quiescent_deltas_reconcile() {
        let recorder = Recorder::new();
        let before = recorder.snapshot();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let recorder = &recorder;
                scope.spawn(move || {
                    for _ in 0..250 {
                        recorder.record(Event::BlockGet, 2, 4096);
                    }
                });
            }
        });
        let after = recorder.snapshot();
        let delta = after.delta(&before).unwrap();
        let entry = delta
            .entries
            .iter()
            .find(|e| e.name == "provider.blocks.get_bytes")
            .unwrap();
        assert_eq!(entry.calls, 1000);
        assert_eq!(entry.elapsed_ns, 2000);
        assert_eq!(entry.units, 4096000);
        assert!(before.delta(&after).is_err());
    }
}
