#![cfg(any(target_os = "linux", target_os = "macos"))]

use mount_rs_core::ErrorCode;
use mount_rs_core::storage::{BlockId, BlockStore, ConcurrentBackingId};
use mount_rs_filesystem_blocks::FilesystemBlockStore;
use std::collections::BTreeSet;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const MARKER: &str = "_mount-rs-backing-id-v1";

fn root() -> (tempfile::TempDir, PathBuf) {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().canonicalize().unwrap().join("blocks");
    std::fs::create_dir(&path).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    (parent, path)
}

fn object_path(root: &Path, id: &BlockId) -> PathBuf {
    root.join(&id.0[1..3]).join(&id.0)
}

#[tokio::test]
async fn durable_roundtrip_reopens_the_same_backing_identity() {
    let (_parent, root) = root();
    let first = FilesystemBlockStore::open(&root, true).unwrap();
    let identity = first.prepare_concurrent_backing().await.unwrap();
    let bytes = b"immutable file blocks survive an independent context";
    let id = first.put(bytes).await.unwrap();
    first.flush().await.unwrap();
    assert_eq!(first.get(&id).await.unwrap(), bytes);
    let second = FilesystemBlockStore::open(&root, true).unwrap();
    assert_eq!(second.prepare_concurrent_backing().await.unwrap(), identity);
    second.verify_concurrent_backing(identity).await.unwrap();
    assert_eq!(second.get_for_migration(&id).await.unwrap(), bytes);
    assert_eq!(second.put(bytes).await.unwrap(), id);
    assert!(second.durable());
}

#[tokio::test]
async fn filesystem_writeback_acknowledgments_do_not_claim_stable_storage() {
    let (_parent, root) = root();
    let first = FilesystemBlockStore::open(&root, true).unwrap();
    let identity = first.prepare_concurrent_backing().await.unwrap();
    let marker = std::fs::read(root.join(MARKER)).unwrap();
    let bytes =
        b"filesystem writeback retains complete immutable bytes across independent contexts";
    let id = first.put(bytes).await.unwrap();
    first.flush().await.unwrap();
    assert_eq!(first.get(&id).await.unwrap(), bytes);
    assert_eq!(std::fs::read(object_path(&root, &id)).unwrap(), bytes);

    let second = FilesystemBlockStore::open(&root, true).unwrap();
    assert_eq!(second.prepare_concurrent_backing().await.unwrap(), identity);
    second.verify_concurrent_backing(identity).await.unwrap();
    assert_eq!(std::fs::read(root.join(MARKER)).unwrap(), marker);
    assert_eq!(second.get_for_migration(&id).await.unwrap(), bytes);
    assert_eq!(second.put(bytes).await.unwrap(), id);
    second.flush().await.unwrap();
    assert_eq!(second.get(&id).await.unwrap(), bytes);
    assert!(
        !first.durable() && !second.durable(),
        "FS_WRITEBACK_DURABILITY_REGRESSION: filesystem acknowledgments must not claim stable storage"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn independent_contexts_concurrently_publish_identical_bytes() {
    let (_parent, root) = root();
    let first = Arc::new(FilesystemBlockStore::open(&root, true).unwrap());
    let second = Arc::new(FilesystemBlockStore::open(&root, true).unwrap());
    let mut tasks = tokio::task::JoinSet::new();
    for n in 0..16 {
        let store = if n % 2 == 0 {
            first.clone()
        } else {
            second.clone()
        };
        tasks.spawn(async move { store.put(b"the one immutable object").await.unwrap() });
    }
    let mut ids = BTreeSet::new();
    while let Some(result) = tasks.join_next().await {
        ids.insert(result.unwrap());
    }
    assert_eq!(ids.len(), 1);
    let id = ids.first().unwrap();
    assert_eq!(first.get(id).await.unwrap(), b"the one immutable object");
    assert_eq!(second.get(id).await.unwrap(), b"the one immutable object");
    assert_eq!(
        first.prepare_concurrent_backing().await.unwrap(),
        second.prepare_concurrent_backing().await.unwrap()
    );
    assert!(std::fs::read_dir(&root).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("_mount-rs-stage-")
    }));
}

