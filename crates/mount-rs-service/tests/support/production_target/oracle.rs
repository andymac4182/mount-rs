use super::{backend::Backend, state::Expected};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const FRESH_ORACLE_SLOTS: usize = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pass {
    Initial,
    Final,
}
impl Pass {
    fn label(self) -> &'static str {
        match self {
            Self::Initial => "initial",
            Self::Final => "final",
        }
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Verified {
    pub files: u64,
    pub bytes: u64,
}
struct ProgressState {
    pass: Option<Pass>,
    slot_limit: usize,
    expected_drives: u64,
    started_drives: u64,
    completed_drives: u64,
    live_slots: u64,
    expected: Verified,
    completed: Verified,
    checked_files: u64,
    compared_bytes: u64,
    complete: bool,
    settled: bool,
    expected_drive_ids: BTreeSet<usize>,
    completed_drive_ids: BTreeSet<usize>,
}
#[derive(Clone)]
pub struct ProgressView {
    state: Arc<Mutex<ProgressState>>,
}
impl ProgressView {
    pub fn snapshot(&self) -> Value {
        let state = self.state.lock().unwrap();
        json!({"pass":state.pass.map(Pass::label),"slot_limit":state.slot_limit,
            "expected_drives":state.expected_drives,"started_drives":state.started_drives,
            "completed_drives":state.completed_drives,"live_slots":state.live_slots,
            "expected_files":state.expected.files,"completed_files":state.completed.files,
            "expected_bytes":state.expected.bytes,"completed_bytes":state.completed.bytes,
            "checked_files":state.checked_files,"compared_bytes":state.compared_bytes,
            "complete":state.complete,"settled":state.settled,
            "completed_drive_ids":state.completed_drive_ids.iter().copied().collect::<Vec<_>>()})
    }
    fn begin(&self, pass: Pass, expected: &[&Expected]) -> Result<(), String> {
        let totals = validate_expected(expected)?;
        let mut state = self.state.lock().unwrap();
        if !state.settled
            || state.live_slots != 0
            || !matches!(
                (state.pass, pass, state.complete),
                (None, Pass::Initial, _) | (Some(Pass::Initial), Pass::Final, true)
            )
        {
            return Err("fresh oracle pass order or settlement invalid".into());
        }
        let slot_limit = state.slot_limit;
        *state = ProgressState {
            pass: Some(pass),
            slot_limit,
            expected_drives: expected.len() as u64,
            started_drives: 0,
            completed_drives: 0,
            live_slots: 0,
            expected: totals,
            completed: Verified::default(),
            checked_files: 0,
            compared_bytes: 0,
            complete: false,
            settled: true,
            expected_drive_ids: expected.iter().map(|drive| drive.drive).collect(),
            completed_drive_ids: BTreeSet::new(),
        };
        Ok(())
    }
    fn compared(&self, bytes: u64) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        state.compared_bytes = state
            .compared_bytes
            .checked_add(bytes)
            .filter(|n| *n <= state.expected.bytes)
            .ok_or("fresh oracle compared bytes overflow or excess")?;
        Ok(())
    }
    fn checked_file(&self) -> Result<(), String> {
        let mut state = self.state.lock().unwrap();
        state.checked_files = state
            .checked_files
            .checked_add(1)
            .filter(|n| *n <= state.expected.files)
            .ok_or("fresh oracle checked files overflow or excess")?;
        Ok(())
    }
}
fn expected_totals(expected: &Expected) -> Result<Verified, String> {
    let mut total = Verified {
        files: u64::try_from(expected.files.len())
            .map_err(|_| "fresh oracle file count overflow")?,
        bytes: 0,
    };
    for file in expected.files.values() {
        if !file.length.is_multiple_of(4096) {
            return Err("fresh oracle expected length is not block aligned".into());
        }
        total.bytes = total
            .bytes
            .checked_add(u64::try_from(file.length).map_err(|_| "fresh oracle length overflow")?)
            .ok_or("fresh oracle expected bytes overflow")?;
    }
    Ok(total)
}
fn validate_expected(expected: &[&Expected]) -> Result<Verified, String> {
    if expected.is_empty() || expected.len() > 10000 {
        return Err("fresh oracle Drive count out of bounds".into());
    }
    let mut drives = BTreeSet::new();
    let mut total = Verified::default();
    for drive in expected {
        if !drives.insert(drive.drive) {
            return Err("fresh oracle duplicate expected Drive".into());
        }
        let next = expected_totals(drive)?;
        total.files = total
            .files
            .checked_add(next.files)
            .ok_or("fresh oracle expected files overflow")?;
        total.bytes = total
            .bytes
            .checked_add(next.bytes)
            .ok_or("fresh oracle expected bytes overflow")?;
    }
    Ok(total)
}
fn credit_completion(
    progress: &ProgressView,
    expected: &Expected,
    verified: Verified,
    now: Instant,
    deadline: Instant,
) -> Result<(), String> {
    if now >= deadline {
        return Err("fresh oracle Drive completed after inherited deadline".into());
    }
    if verified != expected_totals(expected)? {
        return Err("fresh oracle completed totals mismatch".into());
    }
    let mut state = progress.state.lock().unwrap();
    if Instant::now() >= deadline {
        return Err("fresh oracle Drive completed after inherited deadline".into());
    }
    if !state.expected_drive_ids.contains(&expected.drive)
        || state.completed_drive_ids.contains(&expected.drive)
    {
        return Err("fresh oracle foreign or duplicate completed Drive".into());
    }
    let drives = state
        .completed_drives
        .checked_add(1)
        .filter(|n| *n <= state.started_drives && *n <= state.expected_drives)
        .ok_or("fresh oracle completed Drive overflow or excess")?;
    let files = state
        .completed
        .files
        .checked_add(verified.files)
        .filter(|n| *n <= state.checked_files && *n <= state.expected.files)
        .ok_or("fresh oracle completed files overflow or excess")?;
    let bytes = state
        .completed
        .bytes
        .checked_add(verified.bytes)
        .filter(|n| *n <= state.compared_bytes && *n <= state.expected.bytes)
        .ok_or("fresh oracle completed bytes overflow or excess")?;
    state.completed_drive_ids.insert(expected.drive);
    state.completed_drives = drives;
    state.completed = Verified { files, bytes };
    Ok(())
}
struct LiveSlot {
    progress: ProgressView,
}
impl LiveSlot {
    fn enter(progress: &ProgressView) -> Result<Self, String> {
        let mut state = progress.state.lock().unwrap();
        let started = state
            .started_drives
            .checked_add(1)
            .filter(|n| *n <= state.expected_drives)
            .ok_or("fresh oracle started Drive overflow or excess")?;
        let live = state
            .live_slots
            .checked_add(1)
            .filter(|n| *n <= state.slot_limit as u64)
            .ok_or("fresh oracle slot bound exceeded")?;
        state.started_drives = started;
        state.live_slots = live;
        state.settled = false;
        Ok(Self {
            progress: progress.clone(),
        })
    }
}
impl Drop for LiveSlot {
    fn drop(&mut self) {
        let mut state = self.progress.state.lock().unwrap();
        state.live_slots = state
            .live_slots
            .checked_sub(1)
            .expect("retained oracle active slot guard");
    }
}
#[async_trait]
trait DriveOperation: Send + Sync {
    async fn run(
        &self,
        owner: &mut Owner,
        expected: &Expected,
        deadline: Instant,
        progress: &ProgressView,
    ) -> Result<Verified, String>;
}
pub struct Pool {
    pub accounting: super::metrics::Accounting,
    slots: Vec<Owner>,
    progress: ProgressView,
    cleanup_deadline: Option<Instant>,
}
impl Pool {
    pub fn new(slots: usize) -> Result<Self, String> {
        if !(1..=16).contains(&slots) {
            return Err("fresh oracle slot limit must be between one and sixteen".into());
        }
        let accounting =
            super::metrics::Accounting::new(mount_rs_core::diagnostics::profile::enabled());
        let owners = (0..slots)
            .map(|_| Owner {
                accounting: accounting.clone(),
                context: None,
                filesystem: None,
                handle: None,
            })
            .collect();
        Ok(Self {
            accounting,
            slots: owners,
            cleanup_deadline: None,
            progress: ProgressView {
                state: Arc::new(Mutex::new(ProgressState {
                    pass: None,
                    slot_limit: slots,
                    expected_drives: 0,
                    started_drives: 0,
                    completed_drives: 0,
                    live_slots: 0,
                    expected: Verified::default(),
                    completed: Verified::default(),
                    checked_files: 0,
                    compared_bytes: 0,
                    complete: false,
                    settled: true,
                    expected_drive_ids: BTreeSet::new(),
                    completed_drive_ids: BTreeSet::new(),
                })),
            },
        })
    }
    pub fn progress(&self) -> ProgressView {
        self.progress.clone()
    }
    pub fn settled(&self) -> bool {
        self.slots.iter().all(Owner::settled) && self.progress.state.lock().unwrap().live_slots == 0
    }
    pub async fn verify_pass(
        &mut self,
        pass: Pass,
        backend: &Backend,
        expected: &[&Expected],
        deadline: Instant,
    ) -> Result<Verified, String> {
        self.verify_with(pass, backend, expected, deadline).await
    }
    async fn verify_with(
        &mut self,
        pass: Pass,
        operation: &impl DriveOperation,
        expected: &[&Expected],
        deadline: Instant,
    ) -> Result<Verified, String> {
        if !self.settled() || self.cleanup_deadline.is_some() {
            return Err("fresh oracle retained owner is not settled or cleanup has begun".into());
        }
        if Instant::now() >= deadline {
            return Err("fresh oracle inherited pass deadline".into());
        }
        self.progress.begin(pass, expected)?;
        let cursor = Mutex::new(0usize);
        let progress = &self.progress;
        // The futures borrow retained Owners. Dropping this pass cancels work,
        // while every constructed context/filesystem/handle remains in the Pool.
        let jobs = self.slots.iter_mut().map(|owner| {
            let cursor = &cursor;
            async move {
                loop {
                    if !owner.settled() {
                        return Err("fresh oracle slot still owns resources".to_string());
                    }
                    if Instant::now() >= deadline {
                        return Err("fresh oracle inherited pass deadline".to_string());
                    }
                    let next = {
                        let mut cursor = cursor.lock().unwrap();
                        if *cursor == expected.len() {
                            None
                        } else {
                            let next = expected[*cursor];
                            *cursor += 1;
                            Some(next)
                        }
                    };
                    let Some(expected) = next else {
                        return Ok(());
                    };
                    let active = LiveSlot::enter(progress)?;
                    let verified = operation.run(owner, expected, deadline, progress).await?;
                    let close_deadline = deadline.min(Instant::now() + Duration::from_secs(30));
                    owner.close_until(close_deadline).await?;
                    if !owner.settled() {
                        return Err("fresh oracle Drive closure unproven".to_string());
                    }
                    drop(active);
                    credit_completion(progress, expected, verified, Instant::now(), deadline)?;
                }
            }
        });
        let result =
            tokio::time::timeout_at(deadline.into(), futures_util::future::try_join_all(jobs))
                .await
                .map_err(|_| "fresh oracle inherited pass deadline".to_string())
                .and_then(|result| result);
        let settled = self.settled();
        let mut state = self.progress.state.lock().unwrap();
        state.settled = settled;
        state.complete = result.is_ok()
            && Instant::now() < deadline
            && settled
            && state.started_drives == state.expected_drives
            && state.completed_drives == state.expected_drives
            && state.completed == state.expected
            && state.checked_files == state.expected.files
            && state.compared_bytes == state.expected.bytes
            && state.completed_drive_ids == state.expected_drive_ids;
        result?;
        if !state.complete {
            return Err("fresh oracle complete corpus proof missing".into());
        }
        Ok(state.completed)
    }
    pub async fn close_once(&mut self) -> Result<(), String> {
        let deadline = *self
            .cleanup_deadline
            .get_or_insert_with(|| Instant::now() + Duration::from_secs(30));
        // An error in one close must not cancel any other owned slot's attempt.
        let results = futures_util::future::join_all(
            self.slots
                .iter_mut()
                .map(|owner| owner.close_until(deadline)),
        )
        .await;
        let settled = self.settled();
        let failed = !settled || results.iter().any(Result::is_err);
        let mut state = self.progress.state.lock().unwrap();
        state.settled = settled;
        state.complete &= !failed;
        if failed {
            return Err("fresh oracle pool cleanup unproven".into());
        }
        Ok(())
    }
}
pub struct Owner {
    pub accounting: super::metrics::Accounting,
    context: Option<mount_rs_sdk::StorageContext>,
    filesystem: Option<mount_rs_sdk::Filesystem>,
    handle: Option<std::sync::Arc<dyn mount_rs_core::FileHandle>>,
}
impl Default for Owner {
    fn default() -> Self {
        Self {
            context: None,
            filesystem: None,
            handle: None,
            accounting: super::metrics::Accounting::new(
                mount_rs_core::diagnostics::profile::enabled(),
            ),
        }
    }
}
async fn observed<T, E>(
    accounting: &super::metrics::Accounting,
    category: &'static str,
    future: impl std::future::Future<Output = Result<T, E>>,
) -> Result<T, E> {
    let span = accounting.begin(category);
    let result = future.await;
    span.finish(result.is_ok(), 0);
    result
}
impl Owner {
    pub(super) fn settled(&self) -> bool {
        self.context.is_none() && self.filesystem.is_none() && self.handle.is_none()
    }
    /// Establish only empty persistent root/backing state; no namespace/payload preseed.
    pub async fn initialize_empty(
        &mut self,
        backend: &Backend,
        drive: usize,
    ) -> Result<serde_json::Value, String> {
        if self.context.is_none() {
            let span = self.accounting.begin("context_open");
            let context = mount_rs_sdk::StorageContext::new(16);
            span.finish(context.is_ok(), 0);
            self.context = Some(context.map_err(|_| "empty initializer context failed")?);
        }
        self.filesystem = Some(
            observed(
                &self.accounting,
                "filesystem_open",
                backend.open(drive, self.context.as_ref().unwrap()),
            )
            .await?,
        );
        let fs = self.filesystem.as_ref().unwrap();
        let entries = observed(&self.accounting, "membership", fs.driver().readdir("/"))
            .await
            .map_err(|_| "empty initializer membership failed")?;
        if entries.iter().any(|e| e.name != "." && e.name != "..") {
            return Err("empty initializer found unexpected namespace".into());
        }
        let mut receipt = observed(
            &self.accounting,
            "backing_receipt",
            backend.receipt(drive, self.context.as_ref().unwrap()),
        )
        .await?;
        super::process::validate_backing_receipt(&receipt, drive)?;
        observed(&self.accounting, "filesystem_context_close", fs.shutdown())
            .await
            .map_err(|_| "empty initializer filesystem close failed")?;
        self.filesystem = None;
        receipt["empty_root_verified"] = serde_json::json!(true);
        receipt["namespace_entries"] = serde_json::json!(0);
        receipt["filesystem_closed"] = serde_json::json!(true);
        Ok(receipt)
    }

