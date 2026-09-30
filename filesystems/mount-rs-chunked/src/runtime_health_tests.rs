//! Health observation must not wait behind detailed namespace state.

use super::*;
use futures_lite::future::block_on;
use mount_rs_memory::{MemoryBlockStore, MemoryMetadataStore};
use std::sync::mpsc::sync_channel;
use std::time::Instant;

type Filesystem = ChunkedFs<MemoryMetadataStore, MemoryBlockStore>;
const DEADLINE: Duration = Duration::from_secs(2);

fn filesystem() -> Filesystem {
    block_on(ChunkedFs::open(
        MemoryMetadataStore::new(),
        MemoryBlockStore::new(),
        ChunkedOptions::fixed("runtime-health-oracle", 64).unwrap(),
    ))
    .unwrap()
}

/// A timeout is evidence that observation waited for the held state mutex. The
/// guard is released and the worker joined before any assertion consumes it.
fn observe_while_state_held(filesystem: &Filesystem, expected: bool) {
    let clone = filesystem.clone();
    let state = filesystem
        .inner
        .state
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (started, start) = sync_channel(1);
    let (sent, received) = sync_channel(1);
    let (ready, result, joined) = std::thread::scope(|scope| {
        let worker = scope.spawn(move || {
            started.send(()).unwrap();
            sent.send(clone.failed()).unwrap();
        });
        let ready = start.recv_timeout(DEADLINE);
        let result = received.recv_timeout(DEADLINE);
        drop(state);
        let joined = worker.join();
        (ready, result, joined)
    });
    assert!(joined.is_ok(), "health worker panicked after state release");
    assert!(ready.is_ok(), "health worker never started");
    assert_eq!(
        result.expect("failed() waited for the held state mutex"),
        expected
    );
}

#[test]
fn healthy_failed_observation_does_not_wait_for_state_mutex() {
    let filesystem = filesystem();
    assert!(!filesystem.failed());
    observe_while_state_held(&filesystem, false);
    block_on(filesystem.shutdown()).unwrap();
    assert!(!filesystem.failed(), "successful shutdown is not a failure");
}

#[derive(Clone, Copy)]
enum FailureWriter {
    Publication,
    Acknowledgement,
    Direct,
}
impl FailureWriter {
    fn publish(self, filesystem: &Filesystem) {
        match self {
            Self::Publication => drop(PublicationGuard::new(
                &filesystem.inner.state,
                &filesystem.inner.failed,
            )),
            Self::Acknowledgement => {
                let committed = Arc::new(AtomicBool::new(false));
                let guard = MutationAcknowledgementGuard::new(
                    &filesystem.inner.state,
                    &filesystem.inner.failed,
                    committed.clone(),
                );
                committed.store(true, Ordering::Release);
                drop(guard);
            }
            Self::Direct => {
                filesystem.fail_closed(FsError::new(ErrorCode::Eio).with_syscall("health-oracle"));
            }
        }
    }
    fn syscall(self) -> &'static str {
        match self {
            Self::Publication => "metadata-publish",
            Self::Acknowledgement => "mutation-batch",
            Self::Direct => "health-oracle",
        }
    }
}

