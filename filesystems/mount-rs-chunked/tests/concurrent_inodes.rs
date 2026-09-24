//! Independent inode revisions over durable SQLite metadata and blocks.
use futures_lite::future::block_on;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::MetadataStore;
use mount_rs_core::{ErrorCode, FsDriver};
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

type Fs = ChunkedFs<SqliteMetadataStore, SqliteBlockStore>;
struct Volume(PathBuf);
impl Volume {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mount-rs-inodes-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn metadata(&self) -> SqliteMetadataStore {
        SqliteMetadataStore::open(self.0.join("metadata.db")).unwrap()
    }
    async fn open(&self, owner: &str) -> Fs {
        ChunkedFs::open(
            self.metadata(),
            SqliteBlockStore::open(self.0.join("blocks.db")).unwrap(),
            ChunkedOptions::fixed(owner, 16)
                .unwrap()
                .with_inode_updates(true),
        )
        .await
        .unwrap()
    }
}
impl Drop for Volume {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn read_file(fs: &Fs, path: &str) -> Vec<u8> {
    let file = fs.open(path, "r", 0).await.unwrap();
    let mut bytes = vec![0; usize::try_from(file.stat().await.unwrap().size).unwrap()];
    let mut offset = 0;
    while offset < bytes.len() {
        let count = file
            .read(&mut bytes[offset..], Some(offset as u64))
            .await
            .unwrap();
        assert!(count > 0, "file ended before its reported size");
        offset += count;
    }
    file.close().await.unwrap();
    bytes
}

#[test]
fn independent_handle_writes_preserve_structure_and_other_inode_revision() {
    block_on(async {
        let volume = Volume::new();
        let a = volume.open("a").await;
        a.write_file("/a", b"aaaa").await.unwrap();
        a.write_file("/b", b"bbbb").await.unwrap();
        let b = volume.open("b").await;
        let left = a.open("/a", "r+", 0).await.unwrap();
        let right = b.open("/b", "r+", 0).await.unwrap();
        let metadata = volume.metadata();
        let backing = metadata.inode_mode_state().await.unwrap().unwrap().backing;
        let before = metadata.load_inode_snapshot(backing).await.unwrap();
        left.write(b"LEFT", Some(0)).await.unwrap();
        let middle = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(middle.structural_generation, before.structural_generation);
        assert_eq!(
            middle
                .inode_revisions
                .iter()
                .filter(|(inode, revision)| before.inode_revisions.get(inode) != Some(revision))
                .count(),
            1
        );
        right.write(b"RIGHT", Some(0)).await.unwrap();
        let after = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(after.structural_generation, before.structural_generation);
        assert_eq!(
            after
                .inode_revisions
                .iter()
                .filter(|(inode, revision)| before.inode_revisions.get(inode) != Some(revision))
                .count(),
            2
        );
        let mut data = [0; 5];
        assert_eq!(right.read(&mut data, Some(0)).await.unwrap(), 5);
        assert_eq!(&data, b"RIGHT");
        assert_eq!(a.stat("/b").await.unwrap().size, 5);
        // Structural publication must fold both independently updated records.
        a.rename("/a", "/renamed").await.unwrap();
        assert_eq!(read_file(&b, "/renamed").await, b"LEFT");
        assert_eq!(read_file(&a, "/b").await, b"RIGHT");
        left.close().await.unwrap();
        right.close().await.unwrap();
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    });
}

#[test]
fn concurrent_append_replays_same_inode_and_detached_handle_keeps_latest_bytes() {
    let volume = Volume::new();
    let (a, b) = block_on(async {
        let a = volume.open("a").await;
        a.write_file("/data", b"").await.unwrap();
        (a, volume.open("b").await)
    });
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let workers: Vec<_> = [a.clone(), b.clone()]
        .into_iter()
        .enumerate()
        .map(|(index, fs)| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                block_on(async {
                    let file = fs.open("/data", "a", 0).await.unwrap();
                    barrier.wait();
                    for _ in 0..12 {
                        file.write(&[b'A' + index as u8], None).await.unwrap();
                    }
                    file.close().await.unwrap();
                })
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    block_on(async {
        let bytes = read_file(&a, "/data").await;
        assert_eq!(bytes.len(), 24);
        assert_eq!(bytes.iter().filter(|byte| **byte == b'A').count(), 12);
        assert_eq!(bytes.iter().filter(|byte| **byte == b'B').count(), 12);
        let file = a.open("/data", "r+", 0).await.unwrap();
        file.write(b"latest", Some(0)).await.unwrap();
        b.unlink("/data").await.unwrap();
        let mut data = [0; 6];
        file.read(&mut data, Some(0)).await.unwrap();
        assert_eq!(&data, b"latest");
        file.write(b"orphan", Some(0)).await.unwrap();
        file.read(&mut data, Some(0)).await.unwrap();
        assert_eq!(&data, b"orphan");
        assert_eq!(a.stat("/data").await.unwrap_err().code, ErrorCode::Enoent);
        file.close().await.unwrap();
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    });
}

#[test]
fn established_inode_mode_requires_opt_in_and_rejects_writeback() {
    block_on(async {
        let volume = Volume::new();
        let fs = volume.open("inode-mode").await;
        fs.shutdown().await.unwrap();
        let blocks = || SqliteBlockStore::open(volume.0.join("blocks.db")).unwrap();
        assert!(
            ChunkedFs::open(
                volume.metadata(),
                blocks(),
                ChunkedOptions::fixed("old", 16)
                    .unwrap()
                    .with_concurrent_writes(true)
            )
            .await
            .is_err()
        );
        assert_eq!(
            ChunkedFs::open(
                volume.metadata(),
                blocks(),
                ChunkedOptions::fixed("invalid", 16)
                    .unwrap()
                    .with_inode_updates(true)
                    .with_writeback(true)
            )
            .await
            .err()
            .unwrap()
            .code,
            ErrorCode::Einval
        );
    });
}

#[test]
fn existing_replacements_and_handle_truncates_only_publish_their_inode() {
    block_on(async {
        let volume = Volume::new();
        let a = volume.open("a").await;
        a.write_file("/left", b"old").await.unwrap();
        a.write_file("/right", b"other").await.unwrap();
        let b = volume.open("b").await;
        let metadata = volume.metadata();
        let backing = metadata.inode_mode_state().await.unwrap().unwrap().backing;
        let before = metadata.load_inode_snapshot(backing).await.unwrap();
        for bytes in [b"replacement".as_slice(), b"", b"last value"] {
            a.write_file("/left", bytes).await.unwrap();
            assert_eq!(read_file(&b, "/left").await, bytes);
        }
        let replaced = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(replaced.structural_generation, before.structural_generation);
        assert_eq!(
            replaced
                .inode_revisions
                .iter()
                .filter(|(inode, revision)| before.inode_revisions.get(inode) != Some(revision))
                .count(),
            1
        );
        let file = a.open("/left", "r+", 0).await.unwrap();
        file.truncate(4).await.unwrap();
        assert_eq!(read_file(&b, "/left").await, b"last");
        file.truncate(7).await.unwrap();
        assert_eq!(read_file(&b, "/left").await, b"last\0\0\0");
        let truncated = metadata.load_inode_snapshot(backing).await.unwrap();
        assert_eq!(
            truncated.structural_generation,
            before.structural_generation
        );
        assert_eq!(
            truncated
                .inode_revisions
                .iter()
                .filter(|(inode, revision)| before.inode_revisions.get(inode) != Some(revision))
                .count(),
            1
        );
        file.close().await.unwrap();
        b.truncate("/left", 2).await.unwrap();
        assert_eq!(read_file(&a, "/left").await, b"la");
        let opened = a.open("/left", "w", 0).await.unwrap();
        opened.close().await.unwrap();
        assert!(read_file(&b, "/left").await.is_empty());
        assert_eq!(
            metadata
                .inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .structural_generation,
            before.structural_generation
        );
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    });
}

#[test]
fn simultaneous_replacement_and_other_inode_truncate_keep_both_results() {
    let volume = Volume::new();
    let (a, b) = block_on(async {
        let a = volume.open("a").await;
        a.write_file("/left", b"original").await.unwrap();
        a.write_file("/right", b"original").await.unwrap();
        (a, volume.open("b").await)
    });
    let generation = block_on(async {
        volume
            .metadata()
            .inode_mode_state()
            .await
            .unwrap()
            .unwrap()
            .structural_generation
    });
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let replacement = {
        let fs = a.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            block_on(async {
                barrier.wait();
                for _ in 0..12 {
                    fs.write_file("/left", b"replaced").await.unwrap();
                }
            })
        })
    };
    let truncate = {
        let fs = b.clone();
        std::thread::spawn(move || {
            block_on(async {
                let file = fs.open("/right", "r+", 0).await.unwrap();
                barrier.wait();
                for size in [2, 8, 2, 8, 2, 8, 2, 8, 2, 8, 2] {
                    file.truncate(size).await.unwrap();
                }
                file.close().await.unwrap();
            })
        })
    };
    replacement.join().unwrap();
    truncate.join().unwrap();
    block_on(async {
        assert_eq!(read_file(&b, "/left").await, b"replaced");
        assert_eq!(read_file(&a, "/right").await, b"or");
        assert_eq!(
            volume
                .metadata()
                .inode_mode_state()
                .await
                .unwrap()
                .unwrap()
                .structural_generation,
            generation
        );
        a.shutdown().await.unwrap();
        b.shutdown().await.unwrap();
    });
}
