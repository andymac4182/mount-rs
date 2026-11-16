//! Content tags identify a file's bytes, survive a restart and differ for
//! every change, including same-size rewrites in the same millisecond.
use futures_lite::future::block_on;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::FsDriver;
use mount_rs_sqlite::{SqliteBlockStore, SqliteMetadataStore};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

type Fs = ChunkedFs<SqliteMetadataStore, SqliteBlockStore>;

struct Volume(PathBuf);

impl Volume {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "mount-rs-content-tag-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    async fn open(&self) -> Fs {
        ChunkedFs::open(
            SqliteMetadataStore::open(self.0.join("metadata.db")).unwrap(),
            SqliteBlockStore::open(self.0.join("blocks.db")).unwrap(),
            ChunkedOptions::fixed("content-tag", 4096).unwrap(),
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

async fn tag(fs: &Fs, path: &str) -> String {
    fs.content_tag(path)
        .await
        .unwrap()
        .expect("a file has a tag")
}

#[test]
fn content_tags_follow_every_change_and_survive_a_restart() {
    block_on(async {
        let volume = Volume::new();
        let fs = volume.open().await;
        fs.write_file("/a", b"abcd").await.unwrap();
        let first = tag(&fs, "/a").await;

        // A handle reports the tag of the file it has open.
        let handle = fs.open("/a", "r+", 0).await.unwrap();
        assert_eq!(handle.content_tag().await.unwrap(), Some(first.clone()));

        // Each kind of change gives a new tag, including a same-size
        // rewrite made straight after.
        let mut seen = vec![first.clone()];
        fs.write_file("/a", b"wxyz").await.unwrap();
        seen.push(tag(&fs, "/a").await);
        handle.write(b"Q", Some(1)).await.unwrap();
        seen.push(handle.content_tag().await.unwrap().unwrap());
        handle.truncate(2).await.unwrap();
        seen.push(tag(&fs, "/a").await);
        handle.truncate(9000).await.unwrap();
        seen.push(tag(&fs, "/a").await);
        handle.close().await.unwrap();
        let mut distinct = seen.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(distinct.len(), seen.len(), "{seen:?}");

        // Metadata-only changes keep the tag, links share it, and other
        // kinds of node have none.
        let last = tag(&fs, "/a").await;
        fs.chmod("/a", 0o600).await.unwrap();
        fs.symlink("/a", "/link").await.unwrap();
        fs.mkdir("/dir", Default::default()).await.unwrap();
        assert_eq!(tag(&fs, "/a").await, last);
        assert_eq!(tag(&fs, "/link").await, last);
        assert_eq!(fs.content_tag("/dir").await.unwrap(), None);

        // A rename keeps the bytes and so the tag.
        fs.rename("/a", "/b").await.unwrap();
        assert_eq!(tag(&fs, "/b").await, last);
        fs.shutdown().await.unwrap();
        drop(fs);

        let reopened = volume.open().await;
        assert_eq!(tag(&reopened, "/b").await, last);
        reopened.shutdown().await.unwrap();
    });
}
