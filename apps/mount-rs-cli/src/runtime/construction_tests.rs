//! Actual CLI filesystem ownership across construction and postconfiguration.

use super::*;
use crate::config::SplitStorageConfig;
use mount_rs_core::construction::{ConstructionObserver, ConstructionResource};
use mount_rs_sdk::ConstructionJournal;
use std::sync::{Mutex, Weak};

/// A failed assertion must not remove a backing whose terminal close was not
/// acknowledged. Successful tests remove their temporary backing normally.
struct ProtectedFixture(Option<tempfile::TempDir>);

impl ProtectedFixture {
    fn new() -> Self {
        Self(Some(tempfile::tempdir().unwrap()))
    }

    fn path(&self) -> &Path {
        self.0.as_ref().unwrap().path()
    }
}

impl Drop for ProtectedFixture {
    fn drop(&mut self) {
        if std::thread::panicking() {
            std::mem::forget(self.0.take());
        }
    }
}

/// Records only weak identities; the journal is the sole test-side strong owner.
struct RecordingObserver {
    journal: ConstructionJournal,
    seen: Mutex<Vec<Weak<dyn ConstructionResource>>>,
}

impl RecordingObserver {
    fn new(journal: ConstructionJournal) -> Self {
        Self {
            journal,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn registrations(&self) -> Vec<Weak<dyn ConstructionResource>> {
        self.seen.lock().unwrap().clone()
    }
}

impl ConstructionObserver for RecordingObserver {
    fn retain(&self, resource: Arc<dyn ConstructionResource>) {
        self.seen.lock().unwrap().push(Arc::downgrade(&resource));
        self.journal.retain(resource);
    }
}

fn sqlite_plan(root: &Path, uid: u32, gid: u32, read_only: bool) -> DriverRuntimePlan {
    let options = CliOptions {
        driver: DriverChoice::Sqlite,
        database: Some(root.join("filesystem.db")),
        root_uid: Some(uid),
        root_gid: Some(gid),
        read_only,
        ..CliOptions::default()
    };
    DriverRuntimePlan::resolve_with(&options, 1000, 1001, root, root, |_| {
        unreachable!("SQLite has no environment reference")
    })
    .unwrap()
}

fn compact_plan(root: &Path) -> DriverRuntimePlan {
    let options = CliOptions {
        driver: DriverChoice::SplitStore,
        storage: Some(Box::new(SplitStorageConfig {
            metadata: StorageProvider::Sqlite {
                path: root.join("metadata.db"),
            },
            blocks: StorageProvider::Sqlite {
                path: root.join("blocks.db"),
            },
            chunk_size_bytes: 4096,
            lease_ttl_ms: None,
            concurrent_writes: true,
            inode_updates: true,
            compact_inode_updates: true,
            delegated: false,
            checkout_path: None,
            writeback: false,
            owner: Some("cli-observed-compact-owner".into()),
        })),
        ..CliOptions::default()
    };
    DriverRuntimePlan::resolve_with(&options, 111, 222, root, root, |_| {
        unreachable!("SQLite has no environment reference")
    })
    .unwrap()
}

async fn assert_payload(runtime: &DriverRuntime, expected: &[u8], path: &str) {
    let view = Loopback::from_arc(runtime.driver());
    assert_eq!(view.read_file(path).await.unwrap(), expected);
    assert_eq!(view.stat(path).await.unwrap().size, expected.len() as u64);
    let handle = view.open(path, "r", 0).await.unwrap();

    let mut sequential_start = [0; 17];
    assert_eq!(handle.read(&mut sequential_start, None).await.unwrap(), 17);
    assert_eq!(&sequential_start, &expected[..17]);

    // Explicit offsets run backward across EOF and the chunk boundary. They
    // must neither use nor advance the handle's sequential cursor.
    let mut near_end = [0; 23];
    assert_eq!(
        handle
            .read(&mut near_end, Some(expected.len() as u64 - 11))
            .await
            .unwrap(),
        11
    );
    assert_eq!(&near_end[..11], &expected[expected.len() - 11..]);
    let mut across_chunk = [0; 37];
    assert_eq!(
        handle.read(&mut across_chunk, Some(4093)).await.unwrap(),
        across_chunk.len()
    );
    assert_eq!(&across_chunk, &expected[4093..4130]);
    let mut earlier = [0; 29];
    assert_eq!(
        handle.read(&mut earlier, Some(131)).await.unwrap(),
        earlier.len()
    );
    assert_eq!(&earlier, &expected[131..160]);
    let mut sequential_next = [0; 23];
    assert_eq!(handle.read(&mut sequential_next, None).await.unwrap(), 23);
    assert_eq!(&sequential_next, &expected[17..40]);

    let mut exact = vec![0; expected.len()];
    assert_eq!(handle.read(&mut exact, Some(0)).await.unwrap(), exact.len());
    assert_eq!(exact, expected);
    let mut tail = [0; 17];
    assert_eq!(
        handle
            .read(&mut tail, Some(expected.len() as u64))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        handle
            .read(&mut tail, Some(expected.len() as u64 + 29))
            .await
            .unwrap(),
        0
    );
    let mut sequential_after_eof_probes = [0; 13];
    assert_eq!(
        handle
            .read(&mut sequential_after_eof_probes, None)
            .await
            .unwrap(),
        13
    );
    assert_eq!(&sequential_after_eof_probes, &expected[40..53]);
    handle.close().await.unwrap();
}

#[tokio::test]
async fn observed_sqlite_postconfiguration_failure_retains_actual_owner_until_journal_cleanup() {
    let root = ProtectedFixture::new();
    let initial = sqlite_plan(root.path(), 501, 20, false);
    let expected: Vec<u8> = (0..8193).map(|index| (index % 251) as u8).collect();
    let first = initial.open().await.unwrap();
    Loopback::from_arc(first.driver())
        .write_file("/acknowledged", &expected)
        .await
        .unwrap();
    assert_eq!(first.driver().stat("/").await.unwrap().uid, 501);
    first.shutdown().await.unwrap();
    drop(first);

    let mismatch = sqlite_plan(root.path(), 900, 901, true);
    let journal = ConstructionJournal::new();
    let observer = RecordingObserver::new(journal.clone());
    let attempt = journal.begin().unwrap();
    let error = match mismatch
        .open_with_construction_observer(None, None, &observer)
        .await
    {
        Ok(_) => panic!("read-only root mismatch must refuse postconfiguration"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("refusing to mutate"));
    let retained = observer.registrations();
    assert_eq!(retained.len(), 1, "SQLite registered its actual filesystem");
    assert!(retained[0].upgrade().is_some());
    assert_eq!(journal.snapshot().retained_resources, 1);
    attempt.fail();
    journal.close().await.unwrap();
    assert!(journal.snapshot().cleanup_complete);
    assert_eq!(journal.snapshot().retained_resources, 0);
    assert!(retained[0].upgrade().is_none());

    // A writable reopen could silently repair a root changed by the failed
    // attempt before the assertion. A matching read-only plan cannot do so.
    let matching_read_only = sqlite_plan(root.path(), 501, 20, true);
    let reopened = matching_read_only.open().await.unwrap();
    assert_payload(&reopened, &expected, "/acknowledged").await;
    let root_stat = reopened.driver().stat("/").await.unwrap();
    assert_eq!((root_stat.uid, root_stat.gid), (501, 20));
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn observed_compact_split_registers_sdk_group_authority_and_actual_filesystem_before_handoff()
{
    let root = ProtectedFixture::new();
    let plan = compact_plan(root.path());
    let expected: Vec<u8> = (0..8193).map(|index| (index % 247) as u8).collect();
    let journal = ConstructionJournal::new();
    let observer = RecordingObserver::new(journal.clone());
    let attempt = journal.begin().unwrap();
    let runtime = plan
        .open_with_construction_observer(None, None, &observer)
        .await
        .unwrap();
    let retained = observer.registrations();
    assert_eq!(
        retained.len(),
        3,
        "SDK group, authority, and actual filesystem"
    );
    assert_eq!(journal.snapshot().retained_resources, 3);
    let actual = Arc::as_ptr(&runtime.filesystem) as *const ();
    let registered = retained[2].upgrade().unwrap();
    assert_eq!(Arc::as_ptr(&registered) as *const (), actual);
    drop(registered);
    let backing = runtime.filesystem.concurrent_backing_id().unwrap();
    Loopback::from_arc(runtime.driver())
        .write_file("/acknowledged", &expected)
        .await
        .unwrap();
    attempt.handoff().unwrap();
    assert!(journal.snapshot().handed_off);
    assert_eq!(journal.snapshot().retained_resources, 0);
    assert!(
        retained[2].upgrade().is_some(),
        "returned runtime owns filesystem"
    );
    assert_payload(&runtime, &expected, "/acknowledged").await;
    assert_eq!(runtime.driver().stat("/").await.unwrap().uid, 111);
    runtime.shutdown().await.unwrap();
    drop(runtime);
    assert!(retained[2].upgrade().is_none());

    let reopened = plan.open().await.unwrap();
    assert_eq!(reopened.filesystem.concurrent_backing_id(), Some(backing));
    assert_payload(&reopened, &expected, "/acknowledged").await;
    let root_stat = reopened.driver().stat("/").await.unwrap();
    assert_eq!((root_stat.uid, root_stat.gid), (111, 222));
    reopened.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_sqlite_root_identity_is_rejected_before_observed_provider_open() {
    let root = ProtectedFixture::new();
    let options = CliOptions {
        driver: DriverChoice::Sqlite,
        database: Some(root.path().join("unopened.db")),
        root_uid: Some(501),
        root_gid: None,
        ..CliOptions::default()
    };
    let journal = ConstructionJournal::new();
    let observer = RecordingObserver::new(journal.clone());
    let attempt = journal.begin().unwrap();
    let error = match DriverRuntime::open_with_construction_observer(
        &options, 1000, 1001, None, None, &observer,
    )
    .await
    {
        Ok(_) => panic!("partial root identity must be rejected before opening SQLite"),
        Err(error) => error,
    };
    assert_eq!(error.exit_code(), 2);
    assert!(error.to_string().contains("requires both uid and gid"));
    assert!(observer.registrations().is_empty());
    assert_eq!(journal.snapshot().retained_resources, 0);
    attempt.fail();
    journal.close().await.unwrap();
    assert!(!root.path().join("unopened.db").exists());
}