    pub async fn close(&mut self) -> Result<(), String> {
        self.close_until(Instant::now() + Duration::from_secs(30))
            .await
    }
    async fn close_until(&mut self, deadline: Instant) -> Result<(), String> {
        let close_span = self.accounting.begin("filesystem_context_close");
        if Instant::now() >= deadline {
            close_span.finish(false, 0);
            return Err("fresh oracle cleanup unproven".into());
        }
        let closed = tokio::time::timeout_at(deadline.into(), async {
            let mut failed = false;
            if let Some(handle) = &self.handle {
                failed |= handle.close().await.is_err();
            }
            if let Some(fs) = &self.filesystem {
                failed |= fs.shutdown().await.is_err();
            }
            if let Some(context) = &self.context {
                failed |= context.close().await.is_err();
            }
            if failed {
                Err("fresh oracle close failed".to_string())
            } else {
                Ok(())
            }
        })
        .await
        .map_err(|_| "fresh oracle cleanup unproven".to_string())
        .and_then(|r| r)
        .and_then(|()| {
            if Instant::now() < deadline {
                Ok(())
            } else {
                Err("fresh oracle cleanup unproven".into())
            }
        });
        close_span.finish(closed.is_ok(), 0);
        if closed.is_ok() {
            self.handle = None;
            self.filesystem = None;
            self.context = None;
        }
        closed
    }
}
async fn verify_contents(
    owner: &mut Owner,
    driver: Arc<dyn mount_rs_core::FsDriver>,
    expected: &Expected,
    progress: Option<&ProgressView>,
) -> Result<Verified, String> {
    // Check every expected length before a membership read or handle open.
    let expected_total = expected_totals(expected)?;
    if owner.handle.is_some() {
        return Err("fresh oracle retained handle prevents open".into());
    }
    let metrics = owner.accounting.clone();
    observed(&metrics, "membership", async {
        let actual: BTreeSet<_> = driver
            .readdir("/")
            .await
            .map_err(|_| "fresh membership read failed")?
            .into_iter()
            .filter(|e| e.name != "." && e.name != "..")
            .map(|e| e.name)
            .collect();
        let names = expected.files.keys().cloned().collect();
        if actual != names {
            Err("fresh namespace membership mismatch")
        } else {
            Ok(())
        }
    })
    .await?;
    let mut verified_bytes = 0u64;
    let mut verified_files = 0u64;
    for (name, file) in &expected.files {
        owner.handle = Some(
            observed(
                &metrics,
                "file_open",
                driver.open(&format!("/{name}"), "r", 0),
            )
            .await
            .map_err(|_| "fresh oracle open failed")?,
        );
        let handle = owner.handle.as_ref().unwrap();
        let checked: Result<(), String> = async {
            observed(&metrics, "stat", async {
                if handle
                    .stat()
                    .await
                    .map_err(|_| "fresh oracle stat failed")?
                    .size
                    != file.length as u64
                {
                    Err("fresh length mismatch")
                } else {
                    Ok(())
                }
            })
            .await?;
            let mut data = [0; 4096];
            for block in 0..file.length / 4096 {
                let span = metrics.begin("data_read");
                let read = handle.read(&mut data, Some((block * 4096) as u64)).await;
                span.finish(read.is_ok(), read.as_ref().copied().unwrap_or(0) as u64);
                if read.map_err(|_| "fresh read failed")? != 4096 {
                    return Err("fresh every-byte oracle mismatch".into());
                }
                let span = metrics.begin("expected_compare");
                let matches = data == expected.bytes(name, block);
                span.finish(matches, 4096);
                if !matches {
                    return Err("fresh every-byte oracle mismatch".into());
                }
                verified_bytes = verified_bytes
                    .checked_add(4096)
                    .ok_or("fresh oracle verified bytes overflow")?;
                if let Some(progress) = progress {
                    progress.compared(4096)?;
                }
            }
            observed(&metrics, "eof", async {
                if handle
                    .read(&mut data, Some(file.length as u64))
                    .await
                    .map_err(|_| "fresh EOF read failed")?
                    != 0
                {
                    Err("fresh EOF mismatch")
                } else {
                    Ok(())
                }
            })
            .await?;
            Ok(())
        }
        .await;
        let close = observed(&metrics, "handle_close", handle.close())
            .await
            .map_err(|_| "fresh handle close failed");
        checked?;
        close?;
        owner.handle = None;
        verified_files = verified_files
            .checked_add(1)
            .ok_or("fresh oracle verified files overflow")?;
        if let Some(progress) = progress {
            progress.checked_file()?;
        }
    }
    let verified = Verified {
        files: verified_files,
        bytes: verified_bytes,
    };
    if verified != expected_total {
        return Err("fresh oracle full content totals mismatch".into());
    }
    Ok(verified)
}
#[async_trait]
impl DriveOperation for Backend {
    async fn run(
        &self,
        owner: &mut Owner,
        expected: &Expected,
        deadline: Instant,
        progress: &ProgressView,
    ) -> Result<Verified, String> {
        if Instant::now() >= deadline || !owner.settled() {
            return Err("fresh oracle deadline or retained owner prevents open".into());
        }
        let span = owner.accounting.begin("context_open");
        let context = mount_rs_sdk::StorageContext::new(16);
        span.finish(context.is_ok(), 0);
        owner.context = Some(context.map_err(|_| "oracle context failed")?);
        owner.filesystem = Some(
            observed(
                &owner.accounting,
                "filesystem_open",
                self.open(expected.drive, owner.context.as_ref().unwrap()),
            )
            .await?,
        );
        let driver = owner.filesystem.as_ref().unwrap().driver();
        observed(
            &owner.accounting,
            "backing_receipt",
            self.receipt(expected.drive, owner.context.as_ref().unwrap()),
        )
        .await?;
        verify_contents(owner, driver, expected, Some(progress)).await
    }
}

