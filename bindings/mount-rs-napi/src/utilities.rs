//! N-API bindings for the small, platform-independent mountx utility surface.
//!
//! This module deliberately exposes helpers that are backed by Rust/core
//! state.  It does not turn arbitrary JavaScript objects into an `FsDriver`:
//! the native `Filesystem` classes remain the filesystem boundary.

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::task::{Context, Poll};

use crossbeam_queue::SegQueue;
use mount_rs_core::types::FileType;
use napi::bindgen_prelude::{Function, JsObjectValue, Object, Promise, PromiseRaw};
use napi::{Env, Error as NapiError, JsError, JsRangeError, Status, Unknown, ValueType};
use napi_derive::napi;
use tokio::sync::oneshot;

const STACKLESS_CODES: &[&str] = &["ENOENT", "ENOTSUP", "ENOSYS"];

fn is_normalized(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes[0] != b'/' {
        return false;
    }
    if bytes.len() == 1 {
        return true;
    }

    let mut start = 1;
    for index in 1..=bytes.len() {
        if index != bytes.len() && bytes[index] != b'/' {
            continue;
        }
        let size = index - start;
        if size == 0 {
            return false;
        }
        if bytes[start] == b'.' && (size == 1 || (size == 2 && bytes[start + 1] == b'.')) {
            return false;
        }
        start = index + 1;
    }
    true
}

fn split(path: &str) -> Vec<String> {
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            value => segments.push(value.to_owned()),
        }
    }
    segments
}

fn normalize(path: &str) -> String {
    if is_normalized(path) {
        return path.to_owned();
    }
    let segments = split(path);
    if segments.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", segments.join("/"))
    }
}

#[napi(object)]
pub struct JsResolvedPath {
    pub path: String,
    pub segments: Vec<String>,
}

#[napi]
pub fn is_normalized_path(path: String) -> bool {
    is_normalized(&path)
}

#[napi]
pub fn split_path(path: String) -> Vec<String> {
    split(&path)
}

#[napi]
pub fn normalize_path(path: String) -> String {
    normalize(&path)
}

#[napi]
pub fn resolve_path(path: String) -> JsResolvedPath {
    if is_normalized(&path) {
        let segments = if path == "/" {
            Vec::new()
        } else {
            path[1..].split('/').map(ToOwned::to_owned).collect()
        };
        return JsResolvedPath { path, segments };
    }

    let segments = split(&path);
    let resolved = if segments.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", segments.join("/"))
    };
    JsResolvedPath {
        path: resolved,
        segments,
    }
}

/// The public TypeScript API is variadic; the postlude turns this native
/// array bridge into `joinPath(...parts)` without pretending N-API received a
/// JavaScript callback or variadic argument list directly.
#[napi(js_name = "joinPathParts")]
pub fn join_path_parts(parts: Vec<String>) -> String {
    normalize(&parts.join("/"))
}

#[napi]
pub fn dirname(path: String) -> String {
    if is_normalized(&path) {
        let cut = path.rfind('/').unwrap_or(0);
        return if cut == 0 {
            "/".to_owned()
        } else {
            path[..cut].to_owned()
        };
    }

    let mut segments = split(&path);
    segments.pop();
    if segments.is_empty() {
        "/".to_owned()
    } else {
        format!("/{}", segments.join("/"))
    }
}

#[napi]
pub fn basename(path: String) -> String {
    if is_normalized(&path) {
        return if path == "/" {
            "/".to_owned()
        } else {
            path.rsplit('/').next().unwrap_or("/").to_owned()
        };
    }

    split(&path).pop().unwrap_or_else(|| "/".to_owned())
}

#[napi]
pub fn is_path_inside(path: String, parent: String) -> bool {
    if path == parent {
        true
    } else if parent == "/" {
        path.starts_with('/')
    } else {
        path.starts_with(&(parent + "/"))
    }
}

