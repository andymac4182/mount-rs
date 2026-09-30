//! Lossless bounded packs of borrowed final expected state; fixture-only.
use super::{
    file_digest, metrics,
    state::{Expected, LedgerView},
};
use serde::Serialize;
use std::{path::Path, time::Instant};

pub const PACK_DRIVES: usize = 32;
pub const PACK_SCHEMA: &str = "mount-rs-expected-ledger-pack-v1";
pub const INVENTORY_SCHEMA: &str = "mount-rs-expected-ledger-inventory-v1";

#[derive(Serialize)]
struct PackView<'a> {
    schema: &'static str,
    pack: usize,
    first_drive: usize,
    ledgers: Vec<LedgerView<'a>>,
}
#[derive(Serialize)]
pub struct PackReceipt {
    pub pack: usize,
    pub first_drive: usize,
    pub count: usize,
    pub file: String,
    pub sha256: String,
}

pub fn publish_pack(
    output: &Path,
    pack: usize,
    ledgers: &[&Expected],
    deadline: Instant,
) -> Result<PackReceipt, String> {
    let first_drive = pack
        .checked_mul(PACK_DRIVES)
        .ok_or("ledger pack index overflow")?;
    if ledgers.is_empty()
        || ledgers.len() > PACK_DRIVES
        || first_drive
            .checked_add(ledgers.len())
            .is_none_or(|end| end > 10_000)
        || ledgers
            .iter()
            .enumerate()
            .any(|(offset, ledger)| ledger.drive != first_drive + offset)
    {
        return Err("ledger pack roster is not ordered contiguous bounded geometry".into());
    }
    if Instant::now() > deadline {
        return Err("expected-state terminal receipt deadline".into());
    }
    let file = format!("expected/pack-{pack:05}.json.gz");
    let path = output.join(&file);
    let view = PackView {
        schema: PACK_SCHEMA,
        pack,
        first_drive,
        ledgers: ledgers
            .iter()
            .map(|ledger| ledger.borrowed_snapshot())
            .collect(),
    };
    metrics::publish_immutable(&path, &view)?;
    if Instant::now() > deadline {
        return Err("expected-state receipt exceeded30s after write".into());
    }
    // The stored-byte buffer is bounded by the existing encoded writer cap.
    let sha256 = file_digest(&path)?;
    if Instant::now() > deadline {
        return Err("expected-state receipt exceeded30s after hash".into());
    }
    Ok(PackReceipt {
        pack,
        first_drive,
        count: ledgers.len(),
        file,
        sha256,
    })
}

#[cfg(test)]
mod tests {
    use super::super::fixture::FileProfile;
    use super::*;
    use std::time::Duration;

    fn ledger(drive: usize, mutate: bool) -> Expected {
        let mut value = Expected::empty(drive, 1000);
        for identity in 0..1000 {
            let name = format!("mixed-{identity}");
            value.create(name.clone(), identity);
            for block in 0..FileProfile::Mixed.size(identity) / 4096 {
                let generation = if mutate && (identity + block).is_multiple_of(11) {
                    u64::MAX
                } else if mutate {
                    (1u64 << 53) + 7
                } else {
                    0
                };
                value.write(&name, block, generation);
            }
        }
        if mutate {
            value.write("mixed-0", 1, u64::MAX);
            value.rename("mixed-1", "renamed-é".into());
            value.truncate("mixed-990", 4096);
            value.delete("mixed-2");
        }
        value
    }

    #[test]
    fn borrowed_ledger_packs_round_trip_original_dense_sparse_and_mutated_state() {
        for count in [31usize, 32, 33] {
            let directory = tempfile::tempdir().unwrap();
            std::fs::create_dir(directory.path().join("expected")).unwrap();
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut seen = 0;
            for pack in 0..count.div_ceil(PACK_DRIVES) {
                let first = pack * PACK_DRIVES;
                let expected = (first..(first + PACK_DRIVES).min(count))
                    .map(|drive| ledger(drive, true))
                    .collect::<Vec<_>>();
                let references = expected.iter().collect::<Vec<_>>();
                let receipt = publish_pack(directory.path(), pack, &references, deadline).unwrap();
                let path = directory.path().join(&receipt.file);
                assert_eq!(file_digest(&path).unwrap(), receipt.sha256);
                let decoded = metrics::read_compressed(&path).unwrap();
                assert_eq!(decoded["schema"], PACK_SCHEMA);
                assert_eq!(decoded["pack"], pack);
                assert_eq!(decoded["first_drive"], first);
                let values = decoded["ledgers"].as_array().unwrap();
                assert_eq!(values.len(), expected.len());
                for (value, expected) in values.iter().zip(&expected) {
                    assert_eq!(value, &expected.snapshot());
                    assert_eq!(value["generations"].as_array().unwrap().len(), 1534);
                    assert_eq!(value["files"]["mixed-0"]["changed"]["1"], u64::MAX);
                    assert_eq!(value["files"]["renamed-é"]["identity"], 1);
                    seen += 1;
                }
                // Immutable publication cannot replace even identical pack content.
                assert!(publish_pack(directory.path(), pack, &references, deadline).is_err());
            }
            assert_eq!(seen, count);
        }
    }

