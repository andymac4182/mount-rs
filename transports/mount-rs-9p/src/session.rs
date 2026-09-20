//! A stateful 9P2000.L session over the core filesystem driver.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use mount_rs_core::path::join_path;
use mount_rs_core::{
    ErrorCode, FsDriver, FsError, Loopback, MkdirOptions, OpenFlags, Result as FsResult, S_IFMT,
    S_IFREG, Stats,
};
use tokio::sync::{Notify, RwLock};

use crate::constants::*;
use crate::fids::{FidOpenState, FidTable, qid_version, walk_step};
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

#[derive(Clone)]
pub struct P9SessionOptions {
    /// Server-side ceiling for version negotiation. `None` uses 1 MiB.
    pub msize: Option<u32>,
    pub use_driver_ino: bool,
    pub read_only: bool,
    pub claim_ownership: bool,
    pub locks: Option<P9LockTable>,
}

impl Default for P9SessionOptions {
    fn default() -> Self {
        Self {
            msize: None,
            use_driver_ino: true,
            read_only: false,
            claim_ownership: true,
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
    pub messages: HashMap<String, u64>,
}

struct SessionState {
    msize: Option<u32>,
    generation: u64,
    destroyed: bool,
    fids: FidTable,
    users: HashMap<u32, P9User>,
}

struct Pending {
    result: tokio::sync::Mutex<Option<Vec<u8>>>,
    notify: Notify,
}

struct SessionInner {
    driver: Loopback,
    options: P9SessionOptions,
    state: Mutex<SessionState>,
    inflight: Mutex<HashMap<u16, Arc<Pending>>>,
    path_lock: RwLock<()>,
    locks: P9LockClient,
    stats: Mutex<P9SessionStats>,
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
        let locks = options
            .locks
            .clone()
            .unwrap_or_else(|| P9LockTable::new(Default::default()))
            .client();
        Self {
            inner: Arc::new(SessionInner {
                driver: Loopback::from_arc(driver),
                state: Mutex::new(SessionState {
                    msize: None,
                    generation: 0,
                    destroyed: false,
                    fids: FidTable::new(options.use_driver_ino),
                    users: HashMap::new(),
                }),
                options,
                inflight: Mutex::new(HashMap::new()),
                path_lock: RwLock::new(()),
                locks,
                stats: Mutex::new(P9SessionStats::default()),
            }),
        }
    }

    pub fn msize(&self) -> Option<u32> {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .msize
    }

    pub fn version(&self) -> Option<&'static str> {
        self.msize().map(|_| P9_VERSION_DOTL)
    }

    pub fn generation(&self) -> u64 {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .generation
    }

    pub fn destroyed(&self) -> bool {
        self.inner
            .state
            .lock()
            .expect("9P session mutex poisoned")
            .destroyed
    }

    pub fn inflight(&self) -> usize {
        self.inner
            .inflight
            .lock()
            .expect("9P session mutex poisoned")
            .len()
    }

