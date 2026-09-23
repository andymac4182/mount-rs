//! Two independent chunked coordinators over one revision-CAS metadata volume.
//!
//! This provider keeps the same boundaries as a remote metadata store: each
//! coordinator has its own runtime state, while metadata revisions and immutable
//! blocks are shared. A forced known conflict exercises caller-side replay.

use async_trait::async_trait;
use mount_rs_chunked::{ChunkedFs, ChunkedOptions};
use mount_rs_core::storage::{
    BlockId, BlockStore, LoadedMetadata, MetadataStore, Namespace, NodeData, WriterLease,
};
use mount_rs_core::types::now_ms;
use mount_rs_core::{
    ErrorCode, FileType, FsDriver, FsError, GuardedMutation, GuardedMutationResult, GuardedRead,
    GuardedReadResult, GuardedSetattr, MkdirOptions, ObservedEntry, OpenFlags, PathGuard,
    PathIdentity, Result,
};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Duration;

#[derive(Clone, Default)]
struct CasMetadata {
    state: Arc<Mutex<CasState>>,
    fail_load: Arc<AtomicBool>,
    ambiguous_after_apply: Arc<AtomicBool>,
    pending_after_apply: Arc<AtomicBool>,
    after_apply_resume: Arc<tokio::sync::Notify>,
    suspend_release_before_apply: Arc<AtomicBool>,
    release_entered: Arc<AtomicBool>,
    fail_one_renew: Arc<AtomicBool>,
    forced_conflicts: Arc<AtomicUsize>,
    observed_conflicts: Arc<AtomicUsize>,
}

#[derive(Default)]
struct CasState {
    revision: u64,
    namespace: Option<Namespace>,
    published: Vec<Namespace>,
    concurrent_mode: bool,
    legacy_lease: Option<WriterLease>,
    last_fence: u64,
    swap_entries_on_next_cas: Option<(String, String)>,
}

impl CasMetadata {
    fn force_one_ambiguous_commit_after_apply(&self) {
        self.ambiguous_after_apply.store(true, Ordering::SeqCst);
    }

    fn suspend_one_commit_after_apply(&self) {
        self.pending_after_apply.store(true, Ordering::SeqCst);
    }

    fn resume_suspended_commit(&self) {
        self.after_apply_resume.notify_one();
    }

    fn suspend_one_release_before_apply(&self) {
        self.release_entered.store(false, Ordering::SeqCst);
        self.suspend_release_before_apply
            .store(true, Ordering::SeqCst);
    }

    fn fail_next_renew(&self) {
        self.fail_one_renew.store(true, Ordering::SeqCst);
    }

    fn fail_load(&self, fail: bool) {
        self.fail_load.store(fail, Ordering::SeqCst);
    }

    fn force_one_known_conflict(&self) {
        self.forced_conflicts.store(1, Ordering::SeqCst);
    }

    fn swap_entries_on_next_cas(&self, first: &str, second: &str) {
        self.lock().expect("metadata lock").swap_entries_on_next_cas =
            Some((first.to_owned(), second.to_owned()));
    }

    fn conflict_count(&self) -> usize {
        self.observed_conflicts.load(Ordering::SeqCst)
    }

    fn published_count(&self) -> usize {
        self.lock().expect("metadata lock").published.len()
    }

    fn published_file_sizes_since(&self, path: &str, count: usize) -> Vec<u64> {
        let state = self.lock().expect("metadata lock");
        state.published[count..]
            .iter()
            .filter_map(|namespace| {
                let NodeData::Directory { entries } = &namespace.nodes.get(&namespace.root)?.data
                else {
                    return None;
                };
                let entry = entries
                    .iter()
                    .find(|entry| entry.name == path.trim_start_matches('/'))?;
                Some(namespace.nodes.get(&entry.inode)?.stats.size)
            })
            .collect()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, CasState>> {
        self.state
            .lock()
            .map_err(|_| FsError::backend("CAS metadata test state lock poisoned"))
    }
}

#[async_trait]
impl MetadataStore for CasMetadata {
    fn durable(&self) -> bool {
        false
    }

    async fn load(&self) -> Result<LoadedMetadata> {
        if self.fail_load.load(Ordering::SeqCst) {
            return Err(FsError::new(ErrorCode::Eio).with_message("test metadata load unavailable"));
        }
        let state = self.lock()?;
        Ok(LoadedMetadata {
            revision: state.revision,
            namespace: state.namespace.clone(),
        })
    }

    async fn prepare_concurrent_mode(&self) -> Result<()> {
        let mut state = self.lock()?;
        if state.legacy_lease.is_some() {
            return Err(
                FsError::new(ErrorCode::Ebusy).with_message("legacy writer still owns the volume")
            );
        }
        state.concurrent_mode = true;
        Ok(())
    }

    async fn acquire_writer(&self, owner: &str, ttl: Duration) -> Result<WriterLease> {
        if owner.is_empty() || ttl.is_zero() {
            return Err(FsError::new(ErrorCode::Einval));
        }
        let mut state = self.lock()?;
        if state.concurrent_mode {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_message("legacy writer lease disabled by concurrent mode"));
        }
        if state.legacy_lease.is_some() {
            return Err(FsError::new(ErrorCode::Eagain));
        }
        state.last_fence = state
            .last_fence
            .checked_add(1)
            .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
        let lease = WriterLease {
            owner: owner.to_owned(),
            fence: state.last_fence,
            expires_at_ms: u64::MAX,
        };
        state.legacy_lease = Some(lease.clone());
        Ok(lease)
    }

    async fn renew_writer(&self, lease: &WriterLease, _ttl: Duration) -> Result<WriterLease> {
        if self.fail_one_renew.swap(false, Ordering::SeqCst) {
            return Err(
                FsError::new(ErrorCode::Eio).with_message("test transient lease renewal error")
            );
        }
        let state = self.lock()?;
        if state.concurrent_mode || state.legacy_lease.as_ref() != Some(lease) {
            return Err(FsError::new(ErrorCode::Estale));
        }
        Ok(lease.clone())
    }

    async fn release_writer(&self, lease: &WriterLease) -> Result<()> {
        if self
            .suspend_release_before_apply
            .swap(false, Ordering::SeqCst)
        {
            self.release_entered.store(true, Ordering::SeqCst);
            std::future::pending::<()>().await;
        }
        let mut state = self.lock()?;
        if state.concurrent_mode || state.legacy_lease.as_ref() != Some(lease) {
            return Err(FsError::new(ErrorCode::Estale));
        }
        state.legacy_lease = None;
        Ok(())
    }

    async fn publish(
        &self,
        expected_revision: u64,
        lease: &WriterLease,
        namespace: Namespace,
    ) -> Result<u64> {
        namespace.validate()?;
        let revision = {
            let mut state = self.lock()?;
            if state.concurrent_mode || state.legacy_lease.as_ref() != Some(lease) {
                return Err(FsError::new(ErrorCode::Estale));
            }
            if state.revision != expected_revision {
                return Err(FsError::new(ErrorCode::Eagain));
            }
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
            state.published.push(namespace.clone());
            state.namespace = Some(namespace);
            state.revision
        };
        if self.pending_after_apply.swap(false, Ordering::SeqCst) {
            self.after_apply_resume.notified().await;
        }
        Ok(revision)
    }

