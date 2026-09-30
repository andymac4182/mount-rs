use super::test_support::{Gate, Point};
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

fn store() -> (tempfile::TempDir, std::path::PathBuf, FilesystemBlockStore) {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().canonicalize().unwrap().join("blocks");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = FilesystemBlockStore::open(&root, true).unwrap();
    (parent, root, store)
}

fn object_path(root: &Path, id: &BlockId) -> std::path::PathBuf {
    root.join(&id.0[1..3]).join(&id.0)
}

#[tokio::test]
async fn file_barrier_failure_prevents_publication_and_acknowledgment() {
    let (_parent, root, store) = store();
    let bytes = b"file barrier must finish before publication";
    let id = content_id(bytes);
    store.inner.faults.fail_once(Point::File);
    assert!(store.put(bytes).await.unwrap_err().is(ErrorCode::Eio));
    assert!(!object_path(&root, &id).exists());
    assert_eq!(*store.inner.faults.events.lock().unwrap(), [Point::File]);
    assert_eq!(store.put(bytes).await.unwrap(), id);
    assert_eq!(store.get(&id).await.unwrap(), bytes);
}

#[tokio::test]
async fn directory_barrier_failure_rejects_acknowledgment_of_complete_visible_object() {
    let (_parent, root, store) = store();
    let bytes = b"published name still needs its directory barrier";
    let id = content_id(bytes);
    store.inner.faults.fail_once(Point::Directory);
    assert!(store.put(bytes).await.unwrap_err().is(ErrorCode::Eio));
    assert_eq!(std::fs::read(object_path(&root, &id)).unwrap(), bytes);
    assert_eq!(
        *store.inner.faults.events.lock().unwrap(),
        [Point::File, Point::Directory]
    );
    store.inner.faults.events.lock().unwrap().clear();
    assert_eq!(store.put(bytes).await.unwrap(), id);
    assert_eq!(
        *store.inner.faults.events.lock().unwrap(),
        [Point::File, Point::Directory, Point::Publication]
    );
}

#[tokio::test]
async fn duplicate_existing_object_requires_its_own_file_and_directory_barriers() {
    let (_parent, _root, store) = store();
    let bytes = b"an existing object does not imply a completed writer barrier";
    let id = store.put(bytes).await.unwrap();
    store.inner.faults.events.lock().unwrap().clear();
    store.inner.faults.fail_once(Point::File);
    assert!(store.put(bytes).await.unwrap_err().is(ErrorCode::Eio));
    assert_eq!(*store.inner.faults.events.lock().unwrap(), [Point::File]);
    store.inner.faults.events.lock().unwrap().clear();
    store.inner.faults.fail_once(Point::Directory);
    assert!(store.put(bytes).await.unwrap_err().is(ErrorCode::Eio));
    assert_eq!(
        *store.inner.faults.events.lock().unwrap(),
        [Point::File, Point::Directory]
    );
    assert_eq!(store.get(&id).await.unwrap(), bytes);
}