    #[test]
    fn ledger_pack_rejects_bad_roster_and_expired_original_deadline() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("expected")).unwrap();
        let ledger = Expected::empty(1, 1000);
        let deadline = Instant::now() + Duration::from_secs(30);
        assert!(publish_pack(directory.path(), 0, &[], deadline).is_err());
        assert!(publish_pack(directory.path(), 0, &[&ledger], deadline).is_err());
        let ledger = Expected::empty(0, 1000);
        let expired = Instant::now().checked_sub(Duration::from_secs(1)).unwrap();
        assert!(publish_pack(directory.path(), 0, &[&ledger], expired).is_err());
        assert!(
            !directory
                .path()
                .join("expected/pack-00000.json.gz")
                .exists()
        );
    }

    struct FullLedgerCosts {
        started: Instant,
        stage_started: Instant,
        stage: usize,
        totals: [Duration; 4],
        pack: usize,
        completed_packs: usize,
        completed_drives: usize,
    }
    impl FullLedgerCosts {
        fn new(started: Instant) -> Self {
            Self {
                started,
                stage_started: started,
                stage: 4,
                totals: [Duration::ZERO; 4],
                pack: 0,
                completed_packs: 0,
                completed_drives: 0,
            }
        }
        fn enter(&mut self, stage: usize) {
            let now = Instant::now();
            if self.stage < self.totals.len() {
                self.totals[self.stage] += now.duration_since(self.stage_started);
            }
            self.stage = stage;
            self.stage_started = now;
        }
        fn report(&mut self, outcome: &str) {
            let stage = [
                "construct",
                "write_hash",
                "decode",
                "equality",
                "between_packs",
            ][self.stage];
            self.enter(4);
            use std::io::Write as _;
            let report = serde_json::json!({
                "outcome":outcome,"active_stage":stage,"pack_index":self.pack,
                "completed_packs":self.completed_packs,"completed_drives":self.completed_drives,
                "elapsed_seconds":self.started.elapsed().as_secs_f64(),
                "construct_seconds":self.totals[0].as_secs_f64(),
                "write_hash_seconds":self.totals[1].as_secs_f64(),
                "decode_seconds":self.totals[2].as_secs_f64(),
                "equality_seconds":self.totals[3].as_secs_f64(),
                "deadline_seconds":30,"scope":"cumulative nonoverlapping fixture wall costs; includes failing active stage; no isolated CPU or storage attribution"
            });
            let _ = writeln!(std::io::stderr().lock(), "FULL_LEDGER_PHASE_COSTS {report}");
        }
    }
    impl Drop for FullLedgerCosts {
        fn drop(&mut self) {
            if std::thread::panicking() {
                self.report("panicking");
            }
        }
    }

    #[test]
    #[ignore = "explicit complete artifact corpus: 10000 Drives x 1000 files; not native capacity evidence"]
    fn ledger_packs_full_corpus_visits_all_records_with_one_original_30s_deadline() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("expected")).unwrap();
        let started = Instant::now();
        let deadline = started + Duration::from_secs(30);
        let mut costs = FullLedgerCosts::new(started);
        let mut files = 0u64;
        let mut bytes = 0u64;
        let mut drives = 0usize;
        for pack in 0..10_000usize.div_ceil(PACK_DRIVES) {
            costs.pack = pack;
            costs.enter(0);
            let first = pack * PACK_DRIVES;
            let expected = (first..(first + PACK_DRIVES).min(10_000))
                .map(|drive| ledger(drive, false))
                .collect::<Vec<_>>();
            let references = expected.iter().collect::<Vec<_>>();
            costs.enter(1);
            let receipt = publish_pack(directory.path(), pack, &references, deadline).unwrap();
            costs.enter(2);
            let decoded = metrics::read_compressed(&directory.path().join(&receipt.file)).unwrap();
            costs.enter(3);
            for (value, expected) in decoded["ledgers"].as_array().unwrap().iter().zip(&expected) {
                assert_eq!(value, &expected.snapshot());
                assert_eq!(value["drive"], drives);
                assert_eq!(value["generations"].as_array().unwrap().len(), 1534);
                let entries = value["files"].as_object().unwrap();
                files += entries.len() as u64;
                bytes += entries
                    .values()
                    .map(|file| file["length"].as_u64().unwrap())
                    .sum::<u64>();
                drives += 1;
                costs.completed_drives = drives;
            }
            assert!(Instant::now() <= deadline);
            costs.completed_packs += 1;
            costs.enter(4);
        }
        assert_eq!((drives, files, bytes), (10_000, 10_000_000, 62_832_640_000));
        assert_eq!(
            std::fs::read_dir(directory.path().join("expected"))
                .unwrap()
                .count(),
            313
        );
        costs.report("complete");
        assert!(
            Instant::now() <= deadline,
            "single original expected-state deadline after complete reporting"
        );
    }
}
