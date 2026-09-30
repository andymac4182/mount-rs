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

fn observed_store() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    FilesystemBlockStore,
    mount_rs_core::diagnostics::filesystem_blocks::Observer,
) {
    let (parent, root, mut store) = store();
    let observer = mount_rs_core::diagnostics::filesystem_blocks::Observer::isolated();
    Arc::get_mut(&mut store.inner).unwrap().observer = observer.clone();
    (parent, root, store, observer)
}

fn filesystem_profile_successful(
    row: mount_rs_core::diagnostics::filesystem_blocks::CounterSnapshot,
    calls: u64,
) {
    assert_eq!(
        (
            row.started,
            row.inflight,
            row.succeeded,
            row.failed,
            row.abandoned
        ),
        (calls, 0, calls, 0, 0),
    );
}

#[tokio::test]
async fn filesystem_profile_new_put_separates_owned_work_and_all_barriers() {
    use mount_rs_core::diagnostics::filesystem_blocks::{Operation, Path as PutPath, Stage};
    let (_parent, root, store, observer) = observed_store();
    let bytes = b"write stages preserve the durable ownership boundary";
    let id = store.put(bytes).await.unwrap();
    assert_eq!(std::fs::read(object_path(&root, &id)).unwrap(), bytes);
    let snapshot = observer.snapshot().unwrap();
    assert!(!snapshot.saturated && !snapshot.concurrent_activity);
    assert_eq!(snapshot.device_barrier_supported, cfg!(target_os = "macos"));
    let operation = snapshot.operations[Operation::Put as usize];
    filesystem_profile_successful(operation.waiter, 1);
    filesystem_profile_successful(operation.queue, 1);
    filesystem_profile_successful(operation.worker, 1);
    assert_eq!(operation.put_path[PutPath::Created as usize], 1);
    assert_eq!(operation.put_path.iter().sum::<u64>(), 1);
    for stage in [
        Stage::InputCopy,
        Stage::InitialAuthority,
        Stage::ContentId,
        Stage::ShardOpen,
        Stage::StageCreateWrite,
        Stage::FileSync,
        Stage::FileDeviceSync,
        Stage::BeforePublishAuthority,
        Stage::PublishName,
        Stage::ShardSync,
        Stage::RootSync,
        Stage::PostDirectoryDeviceSync,
        Stage::FinalAuthority,
    ] {
        filesystem_profile_successful(snapshot.stages[stage as usize], 1);
    }
    filesystem_profile_successful(snapshot.stages[Stage::ExistingVerify as usize], 0);
    assert_eq!(
        snapshot.stages[Stage::InputCopy as usize].offered_bytes,
        bytes.len() as u64,
    );
    assert_eq!(
        snapshot.stages[Stage::ContentId as usize].offered_bytes,
        bytes.len() as u64,
    );
}

#[tokio::test]
async fn filesystem_profile_existing_put_rechecks_content_and_repeats_barriers() {
    use mount_rs_core::diagnostics::filesystem_blocks::{Operation, Path as PutPath, Stage};
    let (_parent, _root, store, observer) = observed_store();
    let bytes = b"duplicate bytes still require each barrier";
    let id = store.put(bytes).await.unwrap();
    assert_eq!(store.put(bytes).await.unwrap(), id);
    let snapshot = observer.snapshot().unwrap();
    let operation = snapshot.operations[Operation::Put as usize];
    filesystem_profile_successful(operation.worker, 2);
    assert_eq!(operation.put_path[PutPath::Created as usize], 1);
    assert_eq!(operation.put_path[PutPath::Existing as usize], 1);
    assert_eq!(operation.put_path[PutPath::RaceExisting as usize], 0);
    filesystem_profile_successful(snapshot.stages[Stage::ExistingVerify as usize], 1);
    filesystem_profile_successful(snapshot.stages[Stage::StageCreateWrite as usize], 1);
    filesystem_profile_successful(snapshot.stages[Stage::BeforePublishAuthority as usize], 1);
    filesystem_profile_successful(snapshot.stages[Stage::PublishName as usize], 1);
    for stage in [
        Stage::InitialAuthority,
        Stage::ContentId,
        Stage::FileSync,
        Stage::FileDeviceSync,
        Stage::ShardSync,
        Stage::RootSync,
        Stage::PostDirectoryDeviceSync,
        Stage::FinalAuthority,
    ] {
        filesystem_profile_successful(snapshot.stages[stage as usize], 2);
    }
}

