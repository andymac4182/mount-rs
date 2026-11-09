//! Encoding-only diagnostic. No HTTP, storage, concurrency or SLO qualification.
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use mount_rs_core::{DirEntry, FileType};
use std::io::{Read, Write};
use std::time::Instant;

fn p99(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[(values.len() * 99).div_ceil(100) - 1]
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
        for level in [1, 6] {
            let mut encode = Vec::new();
            let mut decode = Vec::new();
            let mut compressed_bytes = 0;
            for _ in 0..1000 {
                let started = Instant::now();
                let mut encoder = GzEncoder::new(Vec::new(), Compression::new(level));
                encoder.write_all(&payload)?;
                let encoded = encoder.finish()?;
                encode.push(started.elapsed().as_secs_f64() * 1000.0);
                compressed_bytes = encoded.len();
                let started = Instant::now();
                let mut decoded = Vec::new();
                GzDecoder::new(encoded.as_slice()).read_to_end(&mut decoded)?;
                decode.push(started.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(decoded, payload);
            }
            results.push(serde_json::json!({
                "directory_entries":count,"samples":1000,"gzip_level":level,
                "json_bytes":payload.len(),"gzip_bytes":compressed_bytes,
                "encode_p99_ms":p99(&mut encode),"decode_p99_ms":p99(&mut decode),
            }));
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "scope":"sequential encoding-only; excludes serialization, HTTP, storage and concurrency",
            "results":results
        }))?
    );
    Ok(())
}