#[tokio::test]
async fn post_directory_device_barrier_must_finish_before_any_put_acknowledgment() {
    let (_parent, _root, store) = store();
    let bytes = b"directory entries precede the final device flush";
    store.inner.faults.fail_once(Point::Publication);
    assert!(store.put(bytes).await.unwrap_err().is(ErrorCode::Eio));
    assert_eq!(
        *store.inner.faults.events.lock().unwrap(),
        [Point::File, Point::Directory, Point::Publication]
    );
    store.inner.faults.events.lock().unwrap().clear();
    store.inner.faults.fail_once(Point::Publication);
    assert!(store.put(bytes).await.unwrap_err().is(ErrorCode::Eio));
    assert_eq!(
        *store.inner.faults.events.lock().unwrap(),
        [Point::File, Point::Directory, Point::Publication]
    );
    store.inner.faults.events.lock().unwrap().clear();
    store.put(bytes).await.unwrap();
    assert_eq!(
        *store.inner.faults.events.lock().unwrap(),
        [Point::File, Point::Directory, Point::Publication]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn aborting_the_async_waiter_retains_the_owned_publication_to_completion() {
    let (_parent, root, store) = store();
    let bytes = b"the blocking publication owns descriptors and its input";
    let id = content_id(bytes);
    let (started_tx, started_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    let (settled_tx, settled_rx) = sync_channel(1);
    *store.inner.faults.gate.lock().unwrap() = Some(Gate {
        started: started_tx,
        release: release_rx,
        settled: settled_tx,
    });
    let publishing = store.clone();
    let task = tokio::spawn(async move { publishing.put(bytes).await });
    tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    drop(store);
    release_tx.send(()).unwrap();
    tokio::task::spawn_blocking(move || settled_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    let reopened = FilesystemBlockStore::open(&root, true).unwrap();
    assert_eq!(reopened.get(&id).await.unwrap(), bytes);
    let shard = root.join(&id.0[1..3]);
    assert!(std::fs::read_dir(shard).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("_mount-rs-stage-")
    }));
}

#[test]
fn missing_tokio_runtime_returns_a_filesystem_error_without_panicking() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let (_parent, _root, store) = store();
    let id = content_id(b"runtime test");
    let mut future = std::pin::pin!(store.get(&id));
    let mut context = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut context) {
        Poll::Ready(Err(error)) => assert!(error.is(ErrorCode::Enotsup)),
        other => panic!("expected a ready unsupported-runtime error, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn substituted_staging_symlink_is_retained_and_never_published_or_followed() {
    use std::os::unix::fs::symlink;
    let (parent, root, store) = store();
    let bytes = b"only the owned stage may be published";
    let id = content_id(bytes);
    let (started_tx, started_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    let (settled_tx, _settled_rx) = sync_channel(1);
    *store.inner.faults.gate.lock().unwrap() = Some(Gate {
        started: started_tx,
        release: release_rx,
        settled: settled_tx,
    });
    let publishing = store.clone();
    let task = tokio::spawn(async move { publishing.put(bytes).await });
    tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .await
        .unwrap();
    let shard = root.join(&id.0[1..3]);
    let stage = std::fs::read_dir(&shard)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("_mount-rs-stage-")
        })
        .unwrap();
    std::fs::remove_file(&stage).unwrap();
    let outside = parent
        .path()
        .canonicalize()
        .unwrap()
        .join("outside-sentinel");
    std::fs::write(&outside, b"outside unchanged").unwrap();
    symlink(&outside, &stage).unwrap();
    release_tx.send(()).unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(!object_path(&root, &id).exists());
    assert!(
        std::fs::symlink_metadata(stage)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read(outside).unwrap(), b"outside unchanged");
}

#[test]
fn marker_initialization_and_reopen_require_file_root_parent_then_device_barriers() {
    let parent = tempfile::tempdir().unwrap();
    let root = parent.path().canonicalize().unwrap().join("blocks");
    std::fs::create_dir(&root).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut initial = Vec::new();
    FilesystemBlockStore::open_with_barriers(&root, true, |point, file| {
        initial.push(point);
        match point {
            OpenBarrier::StagingFile | OpenBarrier::MarkerFile => file_barrier(file),
            OpenBarrier::RootDirectory | OpenBarrier::ParentDirectory => {
                file.sync_all().map_err(io_error)
            }
            OpenBarrier::FinalDevice => final_device_barrier(file),
        }
    })
    .unwrap();
    assert_eq!(
        initial,
        [
            OpenBarrier::StagingFile,
            OpenBarrier::MarkerFile,
            OpenBarrier::RootDirectory,
            OpenBarrier::ParentDirectory,
            OpenBarrier::FinalDevice
        ]
    );
    for fail in [
        OpenBarrier::MarkerFile,
        OpenBarrier::RootDirectory,
        OpenBarrier::ParentDirectory,
        OpenBarrier::FinalDevice,
    ] {
        let mut observed = Vec::new();
        let result = FilesystemBlockStore::open_with_barriers(&root, true, |point, file| {
            observed.push(point);
            if point == fail {
                return Err(FsError::backend("injected marker barrier failure"));
            }
            match point {
                OpenBarrier::StagingFile | OpenBarrier::MarkerFile => file_barrier(file),
                OpenBarrier::RootDirectory | OpenBarrier::ParentDirectory => {
                    file.sync_all().map_err(io_error)
                }
                OpenBarrier::FinalDevice => final_device_barrier(file),
            }
        });
        assert!(result.unwrap_err().is(ErrorCode::Eio));
        assert_eq!(observed.last(), Some(&fail));
    }
    FilesystemBlockStore::open(&root, true).unwrap();
}