#[cfg(test)]
mod fresh_drive_pool_tests {
    use super::*;
    use async_trait::async_trait;
    use mount_rs_core::{
        Capabilities, DirEntry, ErrorCode, FileHandle, FileType, FsDriver, FsError, Stats,
    };
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        task::Poll,
        time::Instant,
    };

    #[derive(Clone, Copy)]
    enum CloseMode {
        Success,
        Error,
        Pending,
    }
    #[derive(Default)]
    struct Evidence {
        starts: Mutex<Vec<usize>>,
        deadlines: Mutex<Vec<Instant>>,
        opens: Mutex<Vec<String>>,
        closes: Mutex<Vec<usize>>,
        mutations: AtomicUsize,
    }
    struct ProbeHandle {
        data: Vec<u8>,
        size: u64,
        count: usize,
        eof_count: usize,
        close: CloseMode,
        drive: usize,
        evidence: Arc<Evidence>,
    }
    fn stats(size: u64) -> Stats {
        Stats {
            dev: 0,
            ino: 2,
            mode: mount_rs_core::S_IFREG | 0o644,
            nlink: 1,
            uid: 0,
            gid: 0,
            rdev: 0,
            size,
            blksize: 4096,
            blocks: size / 512,
            atime_ms: 0,
            mtime_ms: 0,
            ctime_ms: 0,
            birthtime_ms: 0,
        }
    }
    #[async_trait]
    impl FileHandle for ProbeHandle {
        async fn read(
            &self,
            buffer: &mut [u8],
            position: Option<u64>,
        ) -> mount_rs_core::Result<usize> {
            let position = position.ok_or_else(|| FsError::new(ErrorCode::Einval))?;
            if position == self.size {
                return Ok(self.eof_count);
            }
            let start = position as usize;
            buffer[..self.count].copy_from_slice(&self.data[start..start + self.count]);
            Ok(self.count)
        }
        async fn write(&self, _: &[u8], _: Option<u64>) -> mount_rs_core::Result<usize> {
            self.evidence.mutations.fetch_add(1, Ordering::SeqCst);
            Err(FsError::new(ErrorCode::Eacces))
        }
        async fn stat(&self) -> mount_rs_core::Result<Stats> {
            Ok(stats(self.size))
        }
        async fn truncate(&self, _: u64) -> mount_rs_core::Result<()> {
            self.evidence.mutations.fetch_add(1, Ordering::SeqCst);
            Err(FsError::new(ErrorCode::Eacces))
        }
        async fn close(&self) -> mount_rs_core::Result<()> {
            self.evidence.closes.lock().unwrap().push(self.drive);
            match self.close {
                CloseMode::Success => Ok(()),
                CloseMode::Error => Err(FsError::new(ErrorCode::Eio)),
                CloseMode::Pending => std::future::pending().await,
            }
        }
    }
    struct ProbeDriver {
        names: Vec<String>,
        handle: Arc<ProbeHandle>,
    }
    #[async_trait]
    impl FsDriver for ProbeDriver {
        fn capabilities(&self) -> Capabilities {
            Capabilities {
                handles: true,
                read_only: true,
                ..Capabilities::default()
            }
        }
        async fn stat(&self, _: &str) -> mount_rs_core::Result<Stats> {
            Ok(stats(self.handle.size))
        }
        async fn readdir(&self, path: &str) -> mount_rs_core::Result<Vec<DirEntry>> {
            assert_eq!(path, "/");
            Ok(self
                .names
                .iter()
                .map(|name| DirEntry {
                    name: name.clone(),
                    parent_path: "/".into(),
                    file_type: FileType::File,
                })
                .collect())
        }
        async fn open(
            &self,
            path: &str,
            flags: &str,
            _: u32,
        ) -> mount_rs_core::Result<Arc<dyn FileHandle>> {
            assert_eq!(path, "/mixed-0");
            self.handle
                .evidence
                .opens
                .lock()
                .unwrap()
                .push(flags.into());
            if flags != "r" {
                return Err(FsError::new(ErrorCode::Eacces));
            }
            Ok(self.handle.clone())
        }
    }
    fn expected(drive: usize, blocks: usize) -> Expected {
        let mut expected = Expected::empty(drive, 1);
        expected.create("mixed-0".into(), 0);
        if blocks > 0 {
            expected.write("mixed-0", 0, 17);
        }
        if blocks > 1 {
            expected.write("mixed-0", 1, 23);
        }
        expected
    }
    fn probe(drive: usize, close: CloseMode, evidence: &Arc<Evidence>) -> Arc<ProbeHandle> {
        Arc::new(ProbeHandle {
            data: super::super::fixture::oracle_block((drive / 2) as u64, drive as u64, 0, 0, 17)
                .to_vec(),
            size: 4096,
            count: 4096,
            eof_count: 0,
            close,
            drive,
            evidence: evidence.clone(),
        })
    }
    fn instrumented_pool(slots: usize) -> Pool {
        let mut pool = Pool::new(slots).unwrap();
        let accounting = super::super::metrics::Accounting::new(true);
        pool.accounting = accounting.clone();
        for owner in &mut pool.slots {
            owner.accounting = accounting.clone();
        }
        pool
    }
    async fn poll_pending<F: Future>(mut future: Pin<&mut F>) {
        std::future::poll_fn(|cx| {
            assert!(
                future.as_mut().poll(cx).is_pending(),
                "controlled operation must remain pending"
            );
            Poll::Ready(())
        })
        .await;
    }
    struct GatedDrives {
        gates: Vec<tokio::sync::Notify>,
        evidence: Arc<Evidence>,
        fail: Option<usize>,
    }
    impl GatedDrives {
        fn new(drives: usize) -> Self {
            Self {
                gates: (0..drives).map(|_| tokio::sync::Notify::new()).collect(),
                evidence: Arc::default(),
                fail: None,
            }
        }
        fn release(&self, drive: usize) {
            self.gates[drive].notify_one();
        }
    }
    #[async_trait]
    impl DriveOperation for GatedDrives {
        async fn run(
            &self,
            owner: &mut Owner,
            expected: &Expected,
            deadline: Instant,
            progress: &ProgressView,
        ) -> Result<Verified, String> {
            assert!(
                owner.settled(),
                "new Drive must not overwrite retained resources"
            );
            owner.context = Some(mount_rs_sdk::StorageContext::new(16).unwrap());
            owner.handle = Some(probe(expected.drive, CloseMode::Success, &self.evidence));
            self.evidence.starts.lock().unwrap().push(expected.drive);
            self.evidence.deadlines.lock().unwrap().push(deadline);
            let span = owner.accounting.begin("data_read");
            self.gates[expected.drive].notified().await;
            if self.fail == Some(expected.drive) {
                span.finish(false, 0);
                return Err("injected read failure".into());
            }
            span.finish(true, 4096);
            progress.compared(4096)?;
            observed(
                &owner.accounting,
                "handle_close",
                owner.handle.as_ref().unwrap().close(),
            )
            .await
            .map_err(|_| "probe close failed")?;
            owner.handle = None;
            progress.checked_file()?;
            Ok(Verified {
                files: 1,
                bytes: 4096,
            })
        }
    }

    #[tokio::test]
    async fn fresh_drive_pool_bounds_live_slots_and_refills_exactly_once() {
        let expected: Vec<_> = (0..10).map(|drive| expected(drive, 1)).collect();
        let refs: Vec<_> = expected.iter().collect();
        let operation = GatedDrives::new(10);
        let mut pool = Pool::new(3).unwrap();
        let progress = pool.progress();
        let deadline = Instant::now() + Duration::from_secs(600);
        let mut pass = Box::pin(pool.verify_with(Pass::Initial, &operation, &refs, deadline));
        poll_pending(pass.as_mut()).await;
        assert_eq!(
            *operation.evidence.starts.lock().unwrap(),
            vec![0, 1, 2],
            "fill all three bounded slots before waiting"
        );
        assert_eq!(progress.snapshot()["live_slots"], 3);
        operation.release(1);
        poll_pending(pass.as_mut()).await;
        assert_eq!(
            *operation.evidence.starts.lock().unwrap(),
            vec![0, 1, 2, 3],
            "one completed slot admits exactly one replacement"
        );
        assert_eq!(progress.snapshot()["completed_drives"], 1);
        assert_eq!(progress.snapshot()["live_slots"], 3);
        for drive in 0..10 {
            if drive != 1 {
                operation.release(drive);
            }
        }
        assert_eq!(
            pass.await.unwrap(),
            Verified {
                files: 10,
                bytes: 40960
            }
        );
        assert!(pool.settled());
        let snapshot = progress.snapshot();
        assert_eq!(snapshot["complete"], true);
        assert_eq!(
            snapshot["completed_drive_ids"],
            serde_json::json!((0..10).collect::<Vec<_>>())
        );
        let mut closed = operation.evidence.closes.lock().unwrap().clone();
        closed.sort_unstable();
        assert_eq!(closed, (0..10).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn fresh_drive_pool_cancellation_retains_actual_owned_resources() {
        let expected: Vec<_> = (0..5).map(|drive| expected(drive, 1)).collect();
        let refs: Vec<_> = expected.iter().collect();
        let operation = GatedDrives::new(5);
        let mut pool = instrumented_pool(3);
        let progress = pool.progress();
        let mut pass = Box::pin(pool.verify_with(
            Pass::Initial,
            &operation,
            &refs,
            Instant::now() + Duration::from_secs(600),
        ));
        poll_pending(pass.as_mut()).await;
        assert_eq!(progress.snapshot()["live_slots"], 3);
        drop(pass);
        assert_eq!(progress.snapshot()["live_slots"], 0);
        assert_eq!(progress.snapshot()["complete"], false);
        assert!(!pool.settled());
        assert_eq!(
            pool.slots
                .iter()
                .filter(|slot| slot.handle.is_some() && slot.context.is_some())
                .count(),
            3
        );
        let accounting = pool.accounting.snapshot();
        assert_eq!(accounting["data_read"]["cancelled"], 3);
        assert_eq!(accounting["data_read"]["in_flight"], 0);
        pool.close_once().await.unwrap();
        assert!(pool.settled());
        assert_eq!(progress.snapshot()["settled"], true);
        assert_eq!(progress.snapshot()["completed_drives"], 0);
        assert_eq!(
            operation.evidence.starts.lock().unwrap().len(),
            3,
            "cleanup cannot resume or retry work"
        );
        assert_eq!(operation.evidence.closes.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn fresh_drive_pool_error_cancels_siblings_without_retry() {
        let expected: Vec<_> = (0..5).map(|drive| expected(drive, 1)).collect();
        let refs: Vec<_> = expected.iter().collect();
        let mut operation = GatedDrives::new(5);
        operation.fail = Some(1);
        let mut pool = instrumented_pool(3);
        let progress = pool.progress();
        let mut pass = Box::pin(pool.verify_with(
            Pass::Initial,
            &operation,
            &refs,
            Instant::now() + Duration::from_secs(600),
        ));
        poll_pending(pass.as_mut()).await;
        assert_eq!(operation.evidence.starts.lock().unwrap().len(), 3);
        operation.release(1);
        assert!(pass.await.is_err());
        assert_eq!(progress.snapshot()["completed_drives"], 0);
        assert_eq!(progress.snapshot()["live_slots"], 0);
        assert_eq!(pool.accounting.snapshot()["data_read"]["cancelled"], 2);
        assert!(!pool.settled());
        pool.close_once().await.unwrap();
        assert!(pool.settled());
        assert_eq!(operation.evidence.starts.lock().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn fresh_drive_pool_inherits_one_deadline_and_rejects_expired_admission() {
        let expected: Vec<_> = (0..5).map(|drive| expected(drive, 1)).collect();
        let refs: Vec<_> = expected.iter().collect();
        let operation = GatedDrives::new(5);
        for drive in 0..5 {
            operation.release(drive);
        }
        let mut pool = Pool::new(3).unwrap();
        let deadline = Instant::now() + Duration::from_secs(600);
        pool.verify_with(Pass::Initial, &operation, &refs, deadline)
            .await
            .unwrap();
        assert_eq!(
            *operation.evidence.deadlines.lock().unwrap(),
            vec![deadline; 5]
        );
        let operation = GatedDrives::new(5);
        let mut pool = Pool::new(3).unwrap();
        assert!(
            pool.verify_with(Pass::Initial, &operation, &refs, Instant::now())
                .await
                .is_err()
        );
        assert!(operation.evidence.starts.lock().unwrap().is_empty());
    }

    #[test]
    fn fresh_drive_pool_late_credit_cannot_qualify_a_completed_drive() {
        let expected = expected(0, 1);
        let progress = Pool::new(1).unwrap().progress();
        progress.begin(Pass::Initial, &[&expected]).unwrap();
        let deadline = Instant::now();
        assert!(
            credit_completion(
                &progress,
                &expected,
                Verified {
                    files: 1,
                    bytes: 4096
                },
                deadline,
                deadline
            )
            .is_err()
        );
        assert_eq!(progress.snapshot()["completed_drives"], 0);
    }

    #[tokio::test(start_paused = true)]
    async fn fresh_drive_pool_cleanup_attempts_every_slot_under_one_remembered_deadline() {
        let evidence = Arc::new(Evidence::default());
        let mut pool = Pool::new(3).unwrap();
        for (drive, mode) in [CloseMode::Pending, CloseMode::Error, CloseMode::Success]
            .into_iter()
            .enumerate()
        {
            pool.slots[drive].handle = Some(probe(drive, mode, &evidence));
        }
        let mut cleanup = Box::pin(pool.close_once());
        poll_pending(cleanup.as_mut()).await;
        let mut attempts = evidence.closes.lock().unwrap().clone();
        attempts.sort_unstable();
        assert_eq!(
            attempts,
            vec![0, 1, 2],
            "pending/error slot must not prevent any other owned slot's close attempt"
        );
        tokio::time::advance(Duration::from_secs(31)).await;
        assert!(cleanup.await.is_err());
        assert!(pool.slots[0].handle.is_some());
        assert!(pool.slots[1].handle.is_some());
        assert!(pool.slots[2].handle.is_none());
        let deadline = pool.cleanup_deadline.unwrap();
        assert!(!pool.settled());
        assert!(pool.close_once().await.is_err());
        assert_eq!(
            pool.cleanup_deadline,
            Some(deadline),
            "later cleanup must not renew the shared budget"
        );
    }

    #[tokio::test]
    async fn fresh_drive_pool_validates_the_whole_ledger_before_any_open() {
        for case in ["alignment", "duplicate", "overflow"] {
            if case == "overflow" && usize::BITS < 64 {
                continue;
            }
            let mut first = expected(0, 1);
            let mut second = expected(1, 1);
            match case {
                "alignment" => second.files.get_mut("mixed-0").unwrap().length = 4097,
                "duplicate" => second.drive = first.drive,
                "overflow" => first.files.get_mut("mixed-0").unwrap().length = usize::MAX & !4095,
                _ => unreachable!(),
            }
            let operation = GatedDrives::new(2);
            for drive in 0..2 {
                operation.release(drive);
            }
            let mut pool = Pool::new(2).unwrap();
            assert!(
                pool.verify_with(
                    Pass::Initial,
                    &operation,
                    &[&first, &second],
                    Instant::now() + Duration::from_secs(600)
                )
                .await
                .is_err(),
                "{case}"
            );
            assert!(
                operation.evidence.starts.lock().unwrap().is_empty(),
                "{case}: validation must precede every admission"
            );
            assert!(pool.settled());
        }
    }

    #[tokio::test]
    async fn fresh_drive_pool_actual_content_loop_rejects_corruption_identity_membership_eof_and_close()
     {
        let expected = expected(2, 2);
        for case in [
            "good",
            "last byte",
            "stale",
            "wrong Drive",
            "length",
            "short",
            "EOF",
            "close",
            "missing",
            "extra",
        ] {
            let evidence = Arc::new(Evidence::default());
            let mut data = super::super::fixture::oracle_block(1, 2, 0, 0, 17).to_vec();
            data.extend(super::super::fixture::oracle_block(1, 2, 0, 1, 23));
            let mut size = 8192;
            let mut count = 4096;
            let mut eof_count = 0;
            let mut close = CloseMode::Success;
            let mut names = vec!["mixed-0".to_string()];
            match case {
                "last byte" => data[8191] ^= 1,
                "stale" => data[4096..]
                    .copy_from_slice(&super::super::fixture::oracle_block(1, 2, 0, 1, 22)),
                "wrong Drive" => data[4096..]
                    .copy_from_slice(&super::super::fixture::oracle_block(1, 3, 0, 1, 23)),
                "length" => size = 8191,
                "short" => count = 4095,
                "EOF" => eof_count = 1,
                "close" => close = CloseMode::Error,
                "missing" => names.clear(),
                "extra" => names.push("unexpected".into()),
                _ => {}
            }
            let driver: Arc<dyn FsDriver> = Arc::new(ProbeDriver {
                names,
                handle: Arc::new(ProbeHandle {
                    data,
                    size,
                    count,
                    eof_count,
                    close,
                    drive: 2,
                    evidence: evidence.clone(),
                }),
            });
            let mut owner = Owner::default();
            let progress = Pool::new(1).unwrap().progress();
            progress.begin(Pass::Initial, &[&expected]).unwrap();
            let result = verify_contents(&mut owner, driver, &expected, Some(&progress)).await;
            assert_eq!(result.is_ok(), case == "good", "{case}");
            assert_eq!(evidence.mutations.load(Ordering::SeqCst), 0);
            assert!(
                evidence
                    .opens
                    .lock()
                    .unwrap()
                    .iter()
                    .all(|flags| flags == "r")
            );
            if case == "good" {
                assert_eq!(
                    result.unwrap(),
                    Verified {
                        files: 1,
                        bytes: 8192
                    }
                );
                assert_eq!(progress.snapshot()["checked_files"], 1);
                assert_eq!(progress.snapshot()["compared_bytes"], 8192);
                assert!(owner.handle.is_none());
            }
            if case == "last byte" {
                assert_eq!(
                    progress.snapshot()["compared_bytes"],
                    4096,
                    "failed comparison must not credit its bytes"
                );
                assert_eq!(progress.snapshot()["checked_files"], 0);
            }
            if case == "close" {
                assert_eq!(progress.snapshot()["checked_files"], 0);
            }
            let _ = owner.close().await;
        }
    }

    #[tokio::test]
    async fn fresh_drive_pool_actual_content_loop_allows_zero_and_rejects_unchecked_tail_before_open()
     {
        for length in [0, 1] {
            let mut expected = expected(0, 0);
            expected.files.get_mut("mixed-0").unwrap().length = length;
            let evidence = Arc::new(Evidence::default());
            let driver: Arc<dyn FsDriver> = Arc::new(ProbeDriver {
                names: vec!["mixed-0".into()],
                handle: Arc::new(ProbeHandle {
                    data: vec![0; length],
                    size: length as u64,
                    count: 4096,
                    eof_count: 0,
                    close: CloseMode::Success,
                    drive: 0,
                    evidence: evidence.clone(),
                }),
            });
            let mut owner = Owner::default();
            let result = verify_contents(&mut owner, driver, &expected, None).await;
            if length == 0 {
                assert_eq!(result.unwrap(), Verified { files: 1, bytes: 0 });
            } else {
                assert!(
                    result.is_err(),
                    "an unaligned expected tail cannot be skipped"
                );
                assert!(evidence.opens.lock().unwrap().is_empty());
            }
            owner.close().await.unwrap();
        }
    }
}
