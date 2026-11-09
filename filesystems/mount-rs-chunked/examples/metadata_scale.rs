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
    println!("operation,directory_entries,samples,p50_ms,p99_ms,max_ms");
    for count in [10u64, 1000, 100000] {
        let metadata = MemoryMetadataStore::new();
        let blocks = MemoryBlockStore::new();
        let options = || ChunkedOptions::default().with_atime_policy(AtimePolicy::NoAtime);
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
        let last = format!("/file-{:06}", count - 1);
        for operation in ["stat-last-entry", "readdir-full", "statfs"] {
            let mut samples = Vec::new();
            for iteration in 0..120 {
                let started = Instant::now();
                match operation {
                    "stat-last-entry" => {
                        assert_eq!(fs.stat(&last).await?.ino, count + 1);
                    }
                    "readdir-full" => {
                        let entries = fs.readdir("/").await?;
                        assert_eq!(entries.len(), count as usize);
                        assert_eq!(entries.last().unwrap().name, last[1..]);
                    }
                    _ => {
                        assert_eq!(fs.statfs("/").await?.files_free, 1024 * 1024 - count - 1);
                    }
                }
                let elapsed = started.elapsed().as_secs_f64() * 1000.0;
                if iteration >= 20 {
                    samples.push(elapsed);
                }
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "{operation},{count},100,{},{},{}",
                samples[49], samples[98], samples[99]
            );
        }
        fs.shutdown().await?;
        // A fresh owner has no resident namespace. The provider itself stays
        // memory-backed: this measures loading/validation, not cold disk/TiDB.
        let mut opens = Vec::new();
        for iteration in 0..120 {
            let started = Instant::now();
            let reopened = ChunkedFs::open(metadata.clone(), blocks.clone(), options()).await?;
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(reopened.stat(&last).await?.ino, count + 1);
            reopened.shutdown().await?;
            if iteration >= 20 {
                opens.push(elapsed);
            }
        }
        opens.sort_by(f64::total_cmp);
        println!(
            "owner-open-memory-provider,{count},100,{},{},{}",
            opens[49], opens[98], opens[99]
        );
    }
    Ok(())
}
