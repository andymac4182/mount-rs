//! A stateful 9P2000.L session over the core filesystem driver.

use std::collections::HashMap;
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
};

use arc_swap::ArcSwapOption;

use mount_rs_core::append_log::AppendLog;
use mount_rs_core::drain_gate::DrainGate;
use mount_rs_core::park_cell::ParkCell;
use mount_rs_core::path::join_path;
use mount_rs_core::{
    ErrorCode, FsDriver, FsError, Loopback, MkdirOptions, OpenFlags, Result as FsResult, S_IFMT,
    S_IFREG, Stats,
};
use tokio::sync::Notify;

use crate::constants::*;
use crate::fids::{DirCursor, FidOpenState, FidTable, FidView, qid_version, walk_step};
use crate::locks::{P9LockClient, P9LockRequest, P9LockTable};
use crate::protocol::*;
use crate::wire::{P9Error, P9Qid, P9Reader, P9Writer};

pub const DEFAULT_MSIZE: u32 = P9_DEFAULT_MAX_FRAME as u32;
const NAME_MAX: usize = 255;
const NO_NUNAME: u32 = u32::MAX;
const NO_NGID: u32 = u32::MAX;

#[cfg(unix)]
unsafe extern "C" {
    fn getgid() -> std::os::raw::c_uint;
    fn getuid() -> std::os::raw::c_uint;
}