#[tokio::test]
async fn different_roots_have_different_backing_identities() {
    let (_parent_a, a) = root();
    let (_parent_b, b) = root();
    let first = FilesystemBlockStore::open(a, true).unwrap();
    let second = FilesystemBlockStore::open(b, true).unwrap();
    let expected = first.prepare_concurrent_backing().await.unwrap();
    assert_ne!(second.prepare_concurrent_backing().await.unwrap(), expected);
    assert!(
        second
            .verify_concurrent_backing(expected)
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
}

#[tokio::test]
async fn corruption_is_rejected_without_overwriting_existing_object() {
    let (_parent, root) = root();
    let store = FilesystemBlockStore::open(&root, true).unwrap();
    let id = store.put(b"expected immutable bytes").await.unwrap();
    let path = object_path(&root, &id);
    std::fs::write(&path, b"corrupt existing bytes").unwrap();
    assert!(store.get(&id).await.unwrap_err().is(ErrorCode::Eio));
    assert!(
        store
            .put(b"expected immutable bytes")
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    assert_eq!(std::fs::read(path).unwrap(), b"corrupt existing bytes");
}

#[tokio::test]
async fn missing_or_modified_marker_invalidates_an_established_context() {
    for remove in [false, true] {
        let (_parent, root) = root();
        let store = FilesystemBlockStore::open(&root, true).unwrap();
        let id = store
            .put(b"must not use a changed authority")
            .await
            .unwrap();
        if remove {
            std::fs::remove_file(root.join(MARKER)).unwrap();
        } else {
            std::fs::write(root.join(MARKER), b"corrupt marker").unwrap();
        }
        assert!(
            store
                .prepare_concurrent_backing()
                .await
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert!(store.get(&id).await.unwrap_err().is(ErrorCode::Estale));
        assert!(
            store
                .put(b"new bytes")
                .await
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
        assert!(store.flush().await.unwrap_err().is(ErrorCode::Estale));
        assert!(
            FilesystemBlockStore::open(&root, true)
                .unwrap_err()
                .is(ErrorCode::Estale)
        );
    }
}

#[tokio::test]
async fn replacing_root_never_redirects_an_established_context() {
    let (_parent, root) = root();
    let store = FilesystemBlockStore::open(&root, true).unwrap();
    let id = store.put(b"old root bytes").await.unwrap();
    let displaced = root.with_file_name("displaced");
    std::fs::rename(&root, &displaced).unwrap();
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let replacement = FilesystemBlockStore::open(&root, true).unwrap();
    assert!(store.get(&id).await.unwrap_err().is(ErrorCode::Estale));
    assert!(
        store
            .put(b"must not write either root")
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
    assert_ne!(
        replacement.prepare_concurrent_backing().await.unwrap(),
        FilesystemBlockStore::open(displaced, true)
            .unwrap()
            .prepare_concurrent_backing()
            .await
            .unwrap()
    );
}

#[test]
fn open_rejects_symlink_root_or_ancestor() {
    let (parent, root) = root();
    let real_parent = parent.path().canonicalize().unwrap();
    let alias = real_parent.join("alias");
    symlink(&root, &alias).unwrap();
    assert!(FilesystemBlockStore::open(&alias, true).is_err());
    let ancestor_alias = real_parent.join("parent-alias");
    symlink(&real_parent, &ancestor_alias).unwrap();
    assert!(FilesystemBlockStore::open(ancestor_alias.join("blocks"), true).is_err());
}

#[tokio::test]
async fn symlink_shard_cannot_read_or_write_outside_root() {
    let (parent, root) = root();
    let store = FilesystemBlockStore::open(&root, true).unwrap();
    let id = store.put(b"shard test").await.unwrap();
    let shard = root.join(&id.0[1..3]);
    let displaced = root.join("displaced-shard");
    std::fs::rename(&shard, &displaced).unwrap();
    let outside = parent.path().canonicalize().unwrap().join("outside");
    std::fs::create_dir(&outside).unwrap();
    let sentinel = outside.join(&id.0);
    std::fs::write(&sentinel, b"outside unchanged").unwrap();
    symlink(&outside, &shard).unwrap();
    assert!(store.get(&id).await.is_err());
    assert!(store.put(b"shard test").await.is_err());
    assert_eq!(std::fs::read(sentinel).unwrap(), b"outside unchanged");
}

#[tokio::test]
async fn symlink_object_and_marker_are_never_followed() {
    let (parent, root) = root();
    let store = FilesystemBlockStore::open(&root, true).unwrap();
    let id = store.put(b"object test").await.unwrap();
    let path = object_path(&root, &id);
    std::fs::remove_file(&path).unwrap();
    let outside = parent.path().canonicalize().unwrap().join("outside-file");
    std::fs::write(&outside, b"outside unchanged").unwrap();
    symlink(&outside, &path).unwrap();
    assert!(store.get(&id).await.is_err());
    assert!(store.put(b"object test").await.is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"outside unchanged");
    std::fs::remove_file(root.join(MARKER)).unwrap();
    symlink(&outside, root.join(MARKER)).unwrap();
    assert!(store.flush().await.unwrap_err().is(ErrorCode::Estale));
    assert_eq!(std::fs::read(outside).unwrap(), b"outside unchanged");
}

#[tokio::test]
async fn malformed_block_ids_fail_before_any_path_lookup() {
    let (_parent, root) = root();
    let store = FilesystemBlockStore::open(&root, true).unwrap();
    for text in [
        "../escape",
        "",
        "b123",
        "b/../../escape",
        "B0123456789012345678901234567890123456789012345678901234567890123",
        "bFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF",
    ] {
        assert!(
            store
                .get(&BlockId(text.into()))
                .await
                .unwrap_err()
                .is(ErrorCode::Einval)
        );
    }
    let missing = BlockId(format!("b{}", "a".repeat(64)));
    assert!(store.get(&missing).await.unwrap_err().is(ErrorCode::Enoent));
}

#[test]
fn caller_must_supply_an_existing_private_root() {
    let (parent, root) = root();
    let missing = parent.path().canonicalize().unwrap().join("missing");
    assert!(
        FilesystemBlockStore::open(missing, true)
            .unwrap_err()
            .is(ErrorCode::Enoent)
    );
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        FilesystemBlockStore::open(root, true)
            .unwrap_err()
            .is(ErrorCode::Eacces)
    );
}

#[tokio::test]
async fn unsupported_maintenance_never_deletes_immutable_data() {
    let (_parent, root) = root();
    let store = FilesystemBlockStore::open(&root, false).unwrap();
    let id = store.put(b"retained").await.unwrap();
    assert!(!store.durable());
    assert!(store.delete(&id).await.unwrap_err().is(ErrorCode::Enotsup));
    assert!(
        store
            .reconcile(&BTreeSet::new(), Duration::ZERO)
            .await
            .unwrap_err()
            .is(ErrorCode::Enotsup)
    );
    assert_eq!(store.get(&id).await.unwrap(), b"retained");
}

#[tokio::test]
async fn zero_or_different_backing_identity_does_not_validate() {
    let (_parent, root) = root();
    let store = FilesystemBlockStore::open(root, true).unwrap();
    assert!(ConcurrentBackingId::from_bytes([0; 16]).is_err());
    let unrelated = ConcurrentBackingId::from_bytes([9; 16]).unwrap();
    assert!(
        store
            .verify_concurrent_backing(unrelated)
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
}

#[tokio::test]
async fn foreign_hardlinks_to_objects_or_marker_fail_closed() {
    let (parent, root) = root();
    let store = FilesystemBlockStore::open(&root, true).unwrap();
    let id = store.put(b"hardlink test").await.unwrap();
    let outside = parent
        .path()
        .canonicalize()
        .unwrap()
        .join("foreign-block-link");
    std::fs::hard_link(object_path(&root, &id), &outside).unwrap();
    assert!(store.get(&id).await.unwrap_err().is(ErrorCode::Eio));
    assert!(
        store
            .put(b"hardlink test")
            .await
            .unwrap_err()
            .is(ErrorCode::Eio)
    );
    assert_eq!(std::fs::read(&outside).unwrap(), b"hardlink test");
    std::fs::remove_file(outside).unwrap();
    let foreign_marker = parent
        .path()
        .canonicalize()
        .unwrap()
        .join("foreign-marker-link");
    std::fs::hard_link(root.join(MARKER), foreign_marker).unwrap();
    assert!(
        store
            .prepare_concurrent_backing()
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
    assert!(
        FilesystemBlockStore::open(root, true)
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
}

#[tokio::test]
async fn sparse_oversized_object_is_rejected_before_payload_allocation() {
    let (_parent, root) = root();
    let store = FilesystemBlockStore::open(&root, true).unwrap();
    let id = BlockId(format!("b{}", "0".repeat(64)));
    let shard = root.join("00");
    std::fs::create_dir(&shard).unwrap();
    std::fs::set_permissions(&shard, std::fs::Permissions::from_mode(0o700)).unwrap();
    let file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(shard.join(&id.0))
        .unwrap();
    file.set_permissions(std::fs::Permissions::from_mode(0o600))
        .unwrap();
    file.set_len(mount_rs_filesystem_blocks::MAX_BLOCK_BYTES as u64 + 1)
        .unwrap();
    assert!(store.get(&id).await.unwrap_err().is(ErrorCode::Efbig));
    assert!(
        store
            .get_for_migration(&id)
            .await
            .unwrap_err()
            .is(ErrorCode::Efbig)
    );
}

#[tokio::test]
async fn replacing_marker_with_identical_bytes_invalidates_an_established_context() {
    let (_parent, root) = root();
    let store = FilesystemBlockStore::open(&root, true).unwrap();
    let marker = root.join(MARKER);
    let bytes = std::fs::read(&marker).unwrap();
    let replacement = root.join("replacement-marker");
    std::fs::write(&replacement, bytes).unwrap();
    std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600)).unwrap();
    std::fs::rename(replacement, marker).unwrap();
    assert!(
        store
            .prepare_concurrent_backing()
            .await
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
}

#[test]
fn missing_marker_in_a_nonempty_root_is_never_repaired() {
    let (_parent, root) = root();
    std::fs::write(root.join("existing-data"), b"unmanaged").unwrap();
    assert!(
        FilesystemBlockStore::open(&root, true)
            .unwrap_err()
            .is(ErrorCode::Estale)
    );
    assert!(!root.join(MARKER).exists());
}
