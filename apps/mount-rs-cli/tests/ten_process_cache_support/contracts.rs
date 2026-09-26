//! Pure receipt/oracle helpers. These tests do not start processes or fixtures.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

pub type Result<T> = std::result::Result<T, String>;
pub const NODES: usize = 10;
pub const BLOCK_BYTES: usize = 4096;
pub const SETUP_SECONDS: u64 = 600;
pub const WORK_SECONDS: u64 = 1800;
pub const PHASE_SECONDS: u64 = 600;
pub const REQUEST_SECONDS: u64 = 30;
pub const SHUTDOWN_SECONDS: u64 = 95;
pub const AUDIT_SECONDS: u64 = 30;
pub const FILE_CAP: u64 = 8 * 1024 * 1024;
pub const LINE_CAP: usize = 16 * 1024;
pub const RSS_CAP: u64 = 24 * 1024 * 1024 * 1024;
pub const RESOURCE_CAP: u64 = 16 * 1024;
pub const RESOURCE_FRESH_NS: u64 = 1_000_000_000;
pub const RESOURCE_MAX_IDENTITIES: usize = NODES + 3;

#[cfg(all(
    feature = "local-oidc-fixture",
    debug_assertions,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
mod native {
    pub const PARTITIONS: usize = 5;
    pub const FILES: usize = 16;
    pub const RAM_BYTES: usize = 64 * 1024;
    pub const DISK_BYTES: usize = 1024 * 1024;
    pub const DISK_ENTRY_BYTES: usize = super::BLOCK_BYTES + 32;
    pub const FORCE_SECONDS: u64 = 5;
    pub const POLL_MS: u64 = 100;
    pub const TOTAL_CAP: u64 = 160 * 1024 * 1024;
    pub const FREE_DISK_FLOOR: u64 = 64 * 1024 * 1024 * 1024;
    pub fn payload(drive: usize, file: usize) -> Vec<u8> {
        (0..super::BLOCK_BYTES)
            .map(|offset| ((drive * 37 + file * 19 + offset * 13) % 251) as u8)
            .collect()
    }
}
#[cfg(all(
    feature = "local-oidc-fixture",
    debug_assertions,
    any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))
))]
pub use native::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(role: &str, pid: u32, generation: u64) -> RssIdentity {
        RssIdentity {
            role: role.into(),
            node: role.into(),
            pid,
            generation,
        }
    }
    fn observation(identity: RssIdentity, bytes: Option<u64>) -> RssObservation {
        RssObservation {
            identity,
            started_ns: 10,
            finished_ns: 20,
            bytes,
            missing: bytes.is_none().then(|| "unavailable".into()),
        }
    }
    fn resource_frame() -> ResourceFrame {
        let controller = identity("controller", 10, 0);
        let worker = identity("worker", 20, 0);
        ResourceFrame {
            schema: 1,
            run: ResourceRun {
                root: std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("pure-rss-fixture-not-created")
                    .to_string_lossy()
                    .into_owned(),
                controller_pid: 10,
                worker_pid: 20,
                group: 20,
            },
            sequence: 1,
            started_ns: 10,
            finished_ns: 20,
            expected: vec![controller.clone(), worker.clone()],
            observations: vec![
                observation(controller, Some(100)),
                observation(worker, Some(200)),
            ],
            total_bytes: Some(300),
            child_bytes: Some(0),
            max_total_bytes: 300,
            error: None,
            terminal: false,
        }
    }
    #[test]
    fn rss_sum_enforces_aggregate_threshold_with_individually_safe_members() {
        let expected = vec![identity("controller", 1, 0), identity("worker", 2, 0)];
        let measured = |a, b| {
            vec![
                observation(expected[0].clone(), Some(a)),
                observation(expected[1].clone(), Some(b)),
            ]
        };
        assert_eq!(
            rss_totals(&expected, &measured(RSS_CAP - 2, 1))
                .unwrap()
                .total,
            RSS_CAP - 1
        );
        assert!(rss_totals(&expected, &measured(RSS_CAP / 2, RSS_CAP / 2)).is_err());
        assert!(rss_totals(&expected, &measured(RSS_CAP / 2, RSS_CAP / 2 + 1)).is_err());
        assert!(rss_totals(&expected, &measured(RSS_CAP, 0)).is_err());
    }
    #[test]
    fn rss_sum_rejects_missing_duplicate_foreign_and_restarted_identity() {
        let frame = resource_frame();
        let mut missing = frame.observations.clone();
        missing[1].bytes = None;
        missing[1].missing = Some("unavailable".into());
        assert!(rss_totals(&frame.expected, &missing).is_err());
        assert!(rss_totals(&frame.expected, &frame.observations[..1]).is_err());
        let mut duplicate = frame.observations.clone();
        duplicate[1] = duplicate[0].clone();
        assert!(rss_totals(&frame.expected, &duplicate).is_err());
        let mut foreign = frame.observations.clone();
        foreign[1].identity.pid = 99;
        assert!(rss_totals(&frame.expected, &foreign).is_err());
        let mut restarted = frame.observations.clone();
        restarted[1].identity.generation = 1;
        assert!(rss_totals(&frame.expected, &restarted).is_err());
    }
    #[test]
    fn rss_sum_reports_checked_overflow_and_invalid_capture() {
        let frame = resource_frame();
        let mut overflow = frame.observations.clone();
        overflow[0].bytes = Some(u64::MAX);
        overflow[1].bytes = Some(1);
        assert!(
            rss_totals(&frame.expected, &overflow)
                .unwrap_err()
                .contains("overflow")
        );
        let mut backwards = frame.observations.clone();
        backwards[0].started_ns = 21;
        assert!(rss_totals(&frame.expected, &backwards).is_err());
    }
    #[test]
    fn rss_frame_uses_common_clock_and_rejects_stale_future_regressed_or_false_subtotal() {
        let frame = resource_frame();
        assert!(
            frame
                .validate(&frame.run, 1, 10 + RESOURCE_FRESH_NS)
                .is_ok()
        );
        assert!(
            frame
                .validate(&frame.run, 1, 11 + RESOURCE_FRESH_NS)
                .is_err()
        );
        // Retain the exact stale input that failed against the forwarding scaffold.
        assert!(
            frame
                .validate(&frame.run, 1, 21 + RESOURCE_FRESH_NS)
                .is_err()
        );
        assert!(frame.validate(&frame.run, 1, 19).is_err());
        assert!(frame.validate(&frame.run, 2, 20).is_err());
        let mut replaced = frame.run.clone();
        replaced.worker_pid += 1;
        assert!(frame.validate(&replaced, 1, 20).is_err());
        let mut false_total = frame.clone();
        false_total.child_bytes = Some(1);
        assert!(false_total.validate(&frame.run, 1, 20).is_err());
    }
    #[test]
    fn rss_empty_child_frame_still_requires_both_supervisor_samples() {
        let frame = resource_frame();
        let totals = frame.validate(&frame.run, 0, 20).unwrap();
        assert_eq!(totals.total, 300);
        assert_eq!(totals.children, 0);
        let mut empty = frame.clone();
        empty.expected.clear();
        empty.observations.clear();
        empty.total_bytes = Some(0);
        empty.child_bytes = Some(0);
        assert!(empty.validate(&frame.run, 0, 20).is_err());
        let mut missing = frame.clone();
        missing.observations[0].bytes = None;
        missing.observations[0].missing = Some("controller unavailable".into());
        assert!(missing.validate(&frame.run, 0, 20).is_err());
    }
    #[test]
    fn rss_stop_is_bound_to_owned_run_and_single_cleanup_budget() {
        let frame = resource_frame();
        let stop = ResourceStop {
            schema: 1,
            run: frame.run.clone(),
            requested_ns: 100,
            deadline_ns: 100 + SHUTDOWN_SECONDS * 1_000_000_000,
            reason: "aggregate exceeded".into(),
        };
        assert!(stop.validate(&frame.run, 101, stop.deadline_ns).is_ok());
        let mut prolonged = stop.clone();
        prolonged.deadline_ns += 1;
        assert!(
            prolonged
                .validate(&frame.run, 101, prolonged.deadline_ns)
                .is_err()
        );
        assert!(stop.validate(&frame.run, 99, stop.deadline_ns).is_err());
        assert!(
            stop.validate(&frame.run, 101, stop.deadline_ns - 1)
                .is_err()
        );
        let mut replaced = frame.run;
        replaced.worker_pid += 1;
        assert!(stop.validate(&replaced, 101, stop.deadline_ns).is_err());
    }
    #[test]
    fn rss_outer_recomposition_replaces_supervisors_without_double_counting() {
        let mut frame = resource_frame();
        let child = identity("catalog-apply", 30, 1);
        frame.expected.push(child.clone());
        frame.observations.push(observation(child, Some(40)));
        frame.total_bytes = Some(340);
        frame.child_bytes = Some(40);
        frame.max_total_bytes = 340;
        let fresh = [
            observation(frame.expected[0].clone(), Some(1000)),
            observation(frame.expected[1].clone(), Some(2000)),
        ];
        let (totals, _) = frame.recompose(&frame.run, 0, 20, fresh.clone()).unwrap();
        assert_eq!(
            totals,
            RssTotals {
                total: 3040,
                children: 40
            }
        );
        let mut missing = fresh.clone();
        missing[0].bytes = None;
        missing[0].missing = Some("missing controller".into());
        assert!(frame.recompose(&frame.run, 0, 20, missing).is_err());
        let mut replaced = fresh;
        replaced[1].identity.pid = 99;
        assert!(frame.recompose(&frame.run, 0, 20, replaced).is_err());
        let breach = [
            observation(frame.expected[0].clone(), Some(RSS_CAP - 20)),
            observation(frame.expected[1].clone(), Some(1)),
        ];
        let (over, captured) = frame
            .recompose_observed(&frame.run, 0, 20, breach.clone())
            .unwrap();
        assert_eq!(over.total, RSS_CAP + 21);
        assert!(rss_caps(&over, &captured).is_err());
        assert!(frame.recompose(&frame.run, 0, 20, breach).is_err());
    }

    #[test]
    fn deadline_regression_positive_initial_frame_is_late_at_or_after_limit() {
        let frame = resource_frame();
        assert!(frame.validate_initial(&frame.run, 29, 30).is_ok());
        assert!(frame.validate_initial(&frame.run, 30, 30).is_err());
        assert!(frame.validate_initial(&frame.run, 31, 30).is_err());
        // The ordinary freshness validator accepts these frames: only initial readiness expires.
        assert!(frame.validate(&frame.run, 0, 31).is_ok());
    }
    #[test]
    fn deadline_regression_clock_bridge_preserves_earlier_anchor_and_exclusive_limit() {
        let anchor = Instant::now() - Duration::from_secs(1);
        assert_eq!(
            anchored_deadline(anchor, 200, 300).unwrap(),
            anchor + Duration::from_nanos(100)
        );
        assert!(anchored_deadline(anchor, 300, 300).is_err());
        assert!(anchored_deadline(anchor, 301, 300).is_err());
    }
    fn bank() -> Bank {
        Bank {
            partition: partition(0),
            drive: drive(0),
            local_hits: 20,
            peer_hits: 0,
            backing_fetches: 1,
            hit_bytes: 20 * BLOCK_BYTES as u64,
            cache_errors: 0,
            maintenance_dropped: 0,
        }
    }
    #[test]
    fn ram_oracle_is_sensitive_to_backing_amplification_and_peer_masking() {
        assert!(bank().expect(20, 0, 1, 20 * BLOCK_BYTES as u64).is_ok());
        let mut amplified = bank();
        amplified.backing_fetches = 21;
        assert!(amplified.expect(20, 0, 1, 20 * BLOCK_BYTES as u64).is_err());
        let mut masked = bank();
        masked.peer_hits = 1;
        assert!(masked.expect(20, 0, 1, 20 * BLOCK_BYTES as u64).is_err());
    }
    #[test]
    fn shutdown_parser_rejects_duplicate_missing_unknown_and_partial_rows() {
        let lines = (0..NODES)
            .map(|n| {
                let mut b = bank();
                b.partition = partition(n);
                b.drive = drive(n);
                format!("blob_cache {}\n", serde_json::to_string(&b).unwrap())
            })
            .collect::<String>();
        assert_eq!(parse_banks(&lines).unwrap().len(), NODES);
        assert!(parse_banks(&(lines.clone() + &lines)).is_err());
        assert!(
            parse_banks(
                lines
                    .lines()
                    .take(9)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .as_str()
            )
            .is_err()
        );
        assert!(parse_banks(&(lines.clone() + "blob_cache {\"partition\":")).is_err());
        let mut unknown = bank();
        unknown.drive = "unregistered".into();
        assert!(
            parse_banks(&format!(
                "{lines}blob_cache {}\n",
                serde_json::to_string(&unknown).unwrap()
            ))
            .is_err()
        );
    }
    #[test]
    fn output_caps_and_unterminated_line_are_not_complete_evidence() {
        assert!(validate_output(b"small\n", true).is_ok());
        assert!(validate_output(b"small", true).is_err());
        assert!(validate_output(b"small", false).is_ok());
        assert!(validate_output(&vec![b'x'; LINE_CAP + 1], false).is_err());
        assert!(validate_output(&[0xff, b'\n'], true).is_err());
    }
    #[test]
    fn deadline_clips_to_single_parent_budget() {
        let now = Instant::now();
        let parent = now + Duration::from_secs(2);
        assert_eq!(clipped(now, parent, REQUEST_SECONDS).unwrap(), parent);
        assert!(clipped(parent, parent, PHASE_SECONDS).is_err());
        assert_eq!(
            SETUP_SECONDS + WORK_SECONDS + SHUTDOWN_SECONDS + AUDIT_SECONDS,
            2525
        );
    }
    #[test]
    fn unknown_commit_is_never_an_ack_or_permission_to_replay() {
        let pending = Submission {
            attempts: 1,
            acknowledged: false,
            independently_committed: true,
        };
        assert_eq!(pending.classify().unwrap(), "committed_unknown");
        assert!(
            Submission {
                attempts: 2,
                ..pending
            }
            .classify()
            .is_err()
        );
        assert_eq!(
            Submission {
                attempts: 1,
                acknowledged: true,
                independently_committed: true
            }
            .classify()
            .unwrap(),
            "signed_ack"
        );
        assert!(
            Submission {
                attempts: 1,
                acknowledged: true,
                independently_committed: false
            }
            .classify()
            .is_err()
        );
    }
    #[test]
    fn reap_oracle_rejects_forced_cleanup_and_cross_generation_identity() {
        let p = ProcessReceipt {
            node: "node-0".into(),
            generation: 1,
            pid: 123,
            launch: LaunchBinding {
                config_path: "/private/config.json".into(),
                config_sha256: "a".repeat(64),
                cli_binary_path: "/private/mount-rs".into(),
                cli_binary_sha256: "b".repeat(64),
                catalog_path: "/private/catalog.sqlite".into(),
                apply_expected_revision: None,
                apply_document_sha256: None,
                catalog_at_launch: Some(CatalogIdentity {
                    device: 1,
                    inode: 2,
                    revision: 1,
                    document_sha256: "c".repeat(64),
                }),
                catalog_after_completion: Some(CatalogIdentity {
                    device: 1,
                    inode: 2,
                    revision: 1,
                    document_sha256: "c".repeat(64),
                }),
            },
            role: "server".into(),
            reaped: true,
            success: true,
            forced: false,
            sockets_reusable: true,
            disk_lock_reusable: true,
            max_rss_bytes: 1,
            rss_samples: 1,
            rss_first_ns: Some(1),
            rss_last_ns: Some(1),
            stdout_bytes: 1,
            stderr_bytes: 1,
        };
        assert!(p.qualify("node-0", 1, 123).is_ok());
        assert!(p.qualify("node-0", 2, 123).is_err());
        assert!(
            ProcessReceipt {
                forced: true,
                ..p.clone()
            }
            .qualify("node-0", 1, 123)
            .is_err()
        );
        let mut missing_binding = p.clone();
        missing_binding.launch.config_sha256.clear();
        assert!(missing_binding.qualify("node-0", 1, 123).is_err());
        let mut replaced_catalog = p;
        replaced_catalog
            .launch
            .catalog_after_completion
            .as_mut()
            .unwrap()
            .inode = 99;
        assert!(replaced_catalog.qualify("node-0", 1, 123).is_err());
    }
    #[test]
    fn eviction_requires_admission_before_removal_and_replacement() {
        assert!(eviction_oracle(true, false, true).is_ok());
        assert!(eviction_oracle(false, false, true).is_err());
        assert!(eviction_oracle(true, true, true).is_err());
        assert!(eviction_oracle(true, false, false).is_err());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct RssIdentity {
    pub role: String,
    pub node: String,
    pub pid: u32,
    pub generation: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RssObservation {
    pub identity: RssIdentity,
    pub started_ns: u64,
    pub finished_ns: u64,
    pub bytes: Option<u64>,
    pub missing: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ResourceRun {
    pub root: String,
    pub controller_pid: u32,
    pub worker_pid: u32,
    pub group: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceFrame {
    pub schema: u32,
    pub run: ResourceRun,
    pub sequence: u64,
    pub started_ns: u64,
    pub finished_ns: u64,
    pub expected: Vec<RssIdentity>,
    pub observations: Vec<RssObservation>,
    pub total_bytes: Option<u64>,
    pub child_bytes: Option<u64>,
    pub max_total_bytes: u64,
    pub error: Option<String>,
    pub terminal: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub struct RssTotals {
    pub total: u64,
    pub children: u64,
}
pub fn rss_totals(expected: &[RssIdentity], observations: &[RssObservation]) -> Result<RssTotals> {
    let totals = rss_observed_totals(expected, observations)?;
    rss_caps(&totals, observations)?;
    Ok(totals)
}
pub fn rss_caps(totals: &RssTotals, observations: &[RssObservation]) -> Result<()> {
    if totals.total >= RSS_CAP
        || observations
            .iter()
            .any(|v| v.bytes.is_some_and(|bytes| bytes >= RSS_CAP))
    {
        return Err(format!(
            "observed RSS per-PID or aggregate cap exceeded (total={}, cap={RSS_CAP})",
            totals.total
        ));
    }
    Ok(())
}
fn rss_observed_totals(
    expected: &[RssIdentity],
    observations: &[RssObservation],
) -> Result<RssTotals> {
    let identities = expected.iter().collect::<BTreeSet<_>>();
    let pids = expected.iter().map(|v| v.pid).collect::<BTreeSet<_>>();
    if expected.len() > RESOURCE_MAX_IDENTITIES
        || identities.len() != expected.len()
        || pids.len() != expected.len()
        || pids.contains(&0)
        || observations.len() != expected.len()
    {
        return Err("missing, duplicate or excessive RSS roster".into());
    }
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    let mut children = 0u64;
    for value in observations {
        if !identities.contains(&value.identity) || !seen.insert(&value.identity) {
            return Err("duplicate, foreign or replaced RSS identity".into());
        }
        if value.started_ns > value.finished_ns {
            return Err("invalid RSS capture envelope".into());
        }
        let bytes = value.bytes.ok_or("RSS snapshot unavailable")?;
        if value.missing.is_some() {
            return Err("RSS snapshot has missing reason".into());
        }
        total = total.checked_add(bytes).ok_or("RSS sum overflow")?;
        if !matches!(value.identity.role.as_str(), "controller" | "worker") {
            children = children
                .checked_add(bytes)
                .ok_or("RSS child sum overflow")?;
        }
    }
    Ok(RssTotals { total, children })
}
impl ResourceFrame {
    pub fn validate_initial(&self, run: &ResourceRun, now: u64, initial_limit: u64) -> Result<()> {
        if now >= initial_limit {
            return Err("initial report observation deadline exhausted".into());
        }
        self.validate(run, 0, now)?;
        if self.expected.len() != 2 || self.terminal {
            return Err("initial frame not empty-child".into());
        }
        Ok(())
    }
    pub fn recompose(
        &self,
        run: &ResourceRun,
        sequence: u64,
        now: u64,
        supervisors: [RssObservation; 2],
    ) -> Result<(RssTotals, Vec<RssObservation>)> {
        let (totals, observations) = self.recompose_observed(run, sequence, now, supervisors)?;
        rss_caps(&totals, &observations)?;
        Ok((totals, observations))
    }
    pub fn recompose_observed(
        &self,
        run: &ResourceRun,
        sequence: u64,
        now: u64,
        supervisors: [RssObservation; 2],
    ) -> Result<(RssTotals, Vec<RssObservation>)> {
        self.validate(run, sequence, now)?;
        if supervisors.iter().any(|v| {
            v.started_ns > v.finished_ns
                || v.finished_ns > now
                || now.saturating_sub(v.started_ns) > RESOURCE_FRESH_NS
        }) {
            return Err("outer RSS sample missing, stale or future".into());
        }
        let mut expected = self
            .expected
            .iter()
            .filter(|v| matches!(v.role.as_str(), "controller" | "worker"))
            .cloned()
            .collect::<Vec<_>>();
        expected.extend(
            self.expected
                .iter()
                .filter(|v| matches!(v.role.as_str(), "server" | "catalog-apply"))
                .cloned(),
        );
        let mut observations = Vec::from(supervisors);
        observations.extend(
            self.observations
                .iter()
                .filter(|v| matches!(v.identity.role.as_str(), "server" | "catalog-apply"))
                .cloned(),
        );
        Ok((rss_observed_totals(&expected, &observations)?, observations))
    }
    pub fn validate(&self, run: &ResourceRun, sequence: u64, now: u64) -> Result<RssTotals> {
        if self.schema != 1
            || &self.run != run
            || self.run.controller_pid == 0
            || self.run.worker_pid == 0
            || self.run.controller_pid == self.run.worker_pid
            || self.run.group != self.run.worker_pid
            || !std::path::Path::new(&self.run.root).is_absolute()
            || self.sequence == 0
            || self.sequence < sequence
            || self.error.is_some()
            || self.started_ns > self.finished_ns
            || self.finished_ns > now
            || now - self.started_ns > RESOURCE_FRESH_NS
            || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() as u64 > RESOURCE_CAP
        {
            return Err("RSS frame missing, stale, foreign, regressed or incomplete".into());
        }
        for (role, pid) in [
            ("controller", run.controller_pid),
            ("worker", run.worker_pid),
        ] {
            let wanted = RssIdentity {
                role: role.into(),
                node: role.into(),
                pid,
                generation: 0,
            };
            if self.expected.iter().filter(|v| **v == wanted).count() != 1 {
                return Err("RSS frame lacks exact supervisor identity".into());
            }
        }
        for id in &self.expected {
            if !matches!(
                id.role.as_str(),
                "controller" | "worker" | "server" | "catalog-apply"
            ) || (matches!(id.role.as_str(), "server" | "catalog-apply") && id.generation == 0)
            {
                return Err("invalid owned RSS member role/generation".into());
            }
            if (id.role == "controller"
                && (id.pid != run.controller_pid || id.node != "controller" || id.generation != 0))
                || (id.role == "worker"
                    && (id.pid != run.worker_pid || id.node != "worker" || id.generation != 0))
            {
                return Err("foreign supervisor RSS identity".into());
            }
        }
        if self
            .observations
            .iter()
            .any(|v| v.started_ns < self.started_ns || v.finished_ns > self.finished_ns)
        {
            return Err("RSS sample outside publication envelope".into());
        }
        let totals = rss_totals(&self.expected, &self.observations)?;
        if self.total_bytes != Some(totals.total)
            || self.child_bytes != Some(totals.children)
            || self.max_total_bytes < totals.total
            || (self.terminal && self.expected.len() != 2)
        {
            return Err("RSS frame subtotal or terminal roster mismatch".into());
        }
        Ok(totals)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceStop {
    pub schema: u32,
    pub run: ResourceRun,
    pub requested_ns: u64,
    pub deadline_ns: u64,
    pub reason: String,
}
impl ResourceStop {
    pub fn validate(&self, run: &ResourceRun, now: u64, outer_ns: u64) -> Result<()> {
        if self.schema != 1
            || &self.run != run
            || self.requested_ns > now
            || self.deadline_ns < self.requested_ns
            || self.deadline_ns > outer_ns
            || self.deadline_ns - self.requested_ns > SHUTDOWN_SECONDS * 1_000_000_000
            || self.reason.is_empty()
            || self.reason.len() > LINE_CAP
        {
            return Err("invalid owned resource stop request".into());
        }
        Ok(())
    }
}

pub fn partition(n: usize) -> String {
    format!("partition-{}", n / 2)
}
pub fn drive(n: usize) -> String {
    format!("drive-{n}")
}
/// Anchor captured before the common-clock read makes conversion conservative on a sampling stall.
pub fn anchored_deadline(anchor: Instant, common_now: u64, limit: u64) -> Result<Instant> {
    let remaining = limit
        .checked_sub(common_now)
        .filter(|v| *v > 0)
        .ok_or("common-clock deadline exhausted")?;
    anchor
        .checked_add(Duration::from_nanos(remaining))
        .ok_or_else(|| "deadline overflow".into())
}
pub fn clipped(now: Instant, parent: Instant, seconds: u64) -> Result<Instant> {
    if now >= parent {
        return Err("parent deadline exhausted".into());
    }
    Ok(parent.min(now + Duration::from_secs(seconds)))
}
pub fn validate_output(bytes: &[u8], terminal: bool) -> Result<&str> {
    if bytes.len() as u64 > FILE_CAP {
        return Err("cooperative file cap exceeded".into());
    }
    if bytes
        .split(|b| *b == b'\n')
        .any(|line| line.len() > LINE_CAP)
    {
        return Err("line cap exceeded".into());
    }
    if terminal && !bytes.is_empty() && bytes.last() != Some(&b'\n') {
        return Err("terminal output has an incomplete line".into());
    }
    std::str::from_utf8(bytes).map_err(|_| "output is not UTF-8".into())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bank {
    pub partition: String,
    pub drive: String,
    pub local_hits: u64,
    pub peer_hits: u64,
    pub backing_fetches: u64,
    pub hit_bytes: u64,
    pub cache_errors: u64,
    pub maintenance_dropped: u64,
}
impl Bank {
    pub fn expect(&self, local: u64, peer: u64, backing: u64, bytes: u64) -> Result<()> {
        if (
            self.local_hits,
            self.peer_hits,
            self.backing_fetches,
            self.hit_bytes,
        ) != (local, peer, backing, bytes)
        {
            return Err(format!("generation logical bank mismatch: {self:?}"));
        }
        Ok(())
    }
}
pub fn parse_banks(output: &str) -> Result<BTreeMap<String, Bank>> {
    let mut rows = BTreeMap::new();
    for line in output.lines() {
        let Some(json) = line.strip_prefix("blob_cache ") else {
            continue;
        };
        let row: Bank =
            serde_json::from_str(json).map_err(|e| format!("incomplete cache bank: {e}"))?;
        let n = (0..NODES)
            .find(|n| partition(*n) == row.partition && drive(*n) == row.drive)
            .ok_or("unregistered cache scope")?;
        if rows.insert(drive(n), row).is_some() {
            return Err("duplicate cache bank".into());
        }
    }
    if rows.len() != NODES {
        return Err("missing generation cache bank".into());
    }
    Ok(rows)
}
pub fn eviction_oracle(admitted: bool, old_exists: bool, replacement: bool) -> Result<()> {
    if !admitted || old_exists || !replacement {
        return Err("eviction attribution incomplete".into());
    }
    Ok(())
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Submission {
    pub attempts: u64,
    pub acknowledged: bool,
    pub independently_committed: bool,
}
impl Submission {
    pub fn classify(self) -> Result<&'static str> {
        if self.attempts != 1 {
            return Err("submission replay or missing submission".into());
        }
        match (self.acknowledged, self.independently_committed) {
            (true, true) => Ok("signed_ack"),
            (false, true) => Ok("committed_unknown"),
            (false, false) => Ok("unresolved"),
            (true, false) => Err("ACK lacks independent durable oracle".into()),
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct CatalogIdentity {
    pub device: u64,
    pub inode: u64,
    pub revision: u64,
    pub document_sha256: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct LaunchBinding {
    pub config_path: String,
    pub config_sha256: String,
    pub cli_binary_path: String,
    pub cli_binary_sha256: String,
    pub catalog_path: String,
    pub catalog_at_launch: Option<CatalogIdentity>,
    pub catalog_after_completion: Option<CatalogIdentity>,
    pub apply_expected_revision: Option<u64>,
    pub apply_document_sha256: Option<String>,
}
impl LaunchBinding {
    fn qualify(&self, role: &str) -> Result<()> {
        if self.config_path.is_empty()
            || self.config_sha256.len() != 64
            || self.cli_binary_path.is_empty()
            || self.cli_binary_sha256.len() != 64
            || self.catalog_path.is_empty()
        {
            return Err("exact launch binding incomplete".into());
        }
        let after = self
            .catalog_after_completion
            .as_ref()
            .ok_or("completed catalog identity missing")?;
        if after.document_sha256.len() != 64 {
            return Err("catalog document binding incomplete".into());
        }
        if let Some(before) = &self.catalog_at_launch {
            if before.device != after.device || before.inode != after.inode {
                return Err("catalog physical identity changed within generation".into());
            }
        } else if role == "server" {
            return Err("server launch catalog identity missing".into());
        }
        if role == "catalog-apply" {
            let expected = self
                .apply_expected_revision
                .ok_or("catalog CAS launch revision missing")?;
            if expected.checked_add(1) != Some(after.revision)
                || self.apply_document_sha256.as_ref().map(String::len) != Some(64)
            {
                return Err("catalog CAS binding incomplete".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct ProcessReceipt {
    pub launch: LaunchBinding,
    pub node: String,
    pub generation: u64,
    pub pid: u32,
    pub role: String,
    pub reaped: bool,
    pub success: bool,
    pub forced: bool,
    pub sockets_reusable: bool,
    pub disk_lock_reusable: bool,
    pub max_rss_bytes: u64,
    pub rss_samples: u64,
    pub rss_first_ns: Option<u64>,
    pub rss_last_ns: Option<u64>,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
}
impl ProcessReceipt {
    pub fn qualify(&self, node: &str, generation: u64, pid: u32) -> Result<()> {
        self.launch.qualify(&self.role)?;
        if self.node != node
            || self.generation != generation
            || self.pid != pid
            || !self.reaped
            || !self.success
            || self.forced
            || !self.sockets_reusable
            || !self.disk_lock_reusable
            || self.max_rss_bytes >= RSS_CAP
            || self.rss_samples == 0
            || self.rss_first_ns.is_none()
            || self.rss_last_ns.is_none()
        {
            return Err("process generation lifecycle incomplete".into());
        }
        Ok(())
    }
}