fn errno_info(code: &str) -> Option<(i32, &'static str)> {
    Some(match code {
        "EPERM" => (1, "operation not permitted"),
        "ENOENT" => (2, "no such file or directory"),
        "EINTR" => (4, "interrupted system call"),
        "EIO" => (5, "i/o error"),
        "ENXIO" => (6, "no such device or address"),
        "EBADF" => (9, "bad file descriptor"),
        "EAGAIN" => (11, "resource temporarily unavailable"),
        "ENOMEM" => (12, "not enough memory"),
        "EACCES" => (13, "permission denied"),
        "EBUSY" => (16, "resource busy or locked"),
        "EEXIST" => (17, "file already exists"),
        "EXDEV" => (18, "cross-device link not permitted"),
        "ENODEV" => (19, "no such device"),
        "ENOTDIR" => (20, "not a directory"),
        "EISDIR" => (21, "illegal operation on a directory"),
        "EINVAL" => (22, "invalid argument"),
        "ENFILE" => (23, "file table overflow"),
        "EMFILE" => (24, "too many open files"),
        "EFBIG" => (27, "file too large"),
        "ENOSPC" => (28, "no space left on device"),
        "ESPIPE" => (29, "invalid seek"),
        "EROFS" => (30, "read-only file system"),
        "EMLINK" => (31, "too many links"),
        "ERANGE" => (34, "result too large"),
        "ENAMETOOLONG" => (36, "name too long"),
        "ENOSYS" => (38, "function not implemented"),
        "ENOTEMPTY" => (39, "directory not empty"),
        "ELOOP" => (40, "too many symbolic links encountered"),
        "ENODATA" => (61, "no data available"),
        "EPROTO" => (71, "protocol error"),
        "EOVERFLOW" => (75, "value too large for defined data type"),
        "ENOTSUP" => (95, "operation not supported on socket"),
        "ESTALE" => (116, "stale file handle"),
        "EDQUOT" => (122, "disk quota exceeded"),
        _ => return None,
    })
}

#[napi(object)]
pub struct JsErrnoCodes {
    #[napi(js_name = "EPERM")]
    pub eperm: i32,
    #[napi(js_name = "ENOENT")]
    pub enoent: i32,
    #[napi(js_name = "EINTR")]
    pub eintr: i32,
    #[napi(js_name = "EIO")]
    pub eio: i32,
    #[napi(js_name = "ENXIO")]
    pub enxio: i32,
    #[napi(js_name = "EBADF")]
    pub ebadf: i32,
    #[napi(js_name = "EAGAIN")]
    pub eagain: i32,
    #[napi(js_name = "ENOMEM")]
    pub enomem: i32,
    #[napi(js_name = "EACCES")]
    pub eacces: i32,
    #[napi(js_name = "EBUSY")]
    pub ebusy: i32,
    #[napi(js_name = "EEXIST")]
    pub eexist: i32,
    #[napi(js_name = "EXDEV")]
    pub exdev: i32,
    #[napi(js_name = "ENODEV")]
    pub enodev: i32,
    #[napi(js_name = "ENOTDIR")]
    pub enotdir: i32,
    #[napi(js_name = "EISDIR")]
    pub eisdir: i32,
    #[napi(js_name = "EINVAL")]
    pub einval: i32,
    #[napi(js_name = "ENFILE")]
    pub enfile: i32,
    #[napi(js_name = "EMFILE")]
    pub emfile: i32,
    #[napi(js_name = "EFBIG")]
    pub efbig: i32,
    #[napi(js_name = "ENOSPC")]
    pub enospc: i32,
    #[napi(js_name = "ESPIPE")]
    pub espipe: i32,
    #[napi(js_name = "EROFS")]
    pub erofs: i32,
    #[napi(js_name = "EMLINK")]
    pub emlink: i32,
    #[napi(js_name = "ERANGE")]
    pub erange: i32,
    #[napi(js_name = "ENAMETOOLONG")]
    pub enametoolong: i32,
    #[napi(js_name = "ENOSYS")]
    pub enosys: i32,
    #[napi(js_name = "ENOTEMPTY")]
    pub enotempty: i32,
    #[napi(js_name = "ELOOP")]
    pub eloop: i32,
    #[napi(js_name = "ENODATA")]
    pub enodata: i32,
    #[napi(js_name = "EPROTO")]
    pub eproto: i32,
    #[napi(js_name = "EOVERFLOW")]
    pub eoverflow: i32,
    #[napi(js_name = "ENOTSUP")]
    pub enotsup: i32,
    #[napi(js_name = "ESTALE")]
    pub estale: i32,
    #[napi(js_name = "EDQUOT")]
    pub edquot: i32,
}

#[napi]
pub fn errno_codes() -> JsErrnoCodes {
    JsErrnoCodes {
        eperm: 1,
        enoent: 2,
        eintr: 4,
        eio: 5,
        enxio: 6,
        ebadf: 9,
        eagain: 11,
        enomem: 12,
        eacces: 13,
        ebusy: 16,
        eexist: 17,
        exdev: 18,
        enodev: 19,
        enotdir: 20,
        eisdir: 21,
        einval: 22,
        enfile: 23,
        emfile: 24,
        efbig: 27,
        enospc: 28,
        espipe: 29,
        erofs: 30,
        emlink: 31,
        erange: 34,
        enametoolong: 36,
        enosys: 38,
        enotempty: 39,
        eloop: 40,
        enodata: 61,
        eproto: 71,
        eoverflow: 75,
        enotsup: 95,
        estale: 116,
        edquot: 122,
    }
}

fn optional_string(options: Option<&Object<'_>>, name: &str) -> Option<String> {
    options.and_then(|value| value.get_named_property::<String>(name).ok())
}