#[tokio::test]
async fn filesystem_profile_barrier_errors_preserve_visibility_without_worker_success() {
    use mount_rs_core::diagnostics::filesystem_blocks::{Operation, Stage};
    for (point, failed_stage, visible) in [
        (Point::File, Stage::FileSync, false),
        (Point::Directory, Stage::ShardSync, true),
        (Point::Root, Stage::RootSync, true),
        (Point::Publication, Stage::PostDirectoryDeviceSync, true),
    ] {
        let (_parent, root, store, observer) = observed_store();
        let bytes = b"a visible object does not mean a durable acknowledgement";
        let id = content_id(bytes);
        store.inner.faults.fail_once(point);
        assert!(store.put(bytes).await.unwrap_err().is(ErrorCode::Eio));
        assert_eq!(object_path(&root, &id).exists(), visible);
        let snapshot = observer.snapshot().unwrap();
        let operation = snapshot.operations[Operation::Put as usize];
        assert_eq!(
            (
                operation.worker.started,
                operation.worker.inflight,
                operation.worker.succeeded,
                operation.worker.failed,
                operation.worker.abandoned
            ),
            (1, 0, 0, 1, 0),
        );
        assert_eq!(
            (operation.waiter.failed, operation.waiter.succeeded),
            (1, 0)
        );
        let stage = snapshot.stages[failed_stage as usize];
        assert_eq!(
            (stage.started, stage.failed, stage.succeeded, stage.inflight),
            (1, 1, 0, 0)
        );
        assert_eq!(snapshot.stages[Stage::FinalAuthority as usize].started, 0);
        assert!(!snapshot.saturated && !snapshot.concurrent_activity);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filesystem_profile_create_only_race_counts_the_extra_winner_barrier() {
    use mount_rs_core::diagnostics::filesystem_blocks::{Operation, Path as PutPath, Stage};
    let (_parent, root, first, observer) = observed_store();
    let mut second = FilesystemBlockStore::open(&root, true).unwrap();
    Arc::get_mut(&mut second.inner).unwrap().observer = observer.clone();
    let bytes = b"both race participants finish the winner's barriers";
    let (mut release, started_rx, settled_rx) = install_gate(&first.inner.faults.gate);
    let publishing = first.clone();
    let mut task = tokio::spawn(async move { publishing.put(bytes).await });
    let started = observe(started_rx).await;
    // Capture the result, then release the paused owned worker before asserting.
    let second_result = if started {
        let mut second_task = tokio::spawn(async move { second.put(bytes).await });
        finish(&mut second_task).await
    } else {
        None
    };
    let released = release.release();
    let first_result = finish(&mut task).await;
    let settled = observe(settled_rx).await;
    assert!(
        started && released && settled,
        "actual race worker gate did not settle"
    );
    let Some(Ok(Ok(first_id))) = first_result else {
        panic!("first owned writer failed to finish")
    };
    assert!(matches!(second_result, Some(Ok(Ok(second_id))) if second_id == first_id));
    let snapshot = observer.snapshot().unwrap();
    let operation = snapshot.operations[Operation::Put as usize];
    filesystem_profile_successful(operation.worker, 2);
    filesystem_profile_successful(operation.waiter, 2);
    assert_eq!(operation.put_path[PutPath::Created as usize], 1);
    assert_eq!(operation.put_path[PutPath::RaceExisting as usize], 1);
    assert_eq!(operation.put_path[PutPath::Existing as usize], 0);
    let publication = snapshot.stages[Stage::PublishName as usize];
    assert_eq!(
        (
            publication.started,
            publication.inflight,
            publication.succeeded,
            publication.failed,
            publication.abandoned
        ),
        (2, 0, 1, 1, 0),
    );
    filesystem_profile_successful(snapshot.stages[Stage::ExistingVerify as usize], 1);
    filesystem_profile_successful(snapshot.stages[Stage::StageCreateWrite as usize], 2);
    filesystem_profile_successful(snapshot.stages[Stage::FileSync as usize], 3);
    filesystem_profile_successful(snapshot.stages[Stage::FileDeviceSync as usize], 3);
    filesystem_profile_successful(snapshot.stages[Stage::PostDirectoryDeviceSync as usize], 2);
    filesystem_profile_successful(snapshot.stages[Stage::ShardSync as usize], 2);
    filesystem_profile_successful(snapshot.stages[Stage::RootSync as usize], 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn filesystem_profile_dropped_waiter_is_not_cancelled_owned_publication() {
    use mount_rs_core::diagnostics::filesystem_blocks::Operation;
    let (_parent, root, store, observer) = observed_store();
    let bytes = b"profile ownership outlives its async waiter";
    let id = content_id(bytes);
    let (mut release, started_rx, settled_rx) = install_gate(&store.inner.faults.gate);
    let publishing = store.clone();
    let mut task = tokio::spawn(async move { publishing.put(bytes).await });
    let started = observe(started_rx).await;
    let during = observer.snapshot().unwrap();
    task.abort();
    let cancelled = finish(&mut task).await;
    let released = release.release();
    let settled = observe(settled_rx).await;
    let after = tokio::time::timeout(OBSERVATION_LIMIT, async {
        loop {
            let snapshot = observer.snapshot().unwrap();
            if snapshot.operations[Operation::Put as usize].worker.inflight == 0 {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(
        started && released && settled,
        "actual dropped-waiter worker did not settle"
    );
    assert!(matches!(cancelled, Some(Err(error)) if error.is_cancelled()));
    let after = after.expect("owned worker profile did not settle");
    assert_eq!(
        FilesystemBlockStore::open(&root, true)
            .unwrap()
            .get(&id)
            .await
            .unwrap(),
        bytes
    );
    let before = during.operations[Operation::Put as usize];
    assert_eq!((before.worker.started, before.worker.inflight), (1, 1));
    let operation = after.operations[Operation::Put as usize];
    filesystem_profile_successful(operation.queue, 1);
    filesystem_profile_successful(operation.worker, 1);
    assert_eq!(
        (
            operation.waiter.started,
            operation.waiter.inflight,
            operation.waiter.succeeded,
            operation.waiter.failed,
            operation.waiter.abandoned
        ),
        (1, 0, 0, 0, 1),
    );
}

#[test]
fn filesystem_profile_blocking_queue_ends_at_actual_worker_start() {
    use mount_rs_core::diagnostics::filesystem_blocks::Operation;
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    let (_parent, _root, store, observer) = observed_store();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (started_tx, started_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    let mut blocker = runtime.spawn_blocking(move || {
        started_tx.send(()).unwrap();
        release_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let (during, after, was_pending, completed, blocker_finished, released) =
        runtime.block_on(async {
            let mut future = std::pin::pin!(store.put(b"queued write"));
            let mut context = Context::from_waker(Waker::noop());
            let first = future.as_mut().poll(&mut context);
            let during = observer.snapshot().unwrap();
            // Always release the owned blocker before assertions or runtime Drop.
            let released = release_tx.send(()).is_ok();
            let was_pending = matches!(&first, Poll::Pending);
            let completed = match first {
                Poll::Ready(result) => Some(result),
                Poll::Pending => tokio::time::timeout(OBSERVATION_LIMIT, future).await.ok(),
            };
            let blocker_finished = finish(&mut blocker).await;
            (
                during,
                observer.snapshot().unwrap(),
                was_pending,
                completed,
                blocker_finished,
                released,
            )
        });
    assert!(released && was_pending);
    assert!(matches!(completed, Some(Ok(_))));
    assert!(matches!(blocker_finished, Some(Ok(()))));
    let queued = during.operations[Operation::Put as usize];
    assert_eq!(
        (
            queued.queue.started,
            queued.queue.inflight,
            queued.worker.started
        ),
        (1, 1, 0)
    );
    let complete = after.operations[Operation::Put as usize];
    filesystem_profile_successful(complete.queue, 1);
    filesystem_profile_successful(complete.worker, 1);
    filesystem_profile_successful(complete.waiter, 1);
}

#[tokio::test]
async fn filesystem_profile_flush_is_owned_authority_work_without_barrier_replay() {
    use mount_rs_core::diagnostics::filesystem_blocks::Operation;
    let (_parent, _root, store, observer) = observed_store();
    store.flush().await.unwrap();
    let snapshot = observer.snapshot().unwrap();
    let operation = snapshot.operations[Operation::Flush as usize];
    filesystem_profile_successful(operation.queue, 1);
    filesystem_profile_successful(operation.worker, 1);
    filesystem_profile_successful(operation.waiter, 1);
    assert!(snapshot.stages.iter().all(|stage| stage.started == 0));
    assert!(store.inner.faults.events.lock().unwrap().is_empty());
}

#[tokio::test]
async fn filesystem_writeback_puts_do_not_start_forced_sync_stages() {
    use mount_rs_core::diagnostics::filesystem_blocks::{Operation, Path as PutPath, Stage};
    let (_parent, root, store, observer) = observed_store();
    let identity = store.prepare_concurrent_backing().await.unwrap();
    let marker = std::fs::read(root.join("_mount-rs-backing-id-v1")).unwrap();
    let bytes = b"OS writeback keeps complete content and create-only duplicate identity";
    let expected = content_id(bytes);
    let id = store.put(bytes).await.unwrap();
    assert_eq!(id, expected);
    assert_eq!(store.put(bytes).await.unwrap(), id);
    store.flush().await.unwrap();
    assert_eq!(store.get(&id).await.unwrap(), bytes);
    assert_eq!(std::fs::read(object_path(&root, &id)).unwrap(), bytes);

    let reopened = FilesystemBlockStore::open(&root, true).unwrap();
    assert_eq!(reopened.prepare_concurrent_backing().await.unwrap(), identity);
    reopened.verify_concurrent_backing(identity).await.unwrap();
    assert_eq!(
        std::fs::read(root.join("_mount-rs-backing-id-v1")).unwrap(),
        marker
    );
    assert_eq!(reopened.get(&id).await.unwrap(), bytes);

    let snapshot = observer.snapshot().unwrap();
    assert!(!snapshot.saturated && !snapshot.concurrent_activity);
    let put = snapshot.operations[Operation::Put as usize];
    filesystem_profile_successful(put.waiter, 2);
    filesystem_profile_successful(put.queue, 2);
    filesystem_profile_successful(put.worker, 2);
    assert_eq!(put.put_path[PutPath::Created as usize], 1);
    assert_eq!(put.put_path[PutPath::Existing as usize], 1);
    assert_eq!(put.put_path[PutPath::RaceExisting as usize], 0);
    let flush = snapshot.operations[Operation::Flush as usize];
    filesystem_profile_successful(flush.waiter, 1);
    filesystem_profile_successful(flush.queue, 1);
    filesystem_profile_successful(flush.worker, 1);
    filesystem_profile_successful(snapshot.stages[Stage::ExistingVerify as usize], 1);
    filesystem_profile_successful(snapshot.stages[Stage::FinalAuthority as usize], 2);
    for stage in [
        Stage::FileSync,
        Stage::FileDeviceSync,
        Stage::ShardSync,
        Stage::RootSync,
        Stage::PostDirectoryDeviceSync,
    ] {
        let row = snapshot.stages[stage as usize];
        assert_eq!(
            (
                row.started,
                row.inflight,
                row.succeeded,
                row.failed,
                row.abandoned
            ),
            (0, 0, 0, 0, 0),
            "FS_WRITEBACK_SYNC_REGRESSION: {stage:?} must not force OS writeback"
        );
    }
}

#[tokio::test]
async fn filesystem_profile_public_constructor_follows_build_and_preselected_runtime_flag() {
    use mount_rs_core::diagnostics::filesystem_blocks::{Operation, Stage};
    // ROOT selects the environment before this fresh test process starts.
    // This test does not mutate flags or install an isolated observer.
    let runtime_selected =
        std::env::var_os("MOUNT_RS_PROFILE_IO").is_some_and(|value| value == "1");
    let expected = cfg!(feature = "io-profiling") && runtime_selected;
    let (_parent, _root, store) = store();
    assert_eq!(store.inner.observer.is_enabled(), expected);
    let before = store.inner.observer.snapshot();
    let bytes = b"public constructor selects the actual process observer";
    let id = store.put(bytes).await.unwrap();
    assert_eq!(store.get(&id).await.unwrap(), bytes);
    let after = store.inner.observer.snapshot();
    if expected {
        let before = before.expect("enabled constructor omitted its process bank");
        let after = after.expect("enabled constructor lost its process bank");
        assert!(!before.saturated && !after.saturated);
        let previous = before.operations[Operation::Put as usize];
        let current = after.operations[Operation::Put as usize];
        // Lower bounds tolerate other observed work; never assume global zeros
        // or subtract the absolute inflight gauge.
        for (old, new) in [
            (previous.waiter, current.waiter),
            (previous.queue, current.queue),
            (previous.worker, current.worker),
        ] {
            assert!(new.started >= old.started.checked_add(1).unwrap());
            assert!(new.succeeded >= old.succeeded.checked_add(1).unwrap());
        }
        for stage in [
            Stage::InputCopy,
            Stage::ContentId,
            Stage::FileSync,
            Stage::FileDeviceSync,
            Stage::ShardSync,
            Stage::RootSync,
            Stage::PostDirectoryDeviceSync,
            Stage::FinalAuthority,
        ] {
            let old = before.stages[stage as usize];
            let new = after.stages[stage as usize];
            assert!(new.succeeded >= old.succeeded.checked_add(1).unwrap());
        }
    } else {
        assert!(before.is_none() && after.is_none());
    }
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
