use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mount_rs_core::{ErrorCode, Loopback, MkdirOptions, OpenFlags};
use mount_rs_kv::{KeyValueMetadata, KeyValueStore, UnstorageOptions, create_unstorage_driver};

#[derive(Clone, Default)]
struct MemoryStore {
    values: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    metadata: Arc<Mutex<HashMap<String, KeyValueMetadata>>>,
    fail_next_set: Arc<Mutex<bool>>,
}

#[derive(Debug, Clone, Copy)]
struct StoreError;

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("memory store error")
    }
}

#[async_trait]
impl KeyValueStore for MemoryStore {
    type Error = StoreError;

    async fn has_item(&self, key: &str) -> Result<bool, Self::Error> {
        Ok(self.values.lock().expect("values lock").contains_key(key))
    }

    async fn get_item_raw(&self, key: &str) -> Result<Option<Vec<u8>>, Self::Error> {
        Ok(self.values.lock().expect("values lock").get(key).cloned())
    }

    async fn set_item_raw(&self, key: &str, value: Vec<u8>) -> Result<(), Self::Error> {
        let mut fail_next_set = self.fail_next_set.lock().expect("failure lock");
        if *fail_next_set {
            *fail_next_set = false;
            return Err(StoreError);
        }
        drop(fail_next_set);
        self.values
            .lock()
            .expect("values lock")
            .insert(key.to_owned(), value);
        Ok(())
    }

    async fn remove_item(&self, key: &str) -> Result<(), Self::Error> {
        self.values.lock().expect("values lock").remove(key);
        self.metadata.lock().expect("metadata lock").remove(key);
        Ok(())
    }

    async fn get_keys(&self, prefix: &str) -> Result<Vec<String>, Self::Error> {
        let separator = if prefix.is_empty() { "" } else { ":" };
        let prefix = format!("{prefix}{separator}");
        Ok(self
            .values
            .lock()
            .expect("values lock")
            .keys()
            .filter(|key| {
                prefix.is_empty()
                    || key.starts_with(&prefix)
                    || *key == &prefix[..prefix.len().saturating_sub(1)]
            })
            .cloned()
            .collect())
    }

    async fn get_meta(&self, key: &str) -> Result<KeyValueMetadata, Self::Error> {
        Ok(self
            .metadata
            .lock()
            .expect("metadata lock")
            .get(key)
            .copied()
            .unwrap_or_default())
    }
}

impl MemoryStore {
    fn put(&self, key: &str, value: impl AsRef<[u8]>) {
        self.values
            .lock()
            .expect("values lock")
            .insert(key.to_owned(), value.as_ref().to_vec());
    }

    fn bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.values.lock().expect("values lock").get(key).cloned()
    }

    fn set_metadata(&self, key: &str, metadata: KeyValueMetadata) {
        self.metadata
            .lock()
            .expect("metadata lock")
            .insert(key.to_owned(), metadata);
    }

    fn fail_next_set(&self) {
        *self.fail_next_set.lock().expect("failure lock") = true;
    }
}

fn setup() -> (MemoryStore, Loopback) {
    let store = MemoryStore::default();
    let driver = create_unstorage_driver(store.clone(), UnstorageOptions::default());
    (store, Loopback::new(driver))
}

async fn read(fs: &Loopback, path: &str) -> Vec<u8> {
    fs.read_file(path).await.expect("read file")
}

#[tokio::test]
async fn maps_keys_and_synthesizes_directories() {
    let (store, fs) = setup();
    fs.mkdir(
        "/a/b",
        MkdirOptions {
            recursive: true,
            mode: None,
        },
    )
    .await
    .expect("mkdir");
    fs.write_file("/a/b/c", b"hello").await.expect("write");
    assert_eq!(store.bytes("a:b:c"), Some(b"hello".to_vec()));
    let entries = fs.readdir("/a").await.expect("readdir");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].name, "b");
    assert_eq!(entries[0].parent_path, "/a");
    assert_eq!(entries[0].file_type, mount_rs_core::FileType::Directory);
    assert_eq!(read(&fs, "/a/b/c").await, b"hello");
}

#[tokio::test]
async fn rejects_unrepresentable_names_and_hides_metadata_keys() {
    let (store, fs) = setup();
    for path in ["/a:b", "/a?b", "/meta$"] {
        let error = fs.write_file(path, b"x").await.expect_err("invalid key");
        assert_eq!(error.code, ErrorCode::Einval);
        assert_eq!(
            fs.stat(path).await.expect_err("invalid stat").code,
            ErrorCode::Einval
        );
        assert_eq!(
            fs.mkdir(path, Default::default())
                .await
                .expect_err("invalid mkdir")
                .code,
            ErrorCode::Einval
        );
    }
    store.put("note", b"text");
    store.put("note$", b"metadata");
    assert_eq!(fs.readdir("/").await.expect("readdir").len(), 1);
    assert_eq!(fs.stat("/note").await.expect("stat").size, 4);
}