    async fn publish_if_revision(
        &self,
        expected_revision: u64,
        namespace: Namespace,
    ) -> Result<u64> {
        namespace.validate()?;
        let revision = {
            let mut state = self.lock()?;
            if !state.concurrent_mode {
                return Err(FsError::new(ErrorCode::Enotsup));
            }
            if let Some((first, second)) = state.swap_entries_on_next_cas.take() {
                // Simulate another coordinator winning the CAS immediately after
                // this coordinator prepared its guarded candidate. Both inodes
                // remain linked, so the committed namespace remains valid.
                let mut replaced = state
                    .namespace
                    .clone()
                    .ok_or_else(|| FsError::backend("forced swap needs a published namespace"))?;
                let root = replaced.root;
                let NodeData::Directory { entries } = &mut replaced
                    .nodes
                    .get_mut(&root)
                    .ok_or_else(|| FsError::backend("forced swap needs a root node"))?
                    .data
                else {
                    return Err(FsError::backend("forced swap needs a root directory"));
                };
                let first_index = entries
                    .iter()
                    .position(|entry| entry.name == first)
                    .ok_or_else(|| FsError::backend("forced swap first name is missing"))?;
                let second_index = entries
                    .iter()
                    .position(|entry| entry.name == second)
                    .ok_or_else(|| FsError::backend("forced swap second name is missing"))?;
                let first_inode = entries[first_index].inode;
                entries[first_index].inode = entries[second_index].inode;
                entries[second_index].inode = first_inode;
                replaced.validate()?;
                state.revision = state
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                state.published.push(replaced.clone());
                state.namespace = Some(replaced);
                self.observed_conflicts.fetch_add(1, Ordering::SeqCst);
                return Err(FsError::new(ErrorCode::Eagain));
            }
            if state.revision != 0
                && self
                    .forced_conflicts
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |count| {
                        count.checked_sub(1)
                    })
                    .is_ok()
            {
                // A separate writer committed an unrelated metadata revision.
                // The namespace is still valid, but the caller's base is stale.
                state.revision = state
                    .revision
                    .checked_add(1)
                    .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
                self.observed_conflicts.fetch_add(1, Ordering::SeqCst);
                return Err(FsError::new(ErrorCode::Eagain));
            }
            if state.revision != expected_revision {
                self.observed_conflicts.fetch_add(1, Ordering::SeqCst);
                return Err(FsError::new(ErrorCode::Eagain));
            }
            state.revision = state
                .revision
                .checked_add(1)
                .ok_or_else(|| FsError::new(ErrorCode::Eoverflow))?;
            state.published.push(namespace.clone());
            state.namespace = Some(namespace);
            if self.ambiguous_after_apply.swap(false, Ordering::SeqCst) {
                return Err(FsError::new(ErrorCode::Eio).with_message(
                    "test FoundationDB commit may have applied; acknowledgement lost",
                ));
            }
            state.revision
        };
        if self.pending_after_apply.swap(false, Ordering::SeqCst) {
            self.after_apply_resume.notified().await;
        }
        Ok(revision)
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }
}

#[derive(Clone, Default)]
struct SharedBlocks(Arc<Mutex<BTreeMap<BlockId, Vec<u8>>>>);

impl SharedBlocks {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, BTreeMap<BlockId, Vec<u8>>>> {
        self.0
            .lock()
            .map_err(|_| FsError::backend("shared block test state lock poisoned"))
    }
}

#[async_trait]
impl BlockStore for SharedBlocks {
    fn durable(&self) -> bool {
        false
    }

    async fn put(&self, bytes: &[u8]) -> Result<BlockId> {
        let mut text = String::from("test:");
        for byte in bytes {
            write!(&mut text, "{byte:02x}").expect("writing to a string cannot fail");
        }
        let id = BlockId(text);
        let mut blocks = self.lock()?;
        match blocks.get(&id) {
            Some(existing) if existing != bytes => {
                Err(FsError::backend("test block identity collision"))
            }
            Some(_) => Ok(id),
            None => {
                blocks.insert(id.clone(), bytes.to_vec());
                Ok(id)
            }
        }
    }

    async fn get(&self, id: &BlockId) -> Result<Vec<u8>> {
        self.lock()?
            .get(id)
            .cloned()
            .ok_or_else(|| FsError::new(ErrorCode::Enoent))
    }

    async fn flush(&self) -> Result<()> {
        Ok(())
    }

    async fn delete(&self, id: &BlockId) -> Result<()> {
        self.lock()?
            .remove(id)
            .map(|_| ())
            .ok_or_else(|| FsError::new(ErrorCode::Enoent))
    }
}

struct NoopWaker;

