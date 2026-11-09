//! Resident metadata scaling diagnostic, without HTTP or durable storage.
use mount_rs_chunked::{AtimePolicy, ChunkedFs, ChunkedOptions};
use mount_rs_core::{
    driver::FsDriver,
    storage::{DirectoryEntry, MetadataStore, NodeData},
};
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use std::time::{Duration, Instant};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let count = 100_000_u64;
    let metadata = MemoryMetadataStore::new();
    let blocks = MemoryBlockStore::new();
    let options = || {
        ChunkedOptions::default()
            .with_atime_policy(AtimePolicy::NoAtime)
            .with_lease_ttl(Duration::from_secs(240))
            .with_read_lease_cache_window(Duration::from_secs(60))
    };
    let fs = ChunkedFs::open(metadata.clone(), blocks.clone(), options()).await?;
    FsDriver::open(&fs, "/template", "w", 0o600)
        .await?
        .close()
        .await?;
    fs.shutdown().await?;
    let loaded = metadata.load().await?;
    let mut namespace = loaded.namespace.unwrap();
    let template = namespace
        .nodes
        .values()
        .find(|node| matches!(node.data, NodeData::File(_)))
        .unwrap()
        .clone();
    let root_node = namespace.nodes[&namespace.root].clone();
    namespace.nodes.clear();
    namespace.nodes.insert(namespace.root, root_node);
    let mut entries = Vec::new();
    for index in 0..count {
        let inode = index + 2;
        let mut node = template.clone();
        node.stats.ino = inode;
        namespace.nodes.insert(inode, node);
        entries.push(DirectoryEntry {
            name: format!("file-{index:06}"),
            inode,
        });
    }
    namespace.next_inode = count + 2;
    namespace.nodes.get_mut(&namespace.root).unwrap().data = NodeData::Directory { entries };
    namespace.validate()?;
    let lease = metadata
        .acquire_writer("seed", Duration::from_secs(30))
        .await?;
    metadata.publish(loaded.revision, &lease, namespace).await?;
    metadata.flush().await?;
    metadata.release_writer(&lease).await?;
    let fs = ChunkedFs::open(metadata.clone(), blocks.clone(), options()).await?;

    // Two independent executor threads share one volume. Sample root stat while
    // the other thread repeatedly materializes the large directory. The delay
    // between stat samples is outside timing; this is contention, not throughput.
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_fs = fs.clone();
    let worker_barrier = barrier.clone();
    let worker_done = done.clone();
    let worker = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        worker_barrier.wait();
        runtime.block_on(async {
            for _ in 0..100 {
                assert_eq!(worker_fs.readdir("/").await.unwrap().len(), count as usize);
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });
        worker_done.store(true, std::sync::atomic::Ordering::Release);
    });
    barrier.wait();
    let mut samples = Vec::new();
    while !done.load(std::sync::atomic::Ordering::Acquire) && samples.len() < 100_000 {
        let start = Instant::now();
        assert_eq!(fs.stat("/").await?.ino, 1);
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
        tokio::time::sleep(Duration::from_micros(100)).await;
    }
    worker.join().expect("directory worker failed");
    fs.shutdown().await?;
    samples.sort_by(f64::total_cmp);
    assert!(!samples.is_empty());
    let percentile = |percent: usize| samples[(samples.len() * percent).div_ceil(100) - 1];
    println!("operation,directory_entries,samples,p50_ms,p99_ms,max_ms,at_least_10ms");
    println!(
        "stat-during-directory-listing,{count},{},{},{},{},{}",
        samples.len(),
        percentile(50),
        percentile(99),
        samples.last().unwrap(),
        samples.iter().filter(|x| **x >= 10.0).count()
    );
    Ok(())
}