#[tokio::test]
async fn key_wins_over_a_prefix_and_reports_enotdir() {
    let (store, fs) = setup();
    store.put("a", b"file");
    store.put("a:b", b"shadowed");
    assert!(fs.stat("/a").await.expect("stat").is_file());
    assert_eq!(
        fs.readdir("/").await.expect("readdir")[0].file_type,
        mount_rs_core::FileType::File
    );
    assert_eq!(
        fs.stat("/a/b").await.expect_err("shadowed").code,
        ErrorCode::Enotdir
    );
    assert_eq!(
        fs.readdir("/a").await.expect_err("shadowed").code,
        ErrorCode::Enotdir
    );
}

#[tokio::test]
async fn empty_directories_are_process_local() {
    let (store, fs) = setup();
    fs.mkdir("/empty", Default::default()).await.expect("mkdir");
    assert!(fs.stat("/empty").await.expect("stat").is_directory());
    let other = Loopback::new(create_unstorage_driver(store, UnstorageOptions::default()));
    assert_eq!(
        other.stat("/empty").await.expect_err("fresh driver").code,
        ErrorCode::Enoent
    );
}

#[tokio::test]
async fn handles_share_buffers_and_flush_on_sync_or_close() {
    let (store, fs) = setup();
    fs.write_file("/f", b"aaaa").await.expect("write");
    let writer = fs.open("/f", "r+", 0).await.expect("open writer");
    let reader = fs.open("/f", "r", 0).await.expect("open reader");
    writer.write(b"bb", Some(0)).await.expect("write handle");
    assert_eq!(read_handle(&reader, 4).await, b"bbaa");
    assert_eq!(store.bytes("f"), Some(b"aaaa".to_vec()));
    writer.sync().await.expect("sync");
    assert_eq!(store.bytes("f"), Some(b"bbaa".to_vec()));
    writer.close().await.expect("close writer");
    reader.close().await.expect("close reader");
}

#[tokio::test]
async fn stat_sees_unflushed_growth_and_zero_length_positioned_writes_extend() {
    let (store, fs) = setup();
    fs.write_file("/f", b"a").await.expect("write");
    let handle = fs.open("/f", "r+", 0).await.expect("open");
    handle.write(b"bc", Some(3)).await.expect("sparse write");
    assert_eq!(fs.stat("/f").await.expect("stat").size, 5);
    assert_eq!(store.bytes("f"), Some(b"a".to_vec()));
    handle.close().await.expect("close");
    assert_eq!(store.bytes("f"), Some(b"a\0\0bc".to_vec()));

    let zero = fs.open("/f", "r+", 0).await.expect("reopen");
    zero.write(b"", Some(7)).await.expect("zero-length write");
    assert_eq!(fs.stat("/f").await.expect("stat extension").size, 7);
    zero.close().await.expect("close extension");
    assert_eq!(store.bytes("f"), Some(b"a\0\0bc\0\0".to_vec()));
}

#[tokio::test]
async fn failed_close_keeps_dirty_buffer_for_retry_and_path_truncate() {
    let (store, fs) = setup();
    fs.write_file("/f", b"aaaa").await.expect("write");
    let handle = fs.open("/f", "r+", 0).await.expect("open");
    handle.write(b"bbbb", Some(0)).await.expect("write handle");
    store.fail_next_set();
    assert_eq!(
        handle.close().await.expect_err("failed close").code,
        ErrorCode::Eio
    );
    assert_eq!(store.bytes("f"), Some(b"aaaa".to_vec()));

    fs.truncate("/f", 2).await.expect("truncate dirty buffer");
    assert_eq!(store.bytes("f"), Some(b"bb".to_vec()));
    let retry = fs.open("/f", "r", 0).await.expect("fresh open");
    assert_eq!(read_handle(&retry, 4).await, b"bb");
    retry.close().await.expect("close fresh");
}

#[tokio::test]
async fn unlink_and_rename_preserve_open_handle_rules() {
    let (store, fs) = setup();
    fs.write_file("/doomed", b"still here")
        .await
        .expect("write");
    let doomed = fs.open("/doomed", "r+", 0).await.expect("open");
    fs.unlink("/doomed").await.expect("unlink");
    doomed.write(b"x", Some(0)).await.expect("write orphan");
    assert_eq!(read_handle(&doomed, 10).await, b"xtill here");
    doomed.close().await.expect("close orphan");
    assert_eq!(store.bytes("doomed"), None);

    fs.write_file("/from", b"aaaa").await.expect("write source");
    let handle = fs.open("/from", "r+", 0).await.expect("open source");
    handle.write(b"bb", Some(0)).await.expect("write source");
    fs.rename("/from", "/to").await.expect("rename");
    handle.write(b"cc", Some(2)).await.expect("write renamed");
    handle.close().await.expect("close renamed");
    assert_eq!(store.bytes("from"), None);
    assert_eq!(store.bytes("to"), Some(b"bbcc".to_vec()));

    fs.write_file("/source", b"source")
        .await
        .expect("write source");
    fs.write_file("/target", b"target")
        .await
        .expect("write target");
    let replaced = fs.open("/target", "r+", 0).await.expect("open target");
    fs.rename("/source", "/target")
        .await
        .expect("replace target");
    replaced
        .write(b"zzzzzz", Some(0))
        .await
        .expect("write replaced handle");
    replaced.close().await.expect("close replaced handle");
    assert_eq!(store.bytes("source"), None);
    assert_eq!(read(&fs, "/target").await, b"source");
}

