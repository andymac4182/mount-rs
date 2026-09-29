use super::super::{fixture::oracle_block, state::Expected, wire};
use super::{CrossnodeCoverage, Lane};
use async_trait::async_trait;
use mount_rs_core::{Capabilities, DirEntry, ErrorCode, FileHandle, FsDriver, FsError, Stats};
use mount_rs_remote_protocol::OperationName;
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

#[derive(Default)]
struct Evidence {
    opens: Mutex<Vec<String>>,
    reads: AtomicUsize,
    writes: AtomicUsize,
    closes: AtomicUsize,
    read_entered: tokio::sync::Notify,
    release_read: tokio::sync::Notify,
}

struct SentinelDriver {
    payload: [u8; 4096],
    count: usize,
    close_error: bool,
    block_read: bool,
    evidence: Arc<Evidence>,
}

struct SentinelHandle {
    payload: [u8; 4096],
    count: usize,
    close_error: bool,
    block_read: bool,
    evidence: Arc<Evidence>,
}

fn stat() -> Stats {
    Stats {
        dev: 0,
        ino: 2,
        mode: mount_rs_core::S_IFREG | 0o644,
        nlink: 1,
        uid: 0,
        gid: 0,
        rdev: 0,
        size: 4096,
        blksize: 4096,
        blocks: 8,
        atime_ms: 0,
        mtime_ms: 0,
        ctime_ms: 0,
        birthtime_ms: 0,
    }
}

