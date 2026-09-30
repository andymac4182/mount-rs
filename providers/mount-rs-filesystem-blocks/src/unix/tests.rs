use super::test_support::{Gate, Point};
use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
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
        [
            Point::File,
            Point::Directory,
            Point::Root,
            Point::Publication
        ]
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
        [
            Point::File,
            Point::Directory,
            Point::Root,
            Point::Publication
        ]
    );
    store.inner.faults.events.lock().unwrap().clear();
    store.inner.faults.fail_once(Point::Publication);
    assert!(store.put(bytes).await.unwrap_err().is(ErrorCode::Eio));
    assert_eq!(
        *store.inner.faults.events.lock().unwrap(),
        [
            Point::File,
            Point::Directory,
            Point::Root,
            Point::Publication
        ]
    );
    store.inner.faults.events.lock().unwrap().clear();
    store.put(bytes).await.unwrap();
    assert_eq!(
        *store.inner.faults.events.lock().unwrap(),
        [
            Point::File,
            Point::Directory,
            Point::Root,
            Point::Publication
        ]
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

const OBSERVATION_LIMIT: Duration = Duration::from_secs(5);

/// Release is unconditional on every normal, timeout, and panic path. Each gate
/// has a capacity-one channel and this owner sends at most once.
struct ReleaseGate(Option<SyncSender<()>>);

impl ReleaseGate {
    fn release(&mut self) -> bool {
        self.0.take().is_none_or(|release| release.send(()).is_ok())
    }
}

impl Drop for ReleaseGate {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn install_gate(
    slot: &std::sync::Mutex<Option<Gate>>,
) -> (ReleaseGate, Receiver<()>, Receiver<()>) {
    let (started_tx, started_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    let (settled_tx, settled_rx) = sync_channel(1);
    *slot.lock().unwrap() = Some(Gate {
        started: started_tx,
        release: release_rx,
        settled: settled_tx,
    });
    (ReleaseGate(Some(release_tx)), started_rx, settled_rx)
}

async fn observe(receiver: Receiver<()>) -> bool {
    tokio::task::spawn_blocking(move || receiver.recv_timeout(OBSERVATION_LIMIT).is_ok())
        .await
        .unwrap_or(false)
}

async fn finish<T>(
    task: &mut tokio::task::JoinHandle<T>,
) -> Option<std::result::Result<T, tokio::task::JoinError>> {
    match tokio::time::timeout(OBSERVATION_LIMIT, &mut *task).await {
        Ok(result) => Some(result),
        Err(_) => {
            task.abort();
            let _ = tokio::time::timeout(OBSERVATION_LIMIT, &mut *task).await;
            None
        }
    }
}

#[tokio::test]
async fn flush_after_acknowledged_put_does_not_repeat_root_barrier() {
    let (_parent, root, store) = store();
    let acknowledged_bytes = b"the acknowledged put already persisted both directory entries";
    let acknowledged = store.put(acknowledged_bytes).await.unwrap();
    store.inner.faults.events.lock().unwrap().clear();
    store.inner.faults.fail_once(Point::Root);

    let flushed = store.flush().await;
    let flush_events = store.inner.faults.events.lock().unwrap().clone();
    let novel = store
        .put(b"the next novel put must still encounter the armed root fault")
        .await;
    let novel_events = store.inner.faults.events.lock().unwrap().clone();
    let reopened = FilesystemBlockStore::open(&root, true).unwrap();
    let acknowledged_read = reopened.get(&acknowledged).await;

    assert!(
        flushed.is_ok(),
        "acknowledged PUT flush repeated the root barrier"
    );
    assert!(
        flush_events.is_empty(),
        "flush consumed a PUT barrier observation"
    );
    assert!(
        novel.is_err_and(|error| error.is(ErrorCode::Eio)),
        "flush consumed the root fault reserved for the next novel PUT"
    );
    assert_eq!(novel_events, [Point::File, Point::Directory, Point::Root]);
    assert_eq!(acknowledged_read.unwrap(), acknowledged_bytes);
}

#[tokio::test]
async fn duplicate_put_still_requires_root_barrier_before_acknowledgment() {
    let (_parent, root, store) = store();
    let bytes = b"a duplicate must independently finish the root barrier";
    let id = store.put(bytes).await.unwrap();
    store.inner.faults.events.lock().unwrap().clear();
    store.inner.faults.fail_once(Point::Root);

    let duplicate = store.put(bytes).await;
    let refused_events = store.inner.faults.events.lock().unwrap().clone();
    store.inner.faults.events.lock().unwrap().clear();
    let retry = store.put(bytes).await;
    let retry_events = store.inner.faults.events.lock().unwrap().clone();
    let reopened = FilesystemBlockStore::open(&root, true).unwrap();
    let fresh = reopened.get(&id).await;

    assert!(duplicate.is_err_and(|error| error.is(ErrorCode::Eio)));
    assert_eq!(refused_events, [Point::File, Point::Directory, Point::Root]);
    assert_eq!(retry.unwrap(), id);
    assert_eq!(
        retry_events,
        [
            Point::File,
            Point::Directory,
            Point::Root,
            Point::Publication
        ]
    );
    assert_eq!(fresh.unwrap(), bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flush_rejects_marker_replacement_after_its_first_identity_check() {
    let (_parent, root, store) = store();
    store
        .put(b"marker replacement must be checked at flush return")
        .await
        .unwrap();
    let (mut release, started_rx, settled_rx) = install_gate(&store.inner.faults.flush_gate);
    let flushing = store.clone();
    let mut task = tokio::spawn(async move { flushing.flush().await });
    let started = observe(started_rx).await;
    let replacement = if started {
        (|| -> Result<()> {
            let marker = root.join("_mount-rs-backing-id-v1");
            let replacement = root.join("replacement-marker");
            let bytes = std::fs::read(&marker).map_err(io_error)?;
            std::fs::write(&replacement, bytes).map_err(io_error)?;
            std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o600))
                .map_err(io_error)?;
            std::fs::rename(&replacement, &marker).map_err(io_error)
        })()
    } else {
        Ok(())
    };
    let released = release.release();
    let result = finish(&mut task).await;
    let settled = observe(settled_rx).await;

    assert!(
        started,
        "flush did not reach its positive first identity boundary"
    );
    assert!(replacement.is_ok(), "controlled marker replacement failed");
    assert!(released, "flush gate release was not delivered");
    assert!(settled, "the actual flush worker did not settle");
    assert!(
        matches!(result, Some(Ok(Err(error))) if error.is(ErrorCode::Estale)),
        "flush accepted an identical marker with a substituted inode"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flush_rejects_root_replacement_after_its_first_identity_check() {
    let (parent, root, store) = store();
    let bytes = b"the flush must never acknowledge a substituted root";
    let id = store.put(bytes).await.unwrap();
    let displaced = parent
        .path()
        .canonicalize()
        .unwrap()
        .join("displaced-blocks");
    let (mut release, started_rx, settled_rx) = install_gate(&store.inner.faults.flush_gate);
    let flushing = store.clone();
    let mut task = tokio::spawn(async move { flushing.flush().await });
    let started = observe(started_rx).await;
    let replacement = if started {
        (|| -> Result<()> {
            std::fs::rename(&root, &displaced).map_err(io_error)?;
            std::fs::create_dir(&root).map_err(io_error)?;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
                .map_err(io_error)?;
            FilesystemBlockStore::open(&root, true)?;
            Ok(())
        })()
    } else {
        Ok(())
    };
    let released = release.release();
    let result = finish(&mut task).await;
    let settled = observe(settled_rx).await;

    assert!(
        started,
        "flush did not reach its positive first identity boundary"
    );
    assert!(replacement.is_ok(), "controlled root replacement failed");
    assert!(released, "flush gate release was not delivered");
    assert!(settled, "the actual flush worker did not settle");
    assert!(
        matches!(result, Some(Ok(Err(error))) if error.is(ErrorCode::Estale)),
        "flush accepted a substituted root after its first verification"
    );
    let old_root = FilesystemBlockStore::open(&displaced, true).unwrap();
    assert_eq!(old_root.get(&id).await.unwrap(), bytes);
    let replacement_root = FilesystemBlockStore::open(&root, true).unwrap();
    assert!(
        replacement_root
            .get(&id)
            .await
            .unwrap_err()
            .is(ErrorCode::Enoent)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paused_successful_put_keeps_its_root_and_device_barriers_after_flush() {
    let (_parent, root, store) = store();
    let bytes = b"an outstanding put owns its eventual complete barrier sequence";
    let id = content_id(bytes);
    let (mut release, started_rx, settled_rx) = install_gate(&store.inner.faults.gate);
    let publishing = store.clone();
    let mut task = tokio::spawn(async move { publishing.put(bytes).await });
    let started = observe(started_rx).await;
    let before_publication = !object_path(&root, &id).exists();
    let flushing = store.clone();
    let mut flush_task = tokio::spawn(async move { flushing.flush().await });
    let flushed = finish(&mut flush_task).await;
    let before_release_events = store.inner.faults.events.lock().unwrap().clone();
    let released = release.release();
    let result = finish(&mut task).await;
    let settled = observe(settled_rx).await;
    let complete_events = store.inner.faults.events.lock().unwrap().clone();

    assert!(started, "PUT did not enter the actual publication gate");
    assert!(before_publication, "paused PUT was already published");
    assert!(
        matches!(flushed, Some(Ok(Ok(())))),
        "flush incorrectly waited for an unacknowledged PUT"
    );
    assert_eq!(
        before_release_events,
        [Point::File],
        "flush repeated a PUT root barrier"
    );
    assert!(released, "PUT gate release was not delivered");
    assert!(settled, "the actual owned PUT did not settle");
    assert!(matches!(result, Some(Ok(Ok(actual))) if actual == id));
    assert_eq!(
        complete_events,
        [
            Point::File,
            Point::Directory,
            Point::Root,
            Point::Publication
        ]
    );
    let reopened = FilesystemBlockStore::open(&root, true).unwrap();
    assert_eq!(reopened.get(&id).await.unwrap(), bytes);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn flush_does_not_drain_a_cancelled_outstanding_put_and_its_owner_still_finishes() {
    let (_parent, root, store) = store();
    let acknowledged_bytes = b"a successful prior put is already durable";
    let acknowledged = store.put(acknowledged_bytes).await.unwrap();
    store.inner.faults.events.lock().unwrap().clear();
    let bytes = b"cancelling a waiter must not cancel the owned outstanding put";
    let id = content_id(bytes);
    let (mut release, started_rx, settled_rx) = install_gate(&store.inner.faults.gate);
    let publishing = store.clone();
    let mut task = tokio::spawn(async move { publishing.put(bytes).await });
    let started = observe(started_rx).await;
    let before_publication = !object_path(&root, &id).exists();
    let flushing = store.clone();
    let mut flush_task = tokio::spawn(async move { flushing.flush().await });
    let flushed = finish(&mut flush_task).await;
    let before_release_events = store.inner.faults.events.lock().unwrap().clone();
    task.abort();
    let cancelled = finish(&mut task).await;
    drop(store);
    let released = release.release();
    let settled = observe(settled_rx).await;

    assert!(started, "PUT did not enter the actual publication gate");
    assert!(before_publication, "paused PUT was already published");
    assert!(
        matches!(flushed, Some(Ok(Ok(())))),
        "flush incorrectly waited for an unacknowledged PUT"
    );
    assert_eq!(
        before_release_events,
        [Point::File],
        "flush repeated a PUT root barrier"
    );
    assert!(matches!(cancelled, Some(Err(error)) if error.is_cancelled()));
    assert!(released, "cancelled PUT gate release was not delivered");
    assert!(
        settled,
        "cancelled waiter's actual owned PUT did not settle"
    );
    let reopened = FilesystemBlockStore::open(&root, true).unwrap();
    assert_eq!(
        reopened.get(&acknowledged).await.unwrap(),
        acknowledged_bytes
    );
    assert_eq!(reopened.get(&id).await.unwrap(), bytes);
    assert!(
        std::fs::read_dir(root.join(&id.0[1..3]))
            .unwrap()
            .all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with("_mount-rs-stage-")
            })
    );
}