fn current_process_ids() -> (u32, u32) {
    #[cfg(unix)]
    {
        // POSIX getuid/getgid are available on both required targets without
        // introducing a libc dependency into this transport crate.
        unsafe { (getuid(), getgid()) }
    }
    #[cfg(not(unix))]
    {
        (u32::MAX, u32::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct P9User {
    pub uname: String,
    pub uid: Option<u32>,
    pub aname: String,
}

/// Reports one request-level error or malformed frame to an embedding facade.
/// The header is absent when framing failed before a trustworthy tag existed.
pub type P9SessionErrorHook = Arc<dyn Fn(FsError, Option<P9Header>) + Send + Sync + 'static>;

/// Reports one debug-mode reply-exactly-once assertion failure.
pub type P9AssertionHook = Arc<dyn Fn(String) + Send + Sync + 'static>;

/// Optional request/assertion hooks for a [`P9Session`]. Kept separate from
/// [`P9SessionOptions`] so callback ownership stays separate from scalar
/// session policy.
#[derive(Clone, Default)]
pub struct P9SessionHooks {
    pub on_error: Option<P9SessionErrorHook>,
    pub on_assertion: Option<P9AssertionHook>,
}

#[derive(Clone)]
pub struct P9SessionOptions {
    /// Server-side ceiling for version negotiation. `None` uses 1 MiB.
    pub msize: Option<u32>,
    pub use_driver_ino: bool,
    pub read_only: bool,
    pub claim_ownership: bool,
    /// Run reply-exactly-once assertions. Defaults to debug builds.
    pub debug: bool,
    pub locks: Option<P9LockTable>,
}

impl Default for P9SessionOptions {
    fn default() -> Self {
        Self {
            msize: None,
            use_driver_ino: true,
            read_only: false,
            claim_ownership: true,
            debug: cfg!(debug_assertions),
            locks: None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct P9SessionStats {
    pub requests: u64,
    pub replies: u64,
    pub errors: u64,
    pub dropped: u64,
    pub flushed: u64,
    pub assertions: u64,
    pub messages: HashMap<String, u64>,
}

/// The counters behind [`P9SessionStats`], each its own atomic so that
/// counting a message takes no lock. Each counter is exact; a snapshot reads
/// them one after another.
struct StatCounters {
    requests: AtomicU64,
    replies: AtomicU64,
    errors: AtomicU64,
    dropped: AtomicU64,
    flushed: AtomicU64,
    assertions: AtomicU64,
    /// Indexed by the message type byte; named when a snapshot is built.
    messages: [AtomicU64; 256],
}

impl StatCounters {
    fn new() -> Self {
        Self {
            requests: AtomicU64::new(0),
            replies: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            flushed: AtomicU64::new(0),
            assertions: AtomicU64::new(0),
            messages: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    fn snapshot(&self) -> P9SessionStats {
        P9SessionStats {
            requests: self.requests.load(Ordering::Relaxed),
            replies: self.replies.load(Ordering::Relaxed),
            errors: self.errors.load(Ordering::Relaxed),
            dropped: self.dropped.load(Ordering::Relaxed),
            flushed: self.flushed.load(Ordering::Relaxed),
            assertions: self.assertions.load(Ordering::Relaxed),
            // A message type appears once it has been received.
            messages: (0..=u8::MAX)
                .filter_map(|type_| {
                    let count = self.messages[usize::from(type_)].load(Ordering::Relaxed);
                    (count > 0).then(|| (message_name(type_), count))
                })
                .collect(),
        }
    }
}

struct SessionState {
    fids: FidTable,
    users: HashMap<u32, P9User>,
}

struct Pending {
    completed: AtomicBool,
    cancelled: AtomicBool,
    notify: Notify,
}

impl Pending {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            completed: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            notify: Notify::new(),
        })
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
}

const INFLIGHT_PAGE: usize = 256;

type InflightPage = [ArcSwapOption<Pending>; INFLIGHT_PAGE];

/// The calls in flight, one slot per tag.
///
/// A tag is 16 bits, so the table is a fixed two-level array rather than a
/// map: claiming a tag is one compare-and-swap on its slot, which is what
/// makes "a tag is in flight at most once" hold without a lock and without
/// allocating per request. A page of slots is created the first time a tag
/// in its range is used and then lives as long as the session; a client
/// that numbers its tags from zero touches one or two pages.
///
/// Clearing the table visits one slot at a time, so it is not one step: a
/// call that claims a slot behind the sweep stays, exactly as if it had
/// claimed it after the sweep finished.
struct Inflight {
    pages: [OnceLock<Box<InflightPage>>; 256],
}

impl Inflight {
    fn new() -> Self {
        Self {
            pages: std::array::from_fn(|_| OnceLock::new()),
        }
    }

    fn slot(&self, tag: u16) -> Option<&ArcSwapOption<Pending>> {
        self.pages[usize::from(tag >> 8)]
            .get()
            .map(|page| &page[usize::from(tag & 0xff)])
    }

    fn slot_or_create(&self, tag: u16) -> &ArcSwapOption<Pending> {
        let page = self.pages[usize::from(tag >> 8)]
            .get_or_init(|| Box::new(std::array::from_fn(|_| ArcSwapOption::empty())));
        &page[usize::from(tag & 0xff)]
    }

    /// Claim `tag` for `pending`. `false` if a call already holds it.
    fn register(&self, tag: u16, pending: &Arc<Pending>) -> bool {
        self.slot_or_create(tag)
            .compare_and_swap(&None::<Arc<Pending>>, Some(Arc::clone(pending)))
            .is_none()
    }

    /// Give `tag` to `pending` whoever holds it, returning that holder.
    fn replace(&self, tag: u16, pending: &Arc<Pending>) -> Option<Arc<Pending>> {
        self.slot_or_create(tag).swap(Some(Arc::clone(pending)))
    }

    /// Free `tag` if `pending` still holds it. A call that was replaced or
    /// cleared must not free the slot of the call that came after it.
    fn complete(&self, tag: u16, pending: &Arc<Pending>) {
        if let Some(slot) = self.slot(tag) {
            slot.compare_and_swap(pending, None);
        }
    }

    fn get(&self, tag: u16) -> Option<Arc<Pending>> {
        self.slot(tag)?.load_full()
    }

    /// Empty every slot but `keep`'s, returning the calls that held them.
    fn drain_except(&self, keep: Option<u16>) -> Vec<Arc<Pending>> {
        let mut drained = Vec::new();
        for (high, page) in self.pages.iter().enumerate() {
            let Some(page) = page.get() else {
                continue;
            };
            for (low, slot) in page.iter().enumerate() {
                if keep.is_some_and(|tag| usize::from(tag) == (high << 8 | low)) {
                    continue;
                }
                // Most slots are empty; only swap the ones that are not.
                if slot.load().is_some() {
                    drained.extend(slot.swap(None));
                }
            }
        }
        drained
    }

    fn len(&self) -> usize {
        self.pages
            .iter()
            .filter_map(OnceLock::get)
            .flat_map(|page| page.iter())
            .filter(|slot| slot.load().is_some())
            .count()
    }
}

struct SessionInner {
    driver: Loopback,
    options: P9SessionOptions,
    /// A park cell: a walk scans the known qids and a directory read copies
    /// a listing while holding it.
    state: ParkCell<SessionState>,
    /// The negotiated message size, zero until a Tversion agrees one. With
    /// `generation` and `destroyed`, written only inside a `state` section
    /// and read without it: every request reads them several times.
    msize: AtomicU32,
    generation: AtomicU64,
    destroyed: AtomicBool,
    inflight: Inflight,
    path_lock: DrainGate,
    locks: P9LockClient,
    stats: StatCounters,
    hooks: P9SessionHooks,
    assertions: AppendLog<String>,
}

#[derive(Clone)]
pub struct P9Session {
    inner: Arc<SessionInner>,
}

#[derive(Clone)]
struct FidSnapshot {
    path: String,
    open: Option<FidOpenState>,
}

impl P9Session {
    pub fn new<D>(driver: D) -> Self
    where
        D: FsDriver + 'static,
    {
        Self::from_arc(Arc::new(driver))
    }

    pub fn from_arc(driver: Arc<dyn FsDriver>) -> Self {
        Self::with_options(driver, P9SessionOptions::default())
    }

    pub fn with_options(driver: Arc<dyn FsDriver>, options: P9SessionOptions) -> Self {
        Self::with_options_and_hooks(driver, options, P9SessionHooks::default())
    }

    pub fn with_options_and_hooks(
        driver: Arc<dyn FsDriver>,
        options: P9SessionOptions,
        hooks: P9SessionHooks,
    ) -> Self {
        let locks = options
            .locks
            .clone()
            .unwrap_or_else(|| P9LockTable::new(Default::default()))
            .client();
        Self {
            inner: Arc::new(SessionInner {
                driver: Loopback::from_arc(driver),
                state: ParkCell::new(SessionState {
                    fids: FidTable::new(options.use_driver_ino),
                    users: HashMap::new(),
                }),
                msize: AtomicU32::new(0),
                generation: AtomicU64::new(0),
                destroyed: AtomicBool::new(false),
                options,
                inflight: Inflight::new(),
                path_lock: DrainGate::new(),
                locks,
                stats: StatCounters::new(),
                hooks,
                assertions: AppendLog::new(),
            }),
        }
    }

    /// Return the authoritative driver shared by this session.
    pub fn driver(&self) -> Arc<dyn FsDriver> {
        Arc::clone(&self.inner.driver.driver)
    }

    pub fn msize(&self) -> Option<u32> {
        match self.inner.msize.load(Ordering::SeqCst) {
            0 => None,
            msize => Some(msize),
        }
    }

    pub fn version(&self) -> Option<&'static str> {
        self.msize().map(|_| P9_VERSION_DOTL)
    }

    pub fn generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    pub fn destroyed(&self) -> bool {
        self.inner.destroyed.load(Ordering::SeqCst)
    }

    pub fn inflight(&self) -> usize {
        self.inner.inflight.len()
    }

    pub fn stats(&self) -> P9SessionStats {
        self.inner.stats.snapshot()
    }

    /// Return debug-mode assertion failures in occurrence order.
    pub fn assertions(&self) -> Vec<String> {
        self.inner.assertions.snapshot()
    }

    pub fn user_for(&self, fid: u32) -> Option<P9User> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .users
            .get(&fid)
            .cloned()
    }

    /// Return this session's handle on the shared byte-range lock table.
    ///
    /// The clone preserves the client id, so inspection or explicit lock
    /// operations through the public handle affect the same ownership that
    /// protocol requests and teardown use.
    pub fn lock_client(&self) -> P9LockClient {
        self.inner.locks.clone()
    }

    pub fn fid_size(&self) -> usize {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .len()
    }

    pub fn fid_qid_path_count(&self) -> usize {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .qid_path_count()
    }

    pub fn fid_exists(&self, fid: u32) -> bool {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .get(fid)
            .is_some()
    }

    pub fn fid_view(&self, fid: u32) -> FsResult<FidView> {
        let state = self.inner.state.lock().expect("9P session mutex poisoned");
        state
            .fids
            .view(fid)
            .ok_or_else(|| FsError::new(ErrorCode::Ebadf).with_message(format!("EBADF: fid {fid}")))
    }

    pub fn fid_ids(&self) -> Vec<u32> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .fids()
    }

    pub fn fid_create(&self, fid: u32, path: &str) -> FsResult<()> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .create(fid, path)
            .map(|_| ())
    }

    pub fn fid_clone(&self, from: u32, to: u32) -> FsResult<()> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .clone_fid(from, to)
            .map(|_| ())
    }

    pub fn fid_clunk(&self, fid: u32) -> FsResult<FidView> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .clunk(fid)
            .map(|entry| entry.view())
    }

    pub fn fid_set_path(&self, fid: u32, path: &str) -> FsResult<()> {
        let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
        state
            .fids
            .get_mut(fid)
            .ok_or_else(|| {
                FsError::new(ErrorCode::Ebadf).with_message(format!("EBADF: fid {fid}"))
            })?
            .set_path(path);
        Ok(())
    }

    pub fn fid_set_open(&self, fid: u32, open: Option<FidOpenState>) -> FsResult<()> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .set_open(fid, open)
    }

    pub fn fid_set_iounit(&self, fid: u32, iounit: u32) -> FsResult<()> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .set_iounit(fid, iounit)
    }

    pub fn fid_set_cursor(&self, fid: u32, cursor: Option<DirCursor>) -> FsResult<()> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .set_cursor(fid, cursor)
    }

    pub fn fid_resume(&self, fid: u32, offset: u64) -> FsResult<Option<(Vec<String>, usize)>> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .resume(fid, offset)
    }

    pub fn fid_snapshot_entries(
        &self,
        fid: u32,
        entries: Vec<String>,
    ) -> FsResult<(Vec<String>, usize)> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .snapshot(fid, entries)
    }

    pub fn fid_note_offset(&self, fid: u32, offset: u64, index: usize) {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .note_offset(fid, offset, index);
    }

    pub fn fid_qid_for(&self, stats: &Stats, path: &str) -> P9Qid {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .qid_for(stats, path)
    }

    pub fn fid_qid_path_for(&self, stats: &Stats, path: &str) -> u64 {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .qid_for(stats, path)
            .path
    }

    pub fn fid_release(&self, path: &str) {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .release(path);
    }

    pub fn fid_remap(&self, from: &str, to: &str) {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .remap(from, to);
    }

    pub fn fid_clear(&self) {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .clear();
    }

    /// Answer one complete frame. Malformed framing has no trustworthy tag and
    /// therefore returns `None`; malformed bodies are addressed with `Rlerror`.
    pub async fn handle_call(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.count_request(None);
        let (header, body) = match decode_message(bytes) {
            Ok(value) => value,
            Err(error) => {
                self.count_dropped();
                self.report_error(
                    FsError::new(ErrorCode::Eproto).with_message(error.to_string()),
                    None,
                );
                return None;
            }
        };
        self.count_message(header.type_);

        if header.type_ == P9_TFLUSH {
            let reply = self.run(header, body, self.generation()).await;
            return Some(reply);
        }

        let pending = Pending::new();
        let valid_version =
            header.type_ == P9_TVERSION && decode_message_as(bytes, read_tversion).is_ok();
        if valid_version {
            // A valid Tversion aborts every old call, including one that
            // used the version tag. Pending completion is tied to its Arc,
            // so a late old handler cannot remove this new entry.
            let replaced = self.inner.inflight.replace(header.tag, &pending);
            for old in self.inner.inflight.drain_except(Some(header.tag)) {
                old.cancel();
            }
            if let Some(old) = replaced {
                old.cancel();
            }
        } else if !self.inner.inflight.register(header.tag, &pending) {
            // No cell is held here: the assertion hook is the embedder's
            // code and may call back into this session.
            let message = format!(
                "tag {} is already in flight ({})",
                header.tag,
                message_name(header.type_)
            );
            self.assertion(message);
            return Some(
                self.error_reply(
                    header,
                    FsError::new(ErrorCode::Eproto)
                        .with_message(format!("EPROTO: tag {} is already in flight", header.tag)),
                ),
            );
        }

        let generation = self.generation();
        let reply = self.run(header, body, generation).await;
        pending.completed.store(true, Ordering::Release);
        pending.notify.notify_waiters();
        self.inner.inflight.complete(header.tag, &pending);
        Some(reply)
    }

    pub async fn destroy(&self) {
        let (handles, pending) = {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            // `destroyed` is only written here, inside the section, so
            // this check and the store below are one step for destroyers.
            if self.inner.destroyed.load(Ordering::SeqCst) {
                return;
            }
            // Set before the generation moves: a call that sees the new
            // generation must also see why.
            self.inner.destroyed.store(true, Ordering::SeqCst);
            self.inner.generation.fetch_add(1, Ordering::SeqCst);
            let handles = state.fids.open_handles();
            state.fids.clear();
            state.users.clear();
            self.inner.msize.store(0, Ordering::SeqCst);
            // Inside the section so the generation moves before the drain.
            let pending = self.inner.inflight.drain_except(None);
            (handles, pending)
        };
        for pending in pending {
            pending.cancel();
        }
        self.inner.locks.release_all();
        for (_, handle) in handles {
            if let Err(error) = handle.close().await {
                self.report_error(error, None);
            }
        }
    }

    async fn run(&self, header: P9Header, body: P9Reader<'_>, generation: u64) -> Vec<u8> {
        let result = self.dispatch(header, body).await;
        let result = if header.type_ != P9_TVERSION && self.generation() != generation {
            Err(if self.destroyed() {
                FsError::new(ErrorCode::Enodev)
            } else {
                FsError::new(ErrorCode::Eio)
            })
        } else {
            result
        };
        match result {
            Ok(reply) => {
                self.count_reply(false);
                reply
            }
            Err(error) => self.error_reply(header, error),
        }
    }

    async fn dispatch(&self, header: P9Header, mut body: P9Reader<'_>) -> FsResult<Vec<u8>> {
        if self.destroyed() {
            return Err(FsError::new(ErrorCode::Enodev)
                .with_message("ENODEV: the 9P session has been torn down"));
        }
        if header.type_ != P9_TVERSION && self.msize().is_none() {
            return Err(FsError::new(ErrorCode::Eproto).with_message(format!(
                "EPROTO: {} before Tversion",
                message_name(header.type_)
            )));
        }
        match header.type_ {
            P9_TVERSION => {
                let request = decode_body(&mut body, message_name(header.type_), read_tversion)?;
                self.version_exchange(header, request).await
            }
            P9_TFLUSH => {
                let request = decode_body(&mut body, "Tflush", read_tflush)?;
                self.flush(header, request.oldtag).await
            }
            P9_TAUTH => {
                let _ = decode_body(&mut body, "Tauth", read_tauth)?;
                Err(FsError::new(ErrorCode::Enotsup)
                    .with_message("ENOTSUP: this server has no authentication file"))
            }
            P9_TATTACH => {
                let request = decode_body(&mut body, "Tattach", read_tattach)?;
                let _guard = self.inner.path_lock.read().await;
                self.attach(header, request).await
            }
            P9_TWALK => {
                let request = decode_body(&mut body, "Twalk", read_twalk)?;
                let _guard = self.inner.path_lock.read().await;
                self.walk(header, request).await
            }
            P9_TCLUNK => {
                let fid = decode_body(&mut body, "Tclunk", read_fid_request)?.fid;
                self.clunk(header, fid).await
            }
            P9_TGETATTR => {
                let request = decode_body(&mut body, "Tgetattr", read_tgetattr)?;
                let _guard = self.inner.path_lock.read().await;
                self.getattr(header, request).await
            }
            P9_TSTATFS => {
                let fid = decode_body(&mut body, "Tstatfs", read_fid_request)?.fid;
                let _guard = self.inner.path_lock.read().await;
                self.statfs(header, fid).await
            }
            P9_TREADDIR => {
                let request = decode_body(&mut body, "Treaddir", read_treaddir)?;
                let budget = self.io_budget(request.count, P9_READDIRHDRSZ);
                let _guard = self.inner.path_lock.read().await;
                self.readdir(header, request, budget).await
            }
            P9_TLOPEN => {
                let request = decode_body(&mut body, "Tlopen", read_tlopen)?;
                let _guard = self.inner.path_lock.read().await;
                self.lopen(header, request).await
            }
            P9_TLCREATE => {
                let request = decode_body(&mut body, "Tlcreate", read_tlcreate)?;
                let _guard = self.inner.path_lock.read().await;
                self.lcreate(header, request).await
            }
            P9_TREAD => {
                let request = decode_body(&mut body, "Tread", read_tread)?;
                let budget = self.io_budget(request.count, P9_IOHDRSZ);
                self.read_file(header, request, budget).await
            }
            P9_TWRITE => {
                let request = decode_body(&mut body, "Twrite", read_twrite)?;
                self.write_file(header, request).await
            }
            P9_TFSYNC => {
                let request = decode_body(&mut body, "Tfsync", read_tfsync)?;
                self.fsync(header, request).await
            }
            P9_TSETATTR => {
                let request = decode_body(&mut body, "Tsetattr", read_tsetattr)?;
                let _guard = self.inner.path_lock.read().await;
                self.setattr(header, request).await
            }
            P9_TMKDIR => {
                let request = decode_body(&mut body, "Tmkdir", read_tmkdir)?;
                let _guard = self.inner.path_lock.read().await;
                self.mkdir(header, request).await
            }
            P9_TSYMLINK => {
                let request = decode_body(&mut body, "Tsymlink", read_tsymlink)?;
                let _guard = self.inner.path_lock.read().await;
                self.symlink(header, request).await
            }
            P9_TMKNOD => {
                let request = decode_body(&mut body, "Tmknod", read_tmknod)?;
                let _guard = self.inner.path_lock.read().await;
                self.mknod(header, request).await
            }
            P9_TLINK => {
                let request = decode_body(&mut body, "Tlink", read_tlink)?;
                let _guard = self.inner.path_lock.read().await;
                self.link(header, request).await
            }
            P9_TREADLINK => {
                let fid = decode_body(&mut body, "Treadlink", read_fid_request)?.fid;
                let _guard = self.inner.path_lock.read().await;
                self.readlink(header, fid).await
            }
            P9_TRENAME => {
                let request = decode_body(&mut body, "Trename", read_trename)?;
                let _guard = self.inner.path_lock.write().await;
                self.rename(header, request).await
            }
            P9_TRENAMEAT => {
                let request = decode_body(&mut body, "Trenameat", read_trenameat)?;
                let _guard = self.inner.path_lock.write().await;
                self.renameat(header, request).await
            }
            P9_TUNLINKAT => {
                let request = decode_body(&mut body, "Tunlinkat", read_tunlinkat)?;
                let _guard = self.inner.path_lock.read().await;
                self.unlinkat(header, request).await
            }
            P9_TREMOVE => {
                let fid = decode_body(&mut body, "Tremove", read_fid_request)?.fid;
                // Shared with every other path operation and excluded by a
                // rename, so the path taken from the fid inside `remove` is
                // still the entry's name when it is unlinked.
                let _guard = self.inner.path_lock.read().await;
                self.remove(header, fid).await
            }
            P9_TXATTRWALK | P9_TXATTRCREATE => Err(FsError::new(ErrorCode::Enotsup)
                .with_message("ENOTSUP: this server has no extended attributes")),
            P9_TLOCK => {
                let request = decode_body(&mut body, "Tlock", read_tlock)?;
                self.set_lock(header, request).await
            }
            P9_TGETLOCK => {
                let request = decode_body(&mut body, "Tgetlock", read_tgetlock)?;
                self.get_lock(header, request).await
            }
            _ => Err(FsError::new(ErrorCode::Enotsup).with_message(format!(
                "ENOTSUP: {} is not supported",
                message_name(header.type_)
            ))),
        }
    }

    fn io_budget(&self, requested: u32, header_size: u32) -> usize {
        self.msize()
            .unwrap_or(DEFAULT_MSIZE)
            .saturating_sub(header_size)
            .min(requested) as usize
    }

    fn frame<F>(&self, header: P9Header, type_: u8, capacity: usize, write: F) -> FsResult<Vec<u8>>
    where
        F: FnOnce(&mut P9Writer) -> Result<(), P9Error>,
    {
        encode_message(type_, header.tag, capacity, write)
            .map_err(|error| FsError::new(ErrorCode::Eio).with_message(error.to_string()))
    }

    fn error_reply(&self, header: P9Header, error: FsError) -> Vec<u8> {
        self.report_error(error.clone(), Some(header));
        self.count_reply(true);
        // Rlerror is a fixed eleven-byte frame. Build it directly so an
        // internal encoding fallback can never replace the driver's errno
        // with a fabricated EIO.
        let mut bytes = Vec::with_capacity(11);
        bytes.extend_from_slice(&11_u32.to_le_bytes());
        bytes.push(P9_RLERROR);
        bytes.extend_from_slice(&header.tag.to_le_bytes());
        bytes.extend_from_slice(&error.code.errno().unsigned_abs().to_le_bytes());
        bytes
    }

    fn count_request(&self, _type_: Option<u8>) {
        self.inner.stats.requests.fetch_add(1, Ordering::Relaxed);
    }
    fn count_dropped(&self) {
        self.inner.stats.dropped.fetch_add(1, Ordering::Relaxed);
    }
    fn count_message(&self, type_: u8) {
        self.inner.stats.messages[usize::from(type_)].fetch_add(1, Ordering::Relaxed);
    }
    fn count_reply(&self, error: bool) {
        self.inner.stats.replies.fetch_add(1, Ordering::Relaxed);
        if error {
            self.inner.stats.errors.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn report_error(&self, error: FsError, header: Option<P9Header>) {
        if let Some(hook) = &self.inner.hooks.on_error {
            hook(error, header);
        }
    }

    fn assertion(&self, message: String) {
        if !self.inner.options.debug {
            return;
        }
        self.inner.stats.assertions.fetch_add(1, Ordering::Relaxed);
        self.inner.assertions.push(message.clone());
        if let Some(hook) = &self.inner.hooks.on_assertion {
            hook(message);
        }
    }

    async fn flush(&self, header: P9Header, oldtag: u16) -> FsResult<Vec<u8>> {
        if let Some(pending) = self.inner.inflight.get(oldtag) {
            self.inner.stats.flushed.fetch_add(1, Ordering::Relaxed);
            loop {
                let notified = pending.notify.notified();
                tokio::pin!(notified);
                // Register before checking completion. Destruction and request
                // completion use notify_waiters(), which does not retain a
                // permit for a future created after the notification.
                notified.as_mut().enable();
                if pending.completed.load(Ordering::Acquire)
                    || pending.cancelled.load(Ordering::Acquire)
                {
                    break;
                }
                notified.await;
            }
        }
        self.frame(header, P9_RFLUSH, 8, |_| Ok(()))
    }

    async fn version_exchange(&self, header: P9Header, request: Tversion) -> FsResult<Vec<u8>> {
        self.reset(header.tag).await;
        let ceiling = self.inner.options.msize.unwrap_or(DEFAULT_MSIZE);
        let msize = request.msize.min(ceiling);
        let agreed = request.version == P9_VERSION_DOTL && msize >= P9_MIN_MSIZE;
        if agreed {
            // Taken so this cannot land after a destroy or reset cleared it.
            let _state = self.inner.state.lock().expect("9P session mutex poisoned");
            if !self.inner.destroyed.load(Ordering::SeqCst) {
                self.inner.msize.store(msize, Ordering::SeqCst);
            }
        }
        self.frame(header, P9_RVERSION, 32, |writer| {
            write_rversion(
                writer,
                &Rversion {
                    msize,
                    version: if agreed {
                        P9_VERSION_DOTL.to_owned()
                    } else {
                        P9_VERSION_UNKNOWN.to_owned()
                    },
                },
            )
        })
    }

    async fn reset(&self, version_tag: u16) {
        let (handles, pending) = {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            self.inner.generation.fetch_add(1, Ordering::SeqCst);
            let handles = state.fids.open_handles();
            state.fids.clear();
            state.users.clear();
            self.inner.msize.store(0, Ordering::SeqCst);
            let pending = self.inner.inflight.drain_except(Some(version_tag));
            (handles, pending)
        };
        for request in pending {
            request.cancel();
        }
        self.inner.locks.release_all();
        for (_, handle) in handles {
            let _ = handle.close().await;
        }
    }

    async fn attach(&self, header: P9Header, request: Tattach) -> FsResult<Vec<u8>> {
        if request.afid != P9_NOFID {
            return Err(FsError::new(ErrorCode::Enotsup)
                .with_message("ENOTSUP: authentication fids are not supported"));
        }
        let path = join_path(&["/", &request.aname]);
        let stats = self.stat_of(&path).await?;
        if stats.mode & S_IFMT != mount_rs_core::types::S_IFDIR {
            return Err(FsError::new(ErrorCode::Enotdir)
                .with_syscall("attach")
                .with_path(path));
        }
        let qid = {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            state.fids.create(request.fid, &path)?;
            state.users.insert(
                request.fid,
                P9User {
                    uname: request.uname,
                    uid: (request.n_uname != NO_NUNAME).then_some(request.n_uname),
                    aname: request.aname,
                },
            );
            state.fids.qid_for(&stats, &path)
        };
        self.frame(header, P9_RATTACH, 32, |writer| {
            write_rattach(writer, &Rattach { qid });
            Ok(())
        })
    }

    async fn walk(&self, header: P9Header, request: Twalk) -> FsResult<Vec<u8>> {
        let source = self.fid_snapshot(request.fid)?;
        if source.open.is_some() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message("EINVAL: an open fid cannot be walked"));
        }
        if request.newfid != request.fid && self.has_fid(request.newfid) {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message(format!("EINVAL: fid {} is already in use", request.newfid)));
        }
        let mut path = source.path;
        let mut qids = Vec::new();
        for (index, name) in request.wnames.iter().enumerate() {
            let next = walk_step(&path, name);
            let next = match next {
                Ok(next) => next,
                Err(error) if index == 0 => return Err(error),
                Err(_) => break,
            };
            let stats = match self.stat_of(&next).await {
                Ok(stats) => stats,
                Err(error) if index == 0 => return Err(error),
                Err(_) => break,
            };
            let qid = {
                let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
                state.fids.qid_for(&stats, &next)
            };
            qids.push(qid);
            path = next;
        }
        if qids.len() == request.wnames.len() {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            let entry = state.fids.clone_fid(request.fid, request.newfid)?;
            entry.set_path(path);
            if let Some(user) = state.users.get(&request.fid).cloned() {
                state.users.insert(request.newfid, user);
            }
        }
        self.frame(header, P9_RWALK, 16 + qids.len() * P9_QID_SIZE, |writer| {
            write_rwalk(writer, &Rwalk { wqids: qids })
        })
    }

    async fn clunk(&self, header: P9Header, fid: u32) -> FsResult<Vec<u8>> {
        let entry = {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            state.users.remove(&fid);
            state.fids.clunk(fid)?
        };
        self.inner.locks.release_fid(fid);
        if let Some(open) = entry.open
            && let Some(handle) = open.handle
        {
            let _ = handle.close().await;
        }
        self.frame(header, P9_RCLUNK, 8, |_| Ok(()))
    }

    fn has_fid(&self, fid: u32) -> bool {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .get(fid)
            .is_some()
    }

    fn fid_snapshot(&self, fid: u32) -> FsResult<FidSnapshot> {
        let state = self.inner.state.lock().expect("9P session mutex poisoned");
        let entry = state.fids.require(fid)?;
        Ok(FidSnapshot {
            path: entry.path.clone(),
            open: entry.open.clone(),
        })
    }

    async fn stat_of(&self, path: &str) -> FsResult<Stats> {
        match self.inner.driver.lstat(path).await {
            Ok(stats) => Ok(stats),
            Err(error) if error.is(ErrorCode::Enosys) => self.inner.driver.stat(path).await,
            Err(error) => Err(error),
        }
    }

    fn qid_for(&self, stats: &Stats, path: &str) -> P9Qid {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .fids
            .qid_for(stats, path)
    }

    fn require_open(&self, fid: u32, syscall: &str) -> FsResult<FidSnapshot> {
        let snapshot = self.fid_snapshot(fid)?;
        if snapshot.open.is_none() {
            return Err(FsError::new(ErrorCode::Ebadf)
                .with_syscall(syscall)
                .with_path(snapshot.path));
        }
        Ok(snapshot)
    }

    fn require_file(&self, fid: u32, syscall: &str) -> FsResult<FidSnapshot> {
        let snapshot = self.require_open(fid, syscall)?;
        if snapshot.open.as_ref().is_some_and(|open| open.directory) {
            return Err(FsError::new(ErrorCode::Eisdir)
                .with_syscall(syscall)
                .with_path(snapshot.path));
        }
        Ok(snapshot)
    }

    async fn getattr(&self, header: P9Header, request: Tgetattr) -> FsResult<Vec<u8>> {
        let snapshot = self.fid_snapshot(request.fid)?;
        let (qid, stats) = if let Some(open) = &snapshot.open {
            if let (Some(qid), Some(handle)) = (open.qid, open.handle.as_ref())
                && qid.type_ & P9_QTSYMLINK == 0
            {
                match handle.stat().await {
                    Ok(stats) => (
                        P9Qid {
                            type_: qid.type_,
                            version: qid_version(&stats),
                            path: qid.path,
                        },
                        stats,
                    ),
                    Err(error) if error.is(ErrorCode::Enosys) => {
                        self.path_attributes(&snapshot.path).await?
                    }
                    Err(error) => return Err(error),
                }
            } else {
                self.path_attributes(&snapshot.path).await?
            }
        } else {
            self.path_attributes(&snapshot.path).await?
        };
        let born = stats.birthtime_ms > 0;
        let valid =
            request.request_mask & (P9_GETATTR_BASIC | if born { P9_GETATTR_BTIME } else { 0 });
        let value = Rgetattr {
            valid,
            qid,
            mode: stats.mode,
            uid: stats.uid,
            gid: stats.gid,
            nlink: stats.nlink,
            rdev: stats.rdev,
            size: stats.size,
            blksize: stats.blksize,
            blocks: stats.blocks,
            atime: split_ms(stats.atime_ms),
            mtime: split_ms(stats.mtime_ms),
            ctime: split_ms(stats.ctime_ms),
            btime: if born {
                split_ms(stats.birthtime_ms)
            } else {
                P9Time { sec: 0, nsec: 0 }
            },
            r#gen: 0,
            data_version: 0,
        };
        self.frame(header, P9_RGETATTR, 192, |writer| {
            write_rgetattr(writer, value);
            Ok(())
        })
    }

    async fn path_attributes(&self, path: &str) -> FsResult<(P9Qid, Stats)> {
        let stats = self.stat_of(path).await?;
        let qid = self.qid_for(&stats, path);
        Ok((qid, stats))
    }

    async fn statfs(&self, header: P9Header, fid: u32) -> FsResult<Vec<u8>> {
        let snapshot = self.fid_snapshot(fid)?;
        if !self.inner.driver.capabilities.statfs {
            return Err(FsError::new(ErrorCode::Enosys)
                .with_syscall("statfs")
                .with_path(snapshot.path));
        }
        let stats = self.inner.driver.statfs(&snapshot.path).await?;
        self.frame(header, P9_RSTATFS, 80, |writer| {
            write_rstatfs(
                writer,
                Rstatfs {
                    type_: if stats.filesystem_type == 0 {
                        V9FS_MAGIC
                    } else {
                        stats.filesystem_type as u32
                    },
                    bsize: if stats.block_size == 0 {
                        4096
                    } else {
                        stats.block_size.min(u32::MAX as u64) as u32
                    },
                    blocks: stats.blocks,
                    bfree: stats.blocks_free,
                    bavail: stats.blocks_available,
                    files: stats.files,
                    ffree: stats.files_free,
                    fsid: 0,
                    namelen: NAME_MAX as u32,
                },
            );
            Ok(())
        })
    }

    async fn readdir(
        &self,
        header: P9Header,
        request: Treaddir,
        budget: usize,
    ) -> FsResult<Vec<u8>> {
        let snapshot = self.require_open(request.fid, "readdir")?;
        if !snapshot.open.as_ref().is_some_and(|open| open.directory) {
            return Err(FsError::new(ErrorCode::Enotdir)
                .with_syscall("readdir")
                .with_path(snapshot.path));
        }
        let resumed = {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            state.fids.resume(request.fid, request.offset)?
        };
        let (entries, mut index) = if let Some(resumed) = resumed {
            resumed
        } else {
            let mut names = vec![".".to_owned(), "..".to_owned()];
            names.extend(
                self.inner
                    .driver
                    .readdir(&snapshot.path)
                    .await?
                    .into_iter()
                    .map(|entry| entry.name),
            );
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            state.fids.snapshot(request.fid, names)?
        };
        let mut packer = P9DirentPacker::new(budget);
        while index < entries.len() {
            let name = &entries[index];
            let child = join_path(&[&snapshot.path, name]);
            let stats = match self.stat_of(&child).await {
                Ok(stats) => stats,
                Err(_) => {
                    index += 1;
                    continue;
                }
            };
            let qid = self.qid_for(&stats, &child);
            let dirent = P9Dirent {
                qid,
                offset: (index + 1) as u64,
                type_: ((stats.mode & S_IFMT) >> 12) as u8,
                name: name.clone(),
            };
            if !packer.add(&dirent).map_err(|error| {
                FsError::new(ErrorCode::Enametoolong).with_message(error.to_string())
            })? {
                break;
            }
            index += 1;
            let offset = index as u64;
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            state.fids.note_offset(request.fid, offset, index);
        }
        if packer.count() == 0 && index < entries.len() {
            return Err(FsError::new(ErrorCode::Einval).with_message(format!(
                "EINVAL: count {} has no room for a single entry",
                request.count
            )));
        }
        self.frame(header, P9_RREADDIR, packer.size() + 16, |writer| {
            write_rreaddir(
                writer,
                &Rreaddir {
                    data: packer.bytes(),
                },
            );
            Ok(())
        })
    }

    async fn lopen(&self, header: P9Header, request: Tlopen) -> FsResult<Vec<u8>> {
        let snapshot = self.fid_snapshot(request.fid)?;
        if snapshot.open.is_some() {
            return Err(FsError::new(ErrorCode::Einval)
                .with_message(format!("EINVAL: fid {} is already open", request.fid)));
        }
        if write_intent(request.flags) {
            self.require_writable("open", &snapshot.path)?;
        }
        let mut stats = self.stat_of(&snapshot.path).await?;
        let driver_flags = OpenFlags::from_bits(request.flags as u64);
        if stats.mode & S_IFMT == mount_rs_core::types::S_IFDIR {
            if write_intent(request.flags) {
                return Err(FsError::new(ErrorCode::Eisdir)
                    .with_syscall("open")
                    .with_path(snapshot.path));
            }
            self.set_open(
                request.fid,
                FidOpenState {
                    flags: reopen_flags(driver_flags),
                    wire_flags: request.flags,
                    handle: None,
                    directory: true,
                    qid: None,
                },
            )?;
        } else {
            let handle = self
                .inner
                .driver
                .open_flags(&snapshot.path, driver_flags, 0)
                .await?;
            let keep = self.inner.driver.capabilities.handles;
            if !keep {
                let _ = handle.close().await;
            }
            if request.flags & P9_O_TRUNC != 0 {
                match self.stat_of(&snapshot.path).await {
                    Ok(updated) => stats = updated,
                    Err(error) => {
                        if keep {
                            let _ = handle.close().await;
                        }
                        return Err(error);
                    }
                }
            }
            if let Err(error) = self.ensure_openable(request.fid) {
                if keep {
                    let _ = handle.close().await;
                }
                return Err(error);
            }
            let open_handle = keep.then(|| Arc::clone(&handle));
            if let Err(error) = self.set_open(
                request.fid,
                FidOpenState {
                    flags: reopen_flags(driver_flags),
                    wire_flags: request.flags,
                    handle: open_handle,
                    directory: false,
                    qid: None,
                },
            ) {
                if keep {
                    let _ = handle.close().await;
                }
                return Err(error);
            }
        }
        let qid = self.qid_for(&stats, &snapshot.path);
        {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            if let Some(entry) = state.fids.get_mut(request.fid)
                && let Some(open) = entry.open.as_mut()
            {
                open.qid = Some(qid);
                entry.iounit = 0;
            }
        }
        self.frame(header, P9_RLOPEN, 32, |writer| {
            write_rlopen(writer, Rlopen { qid, iounit: 0 });
            Ok(())
        })
    }

    async fn lcreate(&self, header: P9Header, request: Tlcreate) -> FsResult<Vec<u8>> {
        let snapshot = self.fid_snapshot(request.fid)?;
        if snapshot.open.is_some() {
            return Err(
                FsError::new(ErrorCode::Einval).with_message("EINVAL: an open fid cannot create")
            );
        }
        self.require_writable("open", &snapshot.path)?;
        let path = child_of(&snapshot.path, &request.name, "open")?;
        let mut flags = OpenFlags::from_bits((request.flags | P9_O_CREAT) as u64);
        flags.create = true;
        let handle = self
            .inner
            .driver
            .open_flags(&path, flags, request.mode & 0o7777)
            .await?;
        let keep = self.inner.driver.capabilities.handles;
        if !keep {
            let _ = handle.close().await;
        }
        let stats = match self.stat_of(&path).await {
            Ok(stats) => stats,
            Err(error) => {
                if keep {
                    let _ = handle.close().await;
                }
                return Err(error);
            }
        };
        if let Err(error) = self.ensure_openable(request.fid) {
            if keep {
                let _ = handle.close().await;
            }
            return Err(error);
        }
        if let Err(error) = self.claim(&path, request.fid, request.gid).await {
            if keep {
                let _ = handle.close().await;
            }
            return Err(error);
        }
        let qid = self.qid_for(&stats, &path);
        let state_result =
            {
                let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
                match state.fids.get_mut(request.fid) {
                    // The check before the awaits above is only a fast path:
                    // another Tlcreate or Tlopen on this fid may have stored
                    // its handle since. Storing over it would leak that handle.
                    Some(entry) if entry.open.is_some() => Err(FsError::new(ErrorCode::Ebusy)
                        .with_message(format!(
                            "EBUSY: fid {} was opened concurrently",
                            request.fid
                        ))),
                    Some(entry) => {
                        entry.set_path(&path);
                        entry.open = Some(FidOpenState {
                            flags: reopen_flags(flags),
                            wire_flags: request.flags,
                            handle: keep.then(|| Arc::clone(&handle)),
                            directory: false,
                            qid: Some(qid),
                        });
                        entry.iounit = 0;
                        Ok(())
                    }
                    None => Err(FsError::new(ErrorCode::Ebadf)
                        .with_message("EBADF: create fid disappeared")),
                }
            };
        if let Err(error) = state_result {
            if keep {
                let _ = handle.close().await;
            }
            return Err(error);
        }
        self.frame(header, P9_RLCREATE, 32, |writer| {
            write_rlopen(writer, Rlopen { qid, iounit: 0 });
            Ok(())
        })
    }

    fn set_open(&self, fid: u32, open: FidOpenState) -> FsResult<()> {
        let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
        let entry = state.fids.get_mut(fid).ok_or_else(|| {
            FsError::new(ErrorCode::Ebadf).with_message(format!("EBADF: fid {fid}"))
        })?;
        if entry.open.is_some() {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_message(format!("EBUSY: fid {fid} was opened concurrently")));
        }
        entry.open = Some(open);
        Ok(())
    }

    fn ensure_openable(&self, fid: u32) -> FsResult<()> {
        let state = self.inner.state.lock().expect("9P session mutex poisoned");
        let entry = state.fids.require(fid)?;
        if entry.open.is_some() {
            return Err(FsError::new(ErrorCode::Ebusy)
                .with_message(format!("EBUSY: fid {fid} was opened concurrently")));
        }
        Ok(())
    }

    async fn read_file(
        &self,
        header: P9Header,
        request: Tread,
        budget: usize,
    ) -> FsResult<Vec<u8>> {
        let snapshot = self.require_file(request.fid, "read")?;
        let open = snapshot.open.clone().expect("require_file checked open");
        let size = (request.count as usize).min(budget);
        let mut data = vec![0_u8; size];
        let count = if let Some(handle) = open.handle {
            handle.read(&mut data, Some(request.offset)).await?
        } else {
            let handle = {
                let _guard = self.inner.path_lock.read().await;
                self.inner
                    .driver
                    .open_flags(&snapshot.path, open.flags, 0)
                    .await?
            };
            let count = handle.read(&mut data, Some(request.offset)).await;
            let _ = handle.close().await;
            count?
        };
        data.truncate(count.min(size));
        self.frame(header, P9_RREAD, data.len() + 16, |writer| {
            write_rread(writer, &Rread { data });
            Ok(())
        })
    }

    async fn write_file(&self, header: P9Header, request: Twrite) -> FsResult<Vec<u8>> {
        let snapshot = self.require_file(request.fid, "write")?;
        self.require_writable("write", &snapshot.path)?;
        let open = snapshot.open.clone().expect("require_file checked open");
        let count = if let Some(handle) = open.handle {
            handle.write(&request.data, Some(request.offset)).await?
        } else {
            let handle = {
                let _guard = self.inner.path_lock.read().await;
                self.inner
                    .driver
                    .open_flags(&snapshot.path, open.flags, 0)
                    .await?
            };
            let count = handle.write(&request.data, Some(request.offset)).await;
            let _ = handle.close().await;
            count?
        };
        self.frame(header, P9_RWRITE, 16, |writer| {
            write_rwrite(
                writer,
                Rwrite {
                    count: count.min(u32::MAX as usize) as u32,
                },
            );
            Ok(())
        })
    }

    async fn fsync(&self, header: P9Header, request: Tfsync) -> FsResult<Vec<u8>> {
        let snapshot = self.require_open(request.fid, "fsync")?;
        let open = snapshot.open.clone().expect("require_open checked open");
        if !open.directory {
            if let Some(handle) = open.handle {
                if request.datasync == 0 {
                    handle.sync().await?;
                } else {
                    handle.datasync().await?;
                }
            } else {
                let handle = {
                    let _guard = self.inner.path_lock.read().await;
                    self.inner
                        .driver
                        .open_flags(&snapshot.path, open.flags, 0)
                        .await?
                };
                if request.datasync == 0 {
                    handle.sync().await?;
                } else {
                    handle.datasync().await?;
                }
                let _ = handle.close().await;
            }
        }
        self.frame(header, P9_RFSYNC, 8, |_| Ok(()))
    }

    async fn setattr(&self, header: P9Header, request: Tsetattr) -> FsResult<Vec<u8>> {
        let snapshot = self.fid_snapshot(request.fid)?;
        self.require_writable("setattr", &snapshot.path)?;
        let valid = request.valid;
        if valid & P9_SETATTR_MODE != 0 {
            self.inner
                .driver
                .chmod(&snapshot.path, request.mode & 0o7777)
                .await?;
        }
        if valid & (P9_SETATTR_UID | P9_SETATTR_GID) != 0 {
            let uid = if valid & P9_SETATTR_UID == 0 {
                u32::MAX
            } else {
                request.uid
            };
            let gid = if valid & P9_SETATTR_GID == 0 {
                u32::MAX
            } else {
                request.gid
            };
            match self.inner.driver.lchown(&snapshot.path, uid, gid).await {
                Ok(()) => {}
                Err(error) if error.is(ErrorCode::Enosys) => {
                    self.inner.driver.chown(&snapshot.path, uid, gid).await?
                }
                Err(error) => return Err(error),
            }
        }
        if valid & P9_SETATTR_SIZE != 0 {
            if let Some(handle) = snapshot.open.as_ref().and_then(|open| open.handle.clone()) {
                handle.truncate(request.size).await?;
            } else {
                self.inner
                    .driver
                    .truncate(&snapshot.path, request.size)
                    .await?;
            }
        }
        if valid & (P9_SETATTR_ATIME | P9_SETATTR_MTIME) != 0 {
            let current = if valid & P9_SETATTR_ATIME == 0 || valid & P9_SETATTR_MTIME == 0 {
                Some(self.stat_of(&snapshot.path).await?)
            } else {
                None
            };
            if self.inner.driver.driver.has_utimens() {
                let now_ns = i128::from(mount_rs_core::types::now_ms()) * 1_000_000;
                let atime_ns = if valid & P9_SETATTR_ATIME == 0 {
                    nonnegative_ms_to_ns(current.as_ref().map_or(0, |stats| stats.atime_ms))
                } else if valid & P9_SETATTR_ATIME_SET != 0 {
                    time_to_ns(request.atime)
                } else {
                    now_ns
                };
                let mtime_ns = if valid & P9_SETATTR_MTIME == 0 {
                    nonnegative_ms_to_ns(current.as_ref().map_or(0, |stats| stats.mtime_ms))
                } else if valid & P9_SETATTR_MTIME_SET != 0 {
                    time_to_ns(request.mtime)
                } else {
                    now_ns
                };
                // A fid names the final path component.  Preserve 9P's
                // no-follow metadata semantics when the driver offers the
                // exact timestamp extension.
                self.inner
                    .driver
                    .utimens(&snapshot.path, atime_ns, mtime_ns, false)
                    .await?;
            } else {
                let now = mount_rs_core::types::now_ms();
                let atime = if valid & P9_SETATTR_ATIME == 0 {
                    current.as_ref().map_or(now, |stats| stats.atime_ms)
                } else if valid & P9_SETATTR_ATIME_SET != 0 {
                    time_to_ms(request.atime)
                } else {
                    now
                };
                let mtime = if valid & P9_SETATTR_MTIME == 0 {
                    current.as_ref().map_or(now, |stats| stats.mtime_ms)
                } else if valid & P9_SETATTR_MTIME_SET != 0 {
                    time_to_ms(request.mtime)
                } else {
                    now
                };
                match self
                    .inner
                    .driver
                    .lutimes(&snapshot.path, atime, mtime)
                    .await
                {
                    Ok(()) => {}
                    Err(error) if error.is(ErrorCode::Enosys) => {
                        self.inner
                            .driver
                            .utimes(&snapshot.path, atime, mtime)
                            .await?
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        self.frame(header, P9_RSETATTR, 8, |_| Ok(()))
    }

    async fn mkdir(&self, header: P9Header, request: Tmkdir) -> FsResult<Vec<u8>> {
        let parent = self.fid_snapshot(request.dfid)?;
        self.require_writable("mkdir", &parent.path)?;
        let path = child_of(&parent.path, &request.name, "mkdir")?;
        self.inner
            .driver
            .mkdir(
                &path,
                MkdirOptions {
                    recursive: false,
                    mode: Some(request.mode & 0o7777),
                },
            )
            .await?;
        self.claim(&path, request.dfid, request.gid).await?;
        self.qid_reply(header, P9_RMKDIR, &path).await
    }

    async fn symlink(&self, header: P9Header, request: Tsymlink) -> FsResult<Vec<u8>> {
        let parent = self.fid_snapshot(request.dfid)?;
        self.require_writable("symlink", &parent.path)?;
        let path = child_of(&parent.path, &request.name, "symlink")?;
        self.inner.driver.symlink(&request.symtgt, &path).await?;
        self.claim(&path, request.dfid, request.gid).await?;
        self.qid_reply(header, P9_RSYMLINK, &path).await
    }

    async fn mknod(&self, header: P9Header, request: Tmknod) -> FsResult<Vec<u8>> {
        let parent = self.fid_snapshot(request.dfid)?;
        self.require_writable("mknod", &parent.path)?;
        let path = child_of(&parent.path, &request.name, "mknod")?;
        // The core contract carries one opaque device number. Keep the oracle's
        // protocol packing stable rather than using host libc's makedev layout.
        let dev = ((request.major as u64) << 8) | request.minor as u64;
        match self.inner.driver.mknod(&path, request.mode, dev).await {
            Ok(()) => {}
            Err(error)
                if error.is(ErrorCode::Enosys) && matches!(request.mode & S_IFMT, 0 | S_IFREG) =>
            {
                // A regular mknod is the portable 9P fallback for drivers
                // without a node-creation extension. These are our decoded
                // protocol flags, not host libc constants.
                let flags = OpenFlags::from_bits(u64::from(P9_O_WRONLY | P9_O_CREAT | P9_O_EXCL));
                let handle = self
                    .inner
                    .driver
                    .open_flags(&path, flags, request.mode & 0o7777)
                    .await?;
                let _ = handle.close().await;
            }
            Err(error) => return Err(error),
        }
        self.claim(&path, request.dfid, request.gid).await?;
        self.qid_reply(header, P9_RMKNOD, &path).await
    }

    async fn link(&self, header: P9Header, request: Tlink) -> FsResult<Vec<u8>> {
        let existing = self.fid_snapshot(request.fid)?;
        let parent = self.fid_snapshot(request.dfid)?;
        self.require_writable("link", &parent.path)?;
        let path = child_of(&parent.path, &request.name, "link")?;
        self.inner.driver.link(&existing.path, &path).await?;
        self.frame(header, P9_RLINK, 8, |_| Ok(()))
    }

    async fn readlink(&self, header: P9Header, fid: u32) -> FsResult<Vec<u8>> {
        let snapshot = self.fid_snapshot(fid)?;
        let target = self.inner.driver.readlink(&snapshot.path).await?;
        self.frame(header, P9_RREADLINK, target.len() + 16, |writer| {
            write_rreadlink(writer, &Rreadlink { target })
        })
    }

    async fn qid_reply(&self, header: P9Header, type_: u8, path: &str) -> FsResult<Vec<u8>> {
        let stats = self.stat_of(path).await?;
        let qid = self.qid_for(&stats, path);
        self.frame(header, type_, 32, |writer| {
            write_qid_reply(writer, QidReply { qid });
            Ok(())
        })
    }

    async fn claim(&self, path: &str, fid: u32, gid: u32) -> FsResult<()> {
        if !self.inner.options.claim_ownership {
            return Ok(());
        }
        let uid = self
            .user_for(fid)
            .and_then(|user| user.uid)
            .unwrap_or(u32::MAX);
        let wanted_gid = if gid == NO_NGID { u32::MAX } else { gid };
        if uid == u32::MAX && wanted_gid == u32::MAX {
            return Ok(());
        }
        let (process_uid, process_gid) = current_process_ids();
        if uid == process_uid && wanted_gid == process_gid {
            return Ok(());
        }
        match self.inner.driver.lchown(path, uid, wanted_gid).await {
            Ok(()) => Ok(()),
            Err(error)
                if error.is(ErrorCode::Enosys)
                    || error.is(ErrorCode::Eperm)
                    || error.is(ErrorCode::Enotsup) =>
            {
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn require_writable(&self, syscall: &str, path: &str) -> FsResult<()> {
        if self.inner.options.read_only {
            return Err(FsError::new(ErrorCode::Erofs)
                .with_syscall(syscall)
                .with_path(path));
        }
        Ok(())
    }

    async fn rename(&self, header: P9Header, request: Trename) -> FsResult<Vec<u8>> {
        let source = self.fid_snapshot(request.fid)?;
        let parent = self.fid_snapshot(request.dfid)?;
        self.require_writable("rename", &source.path)?;
        let destination = child_of(&parent.path, &request.name, "rename")?;
        self.inner.driver.rename(&source.path, &destination).await?;
        self.remap_paths(&source.path, &destination);
        self.frame(header, P9_RRENAME, 8, |_| Ok(()))
    }

    async fn renameat(&self, header: P9Header, request: Trenameat) -> FsResult<Vec<u8>> {
        let old_dir = self.fid_snapshot(request.olddirfid)?;
        let new_dir = self.fid_snapshot(request.newdirfid)?;
        self.require_writable("rename", &old_dir.path)?;
        let source = child_of(&old_dir.path, &request.oldname, "rename")?;
        let destination = child_of(&new_dir.path, &request.newname, "rename")?;
        self.inner.driver.rename(&source, &destination).await?;
        self.remap_paths(&source, &destination);
        self.frame(header, P9_RRENAMEAT, 8, |_| Ok(()))
    }

    async fn unlinkat(&self, header: P9Header, request: Tunlinkat) -> FsResult<Vec<u8>> {
        let parent = self.fid_snapshot(request.dirfid)?;
        let directory = request.flags & P9_DOTL_AT_REMOVEDIR != 0;
        let syscall = if directory { "rmdir" } else { "unlink" };
        self.require_writable(syscall, &parent.path)?;
        let path = child_of(&parent.path, &request.name, syscall)?;
        if directory {
            self.inner.driver.rmdir(&path).await?;
        } else {
            self.inner.driver.unlink(&path).await?;
        }
        self.released(&path);
        self.frame(header, P9_RUNLINKAT, 8, |_| Ok(()))
    }

    async fn remove(&self, header: P9Header, fid: u32) -> FsResult<Vec<u8>> {
        // 9P removes the fid before attempting the operation, even when the
        // operation fails. The client has already forgotten it in that case.
        let entry = {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            state.users.remove(&fid);
            state.fids.clunk(fid)?
        };
        self.inner.locks.release_fid(fid);
        if let Some(open) = entry.open
            && let Some(handle) = open.handle
        {
            let _ = handle.close().await;
        }
        self.require_writable("remove", &entry.path)?;
        let stats = self.stat_of(&entry.path).await?;
        if stats.mode & S_IFMT == mount_rs_core::types::S_IFDIR {
            self.inner.driver.rmdir(&entry.path).await?;
        } else {
            self.inner.driver.unlink(&entry.path).await?;
        }
        self.released(&entry.path);
        self.frame(header, P9_RREMOVE, 8, |_| Ok(()))
    }

    fn remap_paths(&self, from: &str, to: &str) {
        let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
        state.fids.remap(from, to);
        drop(state);
        self.inner.locks.renamed(from, to);
    }

    fn released(&self, path: &str) {
        let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
        state.fids.release(path);
        drop(state);
        self.inner.locks.released(path);
    }

    async fn set_lock(&self, header: P9Header, request: Tlock) -> FsResult<Vec<u8>> {
        let snapshot = self.fid_snapshot(request.fid)?;
        let status = self.inner.locks.lock(&P9LockRequest {
            path: snapshot.path,
            fid: request.fid,
            type_: request.type_,
            start: request.start,
            length: request.length,
            proc_id: request.proc_id,
            client_id: request.client_id,
        })?;
        self.frame(header, P9_RLOCK, 16, |writer| {
            write_rlock(writer, Rlock { status });
            Ok(())
        })
    }

    async fn get_lock(&self, header: P9Header, request: Tgetlock) -> FsResult<Vec<u8>> {
        let snapshot = self.fid_snapshot(request.fid)?;
        let query = P9LockRequest {
            path: snapshot.path,
            fid: request.fid,
            type_: request.type_,
            start: request.start,
            length: request.length,
            proc_id: request.proc_id,
            client_id: request.client_id,
        };
        let holder =
            self.inner
                .locks
                .getlock(&query)?
                .unwrap_or_else(|| crate::locks::P9LockHolder {
                    type_: P9_LOCK_TYPE_UNLCK,
                    start: query.start,
                    length: query.length,
                    proc_id: query.proc_id,
                    client_id: query.client_id.clone(),
                });
        self.frame(header, P9_RGETLOCK, 48, |writer| {
            write_rgetlock(
                writer,
                &Rgetlock {
                    type_: holder.type_,
                    start: holder.start,
                    length: holder.length,
                    proc_id: holder.proc_id,
                    client_id: holder.client_id,
                },
            )
        })
    }
}

fn decode_body<T>(
    reader: &mut P9Reader<'_>,
    what: impl Into<String>,
    decode: impl FnOnce(&mut P9Reader<'_>) -> Result<T, P9Error>,
) -> FsResult<T> {
    let what = what.into();
    let value = decode(reader).map_err(|error| {
        FsError::new(ErrorCode::Einval).with_message(format!("EINVAL: undecodable {what}: {error}"))
    })?;
    reader.end(&what).map_err(|error| {
        FsError::new(ErrorCode::Einval).with_message(format!("EINVAL: undecodable {what}: {error}"))
    })?;
    Ok(value)
}

fn split_ms(milliseconds: i64) -> P9Time {
    if milliseconds <= 0 {
        return P9Time { sec: 0, nsec: 0 };
    }
    let seconds = milliseconds as u64 / 1000;
    let nanos = (milliseconds as u64 % 1000) * 1_000_000;
    P9Time {
        sec: seconds,
        nsec: nanos,
    }
}

fn time_to_ms(time: P9Time) -> i64 {
    let nanos = time
        .sec
        .saturating_mul(1_000_000_000)
        .saturating_add(time.nsec);
    (nanos / 1_000_000).min(i64::MAX as u64) as i64
}

fn time_to_ns(time: P9Time) -> i128 {
    i128::from(time.sec) * 1_000_000_000 + i128::from(time.nsec)
}

fn nonnegative_ms_to_ns(milliseconds: i64) -> i128 {
    i128::from(milliseconds.max(0)) * 1_000_000
}

fn write_intent(flags: u32) -> bool {
    flags & P9_O_ACCMODE != 0 || flags & (P9_O_CREAT | P9_O_TRUNC) != 0
}

fn reopen_flags(flags: OpenFlags) -> OpenFlags {
    OpenFlags {
        read: flags.read,
        write: flags.write,
        create: false,
        truncate: false,
        append: flags.append,
        exclusive: false,
    }
}

fn child_of(parent: &str, name: &str, syscall: &str) -> FsResult<String> {
    if name == "." || name == ".." || name.is_empty() || name.contains('/') || name.contains('\0') {
        return Err(FsError::new(ErrorCode::Einval)
            .with_syscall(syscall)
            .with_message(format!("EINVAL: '{name}' is not an entry name")));
    }
    if name.len() > NAME_MAX {
        return Err(FsError::new(ErrorCode::Enametoolong)
            .with_syscall(syscall)
            .with_path(name));
    }
    Ok(join_path(&[parent, name]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mount_rs_memfs::MemoryFs;
    use std::{
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
    };
    use tokio::sync::Notify;
    use tokio::time::{Duration, timeout};

    struct BlockingErrorDriver {
        entered: Arc<Notify>,
        release: Arc<Notify>,
    }

    impl FsDriver for BlockingErrorDriver {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            mount_rs_core::Capabilities::default()
        }

        fn stat<'a, 'b, 'async_trait>(
            &'a self,
            _path: &'b str,
        ) -> Pin<
            Box<
                dyn Future<Output = mount_rs_core::Result<mount_rs_core::Stats>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            let entered = Arc::clone(&self.entered);
            let release = Arc::clone(&self.release);
            Box::pin(async move {
                let release_waiter = release.notified();
                tokio::pin!(release_waiter);
                // Register the release waiter before publishing the entered
                // signal. `Notify::notify_waiters` does not retain a permit
                // for a waiter that has not registered yet.
                release_waiter.as_mut().enable();
                entered.notify_one();
                release_waiter.await;
                Err(FsError::new(ErrorCode::Ebadf))
            })
        }

        fn readdir<'a, 'b, 'async_trait>(
            &'a self,
            _path: &'b str,
        ) -> Pin<
            Box<
                dyn Future<Output = mount_rs_core::Result<Vec<mount_rs_core::DirEntry>>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async { Err(FsError::new(ErrorCode::Enotsup)) })
        }

        fn open<'a, 'b, 'c, 'async_trait>(
            &'a self,
            _path: &'b str,
            _flags: &'c str,
            _mode: u32,
        ) -> Pin<
            Box<
                dyn Future<Output = mount_rs_core::Result<Arc<dyn mount_rs_core::FileHandle>>>
                    + Send
                    + 'async_trait,
            >,
        >
        where
            'a: 'async_trait,
            'b: 'async_trait,
            'c: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async { Err(FsError::new(ErrorCode::Enotsup)) })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn destroy_overrides_late_driver_error_for_inflight_call() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let session = P9Session::new(BlockingErrorDriver {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        session.inner.msize.store(8192, Ordering::SeqCst);
        session
            .fid_create(1, "/")
            .expect("create a direct-session fid");
        let request = encode_message(P9_TGETATTR, 7, 256, |writer| {
            write_tgetattr(
                writer,
                Tgetattr {
                    fid: 1,
                    request_mask: P9_GETATTR_BASIC,
                },
            );
            Ok(())
        })
        .expect("Tgetattr encodes");
        let task_session = session.clone();
        let task = tokio::spawn(async move {
            task_session
                .handle_call(&request)
                .await
                .expect("in-flight Tgetattr gets a response")
        });

        timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("Tgetattr reaches the blocking driver");
        assert_eq!(session.inflight(), 1);
        session.destroy().await;
        assert!(session.destroyed());
        assert_eq!(session.inflight(), 0);
        release.notify_waiters();

        let response = timeout(Duration::from_secs(1), task)
            .await
            .expect("destroyed Tgetattr completes")
            .expect("Tgetattr task joins");
        let (header, mut body) = decode_message(&response).expect("stale response decodes");
        assert_eq!(header.type_, P9_RLERROR);
        assert_eq!(read_rlerror(&mut body).expect("Rlerror decodes").ecode, 19);
    }

    #[tokio::test]
    async fn destroy_releases_flush_waiters_and_clears_inflight() {
        let session = P9Session::new(MemoryFs::empty());
        session.inner.msize.store(8192, Ordering::SeqCst);
        assert!(session.inner.inflight.register(41, &Pending::new()));
        let request = encode_message(P9_TFLUSH, 42, 256, |writer| {
            write_tflush(writer, Tflush { oldtag: 41 });
            Ok(())
        })
        .expect("Tflush encodes");
        let task_session = session.clone();
        let task = tokio::spawn(async move {
            task_session
                .handle_call(&request)
                .await
                .expect("Tflush gets a response")
        });

        timeout(Duration::from_secs(1), async {
            loop {
                if session.stats().flushed == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Tflush reaches its wait point");

        session.destroy().await;
        let response = timeout(Duration::from_secs(1), task)
            .await
            .expect("destroy wakes Tflush")
            .expect("Tflush task joins");
        let (header, _) = decode_message(&response).expect("destroyed-session response decodes");
        assert_eq!(header.type_, P9_RLERROR);
        assert_eq!(session.inflight(), 0);
    }

    #[tokio::test]
    async fn debug_duplicate_tag_records_assertion_and_reports_error() {
        let assertions = Arc::new(Mutex::new(Vec::new()));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let assertion_sink = Arc::clone(&assertions);
        let error_sink = Arc::clone(&errors);
        let session = P9Session::with_options_and_hooks(
            Arc::new(MemoryFs::empty()),
            P9SessionOptions {
                debug: true,
                ..P9SessionOptions::default()
            },
            P9SessionHooks {
                on_error: Some(Arc::new(move |error, header| {
                    error_sink
                        .lock()
                        .expect("error sink mutex")
                        .push((error.code, header.map(|header| (header.type_, header.tag))));
                })),
                on_assertion: Some(Arc::new(move |message| {
                    assertion_sink
                        .lock()
                        .expect("assertion sink mutex")
                        .push(message);
                })),
            },
        );
        assert!(session.inner.inflight.register(7, &Pending::new()));
        let request = encode_message(P9_TCLUNK, 7, 256, |writer| {
            write_fid_request(writer, FidRequest { fid: 1 });
            Ok(())
        })
        .expect("Tclunk encodes");

        let response = session
            .handle_call(&request)
            .await
            .expect("duplicate tag gets an error reply");
        let (header, _) = decode_message(&response).expect("error response decodes");
        assert_eq!(header.type_, P9_RLERROR);
        assert_eq!(session.stats().assertions, 1);
        assert_eq!(session.assertions().len(), 1);
        assert_eq!(assertions.lock().expect("assertion sink mutex").len(), 1);
        assert_eq!(
            errors.lock().expect("error sink mutex").as_slice(),
            &[(ErrorCode::Eproto, Some((P9_TCLUNK, 7)),)]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn overlapping_calls_with_the_same_tag_reject_the_second_call() {
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let session = P9Session::new(BlockingErrorDriver {
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        });
        let version = encode_message(P9_TVERSION, P9_NOTAG, 32, |writer| {
            write_tversion(
                writer,
                &Tversion {
                    msize: 8192,
                    version: P9_VERSION_DOTL.to_owned(),
                },
            )
        })
        .unwrap();
        let version_reply = session.handle_call(&version).await.unwrap();
        assert_eq!(decode_message(&version_reply).unwrap().0.type_, P9_RVERSION);
        session.fid_create(1, "/").unwrap();
        let request = encode_message(P9_TGETATTR, 7, 32, |writer| {
            write_tgetattr(
                writer,
                Tgetattr {
                    fid: 1,
                    request_mask: P9_GETATTR_BASIC,
                },
            );
            Ok(())
        })
        .unwrap();
        let first_session = session.clone();
        let first_request = request.clone();
        let first = tokio::spawn(async move { first_session.handle_call(&first_request).await });
        timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("first call reaches the blocked driver");
        assert_eq!(session.inflight(), 1);

        let second_reply = session.handle_call(&request).await.unwrap();
        assert_eq!(decode_message(&second_reply).unwrap().0.type_, P9_RLERROR);
        assert_eq!(session.inflight(), 1);
        assert_eq!(session.stats().assertions, 1);
        assert!(session.assertions()[0].contains("tag 7"));

        release.notify_waiters();
        let first_reply = timeout(Duration::from_secs(1), first)
            .await
            .expect("first call resumes")
            .expect("first task joins")
            .expect("first call receives a reply");
        assert_eq!(decode_message(&first_reply).unwrap().0.type_, P9_RLERROR);
        assert_eq!(session.inflight(), 0);
    }

    /// However many calls claim one tag at once, one of them gets it, and
    /// the tag is free again only when that call completes.
    #[test]
    fn a_tag_is_claimed_by_one_call_at_a_time() {
        const THREADS: usize = 8;
        const ROUNDS: usize = 2_000;
        let inflight = Inflight::new();
        let holders = std::sync::atomic::AtomicUsize::new(0);
        let claimed = std::sync::atomic::AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(THREADS);
        std::thread::scope(|scope| {
            for thread in 0..THREADS {
                let (inflight, holders, claimed, barrier) =
                    (&inflight, &holders, &claimed, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    for round in 0..ROUNDS {
                        // A tag every thread fights over, and one of its own
                        // on another page.
                        let own = 0x4000 + (thread as u16) * 0x100 + (round % 256) as u16;
                        let pending = Pending::new();
                        assert!(inflight.register(own, &pending));
                        assert!(!inflight.register(own, &Pending::new()));
                        if inflight.register(7, &pending) {
                            assert_eq!(
                                holders.fetch_add(1, Ordering::SeqCst),
                                0,
                                "two calls hold tag 7"
                            );
                            claimed.fetch_add(1, Ordering::Relaxed);
                            assert!(Arc::ptr_eq(&inflight.get(7).unwrap(), &pending));
                            // A call that does not hold the tag cannot free it.
                            inflight.complete(7, &Pending::new());
                            assert!(inflight.get(7).is_some());
                            holders.fetch_sub(1, Ordering::SeqCst);
                            inflight.complete(7, &pending);
                        }
                        inflight.complete(own, &pending);
                        assert!(inflight.get(own).is_none());
                    }
                });
            }
        });
        assert!(claimed.load(Ordering::Relaxed) > 0);
        assert_eq!(inflight.len(), 0);
    }

    /// A clear racing claims hands back each call it removed exactly once
    /// and never the kept tag; whatever is left afterwards is still claimed.
    #[test]
    fn clearing_returns_each_removed_call_once() {
        const THREADS: usize = 6;
        const PER_THREAD: usize = 2_000;
        let inflight = Inflight::new();
        let kept = Pending::new();
        assert!(inflight.register(P9_NOTAG, &kept));
        let done = AtomicBool::new(false);
        let barrier = std::sync::Barrier::new(THREADS + 1);
        let (registered, drained) = std::thread::scope(|scope| {
            let claimers = (0..THREADS)
                .map(|thread| {
                    let (inflight, barrier) = (&inflight, &barrier);
                    scope.spawn(move || {
                        barrier.wait();
                        let mut mine = Vec::new();
                        for index in 0..PER_THREAD {
                            let tag = (thread * PER_THREAD + index) as u16;
                            let pending = Pending::new();
                            assert!(inflight.register(tag, &pending), "tags are distinct");
                            mine.push((tag, pending));
                        }
                        mine
                    })
                })
                .collect::<Vec<_>>();
            let sweeper = {
                let (inflight, barrier, done) = (&inflight, &barrier, &done);
                scope.spawn(move || {
                    barrier.wait();
                    let mut drained = Vec::new();
                    while !done.load(Ordering::Acquire) {
                        drained.extend(inflight.drain_except(Some(P9_NOTAG)));
                    }
                    drained
                })
            };
            let registered = claimers
                .into_iter()
                .flat_map(|claimer| claimer.join().unwrap())
                .collect::<Vec<_>>();
            done.store(true, Ordering::Release);
            (registered, sweeper.join().unwrap())
        });
        assert!(!drained.iter().any(|pending| Arc::ptr_eq(pending, &kept)));
        let mut remaining = 0;
        for (tag, pending) in &registered {
            let times = drained
                .iter()
                .filter(|other| Arc::ptr_eq(other, pending))
                .count();
            let still_claimed = inflight
                .get(*tag)
                .is_some_and(|current| Arc::ptr_eq(&current, pending));
            assert_eq!(
                times + usize::from(still_claimed),
                1,
                "a call is either removed once or still in flight"
            );
            remaining += usize::from(still_claimed);
        }
        assert_eq!(drained.len() + remaining, registered.len());
        assert_eq!(inflight.len(), remaining + 1);
        let rest = inflight.drain_except(None);
        assert_eq!(rest.len(), remaining + 1);
        assert_eq!(inflight.len(), 0);
    }

    /// Concurrent calls that reuse a tag still in flight are all refused,
    /// and the assertion hook runs with no session cell held: it reads the
    /// session back, which would never return if one were.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_duplicate_tags_are_all_refused_and_the_hook_may_reenter() {
        const DUPLICATES: usize = 32;
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let reentered = Arc::new(Mutex::new(Vec::new()));
        let hook_session = Arc::new(std::sync::OnceLock::<P9Session>::new());
        let (sink, hook_target) = (Arc::clone(&reentered), Arc::clone(&hook_session));
        let session = P9Session::with_options_and_hooks(
            Arc::new(BlockingErrorDriver {
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
            }),
            P9SessionOptions {
                debug: true,
                ..P9SessionOptions::default()
            },
            P9SessionHooks {
                on_error: None,
                on_assertion: Some(Arc::new(move |_| {
                    let session = hook_target.get().expect("hook session");
                    sink.lock().expect("hook sink").push((
                        session.inflight(),
                        session.generation(),
                        session.user_for(1).is_none(),
                    ));
                })),
            },
        );
        assert!(hook_session.set(session.clone()).is_ok());
        session.inner.msize.store(8192, Ordering::SeqCst);
        session.fid_create(1, "/").unwrap();
        let request = encode_message(P9_TGETATTR, 7, 32, |writer| {
            write_tgetattr(
                writer,
                Tgetattr {
                    fid: 1,
                    request_mask: P9_GETATTR_BASIC,
                },
            );
            Ok(())
        })
        .unwrap();
        let (first_session, first_request) = (session.clone(), request.clone());
        let first = tokio::spawn(async move { first_session.handle_call(&first_request).await });
        timeout(Duration::from_secs(1), entered.notified())
            .await
            .expect("first call reaches the blocked driver");

        let duplicates = (0..DUPLICATES)
            .map(|_| {
                let (session, request) = (session.clone(), request.clone());
                tokio::spawn(async move { session.handle_call(&request).await.unwrap() })
            })
            .collect::<Vec<_>>();
        for duplicate in duplicates {
            let reply = timeout(Duration::from_secs(5), duplicate)
                .await
                .expect("a refused duplicate returns")
                .unwrap();
            assert_eq!(decode_message(&reply).unwrap().0.type_, P9_RLERROR);
        }
        assert_eq!(session.inflight(), 1, "only the first call holds the tag");
        assert_eq!(session.stats().assertions, DUPLICATES as u64);
        let reentered = reentered.lock().expect("hook sink").clone();
        assert_eq!(reentered.len(), DUPLICATES);
        assert!(reentered.iter().all(|seen| *seen == (1, 0, true)));

        release.notify_waiters();
        timeout(Duration::from_secs(1), first)
            .await
            .expect("first call resumes")
            .unwrap()
            .unwrap();
        assert_eq!(session.inflight(), 0);
        // The tag is free again.
        let clunk = encode_message(P9_TCLUNK, 7, 32, |writer| {
            write_fid_request(writer, FidRequest { fid: 1 });
            Ok(())
        })
        .unwrap();
        let reply = session.handle_call(&clunk).await.unwrap();
        assert_eq!(decode_message(&reply).unwrap().0.type_, P9_RCLUNK);
    }

    type DriverFuture<'t, T> = Pin<Box<dyn Future<Output = FsResult<T>> + Send + 't>>;

    /// A memory filesystem with the hooks the race tests need: it counts
    /// the handles it opens and the ones that are closed, can hold every
    /// `lchown` until two callers are inside it, and can hold a `rename`
    /// after the entry has moved.
    struct RaceDriver {
        inner: MemoryFs,
        opens: AtomicU64,
        closes: Arc<AtomicU64>,
        lchown_barrier: Option<tokio::sync::Barrier>,
        hold_rename: bool,
        rename_done: Notify,
        rename_release: Notify,
        removed: Mutex<Vec<String>>,
        /// A path whose `lstat` is held after it has answered.
        hold_lstat: Option<String>,
        lstat_done: Notify,
        lstat_release: Notify,
    }

    impl RaceDriver {
        fn new(inner: MemoryFs) -> Self {
            Self {
                inner,
                opens: AtomicU64::new(0),
                closes: Arc::new(AtomicU64::new(0)),
                lchown_barrier: None,
                hold_rename: false,
                rename_done: Notify::new(),
                rename_release: Notify::new(),
                removed: Mutex::new(Vec::new()),
                hold_lstat: None,
                lstat_done: Notify::new(),
                lstat_release: Notify::new(),
            }
        }

        fn counted(&self, handle: Arc<dyn mount_rs_core::FileHandle>) -> CountedHandle {
            self.opens.fetch_add(1, Ordering::SeqCst);
            CountedHandle {
                inner: handle,
                closes: Arc::clone(&self.closes),
            }
        }
    }

    struct CountedHandle {
        inner: Arc<dyn mount_rs_core::FileHandle>,
        closes: Arc<AtomicU64>,
    }

    impl mount_rs_core::FileHandle for CountedHandle {
        fn read<'a, 'b, 'async_trait>(
            &'a self,
            buffer: &'b mut [u8],
            position: Option<u64>,
        ) -> DriverFuture<'async_trait, usize>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.inner.read(buffer, position).await })
        }

        fn write<'a, 'b, 'async_trait>(
            &'a self,
            buffer: &'b [u8],
            position: Option<u64>,
        ) -> DriverFuture<'async_trait, usize>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.inner.write(buffer, position).await })
        }

        fn stat<'a, 'async_trait>(&'a self) -> DriverFuture<'async_trait, Stats>
        where
            'a: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.inner.stat().await })
        }

        fn truncate<'a, 'async_trait>(&'a self, length: u64) -> DriverFuture<'async_trait, ()>
        where
            'a: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.inner.truncate(length).await })
        }

        fn close<'a, 'async_trait>(&'a self) -> DriverFuture<'async_trait, ()>
        where
            'a: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.closes.fetch_add(1, Ordering::SeqCst);
                self.inner.close().await
            })
        }
    }

    impl FsDriver for RaceDriver {
        fn capabilities(&self) -> mount_rs_core::Capabilities {
            self.inner.capabilities()
        }

        fn stat<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> DriverFuture<'async_trait, Stats>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.inner.stat(path).await })
        }

        fn lstat<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> DriverFuture<'async_trait, Stats>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let stats = self.inner.lstat(path).await?;
                if self.hold_lstat.as_deref() == Some(path) {
                    self.lstat_done.notify_one();
                    self.lstat_release.notified().await;
                }
                Ok(stats)
            })
        }

        fn readdir<'a, 'b, 'async_trait>(
            &'a self,
            path: &'b str,
        ) -> DriverFuture<'async_trait, Vec<mount_rs_core::DirEntry>>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move { self.inner.readdir(path).await })
        }

        fn open<'a, 'b, 'c, 'async_trait>(
            &'a self,
            path: &'b str,
            flags: &'c str,
            mode: u32,
        ) -> DriverFuture<'async_trait, Arc<dyn mount_rs_core::FileHandle>>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            'c: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let handle = self.inner.open(path, flags, mode).await?;
                Ok(Arc::new(self.counted(handle)) as Arc<dyn mount_rs_core::FileHandle>)
            })
        }

        fn open_flags<'a, 'b, 'async_trait>(
            &'a self,
            path: &'b str,
            flags: OpenFlags,
            mode: u32,
        ) -> DriverFuture<'async_trait, Arc<dyn mount_rs_core::FileHandle>>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                let handle = self.inner.open_flags(path, flags, mode).await?;
                Ok(Arc::new(self.counted(handle)) as Arc<dyn mount_rs_core::FileHandle>)
            })
        }

        fn lchown<'a, 'b, 'async_trait>(
            &'a self,
            _path: &'b str,
            _uid: u32,
            _gid: u32,
        ) -> DriverFuture<'async_trait, ()>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                if let Some(barrier) = &self.lchown_barrier {
                    barrier.wait().await;
                }
                Ok(())
            })
        }

        fn rename<'a, 'b, 'c, 'async_trait>(
            &'a self,
            old_path: &'b str,
            new_path: &'c str,
        ) -> DriverFuture<'async_trait, ()>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            'c: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.inner.rename(old_path, new_path).await?;
                if self.hold_rename {
                    // `notify_one` keeps a permit, so neither side can miss
                    // the other's signal.
                    self.rename_done.notify_one();
                    self.rename_release.notified().await;
                }
                Ok(())
            })
        }

        fn unlink<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> DriverFuture<'async_trait, ()>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.removed.lock().expect("removed").push(path.to_owned());
                self.inner.unlink(path).await
            })
        }

        fn rmdir<'a, 'b, 'async_trait>(&'a self, path: &'b str) -> DriverFuture<'async_trait, ()>
        where
            'a: 'async_trait,
            'b: 'async_trait,
            Self: 'async_trait,
        {
            Box::pin(async move {
                self.removed.lock().expect("removed").push(path.to_owned());
                self.inner.rmdir(path).await
            })
        }
    }

    fn fid_message(type_: u8, tag: u16, fid: u32) -> Vec<u8> {
        encode_message(type_, tag, 32, |writer| {
            write_fid_request(writer, FidRequest { fid });
            Ok(())
        })
        .expect("fid request encodes")
    }

    /// Two Tlcreate on one fid both pass the "not open yet" check and then
    /// both wait in the driver. Only one may store its handle in the fid;
    /// the other must be refused and must close the handle it opened, or
    /// the backend keeps that handle for as long as the process runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_lcreate_on_one_fid_stores_one_handle_and_closes_the_other() {
        let mut driver = RaceDriver::new(MemoryFs::empty());
        driver.lchown_barrier = Some(tokio::sync::Barrier::new(2));
        let driver = Arc::new(driver);
        let session = P9Session::from_arc(Arc::clone(&driver) as Arc<dyn FsDriver>);
        session.inner.msize.store(8192, Ordering::SeqCst);
        session.fid_create(1, "/").unwrap();
        // An attached user whose ids are not this process's, so that claiming
        // the new file calls the driver's `lchown`: the await between the
        // check and the store.
        let (process_uid, _) = current_process_ids();
        session.inner.state.lock().unwrap().users.insert(
            1,
            P9User {
                uname: "other".to_owned(),
                uid: Some(process_uid.wrapping_add(1) & 0x7fff_ffff),
                aname: String::new(),
            },
        );

        let tasks = [11u16, 12].map(|tag| {
            let request = encode_message(P9_TLCREATE, tag, 64, |writer| {
                write_tlcreate(
                    writer,
                    &Tlcreate {
                        fid: 1,
                        name: "created".to_owned(),
                        flags: P9_O_RDWR,
                        mode: 0o644,
                        gid: NO_NGID,
                    },
                )
            })
            .unwrap();
            let session = session.clone();
            tokio::spawn(async move { session.handle_call(&request).await.unwrap() })
        });
        let mut created = 0;
        let mut errors = Vec::new();
        for task in tasks {
            let reply = timeout(Duration::from_secs(5), task)
                .await
                .expect("both creates reach the driver together and return")
                .unwrap();
            let (header, mut body) = decode_message(&reply).unwrap();
            match header.type_ {
                P9_RLCREATE => created += 1,
                P9_RLERROR => errors.push(read_rlerror(&mut body).unwrap().ecode),
                other => panic!("unexpected reply {}", message_name(other)),
            }
        }
        assert_eq!(created, 1, "exactly one create owns the fid");
        assert_eq!(errors, vec![16], "the other is refused with EBUSY");

        assert_eq!(driver.opens.load(Ordering::SeqCst), 2);
        assert_eq!(
            driver.closes.load(Ordering::SeqCst),
            1,
            "the handle that was not stored in the fid is closed"
        );
        let stored = session.fid_snapshot(1).unwrap();
        assert_eq!(stored.path, "/created");
        assert!(stored.open.is_some_and(|open| open.handle.is_some()));

        let reply = session
            .handle_call(&fid_message(P9_TCLUNK, 13, 1))
            .await
            .unwrap();
        assert_eq!(decode_message(&reply).unwrap().0.type_, P9_RCLUNK);
        assert_eq!(
            driver.closes.load(Ordering::SeqCst),
            2,
            "clunking the fid closes the stored handle: nothing is left open"
        );
    }

    /// A Tremove that arrives while a rename of the file's parent directory
    /// is in the driver waits for the rename, and then removes the file
    /// under its new name. Without the path gate it would read the fid's
    /// old path and stat a name that no longer exists.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn remove_waits_for_a_rename_of_its_parent_and_removes_the_new_path() {
        let memory = MemoryFs::empty();
        memory.mkdir("/dir", MkdirOptions::default()).await.unwrap();
        memory.write_file("/dir/file", b"content").await.unwrap();
        let mut driver = RaceDriver::new(memory);
        driver.hold_rename = true;
        let driver = Arc::new(driver);
        let session = P9Session::from_arc(Arc::clone(&driver) as Arc<dyn FsDriver>);
        session.inner.msize.store(8192, Ordering::SeqCst);
        session.fid_create(1, "/").unwrap();
        session.fid_create(2, "/dir").unwrap();
        session.fid_create(3, "/dir/file").unwrap();

        let rename = encode_message(P9_TRENAME, 21, 64, |writer| {
            write_trename(
                writer,
                &Trename {
                    fid: 2,
                    dfid: 1,
                    name: "moved".to_owned(),
                },
            )
        })
        .unwrap();
        let rename_session = session.clone();
        let rename =
            tokio::spawn(async move { rename_session.handle_call(&rename).await.unwrap() });
        timeout(Duration::from_secs(5), driver.rename_done.notified())
            .await
            .expect("the rename moves the directory and stops in the driver");

        let remove_session = session.clone();
        let mut remove = tokio::spawn(async move {
            remove_session
                .handle_call(&fid_message(P9_TREMOVE, 22, 3))
                .await
                .unwrap()
        });
        assert!(
            timeout(Duration::from_millis(300), &mut remove)
                .await
                .is_err(),
            "the remove must not run while the rename holds the path gate"
        );
        assert!(
            driver.removed.lock().unwrap().is_empty(),
            "nothing is removed before the rename finishes"
        );
        assert!(session.has_fid(3), "the fid is not clunked early either");

        driver.rename_release.notify_one();
        let reply = timeout(Duration::from_secs(5), rename)
            .await
            .expect("the rename finishes")
            .unwrap();
        assert_eq!(decode_message(&reply).unwrap().0.type_, P9_RRENAME);
        let reply = timeout(Duration::from_secs(5), remove)
            .await
            .expect("the remove runs after the rename")
            .unwrap();
        assert_eq!(decode_message(&reply).unwrap().0.type_, P9_RREMOVE);

        assert_eq!(*driver.removed.lock().unwrap(), vec!["/moved/file"]);
        assert!(driver.inner.stat("/moved/file").await.is_err());
        assert!(driver.inner.stat("/moved").await.is_ok());
        assert!(!session.has_fid(3));
    }

    /// A Twalk has read its fid's path and found the child when a rename of
    /// that directory arrives. The rename waits for the walk, so the new fid
    /// is stored first and then renamed with every other fid. Without the
    /// path gate the rename would finish first and the walk would then store
    /// a path that no longer exists.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn rename_waits_for_a_walk_under_the_renamed_directory() {
        let memory = MemoryFs::empty();
        memory.mkdir("/a", MkdirOptions::default()).await.unwrap();
        memory.write_file("/a/x", b"content").await.unwrap();
        let mut driver = RaceDriver::new(memory);
        driver.hold_lstat = Some("/a/x".to_owned());
        let driver = Arc::new(driver);
        let session = P9Session::from_arc(Arc::clone(&driver) as Arc<dyn FsDriver>);
        session.inner.msize.store(8192, Ordering::SeqCst);
        session.fid_create(1, "/").unwrap();
        session.fid_create(2, "/a").unwrap();

        let walk = encode_message(P9_TWALK, 31, 64, |writer| {
            write_twalk(
                writer,
                &Twalk {
                    fid: 2,
                    newfid: 3,
                    wnames: vec!["x".to_owned()],
                },
            )
        })
        .unwrap();
        let walk_session = session.clone();
        let walk = tokio::spawn(async move { walk_session.handle_call(&walk).await.unwrap() });
        timeout(Duration::from_secs(5), driver.lstat_done.notified())
            .await
            .expect("the walk finds /a/x and stops in the driver");

        let rename = encode_message(P9_TRENAME, 32, 64, |writer| {
            write_trename(
                writer,
                &Trename {
                    fid: 2,
                    dfid: 1,
                    name: "b".to_owned(),
                },
            )
        })
        .unwrap();
        let rename_session = session.clone();
        let mut rename =
            tokio::spawn(async move { rename_session.handle_call(&rename).await.unwrap() });
        let early = timeout(Duration::from_millis(300), &mut rename).await;

        driver.lstat_release.notify_one();
        let reply = timeout(Duration::from_secs(5), walk)
            .await
            .expect("the walk finishes")
            .unwrap();
        assert_eq!(decode_message(&reply).unwrap().0.type_, P9_RWALK);
        let reply = match early {
            Ok(reply) => reply.unwrap(),
            Err(_) => timeout(Duration::from_secs(5), rename)
                .await
                .expect("the rename runs after the walk")
                .unwrap(),
        };
        assert_eq!(decode_message(&reply).unwrap().0.type_, P9_RRENAME);

        assert!(driver.inner.stat("/b/x").await.is_ok());
        assert_eq!(session.fid_snapshot(2).unwrap().path, "/b");
        assert_eq!(
            session.fid_snapshot(3).unwrap().path,
            "/b/x",
            "the walked fid names a path the rename removed"
        );
    }

    /// Many threads try to open the same fids. `set_open` checks that the
    /// fid is not open and stores the open state in one section of the
    /// session state, so each fid is opened by exactly one caller. This is
    /// a stress loop: the section is synchronous and has no seam to park in.
    #[test]
    fn concurrent_set_open_admits_one_caller_per_fid() {
        const THREADS: usize = 4;
        const FIDS: u32 = 20_000;
        const BATCH: u32 = 50;

        let session = P9Session::new(MemoryFs::empty());
        for fid in 0..FIDS {
            session.fid_create(fid, "/").unwrap();
        }
        let barrier = std::sync::Barrier::new(THREADS);
        let wins = AtomicU64::new(0);
        std::thread::scope(|scope| {
            for _ in 0..THREADS {
                scope.spawn(|| {
                    for batch in 0..FIDS / BATCH {
                        barrier.wait();
                        for fid in batch * BATCH..(batch + 1) * BATCH {
                            let open = FidOpenState {
                                flags: reopen_flags(OpenFlags::from_bits(0)),
                                wire_flags: 0,
                                handle: None,
                                directory: true,
                                qid: None,
                            };
                            if session.set_open(fid, open).is_ok() {
                                wins.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                });
            }
        });
        assert_eq!(
            wins.load(Ordering::Relaxed),
            u64::from(FIDS),
            "a fid was opened by two callers"
        );
    }
}