#[napi(ts_return_type = "FsError")]
pub fn fs_error(
    env: Env,
    code: String,
    #[napi(ts_arg_type = "FsErrorOptions | undefined")] options: Option<Unknown<'_>>,
) -> napi::Result<Unknown<'static>> {
    let options = options
        .map(|value| unsafe { value.cast::<Object>() })
        .transpose()?;
    let (errno, description) = errno_info(&code)
        .ok_or_else(|| NapiError::new(Status::InvalidArg, format!("unknown errno code: {code}")))?;
    let syscall = optional_string(options.as_ref(), "syscall");
    let path = optional_string(options.as_ref(), "path");
    let dest = optional_string(options.as_ref(), "dest");
    let message = optional_string(options.as_ref(), "message").unwrap_or_else(|| {
        let mut message = format!("{code}: {description}");
        if let Some(syscall) = &syscall {
            message.push_str(&format!(", {syscall}"));
        }
        if let Some(path) = &path {
            message.push_str(&format!(" '{path}'"));
        }
        if let Some(dest) = &dest {
            message.push_str(&format!(" -> '{dest}'"));
        }
        message
    });

    let error = JsError::from(NapiError::new(Status::GenericFailure, message.clone()));
    let error = error.into_unknown(env);
    let mut object = unsafe { error.cast::<Object>()? };
    // Node's own errors assign errno before code.  The fields are enumerable
    // in that order, which is observable through Object.keys/spread.
    object.set_named_property("errno", -errno)?;
    object.set_named_property("code", code)?;
    if let Some(syscall) = syscall {
        object.set_named_property("syscall", syscall)?;
    }
    if let Some(path) = path {
        object.set_named_property("path", path)?;
    }
    if let Some(dest) = dest {
        object.set_named_property("dest", dest)?;
    }
    if let Some(cause) = options.and_then(|value| value.get_named_property::<Unknown>("cause").ok())
        && cause.get_type()? != ValueType::Undefined
    {
        object.set_named_property("cause", cause)?;
    }
    let code_name = object.get_named_property::<String>("code")?;
    if STACKLESS_CODES.iter().any(|value| *value == code_name) {
        object.set_named_property("stack", format!("Error: {message}"))?;
    }
    Ok(error)
}

fn format_number(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value == f64::INFINITY {
        "Infinity".to_owned()
    } else if value == f64::NEG_INFINITY {
        "-Infinity".to_owned()
    } else if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

#[napi(ts_return_type = "RangeError")]
pub fn range_error(
    env: Env,
    name: String,
    expected: String,
    value: f64,
) -> napi::Result<Unknown<'static>> {
    let message = format!(
        "The value of \"{name}\" is out of range. It must be {expected}. Received {}",
        format_number(value)
    );
    let error = JsRangeError::from(NapiError::new(Status::GenericFailure, message));
    let error = error.into_unknown(env);
    let mut object = unsafe { error.cast::<Object>()? };
    object.set_named_property("code", "ERR_OUT_OF_RANGE")?;
    Ok(error)
}

#[napi(ts_return_type = "error is FsError")]
pub fn is_fs_error(error: Unknown<'_>, code: Option<String>) -> bool {
    let Ok(object) = (unsafe { error.cast::<Object>() }) else {
        return false;
    };
    let Ok(candidate_code) = object.get_named_property::<String>("code") else {
        return false;
    };
    if object.get_named_property::<f64>("errno").is_err() {
        return false;
    }
    code.is_none_or(|expected| candidate_code == expected)
}

#[napi]
pub fn errno_of(error: Unknown<'_>) -> f64 {
    let Ok(object) = (unsafe { error.cast::<Object>() }) else {
        return 5.0;
    };
    if let Ok(code) = object.get_named_property::<String>("code")
        && let Some((errno, _)) = errno_info(&code)
    {
        return f64::from(errno);
    }
    if let Ok(errno) = object.get_named_property::<f64>("errno")
        && errno != 0.0
    {
        return errno.abs();
    }
    5.0
}

#[napi(js_name = "S_IFMT")]
pub const S_IFMT: u32 = 0o170000;
#[napi(js_name = "S_IFREG")]
pub const S_IFREG: u32 = 0o100000;
#[napi(js_name = "S_IFDIR")]
pub const S_IFDIR: u32 = 0o040000;
#[napi(js_name = "S_IFLNK")]
pub const S_IFLNK: u32 = 0o120000;
#[napi(js_name = "S_IFBLK")]
pub const S_IFBLK: u32 = 0o060000;
#[napi(js_name = "S_IFCHR")]
pub const S_IFCHR: u32 = 0o020000;
#[napi(js_name = "S_IFIFO")]
pub const S_IFIFO: u32 = 0o010000;
#[napi(js_name = "S_IFSOCK")]
pub const S_IFSOCK: u32 = 0o140000;
#[napi(js_name = "S_ISGID")]
pub const S_ISGID: u32 = 0o2000;
#[napi(js_name = "S_IXGRP")]
pub const S_IXGRP: u32 = 0o0010;

