use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use mount_rs_core::{Capabilities, FileHandle, FsError, Loopback, MkdirOptions, OpenFlags, Stats};
use mount_rs_kv::{KeyValueMetadata, KeyValueStore, UnstorageOptions, create_unstorage_driver};
use serde_json::{Value, json};
use tokio::sync::Notify;

#[derive(Clone, Default)]
struct MemoryStore {
    state: Arc<Mutex<StoreState>>,
}

#[derive(Default)]
struct StoreState {
    values: BTreeMap<String, Vec<u8>>,
    metadata: BTreeMap<String, KeyValueMetadata>,
    fail_next_set: bool,
    delayed_set: Option<SetGate>,
}

#[derive(Clone)]
struct SetGate {
    reached: Arc<Notify>,
    release: Arc<Notify>,
}

impl MemoryStore {
    fn put(&self, key: &str, value: &[u8]) {
        self.state
            .lock()
            .expect("oracle store lock")
            .values
            .insert(key.to_owned(), value.to_vec());
    }

    fn put_meta(&self, key: &str, metadata: KeyValueMetadata) {
        self.state
            .lock()
            .expect("oracle store lock")
            .metadata
            .insert(key.to_owned(), metadata);
    }

    fn fail_next_set(&self) {
        self.state.lock().expect("oracle store lock").fail_next_set = true;
    }

    fn delay_next_set(&self) -> SetGate {
        let gate = SetGate {
            reached: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        self.state.lock().expect("oracle store lock").delayed_set = Some(gate.clone());
        gate
    }

    fn value(&self, key: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .expect("oracle store lock")
            .values
            .get(key)
            .cloned()
    }
}

impl KeyValueStore for MemoryStore {
    type Error = String;

    fn has_item<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<bool, Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .map_err(|_| "oracle store lock poisoned".to_owned())?
                .values
                .contains_key(key))
        })
    }

    fn get_item_raw<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Vec<u8>>, Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .map_err(|_| "oracle store lock poisoned".to_owned())?
                .values
                .get(key)
                .cloned())
        })
    }

    fn set_item_raw<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
        value: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let delayed = self
                .state
                .lock()
                .map_err(|_| "oracle store lock poisoned".to_owned())?
                .delayed_set
                .take();
            if let Some(gate) = delayed {
                gate.reached.notify_one();
                gate.release.notified().await;
            }
            let mut state = self
                .state
                .lock()
                .map_err(|_| "oracle store lock poisoned".to_owned())?;
            if state.fail_next_set {
                state.fail_next_set = false;
                return Err("transient write failure".to_owned());
            }
            state.values.insert(key.to_owned(), value);
            Ok(())
        })
    }

    fn remove_item<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<(), Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.state
                .lock()
                .map_err(|_| "oracle store lock poisoned".to_owned())?
                .values
                .remove(key);
            self.state
                .lock()
                .map_err(|_| "oracle store lock poisoned".to_owned())?
                .metadata
                .remove(key);
            Ok(())
        })
    }

    fn get_keys<'a, 'b, 'async_trait>(
        &'a self,
        prefix: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<String>, Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            let state = self
                .state
                .lock()
                .map_err(|_| "oracle store lock poisoned".to_owned())?;
            let child_prefix = if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}:")
            };
            Ok(state
                .values
                .keys()
                .filter(|key| {
                    prefix.is_empty() || key.as_str() == prefix || key.starts_with(&child_prefix)
                })
                .cloned()
                .collect())
        })
    }

    fn get_meta<'a, 'b, 'async_trait>(
        &'a self,
        key: &'b str,
    ) -> Pin<Box<dyn Future<Output = Result<KeyValueMetadata, Self::Error>> + Send + 'async_trait>>
    where
        'a: 'async_trait,
        'b: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            Ok(self
                .state
                .lock()
                .map_err(|_| "oracle store lock poisoned".to_owned())?
                .metadata
                .get(key)
                .copied()
                .unwrap_or_default())
        })
    }
}