impl Wake for NoopWaker {
    fn wake(self: Arc<Self>) {}
    fn wake_by_ref(self: &Arc<Self>) {}
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match Future::poll(future.as_mut(), &mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

type TestFs = ChunkedFs<CasMetadata, SharedBlocks>;

fn open_two() -> (CasMetadata, TestFs, TestFs) {
    let metadata = CasMetadata::default();
    let blocks = SharedBlocks::default();
    let first = block_on(ChunkedFs::open(
        metadata.clone(),
        blocks.clone(),
        ChunkedOptions::fixed("writer-a", 4)
            .expect("fixed chunker")
            .with_concurrent_writes(true),
    ))
    .expect("first concurrent coordinator opens");
    let second = block_on(ChunkedFs::open(
        metadata.clone(),
        blocks,
        ChunkedOptions::fixed("writer-b", 4)
            .expect("fixed chunker")
            .with_concurrent_writes(true),
    ))
    .expect("second concurrent coordinator opens");
    (metadata, first, second)
}

fn read_file(fs: &TestFs, path: &str) -> Vec<u8> {
    let handle = block_on(fs.open(path, "r", 0)).expect("open file for reading");
    let size = usize::try_from(block_on(handle.stat()).expect("file stat").size)
        .expect("test file fits in memory");
    let mut bytes = vec![0; size];
    let count = block_on(handle.read(&mut bytes, Some(0))).expect("read file");
    bytes.truncate(count);
    block_on(handle.close()).expect("close read handle");
    bytes
}

fn path_guard(fs: &TestFs, path: &str) -> PathGuard {
    let stats = block_on(fs.lstat(path)).expect("guarded path lookup");
    PathGuard {
        path: path.to_owned(),
        identity: PathIdentity::from_stats(&stats).expect("path has a stable backend inode"),
    }
}

fn guarded_error(fs: &TestFs, request: GuardedMutation) -> FsError {
    match block_on(fs.guarded_mutation(request)) {
        Err(error) => error,
        Ok(_) => panic!("guarded mutation unexpectedly succeeded"),
    }
}

fn guarded_read_error(fs: &TestFs, request: GuardedRead) -> FsError {
    match block_on(fs.guarded_read(request)) {
        Err(error) => error,
        Ok(result) => panic!("guarded read unexpectedly succeeded: {result:?}"),
    }
}

fn assert_guarded_applied(fs: &TestFs, request: GuardedMutation) {
    match block_on(fs.guarded_mutation(request)) {
        Ok(GuardedMutationResult::Applied | GuardedMutationResult::Created(_)) => {}
        Ok(GuardedMutationResult::Opened { .. }) => panic!("unexpected open result"),
        Err(error) => panic!("guarded mutation failed: {error:?}"),
    }
}

#[test]
fn two_live_coordinators_observe_each_others_creates() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/alpha", b"A-one")).expect("first creates alpha");
    assert_eq!(read_file(&second, "/alpha"), b"A-one");

    block_on(second.write_file("/beta", b"B-two")).expect("second creates beta");
    assert_eq!(read_file(&first, "/beta"), b"B-two");
    let names: Vec<_> = block_on(first.readdir("/"))
        .expect("fresh directory listing")
        .into_iter()
        .map(|entry| entry.name)
        .collect();
    assert!(names.contains(&"alpha".to_owned()));
    assert!(names.contains(&"beta".to_owned()));

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn remote_rename_and_remove_are_visible_to_the_other_coordinator() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/alpha", b"A-one")).expect("first creates alpha");
    block_on(second.write_file("/beta", b"B-two")).expect("second creates beta");

    block_on(second.rename("/alpha", "/renamed")).expect("second renames alpha");
    assert_eq!(
        block_on(first.stat("/alpha")).unwrap_err().code,
        ErrorCode::Enoent
    );
    assert_eq!(read_file(&first, "/renamed"), b"A-one");

    block_on(first.unlink("/beta")).expect("first removes beta");
    assert_eq!(
        block_on(second.stat("/beta")).unwrap_err().code,
        ErrorCode::Enoent
    );
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn forced_cas_conflict_replays_disjoint_writes_to_one_inode() {
    let (metadata, first, second) = open_two();
    block_on(first.write_file("/shared", b"........")).expect("create shared file");
    let first_handle = block_on(first.open("/shared", "r+", 0)).expect("first opens file");
    let second_handle = block_on(second.open("/shared", "r+", 0)).expect("second opens file");

    assert_eq!(block_on(first_handle.write(b"AAAA", Some(0))).unwrap(), 4);
    metadata.force_one_known_conflict();
    assert_eq!(block_on(second_handle.write(b"BBBB", Some(4))).unwrap(), 4);
    assert!(
        metadata.conflict_count() >= 1,
        "fake provider must force a known CAS conflict"
    );
    assert_eq!(read_file(&first, "/shared"), b"AAAABBBB");
    assert_eq!(read_file(&second, "/shared"), b"AAAABBBB");

    block_on(first_handle.close()).expect("close first handle");
    block_on(second_handle.close()).expect("close second handle");
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn changed_file_and_parent_directory_mtimes_increase_past_a_future_base() {
    let (metadata, first, second) = open_two();
    block_on(first.write_file("/shared", b"........")).expect("create shared file");
    let future = now_ms()
        .checked_add(60_000)
        .expect("test timestamp fits i64");
    block_on(first.utimes("/shared", future, future)).expect("set future file mtime");
    block_on(first.utimes("/", future, future)).expect("set future root mtime");

    let first_handle = block_on(first.open("/shared", "r+", 0)).expect("first opens shared");
    let second_handle = block_on(second.open("/shared", "r+", 0)).expect("second opens shared");
    block_on(first_handle.write(b"AAAA", Some(0))).expect("first byte range write");
    let first_mtime = block_on(first.stat("/shared"))
        .expect("first file stat")
        .mtime_ms;
    assert!(
        first_mtime > future,
        "data writes must advance a previously future mtime"
    );

    metadata.force_one_known_conflict();
    block_on(second_handle.write(b"BBBB", Some(4))).expect("rebased second byte range write");
    let second_mtime = block_on(second.stat("/shared"))
        .expect("second file stat")
        .mtime_ms;
    assert!(
        second_mtime > first_mtime,
        "the second CAS write needs a new mtime"
    );
    assert_eq!(read_file(&first, "/shared"), b"AAAABBBB");

    block_on(second.rename("/shared", "/renamed")).expect("change root entry");
    let root_mtime = block_on(first.stat("/")).expect("root stat").mtime_ms;
    assert!(
        root_mtime > future,
        "directory entry mutations need a new mtime"
    );

    block_on(first_handle.close()).expect("close first handle");
    block_on(second_handle.close()).expect("close second handle");
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn concurrent_metadata_mutations_keep_ctime_after_a_newer_revision() {
    let (metadata, first, second) = open_two();
    block_on(first.write_file("/shared", b"bytes")).expect("create shared file");
    let future = now_ms()
        .checked_add(60_000)
        .expect("test timestamp fits i64");
    let loaded = block_on(metadata.load()).expect("load shared metadata");
    let mut namespace = loaded.namespace.expect("published namespace");
    let inode = match &namespace.nodes[&namespace.root].data {
        NodeData::Directory { entries } => {
            entries
                .iter()
                .find(|entry| entry.name == "shared")
                .expect("shared entry")
                .inode
        }
        _ => panic!("root must be a directory"),
    };
    namespace
        .nodes
        .get_mut(&inode)
        .expect("shared node")
        .stats
        .ctime_ms = future;
    let root = namespace.root;
    namespace
        .nodes
        .get_mut(&root)
        .expect("root node")
        .stats
        .ctime_ms = future;
    block_on(metadata.publish_if_revision(loaded.revision, namespace))
        .expect("publish a newer remote timestamp");

    let mut prior = future;
    block_on(second.chmod("/shared", 0o600)).expect("chmod shared");
    let next = block_on(first.stat("/shared"))
        .expect("stat chmod")
        .ctime_ms;
    assert!(next > prior, "chmod must advance the persisted ctime");
    prior = next;

    block_on(first.chown("/shared", 42, 43)).expect("chown shared");
    let next = block_on(second.stat("/shared"))
        .expect("stat chown")
        .ctime_ms;
    assert!(next > prior, "chown must advance the persisted ctime");
    prior = next;

    block_on(second.utimes("/shared", 1, 2)).expect("utimes shared");
    let times = block_on(first.stat("/shared")).expect("stat utimes");
    assert!(
        times.ctime_ms > prior,
        "utimes must advance the persisted ctime"
    );
    assert_eq!(times.mtime_ms, 2, "utimes must keep the requested mtime");
    prior = times.ctime_ms;

    metadata.force_one_known_conflict();
    block_on(second.link("/shared", "/hard")).expect("link shared after CAS conflict");
    let next = block_on(first.stat("/shared"))
        .expect("stat hardlink")
        .ctime_ms;
    assert!(next > prior, "hardlink must advance the persisted ctime");
    assert!(metadata.conflict_count() >= 1, "test must force CAS replay");
    prior = next;

    block_on(first.rename("/shared", "/renamed")).expect("rename shared");
    let next = block_on(second.stat("/renamed"))
        .expect("stat renamed file")
        .ctime_ms;
    assert!(next > prior, "rename must advance the source ctime");
    prior = next;

    block_on(second.unlink("/hard")).expect("unlink one hardlink");
    let next = block_on(first.stat("/renamed"))
        .expect("stat after unlink")
        .ctime_ms;
    assert!(next > prior, "unlink must advance the target ctime");
    assert!(
        block_on(first.stat("/")).expect("stat parent").ctime_ms > future,
        "directory entry mutations must advance parent ctime"
    );

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn remote_rename_and_unlink_preserve_existing_inode_handle() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/original", b"content")).expect("create original");
    let handle = block_on(first.open("/original", "r+", 0)).expect("first opens original");

    block_on(second.rename("/original", "/moved")).expect("second renames original");
    let mut bytes = vec![0; 7];
    assert_eq!(block_on(handle.read(&mut bytes, Some(0))).unwrap(), 7);
    assert_eq!(bytes, b"content");
    assert_eq!(block_on(handle.write(b"updated", Some(0))).unwrap(), 7);
    assert_eq!(read_file(&second, "/moved"), b"updated");

    block_on(second.unlink("/moved")).expect("second unlinks moved file");
    assert_eq!(
        block_on(first.stat("/moved")).unwrap_err().code,
        ErrorCode::Enoent
    );
    bytes.fill(0);
    assert_eq!(block_on(handle.read(&mut bytes, Some(0))).unwrap(), 7);
    assert_eq!(bytes, b"updated");
    assert_eq!(block_on(handle.write(b"private", Some(0))).unwrap(), 7);
    bytes.fill(0);
    assert_eq!(block_on(handle.read(&mut bytes, Some(0))).unwrap(), 7);
    assert_eq!(bytes, b"private");

    block_on(handle.close()).expect("close detached handle");
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn remote_rmdir_keeps_an_open_directory_handle_alive_until_close() {
    let (_, first, second) = open_two();
    block_on(first.mkdir("/gone", MkdirOptions::default())).expect("create directory");
    let handle = block_on(first.open("/gone", "r", 0)).expect("open read-only directory");

    block_on(second.rmdir("/gone")).expect("other coordinator removes directory");
    assert_eq!(
        block_on(first.stat("/gone")).unwrap_err().code,
        ErrorCode::Enoent
    );
    let stats = block_on(handle.stat()).expect("open directory handle remains valid");
    assert_eq!(FileType::from_mode(stats.mode), FileType::Directory);

    block_on(handle.close()).expect("close detached directory handle");
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn local_rmdir_and_rename_over_directory_preserve_open_handles() {
    let (_, first, second) = open_two();
    block_on(first.mkdir("/removed", MkdirOptions::default())).expect("create removed dir");
    let removed = block_on(first.open("/removed", "r", 0)).expect("open removed dir");
    block_on(first.rmdir("/removed")).expect("locally remove directory");
    assert_eq!(
        block_on(first.stat("/removed")).unwrap_err().code,
        ErrorCode::Enoent
    );
    assert_eq!(
        FileType::from_mode(block_on(removed.stat()).expect("detached dir fstat").mode),
        FileType::Directory,
    );

    block_on(first.mkdir("/source", MkdirOptions::default())).expect("create source dir");
    block_on(first.mkdir("/target", MkdirOptions::default())).expect("create target dir");
    let replaced = block_on(first.open("/target", "r", 0)).expect("open target dir");
    let target_inode = block_on(replaced.stat())
        .expect("original target fstat")
        .ino;
    block_on(first.rename("/source", "/target")).expect("rename over target dir");
    assert_ne!(
        block_on(second.stat("/target"))
            .expect("new target stat")
            .ino,
        target_inode
    );
    assert_eq!(
        block_on(replaced.stat())
            .expect("detached target fstat")
            .ino,
        target_inode
    );

    block_on(removed.close()).expect("close removed dir");
    block_on(replaced.close()).expect("close replaced dir");
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn close_releases_local_handle_when_remote_metadata_load_fails() {
    let (metadata, first, second) = open_two();
    block_on(first.mkdir("/gone", MkdirOptions::default())).expect("create directory");
    let handle = block_on(first.open("/gone", "r", 0)).expect("open directory");
    block_on(first.rmdir("/gone")).expect("remove directory");

    metadata.fail_load(true);
    block_on(handle.close()).expect("close only needs local handle release");
    metadata.fail_load(false);
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn forced_cas_conflict_replays_whole_file_and_unlink_mutations() {
    let (metadata, first, second) = open_two();
    block_on(first.write_file("/existing", b"old")).expect("create existing file");

    metadata.force_one_known_conflict();
    block_on(second.write_file("/new", b"new content"))
        .expect("whole-file create reloads after a known CAS conflict");
    assert_eq!(read_file(&first, "/new"), b"new content");

    metadata.force_one_known_conflict();
    block_on(first.unlink("/existing")).expect("unlink reloads after a known CAS conflict");
    assert_eq!(
        block_on(second.stat("/existing")).unwrap_err().code,
        ErrorCode::Enoent
    );
    assert!(metadata.conflict_count() >= 2);

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn whole_file_replace_is_one_visible_revision_after_cas_conflict() {
    let (metadata, first, second) = open_two();
    block_on(first.write_file("/atomic", b"old")).expect("create baseline file");
    let previous = metadata.published_count();

    metadata.force_one_known_conflict();
    block_on(second.write_file("/atomic", b"new bytes"))
        .expect("replace whole file after known CAS loss");
    assert_eq!(read_file(&first, "/atomic"), b"new bytes");
    assert_eq!(
        metadata.published_file_sizes_since("/atomic", previous),
        vec![9],
        "readers must never observe an empty or partial whole-file replacement"
    );

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn ambiguous_commit_fails_closed_without_replaying_the_whole_file() {
    let (metadata, first, second) = open_two();
    let before = metadata.published_count();
    metadata.force_one_ambiguous_commit_after_apply();

    let error = block_on(first.write_file("/maybe", b"once"))
        .expect_err("lost acknowledgement cannot count as successful write");
    assert_eq!(error.code, ErrorCode::Eio);
    assert_eq!(metadata.published_count(), before + 1);
    assert_eq!(read_file(&second, "/maybe"), b"once");

    let second_error = block_on(first.write_file("/later", b"do not replay"))
        .expect_err("failed coordinator must reject further writes");
    assert_eq!(second_error.code, ErrorCode::Eio);
    assert_eq!(metadata.published_count(), before + 1);

    block_on(first.shutdown()).expect("failed coordinator releases terminal state");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn canceled_after_applied_cas_fails_closed_without_replay() {
    let (metadata, first, second) = open_two();
    let before = metadata.published_count();
    metadata.suspend_one_commit_after_apply();

    let mut write = Box::pin(first.write_file("/maybe", b"once"));
    let waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    for _ in 0..1_000 {
        assert!(matches!(
            Future::poll(write.as_mut(), &mut context),
            Poll::Pending
        ));
        if metadata.published_count() == before + 1 {
            break;
        }
    }
    assert_eq!(metadata.published_count(), before + 1);
    drop(write);

    assert_eq!(read_file(&second, "/maybe"), b"once");
    let error = block_on(first.write_file("/later", b"do not replay"))
        .expect_err("a canceled publication has an unknown outcome");
    assert_eq!(error.code, ErrorCode::Eio);
    assert_eq!(metadata.published_count(), before + 1);

    block_on(first.shutdown()).expect("failed coordinator shuts down");
    block_on(second.shutdown()).expect("second coordinator shuts down");
}

#[test]
fn canceled_after_applied_lease_publication_fails_closed_and_releases_lease() {
    let metadata = CasMetadata::default();
    let filesystem = block_on(ChunkedFs::open(
        metadata.clone(),
        SharedBlocks::default(),
        ChunkedOptions::fixed("legacy-writer", 4).expect("fixed chunker"),
    ))
    .expect("legacy coordinator opens");
    let before = metadata.published_count();
    metadata.suspend_one_commit_after_apply();

    let mut write = Box::pin(filesystem.write_file("/maybe", b"once"));
    let waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    for _ in 0..1_000 {
        assert!(matches!(
            Future::poll(write.as_mut(), &mut context),
            Poll::Pending
        ));
        if metadata.published_count() == before + 1 {
            break;
        }
    }
    assert_eq!(metadata.published_count(), before + 1);
    drop(write);

    let error = block_on(filesystem.write_file("/later", b"do not replay"))
        .expect_err("a canceled lease publication has an unknown outcome");
    assert_eq!(error.code, ErrorCode::Eio);
    assert_eq!(metadata.published_count(), before + 1);
    block_on(filesystem.shutdown()).expect("failed coordinator releases the lease");
    let replacement = block_on(metadata.acquire_writer("replacement", Duration::from_secs(10)))
        .expect("exact legacy lease was released");
    block_on(metadata.release_writer(&replacement)).expect("release replacement lease");
}

#[test]
fn canceled_batched_follower_after_applied_cas_fails_coordinator_closed() {
    let (metadata, first, second) = open_two();
    let before = metadata.published_count();
    metadata.suspend_one_commit_after_apply();

    let waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    let mut runner = Box::pin(first.write_file("/runner", b"runner"));
    assert!(matches!(
        Future::poll(runner.as_mut(), &mut context),
        Poll::Pending
    ));
    let mut follower = Box::pin(first.write_file("/follower", b"follower"));
    assert!(matches!(
        Future::poll(follower.as_mut(), &mut context),
        Poll::Pending
    ));

    for _ in 0..1_000 {
        assert!(matches!(
            Future::poll(runner.as_mut(), &mut context),
            Poll::Pending
        ));
        if metadata.published_count() == before + 1 {
            break;
        }
    }
    assert_eq!(metadata.published_count(), before + 1);
    drop(follower);
    metadata.resume_suspended_commit();
    block_on(runner).expect("runner's acknowledged write committed");

    assert_eq!(read_file(&second, "/runner"), b"runner");
    assert_eq!(read_file(&second, "/follower"), b"follower");
    let error = block_on(first.write_file("/later", b"later"))
        .expect_err("a committed follower lost its publication acknowledgement");
    assert_eq!(error.code, ErrorCode::Eio);
    assert_eq!(metadata.published_count(), before + 1);
    block_on(first.shutdown()).expect("failed coordinator shuts down");
    block_on(second.shutdown()).expect("second coordinator shuts down");
}

#[test]
fn canceled_batched_follower_before_publication_is_not_committed() {
    let (metadata, first, second) = open_two();
    let before = metadata.published_count();
    let waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    let mut runner = Box::pin(first.write_file("/runner", b"runner"));
    assert!(matches!(
        Future::poll(runner.as_mut(), &mut context),
        Poll::Pending
    ));
    let mut follower = Box::pin(first.write_file("/follower", b"follower"));
    assert!(matches!(
        Future::poll(follower.as_mut(), &mut context),
        Poll::Pending
    ));
    drop(follower);
    block_on(runner).expect("runner's write committed");

    assert_eq!(metadata.published_count(), before + 1);
    assert_eq!(read_file(&second, "/runner"), b"runner");
    assert_eq!(
        block_on(second.stat("/follower")).unwrap_err().code,
        ErrorCode::Enoent
    );
    block_on(first.write_file("/later", b"later"))
        .expect("prepublication cancellation leaves the coordinator usable");
    block_on(first.shutdown()).expect("first coordinator shuts down");
    block_on(second.shutdown()).expect("second coordinator shuts down");
}

#[test]
fn canceled_batched_follower_after_response_send_fails_coordinator_closed() {
    let (metadata, first, second) = open_two();
    let before = metadata.published_count();
    let waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    let mut runner = Box::pin(first.write_file("/runner", b"runner"));
    assert!(matches!(
        Future::poll(runner.as_mut(), &mut context),
        Poll::Pending
    ));
    let mut follower = Box::pin(first.write_file("/follower", b"follower"));
    assert!(matches!(
        Future::poll(follower.as_mut(), &mut context),
        Poll::Pending
    ));

    block_on(runner).expect("runner completes the shared publication and receives its response");
    assert_eq!(metadata.published_count(), before + 1);
    assert_eq!(read_file(&second, "/runner"), b"runner");
    assert_eq!(read_file(&second, "/follower"), b"follower");
    // The follower's response was sent successfully but has not been read.
    drop(follower);

    let error = block_on(first.write_file("/later", b"later"))
        .expect_err("the follower canceled before consuming its committed response");
    assert_eq!(error.code, ErrorCode::Eio);
    assert_eq!(metadata.published_count(), before + 1);
    block_on(first.shutdown()).expect("failed coordinator shuts down");
    block_on(second.shutdown()).expect("second coordinator shuts down");
}

#[test]
fn canceled_shutdown_retries_the_exact_unreleased_lease() {
    let metadata = CasMetadata::default();
    let filesystem = block_on(ChunkedFs::open(
        metadata.clone(),
        SharedBlocks::default(),
        ChunkedOptions::fixed("shutdown-cancel", 4).expect("fixed chunker"),
    ))
    .expect("legacy coordinator opens");
    metadata.suspend_one_release_before_apply();

    let mut shutdown = Box::pin(filesystem.shutdown());
    let waker = Waker::from(Arc::new(NoopWaker));
    let mut context = Context::from_waker(&waker);
    for _ in 0..1_000 {
        assert!(matches!(
            Future::poll(shutdown.as_mut(), &mut context),
            Poll::Pending
        ));
        if metadata.release_entered.load(Ordering::SeqCst) {
            break;
        }
    }
    assert!(metadata.release_entered.load(Ordering::SeqCst));
    drop(shutdown);
    assert_eq!(
        block_on(metadata.acquire_writer("replacement", Duration::from_secs(10)))
            .unwrap_err()
            .code,
        ErrorCode::Eagain,
        "the suspended release has not changed provider state"
    );

    block_on(filesystem.shutdown()).expect("retry must release the retained lease token");
    let replacement = block_on(metadata.acquire_writer("replacement", Duration::from_secs(10)))
        .expect("retry released the old writer lease");
    block_on(metadata.release_writer(&replacement)).expect("release replacement lease");
}

#[test]
fn shutdown_attempts_lease_release_after_transient_refresh_error() {
    let metadata = CasMetadata::default();
    let filesystem = block_on(ChunkedFs::open(
        metadata.clone(),
        SharedBlocks::default(),
        ChunkedOptions::fixed("shutdown-renew-error", 4).expect("fixed chunker"),
    ))
    .expect("legacy coordinator opens");
    metadata.fail_next_renew();

    let error = block_on(filesystem.shutdown())
        .expect_err("transient renewal error remains visible to the caller");
    assert_eq!(error.code, ErrorCode::Eio);
    let replacement = block_on(metadata.acquire_writer("replacement", Duration::from_secs(10)))
        .expect("renewal error must not strand a valid lease");
    block_on(metadata.release_writer(&replacement)).expect("release replacement lease");
    block_on(filesystem.shutdown()).expect("completed release remains idempotent locally");
}

#[test]
fn forced_cas_conflict_replays_open_create_and_truncate() {
    let (metadata, first, second) = open_two();
    block_on(first.write_file("/shared", b"ABCDEFGH")).expect("create shared file");

    metadata.force_one_known_conflict();
    let created = block_on(second.open("/created", "w+", 0o666))
        .expect("open create reloads after a known CAS conflict");
    block_on(created.close()).expect("close created file");
    assert!(block_on(first.stat("/created")).is_ok());

    metadata.force_one_known_conflict();
    let opened = block_on(second.open("/shared", "w+", 0o666))
        .expect("open truncate reloads after a known CAS conflict");
    assert_eq!(
        block_on(first.stat("/shared")).expect("shared stat").size,
        0
    );
    block_on(opened.write(b"ABCDEFGH", Some(0))).expect("restore shared data");

    metadata.force_one_known_conflict();
    block_on(opened.truncate(3)).expect("ftruncate reloads after a known CAS conflict");
    assert_eq!(read_file(&first, "/shared"), b"ABC");
    assert!(metadata.conflict_count() >= 3);

    block_on(opened.close()).expect("close shared file");
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn concurrent_marker_fences_legacy_writer_acquisition() {
    let (metadata, first, second) = open_two();
    let error = block_on(metadata.acquire_writer("legacy", Duration::from_secs(30)))
        .expect_err("legacy writer must be fenced after concurrent mode opens");
    assert_eq!(error.code, ErrorCode::Enotsup);
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_setattr_rejects_recreated_file_after_handle_identity_changes() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/target", b"original")).expect("create original target");
    let handle = block_on(first.open("/target", "r+", 0)).expect("open original file handle");
    let original = PathGuard {
        path: "/target".to_owned(),
        identity: PathIdentity::from_stats(&block_on(handle.stat()).expect("original fstat"))
            .expect("original file handle has an inode"),
    };

    block_on(second.rename("/target", "/old")).expect("rename original away");
    block_on(second.write_file("/target", b"replacement"))
        .expect("replace original path with another inode");
    let replacement = block_on(second.stat("/target")).expect("replacement stat");
    assert_ne!(replacement.ino, original.identity.ino);

    let error = guarded_error(
        &first,
        GuardedMutation::Setattr {
            target: original,
            change: GuardedSetattr {
                mode: Some(0o600),
                uid: Some(42),
                size: Some(2),
                ..GuardedSetattr::default()
            },
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    let after = block_on(second.stat("/target")).expect("replacement remains visible");
    assert_eq!(
        (after.ino, after.mode, after.uid, after.size),
        (
            replacement.ino,
            replacement.mode,
            replacement.uid,
            replacement.size,
        )
    );
    assert_eq!(read_file(&second, "/target"), b"replacement");
    assert_eq!(read_file(&second, "/old"), b"original");

    block_on(handle.close()).expect("close original handle");
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_setattr_rejects_unlinked_and_recreated_file() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/target", b"original")).expect("create original target");
    let original = path_guard(&first, "/target");
    block_on(second.unlink("/target")).expect("unlink original target");
    block_on(second.write_file("/target", b"replacement"))
        .expect("create a different target inode");
    let replacement = block_on(second.stat("/target")).expect("replacement stat");
    assert_ne!(replacement.ino, original.identity.ino);

    let error = guarded_error(
        &first,
        GuardedMutation::Setattr {
            target: original,
            change: GuardedSetattr {
                mode: Some(0o600),
                ..GuardedSetattr::default()
            },
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    let after = block_on(second.stat("/target")).expect("replacement remains visible");
    assert_eq!((after.ino, after.mode), (replacement.ino, replacement.mode));
    assert_eq!(read_file(&second, "/target"), b"replacement");

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_mkdir_rejects_recreated_parent_directory() {
    let (_, first, second) = open_two();
    block_on(first.mkdir("/parent", MkdirOptions::default())).expect("create original parent");
    let parent = path_guard(&first, "/parent");
    block_on(second.rename("/parent", "/moved")).expect("move original parent away");
    block_on(second.mkdir("/parent", MkdirOptions::default()))
        .expect("recreate parent at the same path");
    assert_ne!(
        block_on(second.stat("/parent"))
            .expect("new parent stat")
            .ino,
        parent.identity.ino
    );

    let error = guarded_error(
        &first,
        GuardedMutation::Mkdir {
            parent,
            name: "child".to_owned(),
            mode: 0o755,
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    assert_eq!(
        block_on(second.stat("/parent/child")).unwrap_err().code,
        ErrorCode::Enoent
    );
    assert_eq!(
        block_on(second.stat("/moved/child")).unwrap_err().code,
        ErrorCode::Enoent
    );

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_creates_keep_eexist_for_an_existing_child_of_the_same_parent() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/existing", b"keep me")).expect("create existing child");
    let parent = path_guard(&first, "/");
    let original = block_on(second.stat("/existing")).expect("existing child stat");

    for request in [
        GuardedMutation::Mkdir {
            parent: parent.clone(),
            name: "existing".to_owned(),
            mode: 0o755,
        },
        GuardedMutation::Symlink {
            parent: parent.clone(),
            name: "existing".to_owned(),
            target: "somewhere".to_owned(),
        },
        GuardedMutation::Mknod {
            parent,
            name: "existing".to_owned(),
            mode: mount_rs_core::types::S_IFREG | 0o644,
            dev: 0,
        },
    ] {
        assert_eq!(guarded_error(&second, request).code, ErrorCode::Eexist);
    }
    assert_eq!(
        block_on(first.stat("/existing"))
            .expect("existing child remains")
            .ino,
        original.ino
    );
    assert_eq!(read_file(&first, "/existing"), b"keep me");

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_rename_rejects_rebound_source_entry() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/source", b"old source")).expect("create source");
    let source = path_guard(&first, "/source");
    let root = path_guard(&first, "/");
    block_on(second.rename("/source", "/moved")).expect("move source away");
    block_on(second.write_file("/source", b"new source"))
        .expect("replace source name with another inode");

    let error = guarded_error(
        &first,
        GuardedMutation::Rename {
            from_parent: root.clone(),
            from_name: "source".to_owned(),
            source: ObservedEntry::Identity(source.identity),
            to_parent: root,
            to_name: "destination".to_owned(),
            destination: ObservedEntry::Absent,
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    assert_eq!(read_file(&second, "/source"), b"new source");
    assert_eq!(read_file(&second, "/moved"), b"old source");
    assert_eq!(
        block_on(second.stat("/destination")).unwrap_err().code,
        ErrorCode::Enoent
    );

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_rename_rejects_rebound_destination_entry() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/source", b"source bytes")).expect("create source");
    block_on(first.write_file("/destination", b"old destination")).expect("create old destination");
    let source = path_guard(&first, "/source");
    let destination = path_guard(&first, "/destination");
    let root = path_guard(&first, "/");
    block_on(second.rename("/destination", "/old-destination"))
        .expect("move observed destination away");
    block_on(second.write_file("/destination", b"new destination"))
        .expect("replace destination name with another inode");

    let error = guarded_error(
        &first,
        GuardedMutation::Rename {
            from_parent: root.clone(),
            from_name: "source".to_owned(),
            source: ObservedEntry::Identity(source.identity),
            to_parent: root,
            to_name: "destination".to_owned(),
            destination: ObservedEntry::Identity(destination.identity),
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    assert_eq!(read_file(&second, "/source"), b"source bytes");
    assert_eq!(read_file(&second, "/destination"), b"new destination");
    assert_eq!(read_file(&second, "/old-destination"), b"old destination");

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_link_rejects_rebound_source_but_accepts_same_inode_alias() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/source", b"source bytes")).expect("create source");
    let source = path_guard(&first, "/source");
    let root = path_guard(&first, "/");
    block_on(second.rename("/source", "/moved")).expect("move source away");
    block_on(second.write_file("/source", b"replacement"))
        .expect("create another inode at source path");

    let error = guarded_error(
        &first,
        GuardedMutation::Link {
            source: source.clone(),
            to_parent: root.clone(),
            to_name: "wrong-link".to_owned(),
            destination: ObservedEntry::Absent,
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    assert_eq!(
        block_on(second.stat("/wrong-link")).unwrap_err().code,
        ErrorCode::Enoent
    );
    assert_eq!(read_file(&second, "/source"), b"replacement");

    block_on(second.link("/moved", "/alias")).expect("create alias of original inode");
    let alias = PathGuard {
        path: "/alias".to_owned(),
        identity: source.identity,
    };
    assert_guarded_applied(
        &first,
        GuardedMutation::Link {
            source: alias,
            to_parent: root,
            to_name: "right-link".to_owned(),
            destination: ObservedEntry::Absent,
        },
    );
    assert_eq!(
        block_on(second.stat("/right-link"))
            .expect("new alias stat")
            .ino,
        source.identity.ino
    );
    assert_eq!(read_file(&second, "/right-link"), b"source bytes");

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_setattr_rechecks_identity_after_forced_cas_swap() {
    let (metadata, first, second) = open_two();
    block_on(first.write_file("/target", b"target bytes")).expect("create target X");
    block_on(first.write_file("/other", b"other bytes")).expect("create replacement Y");
    let target = path_guard(&first, "/target");
    let other = block_on(second.stat("/other")).expect("replacement stats before swap");
    metadata.swap_entries_on_next_cas("target", "other");

    let error = guarded_error(
        &first,
        GuardedMutation::Setattr {
            target,
            change: GuardedSetattr {
                mode: Some(0o600),
                size: Some(2),
                ..GuardedSetattr::default()
            },
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    assert!(
        metadata.conflict_count() >= 1,
        "fake provider must swap the names before guarded CAS publication"
    );
    let after = block_on(second.stat("/target")).expect("replacement Y now at target");
    assert_eq!(
        (after.ino, after.mode, after.size),
        (other.ino, other.mode, other.size,)
    );
    assert_eq!(read_file(&second, "/target"), b"other bytes");
    assert_eq!(read_file(&second, "/other"), b"target bytes");

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_setattr_rejects_old_ctime_on_the_same_inode_without_partial_changes() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/target", b"original")).expect("create guarded target");
    let target = path_guard(&first, "/target");
    let observed_ctime = block_on(first.stat("/target"))
        .expect("older target stat")
        .ctime_ms;

    block_on(second.chmod("/target", 0o640)).expect("newer mode change");
    block_on(second.chown("/target", 77, 88)).expect("newer owner change");
    let handle = block_on(second.open("/target", "r+", 0)).expect("newer size handle");
    block_on(handle.truncate(3)).expect("newer size change");
    block_on(handle.close()).expect("close newer size handle");
    let newer = block_on(second.stat("/target")).expect("newer target stat");
    assert_eq!(newer.ino, target.identity.ino);
    assert!(newer.ctime_ms > observed_ctime);

    let error = guarded_error(
        &first,
        GuardedMutation::Setattr {
            target,
            change: GuardedSetattr {
                mode: Some(0o600),
                uid: Some(42),
                gid: Some(43),
                size: Some(1),
                expected_ctime_ms: Some(observed_ctime),
                ..GuardedSetattr::default()
            },
        },
    );
    assert_eq!(error.code, ErrorCode::Eagain);
    let after = block_on(second.stat("/target")).expect("target after guarded failure");
    assert_eq!(
        (
            after.ino,
            after.mode,
            after.uid,
            after.gid,
            after.size,
            after.ctime_ms
        ),
        (
            newer.ino,
            newer.mode,
            newer.uid,
            newer.gid,
            newer.size,
            newer.ctime_ms
        )
    );
    assert_eq!(read_file(&second, "/target"), b"ori");

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn exclusive_mode_guarded_setattr_applies_then_rejects_rebound_path() {
    let fs: TestFs = block_on(ChunkedFs::open(
        CasMetadata::default(),
        SharedBlocks::default(),
        ChunkedOptions::fixed("exclusive-writer", 4).expect("fixed chunker"),
    ))
    .expect("exclusive coordinator opens");
    assert!(fs.supports_guarded_mutations());
    block_on(fs.write_file("/target", b"original")).expect("create original file");
    let original = path_guard(&fs, "/target");

    assert_guarded_applied(
        &fs,
        GuardedMutation::Setattr {
            target: original.clone(),
            change: GuardedSetattr {
                mode: Some(0o600),
                size: Some(4),
                ..GuardedSetattr::default()
            },
        },
    );
    let changed = block_on(fs.stat("/target")).expect("changed target stat");
    assert_eq!(changed.mode & 0o777, 0o600);
    assert_eq!(read_file(&fs, "/target"), b"orig");

    block_on(fs.rename("/target", "/old")).expect("move guarded inode away");
    block_on(fs.write_file("/target", b"replacement"))
        .expect("create a different inode at target path");
    let replacement = block_on(fs.stat("/target")).expect("replacement stat");
    assert_ne!(replacement.ino, original.identity.ino);
    let error = guarded_error(
        &fs,
        GuardedMutation::Setattr {
            target: original,
            change: GuardedSetattr {
                mode: Some(0o700),
                size: Some(1),
                ..GuardedSetattr::default()
            },
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    let after = block_on(fs.stat("/target")).expect("replacement after stale request");
    assert_eq!(
        (after.ino, after.mode, after.size),
        (replacement.ino, replacement.mode, replacement.size)
    );
    assert_eq!(read_file(&fs, "/target"), b"replacement");

    block_on(fs.shutdown()).expect("exclusive coordinator shuts down");
}

#[test]
fn guarded_open_rejects_symlink_trap_before_truncating_its_target() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/victim", b"keep victim bytes")).expect("create victim file");
    block_on(first.symlink("/victim", "/trap")).expect("create final symlink trap");
    let parent = path_guard(&second, "/");
    let trap = path_guard(&second, "/trap");
    let victim = block_on(second.stat("/victim")).expect("victim before open");

    let error = guarded_error(
        &second,
        GuardedMutation::Open {
            parent,
            name: "trap".to_owned(),
            observed: ObservedEntry::Identity(trap.identity),
            flags: OpenFlags::parse("w+", "/trap").expect("truncate flags"),
            mode: 0o666,
        },
    );
    assert_eq!(error.code, ErrorCode::Eexist);
    let after = block_on(first.stat("/victim")).expect("victim after rejected open");
    assert_eq!((after.ino, after.size), (victim.ino, victim.size));
    assert_eq!(read_file(&first, "/victim"), b"keep victim bytes");
    assert_eq!(
        block_on(first.lstat("/trap"))
            .expect("trap remains symlink")
            .ino,
        trap.identity.ino
    );

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_open_requires_observed_inode_before_truncating_existing_file() {
    let (_, first, second) = open_two();
    block_on(first.write_file("/regular", b"keep regular bytes")).expect("create regular file");
    let parent = path_guard(&first, "/");
    let file = path_guard(&first, "/regular");
    let flags = OpenFlags::parse("w+", "/regular").expect("truncate flags");

    let error = guarded_error(
        &second,
        GuardedMutation::Open {
            parent: parent.clone(),
            name: "regular".to_owned(),
            observed: ObservedEntry::Any,
            flags,
            mode: 0o666,
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    assert_eq!(read_file(&first, "/regular"), b"keep regular bytes");

    let (handle, identity) = match block_on(second.guarded_mutation(GuardedMutation::Open {
        parent,
        name: "regular".to_owned(),
        observed: ObservedEntry::Identity(file.identity),
        flags,
        mode: 0o666,
    })) {
        Ok(GuardedMutationResult::Opened { handle, identity }) => (handle, identity),
        Ok(_) => panic!("guarded open did not return an open handle"),
        Err(error) => panic!("observed inode open failed: {error:?}"),
    };
    assert_eq!(identity, file.identity);
    assert_eq!(block_on(handle.stat()).expect("truncated fstat").size, 0);
    assert_eq!(
        block_on(first.stat("/regular"))
            .expect("truncated stat")
            .size,
        0
    );
    block_on(handle.close()).expect("close guarded handle");

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_open_rechecks_observed_inode_after_forced_cas_swap() {
    let (metadata, first, second) = open_two();
    block_on(first.write_file("/target", b"target bytes")).expect("create observed target X");
    block_on(first.write_file("/other", b"other bytes")).expect("create replacement inode Y");
    let parent = path_guard(&first, "/");
    let target = path_guard(&first, "/target");
    metadata.swap_entries_on_next_cas("target", "other");

    let error = guarded_error(
        &second,
        GuardedMutation::Open {
            parent,
            name: "target".to_owned(),
            observed: ObservedEntry::Identity(target.identity),
            flags: OpenFlags::parse("w+", "/target").expect("truncate flags"),
            mode: 0o666,
        },
    );
    assert_eq!(error.code, ErrorCode::Estale);
    assert!(
        metadata.conflict_count() >= 1,
        "fake provider must swap the observed path before retry"
    );
    assert_eq!(read_file(&first, "/target"), b"other bytes");
    assert_eq!(read_file(&first, "/other"), b"target bytes");

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_reads_return_original_parent_and_child_attributes() {
    let (_, first, second) = open_two();
    block_on(first.mkdir("/outer", MkdirOptions::default())).expect("create outer parent");
    block_on(first.mkdir("/outer/dir", MkdirOptions::default())).expect("create guarded dir");
    block_on(first.write_file("/outer/dir/child", b"child bytes")).expect("create directory child");
    block_on(first.symlink("/unresolved", "/outer/dir/link"))
        .expect("create nofollow symlink child");
    let directory = path_guard(&second, "/outer/dir");
    let outer = path_guard(&second, "/outer");
    let child = block_on(second.lstat("/outer/dir/child")).expect("child attributes");
    let link = block_on(second.lstat("/outer/dir/link")).expect("link attributes");

    let GuardedReadResult::Stat(stat) = block_on(second.guarded_read(GuardedRead::Stat {
        target: directory.clone(),
    }))
    .expect("guarded directory stat") else {
        panic!("guarded stat returned another result kind");
    };
    assert_eq!(stat.ino, directory.identity.ino);

    for (name, expected_ino) in [
        (".", directory.identity.ino),
        ("..", outer.identity.ino),
        ("child", child.ino),
    ] {
        let GuardedReadResult::Lookup {
            parent,
            child: looked_up,
        } = block_on(second.guarded_read(GuardedRead::Lookup {
            parent: directory.clone(),
            name: name.to_owned(),
        }))
        .expect("guarded directory lookup")
        else {
            panic!("guarded lookup returned another result kind");
        };
        assert_eq!(
            parent.ino, directory.identity.ino,
            "lookup {name} must retain its guarded parent"
        );
        assert_eq!(
            looked_up.ino, expected_ino,
            "lookup {name} must use original parent"
        );
    }

    assert_eq!(
        guarded_read_error(
            &second,
            GuardedRead::Readdir {
                directory: directory.clone(),
                max_entries: 1,
            },
        )
        .code,
        ErrorCode::Eoverflow,
        "two existing children must overflow a one-entry bound"
    );
    let GuardedReadResult::Directory { stats, entries } =
        block_on(second.guarded_read(GuardedRead::Readdir {
            directory: directory.clone(),
            max_entries: usize::MAX,
        }))
        .expect("guarded directory listing")
    else {
        panic!("guarded readdir returned another result kind");
    };
    assert_eq!(stats.ino, directory.identity.ino);
    assert_eq!(entries.len(), 2, "unbounded listing retains both children");
    let listed_child = entries
        .iter()
        .find(|entry| entry.name == "child")
        .expect("child in guarded listing");
    assert_eq!(
        (listed_child.stats.ino, listed_child.stats.size),
        (child.ino, child.size)
    );
    let listed_link = entries
        .iter()
        .find(|entry| entry.name == "link")
        .expect("nofollow link in guarded listing");
    assert_eq!(listed_link.stats.ino, link.ino);
    assert_eq!(
        FileType::from_mode(listed_link.stats.mode),
        FileType::Symlink
    );

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_reads_reject_rebound_directory_handle_without_exposing_new_children() {
    let (_, first, second) = open_two();
    block_on(first.mkdir("/outer", MkdirOptions::default())).expect("create parent");
    block_on(first.mkdir("/outer/dir", MkdirOptions::default()))
        .expect("create original directory");
    block_on(first.write_file("/outer/dir/original", b"old")).expect("create original child");
    let handle =
        block_on(first.open("/outer/dir", "r", 0)).expect("open original directory handle");
    let directory = PathGuard {
        path: "/outer/dir".to_owned(),
        identity: PathIdentity::from_stats(&block_on(handle.stat()).expect("original dir fstat"))
            .expect("original directory has an inode"),
    };

    block_on(second.rename("/outer/dir", "/outer/moved")).expect("move original directory away");
    block_on(second.mkdir("/outer/dir", MkdirOptions::default()))
        .expect("recreate same directory path");
    block_on(second.write_file("/outer/dir/replacement", b"secret"))
        .expect("create child in replacement directory");
    assert_ne!(
        block_on(second.stat("/outer/dir"))
            .expect("replacement dir stat")
            .ino,
        directory.identity.ino
    );
    assert_eq!(read_file(&second, "/outer/dir/replacement"), b"secret");

    for request in [
        GuardedRead::Stat {
            target: directory.clone(),
        },
        GuardedRead::Lookup {
            parent: directory.clone(),
            name: "replacement".to_owned(),
        },
        GuardedRead::Lookup {
            parent: directory.clone(),
            name: ".".to_owned(),
        },
        GuardedRead::Lookup {
            parent: directory.clone(),
            name: "..".to_owned(),
        },
        GuardedRead::Readdir {
            directory,
            max_entries: 8,
        },
    ] {
        assert_eq!(guarded_read_error(&first, request).code, ErrorCode::Estale);
    }
    let replacement = path_guard(&second, "/outer/dir");
    let GuardedReadResult::Lookup { parent, child } =
        block_on(second.guarded_read(GuardedRead::Lookup {
            parent: replacement.clone(),
            name: "replacement".to_owned(),
        }))
        .expect("new directory handle can look up its own child")
    else {
        panic!("replacement lookup returned another result kind");
    };
    assert_eq!(parent.ino, replacement.identity.ino);
    assert_eq!(child.size, 6);

    block_on(handle.close()).expect("close original directory handle");
    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}

#[test]
fn guarded_readlink_rejects_rebound_symlink_and_returns_matching_attributes() {
    let (_, first, second) = open_two();
    block_on(first.symlink("/old-target", "/link")).expect("create original symlink");
    let original = path_guard(&first, "/link");
    let GuardedReadResult::Readlink { stats, target } =
        block_on(second.guarded_read(GuardedRead::Readlink {
            target: original.clone(),
        }))
        .expect("guarded original readlink")
    else {
        panic!("guarded readlink returned another result kind");
    };
    assert_eq!(stats.ino, original.identity.ino);
    assert_eq!(FileType::from_mode(stats.mode), FileType::Symlink);
    assert_eq!(stats.size, target.len() as u64);
    assert_eq!(target, "/old-target");

    block_on(second.rename("/link", "/moved-link")).expect("move original symlink away");
    block_on(second.symlink("/new-target", "/link")).expect("create replacement symlink");
    assert_eq!(
        guarded_read_error(&first, GuardedRead::Readlink { target: original }).code,
        ErrorCode::Estale
    );
    let replacement = path_guard(&second, "/link");
    let GuardedReadResult::Readlink { stats, target } =
        block_on(second.guarded_read(GuardedRead::Readlink {
            target: replacement.clone(),
        }))
        .expect("new symlink readlink")
    else {
        panic!("replacement readlink returned another result kind");
    };
    assert_eq!(stats.ino, replacement.identity.ino);
    assert_eq!(target, "/new-target");

    block_on(first.shutdown()).expect("first shuts down");
    block_on(second.shutdown()).expect("second shuts down");
}
