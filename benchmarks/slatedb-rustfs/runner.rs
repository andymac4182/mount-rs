//! Same write/full-read/delete loop for SlateDB as a key-value filesystem and
//! SlateDB metadata plus RustFS immutable blocks. Use a disposable bucket.

use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::driver::FsDriver;
use mount_rs_kv::{UnstorageOptions, create_unstorage_driver};
use mount_rs_rustfs::{RustFsBlockStore, RustFsConfig};
use mount_rs_slatedb::{SlateDbMetadataStore, SlateDbStore, rustfs_object_store};
use serde_json::json;
use std::env;
use std::time::Instant;

fn required(name: &str) -> String {
    env::var(name).unwrap_or_else(|_| panic!("{name} must be set by the RustFS harness"))
}

fn sample_stats(samples: &[f64]) -> serde_json::Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    let percentile =
        |percent: usize| sorted[(sorted.len() * percent).div_ceil(100).saturating_sub(1)];
    let middle = sorted.len() / 2;
    let median = if sorted.len().is_multiple_of(2) {
        (sorted[middle - 1] + sorted[middle]) / 2.0
    } else {
        sorted[middle]
    };
    json!({"count": sorted.len(), "median_ms": median, "p95_ms": percentile(95), "raw_ms": samples})
}

async fn measure<D: FsDriver>(
    driver: &D,
    size: usize,
    iterations: usize,
) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    let payload: Vec<u8> = (0..size).map(|index| (index % 251) as u8).collect();
    let mut writes = Vec::new();
    let mut reads = Vec::new();
    let mut deletes = Vec::new();
    for index in 0..iterations {
        let path = format!("/bench-{size}-{index}");
        let start = Instant::now();
        driver.write_file(&path, &payload).await?;
        writes.push(start.elapsed().as_secs_f64() * 1000.0);

        let start = Instant::now();
        let handle = driver.open(&path, "r", 0).await?;
        let mut read = vec![0; size];
        let mut offset = 0;
        while offset < size {
            let count = handle
                .read(&mut read[offset..], Some(offset as u64))
                .await?;
            if count == 0 {
                return Err("short read".into());
            }
            offset += count;
        }
        handle.close().await?;
        reads.push(start.elapsed().as_secs_f64() * 1000.0);
        if read != payload {
            return Err("read bytes differ".into());
        }

        let start = Instant::now();
        driver.unlink(&path).await?;
        deletes.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    Ok(
        json!({"size_bytes": size, "iterations": iterations, "write": sample_stats(&writes), "read": sample_stats(&reads), "delete": sample_stats(&deletes)}),
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = RustFsConfig {
        endpoint: required("RUSTFS_ENDPOINT"),
        bucket: required("RUSTFS_BUCKET"),
        region: required("RUSTFS_REGION"),
        access_key_id: required("RUSTFS_ACCESS_KEY_ID"),
        secret_access_key: required("RUSTFS_SECRET_ACCESS_KEY"),
    };
    let prefix = required("RUSTFS_COMBO_PREFIX");
    let cases = [(4096, 10), (1024 * 1024, 3)];

    let kv_store = SlateDbStore::open(
        &format!("{prefix}/slatedb-kv"),
        rustfs_object_store(&config)?,
    )
    .await?;
    let kv = create_unstorage_driver(kv_store.clone(), UnstorageOptions::default());
    let mut kv_results = Vec::new();
    for (size, iterations) in cases {
        kv_results.push(measure(&kv, size, iterations).await?);
    }
    drop(kv);
    kv_store.close().await?;

    let metadata = SlateDbMetadataStore::open(
        &format!("{prefix}/slatedb-metadata"),
        rustfs_object_store(&config)?,
    )
    .await?;
    let blocks = RustFsBlockStore::from_config(&config, format!("{prefix}/blocks"), false)?;
    let fs = ChunkedFs::open(
        metadata.clone(),
        blocks,
        ChunkedOptions::fixed("slatedb-benchmark", 65536)?,
    )
    .await?;
    let mut split_results = Vec::new();
    for (size, iterations) in cases {
        split_results.push(measure(&fs, size, iterations).await?);
    }
    fs.shutdown().await?;
    metadata.close().await?;

    let result = json!({
        "format": "mount-rs-slatedb-rustfs-benchmark-v1",
        "rustfs_image": required("RUSTFS_HARNESS_IMAGE"),
        "topology": "single-node loopback RustFS",
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "read_cache_state": "warm same-writer; reads can be served from SlateDB or block-store caches",
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "cases": [
            {"role": "SlateDB KeyValueFs", "results": kv_results},
            {"role": "SlateDB metadata + RustFS blocks", "results": split_results}
        ]
    });
    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}
