//! Direct memory-backed metadata/content scaling diagnostic; excludes fixture setup.
use mount_rs_chunked::{AtimePolicy, ChunkedFs, ChunkedOptions};
use mount_rs_core::driver::FsDriver;
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use std::time::Instant;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("operation,file_bytes,extents,read_bytes,samples,p50_ms,p99_ms,max_ms");
    for size in [4096, 262144, 4194304] {
        let fs = ChunkedFs::open(
            MemoryMetadataStore::new(),
            MemoryBlockStore::new(),
            ChunkedOptions::fixed("layout-scale", 256)?.with_atime_policy(AtimePolicy::NoAtime),
        )
        .await?;
        let data: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        let file = FsDriver::open(&fs, "/data", "w+", 0o600).await?;
        assert_eq!(file.write(&data, Some(0)).await?, size);
        let mut buffer = [0; 256];
        for operation in ["stat", "handle-read", "open-read-close"] {
            let mut samples = Vec::new();
            for i in 0..220 {
                let position = (i * 7919) % (size - buffer.len() + 1);
                let started = Instant::now();
                let count = match operation {
                    "stat" => {
                        let stats = fs.stat("/data").await?;
                        assert_eq!(stats.size, size as u64);
                        0
                    }
                    "open-read-close" => {
                        let reader = FsDriver::open(&fs, "/data", "r", 0).await?;
                        let count = reader.read(&mut buffer, Some(position as u64)).await?;
                        reader.close().await?;
                        count
                    }
                    _ => file.read(&mut buffer, Some(position as u64)).await?,
                };
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                if operation != "stat" {
                    assert_eq!(count, buffer.len());
                    assert_eq!(buffer, data[position..position + buffer.len()]);
                }
                if i >= 20 {
                    samples.push(elapsed);
                }
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "{operation},{size},{},{},200,{},{},{}",
                size / 256,
                if operation == "stat" { 0 } else { 256 },
                samples[99],
                samples[197],
                samples[199]
            );
        }
        file.close().await?;
        fs.shutdown().await?;
    }
    Ok(())
}