#[napi]
pub fn file_type_mode(mode: u32) -> u32 {
    FileType::from_mode(mode).mode_bits()
}

#[napi]
pub fn is_special_mode(mode: u32) -> bool {
    FileType::from_mode(mode).is_special()
}

#[derive(Clone, Default)]
struct NativePathLock {
    owner: Arc<Owner>,
}

/// What a caller tells the owner of the lock state.
enum Message {
    /// A writer was called: its ticket joins the back of the queue. Sent
    /// synchronously from the N-API entry point, so the queue is in call
    /// order.
    Reserve(u64),
    /// A caller polled for the first time. `granted` is answered when it
    /// may start; a writer names its ticket.
    Acquire {
        ticket: Option<u64>,
        granted: oneshot::Sender<()>,
    },
    /// A writer that never started was dropped, from anywhere in the queue.
    Cancel(u64),
    /// A caller that had started is done.
    Release { write: bool },
}

#[derive(Default)]
struct LockState {
    readers: usize,
    writer: bool,
    gate: bool,
    /// Tickets in call order.
    writers: VecDeque<u64>,
    /// Callers that polled and could not start, in the order they polled.
    waiting: Vec<Waiter>,
}

struct Waiter {
    /// A writer's ticket.
    ticket: Option<u64>,
    /// Taken when the caller is told to start.
    granted: Option<oneshot::Sender<()>>,
}

impl LockState {
    /// The first poll of a caller that can start at once, with no channel.
    fn start_now(&mut self, ticket: Option<u64>) -> bool {
        match ticket {
            None => {
                let free = !self.writer && !self.gate;
                if free {
                    self.readers += 1;
                }
                free
            }
            Some(ticket) => {
                let at_front = self.writers.front() == Some(&ticket);
                if !at_front || self.writer {
                    return false;
                }
                // This is the upstream `#gate = new Promise(...)` point:
                // readers arriving after the writer has actually started
                // must wait, even while existing readers drain.
                self.gate = true;
                if self.readers != 0 {
                    return false;
                }
                self.writers.pop_front();
                self.writer = true;
                true
            }
        }
    }

    fn handle(&mut self, message: Message) {
        match message {
            Message::Reserve(ticket) => {
                // A ticket that has not polled changes nothing for anyone.
                self.writers.push_back(ticket);
                return;
            }
            Message::Acquire { ticket, granted } => self.waiting.push(Waiter {
                ticket,
                granted: Some(granted),
            }),
            Message::Cancel(ticket) => {
                self.waiting.retain(|waiter| waiter.ticket != Some(ticket));
                if let Some(index) = self.writers.iter().position(|queued| *queued == ticket) {
                    self.writers.remove(index);
                    if self.writers.is_empty() && !self.writer {
                        self.gate = false;
                    }
                }
            }
            Message::Release { write: true } => {
                self.writer = false;
                self.gate = false;
            }
            Message::Release { write: false } => self.readers -= 1,
        }
        while self.settle() {}
    }

    /// Start every waiting caller that may now start, looking at them in
    /// the order they polled, as if each had polled again. A caller whose
    /// receiver is gone was dropped: it is passed over, which is the same
    /// as its having started and released at once. Returns true if that
    /// happened to a writer, so that the callers already passed must be
    /// looked at again.
    fn settle(&mut self) -> bool {
        let Self {
            readers,
            writer,
            gate,
            writers,
            waiting,
        } = self;
        let mut again = false;
        waiting.retain_mut(|waiter| {
            let open = waiter
                .granted
                .as_ref()
                .is_some_and(|granted| !granted.is_closed());
            if !open {
                // Dropped. A writer's ticket leaves with its `Cancel`.
                return false;
            }
            if *writer {
                return true;
            }
            let start = match waiter.ticket {
                None => !*gate,
                Some(ticket) => {
                    let at_front = writers.front() == Some(&ticket);
                    if at_front {
                        // The upstream `#gate = new Promise(...)` point.
                        *gate = true;
                    }
                    at_front && *readers == 0
                }
            };
            if !start {
                return true;
            }
            let told = waiter
                .granted
                .take()
                .is_some_and(|granted| granted.send(()).is_ok());
            match (waiter.ticket, told) {
                (None, true) => *readers += 1,
                (None, false) => {}
                (Some(_), told) => {
                    writers.pop_front();
                    *writer = told;
                    if !told {
                        // Dropped as it was told: what `Cancel` does.
                        if writers.is_empty() {
                            *gate = false;
                        }
                        again = true;
                    }
                }
            }
            false
        });
        again
    }
}