    pub fn stats(&self) -> P9SessionStats {
        self.inner
            .stats
            .lock()
            .expect("9P session mutex poisoned")
            .clone()
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

    /// Answer one complete frame. Malformed framing has no trustworthy tag and
    /// therefore returns `None`; malformed bodies are addressed with `Rlerror`.
    pub async fn handle_call(&self, bytes: &[u8]) -> Option<Vec<u8>> {
        self.count_request(None);
        let (header, body) = match decode_message(bytes) {
            Ok(value) => value,
            Err(_error) => {
                self.count_dropped();
                return None;
            }
        };
        self.count_message(header.type_);

        if header.type_ == P9_TFLUSH {
            let reply = self.run(header, body, self.generation()).await;
            return Some(reply);
        }

        let pending = Arc::new(Pending {
            result: tokio::sync::Mutex::new(None),
            notify: Notify::new(),
        });
        {
            let mut inflight = self
                .inner
                .inflight
                .lock()
                .expect("9P session mutex poisoned");
            if inflight.contains_key(&header.tag) {
                return Some(
                    self.error_reply(
                        header,
                        FsError::new(ErrorCode::Eproto).with_message(format!(
                            "EPROTO: tag {} is already in flight",
                            header.tag
                        )),
                    ),
                );
            }
            inflight.insert(header.tag, Arc::clone(&pending));
        }

        let generation = self.generation();
        let reply = self.run(header, body, generation).await;
        {
            let mut result = pending.result.lock().await;
            *result = Some(reply.clone());
        }
        pending.notify.notify_waiters();
        self.inner
            .inflight
            .lock()
            .expect("9P session mutex poisoned")
            .remove(&header.tag);
        Some(reply)
    }

    pub async fn destroy(&self) {
        let handles = {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            if state.destroyed {
                return;
            }
            state.destroyed = true;
            state.generation = state.generation.wrapping_add(1);
            let handles = state.fids.open_handles();
            state.fids.clear();
            state.users.clear();
            state.msize = None;
            handles
        };
        self.inner.locks.release_all();
        for (_, handle) in handles {
            let _ = handle.close().await;
        }
    }

    async fn run(&self, header: P9Header, body: P9Reader<'_>, generation: u64) -> Vec<u8> {
        let result = self.dispatch(header, body).await.and_then(|reply| {
            if header.type_ != P9_TVERSION && self.generation() != generation {
                Err(if self.destroyed() {
                    FsError::new(ErrorCode::Enodev)
                } else {
                    FsError::new(ErrorCode::Eio)
                })
            } else {
                Ok(reply)
            }
        });
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
        self.count_reply(true);
        self.frame(header, P9_RLERROR, 16, |writer| {
            write_rlerror(
                writer,
                Rlerror {
                    ecode: error.code.errno().unsigned_abs(),
                },
            );
            Ok(())
        })
        .unwrap_or_else(|_| {
            // Rlerror is fixed-width and cannot fail in practice. Keep the
            // final fallback total if an internal invariant is violated.
            let mut bytes = Vec::with_capacity(11);
            bytes.extend_from_slice(&11_u32.to_le_bytes());
            bytes.push(P9_RLERROR);
            bytes.extend_from_slice(&header.tag.to_le_bytes());
            bytes.extend_from_slice(&(ErrorCode::Eio.errno() as u32).to_le_bytes());
            bytes
        })
    }

    fn count_request(&self, _type_: Option<u8>) {
        self.inner
            .stats
            .lock()
            .expect("9P stats mutex poisoned")
            .requests += 1;
    }
    fn count_dropped(&self) {
        self.inner
            .stats
            .lock()
            .expect("9P stats mutex poisoned")
            .dropped += 1;
    }
    fn count_message(&self, type_: u8) {
        let mut stats = self.inner.stats.lock().expect("9P stats mutex poisoned");
        *stats.messages.entry(message_name(type_)).or_default() += 1;
    }
    fn count_reply(&self, error: bool) {
        let mut stats = self.inner.stats.lock().expect("9P stats mutex poisoned");
        stats.replies += 1;
        if error {
            stats.errors += 1;
        }
    }

    async fn flush(&self, header: P9Header, oldtag: u16) -> FsResult<Vec<u8>> {
        let pending = self
            .inner
            .inflight
            .lock()
            .expect("9P session mutex poisoned")
            .get(&oldtag)
            .cloned();
        if let Some(pending) = pending {
            self.inner
                .stats
                .lock()
                .expect("9P stats mutex poisoned")
                .flushed += 1;
            loop {
                if pending.result.lock().await.is_some() {
                    break;
                }
                pending.notify.notified().await;
            }
        }
        self.frame(header, P9_RFLUSH, 8, |_| Ok(()))
    }

    async fn version_exchange(&self, header: P9Header, request: Tversion) -> FsResult<Vec<u8>> {
        self.reset().await;
        let ceiling = self.inner.options.msize.unwrap_or(DEFAULT_MSIZE);
        let msize = request.msize.min(ceiling);
        let agreed = request.version == P9_VERSION_DOTL && msize >= P9_MIN_MSIZE;
        if agreed {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            if !state.destroyed {
                state.msize = Some(msize);
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

    async fn reset(&self) {
        let handles = {
            let mut state = self.inner.state.lock().expect("9P session mutex poisoned");
            state.generation = state.generation.wrapping_add(1);
            let handles = state.fids.open_handles();
            state.fids.clear();
            state.users.clear();
            state.msize = None;
            handles
        };
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
                    Some(entry) => {
                        entry.set_path(&path);
                        entry.open = Some(FidOpenState {
                            flags: reopen_flags(flags),
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
