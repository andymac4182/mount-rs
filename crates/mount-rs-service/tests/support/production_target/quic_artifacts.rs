//! Immutable per-cell retained client QUIC observations for this fixture only.
use super::{config::PATTERNS, metrics};
use serde::{Serialize, Serializer, ser::SerializeSeq};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, time::Instant};

pub const SCHEMA: &str = "mount-rs-controller-quic-v1";
pub const SCOPE: &str = "actual retained client connections; active workload plus idle liveness; snapshot observer outside active throughput interval; server transport retained separately in worker phase receipts";
const COUNTERS: [&str; 10] = [
    "tx_bytes",
    "rx_bytes",
    "tx_datagrams",
    "rx_datagrams",
    "tx_ios",
    "rx_ios",
    "lost_packets",
    "lost_bytes",
    "sent_packets",
    "congestion_events",
];
const METRIC_ID: [&str; 12] = [
    "pid",
    "role",
    "server",
    "controller_pid",
    "generation",
    "sequence",
    "phase",
    "boundary",
    "source_digest",
    "binary_digest",
    "catalog_digest",
    "backend_prefix",
];

fn shape(value: &Value, names: &[&str]) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or("QUIC artifact object unavailable")?;
    if object.len() != names.len() || names.iter().any(|name| !object.contains_key(*name)) {
        return Err("QUIC artifact object shape changed".into());
    }
    Ok(())
}

fn hash(value: &Value) -> Result<&str, String> {
    let text = value.as_str().ok_or("QUIC digest unavailable")?;
    if text.len() != 64
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("QUIC digest invalid".into());
    }
    Ok(text)
}

fn check_deadline(deadline: Instant) -> Result<(), String> {
    if Instant::now() >= deadline {
        return Err("QUIC artifact inherited phase deadline".into());
    }
    Ok(())
}

pub fn cell_identity(base: &Value, mode: &str, pattern: &str) -> Result<Value, String> {
    shape(base, &METRIC_ID)?;
    if !matches!(mode, "mostly_idle" | "all_active") || !PATTERNS.contains(&pattern) {
        return Err("QUIC cell label invalid".into());
    }
    let pid = base["pid"]
        .as_u64()
        .filter(|pid| (1..=u64::from(u32::MAX)).contains(pid))
        .ok_or("QUIC controller PID invalid")?;
    if base["controller_pid"].as_u64() != Some(pid)
        || base["role"] != "controller"
        || !base["server"].is_null()
        || base["generation"].as_u64().is_none()
        || base["sequence"]
            .as_u64()
            .filter(|sequence| *sequence > 0)
            .is_none()
        || base["phase"] != format!("{mode}/{pattern}")
        || base["boundary"] != "after_idle"
        || base["backend_prefix"]
            .as_str()
            .filter(|prefix| !prefix.is_empty())
            .is_none()
    {
        return Err("QUIC controller boundary identity invalid".into());
    }
    for field in ["source_digest", "binary_digest", "catalog_digest"] {
        hash(&base[field])?;
    }
    let mut identity = base.clone();
    identity["mode"] = json!(mode);
    identity["pattern"] = json!(pattern);
    Ok(identity)
}

fn rows(value: &Value, lanes: usize) -> Result<&[Value], String> {
    let rows = value
        .as_array()
        .ok_or("QUIC connection roster unavailable")?;
    if !(1..=10_000).contains(&lanes) || rows.len() != lanes {
        return Err("QUIC connection roster incomplete".into());
    }
    for (lane, row) in rows.iter().enumerate() {
        shape(row, &["lane", "quic"])?;
        if row["lane"].as_u64() != Some(lane as u64) {
            return Err("QUIC lane identity changed".into());
        }
        shape(&row["quic"], &COUNTERS)?;
        if COUNTERS
            .iter()
            .any(|field| row["quic"][*field].as_u64().is_none())
        {
            return Err("QUIC counter is not an exact unsigned u64 integer".into());
        }
    }
    Ok(rows)
}

struct BorrowedRows<'a> {
    rows: &'a [Value],
    deadline: Instant,
}
impl Serialize for BorrowedRows<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::Error;
        let mut sequence = serializer.serialize_seq(Some(self.rows.len()))?;
        for row in self.rows {
            check_deadline(self.deadline).map_err(S::Error::custom)?;
            sequence.serialize_element(row)?;
        }
        sequence.end()
    }
}
#[derive(Serialize)]
struct Envelope<'a> {
    schema: &'static str,
    identity: &'a Value,
    scope: &'static str,
    connections: BorrowedRows<'a>,
}