/// The lock state and the messages for it. There is no lock around the
/// state: whoever finds no message unhandled becomes its owner for as long
/// as messages keep arriving, and everyone else leaves a message and goes.
/// Nobody waits for the state.
#[derive(Default)]
struct Owner {
    next_ticket: AtomicU64,
    /// Messages sent and not yet handled, the one being handled included.
    /// The caller that moves it from zero owns `state` until it is zero
    /// again.
    unhandled: AtomicUsize,
    inbox: SegQueue<Message>,
    state: UnsafeCell<LockState>,
}

// SAFETY: `state` is only reached by the caller that moved `unhandled` from
// zero, one at a time, and may be reached from any thread.
unsafe impl Sync for Owner {}

/// Possession of the state: the caller that moved `unhandled` from zero.
struct Owned<'a>(&'a Owner);

impl Owned<'_> {
    fn state(&mut self) -> &mut LockState {
        // SAFETY: this caller moved `unhandled` from zero, and gives the
        // state up only by consuming `self`.
        unsafe { &mut *self.0.state.get() }
    }

    /// One message has been handled: stop being the owner, unless others
    /// were left meanwhile.
    fn handled(self) {
        if self.0.unhandled.fetch_sub(1, Ordering::SeqCst) != 1 {
            self.drain();
        }
    }

    /// Handle queued messages until none is unhandled.
    fn drain(mut self) {
        loop {
            // A message is counted after it is queued, so one is there.
            let message = self.0.inbox.pop();
            debug_assert!(message.is_some());
            if let Some(message) = message {
                self.state().handle(message);
            }
            if self.0.unhandled.fetch_sub(1, Ordering::SeqCst) == 1 {
                return;
            }
        }
    }
}

impl Owner {
    /// Become the owner if no message is unhandled.
    fn take(&self) -> Option<Owned<'_>> {
        self.unhandled
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .ok()?;
        let owned = Owned(self);
        if self.inbox.is_empty() {
            return Some(owned);
        }
        // Messages are queued whose senders have not counted them yet.
        // The caller's own goes behind them, so give the state up again.
        owned.handled();
        None
    }

    fn send(&self, message: Message) {
        if let Some(mut owned) = self.take() {
            owned.state().handle(message);
            owned.handled();
            return;
        }
        self.inbox.push(message);
        if self.unhandled.fetch_add(1, Ordering::SeqCst) == 0 {
            // The owner left between the two steps.
            Owned(self).drain();
        }
    }

    /// A caller's first poll. `None`: it has started. Otherwise the channel
    /// on which it is told when it may.
    fn acquire(&self, ticket: Option<u64>) -> Option<oneshot::Receiver<()>> {
        if let Some(mut owned) = self.take() {
            let waiting = if owned.state().start_now(ticket) {
                None
            } else {
                let (granted, receiver) = oneshot::channel();
                owned.state().handle(Message::Acquire { ticket, granted });
                Some(receiver)
            };
            owned.handled();
            return waiting;
        }
        let (granted, receiver) = oneshot::channel();
        self.send(Message::Acquire { ticket, granted });
        Some(receiver)
    }
}

struct Acquire {
    owner: Arc<Owner>,
    write: bool,
    writer_ticket: Option<u64>,
    stage: Stage,
}

enum Stage {
    /// Not polled yet: a writer has only its ticket.
    Unpolled,
    Waiting(oneshot::Receiver<()>),
    /// Holds the lock, with no guard yet to give it back. A reader is here
    /// from its call until its first poll, so a future dropped in between
    /// must release.
    Holding,
    /// The guard has been handed out and owns the release.
    Issued,
}

impl Acquire {
    /// What the first poll does: start, or join the callers waiting.
    fn begin(&mut self) {
        if matches!(self.stage, Stage::Unpolled) {
            self.stage = match self.owner.acquire(self.writer_ticket) {
                Some(receiver) => Stage::Waiting(receiver),
                None => Stage::Holding,
            };
        }
    }
}

impl Future for Acquire {
    type Output = LockGuard;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.begin();
        if let Stage::Waiting(receiver) = &mut this.stage {
            // The owner answers every sender whose receiver is open, so the
            // channel does not close under a caller that is still waiting.
            match Pin::new(receiver).poll(context) {
                Poll::Ready(Ok(())) => this.stage = Stage::Holding,
                Poll::Ready(Err(_)) | Poll::Pending => return Poll::Pending,
            }
        }
        this.stage = Stage::Issued;
        Poll::Ready(LockGuard {
            owner: Arc::clone(&this.owner),
            write: this.write,
        })
    }
}