fn options() -> UnstorageOptions {
    UnstorageOptions {
        uid: 1000,
        gid: 1001,
        file_mode: 0o644,
        dir_mode: 0o755,
        read_only: false,
    }
}

fn fs(store: MemoryStore) -> Loopback {
    Loopback::new(create_unstorage_driver(store, options()))
}

fn error_shape(error: &FsError) -> Value {
    json!({
        "code": error.code.as_str(),
        "syscall": error.syscall,
        "path": error.path,
        "dest": error.dest,
    })
}

fn error_with_message(error: &FsError) -> Value {
    let mut value = error_shape(error);
    value["message"] = Value::String(error.to_string());
    value
}

async fn capture<T>(
    operation: impl std::future::Future<Output = mount_rs_core::Result<T>>,
) -> Value {
    match operation.await {
        Ok(_) => Value::Null,
        Err(error) => error_shape(&error),
    }
}

async fn capture_message<T>(
    operation: impl std::future::Future<Output = mount_rs_core::Result<T>>,
) -> Value {
    match operation.await {
        Ok(_) => Value::Null,
        Err(error) => error_with_message(&error),
    }
}

fn stable_stats(stats: &Stats, include_times: bool) -> Value {
    let mut value = json!({
        "mode": stats.mode,
        "nlink": stats.nlink,
        "uid": stats.uid,
        "gid": stats.gid,
        "size": stats.size,
        "blksize": stats.blksize,
        "blocks": stats.blocks,
        "isFile": stats.is_file(),
        "isDirectory": stats.is_directory(),
        "isSymbolicLink": stats.is_symbolic_link(),
        "isBlockDevice": stats.is_block_device(),
        "isCharacterDevice": stats.is_character_device(),
        "isFIFO": stats.is_fifo(),
        "isSocket": stats.is_socket(),
    });
    if include_times {
        value["atimeMs"] = json!(stats.atime_ms);
        value["mtimeMs"] = json!(stats.mtime_ms);
        value["birthtimeMs"] = json!(stats.birthtime_ms);
    }
    value
}

fn stable_capabilities(capabilities: Capabilities) -> Value {
    json!({
        "handles": capabilities.handles,
        "hardlinks": capabilities.hardlinks,
        "symlinks": capabilities.symlinks,
        "permissions": capabilities.permissions,
        "times": capabilities.times,
        "truncate": capabilities.truncate,
        "atomicRename": capabilities.atomic_rename,
        "caseSensitive": capabilities.case_sensitive,
        "statfs": capabilities.statfs,
        "readOnly": capabilities.read_only,
        "durableWrites": capabilities.durable_writes,
        "mknod": capabilities.mknod,
    })
}