fn observe_failure_before_writer_obtains_state(writer: FailureWriter) {
    let filesystem = filesystem();
    let existing_clone = filesystem.clone();
    let publisher = filesystem.clone();
    let observer = filesystem.clone();
    let state = filesystem.inner.state.lock().unwrap();
    let (started, start) = sync_channel(1);
    let (sent, received) = sync_channel(1);
    let (ready, result, detailed_before_release, writer_joined, observer_joined) =
        std::thread::scope(|scope| {
            let publisher = scope.spawn(move || {
                started.send(()).unwrap();
                writer.publish(&publisher);
            });
            let ready = start.recv_timeout(DEADLINE);
            let observer = scope.spawn(move || {
                let deadline = Instant::now() + DEADLINE;
                // A healthy first sample can precede the writer being scheduled.
                // Retry a nonblocking observation until its known-failure latch is
                // visible; a lock-taking implementation blocks on the first call.
                loop {
                    let failed = observer.failed();
                    if failed || Instant::now() >= deadline {
                        sent.send(failed).unwrap();
                        break;
                    }
                    std::thread::yield_now();
                }
            });
            let result = received.recv_timeout(DEADLINE);
            let detailed_before_release = state.failure.is_some();
            drop(state);
            let writer_joined = publisher.join();
            let observer_joined = observer.join();
            (
                ready,
                result,
                detailed_before_release,
                writer_joined,
                observer_joined,
            )
        });
    assert!(writer_joined.is_ok(), "failure publisher panicked");
    assert!(observer_joined.is_ok(), "health observer panicked");
    assert!(ready.is_ok(), "failure publisher never started");
    assert!(
        !detailed_before_release,
        "writer changed state while its mutex was held"
    );
    assert!(result.expect("known failure was not visible before the state mutex was released"));
    let detailed = filesystem
        .inner
        .state
        .lock()
        .unwrap()
        .failure
        .clone()
        .unwrap();
    assert_eq!(detailed.code, ErrorCode::Eio);
    assert_eq!(detailed.syscall.as_deref(), Some(writer.syscall()));
    assert!(filesystem.failed());
    assert!(
        existing_clone.failed(),
        "clone predating failure lost sticky health"
    );
    let later =
        filesystem.fail_closed(FsError::new(ErrorCode::Estale).with_syscall("later-failure"));
    assert_eq!(later.code, ErrorCode::Estale);
    assert_eq!(
        filesystem
            .inner
            .state
            .lock()
            .unwrap()
            .failure
            .as_ref()
            .unwrap()
            .syscall,
        detailed.syscall
    );
    block_on(filesystem.shutdown()).unwrap();
    assert!(filesystem.failed());
    assert!(existing_clone.failed(), "shutdown cleared sticky failure");
}

#[test]
fn publication_failure_is_visible_before_detailed_state_lock() {
    observe_failure_before_writer_obtains_state(FailureWriter::Publication);
}

#[test]
fn committed_unobserved_acknowledgement_is_visible_before_detailed_state_lock() {
    observe_failure_before_writer_obtains_state(FailureWriter::Acknowledgement);
}

#[test]
fn fail_closed_is_visible_before_detailed_state_lock() {
    observe_failure_before_writer_obtains_state(FailureWriter::Direct);
}

#[test]
fn disarmed_publication_and_uncommitted_or_observed_acknowledgements_stay_healthy() {
    let filesystem = filesystem();
    let existing_clone = filesystem.clone();
    let mut publication = PublicationGuard::new(&filesystem.inner.state, &filesystem.inner.failed);
    publication.disarm();
    drop(publication);
    drop(MutationAcknowledgementGuard::new(
        &filesystem.inner.state,
        &filesystem.inner.failed,
        Arc::new(AtomicBool::new(false)),
    ));
    let mut observed = MutationAcknowledgementGuard::new(
        &filesystem.inner.state,
        &filesystem.inner.failed,
        Arc::new(AtomicBool::new(true)),
    );
    observed.disarm();
    drop(observed);
    assert!(filesystem.inner.state.lock().unwrap().failure.is_none());
    assert!(!existing_clone.failed());
    observe_while_state_held(&filesystem, false);
    block_on(filesystem.shutdown()).unwrap();
    assert!(!filesystem.failed());
}

#[test]
fn observed_state_poison_is_nonblocking_and_sticky_across_clones_and_test_only_clear() {
    let filesystem = filesystem();
    let existing_clone = filesystem.clone();
    let poisoner = filesystem.clone();
    let poisoned = std::thread::scope(|scope| {
        scope
            .spawn(move || {
                let _state = poisoner.inner.state.lock().unwrap();
                panic!("intentional runtime state poison");
            })
            .join()
    });
    assert!(poisoned.is_err());
    assert!(filesystem.inner.state.is_poisoned());
    assert!(filesystem.failed());
    assert!(existing_clone.failed());
    observe_while_state_held(&filesystem, true);
    // Production never clears poison. This test isolates the sticky observation
    // contract from the mutex's independent poison flag, without claiming that
    // a cleared mutex recovers the filesystem or its namespace.
    filesystem.inner.state.clear_poison();
    assert!(!filesystem.inner.state.is_poisoned());
    assert!(
        filesystem.failed(),
        "clearing poison cleared an observed failure"
    );
    assert!(existing_clone.failed());
    observe_while_state_held(&existing_clone, true);
}