impl Drop for Acquire {
    fn drop(&mut self) {
        match std::mem::replace(&mut self.stage, Stage::Issued) {
            Stage::Issued => {}
            Stage::Holding => self.owner.send(Message::Release { write: self.write }),
            Stage::Unpolled => {
                if let Some(ticket) = self.writer_ticket {
                    self.owner.send(Message::Cancel(ticket));
                }
            }
            Stage::Waiting(mut receiver) => {
                // After `close` the owner can no longer start this caller;
                // if it already did, this caller holds the lock and gives
                // it back.
                receiver.close();
                if receiver.try_recv().is_ok() {
                    self.owner.send(Message::Release { write: self.write });
                } else if let Some(ticket) = self.writer_ticket {
                    self.owner.send(Message::Cancel(ticket));
                }
            }
        }
    }
}

struct LockGuard {
    owner: Arc<Owner>,
    write: bool,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        self.owner.send(Message::Release { write: self.write });
    }
}

impl NativePathLock {
    fn reserve_writer(&self) -> u64 {
        let ticket = self.owner.next_ticket.fetch_add(1, Ordering::Relaxed);
        self.owner.send(Message::Reserve(ticket));
        ticket
    }

    fn acquire(&self, write: bool, writer_ticket: Option<u64>) -> Acquire {
        Acquire {
            owner: Arc::clone(&self.owner),
            write,
            writer_ticket,
            stage: Stage::Unpolled,
        }
    }
}

/// Native equivalent of mountx's PathLock.  N-API callbacks are intentionally
/// limited to `() => Promise<void>`; arbitrary JS driver callbacks are not a
/// supported boundary for this crate.
#[napi]
#[derive(Default)]
pub struct PathLock {
    inner: NativePathLock,
}

#[napi]
impl PathLock {
    #[napi(constructor)]
    pub fn new() -> Self {
        Self::default()
    }