fn sorted_entries(entries: Vec<mount_rs_core::DirEntry>) -> Value {
    let mut entries = entries
        .into_iter()
        .map(|entry| {
            json!({
                "name": entry.name,
                "kind": if entry.is_directory() { "directory" } else { "file" },
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    json!(entries)
}

async fn read_at(handle: &Arc<dyn FileHandle>, length: usize, position: u64) -> Vec<u8> {
    let mut buffer = vec![0; length];
    let count = handle.read(&mut buffer, Some(position)).await.unwrap();
    buffer.truncate(count);
    buffer
}

async fn basic_scenario() -> Value {
    let store = MemoryStore::default();
    store.put("note", b"text");
    store.put("note$", b"hidden metadata");
    store.put("shadow", b"file wins");
    store.put("shadow:child", b"unreachable");
    store.put("meta", b"four");
    store.put_meta(
        "meta",
        KeyValueMetadata {
            size: Some(1234),
            mtime_ms: Some(1_700_000_000_000),
            ..KeyValueMetadata::default()
        },
    );

    let fs = fs(store.clone());
    fs.mkdir(
        "/a/b",
        MkdirOptions {
            recursive: true,
            mode: None,
        },
    )
    .await
    .unwrap();
    fs.write_file("/a/b/c.bin", &[0, 1, 254, 255])
        .await
        .unwrap();

    let binary = fs.read_file("/a/b/c.bin").await.unwrap();
    let nested = sorted_entries(fs.readdir("/a").await.unwrap());
    let root_entries = sorted_entries(fs.readdir("/").await.unwrap());
    let binary_stats = stable_stats(&fs.stat("/a/b/c.bin").await.unwrap(), false);

    let metadata_stats = stable_stats(&fs.stat("/meta").await.unwrap(), true);
    fs.chmod("/meta", 0o600).await.unwrap();
    fs.utimes("/meta", 1000, 2000).await.unwrap();
    let metadata_overlay = stable_stats(&fs.stat("/meta").await.unwrap(), true);

    let invalid = json!({
        "colon": {
            "write": capture(fs.write_file("/a:b", b"x")).await,
            "stat": capture(fs.stat("/a:b")).await,
            "mkdir": capture(fs.mkdir("/a:b", MkdirOptions::default())).await,
        },
        "question": {
            "write": capture(fs.write_file("/a?b", b"x")).await,
            "stat": capture(fs.stat("/a?b")).await,
            "mkdir": capture(fs.mkdir("/a?b", MkdirOptions::default())).await,
        },
        "metadataSuffix": {
            "write": capture(fs.write_file("/meta$", b"x")).await,
            "stat": capture(fs.stat("/meta$")).await,
            "mkdir": capture(fs.mkdir("/meta$", MkdirOptions::default())).await,
        },
        "missing": capture(fs.stat("/missing")).await,
        "missingChild": capture(fs.stat("/missing/child")).await,
    });

    let shadow = json!({
        "kind": if fs.stat("/shadow").await.unwrap().is_file() { "file" } else { "directory" },
        "child": capture(fs.stat("/shadow/child")).await,
    });

    let create_bits = 0o100 | 2;
    let create_flags = OpenFlags::from_bits(create_bits);
    let numeric_create = fs
        .open_flags("/numeric", create_flags, 0o640)
        .await
        .unwrap();
    numeric_create.write(b"n", Some(0)).await.unwrap();
    numeric_create.close().await.unwrap();
    let append_bits = 0o100 | 1 | 0o2000;
    let append_flags = OpenFlags::from_bits(append_bits);
    let numeric_append = fs.open_flags("/numeric", append_flags, 0).await.unwrap();
    numeric_append.write(b"+a", Some(0)).await.unwrap();
    numeric_append.close().await.unwrap();
    let numeric_stats = stable_stats(&fs.stat("/numeric").await.unwrap(), false);

    json!({
        "binary": binary,
        "nestedEntries": nested,
        "rootEntries": root_entries,
        "binaryStats": binary_stats,
        "metadataStats": metadata_stats,
        "metadataOverlay": metadata_overlay,
        "invalid": invalid,
        "shadow": shadow,
        "numeric": {
            "createBits": create_bits,
            "appendBits": append_bits,
            "data": store.value("numeric"),
            "stats": numeric_stats,
        },
        "capabilities": stable_capabilities(fs.capabilities),
        "stored": {
            "a:b:c.bin": store.value("a:b:c.bin"),
            "note": store.value("note"),
            "note$": store.value("note$"),
            "meta": store.value("meta"),
        },
    })
}

async fn handles_scenario() -> Value {
    let shared_store = MemoryStore::default();
    let shared_fs = fs(shared_store.clone());
    shared_fs.write_file("/shared", b"aaaa").await.unwrap();
    let writer = shared_fs.open("/shared", "r+", 0).await.unwrap();
    let reader = shared_fs.open("/shared", "r", 0).await.unwrap();
    writer.write(b"bb", Some(0)).await.unwrap();
    let visible = read_at(&reader, 4, 0).await;
    let before_sync = shared_store.value("shared");
    let before_sync_stats = stable_stats(&shared_fs.stat("/shared").await.unwrap(), false);
    writer.sync().await.unwrap();
    let after_sync = shared_store.value("shared");
    writer.close().await.unwrap();
    reader.close().await.unwrap();

    let failure_store = MemoryStore::default();
    let failure_fs = fs(failure_store.clone());
    failure_fs.write_file("/failure", b"aaaa").await.unwrap();
    let failure_handle = failure_fs.open("/failure", "r+", 0).await.unwrap();
    failure_handle.write(b"bbbb", Some(0)).await.unwrap();
    failure_store.fail_next_set();
    let failure = capture_message(failure_handle.close()).await;
    let retry = failure_fs.open("/failure", "r+", 0).await.unwrap();
    retry.sync().await.unwrap();
    retry.close().await.unwrap();
    failure_handle.close().await.unwrap();

    let truncate_store = MemoryStore::default();
    let truncate_fs = fs(truncate_store.clone());
    truncate_fs.write_file("/truncate", b"aaaa").await.unwrap();
    let truncate_handle = truncate_fs.open("/truncate", "r+", 0).await.unwrap();
    truncate_handle.write(b"bbbb", Some(0)).await.unwrap();
    truncate_store.fail_next_set();
    let _ = capture_message(truncate_handle.close()).await;
    truncate_fs.truncate("/truncate", 2).await.unwrap();

    let unlink_store = MemoryStore::default();
    let unlink_fs = fs(unlink_store.clone());
    unlink_fs
        .write_file("/doomed", b"still here")
        .await
        .unwrap();
    let doomed = unlink_fs.open("/doomed", "r+", 0).await.unwrap();
    unlink_fs.unlink("/doomed").await.unwrap();
    let orphan_read = read_at(&doomed, 10, 0).await;
    let orphan_stat = stable_stats(&doomed.stat().await.unwrap(), false);
    doomed.write(b"x", Some(0)).await.unwrap();
    doomed.close().await.unwrap();

    let replaced_store = MemoryStore::default();
    let replaced_fs = fs(replaced_store.clone());
    replaced_fs.write_file("/source", b"source").await.unwrap();
    replaced_fs.write_file("/target", b"target").await.unwrap();
    let replaced = replaced_fs.open("/target", "r+", 0).await.unwrap();
    replaced_fs.rename("/source", "/target").await.unwrap();
    replaced.write(b"zzzzzz", Some(0)).await.unwrap();
    replaced.close().await.unwrap();

    let renamed_store = MemoryStore::default();
    let renamed_fs = fs(renamed_store.clone());
    renamed_fs.write_file("/from", b"aaaa").await.unwrap();
    let renamed = renamed_fs.open("/from", "r+", 0).await.unwrap();
    renamed.write(b"bb", Some(0)).await.unwrap();
    renamed_fs.rename("/from", "/to").await.unwrap();
    renamed.write(b"cc", Some(2)).await.unwrap();
    renamed.close().await.unwrap();

    let zero_store = MemoryStore::default();
    let zero_fs = fs(zero_store.clone());
    zero_fs.write_file("/zero", b"x").await.unwrap();
    let zero = zero_fs.open("/zero", "r+", 0).await.unwrap();
    zero.write(b"", Some(3)).await.unwrap();
    let zero_stats = stable_stats(&zero_fs.stat("/zero").await.unwrap(), false);
    zero.close().await.unwrap();

    json!({
        "shared": {
            "visibleBeforeSync": visible,
            "storedBeforeSync": before_sync,
            "statsBeforeSync": before_sync_stats,
            "storedAfterSync": after_sync,
        },
        "flushFailure": {
            "error": failure,
            "storedAfterRetry": failure_store.value("failure"),
        },
        "truncateAfterFailure": truncate_store.value("truncate"),
        "unlink": {
            "read": orphan_read,
            "stat": orphan_stat,
            "stored": unlink_store.value("doomed"),
        },
        "replacedDestination": {
            "target": replaced_fs.read_file("/target").await.unwrap(),
            "source": replaced_store.value("source"),
        },
        "renamedOpen": {
            "from": renamed_store.value("from"),
            "to": renamed_store.value("to"),
        },
        "zeroPositionWrite": {
            "data": zero_store.value("zero"),
            "stats": zero_stats,
        },
    })
}

async fn edge_scenario() -> Value {
    let shared_store = MemoryStore::default();
    let shared_fs = fs(shared_store.clone());
    shared_fs.write_file("/shared", b"abcdef").await.unwrap();
    let reader = shared_fs.open("/shared", "r", 0).await.unwrap();
    let truncating = shared_fs.open("/shared", "w", 0).await.unwrap();
    let mut reader_buffer = vec![0; 6];
    let reader_count = reader.read(&mut reader_buffer, Some(0)).await.unwrap();
    reader_buffer.truncate(reader_count);
    let shared_before_close = json!({
        "storedBeforeClose": shared_store.value("shared"),
        "readerAfterTruncate": reader_buffer,
        "statsAfterTruncate": stable_stats(&shared_fs.stat("/shared").await.unwrap(), false),
    });
    truncating.close().await.unwrap();
    reader.close().await.unwrap();
    let shared = json!({
        "storedBeforeClose": shared_before_close["storedBeforeClose"].clone(),
        "readerAfterTruncate": shared_before_close["readerAfterTruncate"].clone(),
        "statsAfterTruncate": shared_before_close["statsAfterTruncate"].clone(),
        "storedAfterClose": shared_store.value("shared"),
    });

    let pending_store = MemoryStore::default();
    let pending_fs = fs(pending_store.clone());
    pending_fs.write_file("/pending", b"aaaa").await.unwrap();
    let first = pending_fs.open("/pending", "r+", 0).await.unwrap();
    let second = pending_fs.open("/pending", "r+", 0).await.unwrap();
    first.write(b"bbbb", Some(0)).await.unwrap();
    let gate = pending_store.delay_next_set();
    let syncing = tokio::spawn({
        let first = Arc::clone(&first);
        async move { first.sync().await }
    });
    gate.reached.notified().await;
    second.write(b"cccc", Some(0)).await.unwrap();
    gate.release.notify_one();
    syncing.await.unwrap().unwrap();
    first.close().await.unwrap();
    second.close().await.unwrap();
    let pending_flush = pending_store.value("pending");

    let renamed_store = MemoryStore::default();
    let renamed_fs = fs(renamed_store.clone());
    renamed_fs.write_file("/from", b"old").await.unwrap();
    let clean = renamed_fs.open("/from", "r", 0).await.unwrap();
    renamed_fs.rename("/from", "/to").await.unwrap();
    clean.close().await.unwrap();
    renamed_store.put("to", b"fresh");
    let clean_rename = json!({
        "from": renamed_store.value("from"),
        "to": renamed_fs.read_file("/to").await.unwrap(),
    });

    let metadata_store = MemoryStore::default();
    let metadata_fs = fs(metadata_store.clone());
    metadata_store.put("meta", b"old");
    metadata_store.put_meta(
        "meta",
        KeyValueMetadata {
            size: Some(1234),
            ..KeyValueMetadata::default()
        },
    );
    metadata_fs.unlink("/meta").await.unwrap();
    let recreated = metadata_fs.open("/meta", "w", 0o666).await.unwrap();
    recreated.close().await.unwrap();
    let metadata_after_recreate = stable_stats(&metadata_fs.stat("/meta").await.unwrap(), false);

    let operations_store = MemoryStore::default();
    let operations_fs = fs(operations_store);
    operations_fs.write_file("/file", b"x").await.unwrap();
    operations_fs
        .mkdir("/empty", MkdirOptions::default())
        .await
        .unwrap();
    operations_fs
        .mkdir("/nonempty", MkdirOptions::default())
        .await
        .unwrap();
    operations_fs
        .write_file("/nonempty/child", b"x")
        .await
        .unwrap();
    operations_fs
        .mkdir("/source", MkdirOptions::default())
        .await
        .unwrap();
    operations_fs
        .write_file("/source/child", b"x")
        .await
        .unwrap();
    operations_fs
        .mkdir("/destination", MkdirOptions::default())
        .await
        .unwrap();
    operations_fs
        .write_file("/destination/child", b"x")
        .await
        .unwrap();
    let recursive_first = operations_fs
        .mkdir(
            "/created/leaf",
            MkdirOptions {
                recursive: true,
                mode: None,
            },
        )
        .await
        .unwrap();
    let recursive_again = operations_fs
        .mkdir(
            "/created/leaf",
            MkdirOptions {
                recursive: true,
                mode: None,
            },
        )
        .await
        .unwrap();
    let operation_errors = json!({
        "mkdirExisting": capture(operations_fs.mkdir("/file", MkdirOptions::default())).await,
        "mkdirThroughFile": capture(
            operations_fs.mkdir(
                "/file/child",
                MkdirOptions {
                    recursive: true,
                    mode: None,
                },
            ),
        )
        .await,
        "rmdirFile": capture(operations_fs.rmdir("/file")).await,
        "rmdirMissing": capture(operations_fs.rmdir("/missing")).await,
        "rmdirNonempty": capture(operations_fs.rmdir("/nonempty")).await,
        "rmdirRoot": capture(operations_fs.rmdir("/")).await,
        "unlinkDirectory": capture(operations_fs.unlink("/empty")).await,
        "renameMissing": capture(operations_fs.rename("/missing", "/new")).await,
        "renameFileToDirectory": capture(operations_fs.rename("/file", "/empty")).await,
        "renameDirectoryToFile": capture(operations_fs.rename("/source", "/file")).await,
        "renameDirectoryIntoSelf": capture(
            operations_fs.rename("/source", "/source/child/deeper"),
        )
        .await,
        "renameDirectoryNonempty": capture(
            operations_fs.rename("/source", "/destination"),
        )
        .await,
    });

    let handle_store = MemoryStore::default();
    let handle_fs = fs(handle_store);
    handle_fs.write_file("/f", b"x").await.unwrap();
    let read_only_handle = handle_fs.open("/f", "r", 0).await.unwrap();
    let directory_handle = operations_fs.open("/empty", "r", 0).await.unwrap();
    let mut one_byte = [0; 1];
    let handle_errors = json!({
        "writeOnReadOnly": capture(read_only_handle.write(b"y", Some(0))).await,
        "truncateOnReadOnly": capture(read_only_handle.truncate(0)).await,
        "directoryRead": capture(directory_handle.read(&mut one_byte, Some(0))).await,
        "directoryWriteOpen": capture(operations_fs.open("/empty", "w", 0)).await,
    });
    directory_handle.close().await.unwrap();
    read_only_handle.close().await.unwrap();
    let read_after_close = capture(read_only_handle.read(&mut one_byte, Some(0))).await;

    json!({
        "sharedTruncate": shared,
        "pendingFlush": pending_flush,
        "cleanRename": clean_rename,
        "metadataAfterRecreate": metadata_after_recreate,
        "operations": {
            "recursiveFirst": recursive_first,
            "recursiveAgain": recursive_again,
            "errors": operation_errors,
            "source": operations_fs.read_file("/source/child").await.unwrap(),
            "destination": operations_fs.read_file("/destination/child").await.unwrap(),
        },
        "handleErrors": {
            "writeOnReadOnly": handle_errors["writeOnReadOnly"].clone(),
            "truncateOnReadOnly": handle_errors["truncateOnReadOnly"].clone(),
            "directoryRead": handle_errors["directoryRead"].clone(),
            "directoryWriteOpen": handle_errors["directoryWriteOpen"].clone(),
            "readAfterClose": read_after_close,
        },
    })
}

async fn resolution_scenario() -> Value {
    let shadow_store = MemoryStore::default();
    shadow_store.put("a:b", b"shadowing");
    shadow_store.put("a:b:c:d", b"deep");
    let shadow_fs = fs(shadow_store);
    let below = json!({
        "/a/b/c": capture(shadow_fs.stat("/a/b/c")).await,
        "/a/b/c/d/e": capture(shadow_fs.stat("/a/b/c/d/e")).await,
        "/a/b/zz": capture(shadow_fs.stat("/a/b/zz")).await,
    });
    let missing = json!({
        "/a/zz/c": capture(shadow_fs.stat("/a/zz/c")).await,
        "/zz/b/c": capture(shadow_fs.stat("/zz/b/c")).await,
    });

    let truncate_store = MemoryStore::default();
    let truncate_fs = fs(truncate_store.clone());
    truncate_store.put("f", b"abcdef");
    truncate_fs.truncate("/f", 3).await.unwrap();

    json!({
        "shadow": {
            "kind": if shadow_fs.stat("/a/b").await.unwrap().is_file() {
                "file"
            } else {
                "directory"
            },
            "below": below,
            "leaf": if shadow_fs.stat("/a/b/c/d").await.unwrap().is_file() {
                "file"
            } else {
                "directory"
            },
            "missing": missing,
        },
        "nonzeroTruncate": truncate_fs.read_file("/f").await.unwrap(),
    })
}

async fn readonly_scenario() -> Value {
    let store = MemoryStore::default();
    store.put("f", b"content");
    let read_only = Loopback::new(create_unstorage_driver(
        store.clone(),
        UnstorageOptions {
            read_only: true,
            ..options()
        },
    ));
    let read = read_only.read_file("/f").await.unwrap();
    let create_append = OpenFlags::from_bits(0o100 | 1 | 0o2000);
    let errors = json!({
        "openExisting": capture(read_only.open("/f", "w", 0)).await,
        "openNew": capture(read_only.open_flags("/new", create_append, 0)).await,
        "mkdir": capture(read_only.mkdir("/dir", MkdirOptions::default())).await,
        "rmdir": capture(read_only.rmdir("/dir")).await,
        "unlink": capture(read_only.unlink("/f")).await,
        "rename": capture(read_only.rename("/f", "/g")).await,
        "truncate": capture(read_only.truncate("/f", 0)).await,
        "chmod": capture(read_only.chmod("/f", 0o600)).await,
        "chown": capture(read_only.chown("/f", 0, 0)).await,
        "utimes": capture(read_only.utimes("/f", 1000, 2000)).await,
    });

    let unsupported_store = MemoryStore::default();
    let unsupported_fs = fs(unsupported_store.clone());
    unsupported_fs.write_file("/f", b"x").await.unwrap();
    let unsupported = json!({
        "link": capture(unsupported_fs.link("/f", "/g")).await,
        "symlink": capture(unsupported_fs.symlink("f", "/g")).await,
        "readlink": capture(unsupported_fs.readlink("/f")).await,
        "statfs": capture(unsupported_fs.statfs("/")).await,
    });

    json!({
        "read": read,
        "capabilities": stable_capabilities(read_only.capabilities),
        "errors": errors,
        "unsupported": unsupported,
        "stored": store.value("f"),
    })
}

#[tokio::main]
async fn main() {
    let output = json!({
        "basic": basic_scenario().await,
        "handles": handles_scenario().await,
        "edges": edge_scenario().await,
        "resolution": resolution_scenario().await,
        "readonly": readonly_scenario().await,
    });
    println!("{}", serde_json::to_string(&output).expect("oracle JSON"));
}

impl fmt::Debug for MemoryStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryStore")
            .finish_non_exhaustive()
    }
}