fn filename(identity: &Value) -> Result<String, String> {
    let generation = identity["generation"]
        .as_u64()
        .ok_or("QUIC generation invalid")?;
    let sequence = identity["sequence"]
        .as_u64()
        .ok_or("QUIC sequence invalid")?;
    Ok(format!("quic/g{generation}-s{sequence}.json.gz"))
}

pub fn publish_boundary(
    root: &Path,
    base: &Value,
    mode: &str,
    pattern: &str,
    connections: &Value,
    lanes: usize,
    deadline: Instant,
) -> Result<Value, String> {
    let span = metrics::observer().begin("receipt_publication");
    let result = (|| {
        check_deadline(deadline)?;
        let identity = cell_identity(base, mode, pattern)?;
        let rows = rows(connections, lanes)?;
        let name = filename(&identity)?;
        std::fs::create_dir_all(root.join("quic"))
            .map_err(|_| "QUIC artifact directory unavailable")?;
        let path = root.join(&name);
        // Reuse the bounded immutable publisher; retain every original row.
        metrics::publish_immutable(
            &path,
            &Envelope {
                schema: SCHEMA,
                identity: &identity,
                scope: SCOPE,
                connections: BorrowedRows { rows, deadline },
            },
        )?;
        check_deadline(deadline)?;
        let sha256 = super::file_digest(&path)?;
        check_deadline(deadline)?;
        Ok(json!({"scope":SCOPE,"artifact":{
            "schema":SCHEMA,"file":name,"sha256":sha256,"identity":identity,"lanes":lanes
        }}))
    })();
    span.finish(result.is_ok(), 0);
    result
}

fn retained(root: &Path, receipt: &Value, name: &str) -> Result<Value, String> {
    if receipt["file"] != name {
        return Err("QUIC retained path mismatch".into());
    }
    let path = root.join(name);
    let metadata =
        std::fs::symlink_metadata(&path).map_err(|_| "QUIC retained file unavailable")?;
    if !metadata.is_file() || metadata.len() > metrics::ENCODED_METRIC_LIMIT {
        return Err("QUIC retained regular-file bound".into());
    }
    if super::file_digest(&path)? != hash(&receipt["sha256"])? {
        return Err("QUIC retained hash mismatch".into());
    }
    metrics::read_compressed(&path)
}

pub fn read_boundary(
    root: &Path,
    stage: &Value,
    expected_base: &Value,
    lanes: usize,
) -> Result<Value, String> {
    let mode = stage["mode"]
        .as_str()
        .ok_or("QUIC stage mode unavailable")?;
    let pattern = stage["pattern"]
        .as_str()
        .ok_or("QUIC stage pattern unavailable")?;
    let expected = cell_identity(expected_base, mode, pattern)?;
    let boundary = &stage["controller_quic_boundary"];
    shape(boundary, &["scope", "artifact"])?;
    if boundary["scope"] != SCOPE || stage["connected_clients"].as_u64() != Some(lanes as u64) {
        return Err("QUIC terminal roster or scope mismatch".into());
    }
    let receipt = &boundary["artifact"];
    shape(receipt, &["schema", "file", "sha256", "identity", "lanes"])?;
    if receipt["schema"] != SCHEMA
        || receipt["identity"] != expected
        || receipt["lanes"].as_u64() != Some(lanes as u64)
    {
        return Err("QUIC reference identity mismatch".into());
    }
    let frame = retained(root, receipt, &filename(&expected)?)?;
    shape(&frame, &["schema", "identity", "scope", "connections"])?;
    if frame["schema"] != SCHEMA || frame["identity"] != expected || frame["scope"] != SCOPE {
        return Err("QUIC shard envelope identity mismatch".into());
    }
    rows(&frame["connections"], lanes)?;
    Ok(frame)
}

fn metric_frame(
    root: &Path,
    journal: &Value,
    row: &Value,
    catalog: Option<&Value>,
    backend: Option<&Value>,
) -> Result<Value, String> {
    if row["complete"] != true
        || (journal["metrics_required_for_outcome"] == true && row["metrics_complete"] != true)
    {
        return Err("QUIC metric boundary incomplete".into());
    }
    let generation = row["generation"]
        .as_u64()
        .ok_or("QUIC metric generation invalid")?;
    let sequence = row["sequence"]
        .as_u64()
        .ok_or("QUIC metric sequence invalid")?;
    let frame = retained(
        root,
        &row["controller"],
        &format!("metrics/g{generation}-s{sequence}.json.gz"),
    )?;
    let id = &frame["identity"];
    shape(id, &METRIC_ID)?;
    let expected = json!({
        "pid":journal["controller_resources"]["pid"],"role":"controller","server":null,
        "controller_pid":journal["controller_resources"]["pid"],"generation":generation,
        "sequence":sequence,"phase":row["phase"],"boundary":row["boundary"],
        "source_digest":journal["source"]["digest"],"binary_digest":journal["source"]["binary_sha256"],
        "catalog_digest":catalog.unwrap_or(&id["catalog_digest"]),
        "backend_prefix":backend.unwrap_or(&id["backend_prefix"])
    });
    if id != &expected
        || frame["capture_complete"] != true
        || (journal["metrics_required_for_outcome"] == true && frame["metrics_complete"] != true)
    {
        return Err("QUIC independent metric frame mismatch".into());
    }
    hash(&id["catalog_digest"])?;
    if id["backend_prefix"]
        .as_str()
        .filter(|prefix| !prefix.is_empty())
        .is_none()
    {
        return Err("QUIC backend identity invalid".into());
    }
    Ok(frame)
}

