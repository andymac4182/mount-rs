use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mount_rs_core::{ErrorCode, FileType, Loopback, MkdirOptions, OpenFlags};
use mount_rs_kv::{KeyValueMetadata, KeyValueStore, UnstorageOptions, create_unstorage_driver};

#[derive(Clone, Default)]
struct MemoryStore {
    values: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    metadata: Arc<Mutex<HashMap<String, KeyValueMetadata>>>,
    bounded_limits: Arc<Mutex<Vec<usize>>>,
    lexical_listing: Arc<Mutex<bool>>,
    fail_next_set: Arc<Mutex<bool>>,
    held_set: Arc<HeldSet>,
    held_get: Arc<HeldSet>,
}

/// Holds the next `set_item_raw` before it reaches the values, so a later
/// one can be issued while it is in flight. For `get_item_raw` it holds
/// the answer after the value was read, so the value can change meanwhile.
#[derive(Default)]
struct HeldSet {
    armed: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
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
        let value = self.values.lock().expect("values lock").get(key).cloned();
        if self
            .held_get
            .armed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.held_get.entered.notify_one();
            self.held_get.resume.notified().await;
        }
        Ok(value)
    }

    async fn set_item_raw(&self, key: &str, value: Vec<u8>) -> Result<(), Self::Error> {
        if self
            .held_set
            .armed
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.held_set.entered.notify_one();
            self.held_set.resume.notified().await;
        }
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
        if *self.lexical_listing.lock().expect("listing mode lock") {
            return Ok(self
                .values
                .lock()
                .expect("values lock")
                .keys()
                .filter(|key| key.starts_with(prefix))
                .cloned()
                .collect());
        }
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

    async fn get_keys_bounded(
        &self,
        prefix: &str,
        max_keys: usize,
    ) -> Result<Option<Vec<String>>, Self::Error> {
        self.bounded_limits
            .lock()
            .expect("bounded limits lock")
            .push(max_keys);
        let mut keys = self.get_keys(prefix).await?;
        keys.sort();
        Ok(Some(
            keys.into_iter().take(max_keys.saturating_add(1)).collect(),
        ))
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
    fn use_lexical_listing(&self) {
        *self.lexical_listing.lock().expect("listing mode lock") = true;
    }

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

fn decoded_flags(mask: u8) -> OpenFlags {
    OpenFlags {
        read: mask & 1 != 0,
        write: mask & 2 != 0,
        create: mask & 4 != 0,
        truncate: mask & 8 != 0,
        append: mask & 16 != 0,
        exclusive: mask & 32 != 0,
    }
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
async fn lexical_key_prefixes_do_not_create_directories_from_sibling_keys() {
    for (parent, sibling) in [("a", "ab"), ("é", "éx")] {
        let (store, fs) = setup();
        store.use_lexical_listing();
        store.put(&format!("{sibling}:child"), b"value");
        let path = format!("/./{parent}/");
        assert_eq!(
            fs.stat(&path)
                .await
                .expect_err("sibling must not create a directory")
                .code,
            ErrorCode::Enoent,
            "parent {parent}"
        );
    }
}

#[tokio::test]
async fn native_unrepresentable_keys_do_not_create_directory_entries() {
    let (store, fs) = setup();
    store.put("a:.:child", b"hidden");
    store.put("a:..:child", b"hidden");
    store.put("a::child", b"hidden");
    store.put("a:bad?", b"hidden");
    store.put("a:slash/name", b"hidden");
    store.put("a:meta$", b"metadata");

    assert_eq!(
        fs.stat("/a")
            .await
            .expect_err("no addressable descendants")
            .code,
        ErrorCode::Enoent
    );
    assert!(fs.readdir("/").await.expect("root listing").is_empty());
    assert!(
        fs.readdir_bounded("/", 6)
            .await
            .expect("complete bounded root listing")
            .is_empty()
    );
}

#[tokio::test]
async fn bounded_readdir_enforces_the_provider_limit() {
    let (store, fs) = setup();
    store.put("one", b"1");
    store.put("two", b"2");
    store.put("three", b"3");

    assert_eq!(
        fs.readdir_bounded("/", 2)
            .await
            .expect_err("bounded listing must overflow")
            .code,
        ErrorCode::Eoverflow
    );
    assert_eq!(
        fs.readdir_bounded("/", 3)
            .await
            .expect("bounded listing")
            .len(),
        3
    );
    assert_eq!(
        *store.bounded_limits.lock().expect("bounded limits lock"),
        vec![2, 3],
        "the adapter must request the caller's exact provider bound"
    );
}

#[tokio::test]
async fn bounded_readdir_reports_truncated_provider_pages_even_when_metadata_is_filtered() {
    let (store, fs) = setup();
    store.put("a$", b"metadata");
    store.put("b", b"one");
    store.put("c", b"two");

    assert_eq!(
        fs.readdir_bounded("/", 1)
            .await
            .expect_err("provider page contains an overflow signal")
            .code,
        ErrorCode::Eoverflow
    );
}

#[tokio::test]
async fn small_keyspaces_preserve_immediate_children_and_fail_closed_bounds() {
    const KEYS: [&str; 6] = [
        "a:direct",
        "a:direct:grand",
        "a:unicode:深",
        "a:雪",
        "ab:sibling",
        "a:meta$",
    ];
    for mask in 0..(1_u8 << KEYS.len()) {
        let (store, fs) = setup();
        fs.mkdir("/a", Default::default())
            .await
            .expect("local directory");
        store.use_lexical_listing();
        for (index, key) in KEYS.iter().enumerate() {
            if mask & (1 << index) != 0 {
                store.put(key, b"value");
            }
        }

        let mut expected = Vec::new();
        if mask & 1 != 0 {
            expected.push(("direct".to_owned(), FileType::File));
        } else if mask & 2 != 0 {
            expected.push(("direct".to_owned(), FileType::Directory));
        }
        if mask & 4 != 0 {
            expected.push(("unicode".to_owned(), FileType::Directory));
        }
        if mask & 8 != 0 {
            expected.push(("雪".to_owned(), FileType::File));
        }
        expected.sort_by(|left, right| left.0.cmp(&right.0));

        let mut entries = fs
            .readdir("/a/./")
            .await
            .expect("unbounded normalized listing")
            .into_iter()
            .map(|entry| (entry.name, entry.file_type))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(entries, expected, "mask {mask}");

        let raw_key_count = mask.count_ones() as usize;
        for bound in 1..=KEYS.len() {
            let bounded = fs.readdir_bounded("/a/./", bound).await;
            if raw_key_count > bound {
                assert_eq!(
                    bounded.expect_err("provider page overflow").code,
                    ErrorCode::Eoverflow,
                    "mask {mask}, bound {bound}"
                );
            } else {
                let mut entries = bounded
                    .expect("complete bounded listing")
                    .into_iter()
                    .map(|entry| (entry.name, entry.file_type))
                    .collect::<Vec<_>>();
                entries.sort_by(|left, right| left.0.cmp(&right.0));
                assert_eq!(entries, expected, "mask {mask}, bound {bound}");
            }
        }
    }
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
async fn every_decoded_flag_combination_checks_truncation_before_mutating() {
    for mask in 0..64_u8 {
        let (store, fs) = setup();
        store.put("f", b"keep");
        let flags = decoded_flags(mask);
        let outcome = match fs.open_flags("/f", flags, 0).await {
            Ok(handle) => {
                handle.close().await.expect("close");
                Ok(())
            }
            Err(error) => Err(error.code),
        };
        let expected_outcome = if flags.truncate && !flags.write {
            Err(ErrorCode::Einval)
        } else if flags.exclusive {
            Err(ErrorCode::Eexist)
        } else {
            Ok(())
        };
        let expected_bytes: &[u8] = if flags.truncate && flags.write && !flags.exclusive {
            b""
        } else {
            b"keep"
        };
        assert_eq!(
            store.bytes("f"),
            Some(expected_bytes.to_vec()),
            "mask {mask}"
        );
        assert_eq!(outcome, expected_outcome, "mask {mask}");
    }
}

#[tokio::test]
async fn read_only_driver_preserves_bytes_for_every_decoded_flag_combination() {
    for mask in 0..64_u8 {
        let store = MemoryStore::default();
        store.put("f", b"keep");
        let fs = Loopback::new(create_unstorage_driver(
            store.clone(),
            UnstorageOptions {
                read_only: true,
                ..Default::default()
            },
        ));
        let flags = decoded_flags(mask);
        let outcome = match fs.open_flags("/f", flags, 0).await {
            Ok(handle) => {
                handle.close().await.expect("close");
                Ok(())
            }
            Err(error) => Err(error.code),
        };
        let expected_outcome = if flags.truncate && !flags.write {
            Err(ErrorCode::Einval)
        } else if flags.write || flags.create || flags.truncate {
            Err(ErrorCode::Erofs)
        } else if flags.exclusive {
            Err(ErrorCode::Eexist)
        } else {
            Ok(())
        };
        assert_eq!(store.bytes("f"), Some(b"keep".to_vec()), "mask {mask}");
        assert_eq!(outcome, expected_outcome, "mask {mask}");
    }
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

#[tokio::test]
async fn overlapping_flushes_of_one_file_reach_the_store_in_order() {
    let (store, fs) = setup();
    fs.write_file("/f", b"....").await.expect("write");
    let first = fs.open("/f", "r+", 0).await.expect("open first");
    let second = fs.open("/f", "r+", 0).await.expect("open second");
    first.write(b"old.", Some(0)).await.expect("older write");

    // The older save is on its way to the store when the newer one starts.
    store
        .held_set
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let syncing = first.clone();
    let older = tokio::spawn(async move { syncing.sync().await });
    store.held_set.entered.notified().await;
    second.write(b"new!", Some(0)).await.expect("newer write");
    let syncing = second.clone();
    let newer = tokio::spawn(async move { syncing.sync().await });
    for _ in 0..100 {
        tokio::task::yield_now().await;
    }
    // The newer save has not overtaken the older one.
    assert_eq!(store.bytes("f"), Some(b"....".to_vec()));
    assert!(!newer.is_finished());
    store.held_set.resume.notify_one();
    older.await.expect("join older").expect("older sync");
    newer.await.expect("join newer").expect("newer sync");
    // Both syncs succeeded, so the store holds the latest contents.
    assert_eq!(store.bytes("f"), Some(b"new!".to_vec()));
    first.close().await.expect("close first");
    second.close().await.expect("close second");
    assert_eq!(store.bytes("f"), Some(b"new!".to_vec()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn opens_racing_the_last_close_share_one_buffer_and_lose_no_write() {
    const TASKS: usize = 4;
    const ROUNDS: usize = 4000;
    let (store, fs) = setup();
    let fs = Arc::new(fs);
    fs.write_file("/f", &vec![0; TASKS * ROUNDS])
        .await
        .expect("write");
    let mut tasks = Vec::new();
    for task in 0..TASKS {
        let fs = Arc::clone(&fs);
        tasks.push(tokio::spawn(async move {
            for round in 0..ROUNDS {
                // Each open may find the buffer another task is closing.
                let handle = fs.open("/f", "r+", 0).await.expect("open");
                let offset = (round * TASKS + task) as u64;
                handle.write(&[1], Some(offset)).await.expect("write");
                handle.close().await.expect("close");
            }
        }));
    }
    for task in tasks {
        task.await.expect("join");
    }
    let stored = store.bytes("f").expect("stored");
    let lost = stored.iter().filter(|byte| **byte == 0).count();
    assert_eq!(lost, 0, "{lost} acknowledged writes are missing");
    assert_eq!(read(&fs, "/f").await, stored);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_open_never_lands_on_a_buffer_the_last_close_dropped() {
    const TASKS: usize = 3;
    const ROUNDS: usize = 40;
    // Many short trials: the tasks collide most when they start together.
    for trial in 0..800 {
        let (_store, fs) = setup();
        let fs = Arc::new(fs);
        fs.write_file("/f", &[0; TASKS]).await.expect("write");
        let start = Arc::new(tokio::sync::Barrier::new(TASKS));
        let mut tasks = Vec::new();
        for task in 0..TASKS {
            let (fs, start) = (Arc::clone(&fs), Arc::clone(&start));
            tasks.push(tokio::spawn(async move {
                start.wait().await;
                for round in 0..ROUNDS {
                    // This open may find the buffer another task is closing.
                    let writer = fs.open("/f", "r+", 0).await.expect("open");
                    let tag = [round as u8 + 1];
                    writer.write(&tag, Some(task as u64)).await.expect("write");
                    // While it is open, every other open shares its buffer
                    // and so sees the write before any save.
                    let reader = fs.open("/f", "r", 0).await.expect("open again");
                    let mut seen = [0];
                    reader
                        .read(&mut seen, Some(task as u64))
                        .await
                        .expect("read");
                    assert_eq!(
                        seen, tag,
                        "trial {trial} round {round}: a second buffer for one file"
                    );
                    reader.close().await.expect("close reader");
                    writer.close().await.expect("close writer");
                }
            }));
        }
        for task in tasks {
            task.await.expect("join");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_first_stats_agree_on_one_inode_per_path() {
    const TASKS: usize = 8;
    const PATHS: usize = 200;
    let (store, fs) = setup();
    for index in 0..PATHS {
        store.put(&format!("f{index}"), b"x");
    }
    let fs = Arc::new(fs);
    let start = Arc::new(tokio::sync::Barrier::new(TASKS));
    let mut tasks = Vec::new();
    for task in 0..TASKS {
        let (fs, start) = (Arc::clone(&fs), Arc::clone(&start));
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            let mut seen = vec![0; PATHS];
            // Every task meets the others on a different path first.
            for step in 0..PATHS {
                let index = (step + task * PATHS / TASKS) % PATHS;
                seen[index] = fs.stat(&format!("/f{index}")).await.expect("stat").ino;
            }
            seen
        }));
    }
    let mut seen = Vec::new();
    for task in tasks {
        seen.push(task.await.expect("join"));
    }
    for other in &seen[1..] {
        assert_eq!(other, &seen[0], "two inode numbers for one path");
    }
    let distinct = seen[0]
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    assert_eq!(distinct, PATHS, "one inode number for two paths");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_attribute_updates_are_applied_whole_and_none_is_lost() {
    const ROUNDS: i64 = 20_000;
    let (store, fs) = setup();
    store.put("f", b"x");
    let fs = Arc::new(fs);
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // One task sets both times to one value, another only the mode.
    let times = {
        let fs = Arc::clone(&fs);
        tokio::spawn(async move {
            for round in 1..=ROUNDS {
                fs.utimes("/f", round, round).await.expect("utimes");
                // Only this task sets the times: a mode change working
                // from an older copy must not put the old ones back.
                let stats = fs.stat("/f").await.expect("stat");
                assert_eq!((stats.atime_ms, stats.mtime_ms), (round, round));
            }
        })
    };
    let modes = {
        let fs = Arc::clone(&fs);
        tokio::spawn(async move {
            for round in 1..=ROUNDS {
                let mode = 0o400 | (round as u32 % 0o100);
                fs.chmod("/f", mode).await.expect("chmod");
                // Only this task sets the mode.
                let stats = fs.stat("/f").await.expect("stat");
                assert_eq!(stats.mode & 0o777, mode, "a mode change was lost");
            }
        })
    };
    let reader = {
        let (fs, done) = (Arc::clone(&fs), Arc::clone(&done));
        tokio::spawn(async move {
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                let stats = fs.stat("/f").await.expect("stat");
                assert_eq!(
                    stats.atime_ms, stats.mtime_ms,
                    "a stat saw half of one utimes"
                );
                tokio::task::yield_now().await;
            }
        })
    };
    let times = times.await;
    let modes = modes.await;
    // Stop the reader before reporting, or a failure would leave it
    // running.
    done.store(true, std::sync::atomic::Ordering::Relaxed);
    reader.await.expect("join reader");
    times.expect("join times");
    modes.expect("join modes");

    // Neither task's last update was overwritten by the other's.
    let stats = fs.stat("/f").await.expect("stat");
    assert_eq!((stats.atime_ms, stats.mtime_ms), (ROUNDS, ROUNDS));
    assert_eq!(stats.mode & 0o777, 0o400 | (ROUNDS as u32 % 0o100));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn renames_racing_other_table_changes_lose_nothing() {
    const NESTED: usize = 20;
    const CHURN_TASKS: usize = 3;
    const CHURN: usize = 300;
    const RENAMES: usize = 100;
    let (store, fs) = setup();
    let fs = Arc::new(fs);
    fs.mkdir("/a", Default::default()).await.expect("mkdir");
    let mut before = Vec::new();
    for index in 0..NESTED {
        let path = format!("/a/n{index}");
        let mode = 0o700 | index as u32;
        fs.mkdir(
            &path,
            MkdirOptions {
                recursive: false,
                mode: Some(mode),
            },
        )
        .await
        .expect("mkdir nested");
        before.push((fs.stat(&path).await.expect("stat nested").ino, mode));
    }
    fs.write_file("/a/file", b"1234").await.expect("write");
    let handle = fs.open("/a/file", "r+", 0).await.expect("open");
    handle.write(b"56", Some(4)).await.expect("grow");

    let mut tasks = Vec::new();
    {
        let fs = Arc::clone(&fs);
        tasks.push(tokio::spawn(async move {
            for _ in 0..RENAMES {
                fs.rename("/a", "/b").await.expect("rename there");
                fs.rename("/b", "/a").await.expect("rename back");
            }
        }));
    }
    for task in 0..CHURN_TASKS {
        let fs = Arc::clone(&fs);
        tasks.push(tokio::spawn(async move {
            for index in 0..CHURN {
                // Each of these changes a table the rename is moving
                // paths in.
                let directory = format!("/other{task}x{index}");
                let mode = 0o500 | (index as u32 % 0o100);
                fs.mkdir(
                    &directory,
                    MkdirOptions {
                        recursive: false,
                        mode: Some(mode),
                    },
                )
                .await
                .expect("mkdir other");
                let file = format!("/file{task}x{index}");
                let handle = fs.open(&file, "w", 0o640).await.expect("create");
                handle.write(b"x", None).await.expect("write");
                handle.close().await.expect("close");
                if index % 3 == 0 {
                    fs.unlink(&file).await.expect("unlink");
                }
            }
        }));
    }
    for task in tasks {
        task.await.expect("join");
    }

    for task in 0..CHURN_TASKS {
        for index in 0..CHURN {
            let stats = fs
                .stat(&format!("/other{task}x{index}"))
                .await
                .expect("a directory made during the renames is gone");
            assert_eq!(stats.mode & 0o777, 0o500 | (index as u32 % 0o100));
            let file = fs.stat(&format!("/file{task}x{index}")).await;
            if index % 3 == 0 {
                assert_eq!(file.expect_err("unlinked").code, ErrorCode::Enoent);
            } else {
                assert_eq!(file.expect("file").mode & 0o777, 0o640);
            }
        }
    }
    // The renamed tree kept its inode numbers and modes under its name.
    for (index, (ino, mode)) in before.into_iter().enumerate() {
        let stats = fs.stat(&format!("/a/n{index}")).await.expect("nested");
        assert_eq!((stats.ino, stats.mode & 0o777), (ino, mode));
    }
    assert_eq!(
        fs.stat("/b").await.expect_err("old name").code,
        ErrorCode::Enoent
    );
    // The open file followed every rename with its unsaved bytes.
    assert_eq!(fs.stat("/a/file").await.expect("stat open").size, 6);
    handle.close().await.expect("close");
    assert_eq!(store.bytes("a:file"), Some(b"123456".to_vec()));
    assert_eq!(store.bytes("b:file"), None);
}

#[tokio::test]
async fn rename_moves_what_is_inside_and_leaves_lookalike_siblings() {
    let (_store, fs) = setup();
    let mut inodes = HashMap::new();
    for path in [
        "/a", "/a/x", "/a/x/y", "/a-x", "/a.x", "/a0", "/ab", "/ab/x",
    ] {
        fs.mkdir(path, Default::default()).await.expect("mkdir");
        inodes.insert(path, fs.stat(path).await.expect("stat").ino);
    }
    fs.rename("/a", "/z").await.expect("rename");
    for (old, new) in [("/a", "/z"), ("/a/x", "/z/x"), ("/a/x/y", "/z/x/y")] {
        assert_eq!(
            fs.stat(old).await.expect_err("moved away").code,
            ErrorCode::Enoent
        );
        let stats = fs.stat(new).await.expect("moved");
        assert!(stats.is_directory());
        assert_eq!(stats.ino, inodes[old], "{old} was renumbered");
    }
    for sibling in ["/a-x", "/a.x", "/a0", "/ab", "/ab/x"] {
        let stats = fs.stat(sibling).await.expect("sibling stays");
        assert!(stats.is_directory());
        assert_eq!(stats.ino, inodes[sibling], "{sibling} was renumbered");
    }
    let mut names = fs
        .readdir("/")
        .await
        .expect("readdir")
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, ["a-x", "a.x", "a0", "ab", "z"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn many_files_open_at_once_each_keep_one_buffer() {
    const TASKS: usize = 4;
    // More files than the open-file table has buckets.
    const FILES: usize = 600;
    let (store, fs) = setup();
    let fs = Arc::new(fs);
    let mut tasks = Vec::new();
    for task in 0..TASKS {
        let fs = Arc::clone(&fs);
        tasks.push(tokio::spawn(async move {
            let mut handles = Vec::new();
            for index in 0..FILES {
                let path = format!("/t{task}x{index}");
                let handle = fs.open(&path, "w+", 0o644).await.expect("create");
                let tag = format!("{task}:{index}");
                handle.write(tag.as_bytes(), Some(0)).await.expect("write");
                handles.push((path, tag, handle));
            }
            for (path, tag, handle) in handles {
                // A second open shares the buffer, so it reads bytes the
                // store has not been given.
                let again = fs.open(&path, "r", 0).await.expect("open again");
                assert_eq!(read_handle(&again, tag.len()).await, tag.as_bytes());
                again.close().await.expect("close again");
                handle.close().await.expect("close");
            }
        }));
    }
    for task in tasks {
        task.await.expect("join");
    }
    for task in 0..TASKS {
        for index in 0..FILES {
            assert_eq!(
                store.bytes(&format!("t{task}x{index}")),
                Some(format!("{task}:{index}").into_bytes())
            );
        }
    }
}

#[tokio::test]
async fn writes_through_a_handle_stamp_the_file_under_its_current_name() {
    let (_store, fs) = setup();
    fs.write_file("/f", b"aaaa").await.expect("write");
    let ino = fs.stat("/f").await.expect("stat").ino;
    let handle = fs.open("/f", "r+", 0).await.expect("open");
    handle.write(b"b", Some(0)).await.expect("first write");

    // Attributes set by path are the ones the handle keeps stamping.
    fs.chmod("/f", 0o600).await.expect("chmod");
    fs.utimes("/f", 1000, 2000).await.expect("utimes");
    handle.write(b"c", Some(1)).await.expect("second write");
    let stats = fs.stat("/f").await.expect("stat");
    assert_eq!((stats.ino, stats.mode & 0o777), (ino, 0o600));
    assert_eq!(stats.atime_ms, 1000);
    assert!(stats.mtime_ms > 2000 && stats.ctime_ms == stats.mtime_ms);

    // After a rename the handle stamps the new name, not the old one.
    fs.rename("/f", "/g").await.expect("rename");
    fs.utimes("/g", 3000, 4000).await.expect("utimes renamed");
    handle.write(b"d", Some(2)).await.expect("third write");
    let stats = fs.stat("/g").await.expect("stat renamed");
    assert_eq!((stats.ino, stats.mode & 0o777), (ino, 0o600));
    assert_eq!(stats.atime_ms, 3000);
    assert!(stats.mtime_ms > 4000);
    assert_eq!(
        fs.stat("/f").await.expect_err("old name").code,
        ErrorCode::Enoent
    );

    // A file created over an unlinked name starts with its own attributes.
    fs.unlink("/g").await.expect("unlink");
    fs.write_file("/g", b"new").await.expect("recreate");
    let recreated = fs.stat("/g").await.expect("stat recreated");
    assert_ne!(recreated.ino, ino);
    assert_ne!(recreated.mode & 0o777, 0o600);
    handle.close().await.expect("close");
}

#[tokio::test]
async fn an_open_does_not_keep_contents_read_before_another_buffer_was_saved() {
    let (store, fs) = setup();
    let fs = Arc::new(fs);
    fs.write_file("/f", &[0, 0]).await.expect("write");

    // The first open finds no buffer and reads the store; its answer is
    // on the way back.
    store
        .held_get
        .armed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let opening = {
        let fs = Arc::clone(&fs);
        tokio::spawn(async move { fs.open("/f", "r+", 0).await })
    };
    store.held_get.entered.notified().await;

    // Meanwhile the file is opened, written, saved and closed: its buffer
    // has come and gone.
    let other = fs.open("/f", "r+", 0).await.expect("open other");
    other.write(&[1], Some(0)).await.expect("write other");
    other.close().await.expect("close other");
    assert_eq!(store.bytes("f"), Some(vec![1, 0]));

    // The first open must not build its buffer from what it read.
    store.held_get.resume.notify_one();
    let first = opening.await.expect("join").expect("open first");
    first.write(&[1], Some(1)).await.expect("write first");
    first.close().await.expect("close first");
    assert_eq!(
        store.bytes("f"),
        Some(vec![1, 1]),
        "a saved write was undone"
    );
}

/// A write that is told Ok must be in the store once the handle's close
/// has returned, however the two race: the close either waits for the
/// write and saves it, or the write finds the handle closed. The losing
/// order needs a write to be descheduled between two adjacent statements
/// while a whole close runs, so this run guards the property without being
/// able to promise it would catch the fault.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_acknowledged_while_the_handle_closes_is_saved() {
    let (store, fs) = setup();
    let fs = Arc::new(fs);
    for round in 0..3000_u32 {
        fs.write_file("/f", &[0]).await.expect("seed");
        let handle = fs.open("/f", "r+", 0).await.expect("open");
        let start = Arc::new(tokio::sync::Barrier::new(2));
        let writer = {
            let (handle, start) = (Arc::clone(&handle), Arc::clone(&start));
            tokio::spawn(async move {
                start.wait().await;
                let mut acknowledged = 0_u8;
                for value in 1..=40_u8 {
                    match handle.write(&[value], Some(0)).await {
                        Ok(_) => acknowledged = value,
                        Err(error) => {
                            assert_eq!(error.code, ErrorCode::Ebadf);
                            break;
                        }
                    }
                    if value % 8 == 0 {
                        tokio::task::yield_now().await;
                    }
                }
                acknowledged
            })
        };
        let closer = {
            let (handle, start) = (Arc::clone(&handle), Arc::clone(&start));
            tokio::spawn(async move {
                start.wait().await;
                for _ in 0..round % 7 {
                    std::hint::spin_loop();
                }
                handle.close().await.expect("close");
            })
        };
        let acknowledged = writer.await.expect("writer");
        closer.await.expect("closer");
        assert_eq!(
            store.bytes("f"),
            Some(vec![acknowledged]),
            "round {round}: an acknowledged write is not in the store"
        );
    }
}

/// Two calls on one handle that both write at the handle's cursor must
/// land one after the other: each loads the cursor, writes there and
/// advances it inside the entry's lock. There is no seam inside that
/// section (it never calls the store), so this is a stress run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cursor_writes_racing_on_one_handle_each_land_at_their_own_offset() {
    const TASKS: usize = 4;
    const WRITES: usize = 2000;
    let (store, fs) = setup();
    // Already full length, so the writes only fill it in and a missing
    // one shows as a byte left at zero.
    fs.write_file("/f", &vec![0; TASKS * WRITES])
        .await
        .expect("seed");
    let handle = fs.open("/f", "r+", 0).await.expect("open");
    let start = Arc::new(tokio::sync::Barrier::new(TASKS));
    let mut tasks = Vec::new();
    for task in 0..TASKS {
        let (handle, start) = (Arc::clone(&handle), Arc::clone(&start));
        tasks.push(tokio::spawn(async move {
            start.wait().await;
            for _ in 0..WRITES {
                handle.write(&[task as u8 + 1], None).await.expect("write");
            }
        }));
    }
    for task in tasks {
        task.await.expect("join");
    }
    handle.close().await.expect("close");
    let stored = store.bytes("f").expect("stored");
    assert_eq!(
        stored.len(),
        TASKS * WRITES,
        "cursor writes landed on top of each other"
    );
    for task in 0..TASKS {
        let landed = stored
            .iter()
            .filter(|byte| **byte == task as u8 + 1)
            .count();
        assert_eq!(landed, WRITES, "task {task} lost cursor writes");
    }
}
