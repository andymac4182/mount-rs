//! N-API bindings for the small, platform-independent mountx utility surface.
//!
//! This module deliberately exposes helpers that are backed by Rust/core
//! state.  It does not turn arbitrary JavaScript objects into an `FsDriver`:
//! the native `Filesystem` classes remain the filesystem boundary.

use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};

use mount_rs_core::types::FileType;
use napi::bindgen_prelude::{Function, JsObjectValue, Object, Promise, PromiseRaw};
use napi::{Env, Error as NapiError, JsError, JsRangeError, Status, Unknown, ValueType};
use napi_derive::napi;

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
    state: Arc<Mutex<LockState>>,
}

#[derive(Default)]
struct LockState {
    readers: usize,
    writer: bool,
    gate: bool,
    next_writer_ticket: u64,
    writers: VecDeque<u64>,
    waiters: Vec<Waker>,
}

fn lock_state(lock: &Mutex<LockState>) -> MutexGuard<'_, LockState> {
    lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn take_waiters(state: &mut LockState) -> Vec<Waker> {
    std::mem::take(&mut state.waiters)
}

struct Acquire {
    lock: Arc<Mutex<LockState>>,
    write: bool,
    writer_ticket: Option<u64>,
    granted: bool,
}

impl Future for Acquire {
    type Output = LockGuard;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut state = lock_state(&this.lock);
        let available = if this.write {
            let at_front = this
                .writer_ticket
                .is_some_and(|ticket| state.writers.front() == Some(&ticket));
            if at_front && !state.writer {
                // This is the upstream `#gate = new Promise(...)` point:
                // readers arriving after the writer has actually started
                // must wait, even while existing readers drain.
                state.gate = true;
            }
            at_front && !state.writer && state.readers == 0
        } else {
            !state.writer && !state.gate
        };

        if available {
            if this.write {
                state.writers.pop_front();
                state.writer = true;
            } else {
                state.readers += 1;
            }
            this.granted = true;
            return Poll::Ready(LockGuard {
                lock: Arc::clone(&this.lock),
                write: this.write,
            });
        }

        if !state
            .waiters
            .iter()
            .any(|waiter| waiter.will_wake(context.waker()))
        {
            state.waiters.push(context.waker().clone());
        }
        Poll::Pending
    }
}

impl Drop for Acquire {
    fn drop(&mut self) {
        if self.granted {
            return;
        }
        if let Some(ticket) = self.writer_ticket {
            let waiters = {
                let mut state = lock_state(&self.lock);
                if let Some(index) = state.writers.iter().position(|queued| *queued == ticket) {
                    state.writers.remove(index);
                    if state.writers.front().is_none() && !state.writer {
                        state.gate = false;
                    }
                    take_waiters(&mut state)
                } else {
                    Vec::new()
                }
            };
            for waiter in waiters {
                waiter.wake();
            }
        }
    }
}

struct LockGuard {
    lock: Arc<Mutex<LockState>>,
    write: bool,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let waiters = {
            let mut state = lock_state(&self.lock);
            if self.write {
                state.writer = false;
                state.gate = false;
            } else {
                state.readers -= 1;
            }
            take_waiters(&mut state)
        };
        for waiter in waiters {
            waiter.wake();
        }
    }
}

impl NativePathLock {
    fn reserve_writer(&self) -> u64 {
        let mut state = lock_state(&self.state);
        let ticket = state.next_writer_ticket;
        state.next_writer_ticket = state.next_writer_ticket.wrapping_add(1);
        state.writers.push_back(ticket);
        ticket
    }

    fn acquire(&self, write: bool, writer_ticket: Option<u64>) -> Acquire {
        Acquire {
            lock: Arc::clone(&self.state),
            write,
            writer_ticket,
            granted: false,
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
        let lock = self.inner.clone();
        let writer_ticket = write.then(|| lock.reserve_writer());
        let promise: PromiseRaw<'_, ()> = match env.spawn_future(async move {
            let _guard = lock.acquire(write, writer_ticket).await;
            let result = callback.call_async_catch(()).await?;
            result.await?;
            Ok(())
        }) {
            Ok(promise) => promise,
            Err(error) => {
                if let Some(ticket) = writer_ticket {
                    let mut state = lock_state(&self.inner.state);
                    if let Some(index) = state.writers.iter().position(|queued| *queued == ticket) {
                        state.writers.remove(index);
                    }
                }
                return Err(error);
            }
        };
        // `PromiseRaw` only carries the Env lifetime at the type level.  The
        // N-API promise and its callback queue are owned by Node; all values
        // captured by the future are owned or thread-safe before this point.
        Ok(unsafe { std::mem::transmute::<PromiseRaw<'_, ()>, PromiseRaw<'static, ()>>(promise) })
    }
}