fn one_boundary<'a>(
    boundaries: &'a [Value],
    phase: &str,
    boundary: &str,
) -> Result<&'a Value, String> {
    let mut matches = boundaries
        .iter()
        .filter(|row| row["phase"] == phase && row["boundary"] == boundary);
    let row = matches
        .next()
        .ok_or("QUIC independent metric boundary missing")?;
    if matches.next().is_some() {
        return Err("QUIC independent metric boundary duplicated".into());
    }
    Ok(row)
}

pub fn verify_stages(root: &Path, journal: &Value) -> Result<(), String> {
    let lanes = journal["configuration"]["drives"]
        .as_u64()
        .filter(|lanes| (1..=10_000).contains(lanes))
        .ok_or("QUIC Drive geometry invalid")? as usize;
    let stages = journal["stages"]
        .as_array()
        .ok_or("QUIC cell roster missing")?;
    if stages.len() != PATTERNS.len() * 2 {
        return Err("QUIC cell roster incomplete".into());
    }
    let boundaries = journal["phase_metrics"]["boundaries"]
        .as_array()
        .ok_or("QUIC metric roster missing")?;
    let initial = metric_frame(
        root,
        journal,
        one_boundary(boundaries, "initial_fresh_oracle", "after")?,
        None,
        None,
    )?;
    let catalog = initial["identity"]["catalog_digest"].clone();
    let backend = initial["identity"]["backend_prefix"].clone();
    drop(initial);
    metric_frame(
        root,
        journal,
        one_boundary(boundaries, "final_fresh_oracle", "after")?,
        Some(&catalog),
        Some(&backend),
    )?;
    let mut paths = BTreeSet::new();
    for (cell, stage) in stages.iter().enumerate() {
        let mode = if cell < PATTERNS.len() {
            "mostly_idle"
        } else {
            "all_active"
        };
        let pattern = PATTERNS[cell % PATTERNS.len()];
        if stage["mode"] != mode || stage["pattern"] != pattern {
            return Err("QUIC cell labels or order changed".into());
        }
        let active = if mode == "mostly_idle" {
            (lanes / 100).clamp(1, 100)
        } else {
            lanes
        };
        if stage["configured_active_clients"].as_u64() != Some(active as u64) {
            return Err("QUIC configured client roster changed".into());
        }
        let sequences = stage["metric_sequences"]
            .as_array()
            .filter(|sequences| sequences.len() == 3)
            .ok_or("QUIC stage sequence roster invalid")?;
        let first = sequences[0]
            .as_u64()
            .filter(|sequence| *sequence > 0)
            .ok_or("QUIC stage sequence invalid")?;
        let phase = format!("{mode}/{pattern}");
        if boundaries
            .iter()
            .filter(|row| row["phase"] == phase)
            .count()
            != 3
        {
            return Err("QUIC controller boundary roster changed".into());
        }
        let mut generation = None;
        let mut after_idle = None;
        for (offset, boundary) in ["before_active", "after_active", "after_idle"]
            .into_iter()
            .enumerate()
        {
            let sequence = first
                .checked_add(offset as u64)
                .ok_or("QUIC stage sequence overflow")?;
            if sequences[offset].as_u64() != Some(sequence) {
                return Err("QUIC stage sequences are not consecutive".into());
            }
            let row = one_boundary(boundaries, &phase, boundary)?;
            if row["sequence"].as_u64() != Some(sequence) {
                return Err("QUIC stage sequence relabelled".into());
            }
            let frame = metric_frame(root, journal, row, Some(&catalog), Some(&backend))?;
            let current = frame["identity"]["generation"]
                .as_u64()
                .ok_or("QUIC stage generation invalid")?;
            if generation.is_some_and(|generation| generation != current) {
                return Err("QUIC stage generations changed".into());
            }
            generation = Some(current);
            if offset == 2 {
                after_idle = Some(frame["identity"].clone());
            }
        }
        let after_idle = after_idle.ok_or("QUIC after-idle frame missing")?;
        let shard = read_boundary(root, stage, &after_idle, lanes)?;
        if !paths.insert(filename(&shard["identity"])?) {
            return Err("QUIC shard reused across cells".into());
        }
        // Release this decoded cell before loading the next one.
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn identity() -> Value {
        json!({
            "pid":42,"controller_pid":42,"role":"controller","server":null,
            "generation":1,"sequence":23,"phase":format!("mostly_idle/{}",PATTERNS[0]),
            "boundary":"after_idle","source_digest":super::super::digest(b"quic-source"),
            "binary_digest":super::super::digest(b"quic-binary"),
            "catalog_digest":super::super::digest(b"quic-catalog"),"backend_prefix":"quic-control"
        })
    }
    fn stage(boundary: Value) -> Value {
        json!({"mode":"mostly_idle","pattern":PATTERNS[0],"connected_clients":3,
            "metric_sequences":[21,22,23],"controller_quic_boundary":boundary})
    }
    fn control() -> (tempfile::TempDir, Value, Value, Value) {
        let root = tempfile::tempdir().unwrap();
        let identity = identity();
        let mut network =
            super::super::resource_profile::artifact_fixture_connection_deltas(0, 3).unwrap();
        network[0]["quic"]["tx_bytes"] = json!(9_007_199_254_740_993_u64);
        network[1]["quic"]["rx_bytes"] = json!(u64::MAX);
        let boundary = publish_boundary(
            root.path(),
            &identity,
            "mostly_idle",
            PATTERNS[0],
            &network,
            3,
            Instant::now() + Duration::from_secs(30),
        )
        .unwrap();
        (root, identity, network, stage(boundary))
    }

    #[test]
    fn controller_quic_shard_preserves_exact_u64_rows_and_immutable_publication() {
        let (root, identity, network, stage) = control();
        let frame = read_boundary(root.path(), &stage, &identity, 3).unwrap();
        assert_eq!(frame["connections"], network);
        assert_eq!(
            frame["connections"][0]["quic"]["tx_bytes"].as_u64(),
            Some(9_007_199_254_740_993)
        );
        assert_eq!(
            frame["connections"][1]["quic"]["rx_bytes"].as_u64(),
            Some(u64::MAX)
        );
        let name = stage["controller_quic_boundary"]["artifact"]["file"]
            .as_str()
            .unwrap();
        let before = std::fs::read(root.path().join(name)).unwrap();
        assert!(
            publish_boundary(
                root.path(),
                &identity,
                "mostly_idle",
                PATTERNS[0],
                &network,
                3,
                Instant::now() + Duration::from_secs(30)
            )
            .is_err()
        );
        assert_eq!(std::fs::read(root.path().join(name)).unwrap(), before);
        assert_eq!(
            read_boundary(root.path(), &stage, &identity, 3).unwrap(),
            frame
        );
    }

    #[test]
    fn controller_quic_shard_rejects_missing_hash_relabel_and_inline_rosters() {
        let (root, identity, network, stage) = control();
        let empty = tempfile::tempdir().unwrap();
        assert!(read_boundary(empty.path(), &stage, &identity, 3).is_err());
        for kind in [
            "hash",
            "mode",
            "pattern",
            "lane_count",
            "path",
            "source",
            "generation",
            "sequence",
            "inline",
        ] {
            let mut changed = stage.clone();
            match kind {
                "hash" => {
                    changed["controller_quic_boundary"]["artifact"]["sha256"] =
                        json!("0".repeat(64))
                }
                "mode" => changed["mode"] = json!("all_active"),
                "pattern" => changed["pattern"] = json!(PATTERNS[1]),
                "lane_count" => changed["controller_quic_boundary"]["artifact"]["lanes"] = json!(2),
                "path" => {
                    changed["controller_quic_boundary"]["artifact"]["file"] =
                        json!("../outside.json.gz")
                }
                "source" => {
                    changed["controller_quic_boundary"]["artifact"]["identity"]["source_digest"] =
                        json!("0".repeat(64))
                }
                "generation" => {
                    changed["controller_quic_boundary"]["artifact"]["identity"]["generation"] =
                        json!(2)
                }
                "sequence" => {
                    changed["controller_quic_boundary"]["artifact"]["identity"]["sequence"] =
                        json!(24)
                }
                "inline" => changed["controller_quic_boundary"]["connections"] = network.clone(),
                _ => unreachable!(),
            }
            assert!(
                read_boundary(root.path(), &changed, &identity, 3).is_err(),
                "accepted {kind}"
            );
        }
    }

    #[test]
    fn controller_quic_shard_rejects_hash_valid_shape_and_integer_corruption() {
        let (root, identity, _, stage) = control();
        let original = read_boundary(root.path(), &stage, &identity, 3).unwrap();
        for kind in [
            "missing_lane",
            "duplicate_lane",
            "extra_row_field",
            "missing_counter",
            "extra_counter",
            "bool",
            "float",
            "string",
            "negative",
            "overflow",
            "source",
            "generation",
            "sequence",
            "mode",
            "pattern",
        ] {
            let mut frame = original.clone();
            match kind {
                "missing_lane" => {
                    frame["connections"].as_array_mut().unwrap().pop();
                }
                "duplicate_lane" => frame["connections"][1]["lane"] = json!(0),
                "extra_row_field" => frame["connections"][0]["extra"] = json!(0),
                "missing_counter" => {
                    frame["connections"][0]["quic"]
                        .as_object_mut()
                        .unwrap()
                        .remove("tx_bytes");
                }
                "extra_counter" => frame["connections"][0]["quic"]["extra"] = json!(0),
                "bool" => frame["connections"][0]["quic"]["tx_bytes"] = json!(true),
                "float" => frame["connections"][0]["quic"]["tx_bytes"] = json!(1.0),
                "string" => frame["connections"][0]["quic"]["tx_bytes"] = json!("1"),
                "negative" => frame["connections"][0]["quic"]["tx_bytes"] = json!(-1),
                "overflow" => {
                    frame["connections"][0]["quic"]["tx_bytes"] =
                        json!(18_446_744_073_709_551_616.0)
                }
                "source" => frame["identity"]["source_digest"] = json!("0".repeat(64)),
                "generation" => frame["identity"]["generation"] = json!(2),
                "sequence" => frame["identity"]["sequence"] = json!(24),
                "mode" => frame["identity"]["mode"] = json!("all_active"),
                "pattern" => frame["identity"]["pattern"] = json!(PATTERNS[1]),
                _ => unreachable!(),
            }
            let altered_root = tempfile::tempdir().unwrap();
            std::fs::create_dir(altered_root.path().join("quic")).unwrap();
            let name = filename(&original["identity"]).unwrap();
            let path = altered_root.path().join(&name);
            // Hash-valid corrupt artifacts test the reader rather than only the producer.
            metrics::publish_immutable(&path, &frame).unwrap();
            let mut altered_stage = stage.clone();
            altered_stage["controller_quic_boundary"]["artifact"]["sha256"] =
                json!(super::super::file_digest(&path).unwrap());
            assert!(
                read_boundary(altered_root.path(), &altered_stage, &identity, 3).is_err(),
                "accepted {kind}"
            );
        }
    }

    #[test]
    fn controller_quic_shard_inherits_expired_phase_deadline() {
        let root = tempfile::tempdir().unwrap();
        let network =
            super::super::resource_profile::artifact_fixture_connection_deltas(0, 3).unwrap();
        let error = publish_boundary(
            root.path(),
            &identity(),
            "mostly_idle",
            PATTERNS[0],
            &network,
            3,
            Instant::now(),
        )
        .unwrap_err();
        assert_eq!(error, "QUIC artifact inherited phase deadline");
        assert!(!root.path().join("quic").exists());
    }

    #[test]
    fn controller_quic_shard_rejects_hash_valid_duplicate_json_key() {
        use std::io::Write;
        let (root, identity, _, stage) = control();
        let original = read_boundary(root.path(), &stage, &identity, 3).unwrap();
        let plain = serde_json::to_string(&original).unwrap();
        assert_eq!(plain.match_indices("\"schema\":").count(), 1);
        let duplicate = plain.replacen("\"schema\":", "\"schema\":\"duplicate\",\"schema\":", 1);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(duplicate.as_bytes()).unwrap();
        let stored = encoder.finish().unwrap();
        let altered_root = tempfile::tempdir().unwrap();
        std::fs::create_dir(altered_root.path().join("quic")).unwrap();
        let name = filename(&original["identity"]).unwrap();
        let path = altered_root.path().join(name);
        // Deliberately inject corrupt raw bytes into an isolated fixture.
        std::fs::write(&path, stored).unwrap();
        let mut altered_stage = stage.clone();
        altered_stage["controller_quic_boundary"]["artifact"]["sha256"] =
            json!(super::super::file_digest(&path).unwrap());
        assert!(read_boundary(altered_root.path(), &altered_stage, &identity, 3).is_err());
    }
}