#[tokio::test]
async fn directory_rename_moves_nested_keys_and_open_handles() {
    let (store, fs) = setup();
    fs.mkdir(
        "/from/inner",
        MkdirOptions {
            recursive: true,
            mode: None,
        },
    )
    .await
    .expect("mkdir");
    fs.write_file("/from/inner/file", b"aaaa")
        .await
        .expect("write");
    let handle = fs.open("/from/inner/file", "r+", 0).await.expect("open");
    handle.write(b"bb", Some(0)).await.expect("write open file");
    fs.rename("/from", "/to").await.expect("rename directory");
    handle
        .write(b"cc", Some(2))
        .await
        .expect("write renamed file");
    handle.close().await.expect("close renamed file");
    assert_eq!(store.bytes("from:inner:file"), None);
    assert_eq!(store.bytes("to:inner:file"), Some(b"bbcc".to_vec()));
    assert_eq!(read(&fs, "/to/inner/file").await, b"bbcc");
    assert_eq!(
        fs.stat("/from").await.expect_err("old directory").code,
        ErrorCode::Enoent
    );
}

async fn read_handle(handle: &Arc<dyn mount_rs_core::FileHandle>, length: usize) -> Vec<u8> {
    let mut data = vec![0; length];
    let count = handle.read(&mut data, Some(0)).await.expect("read handle");
    data.truncate(count);
    data
}

#[tokio::test]
async fn open_flags_and_truncate_avoid_unneeded_reads() {
    let (store, fs) = setup();
    fs.write_file("/f", b"abcdef").await.expect("write");
    fs.open("/f", "w", 0)
        .await
        .expect("truncate open")
        .close()
        .await
        .expect("close");
    assert_eq!(store.bytes("f"), Some(Vec::new()));
    fs.write_file("/f", b"abcdef").await.expect("write again");
    fs.truncate("/f", 0).await.expect("truncate");
    assert_eq!(store.bytes("f"), Some(Vec::new()));
    let flags = OpenFlags {
        read: true,
        write: true,
        create: true,
        truncate: false,
        append: true,
        exclusive: false,
    };
    let handle = fs.open_flags("/f", flags, 0).await.expect("decoded flags");
    handle.write(b"z", None).await.expect("append");
    handle.close().await.expect("close");
    assert_eq!(read(&fs, "/f").await, b"z");
}

#[tokio::test]
async fn metadata_overlay_and_read_only_capabilities() {
    let (store, fs) = setup();
    store.put("f", b"four");
    store.set_metadata(
        "f",
        KeyValueMetadata {
            size: Some(1234),
            atime_ms: Some(1_699_999_999_000),
            mtime_ms: Some(1_700_000_000_000),
            ctime_ms: Some(1_700_000_000_500),
            birthtime_ms: Some(1_699_999_998_000),
        },
    );
    let stats = fs.stat("/f").await.expect("stat");
    assert_eq!(stats.size, 1234);
    assert_eq!(stats.atime_ms, 1_699_999_999_000);
    assert_eq!(stats.mtime_ms, 1_700_000_000_000);
    assert_eq!(stats.ctime_ms, 1_700_000_000_500);
    assert_eq!(stats.birthtime_ms, 1_699_999_998_000);
    fs.chmod("/f", 0o600).await.expect("chmod");
    fs.utimes("/f", 1000, 2000).await.expect("utimes");
    let stats = fs.stat("/f").await.expect("stat overlay");
    assert_eq!(stats.mode & 0o777, 0o600);
    assert_eq!(stats.mtime_ms, 2000);

    let read_only = Loopback::new(create_unstorage_driver(
        store,
        UnstorageOptions {
            read_only: true,
            ..Default::default()
        },
    ));
    assert!(read_only.capabilities.read_only);
    let error = match read_only.open("/f", "w", 0).await {
        Ok(_) => panic!("read-only open unexpectedly succeeded"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::Erofs);
    assert_eq!(
        read_only.unlink("/f").await.expect_err("read-only").code,
        ErrorCode::Erofs
    );
}

#[tokio::test]
async fn keeps_special_node_mknod_explicitly_unsupported() {
    let (store, fs) = setup();
    assert!(!fs.capabilities.mknod);

    let cases = [
        ("fifo", 0o010644, 0),
        ("socket", 0o140600, 0),
        ("character", 0o020666, (1 << 8) | 3),
        ("block", 0o060660, 7 << 8),
    ];
    for (name, mode, dev) in cases {
        let path = format!("/{name}");
        let error = fs
            .mknod(&path, mode, dev)
            .await
            .expect_err("special node unexpectedly created");
        assert_eq!(error.code, ErrorCode::Enosys);
        assert_eq!(error.syscall.as_deref(), Some("mknod"));
        assert_eq!(error.path, None);
        assert_eq!(store.bytes(name), None);
    }
}
