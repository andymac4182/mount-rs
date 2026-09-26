use super::fixture::{FileProfile, GenerationLedger, oracle_block};
use serde::Serialize;
use std::collections::BTreeMap;

/// Mutations become expected state only after an exact successful response.
/// The initial ledger remains compact; sparse entries represent changed lengths,
/// renamed files and appended blocks. File identity never follows its name.
pub struct Expected {
    pub drive: usize,
    pub files: BTreeMap<String, FileState>,
    ledger: GenerationLedger,
}
#[derive(Clone, Serialize)]
pub struct FileState {
    pub identity: usize,
    pub length: usize,
    pub changed: BTreeMap<usize, u64>,
}
impl Expected {
    pub fn empty(drive: usize, files: usize) -> Self {
        Self {
            drive,
            files: BTreeMap::new(),
            ledger: GenerationLedger::new(FileProfile::Mixed, files),
        }
    }
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"drive":self.drive,"files":self.files,"generations":self.ledger.generations,"oracle":"tuple-seeded4096-byte blocks; initial generation0"})
    }
    pub fn create(&mut self, name: String, identity: usize) {
        self.files.insert(
            name,
            FileState {
                identity,
                length: 0,
                changed: BTreeMap::new(),
            },
        );
    }
    pub fn write(&mut self, name: &str, block: usize, generation: u64) {
        let f = self
            .files
            .get_mut(name)
            .expect("acknowledged open precedes write");
        f.length = f.length.max((block + 1) * 4096);
        if block < FileProfile::Mixed.size(f.identity) / 4096 {
            self.ledger.commit(f.identity, block, generation);
        } else {
            f.changed.insert(block, generation);
        }
    }
    pub fn truncate(&mut self, name: &str, length: usize) {
        let f = self.files.get_mut(name).unwrap();
        f.length = length;
        f.changed.retain(|block, _| block * 4096 < length);
    }
    pub fn rename(&mut self, from: &str, to: String) {
        let f = self.files.remove(from).unwrap();
        self.files.insert(to, f);
    }
    pub fn delete(&mut self, name: &str) {
        self.files.remove(name);
    }
    pub fn bytes(&self, name: &str, block: usize) -> [u8; 4096] {
        let f = &self.files[name];
        let generation = f
            .changed
            .get(&block)
            .copied()
            .unwrap_or_else(|| self.ledger.generation(f.identity, block));
        oracle_block(
            (self.drive / 2) as u64,
            self.drive as u64,
            f.identity as u64,
            block as u64,
            generation,
        )
    }
}
#[derive(Default, Clone, Serialize)]
pub struct Counts {
    pub attempts: u64,
    pub acknowledged: u64,
    pub failed: u64,
    pub uncertain: u64,
    pub pending: Option<u64>,
    pub uncertain_request_ids: Vec<u64>,
    pub latency_histogram_log2_us: [u64; 32],
    pub next_id: u64,
    pub latency_us: u64,
    pub latency_max_us: u64,
}
impl Counts {
    pub fn begin(&mut self) -> u64 {
        assert!(self.pending.is_none());
        self.next_id += 1;
        self.attempts += 1;
        self.pending = Some(self.next_id);
        self.next_id
    }
    pub fn acknowledge(&mut self, id: u64) -> Result<(), String> {
        if self.pending != Some(id) {
            return Err("response identity mismatch".into());
        }
        self.pending = None;
        self.acknowledged += 1;
        Ok(())
    }
    pub fn uncertain(&mut self) {
        if let Some(id) = self.pending.take() {
            self.uncertain_request_ids.push(id);
            self.uncertain += 1;
        }
    }
    pub fn failed(&mut self) {
        if self.pending.take().is_some() {
            self.failed += 1;
        }
    }
}
#[test]
fn wrong_ack_and_unknown_outcome_never_commit() {
    let mut counts = Counts::default();
    let id = counts.begin();
    assert!(counts.acknowledge(id + 1).is_err());
    counts.uncertain();
    assert_eq!(counts.acknowledged, 0);
    assert_eq!(counts.uncertain, 1);
    assert_eq!(counts.begin(), id + 1);
}
#[test]
fn oracle_tracks_rename_append_truncate_and_detects_each_byte() {
    let mut expected = Expected::empty(4, 2);
    expected.create("mixed-0".into(), 0);
    expected.write("mixed-0", 0, 0);
    let initial = expected.bytes("mixed-0", 0);
    expected.write("mixed-0", 0, 1);
    let changed = expected.bytes("mixed-0", 0);
    assert_ne!(initial, changed);
    expected.write("mixed-0", 1, 2);
    expected.rename("mixed-0", "renamed".into());
    assert_eq!(expected.files["renamed"].length, 8192);
    assert_eq!(expected.bytes("renamed", 1), oracle_block(2, 4, 0, 1, 2));
    expected.truncate("renamed", 4096);
    assert_eq!(expected.files["renamed"].length, 4096);
    for offset in 0..4096 {
        let mut bad = changed;
        bad[offset] ^= 1;
        assert_ne!(expected.bytes("renamed", 0), bad);
    }
    expected.delete("renamed");
    assert!(expected.files.is_empty());
}
