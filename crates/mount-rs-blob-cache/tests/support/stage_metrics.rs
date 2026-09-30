use mount_rs_core::diagnostics::{profile, storage};

pub const ADMISSION: &str = "blob_cache.miss.admission_wait";
pub const FLIGHT: &str = "blob_cache.miss.singleflight_wait";
pub const RAM: &str = "blob_cache.ram.lookup";
pub const DISK: &str = "blob_cache.disk.lookup";
pub const LOCK: &str = "blob_cache.peer.connection_lock_wait";
pub const ESTABLISH: &str = "blob_cache.peer.connection_establish";
pub const RAM_HIT: &str = "blob_cache.ram.hit_bytes";
pub const DISK_HIT: &str = "blob_cache.disk.hit_bytes";

pub struct Expected(pub &'static str, pub u64, pub u64, pub u64, pub u64);
type PendingObservation = (
    &'static str,
    storage::Snapshot,
    u64,
    Vec<(&'static str, u64)>,
);

#[derive(Default)]
pub struct Checks {
    phases: Vec<(&'static str, storage::Snapshot, Vec<Expected>)>,
    pending: Vec<PendingObservation>,
    hits: Vec<(
        &'static str,
        profile::Snapshot,
        profile::Snapshot,
        &'static str,
        u64,
        u64,
    )>,
}
impl Checks {
    pub fn phase(
        &mut self,
        name: &'static str,
        before: &storage::Snapshot,
        expected: Vec<Expected>,
    ) {
        self.phases
            .push((name, storage::snapshot().delta(before).unwrap(), expected));
    }
    pub fn pending(&mut self, name: &'static str, global: u64, rows: Vec<(&'static str, u64)>) {
        self.pending.push((name, storage::snapshot(), global, rows));
    }
    pub fn hit(
        &mut self,
        phase: &'static str,
        before: &profile::Snapshot,
        name: &'static str,
        calls: u64,
        units: u64,
    ) {
        let after = profile::snapshot();
        self.hits.push((
            phase,
            after.delta(before).unwrap(),
            after,
            name,
            calls,
            units,
        ));
    }
    pub fn verify(self) {
        // Authored static labels need no escaping. Emit all observations before
        // assertions so a missing producer cannot hide later retained phases.
        for (phase, snapshot, expected) in &self.phases {
            for Expected(name, _, _, _, _) in expected {
                if let Some(row) = snapshot.entries.iter().find(|row| row.name == *name) {
                    println!(
                        "MOUNT_RS_CACHE_STAGE {{\"kind\":\"storage_delta\",\"phase\":\"{phase}\",\"name\":\"{name}\",\"available\":true,\"calls\":{},\"success\":{},\"error\":{},\"cancelled\":{},\"bytes\":{},\"elapsed_ns\":{},\"in_flight\":{},\"global_in_flight\":{},\"histogram_total\":{},\"returned_rows\":{},\"returned_row_observations\":{}}}",
                        row.calls,
                        row.success,
                        row.error,
                        row.cancelled,
                        row.bytes,
                        row.elapsed_ns,
                        row.in_flight,
                        snapshot.in_flight,
                        row.latency_log2_us.iter().sum::<u64>(),
                        row.returned_rows,
                        row.returned_row_observations
                    );
                } else {
                    println!(
                        "MOUNT_RS_CACHE_STAGE {{\"kind\":\"storage_delta\",\"phase\":\"{phase}\",\"name\":\"{name}\",\"available\":false,\"global_in_flight\":{}}}",
                        snapshot.in_flight
                    );
                }
            }
        }
        for (phase, snapshot, _, expected) in &self.pending {
            println!(
                "MOUNT_RS_CACHE_STAGE {{\"kind\":\"pending_global\",\"phase\":\"{phase}\",\"in_flight\":{}}}",
                snapshot.in_flight
            );
            for (name, _) in expected {
                if let Some(row) = snapshot.entries.iter().find(|row| row.name == *name) {
                    println!(
                        "MOUNT_RS_CACHE_STAGE {{\"kind\":\"pending_row\",\"phase\":\"{phase}\",\"name\":\"{name}\",\"available\":true,\"calls\":{},\"success\":{},\"error\":{},\"cancelled\":{},\"bytes\":{},\"elapsed_ns\":{},\"in_flight\":{},\"histogram_total\":{},\"returned_rows\":{},\"returned_row_observations\":{}}}",
                        row.calls,
                        row.success,
                        row.error,
                        row.cancelled,
                        row.bytes,
                        row.elapsed_ns,
                        row.in_flight,
                        row.latency_log2_us.iter().sum::<u64>(),
                        row.returned_rows,
                        row.returned_row_observations
                    );
                } else {
                    println!(
                        "MOUNT_RS_CACHE_STAGE {{\"kind\":\"pending_row\",\"phase\":\"{phase}\",\"name\":\"{name}\",\"available\":false}}"
                    );
                }
            }
        }
        for (phase, delta, after, name, _, _) in &self.hits {
            if after.entries.iter().any(|row| row.name == *name) {
                let (calls, units, elapsed_ns) = delta
                    .entries
                    .iter()
                    .find(|row| row.name == *name)
                    .map(|row| (row.calls, row.units, row.elapsed_ns))
                    .unwrap_or((0, 0, 0));
                println!(
                    "MOUNT_RS_CACHE_STAGE {{\"kind\":\"hit_delta\",\"phase\":\"{phase}\",\"name\":\"{name}\",\"available\":true,\"calls\":{calls},\"units\":{units},\"elapsed_ns\":{elapsed_ns}}}"
                );
            } else {
                println!(
                    "MOUNT_RS_CACHE_STAGE {{\"kind\":\"hit_delta\",\"phase\":\"{phase}\",\"name\":\"{name}\",\"available\":false}}"
                );
            }
        }
        // Literal names compile against the old bank. Only after every behavior
        // and ownership oracle completes do missing producers become semantic RED.
        for (phase, snapshot, expected) in self.phases {
            assert_eq!(snapshot.in_flight, 0, "global quiescence for {phase}");
            for Expected(name, success, error, cancelled, bytes) in expected {
                let row = snapshot
                    .entries
                    .iter()
                    .find(|row| row.name == name)
                    .unwrap_or_else(|| {
                        panic!(
                            "missing stage metric {name} after completed behavior oracles ({phase})"
                        )
                    });
                assert_eq!(
                    (row.success, row.error, row.cancelled, row.bytes),
                    (success, error, cancelled, bytes),
                    "stage outcomes for {phase}: {name}"
                );
                assert_eq!(
                    row.calls,
                    success + error + cancelled,
                    "terminal calls for {phase}: {name}"
                );
                assert_eq!(
                    row.latency_log2_us.iter().sum::<u64>(),
                    row.calls,
                    "histogram for {phase}: {name}"
                );
                assert_eq!(row.in_flight, 0, "row quiescence for {phase}: {name}");
                assert_eq!(
                    (row.returned_rows, row.returned_row_observations),
                    (0, 0),
                    "unknown SQL rows for {phase}: {name}"
                );
            }
        }
        for (phase, snapshot, global, expected) in self.pending {
            assert_eq!(
                snapshot.in_flight, global,
                "pending global gauge for {phase}"
            );
            for (name, inflight) in expected {
                let row = snapshot
                    .entries
                    .iter()
                    .find(|row| row.name == name)
                    .unwrap_or_else(|| panic!("missing pending stage metric {name} ({phase})"));
                assert_eq!(
                    row.in_flight, inflight,
                    "pending row gauge for {phase}: {name}"
                );
            }
        }
        for (phase, delta, after, name, calls, units) in self.hits {
            assert!(
                after.entries.iter().any(|row| row.name == name),
                "missing hit metric {name} ({phase})"
            );
            let actual = delta
                .entries
                .iter()
                .find(|row| row.name == name)
                .map(|row| (row.calls, row.units))
                .unwrap_or((0, 0));
            assert_eq!(
                actual,
                (calls, units),
                "hit calls and returned bytes for {phase}: {name}"
            );
        }
    }
}
