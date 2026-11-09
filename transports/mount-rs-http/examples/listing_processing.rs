//! Sequential CPU diagnostic, not an HTTP or production latency qualification.
use mount_rs_core::{DirEntry, FileType};
use std::collections::BTreeSet;
use std::time::Instant;

fn summary(mut values: Vec<f64>) -> serde_json::Value {
    values.sort_by(f64::total_cmp);
    serde_json::json!({
        "samples":values.len(),
        "p50_ms":values[values.len()/2],
        "p99_ms":values[(values.len()*99).div_ceil(100)-1],
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut results = Vec::new();
    for count in [100, 1000, 10000] {
        let mut entries: Vec<_> = (0..count)
            .map(|i| DirEntry {
                name: format!("fixture-{i:05}"),
                parent_path: "/".into(),
                file_type: FileType::Directory,
            })
            .collect();
        entries.push(DirEntry {
            name: "data".into(),
            parent_path: "/".into(),
            file_type: FileType::File,
        });
        let payload = serde_json::to_vec(&entries)?;
        let expected: Vec<_> = (0..count).map(|i| format!("fixture-{i:05}")).collect();
        let mut cloning = Vec::new();
        let mut encoding = Vec::new();
        let mut legacy = Vec::new();
        let mut typed = Vec::new();
        for _ in 0..1000 {
            let start = Instant::now();
            let cloned = std::hint::black_box(entries.clone());
            cloning.push(start.elapsed().as_secs_f64() * 1000.0);
            drop(cloned);
            let start = Instant::now();
            let encoded = serde_json::to_vec(&entries)?;
            encoding.push(start.elapsed().as_secs_f64() * 1000.0);
            assert_eq!(encoded, payload);
            let start = Instant::now();
            let value: serde_json::Value = serde_json::from_slice(&payload)?;
            let names: BTreeSet<_> = value
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|item| item["name"].as_str())
                .collect();
            assert!(names.contains("data"));
            assert!((0..count).all(|i| names.contains(format!("fixture-{i:05}").as_str())));
            // Include destruction, which also occupies the client executor.
            drop(names);
            drop(value);
            legacy.push(start.elapsed().as_secs_f64() * 1000.0);
            let start = Instant::now();
            let value: Vec<DirEntry> = serde_json::from_slice(&payload)?;
            let names: BTreeSet<_> = value.iter().map(|entry| entry.name.as_str()).collect();
            assert!(names.contains("data"));
            assert!(expected.iter().all(|name| names.contains(name.as_str())));
            drop(names);
            drop(value);
            typed.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        results.push(serde_json::json!({
            "fixture_entries":count,"json_bytes":payload.len(),
            "clone":summary(cloning),"serialize":summary(encoding),
            "legacy_value_parse_and_validate":summary(legacy),
            "typed_parse_and_precomputed_names_validate":summary(typed),
        }));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "scope":"sequential CPU only, excludes HTTP, compression and concurrency; typed comparison also precomputes expected names",
            "results":results
        }))?
    );
    Ok(())
}