    #[napi]
    pub fn read(
        &self,
        env: Env,
        callback: Function<(), Promise<()>>,
    ) -> napi::Result<PromiseRaw<'static, ()>> {
        self.run(env, callback, false)
    }

    #[napi]
    pub fn write(
        &self,
        env: Env,
        callback: Function<(), Promise<()>>,
    ) -> napi::Result<PromiseRaw<'static, ()>> {
        self.run(env, callback, true)
    }

    fn run(
        &self,
        env: Env,
        callback: Function<(), Promise<()>>,
        write: bool,
    ) -> napi::Result<PromiseRaw<'static, ()>> {
        let callback = callback
            .build_threadsafe_function::<()>()
            .callee_handled::<false>()
            .build()?;
        let writer_ticket = write.then(|| self.inner.reserve_writer());
        let mut acquire = self.inner.acquire(write, writer_ticket);
        if !write {
            // A reader takes its place now, in the call. Left to its first
            // poll on the runtime, a reader called before a writer could be
            // polled after it and wait behind its gate, where upstream's
            // reader has already entered. A writer still sets the gate at
            // its first poll.
            acquire.begin();
        }
        // A future that cannot be spawned is dropped, and `acquire` with
        // it: that withdraws the ticket or gives the read back.
        let promise: PromiseRaw<'_, ()> = env.spawn_future(async move {
            let _guard = acquire.await;
            let result = callback.call_async_catch(()).await?;
            result.await?;
            Ok(())
        })?;
        // `PromiseRaw` only carries the Env lifetime at the type level.  The
        // N-API promise and its callback queue are owned by Node; all values
        // captured by the future are owned or thread-safe before this point.
        Ok(unsafe { std::mem::transmute::<PromiseRaw<'_, ()>, PromiseRaw<'static, ()>>(promise) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use napi::tokio::{self as rt_tokio, runtime::Runtime};
    use std::sync::atomic::AtomicI64;
    use std::task::Waker;
    use std::time::Instant;

    fn runtime(workers: usize) -> Runtime {
        rt_tokio::runtime::Builder::new_multi_thread()
            .worker_threads(workers)
            .enable_all()
            .build()
            .unwrap()
    }

    /// Poll once with a waker that does nothing: the tests below decide
    /// themselves when to look again.
    fn poll_once(acquire: &mut Acquire) -> Option<LockGuard> {
        let mut context = Context::from_waker(Waker::noop());
        match Pin::new(acquire).poll(&mut context) {
            Poll::Ready(guard) => Some(guard),
            Poll::Pending => None,
        }
    }

    fn writer(lock: &NativePathLock) -> Acquire {
        let ticket = lock.reserve_writer();
        lock.acquire(true, Some(ticket))
    }

    fn reader(lock: &NativePathLock) -> Acquire {
        lock.acquire(false, None)
    }

    #[test]
    fn reader_issued_after_an_unpolled_writer_starts() {
        let lock = NativePathLock::default();
        let first = poll_once(&mut reader(&lock)).unwrap();
        let mut queued = writer(&lock);
        // The writer holds a ticket but has not polled: no gate yet.
        let second = poll_once(&mut reader(&lock)).unwrap();
        assert!(poll_once(&mut queued).is_none());
        // Now it has, and a later reader waits behind it.
        let mut late = reader(&lock);
        assert!(poll_once(&mut late).is_none());
        drop(first);
        assert!(poll_once(&mut queued).is_none());
        drop(second);
        let writing = poll_once(&mut queued).unwrap();
        assert!(poll_once(&mut late).is_none());
        drop(writing);
        assert!(poll_once(&mut late).is_some());
    }

    #[test]
    fn reader_begun_in_its_call_is_ahead_of_a_writer_that_polls_first() {
        let lock = NativePathLock::default();
        let first = poll_once(&mut reader(&lock)).unwrap();
        let mut queued = writer(&lock);
        // What `PathLock::read` does before it spawns the reader's future.
        let mut second = reader(&lock);
        second.begin();
        // The writer's future is polled first; the reader is in all the same.
        assert!(poll_once(&mut queued).is_none());
        let second = poll_once(&mut second).unwrap();
        drop(first);
        assert!(poll_once(&mut queued).is_none());
        drop(second);
        assert!(poll_once(&mut queued).is_some());
    }

    #[test]
    fn a_reader_dropped_between_its_call_and_its_first_poll_gives_the_read_back() {
        let lock = NativePathLock::default();
        // What `PathLock::read` does before it spawns the reader's future,
        // which is then dropped unpolled, as it is when the spawn fails.
        let mut abandoned = reader(&lock);
        abandoned.begin();
        drop(abandoned);
        // A writer would wait for ever on a read that nobody holds.
        let guard = poll_once(&mut writer(&lock)).expect("the lock is free");
        drop(guard);
        assert!(poll_once(&mut reader(&lock)).is_some());
    }

    #[test]
    fn writers_run_in_call_order_whatever_order_they_poll_in() {
        let lock = NativePathLock::default();
        let mut first = writer(&lock);
        let mut second = writer(&lock);
        let mut third = writer(&lock);
        assert!(poll_once(&mut third).is_none());
        assert!(poll_once(&mut second).is_none());
        let guard = poll_once(&mut first).unwrap();
        assert!(poll_once(&mut second).is_none());
        drop(guard);
        assert!(poll_once(&mut third).is_none());
        let guard = poll_once(&mut second).unwrap();
        drop(guard);
        assert!(poll_once(&mut third).is_some());
    }

    #[test]
    fn cancel_in_mid_queue_leaves_the_writers_around_it_in_order() {
        let lock = NativePathLock::default();
        let holder = poll_once(&mut reader(&lock)).unwrap();
        let mut first = writer(&lock);
        let mut second = writer(&lock);
        let mut third = writer(&lock);
        let unpolled = writer(&lock);
        let mut last = writer(&lock);
        for acquire in [&mut first, &mut second, &mut third, &mut last] {
            assert!(poll_once(acquire).is_none());
        }
        // One from the middle that has polled, one that never did.
        drop(second);
        drop(unpolled);
        // The front writer has polled, so its gate holds new readers out.
        let mut gated = reader(&lock);
        assert!(poll_once(&mut gated).is_none());
        drop(holder);
        assert!(poll_once(&mut third).is_none());
        assert!(poll_once(&mut last).is_none());
        let guard = poll_once(&mut first).unwrap();
        assert!(poll_once(&mut third).is_none());
        drop(guard);
        assert!(poll_once(&mut last).is_none());
        let guard = poll_once(&mut third).unwrap();
        drop(guard);
        let guard = poll_once(&mut last).unwrap();
        assert!(poll_once(&mut gated).is_none());
        drop(guard);
        assert!(poll_once(&mut gated).is_some());
    }

    #[test]
    fn cancelling_the_only_writer_lets_gated_readers_in() {
        let lock = NativePathLock::default();
        let holder = poll_once(&mut reader(&lock)).unwrap();
        let mut only = writer(&lock);
        assert!(poll_once(&mut only).is_none());
        let mut gated = reader(&lock);
        assert!(poll_once(&mut gated).is_none());
        drop(only);
        let guard = poll_once(&mut gated).unwrap();
        drop((holder, guard));
        assert!(poll_once(&mut writer(&lock)).is_some());
    }

    #[test]
    fn a_writer_dropped_once_the_lock_is_free_for_it_holds_nothing() {
        let lock = NativePathLock::default();
        let holder = poll_once(&mut reader(&lock)).unwrap();
        let mut front = writer(&lock);
        let mut behind = writer(&lock);
        assert!(poll_once(&mut front).is_none());
        assert!(poll_once(&mut behind).is_none());
        // The lock becomes free for `front`, which is dropped without
        // looking again: that must count as a release, not strand `behind`.
        drop(holder);
        drop(front);
        let guard = poll_once(&mut behind).unwrap();
        drop(guard);
        assert!(poll_once(&mut reader(&lock)).is_some());
    }

    /// Writers exclude everyone, start in ticket order, and nothing is left
    /// held, while tasks on eight workers acquire and give up at random.
    #[test]
    fn stress_exclusion_and_order_with_random_cancels() {
        const WRITER: i64 = 1 << 32;
        let runtime = runtime(8);
        let lock = NativePathLock::default();
        let inside = Arc::new(AtomicI64::new(0));
        let last_writer = Arc::new(AtomicU64::new(0));
        let cancelled = Arc::new(AtomicU64::new(0));
        runtime.block_on(async {
            let mut tasks = Vec::new();
            let mut sequence = 0u64;
            for round in 0..4_000u64 {
                let write = round % 3 == 0;
                let give_up = round % 7 == 3 || round % 11 == 5;
                // Tickets are reserved here, in one place, as the N-API
                // entry point does: `sequence` is the call order.
                let ticket = write.then(|| lock.reserve_writer());
                if write {
                    sequence += 1;
                }
                let order = sequence;
                let lock = lock.clone();
                let inside = Arc::clone(&inside);
                let last_writer = Arc::clone(&last_writer);
                let cancelled = Arc::clone(&cancelled);
                tasks.push(rt_tokio::spawn(async move {
                    let mut acquire = lock.acquire(write, ticket);
                    let guard = if give_up {
                        // Look once, and walk away if it is not ready.
                        let first = std::future::poll_fn(|context| {
                            Poll::Ready(match Pin::new(&mut acquire).poll(context) {
                                Poll::Ready(guard) => Some(guard),
                                Poll::Pending => None,
                            })
                        })
                        .await;
                        match first {
                            Some(guard) => guard,
                            None => {
                                rt_tokio::task::yield_now().await;
                                drop(acquire);
                                cancelled.fetch_add(1, Ordering::Relaxed);
                                return;
                            }
                        }
                    } else {
                        acquire.await
                    };
                    if write {
                        assert_eq!(inside.fetch_add(WRITER, Ordering::SeqCst), 0);
                        let before = last_writer.swap(order, Ordering::SeqCst);
                        assert!(before < order, "writer {order} ran after {before}");
                    } else {
                        assert!(inside.fetch_add(1, Ordering::SeqCst) < WRITER);
                    }
                    rt_tokio::task::yield_now().await;
                    inside.fetch_sub(if write { WRITER } else { 1 }, Ordering::SeqCst);
                    drop(guard);
                }));
                if round % 64 == 0 {
                    rt_tokio::task::yield_now().await;
                }
            }
            for task in tasks {
                task.await.unwrap();
            }
            // Nothing is left held or queued.
            let guard = lock.acquire(true, Some(lock.reserve_writer())).await;
            drop(guard);
            let _reader = lock.acquire(false, None).await;
        });
        assert_eq!(inside.load(Ordering::SeqCst), 0);
        assert!(cancelled.load(Ordering::Relaxed) > 0);
    }

    async fn cycle(lock: &NativePathLock, write: bool) {
        let ticket = write.then(|| lock.reserve_writer());
        drop(lock.acquire(write, ticket).await);
    }

    /// `callers` tasks each acquire and release `each` times; every
    /// `write_every`-th acquisition of a caller is a write (0: none).
    fn bench(runtime: &Runtime, label: &str, callers: usize, each: usize, write_every: usize) {
        let mut rounds = Vec::new();
        for _ in 0..5 {
            let lock = NativePathLock::default();
            let started = Instant::now();
            runtime.block_on(async {
                let tasks: Vec<_> = (0..callers)
                    .map(|_| {
                        let lock = lock.clone();
                        rt_tokio::spawn(async move {
                            for index in 0..each {
                                let write = write_every != 0 && index % write_every == 0;
                                cycle(&lock, write).await;
                            }
                        })
                    })
                    .collect();
                for task in tasks {
                    task.await.unwrap();
                }
            });
            rounds.push(started.elapsed().as_nanos() as f64 / (callers * each) as f64);
        }
        rounds.sort_by(f64::total_cmp);
        println!(
            "path lock {label:<22} callers={callers} ns/op min={:>8.1} median={:>8.1} max={:>8.1}",
            rounds[0], rounds[2], rounds[4]
        );
    }

    #[test]
    #[ignore = "benchmark; run with --release --ignored --nocapture"]
    fn bench_path_lock() {
        let runtime = runtime(8);
        bench(&runtime, "read", 1, 2_000_000, 0);
        bench(&runtime, "write", 1, 2_000_000, 1);
        bench(&runtime, "read", 8, 250_000, 0);
        bench(&runtime, "write", 8, 100_000, 1);
        bench(&runtime, "1 write in 8", 8, 100_000, 8);
    }
}