#[async_trait]
impl FileHandle for SentinelHandle {
    async fn read(&self, buffer: &mut [u8], position: Option<u64>) -> mount_rs_core::Result<usize> {
        self.evidence.reads.fetch_add(1, Ordering::SeqCst);
        assert_eq!(position, Some(0));
        assert_eq!(buffer.len(), 4096);
        if self.block_read {
            self.evidence.read_entered.notify_one();
            self.evidence.release_read.notified().await;
        }
        buffer[..self.count].copy_from_slice(&self.payload[..self.count]);
        Ok(self.count)
    }
    async fn write(&self, _: &[u8], _: Option<u64>) -> mount_rs_core::Result<usize> {
        self.evidence.writes.fetch_add(1, Ordering::SeqCst);
        Err(FsError::new(ErrorCode::Eacces))
    }
    async fn stat(&self) -> mount_rs_core::Result<Stats> {
        Ok(stat())
    }
    async fn truncate(&self, _: u64) -> mount_rs_core::Result<()> {
        self.evidence.writes.fetch_add(1, Ordering::SeqCst);
        Err(FsError::new(ErrorCode::Eacces))
    }
    async fn close(&self) -> mount_rs_core::Result<()> {
        self.evidence.closes.fetch_add(1, Ordering::SeqCst);
        if self.close_error {
            Err(FsError::new(ErrorCode::Eio))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl FsDriver for SentinelDriver {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            handles: true,
            read_only: true,
            ..Capabilities::default()
        }
    }
    async fn stat(&self, path: &str) -> mount_rs_core::Result<Stats> {
        assert_eq!(path, "/mixed-0");
        Ok(stat())
    }
    async fn readdir(&self, _: &str) -> mount_rs_core::Result<Vec<DirEntry>> {
        Err(FsError::new(ErrorCode::Enotsup))
    }
    async fn open(
        &self,
        path: &str,
        flags: &str,
        _: u32,
    ) -> mount_rs_core::Result<Arc<dyn FileHandle>> {
        assert_eq!(path, "/mixed-0");
        self.evidence.opens.lock().unwrap().push(flags.into());
        if flags != "r" {
            return Err(FsError::new(ErrorCode::Eacces));
        }
        Ok(Arc::new(SentinelHandle {
            payload: self.payload,
            count: self.count,
            close_error: self.close_error,
            block_read: self.block_read,
            evidence: self.evidence.clone(),
        }))
    }
}

fn expected() -> Expected {
    // The acknowledged model is prepared independently from the driver's
    // returned bytes, including a noninitial generation to catch stale data.
    let mut expected = Expected::empty(2, 1);
    expected.create("mixed-0".into(), 0);
    expected.write("mixed-0", 0, 17);
    expected
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crossnode_sentinel_actual_quic_requires_current_bytes_and_acknowledged_close() {
    let good = oracle_block(1, 2, 0, 0, 17);
    let mut corrupt = good;
    corrupt[4095] ^= 1;
    for (label, payload, count, close_error, qualifies) in [
        ("current", good, 4096, false, true),
        ("corrupt", corrupt, 4096, false, false),
        ("stale", oracle_block(1, 2, 0, 0, 16), 4096, false, false),
        (
            "wrong Drive",
            oracle_block(1, 3, 0, 0, 17),
            4096,
            false,
            false,
        ),
        ("short", good, 4095, false, false),
        ("empty", good, 0, false, false),
        ("close failure", good, 4096, true, false),
    ] {
        let evidence = Arc::new(Evidence::default());
        let driver = Arc::new(SentinelDriver {
            payload,
            count,
            close_error,
            block_read: false,
            evidence: evidence.clone(),
        }) as Arc<dyn FsDriver>;
        let (server, endpoint, _directory) =
            wire::setup_with_drives(vec![driver; 3]).await.unwrap();
        let connection = wire::connect_sandbox(&endpoint, server.local_addr(), 2)
            .await
            .unwrap();
        let mut lane = Lane::new(connection, 2, 1);
        lane.expected = expected();
        let before = lane.expected.snapshot();
        let mut coverage = CrossnodeCoverage::new(3).unwrap();
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            lane.request(OperationName::Stat, json!({"path":"/mixed-0"}))
                .await?;
            lane.crossnode_sentinel().await
        })
        .await
        .expect("actual QUIC sentinel control deadline");
        if let Ok(bytes) = result.as_ref() {
            assert_eq!(*bytes, 4096);
            coverage.record(2, 0).unwrap();
        }
        let counters = lane.counts.lock().unwrap().clone();
        let completed = coverage.completed_pairs();
        lane.connection
            .close(0u32.into(), b"sentinel control complete");
        server.close().await;
        endpoint.wait_idle().await;

        assert_eq!(result.is_ok(), qualifies, "{label}: {result:?}");
        assert_eq!(completed, usize::from(qualifies), "{label}");
        assert_eq!(
            completed * 4096,
            if qualifies { 4096 } else { 0 },
            "{label}"
        );
        assert_eq!(
            lane.expected.snapshot(),
            before,
            "{label}: changed expectation"
        );
        assert_eq!(*evidence.opens.lock().unwrap(), ["r"], "{label}");
        assert_eq!(
            evidence.reads.load(Ordering::SeqCst),
            1,
            "{label}: replayed read"
        );
        assert_eq!(
            evidence.writes.load(Ordering::SeqCst),
            0,
            "{label}: wrote payload"
        );
        assert!(counters.pending.is_none(), "{label}");
        assert_eq!(
            counters.attempts,
            counters.acknowledged + counters.failed + counters.uncertain,
            "{label}"
        );
        if count == 4096 {
            assert!(
                evidence.closes.load(Ordering::SeqCst) >= 1,
                "{label}: close omitted"
            );
            assert_eq!(counters.attempts, 4, "{label}");
            assert_eq!(counters.uncertain, 0, "{label}");
            assert_eq!(counters.failed, u64::from(close_error), "{label}");
        } else {
            assert_eq!(counters.attempts, 3, "{label}: unknown read was replayed");
            assert_eq!(counters.uncertain, 1, "{label}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn crossnode_sentinel_cancellation_retains_pending_request_without_replay() {
    let evidence = Arc::new(Evidence::default());
    let driver = Arc::new(SentinelDriver {
        payload: oracle_block(1, 2, 0, 0, 17),
        count: 4096,
        close_error: false,
        block_read: true,
        evidence: evidence.clone(),
    }) as Arc<dyn FsDriver>;
    let (server, endpoint, _directory) = wire::setup_with_drives(vec![driver; 3]).await.unwrap();
    let connection = wire::connect_sandbox(&endpoint, server.local_addr(), 2)
        .await
        .unwrap();
    let mut lane = Lane::new(connection, 2, 1);
    lane.expected = expected();
    let before = lane.expected.snapshot();
    let mut operation = Box::pin(lane.crossnode_sentinel());
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            _ = evidence.read_entered.notified() => {}
            result = &mut operation => panic!("blocked read unexpectedly completed: {result:?}"),
        }
    })
    .await
    .expect("actual QUIC cancellation control deadline");
    drop(operation);
    let pending = lane.counts.lock().unwrap().pending;
    // Match controller teardown: cancellation cannot manufacture an ACK.
    lane.counts.lock().unwrap().uncertain();
    lane.connection
        .close(1u32.into(), b"cancelled sentinel; never replayed");
    evidence.release_read.notify_one();
    server.close().await;
    endpoint.wait_idle().await;
    let counters = lane.counts.lock().unwrap();
    assert!(pending.is_some());
    assert_eq!(counters.attempts, 2);
    assert_eq!(counters.acknowledged, 1);
    assert_eq!(counters.uncertain, 1);
    assert!(counters.pending.is_none());
    assert_eq!(evidence.reads.load(Ordering::SeqCst), 1);
    assert_eq!(evidence.writes.load(Ordering::SeqCst), 0);
    assert_eq!(lane.expected.snapshot(), before);
}

#[test]
fn crossnode_payload_coverage_rejects_missing_duplicate_and_out_of_range_pairs() {
    let mut coverage = CrossnodeCoverage::new(2).unwrap();
    assert!(coverage.verify_complete().is_err());
    for drive in 0..2 {
        for server in 0..10 {
            if (drive, server) != (1, 9) {
                coverage.record(drive, server).unwrap();
            }
        }
    }
    assert_eq!(coverage.completed_pairs(), 19);
    assert_eq!(coverage.expected_pairs(), 20);
    assert!(coverage.verify_complete().is_err());
    assert!(coverage.record(0, 0).is_err());
    assert_eq!(coverage.completed_pairs(), 19);
    assert!(coverage.record(2, 0).is_err());
    assert!(coverage.record(0, 10).is_err());
    coverage.record(1, 9).unwrap();
    assert_eq!(coverage.completed_pairs(), 20);
    coverage.verify_complete().unwrap();
}
